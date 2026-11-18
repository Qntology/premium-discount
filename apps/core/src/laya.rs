//! convaiinnovations/laya-multilingual (리비전 1720e3e) 로컬 실행: 욕설 1차 게이트 (지시서 Stage 1).
//!
//! 구조는 laya 0.4.31 패키지(`laya/common.py`)를 그대로 옮겼다. 참조 픽스처는 진짜 laya + torch로 만든다
//! (`tests/reference/laya_reference.py` → `tests/fixtures/laya_reference.json`).
//! - 인코더: mmBERT-base (ModernBERT, 22층, H=768, 12헤드, 3층마다 전역 어텐션, 나머지는 |i-j| <= 64 창, RoPE theta 160000,
//!   GeGLU MLP 1152, 편향 없는 LayerNorm). 입력 임베딩 표(256,000 x 768, F16 393MB)는 파일 매핑에서 행만 읽는다.
//! - 결정 헤드: 질문 유형 임베딩을 더한 뒤 PyTorch `TransformerEncoderLayer` 2층(norm_first, ReLU, FFN 3072),
//!   선택지마다 자기 `[MASK]` 위치에서 점수(LayerNorm → Linear → GELU → Linear), 질문 안에서 softmax(로짓 / 온도).
//! - 입력: `[CLS] <유형> question: <지시문> [SEP] [MASK] 선택지0 [MASK] 선택지1 ... [SEP] 상태 [SEP]`,
//!   최대 1024 토큰 중 질문·선택지에 256 (rl_agent_config.json).
//! - 정밀도: 지시서대로 양자화 면제가 기본이다 (F32, 원본 F16에서 정확히 바꾼 값). Q8_0은 설정으로만.

use std::path::Path;

use candle_core::{DType, Device, IndexOp, Module, Result, Tensor, D};
use candle_nn::VarBuilder;
use tokenizers::Tokenizer;

use crate::embeddinggemma_tokens::TokenTable;
use crate::error::AppError;
use crate::filter::{LocalFilter, ProfanityFilter, Verdict};
use crate::quant::{load_lin, Bits, Kernel, Lin};

pub const WEIGHTS_FILE: &str = "model.safetensors";
pub const TOKENIZER_FILE: &str = "tokenizer/tokenizer.json";

const HIDDEN: usize = 768;
const HEADS: usize = 12;
const HEAD_DIM: usize = 64;
const LAYERS: usize = 22;
const MLP_DIM: usize = 1152;
const VOCAB: usize = 256_000;
/// 3층마다 전역 어텐션 (0, 3, 6, ...). 나머지는 창 어텐션
const GLOBAL_EVERY: usize = 3;
/// 창 어텐션 반경 (local_attention 128 / 2). |질의 - 키| <= 64를 포함한다
const WINDOW_RADIUS: usize = 64;
const ROPE_THETA: f64 = 160_000.0;
const NORM_EPS: f64 = 1e-5;
const HEAD_FF: usize = 3072;
const HEAD_LAYERS: usize = 2;
/// 질문 하나의 전체 길이와 그중 질문·선택지 몫 (rl_agent_config.json: max_len, head_max_len)
pub const MAX_LEN: usize = 1024;
pub const HEAD_MAX_LEN: usize = 256;
/// 선택지 하나의 최대 토큰 (laya build_head: truncation max_length=48)
const OPTION_MAX_TOKENS: usize = 48;

const CLS_ID: u32 = 2;
const SEP_ID: u32 = 1;
const MASK_ID: u32 = 4;
const MASK_TOKEN: &str = "<mask>";

/// 질문 유형 (laya QTYPES: choice 0, score 1, noul 2)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QKind {
    Choice,
    Score,
    Noul,
}

impl QKind {
    fn index(self) -> usize {
        match self {
            Self::Choice => 0,
            Self::Score => 1,
            Self::Noul => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Choice => "choice",
            Self::Score => "score",
            Self::Noul => "noul",
        }
    }
}

/// 질문 하나 (선택지 문구는 laya `render_options`와 같은 모양으로 만든다)
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub kind: QKind,
    pub instructions: String,
    pub options: Vec<String>,
    /// 선택지 이름 (choice의 키, noul은 false/true)
    pub labels: Vec<String>,
}

impl Question {
    /// choice: 선택지는 "키: 설명" (설명이 비었으면 키만)
    pub fn choice(instructions: &str, criteria: &[(&str, &str)]) -> Self {
        Self {
            kind: QKind::Choice,
            instructions: instructions.into(),
            options: criteria.iter().map(|(k, v)| if v.is_empty() { k.to_string() } else { format!("{k}: {v}") }).collect(),
            labels: criteria.iter().map(|(k, _)| k.to_string()).collect(),
        }
    }

    /// noul: 선택지는 [false, true] 순서, 기본 설명을 쓴다
    pub fn noul(instructions: &str) -> Self {
        Self {
            kind: QKind::Noul,
            instructions: instructions.into(),
            options: vec!["false: no, the statement does not hold".into(), "true: yes, the statement holds".into()],
            labels: vec!["false".into(), "true".into()],
        }
    }
}

/// 앱의 욕설 판정 질문 (tests/reference/laya_reference.py의 QUESTION과 같아야 한다).
/// README의 권고대로 noul 대신 중립 키 A/B 두 개짜리 choice로 묻는다 (noul은 "예"를 낮게 보고할 수 있다).
pub fn profanity_question() -> Question {
    Question::choice(
        "Does `body` contain profanity, slurs, sexual insults or abusive language aimed at someone?",
        &[("A", "yes, it contains profanity, slurs or abusive insults"), ("B", "no, the language is polite or neutral")],
    )
}

fn layer_norm(x: &Tensor, w: &Tensor, b: Option<&Tensor>, eps: f64) -> Result<Tensor> {
    let mean = x.mean_keepdim(D::Minus1)?;
    let xc = x.broadcast_sub(&mean)?;
    let var = xc.sqr()?.mean_keepdim(D::Minus1)?;
    let y = xc.broadcast_div(&(var + eps)?.sqrt()?)?.broadcast_mul(w)?;
    match b {
        Some(b) => y.broadcast_add(b),
        None => Ok(y),
    }
}

/// (t, HEAD_DIM) cos, sin (rotate_half 방식: 앞 절반과 뒤 절반이 같은 주파수)
fn rope(t: usize, dev: &Device) -> Result<(Tensor, Tensor)> {
    let half = HEAD_DIM / 2;
    let mut cos = vec![0f32; t * HEAD_DIM];
    let mut sin = vec![0f32; t * HEAD_DIM];
    for p in 0..t {
        for i in 0..half {
            let inv = 1.0 / ROPE_THETA.powf((2 * i) as f64 / HEAD_DIM as f64);
            let a = p as f64 * inv;
            let (c, s) = (a.cos() as f32, a.sin() as f32);
            cos[p * HEAD_DIM + i] = c;
            cos[p * HEAD_DIM + i + half] = c;
            sin[p * HEAD_DIM + i] = s;
            sin[p * HEAD_DIM + i + half] = s;
        }
    }
    Ok((Tensor::from_vec(cos, (t, HEAD_DIM), dev)?, Tensor::from_vec(sin, (t, HEAD_DIM), dev)?))
}

/// x: (heads, t, HEAD_DIM)
fn apply_rope(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    let half = HEAD_DIM / 2;
    let x1 = x.narrow(D::Minus1, 0, half)?;
    let x2 = x.narrow(D::Minus1, half, half)?;
    let rotated = Tensor::cat(&[&x2.neg()?, &x1], D::Minus1)?;
    x.broadcast_mul(cos)?.broadcast_add(&rotated.broadcast_mul(sin)?)
}

/// 창 어텐션 마스크 (t, t): |i - j| > 64면 매우 작은 값
fn window_mask(t: usize, dev: &Device) -> Result<Tensor> {
    let mut v = vec![0f32; t * t];
    for i in 0..t {
        for j in 0..t {
            if i.abs_diff(j) > WINDOW_RADIUS {
                v[i * t + j] = f32::NEG_INFINITY;
            }
        }
    }
    Tensor::from_vec(v, (t, t), dev)
}

/// (heads, t, d) 어텐션. scale은 1/sqrt(d)
fn attention(q: &Tensor, k: &Tensor, v: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
    let scores = (q.matmul(&k.t()?)? / (HEAD_DIM as f64).sqrt())?;
    let scores = match mask {
        Some(m) => scores.broadcast_add(m)?,
        None => scores,
    };
    candle_nn::ops::softmax_last_dim(&scores)?.matmul(v)
}

struct EncoderLayer {
    /// 0번 층은 없다 (Identity)
    attn_norm: Option<Tensor>,
    wqkv: Lin,
    wo: Lin,
    mlp_norm: Tensor,
    wi: Lin,
    mlp_wo: Lin,
    global: bool,
}

impl EncoderLayer {
    fn forward(&self, h: &Tensor, cos: &Tensor, sin: &Tensor, mask: &Tensor) -> Result<Tensor> {
        let t = h.dim(0)?;
        let x = match &self.attn_norm {
            Some(w) => layer_norm(h, w, None, NORM_EPS)?,
            None => h.clone(),
        };
        // Wqkv 출력은 [3][헤드][64] 순서
        let qkv = self.wqkv.forward(&x)?.reshape((t, 3, HEADS, HEAD_DIM))?;
        let part = |i: usize| -> Result<Tensor> { qkv.i((.., i, .., ..))?.transpose(0, 1)?.contiguous() };
        let (q, k, v) = (part(0)?, part(1)?, part(2)?);
        let q = apply_rope(&q, cos, sin)?;
        let k = apply_rope(&k, cos, sin)?;
        let out = attention(&q, &k, &v, if self.global { None } else { Some(mask) })?;
        let out = out.transpose(0, 1)?.contiguous()?.reshape((t, HIDDEN))?;
        let h = (h + self.wo.forward(&out)?)?;
        let x = layer_norm(&h, &self.mlp_norm, None, NORM_EPS)?;
        let y = self.wi.forward(&x)?;
        let input = y.narrow(D::Minus1, 0, MLP_DIM)?;
        let gate = y.narrow(D::Minus1, MLP_DIM, MLP_DIM)?;
        let y = self.mlp_wo.forward(&(input.gelu_erf()? * gate)?)?;
        h + y
    }
}

/// PyTorch nn.TransformerEncoderLayer (norm_first=True, activation=relu, batch_first)
struct HeadLayer {
    norm1: (Tensor, Tensor),
    in_proj_w: Tensor,
    in_proj_b: Tensor,
    out_w: Tensor,
    out_b: Tensor,
    norm2: (Tensor, Tensor),
    lin1: (Tensor, Tensor),
    lin2: (Tensor, Tensor),
}

fn affine(x: &Tensor, w: &Tensor, b: &Tensor) -> Result<Tensor> {
    x.matmul(&w.t()?)?.broadcast_add(b)
}

impl HeadLayer {
    fn load(vb: &VarBuilder) -> Result<Self> {
        let pair = |vb: VarBuilder, n: usize, m: Option<usize>| -> Result<(Tensor, Tensor)> {
            match m {
                Some(m) => Ok((vb.get((n, m), "weight")?, vb.get(n, "bias")?)),
                None => Ok((vb.get(n, "weight")?, vb.get(n, "bias")?)),
            }
        };
        let a = vb.pp("self_attn");
        Ok(Self {
            norm1: pair(vb.pp("norm1"), HIDDEN, None)?,
            in_proj_w: a.get((3 * HIDDEN, HIDDEN), "in_proj_weight")?,
            in_proj_b: a.get(3 * HIDDEN, "in_proj_bias")?,
            out_w: a.pp("out_proj").get((HIDDEN, HIDDEN), "weight")?,
            out_b: a.pp("out_proj").get(HIDDEN, "bias")?,
            norm2: pair(vb.pp("norm2"), HIDDEN, None)?,
            lin1: pair(vb.pp("linear1"), HEAD_FF, Some(HIDDEN))?,
            lin2: pair(vb.pp("linear2"), HIDDEN, Some(HEAD_FF))?,
        })
    }

    fn forward(&self, h: &Tensor) -> Result<Tensor> {
        let t = h.dim(0)?;
        let x = layer_norm(h, &self.norm1.0, Some(&self.norm1.1), NORM_EPS)?;
        let qkv = affine(&x, &self.in_proj_w, &self.in_proj_b)?;
        let split = |i: usize| -> Result<Tensor> {
            qkv.narrow(D::Minus1, i * HIDDEN, HIDDEN)?.reshape((t, HEADS, HEAD_DIM))?.transpose(0, 1)?.contiguous()
        };
        let out = attention(&split(0)?, &split(1)?, &split(2)?, None)?;
        let out = out.transpose(0, 1)?.contiguous()?.reshape((t, HIDDEN))?;
        let h = (h + affine(&out, &self.out_w, &self.out_b)?)?;
        let x = layer_norm(&h, &self.norm2.0, Some(&self.norm2.1), NORM_EPS)?;
        let y = affine(&affine(&x, &self.lin1.0, &self.lin1.1)?.relu()?, &self.lin2.0, &self.lin2.1)?;
        h + y
    }
}

/// laya-multilingual 모델 (인코더 + 결정 헤드)
pub struct LayaModel {
    tokenizer: Tokenizer,
    tokens: TokenTable,
    emb_norm: Tensor,
    layers: Vec<EncoderLayer>,
    final_norm: Tensor,
    type_emb: Tensor,
    head: Vec<HeadLayer>,
    scorer_norm: (Tensor, Tensor),
    scorer1: (Tensor, Tensor),
    scorer2: (Tensor, Tensor),
    temperature: [f32; 3],
    device: Device,
}

impl LayaModel {
    /// 모델 폴더(model.safetensors, tokenizer/tokenizer.json)를 연다. `bits`가 있으면 인코더 선형층만 그 비트로 양자화한다.
    pub fn load(dir: &Path, bits: Option<Bits>) -> std::result::Result<Self, AppError> {
        let err = |what: &'static str| move |e: candle_core::Error| AppError::Filter(format!("laya {what}: {e}"));
        let tokenizer = Tokenizer::from_file(dir.join(TOKENIZER_FILE))
            .map_err(|e| AppError::Filter(format!("laya 토크나이저 로드 실패: {e}")))?;
        let device = Device::Cpu;
        let weights = dir.join(WEIGHTS_FILE);
        if !weights.is_file() {
            return Err(AppError::Filter(format!("laya 가중치가 없습니다: {}", weights.display())));
        }
        // SAFETY: 파일은 읽기 전용으로 매핑하고, 앱이 쓰는 동안 바꾸지 않는다고 가정한다
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights], DType::F32, &device) }.map_err(err("가중치 열기"))?;
        let tokens = TokenTable::open_named(&weights, "encoder.embeddings.tok_embeddings.weight", VOCAB, HIDDEN, &device)
            .map_err(err("임베딩 표 열기"))?;
        let enc = vb.pp("encoder");
        let mut none = None;
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = enc.pp("layers").pp(i.to_string());
            let lin = |vb: &VarBuilder, shape: (usize, usize), none: &mut Option<&mut crate::qcache::QCache>| {
                load_lin(vb, shape, bits, Kernel::default(), none, String::new())
            };
            layers.push(EncoderLayer {
                attn_norm: if i == 0 { None } else { Some(l.pp("attn_norm").get(HIDDEN, "weight").map_err(err("attn_norm"))?) },
                wqkv: lin(&l.pp("attn").pp("Wqkv"), (3 * HIDDEN, HIDDEN), &mut none).map_err(err("Wqkv"))?,
                wo: lin(&l.pp("attn").pp("Wo"), (HIDDEN, HIDDEN), &mut none).map_err(err("Wo"))?,
                mlp_norm: l.pp("mlp_norm").get(HIDDEN, "weight").map_err(err("mlp_norm"))?,
                wi: lin(&l.pp("mlp").pp("Wi"), (2 * MLP_DIM, HIDDEN), &mut none).map_err(err("Wi"))?,
                mlp_wo: lin(&l.pp("mlp").pp("Wo"), (HIDDEN, MLP_DIM), &mut none).map_err(err("mlp Wo"))?,
                global: i % GLOBAL_EVERY == 0,
            });
        }
        let head = (0..HEAD_LAYERS)
            .map(|i| HeadLayer::load(&vb.pp("head").pp("layers").pp(i.to_string())))
            .collect::<Result<Vec<_>>>()
            .map_err(err("결정 헤드"))?;
        let s = vb.pp("scorer");
        let temperature: Vec<f32> = vb.get(3, "temperature").and_then(|t| t.to_vec1()).map_err(err("temperature"))?;
        Ok(Self {
            tokenizer,
            tokens,
            emb_norm: enc.pp("embeddings").pp("norm").get(HIDDEN, "weight").map_err(err("embeddings.norm"))?,
            layers,
            final_norm: enc.pp("final_norm").get(HIDDEN, "weight").map_err(err("final_norm"))?,
            type_emb: vb.get((3, HIDDEN), "type_emb.weight").map_err(err("type_emb"))?,
            head,
            scorer_norm: (s.pp("0").get(HIDDEN, "weight").map_err(err("scorer"))?, s.pp("0").get(HIDDEN, "bias").map_err(err("scorer"))?),
            scorer1: (s.pp("1").get((HIDDEN, HIDDEN), "weight").map_err(err("scorer"))?, s.pp("1").get(HIDDEN, "bias").map_err(err("scorer"))?),
            scorer2: (s.pp("3").get((1, HIDDEN), "weight").map_err(err("scorer"))?, s.pp("3").get(1, "bias").map_err(err("scorer"))?),
            temperature: [temperature[0], temperature[1], temperature[2]],
            device,
        })
    }

    fn encode(&self, text: &str, max: Option<usize>) -> std::result::Result<Vec<u32>, AppError> {
        let enc = self.tokenizer.encode(text, false).map_err(|e| AppError::Filter(format!("laya 토큰화 실패: {e}")))?;
        let ids = enc.get_ids();
        Ok(match max {
            Some(m) => ids[..ids.len().min(m)].to_vec(),
            None => ids.to_vec(),
        })
    }

    /// laya `build_sequence`와 같은 입력: (토큰, 선택지마다 [MASK] 위치)
    pub fn build_sequence(&self, state_json: &str, q: &Question) -> std::result::Result<(Vec<u32>, Vec<usize>), AppError> {
        let ins = q.instructions.replace(MASK_TOKEN, " ");
        let head_ids = self.encode(&format!("{} question: {ins}", q.kind.name()), None)?;
        let mut opt_ids = Vec::with_capacity(q.options.len());
        for opt in &q.options {
            let mut o = vec![MASK_ID];
            o.extend(self.encode(&format!(" {}", opt.replace(MASK_TOKEN, " ")), Some(OPTION_MAX_TOKENS))?);
            opt_ids.push(o);
        }
        let total = |o: &Vec<Vec<u32>>| o.iter().map(Vec::len).sum::<usize>() as isize;
        let mut opt_budget = HEAD_MAX_LEN as isize - total(&opt_ids);
        if opt_budget < 16 {
            let per = 4.max((HEAD_MAX_LEN - 16) / opt_ids.len().max(1));
            for o in opt_ids.iter_mut() {
                o.truncate(per);
            }
            opt_budget = HEAD_MAX_LEN as isize - total(&opt_ids);
        }
        let keep = (opt_budget.max(8)) as usize;
        let mut ids = vec![CLS_ID];
        ids.extend_from_slice(&head_ids[..head_ids.len().min(keep)]);
        ids.push(SEP_ID);
        let mut markers = Vec::with_capacity(opt_ids.len());
        for o in &opt_ids {
            markers.push(ids.len());
            ids.extend_from_slice(o);
        }
        ids.push(SEP_ID);
        let room = MAX_LEN.saturating_sub(ids.len() + 1);
        let state_ids = self.encode(&state_json.replace(MASK_TOKEN, " "), None)?;
        ids.extend_from_slice(&state_ids[..state_ids.len().min(room)]);
        ids.push(SEP_ID);
        ids.truncate(MAX_LEN);
        markers.retain(|&m| m < MAX_LEN);
        Ok((ids, markers))
    }

    /// 인코더 마지막 은닉 상태 (t, 768). 시험에서 참조와 중간값을 비교할 때 쓴다
    pub fn encoder_hidden(&self, ids: &[u32]) -> Result<Tensor> {
        let t = ids.len();
        let mut h = layer_norm(&self.tokens.rows(ids)?, &self.emb_norm, None, NORM_EPS)?;
        let (cos, sin) = rope(t, &self.device)?;
        let mask = window_mask(t, &self.device)?;
        for layer in &self.layers {
            h = layer.forward(&h, &cos, &sin, &mask)?;
        }
        layer_norm(&h, &self.final_norm, None, NORM_EPS)
    }

    /// 선택지마다의 원시 로짓 (온도 적용 전)
    pub fn logits(&self, ids: &[u32], markers: &[usize], kind: QKind) -> Result<Vec<f32>> {
        let h = self.encoder_hidden(ids)?;
        let mut h = h.broadcast_add(&self.type_emb.get(kind.index())?)?;
        for layer in &self.head {
            h = layer.forward(&h)?;
        }
        let idx = Tensor::from_vec(markers.iter().map(|&m| m as u32).collect::<Vec<_>>(), markers.len(), &self.device)?;
        let m = h.index_select(&idx, 0)?;
        let m = layer_norm(&m, &self.scorer_norm.0, Some(&self.scorer_norm.1), NORM_EPS)?;
        let m = affine(&m, &self.scorer1.0, &self.scorer1.1)?.gelu_erf()?;
        affine(&m, &self.scorer2.0, &self.scorer2.1)?.squeeze(D::Minus1)?.to_vec1()
    }

    /// 상태(`{"body": 글}`)에 대한 질문의 선택지 확률
    pub fn predict(&self, body: &str, q: &Question) -> std::result::Result<Vec<f32>, AppError> {
        let state = format!("{{\"body\": {}}}", serde_json::to_string(body).map_err(|e| AppError::Filter(e.to_string()))?);
        let (ids, markers) = self.build_sequence(&state, q)?;
        if markers.len() != q.options.len() {
            return Err(AppError::Filter("laya: 선택지가 입력 길이에 다 들어가지 않았습니다".into()));
        }
        let logits = self.logits(&ids, &markers, q.kind).map_err(|e| AppError::Filter(format!("laya 실행 실패: {e}")))?;
        let t = self.temperature[q.kind.index()];
        let z: Vec<f32> = logits.iter().map(|l| l / t).collect();
        let max = z.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let e: Vec<f32> = z.iter().map(|v| (v - max).exp()).collect();
        let sum: f32 = e.iter().sum();
        Ok(e.iter().map(|v| v / sum).collect())
    }
}

/// 사전 필터(LocalFilter)로 먼저 거르고, 통과한 글만 laya에 묻는다. P(욕설) >= threshold면 막는다.
pub struct LayaFilter {
    model: LayaModel,
    dictionary: LocalFilter,
    question: Question,
    pub threshold: f32,
}

/// 기본 문턱. 참조 11문장에서 욕설 P(A)는 0.58~0.89, 정상은 0.25 이하였다 (README 17절의 평가)
pub const DEFAULT_THRESHOLD: f32 = 0.5;

impl LayaFilter {
    pub fn new(model: LayaModel) -> Self {
        Self { model, dictionary: LocalFilter::default(), question: profanity_question(), threshold: DEFAULT_THRESHOLD }
    }

    /// 욕설일 확률 P(A)
    pub fn profanity_probability(&self, text: &str) -> std::result::Result<f32, AppError> {
        Ok(self.model.predict(text, &self.question)?[0])
    }
}

impl ProfanityFilter for LayaFilter {
    fn check(&self, text: &str) -> std::result::Result<Verdict, AppError> {
        if self.dictionary.check(text)? == Verdict::Profane {
            return Ok(Verdict::Profane);
        }
        Ok(if self.profanity_probability(text)? >= self.threshold { Verdict::Profane } else { Verdict::Clean })
    }
}

/// 설정에 맞는 laya 필터를 연다. 설치되어 있지 않으면 None (셸은 사전 필터를 그대로 쓴다)
pub fn open_filter(dir: &Path, bits: Option<Bits>) -> Option<std::result::Result<LayaFilter, AppError>> {
    if !(dir.join(WEIGHTS_FILE).is_file() && dir.join(TOKENIZER_FILE).is_file()) {
        return None;
    }
    Some(LayaModel::load(dir, bits).map(LayaFilter::new))
}

