//! Qwen/Qwen3.5-0.8B (리비전 2fc0636) 텍스트 모델의 로컬 실행 (지시서 Stage 3: items 초안 분류·요약 스케줄러).
//!
//! 연산은 transformers 5.23 `models/qwen3_5/modeling_qwen3_5.py`를 그대로 따른다 (참조 픽스처는 그 코드로 만든다:
//! `tests/reference/qwen35_reference.py`).
//! - 24층: 18층은 Gated DeltaNet(선형 어텐션), 6층(3, 7, ..., 23)은 전역 어텐션 (3:1 하이브리드)
//! - Gated DeltaNet: in_proj_qkv → 깊이별 인과 합성곱(커널 4, SiLU) → q·k L2 정규화, q에 1/sqrt(128) →
//!   상태 S(128 x 128, 헤드 16개)를 exp(g)로 감쇠하고 델타 규칙으로 갱신 → 게이트 RMSNorm(SiLU(z)) → out_proj.
//!   g = -exp(A_log) * softplus(a + dt_bias), beta = sigmoid(b). A_log·dt_bias·conv1d·norm은 F32 (지시서 Tier 1 제어 척추)
//! - 전역 어텐션: q_proj가 질의와 출력 게이트를 함께 낸다 (헤드마다 512 = 질의 256 + 게이트 256), q_norm·k_norm,
//!   RoPE는 앞 64차원만(partial 0.25, theta 1e7), GQA 8:2, 출력에 sigmoid(게이트)
//! - RMSNorm은 (1 + w) 형태 (게이트 RMSNorm만 w)
//! - **입력 임베딩(248,320 x 1024 BF16)은 파일 매핑에서 행만 읽는다** (어휘 전체 유지, UNK 0%).
//!   **출력 헤드만** 잘라 쓸 수 있다 (`Head::Trimmed`, 지시서의 비대칭 Trimming). 출력 헤드는 입력 임베딩과 묶여 있다.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{DType, Device, Module, Result, Tensor, D};
use candle_nn::VarBuilder;
use tokenizers::Tokenizer;

use crate::embeddinggemma_tokens::TokenTable;
use crate::quant::{load_lin, Bits, Kernel, Lin};

pub const WEIGHTS_FILE: &str = "model.safetensors-00001-of-00001.safetensors";
pub const TOKENIZER_FILE: &str = "tokenizer.json";
pub const MODEL_ID: &str = "Qwen/Qwen3.5-0.8B";

const HIDDEN: usize = 1024;
const LAYERS: usize = 24;
const HEADS: usize = 8;
const KV_HEADS: usize = 2;
const HEAD_DIM: usize = 256;
const ROTARY_DIM: usize = 64;
const ROPE_THETA: f64 = 10_000_000.0;
const LIN_HEADS: usize = 16;
const LIN_DIM: usize = 128;
const KEY_DIM: usize = LIN_HEADS * LIN_DIM;
const CONV_DIM: usize = KEY_DIM * 3;
const CONV_K: usize = 4;
const MLP: usize = 3584;
pub const VOCAB: usize = 248_320;
const EPS: f64 = 1e-6;
const EMBED: &str = "model.language_model.embed_tokens.weight";

/// 3, 7, 11, ..., 23번 층이 전역 어텐션
pub fn is_full_attention(layer: usize) -> bool {
    layer % 4 == 3
}

/// Qwen3.5 RMSNorm: x / sqrt(mean(x^2) + eps) * (1 + w)
fn rms_norm_1p(x: &Tensor, w_plus_one: &Tensor) -> Result<Tensor> {
    if !x.device().is_cpu()
        && crate::utils::accel_op_ok(x.device(), crate::utils::AccelOp::DecoderOps)
    {
        if let Ok(y) = candle_core::decoder_ops::rmsnorm_fused(x, w_plus_one, EPS as f32, false) {
            return Ok(y);
        }
    }
    let ms = x.sqr()?.mean_keepdim(D::Minus1)?;
    x.broadcast_div(&(ms + EPS)?.sqrt()?)?.broadcast_mul(w_plus_one)
}

struct Gdn {
    in_qkv: Lin,
    in_z: Lin,
    in_b: Lin,
    in_a: Lin,
    /// `[CONV_DIM, CONV_K]` (F32, fused_gated_delta_rule 용)
    conv_w: Tensor,
    /// `[LIN_HEADS]` (`-exp(A_log)`)
    neg_a: Tensor,
    /// `[LIN_HEADS]`
    dt_bias: Tensor,
    /// `[LIN_DIM]` (게이트 RMSNorm 가중치)
    norm_w: Tensor,
    out: Lin,
}

struct Attn {
    q: Lin,
    k: Lin,
    v: Lin,
    o: Lin,
    q_norm: Tensor,
    k_norm: Tensor,
}

enum Mixer {
    Linear(Gdn),
    Full(Attn),
}

struct Layer {
    input_norm: Tensor,
    post_norm: Tensor,
    mixer: Mixer,
    gate: Lin,
    up: Lin,
    down: Lin,
}

/// 한 시퀀스의 상태 (프롬프트를 넣고 한 토큰씩 이어 간다)
pub struct State {
    /// GDN 층별: `[1, CONV_DIM, CONV_K - 1]` 인과 합성곱 상태
    conv: Vec<Option<Tensor>>,
    /// GDN 층별: `[1, LIN_HEADS, LIN_DIM, LIN_DIM]` 순환 상태
    ssm: Vec<Option<Tensor>>,
    /// 전역 층별: 지금까지의 k, v (위치 x KV_HEADS x HEAD_DIM)
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
    pub pos: usize,
}

/// 출력 헤드
pub enum Head {
    /// 전체 어휘 (입력 임베딩 표 전체를 F32로 올린다, 약 1GB. 검증·비교용)
    Full(Tensor),
    /// 잘라 낸 어휘: (선택 토큰 id, 그 행들 F32)
    Trimmed { ids: Vec<u32>, weight: Tensor },
}

impl Head {
    pub fn vocab(&self) -> usize {
        match self {
            Self::Full(w) => w.dim(0).unwrap_or(0),
            Self::Trimmed { ids, .. } => ids.len(),
        }
    }

    /// 마지막 은닉 (HIDDEN) → (토큰 id, 로짓) 목록
    fn logits(&self, h: &Tensor) -> Result<(Vec<f32>, Option<&[u32]>)> {
        let h = h.unsqueeze(0)?;
        match self {
            Self::Full(w) => Ok((h.matmul(&w.t()?)?.squeeze(0)?.to_vec1()?, None)),
            Self::Trimmed { ids, weight } => Ok((h.matmul(&weight.t()?)?.squeeze(0)?.to_vec1()?, Some(ids))),
        }
    }
}

pub struct Qwen35 {
    pub tokenizer: Tokenizer,
    tokens: TokenTable,
    layers: Vec<Layer>,
    norm: Tensor,
    rope_inv: Vec<f32>,
    device: Device,
}

impl Qwen35 {
    /// 모델 폴더를 연다. `bits`가 있으면 층의 선형층(어텐션·GDN 투영·MLP)만 양자화한다 (제어 텐서·정규화·헤드는 F32)
    pub fn load(dir: &Path, bits: Option<Bits>) -> Result<Self> {
        let tokenizer = Tokenizer::from_file(dir.join(TOKENIZER_FILE)).map_err(|e| candle_core::Error::Msg(format!("토크나이저: {e}")))?;
        let device = Device::Cpu;
        // 원본 파일, 없으면 앱이 텍스트 부분만 받아 조립한 파일 (model_fetch.rs)
        let weights = [WEIGHTS_FILE, crate::embeddinggemma::TEXT_WEIGHTS_FILE]
            .iter()
            .map(|f| dir.join(f))
            .find(|p| p.is_file())
            .ok_or_else(|| candle_core::Error::Msg(format!("Qwen3.5 가중치가 없습니다 ({})", dir.display())))?;
        // SAFETY: 파일은 읽기 전용으로 매핑하고, 앱이 쓰는 동안 바꾸지 않는다고 가정한다
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights], DType::F32, &device)? };
        let tokens = TokenTable::open_named(&weights, EMBED, VOCAB, HIDDEN, &device)?;
        let lm = vb.pp("model").pp("language_model");
        let one_plus = |t: Tensor| -> Result<Tensor> { t + 1.0 };
        let mut none = None;
        let mut lin = |vb: &VarBuilder, shape: (usize, usize)| load_lin(vb, shape, bits, Kernel::default(), &mut none, String::new());
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = lm.pp("layers").pp(i.to_string());
            let mixer = if is_full_attention(i) {
                let a = l.pp("self_attn");
                Mixer::Full(Attn {
                    q: lin(&a.pp("q_proj"), (HEADS * HEAD_DIM * 2, HIDDEN))?,
                    k: lin(&a.pp("k_proj"), (KV_HEADS * HEAD_DIM, HIDDEN))?,
                    v: lin(&a.pp("v_proj"), (KV_HEADS * HEAD_DIM, HIDDEN))?,
                    o: lin(&a.pp("o_proj"), (HIDDEN, HEADS * HEAD_DIM))?,
                    q_norm: one_plus(a.pp("q_norm").get(HEAD_DIM, "weight")?)?,
                    k_norm: one_plus(a.pp("k_norm").get(HEAD_DIM, "weight")?)?,
                })
            } else {
                let g = l.pp("linear_attn");
                // 제어 텐서는 F32 그대로 (양자화하지 않는다)
                let a_log = g.get(LIN_HEADS, "A_log")?;
                let neg_a = a_log.exp()?.neg()?;
                Mixer::Linear(Gdn {
                    in_qkv: lin(&g.pp("in_proj_qkv"), (CONV_DIM, HIDDEN))?,
                    in_z: lin(&g.pp("in_proj_z"), (KEY_DIM, HIDDEN))?,
                    in_b: Lin::new(g.pp("in_proj_b").get((LIN_HEADS, HIDDEN), "weight")?, None, Kernel::default())?,
                    in_a: Lin::new(g.pp("in_proj_a").get((LIN_HEADS, HIDDEN), "weight")?, None, Kernel::default())?,
                    conv_w: g.pp("conv1d").get((CONV_DIM, 1, CONV_K), "weight")?.reshape((CONV_DIM, CONV_K))?,
                    neg_a,
                    dt_bias: g.get(LIN_HEADS, "dt_bias")?,
                    norm_w: g.pp("norm").get(LIN_DIM, "weight")?,
                    out: lin(&g.pp("out_proj"), (HIDDEN, KEY_DIM))?,
                })
            };
            let m = l.pp("mlp");
            layers.push(Layer {
                input_norm: one_plus(l.pp("input_layernorm").get(HIDDEN, "weight")?)?,
                post_norm: one_plus(l.pp("post_attention_layernorm").get(HIDDEN, "weight")?)?,
                mixer,
                gate: lin(&m.pp("gate_proj"), (MLP, HIDDEN))?,
                up: lin(&m.pp("up_proj"), (MLP, HIDDEN))?,
                down: lin(&m.pp("down_proj"), (HIDDEN, MLP))?,
            });
        }
        let norm = one_plus(lm.pp("norm").get(HIDDEN, "weight")?)?;
        let half = ROTARY_DIM / 2;
        let inv: Vec<f32> = (0..half).map(|i| (1.0 / ROPE_THETA.powf((2 * i) as f64 / ROTARY_DIM as f64)) as f32).collect();
        Ok(Self { tokenizer, tokens, layers, norm, rope_inv: inv, device })
    }

    pub fn new_state(&self) -> State {
        let gdn = self.layers.iter().filter(|l| matches!(l.mixer, Mixer::Linear(_))).count();
        let full = LAYERS - gdn;
        State {
            conv: vec![None; gdn],
            ssm: vec![None; gdn],
            keys: vec![Vec::new(); full],
            values: vec![Vec::new(); full],
            pos: 0,
        }
    }

    /// 출력 헤드: 전체 어휘 (입력 임베딩 표 전체를 F32로, 약 1GB)
    pub fn full_head(&self) -> Result<Head> {
        let ids: Vec<u32> = (0..VOCAB as u32).collect();
        let mut rows = Vec::with_capacity(VOCAB);
        for chunk in ids.chunks(16_384) {
            rows.push(self.tokens.rows(chunk)?);
        }
        Ok(Head::Full(Tensor::cat(&rows, 0)?))
    }

    /// 출력 헤드: 이 토큰들만 (지시서의 비대칭 Trimming. 입력 쪽 어휘는 그대로)
    pub fn trimmed_head(&self, ids: Vec<u32>) -> Result<Head> {
        let weight = self.tokens.rows(&ids)?;
        Ok(Head::Trimmed { ids, weight })
    }

    /// 새 토큰들을 넣고 마지막 위치의 은닉(정규화 후)을 돌려준다
    pub fn forward(&self, ids: &[u32], st: &mut State) -> Result<Tensor> {
        let x = self.forward_hidden(ids, st)?;
        let t = x.dim(0)?;
        rms_norm_1p(&x.get(t - 1)?.unsqueeze(0)?, &self.norm)?.squeeze(0)
    }

    /// 모든 위치의 은닉(정규화 후, t x HIDDEN). 교사 강제 NLL을 잴 때 쓴다
    pub fn forward_all(&self, ids: &[u32], st: &mut State) -> Result<Tensor> {
        rms_norm_1p(&self.forward_hidden(ids, st)?, &self.norm)
    }

    /// 전체 어휘 헤드로 잰 평균 음의 로그우도 (ids[1..]을 앞 토큰들로 예측)
    pub fn nll(&self, ids: &[u32], head: &Head) -> Result<f32> {
        let Head::Full(w) = head else { return Err(candle_core::Error::Msg("NLL은 전체 헤드로만 잰다".into())) };
        let mut st = self.new_state();
        let h = self.forward_all(&ids[..ids.len() - 1], &mut st)?;
        let logits = h.matmul(&w.t()?)?;
        let lsm = candle_nn::ops::log_softmax(&logits, D::Minus1)?;
        let targets = Tensor::from_vec(ids[1..].to_vec(), (ids.len() - 1, 1), &self.device)?;
        let picked = lsm.gather(&targets, D::Minus1)?;
        Ok(-picked.mean_all()?.to_scalar::<f32>()?)
    }

    fn forward_hidden(&self, ids: &[u32], st: &mut State) -> Result<Tensor> {
        let t = ids.len();
        let mut x = self.tokens.rows(ids)?;
        let (mut gi, mut fi) = (0, 0);
        for layer in &self.layers {
            let h = rms_norm_1p(&x, &layer.input_norm)?;
            let mixed = match &layer.mixer {
                Mixer::Linear(g) => {
                    let out = self.gdn(g, &h, &mut st.conv[gi], &mut st.ssm[gi])?;
                    gi += 1;
                    out
                }
                Mixer::Full(a) => {
                    let out = self.attention(a, &h, st.pos, &mut st.keys[fi], &mut st.values[fi])?;
                    fi += 1;
                    out
                }
            };
            x = (x + mixed)?;
            let h = rms_norm_1p(&x, &layer.post_norm)?;
            let gate = layer.gate.forward(&h)?;
            let up = layer.up.forward(&h)?;
            let glu = if !gate.device().is_cpu()
                && crate::utils::accel_op_ok(gate.device(), crate::utils::AccelOp::DecoderOps)
            {
                candle_core::decoder_ops::silu_mul(&gate, &up)
                    .or_else(|_| &gate.silu()? * &up)?
            } else {
                (&gate.silu()? * &up)?
            };
            let y = layer.down.forward(&glu)?;
            x = (x + y)?;
        }
        st.pos += t;
        Ok(x)
    }

    fn gdn(&self, g: &Gdn, h: &Tensor, conv: &mut Option<Tensor>, ssm: &mut Option<Tensor>) -> Result<Tensor> {
        let t = h.dim(0)?;
        let mixed = g.in_qkv.forward(h)?.unsqueeze(0)?;
        let z = g.in_z.forward(h)?.reshape((t * LIN_HEADS, LIN_DIM))?;
        let b = g.in_b.forward(h)?.unsqueeze(0)?;
        let a = g.in_a.forward(h)?.unsqueeze(0)?;
        let cfg = candle_core::delta_rule::GdnConfig::new(LIN_HEADS, LIN_HEADS, LIN_DIM, LIN_DIM);
        let gdn_out = candle_core::delta_rule::fused_gated_delta_rule(
            &mixed,
            &g.conv_w,
            None,
            conv.as_ref(),
            &a,
            &b,
            &g.neg_a,
            &g.dt_bias,
            ssm.as_ref(),
            &cfg,
        )?;
        *conv = Some(gdn_out.conv_state);
        *ssm = Some(gdn_out.recurrent_state);
        let core_2d = gdn_out.out.reshape((t * LIN_HEADS, LIN_DIM))?;
        let gated = if !core_2d.device().is_cpu()
            && crate::utils::accel_op_ok(core_2d.device(), crate::utils::AccelOp::DecoderOps)
        {
            candle_core::decoder_ops::rmsnorm_fused(&core_2d, &g.norm_w, EPS as f32, false)
                .and_then(|normed| candle_core::decoder_ops::silu_mul(&z, &normed))
                .or_else(|_| {
                    let ms = core_2d.sqr()?.mean_keepdim(D::Minus1)?;
                    let normed = core_2d.broadcast_div(&(ms + EPS)?.sqrt()?)?.broadcast_mul(&g.norm_w)?;
                    &normed * &z.silu()?
                })?
        } else {
            let ms = core_2d.sqr()?.mean_keepdim(D::Minus1)?;
            let normed = core_2d.broadcast_div(&(ms + EPS)?.sqrt()?)?.broadcast_mul(&g.norm_w)?;
            (&normed * &z.silu()?)?
        };
        g.out.forward(&gated.reshape((t, KEY_DIM))?)
    }

    fn rope_rows(&self, pos0: usize, t: usize) -> (Vec<f32>, Vec<f32>) {
        let half = ROTARY_DIM / 2;
        let mut cos = vec![0f32; t * half];
        let mut sin = vec![0f32; t * half];
        for i in 0..t {
            for (j, &f) in self.rope_inv.iter().enumerate() {
                let (s, c) = ((pos0 + i) as f32 * f).sin_cos();
                cos[i * half + j] = c;
                sin[i * half + j] = s;
            }
        }
        (cos, sin)
    }

    fn attention(&self, a: &Attn, h: &Tensor, pos0: usize, keys: &mut Vec<f32>, values: &mut Vec<f32>) -> Result<Tensor> {
        let t = h.dim(0)?;
        let qg = a.q.forward(h)?.reshape((t, HEADS, 2 * HEAD_DIM))?;
        let q = qg.narrow(D::Minus1, 0, HEAD_DIM)?;
        let gate = qg.narrow(D::Minus1, HEAD_DIM, HEAD_DIM)?.reshape((t, HEADS * HEAD_DIM))?;
        let q: Vec<f32> = rms_norm_1p(&q, &a.q_norm)?.flatten_all()?.to_vec1()?;
        let k: Vec<f32> = rms_norm_1p(&a.k.forward(h)?.reshape((t, KV_HEADS, HEAD_DIM))?, &a.k_norm)?.flatten_all()?.to_vec1()?;
        let v: Vec<f32> = a.v.forward(h)?.flatten_all()?.to_vec1()?;
        let (cos, sin) = self.rope_rows(pos0, t);
        let half = ROTARY_DIM / 2;
        let rotate = |x: &mut [f32], i: usize| {
            for j in 0..half {
                let (c, s) = (cos[i * half + j], sin[i * half + j]);
                let (x1, x2) = (x[j], x[j + half]);
                x[j] = x1 * c - x2 * s;
                x[j + half] = x2 * c + x1 * s;
            }
        };
        let mut q = q;
        let mut k = k;
        for i in 0..t {
            for hd in 0..HEADS {
                rotate(&mut q[(i * HEADS + hd) * HEAD_DIM..(i * HEADS + hd + 1) * HEAD_DIM], i);
            }
            for hd in 0..KV_HEADS {
                rotate(&mut k[(i * KV_HEADS + hd) * HEAD_DIM..(i * KV_HEADS + hd + 1) * HEAD_DIM], i);
            }
        }
        keys.extend_from_slice(&k);
        values.extend_from_slice(&v);
        let n = keys.len() / (KV_HEADS * HEAD_DIM);
        let mut out = vec![0f32; t * HEADS * HEAD_DIM];
        let cfg = premium_discount_cpu_kernels::CpuStreamAttnCfg {
            num_heads: HEADS,
            num_kv_heads: KV_HEADS,
            head_dim: HEAD_DIM,
            scale: 1.0 / (HEAD_DIM as f32).sqrt(),
            causal: true,
            past_offset: pos0,
        };
        premium_discount_cpu_kernels::stream_attention_f32(&q, keys, values, t, n, &cfg, &mut out);
        let out = Tensor::from_vec(out, (t, HEADS * HEAD_DIM), &self.device)?;
        let out = (out * candle_nn::ops::sigmoid(&gate)?)?;
        a.o.forward(&out)
    }

    /// 마지막 은닉 → 다음 토큰 (그리디). 잘라 낸 헤드면 그 안에서 고른다.
    /// - `allow = Some(ids)` with a small whitelist scores ONLY the allowed rows (avoiding full-vocab GEMV).
    /// - `allow = None` on GPU uses Pack A/C fused tile argmax (`fused_linear_argmax_cs`) with 0-byte logits materialization.
    pub fn next_token(&self, h: &Tensor, head: &Head, allow: Option<&[u32]>) -> Result<(u32, f32)> {
        if let Some(allowed) = allow {
            if !allowed.is_empty() && allowed.len() <= 64 {
                let h_row = h.unsqueeze(0)?;
                let mut best = (0u32, f32::NEG_INFINITY);
                match head {
                    Head::Full(w) => {
                        let rows = Tensor::from_slice(allowed, allowed.len(), w.device())?;
                        let sub = w.index_select(&rows, 0)?;
                        let logits: Vec<f32> = h_row.matmul(&sub.t()?)?.squeeze(0)?.to_vec1()?;
                        for (&id, &l) in allowed.iter().zip(&logits) {
                            if l > best.1 || (l == best.1 && id < best.0) {
                                best = (id, l);
                            }
                        }
                        return Ok(best);
                    }
                    Head::Trimmed { ids, weight } => {
                        let mut idxs = Vec::with_capacity(allowed.len());
                        let mut matched_ids = Vec::with_capacity(allowed.len());
                        for (i, &id) in ids.iter().enumerate() {
                            if allowed.contains(&id) {
                                idxs.push(i as u32);
                                matched_ids.push(id);
                            }
                        }
                        if idxs.is_empty() {
                            return Ok(best);
                        }
                        let rows = Tensor::from_slice(&idxs, idxs.len(), weight.device())?;
                        let sub = weight.index_select(&rows, 0)?;
                        let logits: Vec<f32> = h_row.matmul(&sub.t()?)?.squeeze(0)?.to_vec1()?;
                        for (&id, &l) in matched_ids.iter().zip(&logits) {
                            if l > best.1 || (l == best.1 && id < best.0) {
                                best = (id, l);
                            }
                        }
                        return Ok(best);
                    }
                }
            }
        }
        if allow.is_none()
            && !h.device().is_cpu()
            && crate::utils::accel_op_ok(h.device(), crate::utils::AccelOp::LmHeadArgmax)
        {
            let h_row = h.unsqueeze(0)?;
            let (weight, ids_opt) = match head {
                Head::Full(w) => (w, None),
                Head::Trimmed { ids, weight } => (weight, Some(ids.as_slice())),
            };
            if let Ok(out) = candle_core::lm_head::fused_linear_argmax_cs(&h_row, weight, None, None, None) {
                let local_id = out.ids.to_vec1::<u32>()?[0];
                let val = out
                    .max_vals
                    .and_then(|t| t.to_vec1::<f32>().ok())
                    .and_then(|v| v.first().copied())
                    .unwrap_or(0.0);
                let token_id = ids_opt.map_or(local_id, |v| v[local_id as usize]);
                return Ok((token_id, val));
            }
        }
        let (logits, ids) = head.logits(h)?;
        let id_of = |i: usize| ids.map_or(i as u32, |v| v[i]);
        let allow_set: Option<std::collections::HashSet<u32>> =
            allow.map(|a| a.iter().copied().collect());
        let mut best = (0u32, f32::NEG_INFINITY);
        for (i, &l) in logits.iter().enumerate() {
            let id = id_of(i);
            if let Some(ref set) = allow_set {
                if !set.contains(&id) {
                    continue;
                }
            }
            if l > best.1 || (l == best.1 && id < best.0) {
                best = (id, l);
            }
        }
        Ok(best)
    }

    /// 전체 어휘 로짓 (검증용: 헤드가 Full이어야 한다)
    pub fn logits(&self, h: &Tensor, head: &Head) -> Result<Vec<f32>> {
        Ok(head.logits(h)?.0)
    }

    /// 프롬프트 뒤로 그리디 생성 (`stop`이 참이면 멈춘다)
    pub fn generate(&self, prompt: &[u32], max_new: usize, head: &Head, stop: &dyn Fn(&[u32]) -> bool) -> Result<Vec<u32>> {
        let mut st = self.new_state();
        let mut h = self.forward(prompt, &mut st)?;
        let mut out = Vec::new();
        for _ in 0..max_new {
            let (id, _) = self.next_token(&h, head, None)?;
            out.push(id);
            if stop(&out) {
                break;
            }
            h = self.forward(&[id], &mut st)?;
        }
        Ok(out)
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// 토큰 id 목록을 글로
    pub fn decode(&self, ids: &[u32]) -> String {
        self.tokenizer.decode(ids, false).unwrap_or_default()
    }

    pub fn encode(&self, text: &str) -> Vec<u32> {
        self.tokenizer.encode(text, false).map(|e| e.get_ids().to_vec()).unwrap_or_default()
    }

    /// 특수 토큰 id (예: "<|im_end|>")
    pub fn token_id(&self, token: &str) -> Option<u32> {
        self.tokenizer.token_to_id(token)
    }
}

/// Qwen3.5 채팅 템플릿(chat_template.jinja)으로 만든 프롬프트. 생각 모드는 끈다 (enable_thinking=False와 같은 결과)
pub fn chat_prompt(system: &str, user: &str) -> String {
    format!("<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{user}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n")
}

/// GPT-2 바이트 수준 BPE의 문자 → 바이트 표 (토큰 문자열을 실제 바이트로 되돌린다)
fn unicode_to_byte() -> HashMap<char, u8> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    bs.iter().zip(&cs).map(|(b, c)| (char::from_u32(*c).expect("valid"), *b as u8)).collect()
}

/// 출력 어휘 계획: 반드시 넣는 토큰과, 자리가 남으면 채우는 토큰(우선순위 순)
#[derive(Debug, Clone)]
pub struct VocabPlan {
    pub required: Vec<u32>,
    pub fill: Vec<u32>,
    pub size: usize,
}

impl VocabPlan {
    /// 요청마다의 출력 어휘: 반드시 넣는 토큰 + 이 요청의 페이지 토큰(모델이 옮겨 쓸 단어) + 남는 자리만큼 채우는 토큰.
    /// 크기는 언제나 `size`(64의 배수)다. 페이지 토큰이 많으면 채우는 토큰을 그만큼 덜 넣는다
    pub fn ids_for(&self, page_ids: &[u32]) -> Vec<u32> {
        let mut set: std::collections::BTreeSet<u32> = self.required.iter().copied().collect();
        for id in page_ids {
            if set.len() >= self.size {
                break;
            }
            set.insert(*id);
        }
        for id in &self.fill {
            if set.len() >= self.size {
                break;
            }
            set.insert(*id);
        }
        set.into_iter().collect()
    }
}

/// 출력 어휘 마이닝 (지시서의 lm_head 비대칭 Trimming): 크기는 64의 배수.
/// 반드시 넣는 것 → (1) 바이트 토큰 256개(어떤 UTF-8도 바이트로 낼 수 있는 폴백) (2) 특수 토큰
/// (3) 한글 완성형만으로 된 토큰과 다바이트 문자 조각 (4) `corpus`(JSON 스키마·도메인 문장)를 토큰화해 나온 토큰.
/// 남는 자리는 병합 순위가 높은(id가 작은) 토큰으로 채운다. 입력 쪽 어휘는 건드리지 않는다.
pub fn mine_vocab(tokenizer: &Tokenizer, corpus: &[&str], size: usize) -> std::result::Result<Vec<u32>, String> {
    Ok(plan_vocab(tokenizer, corpus, size)?.ids_for(&[]))
}

/// `mine_vocab`의 계획만 (요청마다 페이지 토큰을 더할 때 쓴다)
pub fn plan_vocab(tokenizer: &Tokenizer, corpus: &[&str], size: usize) -> std::result::Result<VocabPlan, String> {
    if !size.is_multiple_of(64) {
        return Err(format!("어휘 크기 {size}는 64의 배수여야 합니다"));
    }
    let u2b = unicode_to_byte();
    let vocab = tokenizer.get_vocab(true);
    let mut keep = std::collections::BTreeSet::new();
    for (tok, id) in &vocab {
        let chars: Vec<char> = tok.chars().collect();
        // 특수 토큰 (<|im_end|> 등): 바이트 표에 없는 문자를 포함한다
        if !chars.iter().all(|c| u2b.contains_key(c)) {
            if tok.starts_with('<') && tok.ends_with('>') {
                keep.insert(*id);
            }
            continue;
        }
        if chars.len() == 1 {
            keep.insert(*id); // 바이트 토큰
            continue;
        }
        let bytes: Vec<u8> = chars.iter().map(|c| u2b[c]).collect();
        match std::str::from_utf8(&bytes) {
            Ok(s) => {
                let core = s.trim();
                if !core.is_empty() && core.chars().all(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c)) {
                    keep.insert(*id); // 한글 완성형
                }
            }
            Err(_) => {
                keep.insert(*id); // 다바이트 문자 조각
            }
        }
    }
    for text in corpus {
        let enc = tokenizer.encode(*text, false).map_err(|e| e.to_string())?;
        keep.extend(enc.get_ids().iter().copied());
    }
    if keep.len() > size {
        return Err(format!("반드시 넣을 토큰이 {}개로 {size}개를 넘습니다", keep.len()));
    }
    let mut fill: Vec<u32> = vocab.values().copied().filter(|id| !keep.contains(id)).collect();
    fill.sort_unstable();
    Ok(VocabPlan { required: keep.into_iter().collect(), fill, size })
}
