//! embeddinggemma-2 텍스트 백본 (Gemma 4 텍스트 구조, 토큰별 PLE 테이블 없음).
//!
//! 구조와 연산은 config.json(text_config)과 transformers의 Gemma4TextModel을 따른다.
//! 가중치는 model.safetensors의 `language_model.*`만 읽는다. 이미지·오디오 인코더는 읽지 않는다.
//!
//! 체크포인트에는 `embed_tokens_per_layer`(토큰별 PLE 테이블)가 없다. 그래서 PLE는 문맥 투영만 쓴다
//! (Gemma4TextModel의 `input_ids = None` 경로와 같다).

use std::path::Path;

use candle_core::{DType, Device, IndexOp, Result, Tensor, D};
// 주의: candle의 Tensor 연산자(*, +)는 Result가 아니라 Tensor를 돌려준다. 실패 가능한 연산은 메서드로 쓴다.
use candle_nn::{ops, Linear, Module, VarBuilder};

use crate::embeddinggemma_tokens::TokenTable;
use crate::qcache::QCache;
use crate::quant::{load_lin as lin, Kernel, Lin, Proj, QuantRecipe};
use crate::embeddinggemma::{
    attention_kind, Attention, OUTPUT_DIM, ROPE_THETA_FULL, ROPE_THETA_SLIDING, SLIDING_WINDOW, TEXT_GLOBAL_HEAD_DIM,
    TEXT_GLOBAL_KV_HEADS, TEXT_HEAD_DIM, TEXT_HIDDEN_SIZE, TEXT_NUM_HEADS, TEXT_NUM_KV_HEADS,
    TEXT_NUM_LAYERS, TEXT_VOCAB_SIZE,
};

const INTERMEDIATE_SIZE: usize = 2048;
const PLE_DIM: usize = 512;
const RMS_EPS: f64 = 1e-6;
/// 허용되지 않은 위치의 점수 (softmax에서 0이 된다)
const MASKED: f32 = -1e9;

/// Gemma RMS norm: x / sqrt(mean(x^2) + eps) * weight. weight가 없으면 스케일 없이 정규화만 한다.
struct RmsNorm {
    weight: Option<Tensor>,
}

impl RmsNorm {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        if let Some(w) = &self.weight {
            if !x.device().is_cpu()
                && crate::utils::accel_op_ok(x.device(), crate::utils::AccelOp::DecoderOps)
            {
                if let Ok(y) = candle_core::decoder_ops::rmsnorm_fused(x, w, RMS_EPS as f32, false) {
                    return Ok(y);
                }
            }
        }
        let mean_sq = x.sqr()?.mean_keepdim(D::Minus1)?;
        let inv = mean_sq.affine(1.0, RMS_EPS)?.powf(-0.5)?;
        let y = x.broadcast_mul(&inv)?;
        match &self.weight {
            Some(w) => y.broadcast_mul(w),
            None => Ok(y),
        }
    }
}

/// RoPE의 cos, sin 표 (seq x head_dim). 앞 절반과 뒤 절반이 같다 (transformers의 cat(freqs, freqs))
fn rope_tables(theta: f64, head_dim: usize, seq: usize, dev: &Device) -> Result<(Tensor, Tensor)> {
    let half = head_dim / 2;
    let inv: Vec<f32> = (0..half)
        .map(|i| (1.0 / theta.powf((2 * i) as f64 / head_dim as f64)) as f32)
        .collect();
    let mut cos = vec![0f32; seq * head_dim];
    let mut sin = vec![0f32; seq * head_dim];
    for t in 0..seq {
        let base = t * head_dim;
        let tf = t as f32;
        for (i, &f) in inv.iter().enumerate() {
            let (s, c) = (tf * f).sin_cos();
            cos[base + i] = c;
            cos[base + half + i] = c;
            sin[base + i] = s;
            sin[base + half + i] = s;
        }
    }
    Ok((
        Tensor::from_vec(cos, (seq, head_dim), dev)?,
        Tensor::from_vec(sin, (seq, head_dim), dev)?,
    ))
}

/// x: [b, t, h, d]. cos, sin: [t, d]. 마지막 차원을 절반씩 돌린다 (rotate_half)
fn apply_rope(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    let d = x.dim(D::Minus1)?;
    let x1 = x.narrow(D::Minus1, 0, d / 2)?;
    let x2 = x.narrow(D::Minus1, d / 2, d / 2)?;
    let rotated = Tensor::cat(&[&x2.neg()?, &x1], D::Minus1)?;
    let cos = cos.unsqueeze(0)?.unsqueeze(2)?;
    let sin = sin.unsqueeze(0)?.unsqueeze(2)?;
    x.broadcast_mul(&cos)?.broadcast_add(&rotated.broadcast_mul(&sin)?)
}

/// KV head를 query head 수만큼 반복한다 (GQA). x: [b, kv, t, d]
fn repeat_kv(x: Tensor, reps: usize) -> Result<Tensor> {
    if reps == 1 {
        return Ok(x);
    }
    let (b, kv, t, d) = x.dims4()?;
    x.unsqueeze(2)?
        .expand((b, kv, reps, t, d))?
        .contiguous()?
        .reshape((b, kv * reps, t, d))
}

/// 양방향 모드의 슬라이딩 창 (config의 use_bidirectional_attention="all"이면 sliding_window가 257이 된다)
const BIDIRECTIONAL_SLIDING_WINDOW: usize = 257;

/// 어텐션 마스크 (t x t). 허용되면 0, 아니면 MASKED.
/// bidirectional=false(인과): k <= q, 슬라이딩은 q - k < 512.
/// bidirectional=true: 제한 없음, 슬라이딩은 |q - k| <= 257 (transformers의 bidirectional 마스크와 같다).
fn attention_mask(t: usize, sliding: bool, bidirectional: bool, dev: &Device) -> Result<Tensor> {
    let mut v = vec![0f32; t * t];
    for q in 0..t {
        for k in 0..t {
            let causal_ok = bidirectional || k <= q;
            let window_ok = !sliding
                || if bidirectional {
                    q.abs_diff(k) <= BIDIRECTIONAL_SLIDING_WINDOW
                } else {
                    k + SLIDING_WINDOW > q
                };
            if !(causal_ok && window_ok) {
                v[q * t + k] = MASKED;
            }
        }
    }
    Tensor::from_vec(v, (t, t), dev)
}

struct SelfAttention {
    q_proj: Lin,
    k_proj: Lin,
    v_proj: Lin,
    o_proj: Lin,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    v_norm: RmsNorm,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
}

impl SelfAttention {
    fn forward(&self, x: &Tensor, cos: &Tensor, sin: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let (b, t, _) = x.dims3()?;
        let q = self.q_proj.forward(x)?.reshape((b, t, self.heads, self.head_dim))?;
        let q = apply_rope(&self.q_norm.forward(&q)?, cos, sin)?;

        let k = self.k_proj.forward(x)?.reshape((b, t, self.kv_heads, self.head_dim))?;
        let k = apply_rope(&self.k_norm.forward(&k)?, cos, sin)?;

        let v = self.v_proj.forward(x)?.reshape((b, t, self.kv_heads, self.head_dim))?;
        let v = self.v_norm.forward(&v)?;

        // When unmasked on CPU F32 (e.g. bidirectional full/short-window attention), stream directly in SHD layout
        // without materializing a [B, H, T, T] score matrix or repeating KV heads.
        if mask.is_none() && b == 1 && x.device().is_cpu() && x.dtype() == DType::F32 {
            let q_vec = q.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let k_vec = k.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let v_vec = v.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let mut out = vec![0.0f32; t * self.heads * self.head_dim];
            let cfg = premium_discount_cpu_kernels::CpuStreamAttnCfg {
                num_heads: self.heads,
                num_kv_heads: self.kv_heads,
                head_dim: self.head_dim,
                scale: 1.0,
                causal: false,
                past_offset: 0,
            };
            premium_discount_cpu_kernels::stream_attention_f32(&q_vec, &k_vec, &v_vec, t, t, &cfg, &mut out);
            let out = Tensor::from_vec(out, (1, t, self.heads * self.head_dim), x.device())?;
            return self.o_proj.forward(&out);
        }

        let q = q.transpose(1, 2)?.contiguous()?;
        let k = k.transpose(1, 2)?.contiguous()?;
        let v = v.transpose(1, 2)?.contiguous()?;

        if mask.is_none()
            && self.head_dim <= 256
            && !x.device().is_cpu()
            && crate::utils::accel_op_ok(x.device(), crate::utils::AccelOp::FlashAttn)
        {
            let cfg = candle_nn::flash_attn::FlashAttnConfig::new(1.0);
            if let Ok(out) = candle_nn::flash_attn::flash_attn_bhsd(&q, &k, &v, &cfg) {
                let out = out.transpose(1, 2)?.reshape((b, t, self.heads * self.head_dim))?;
                return self.o_proj.forward(&out);
            }
        }

        let reps = self.heads / self.kv_heads;
        let k = repeat_kv(k, reps)?;
        let v = repeat_kv(v, reps)?;

        // 점수 배율은 1.0 (config의 query_pre_attn_scalar 대신 q_norm/k_norm을 쓴다)
        let scores = q.matmul(&k.transpose(2, 3)?.contiguous()?)?;
        let scores = match mask {
            Some(m) => scores.broadcast_add(m)?,
            None => scores,
        };
        let probs = ops::softmax_last_dim(&scores)?;
        let out = probs.matmul(&v)?;
        let out = out.transpose(1, 2)?.reshape((b, t, self.heads * self.head_dim))?;
        self.o_proj.forward(&out)
    }
}

struct Layer {
    kind: Attention,
    attn: SelfAttention,
    gate: Lin,
    up: Lin,
    down: Lin,
    input_norm: RmsNorm,
    post_attn_norm: RmsNorm,
    pre_ff_norm: RmsNorm,
    post_ff_norm: RmsNorm,
    ple_gate: Lin,
    ple_proj: Lin,
    post_ple_norm: RmsNorm,
    layer_scalar: Tensor,
}

impl Layer {
    fn forward(&self, h: &Tensor, ple: &Tensor, cos: &Tensor, sin: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let x = self.input_norm.forward(h)?;
        let x = self.attn.forward(&x, cos, sin, mask)?;
        let x = self.post_attn_norm.forward(&x)?;
        let h = h.broadcast_add(&x)?;

        let x = self.pre_ff_norm.forward(&h)?;
        let x = self.gate.forward(&x)?.gelu()?.broadcast_mul(&self.up.forward(&x)?)?;
        let x = self.down.forward(&x)?;
        let x = self.post_ff_norm.forward(&x)?;
        let h = h.broadcast_add(&x)?;

        let g = self.ple_gate.forward(&h)?.gelu()?;
        let g = g.broadcast_mul(ple)?;
        let p = self.post_ple_norm.forward(&self.ple_proj.forward(&g)?)?;
        let h = h.broadcast_add(&p)?;
        h.broadcast_mul(&self.layer_scalar)
    }
}

/// 캐시 안 이름 (층 번호 + 원본 텐서의 마지막 이름)
fn key(layer: usize, name: &str) -> String {
    format!("layers.{layer}.{name}")
}

/// 텍스트 백본. 입력 토큰 id → 정규화 전 hidden (1 x t x 512)
pub struct TextModel {
    /// 입력 임베딩 표. 파일 매핑에 두고 필요한 행만 읽는다 (embeddinggemma_tokens.rs)
    tokens: TokenTable,
    ple_proj: Linear,
    ple_norm: RmsNorm,
    layers: Vec<Layer>,
    norm: RmsNorm,
    /// 512 -> 768 출력 투영 (language_model.embedding_projection, bias 없음)
    projection: Linear,
    device: Device,
    /// 양방향 어텐션 여부. config.json에는 없다. 기본값은 양방향이다: Python 참조 구현(use_bidirectional_attention="all")과 일치하지만,
    /// 공식 embedding_gemma2 구현은 확인하지 못했다
    bidirectional: bool,
}

impl TextModel {
    /// model.safetensors에서 텍스트 백본의 텐서만 읽는다. 층 가중치는 f32로 올리고, 입력 임베딩 표는 파일 매핑에 둔다.
    pub fn load(model_file: &Path, device: &Device, bidirectional: bool) -> Result<Self> {
        // SAFETY: 파일은 읽기 전용으로 매핑하고, 로드 중에 바꾸지 않는다고 가정한다
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[model_file], DType::F32, device)? };
        let tokens = TokenTable::open(model_file, TEXT_VOCAB_SIZE, TEXT_HIDDEN_SIZE, device)?;
        Self::from_vb(&vb, tokens, device, bidirectional, &QuantRecipe::Off, Kernel::default(), None)
    }

    /// 루트 VarBuilder에서 `language_model.*` 텐서를 읽는다. 여러 타워가 같은 파일을 함께 쓸 때 쓴다.
    /// 입력 임베딩 표(`embed_tokens`)는 VarBuilder로 읽지 않는다 (F32로 펼쳐지므로). `tokens`로 받는다.
    /// 층의 투영 9종은 `recipe`가 정한 대로 한 번 양자화한다 (quant.rs). 정규화·스케일·출력 투영·PLE 전역 투영은 F32 그대로다.
    pub fn from_vb(
        vb: &VarBuilder,
        tokens: TokenTable,
        device: &Device,
        bidirectional: bool,
        recipe: &QuantRecipe,
        kernel: Kernel,
        mut cache: Option<&mut QCache>,
    ) -> Result<Self> {
        let lm = vb.pp("language_model");

        let ple = lm.pp("ple");
        let ple_proj = Linear::new(
            ple.pp("per_layer_model_projection")
                .get((TEXT_NUM_LAYERS * PLE_DIM, TEXT_HIDDEN_SIZE), "weight")?,
            None,
        );
        let ple_norm = RmsNorm {
            weight: Some(ple.pp("per_layer_projection_norm").get(PLE_DIM, "weight")?),
        };
        let norm = RmsNorm {
            weight: Some(lm.pp("norm").get(TEXT_HIDDEN_SIZE, "weight")?),
        };
        let projection = Linear::new(
            vb.get((OUTPUT_DIM, TEXT_HIDDEN_SIZE), "language_model.embedding_projection.weight")?,
            None,
        );

        let mut layers = Vec::with_capacity(TEXT_NUM_LAYERS);
        for i in 0..TEXT_NUM_LAYERS {
            let l = lm.pp("layers").pp(i.to_string());
            let kind = attention_kind(i).expect("layer index is in range");
            let (head_dim, kv_heads) = match kind {
                Attention::Full => (TEXT_GLOBAL_HEAD_DIM, TEXT_GLOBAL_KV_HEADS),
                Attention::Sliding => (TEXT_HEAD_DIM, TEXT_NUM_KV_HEADS),
            };
            let a = l.pp("self_attn");
            let q_dim = TEXT_NUM_HEADS * head_dim;
            let kv_dim = kv_heads * head_dim;
            let attn = SelfAttention {
                q_proj: lin(&a.pp("q_proj"), (q_dim, TEXT_HIDDEN_SIZE), recipe.bits(i, Proj::Q), kernel, &mut cache, key(i, "q_proj"))?,
                k_proj: lin(&a.pp("k_proj"), (kv_dim, TEXT_HIDDEN_SIZE), recipe.bits(i, Proj::K), kernel, &mut cache, key(i, "k_proj"))?,
                v_proj: lin(&a.pp("v_proj"), (kv_dim, TEXT_HIDDEN_SIZE), recipe.bits(i, Proj::V), kernel, &mut cache, key(i, "v_proj"))?,
                o_proj: lin(&a.pp("o_proj"), (TEXT_HIDDEN_SIZE, q_dim), recipe.bits(i, Proj::O), kernel, &mut cache, key(i, "o_proj"))?,
                q_norm: RmsNorm {
                    weight: Some(a.pp("q_norm").get(head_dim, "weight")?),
                },
                k_norm: RmsNorm {
                    weight: Some(a.pp("k_norm").get(head_dim, "weight")?),
                },
                v_norm: RmsNorm { weight: None },
                heads: TEXT_NUM_HEADS,
                kv_heads,
                head_dim,
            };
            let m = l.pp("mlp");
            let hidden_norm = |name: &str| -> Result<RmsNorm> {
                Ok(RmsNorm {
                    weight: Some(l.pp(name).get(TEXT_HIDDEN_SIZE, "weight")?),
                })
            };
            let p = l.pp("ple_block");
            layers.push(Layer {
                kind,
                attn,
                gate: lin(&m.pp("gate_proj"), (INTERMEDIATE_SIZE, TEXT_HIDDEN_SIZE), recipe.bits(i, Proj::Gate), kernel, &mut cache, key(i, "gate_proj"))?,
                up: lin(&m.pp("up_proj"), (INTERMEDIATE_SIZE, TEXT_HIDDEN_SIZE), recipe.bits(i, Proj::Up), kernel, &mut cache, key(i, "up_proj"))?,
                down: lin(&m.pp("down_proj"), (TEXT_HIDDEN_SIZE, INTERMEDIATE_SIZE), recipe.bits(i, Proj::Down), kernel, &mut cache, key(i, "down_proj"))?,
                input_norm: hidden_norm("input_layernorm")?,
                post_attn_norm: hidden_norm("post_attention_layernorm")?,
                pre_ff_norm: hidden_norm("pre_feedforward_layernorm")?,
                post_ff_norm: hidden_norm("post_feedforward_layernorm")?,
                ple_gate: lin(
                    &p.pp("per_layer_input_gate"),
                    (PLE_DIM, TEXT_HIDDEN_SIZE),
                    recipe.bits(i, Proj::PleGate),
                    kernel,
                    &mut cache,
                    key(i, "per_layer_input_gate"),
                )?,
                ple_proj: lin(
                    &p.pp("per_layer_projection"),
                    (TEXT_HIDDEN_SIZE, PLE_DIM),
                    recipe.bits(i, Proj::PleProj),
                    kernel,
                    &mut cache,
                    key(i, "per_layer_projection"),
                )?,
                post_ple_norm: RmsNorm {
                    weight: Some(p.pp("post_per_layer_input_norm").get(TEXT_HIDDEN_SIZE, "weight")?),
                },
                layer_scalar: l.get(1, "layer_scalar")?,
            });
        }

        Ok(Self {
            tokens,
            ple_proj,
            ple_norm,
            layers,
            norm,
            projection,
            device: device.clone(),
            bidirectional,
        })
    }

    /// 토큰 id 목록 → 입력 임베딩 (t x 512). Gemma 규칙에 따라 sqrt(512)를 곱한다.
    pub fn token_embeddings(&self, ids: &[u32]) -> Result<Tensor> {
        self.tokens.rows(ids)?.affine((TEXT_HIDDEN_SIZE as f64).sqrt(), 0.0)
    }

    /// 양자화된 층 투영 (캐시에 쓸 때). 이름은 `lin`이 캐시에서 찾는 이름과 같다
    pub fn quantized_tensors(&self) -> Vec<(String, &candle_core::quantized::QTensor)> {
        let mut out = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            let named: [(&str, &Lin); 9] = [
                ("q_proj", &l.attn.q_proj),
                ("k_proj", &l.attn.k_proj),
                ("v_proj", &l.attn.v_proj),
                ("o_proj", &l.attn.o_proj),
                ("gate_proj", &l.gate),
                ("up_proj", &l.up),
                ("down_proj", &l.down),
                ("per_layer_input_gate", &l.ple_gate),
                ("per_layer_projection", &l.ple_proj),
            ];
            for (name, lin) in named {
                if let Some(q) = lin.qtensor() {
                    out.push((key(i, name), q));
                }
            }
        }
        out
    }

    /// 층 투영 가중치가 메모리에서 차지하는 크기 (바이트). 양자화 효과를 재는 데 쓴다
    pub fn layer_weight_bytes(&self) -> usize {
        self.layers
            .iter()
            .map(|l| {
                [&l.attn.q_proj, &l.attn.k_proj, &l.attn.v_proj, &l.attn.o_proj, &l.gate, &l.up, &l.down, &l.ple_gate, &l.ple_proj]
                    .iter()
                    .map(|x| x.weight_bytes())
                    .sum::<usize>()
            })
            .sum()
    }

    /// 이 토큰들의 임베딩 행을 메모리에 상주시킨다 (특수 토큰, 역할 접두어). 결과 값은 바뀌지 않는다.
    pub fn pin_tokens(&mut self, ids: &[u32]) -> Result<()> {
        self.tokens.pin(ids)
    }

    /// 토큰 id 목록 → 최종 hidden (1 x t x 512). 풀링과 투영은 호출하는 쪽이 한다.
    pub fn forward(&self, ids: &[u32]) -> Result<Tensor> {
        let h = self.token_embeddings(ids)?.unsqueeze(0)?;
        self.forward_embeds(&h)
    }

    /// 입력 임베딩 (1 x t x 512) → 최종 hidden (1 x t x 512). 이미지·오디오 토큰은 호출하는 쪽이 임베딩 자리에 끼워 넣는다.
    pub fn forward_embeds(&self, input: &Tensor) -> Result<Tensor> {
        let t = input.dim(1)?;
        let dev = &self.device;
        let mut h = input.clone();

        // PLE: 문맥 투영만 (토큰별 테이블 없음)
        let ple = self
            .ple_proj
            .forward(&h)?
            .affine(1.0 / (TEXT_HIDDEN_SIZE as f64).sqrt(), 0.0)?
            .reshape((1, t, TEXT_NUM_LAYERS, PLE_DIM))?;
        let ple = self.ple_norm.forward(&ple)?;

        let (cos_s, sin_s) = rope_tables(ROPE_THETA_SLIDING, TEXT_HEAD_DIM, t, dev)?;
        let (cos_f, sin_f) = rope_tables(ROPE_THETA_FULL, TEXT_GLOBAL_HEAD_DIM, t, dev)?;
        let mask_sliding = if self.bidirectional && t <= BIDIRECTIONAL_SLIDING_WINDOW + 1 {
            None
        } else {
            Some(attention_mask(t, true, self.bidirectional, dev)?)
        };
        let mask_full = if self.bidirectional {
            None
        } else {
            Some(attention_mask(t, false, false, dev)?)
        };

        for (i, layer) in self.layers.iter().enumerate() {
            let ple_i = ple.i((.., .., i, ..))?;
            h = match layer.kind {
                Attention::Full => layer.forward(&h, &ple_i, &cos_f, &sin_f, mask_full.as_ref())?,
                Attention::Sliding => layer.forward(&h, &ple_i, &cos_s, &sin_s, mask_sliding.as_ref())?,
            };
        }
        self.norm.forward(&h)
    }

    /// 토큰 id -> 768차원 벡터
    pub fn embed(&self, ids: &[u32]) -> Result<Vec<f32>> {
        self.embed_hidden(&self.forward(ids)?)
    }

    /// 최종 hidden (1 x t x 512) -> 768차원 벡터.
    /// `self.projection` is bias-free linear, so mean-pooling before the 512->768 projection is mathematically
    /// identical to projecting every token and averaging, while reducing projection work by `t`x.
    pub fn embed_hidden(&self, hidden: &Tensor) -> Result<Vec<f32>> {
        let pooled_hidden = hidden.mean(1)?;
        let pooled = self.projection.forward(&pooled_hidden)?;
        let norm = pooled.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?;
        pooled.broadcast_div(&norm)?.squeeze(0)?.to_vec1::<f32>()
    }
}
