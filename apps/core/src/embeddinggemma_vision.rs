//! embeddinggemma-2 이미지·비디오 경로.
//!
//! 이미지를 크기 조정(bicubic, antialias)하고 16x16 패치로 나눈 뒤, 비전 타워(16층, 2차원 axial RoPE)를 거쳐
//! 3x3 평균 풀링과 투영으로 512차원 소프트 토큰을 만든다. 구조와 연산은 transformers 5.17의 Gemma4VisionModel,
//! Gemma4MultimodalEmbedder, Gemma4ImageProcessor를 따른다. 참조 비교는 tests/embeddinggemma_vision.rs.
//!
//! 비디오는 프레임마다 같은 경로를 쓰고 프레임당 최대 140 토큰을 쓴다. 디코딩은 이 모듈 밖의 일이다.
//! 추정으로 채운 부분: 비디오 프레임 선택 규칙(overflow uniform). 설치된 transformers에는 이 옵션이 없다.

use candle_core::quantized::QTensor;
use candle_core::{Device, Result, Tensor, D};
use candle_nn::{ops, Linear, Module, VarBuilder};

use crate::quant::{Lin, TowerQuant};

/// 패치 한 변 (픽셀)
pub const PATCH_SIZE: usize = 16;
/// 풀링 커널 한 변. 소프트 토큰 한 개는 패치 3x3 칸의 평균이다
pub const POOLING_KERNEL: usize = 3;
/// 목표 높이와 너비는 이 값의 배수여야 한다 (패치 크기 x 풀링 커널)
pub const SIDE_MULTIPLE: usize = PATCH_SIZE * POOLING_KERNEL;
/// 이미지 한 장의 소프트 토큰 상한 (processor_config.json image_processor.max_soft_tokens)
pub const IMAGE_MAX_SOFT_TOKENS: usize = 280;
/// 비디오 프레임 한 장의 소프트 토큰 상한 (processor_config.json video_processor.max_soft_tokens)
pub const VIDEO_MAX_SOFT_TOKENS: usize = 140;
/// 비디오 샘플링 목표 (초당 프레임 수)
pub const VIDEO_FPS: f64 = 1.0;
/// 비디오에서 가져올 최대 프레임 수
pub const VIDEO_MAX_FRAMES: usize = 32;
/// 소프트 토큰 차원 (embed_vision 출력, 텍스트 hidden size와 같다)
pub const SOFT_TOKEN_DIM: usize = 512;

const HIDDEN: usize = 768;
const HEADS: usize = 12;
const HEAD_DIM: usize = 64;
const LAYERS: usize = 16;
const INTERMEDIATE: usize = 3072;
const POSITION_TABLE: usize = 10240;
const PATCH_DIM: usize = PATCH_SIZE * PATCH_SIZE * 3;
const ROPE_THETA: f32 = 100.0;
const RMS_EPS: f64 = 1e-6;
/// processor_config.json의 rescale_factor (1/255)
const RESCALE: f32 = 0.00392156862745098;
/// bicubic 커널의 a 값 (torch와 PIL 모두 -0.5)
const BICUBIC_A: f64 = -0.5;

/// RGB 8비트 이미지. 픽셀은 행 우선, 채널 인터리브 (R G B R G B ...)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl RgbImage {
    /// 픽셀 개수가 width x height x 3과 맞는지 확인한다
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(candle_core::Error::Msg("image size must be positive".into()));
        }
        let expected = width * height * 3;
        if pixels.len() != expected {
            return Err(candle_core::Error::Msg(format!(
                "RGB image {width}x{height} needs {expected} bytes, got {}",
                pixels.len()
            )));
        }
        Ok(Self { width, height, pixels })
    }
}

/// 목표 크기 (높이, 너비). 패치 수가 max_soft_tokens x 9를 넘지 않는 가장 큰 크기이고, 각 변은 48의 배수다.
/// transformers의 get_aspect_ratio_preserving_size와 같은 규칙이다.
pub fn target_size(height: usize, width: usize, max_soft_tokens: usize) -> Result<(usize, usize)> {
    let max_patches = max_soft_tokens * POOLING_KERNEL * POOLING_KERNEL;
    let target_px = (max_patches * PATCH_SIZE * PATCH_SIZE) as f64;
    let factor = (target_px / (height * width) as f64).sqrt();
    let side = SIDE_MULTIPLE as f64;
    let mut th = ((factor * height as f64) / side).floor() as usize * SIDE_MULTIPLE;
    let mut tw = ((factor * width as f64) / side).floor() as usize * SIDE_MULTIPLE;
    if th == 0 && tw == 0 {
        return Err(candle_core::Error::Msg(format!("cannot resize {height}x{width} to a multiple of {SIDE_MULTIPLE}")));
    }
    let max_side = (max_patches / (POOLING_KERNEL * POOLING_KERNEL)) * SIDE_MULTIPLE;
    if th == 0 {
        th = SIDE_MULTIPLE;
        tw = (width / height * SIDE_MULTIPLE).min(max_side);
    } else if tw == 0 {
        tw = SIDE_MULTIPLE;
        th = (height / width * SIDE_MULTIPLE).min(max_side);
    }
    if (th * tw) as f64 > target_px {
        return Err(candle_core::Error::Msg(format!("{th}x{tw} exceeds {max_patches} patches")));
    }
    Ok((th, tw))
}

/// 이미지 한 장이 내는 소프트 토큰 수 (목표 크기에서 계산한다. 인코딩 결과 길이와 같아야 한다)
pub fn soft_token_count(height: usize, width: usize, max_soft_tokens: usize) -> Result<usize> {
    let (th, tw) = target_size(height, width, max_soft_tokens)?;
    Ok((th / SIDE_MULTIPLE) * (tw / SIDE_MULTIPLE))
}

/// torch uint8 경로의 고정소수점 가중치 소수 비트 (32 - 8 - 2)
const PRECISION_BITS: u32 = 22;
/// 고정소수점 누산에서 쓰는 반올림 상수 (2^21)
const ROUND: i64 = 1 << (PRECISION_BITS - 1);

/// 8비트로 되돌린다: 오른쪽으로 22비트 옮기고 0~255로 자른다
fn clip8(acc: i64) -> u8 {
    (acc >> PRECISION_BITS).clamp(0, 255) as u8
}

/// 축 하나의 리샘플링 가중치. 시작 인덱스와 각 입력 칸의 정수 가중치(2^22 배율)를 가진다.
struct Taps {
    start: usize,
    weights: Vec<i64>,
}

fn cubic(x: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        ((BICUBIC_A + 2.0) * x - (BICUBIC_A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * BICUBIC_A
    } else {
        0.0
    }
}

/// torch의 antialias bicubic 가중치 (축 한 개). 축소일 때는 커널을 배율만큼 넓힌다.
fn antialias_taps(input: usize, output: usize) -> Vec<Taps> {
    let scale = input as f64 / output as f64;
    let support = if scale >= 1.0 { 2.0 * scale } else { 2.0 };
    let invscale = if scale >= 1.0 { 1.0 / scale } else { 1.0 };
    (0..output)
        .map(|i| {
            let center = scale * (i as f64 + 0.5);
            let start = (center - support + 0.5).trunc().max(0.0) as usize;
            let end = ((center + support + 0.5).trunc() as usize).min(input);
            let raw: Vec<f64> = (start..end)
                .map(|j| cubic((j as f64 - center + 0.5) * invscale))
                .collect();
            let total: f64 = raw.iter().sum();
            let scale = (1i64 << PRECISION_BITS) as f64;
            Taps {
                start,
                weights: raw
                    .iter()
                    .map(|w| {
                        let x = w / total;
                        // C++의 static_cast<int>(x * 2^22 +- 0.5)와 같다 (0을 향해 자른다)
                        (x * scale + if x >= 0.0 { 0.5 } else { -0.5 }) as i64
                    })
                    .collect(),
            }
        })
        .collect()
}

/// 크기 조정 (bicubic, antialias). 가로 pass 결과를 8비트로 반올림해 두고, 세로 pass를 한 번 더 한다.
/// torchvision의 tvF.resize(uint8 텐서, BICUBIC, antialias=True)와 같다. 중간 반올림까지 맞춰야 값이 일치한다
/// 정수 고정소수점으로 계산한다. 작은 골든 값은 torch와 비트 단위로 같고, 큰 이미지는 일부 픽셀이 1만큼 다를 수 있다.
pub fn resize_bicubic(image: &RgbImage, height: usize, width: usize) -> RgbImage {
    let (ih, iw) = (image.height, image.width);
    let htaps = antialias_taps(iw, width);
    let vtaps = antialias_taps(ih, height);

    let mut mid = vec![0u8; ih * width * 3];
    for y in 0..ih {
        for (x, t) in htaps.iter().enumerate() {
            for c in 0..3 {
                let mut acc = ROUND;
                for (k, &w) in t.weights.iter().enumerate() {
                    acc += w * image.pixels[(y * iw + t.start + k) * 3 + c] as i64;
                }
                mid[(y * width + x) * 3 + c] = clip8(acc);
            }
        }
    }

    let mut out = vec![0u8; height * width * 3];
    for (y, t) in vtaps.iter().enumerate() {
        for x in 0..width {
            for c in 0..3 {
                let mut acc = ROUND;
                for (k, &w) in t.weights.iter().enumerate() {
                    acc += w * mid[((t.start + k) * width + x) * 3 + c] as i64;
                }
                out[(y * width + x) * 3 + c] = clip8(acc);
            }
        }
    }
    RgbImage { width, height, pixels: out }
}

/// 모델 입력 (패치 단위). 패치 순서는 행 우선이고, 패치 안 픽셀 순서는 (행, 열, 채널)이다.
#[derive(Debug, Clone, PartialEq)]
pub struct Preprocessed {
    /// [n, 768]. 값은 2 * (픽셀 / 255 - 0.5)
    pub pixels: Vec<f32>,
    /// 패치마다 (x, y) = (열, 행)
    pub positions: Vec<[u32; 2]>,
    /// 패치 격자 (행 수, 열 수)
    pub grid: (usize, usize),
}

/// 리사이즈, 리스케일, 패치화. transformers Gemma4ImageProcessor와 같은 순서다.
pub fn preprocess(image: &RgbImage, max_soft_tokens: usize) -> Result<Preprocessed> {
    let (th, tw) = target_size(image.height, image.width, max_soft_tokens)?;
    let owned;
    let img = if (th, tw) == (image.height, image.width) {
        image
    } else {
        owned = resize_bicubic(image, th, tw);
        &owned
    };

    let (ph, pw) = (th / PATCH_SIZE, tw / PATCH_SIZE);
    let mut lut = [0f32; 256];
    for i in 0..256 {
        lut[i] = 2.0 * (i as f32 * RESCALE - 0.5);
    }
    let mut pixels = Vec::with_capacity(ph * pw * PATCH_DIM);
    let mut positions = Vec::with_capacity(ph * pw);
    for py in 0..ph {
        for px in 0..pw {
            positions.push([px as u32, py as u32]);
            for ry in 0..PATCH_SIZE {
                let row = (py * PATCH_SIZE + ry) * tw + px * PATCH_SIZE;
                for rx in 0..PATCH_SIZE {
                    let base = (row + rx) * 3;
                    pixels.push(lut[img.pixels[base] as usize]);
                    pixels.push(lut[img.pixels[base + 1] as usize]);
                    pixels.push(lut[img.pixels[base + 2] as usize]);
                }
            }
        }
    }
    Ok(Preprocessed {
        pixels,
        positions,
        grid: (ph, pw),
    })
}

/// 비디오에서 가져올 프레임 번호. 초당 1프레임을 목표로 하고 최대 32프레임까지 고르게 나눈다.
/// 추정: transformers 5.17의 video processor에는 overflow 옵션이 없어서 규칙을 재구성했다.
pub fn video_frame_indices(total_frames: usize, source_fps: f64) -> Vec<usize> {
    if total_frames == 0 || !(source_fps > 0.0) {
        return Vec::new();
    }
    let duration = total_frames as f64 / source_fps;
    let wanted = ((duration * VIDEO_FPS) as usize).max(1);
    let count = wanted.min(VIDEO_MAX_FRAMES).min(total_frames);
    let step = total_frames as f64 / count as f64;
    (0..count).map(|k| (k as f64 * step) as usize).collect()
}

/// RMS norm: x / sqrt(mean(x^2) + eps) * weight. weight가 None이면 스케일 없이 정규화만 한다.
fn rms_norm(x: &Tensor, weight: Option<&Tensor>) -> Result<Tensor> {
    let inv = x.sqr()?.mean_keepdim(D::Minus1)?.affine(1.0, RMS_EPS)?.powf(-0.5)?;
    let y = x.broadcast_mul(&inv)?;
    match weight {
        Some(w) => y.broadcast_mul(w),
        None => Ok(y),
    }
}

fn rotate_half(x: &Tensor) -> Result<Tensor> {
    let d = x.dim(D::Minus1)?;
    let x1 = x.narrow(D::Minus1, 0, d / 2)?;
    let x2 = x.narrow(D::Minus1, d / 2, d / 2)?;
    Tensor::cat(&[&x2.neg()?, &x1], D::Minus1)
}

/// 2차원 axial RoPE. 앞 32채널은 x 위치, 뒤 32채널은 y 위치로 돌린다. x: [n, heads, 64], cos, sin: [n, 1, 64]
fn apply_rope(x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
    let half = HEAD_DIM / 2;
    let mut parts = Vec::with_capacity(2);
    for p in 0..2 {
        let xp = x.narrow(D::Minus1, p * half, half)?;
        let cp = cos.narrow(D::Minus1, p * half, half)?;
        let sp = sin.narrow(D::Minus1, p * half, half)?;
        parts.push(xp.broadcast_mul(&cp)?.broadcast_add(&rotate_half(&xp)?.broadcast_mul(&sp)?)?);
    }
    Tensor::cat(&parts, D::Minus1)
}

/// 패치 위치마다 cos, sin 표 [n, 1, 64]. 채널 순서는 [x 주파수, x 주파수, y 주파수, y 주파수] (각 16개).
fn rope_tables(positions: &[[u32; 2]], dev: &Device) -> Result<(Tensor, Tensor)> {
    let n = positions.len();
    let inv_freq: Vec<f32> = (0..HEAD_DIM / 4)
        .map(|i| 1.0 / ROPE_THETA.powf((2 * i) as f32 / (HEAD_DIM / 2) as f32))
        .collect();
    let mut cos = Vec::with_capacity(n * HEAD_DIM);
    let mut sin = Vec::with_capacity(n * HEAD_DIM);
    for p in positions {
        for axis in [p[0] as f32, p[1] as f32] {
            for _ in 0..2 {
                for &f in &inv_freq {
                    let angle = axis * f;
                    cos.push(angle.cos());
                    sin.push(angle.sin());
                }
            }
        }
    }
    Ok((
        Tensor::from_vec(cos, (n, 1, HEAD_DIM), dev)?,
        Tensor::from_vec(sin, (n, 1, HEAD_DIM), dev)?,
    ))
}

fn linear(vb: &VarBuilder, out_dim: usize, in_dim: usize) -> Result<Linear> {
    Ok(Linear::new(vb.get((out_dim, in_dim), "weight")?, None))
}

struct Block {
    input_norm: Tensor,
    post_attn_norm: Tensor,
    pre_ff_norm: Tensor,
    post_ff_norm: Tensor,
    q_proj: Lin,
    k_proj: Lin,
    v_proj: Lin,
    o_proj: Lin,
    q_norm: Tensor,
    k_norm: Tensor,
    gate: Lin,
    up: Lin,
    down: Lin,
}

/// 양자화 대상 선형층의 이름 (캐시 이름의 끝)
const BLOCK_LINEARS: [&str; 7] = ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"];

impl Block {
    fn load(l: &VarBuilder, i: usize, q: &mut TowerQuant) -> Result<Self> {
        let a = l.pp("self_attn");
        let mlp = l.pp("mlp");
        let key = |name: &str| format!("layers.{i}.{name}");
        // 체크포인트는 선형 층을 clippable wrapper로 저장한다 (`...proj.linear.weight`). 클리핑은 꺼져 있다.
        Ok(Self {
            input_norm: l.pp("input_layernorm").get(HIDDEN, "weight")?,
            post_attn_norm: l.pp("post_attention_layernorm").get(HIDDEN, "weight")?,
            pre_ff_norm: l.pp("pre_feedforward_layernorm").get(HIDDEN, "weight")?,
            post_ff_norm: l.pp("post_feedforward_layernorm").get(HIDDEN, "weight")?,
            q_proj: q.lin(&a.pp("q_proj").pp("linear"), i, (HIDDEN, HIDDEN), key("q_proj"))?,
            k_proj: q.lin(&a.pp("k_proj").pp("linear"), i, (HIDDEN, HIDDEN), key("k_proj"))?,
            v_proj: q.lin(&a.pp("v_proj").pp("linear"), i, (HIDDEN, HIDDEN), key("v_proj"))?,
            o_proj: q.lin(&a.pp("o_proj").pp("linear"), i, (HIDDEN, HIDDEN), key("o_proj"))?,
            q_norm: a.pp("q_norm").get(HEAD_DIM, "weight")?,
            k_norm: a.pp("k_norm").get(HEAD_DIM, "weight")?,
            gate: q.lin(&mlp.pp("gate_proj").pp("linear"), i, (INTERMEDIATE, HIDDEN), key("gate_proj"))?,
            up: q.lin(&mlp.pp("up_proj").pp("linear"), i, (INTERMEDIATE, HIDDEN), key("up_proj"))?,
            down: q.lin(&mlp.pp("down_proj").pp("linear"), i, (HIDDEN, INTERMEDIATE), key("down_proj"))?,
        })
    }

    fn attention(&self, x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
        let n = x.dim(0)?;
        let q = self.q_proj.forward(x)?.reshape((n, HEADS, HEAD_DIM))?;
        let q = apply_rope(&rms_norm(&q, Some(&self.q_norm))?, cos, sin)?;
        let k = self.k_proj.forward(x)?.reshape((n, HEADS, HEAD_DIM))?;
        let k = apply_rope(&rms_norm(&k, Some(&self.k_norm))?, cos, sin)?;
        let v = rms_norm(&self.v_proj.forward(x)?.reshape((n, HEADS, HEAD_DIM))?, None)?;

        if x.device().is_cpu() && x.dtype() == candle_core::DType::F32 {
            let q_vec = q.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let k_vec = k.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let v_vec = v.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let mut out = vec![0.0f32; n * HEADS * HEAD_DIM];
            let cfg = premium_discount_cpu_kernels::CpuStreamAttnCfg {
                num_heads: HEADS,
                num_kv_heads: HEADS,
                head_dim: HEAD_DIM,
                scale: 1.0,
                causal: false,
                past_offset: 0,
            };
            premium_discount_cpu_kernels::stream_attention_f32(&q_vec, &k_vec, &v_vec, n, n, &cfg, &mut out);
            let out = Tensor::from_vec(out, (n, HEADS * HEAD_DIM), x.device())?;
            return self.o_proj.forward(&out);
        }

        let q = q.transpose(0, 1)?.contiguous()?;
        let k = k.transpose(0, 1)?.contiguous()?;
        let v = v.transpose(0, 1)?.contiguous()?;
        // 점수 배율은 1.0이다 (q_norm, k_norm이 크기를 맞춘다). 패딩이 없어서 마스크도 없다.
        let out = if crate::utils::accel_op_ok(x.device(), crate::utils::AccelOp::FlashAttn) {
            let cfg = candle_nn::flash_attn::FlashAttnConfig::new(1.0);
            match candle_nn::flash_attn::flash_attn_bhsd(&q.unsqueeze(0)?, &k.unsqueeze(0)?, &v.unsqueeze(0)?, &cfg) {
                Ok(o) => o.squeeze(0)?,
                Err(_) => {
                    let scores = q.matmul(&k.transpose(1, 2)?.contiguous()?)?;
                    ops::softmax_last_dim(&scores)?.matmul(&v)?
                }
            }
        } else {
            let scores = q.matmul(&k.transpose(1, 2)?.contiguous()?)?;
            ops::softmax_last_dim(&scores)?.matmul(&v)?
        };
        let out = out
            .transpose(0, 1)?
            .contiguous()?
            .reshape((n, HEADS * HEAD_DIM))?;
        self.o_proj.forward(&out)
    }
}

/// 이미지 인코더. 비전 타워 + 풀링 + 임베딩 투영.
pub struct VisionTower {
    input_proj: Linear,
    pos_x: Tensor,
    pos_y: Tensor,
    blocks: Vec<Block>,
    embed_proj: Linear,
    device: Device,
}

impl VisionTower {
    /// 루트 VarBuilder에서 `vision_tower.*`와 `embed_vision.*`를 읽는다 (F32).
    pub fn load(vb: &VarBuilder, device: &Device) -> Result<Self> {
        Self::load_with(vb, device, TowerQuant::off())
    }

    /// 인코더 층의 선형층(q, k, v, o, gate, up, down)만 `quant`대로 양자화한다.
    /// 패치 입력 투영, 위치 표, 관문 투영(embed_vision.embedding_projection), 정규화는 F32 그대로다.
    pub fn load_with(vb: &VarBuilder, device: &Device, mut quant: TowerQuant) -> Result<Self> {
        let vt = vb.pp("vision_tower");
        let patch = vt.pp("patch_embedder");
        let table = patch.get((2, POSITION_TABLE, HIDDEN), "position_embedding_table")?;
        let enc = vt.pp("encoder").pp("layers");
        let blocks = (0..LAYERS)
            .map(|i| Block::load(&enc.pp(i.to_string()), i, &mut quant))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            input_proj: linear(&patch.pp("input_proj"), HIDDEN, PATCH_DIM)?,
            pos_x: table.get(0)?,
            pos_y: table.get(1)?,
            blocks,
            embed_proj: linear(&vb.pp("embed_vision").pp("embedding_projection"), SOFT_TOKEN_DIM, HIDDEN)?,
            device: device.clone(),
        })
    }

    /// 양자화된 선형층 (캐시에 쓸 때)
    pub fn quantized_tensors(&self) -> Vec<(String, &QTensor)> {
        let mut out = Vec::new();
        for (i, b) in self.blocks.iter().enumerate() {
            let lins = [&b.q_proj, &b.k_proj, &b.v_proj, &b.o_proj, &b.gate, &b.up, &b.down];
            for (name, lin) in BLOCK_LINEARS.iter().zip(lins) {
                if let Some(q) = lin.qtensor() {
                    out.push((format!("layers.{i}.{name}"), q));
                }
            }
        }
        out
    }

    /// 선형층 가중치 크기 (바이트)
    pub fn linear_bytes(&self) -> usize {
        self.blocks.iter().map(|b| [&b.q_proj, &b.k_proj, &b.v_proj, &b.o_proj, &b.gate, &b.up, &b.down].iter().map(|l| l.weight_bytes()).sum::<usize>()).sum()
    }

    /// 이미지 한 장 -> 소프트 토큰 [n, 512]. n은 `soft_token_count`와 같다.
    /// 비디오 프레임은 `VIDEO_MAX_SOFT_TOKENS`로 이 함수를 부른다.
    pub fn encode_image(&self, image: &RgbImage, max_soft_tokens: usize) -> Result<Tensor> {
        let prep = preprocess(image, max_soft_tokens)?;
        self.encode_patches(prep)
    }

    fn encode_patches(&self, prep: Preprocessed) -> Result<Tensor> {
        let dev = &self.device;
        let n = prep.positions.len();
        let xs: Vec<u32> = prep.positions.iter().map(|p| p[0]).collect();
        let ys: Vec<u32> = prep.positions.iter().map(|p| p[1]).collect();
        let x_emb = self.pos_x.index_select(&Tensor::from_vec(xs, n, dev)?, 0)?;
        let y_emb = self.pos_y.index_select(&Tensor::from_vec(ys, n, dev)?, 0)?;
        let (cos, sin) = rope_tables(&prep.positions, dev)?;

        let patches = Tensor::from_vec(prep.pixels, (n, PATCH_DIM), dev)?;
        let mut h = self
            .input_proj
            .forward(&patches)?
            .broadcast_add(&x_emb)?
            .broadcast_add(&y_emb)?;

        for block in &self.blocks {
            let x = rms_norm(&h, Some(&block.input_norm))?;
            let a = block.attention(&x, &cos, &sin)?;
            let a = rms_norm(&a, Some(&block.post_attn_norm))?;
            h = h.broadcast_add(&a)?;

            let x = rms_norm(&h, Some(&block.pre_ff_norm))?;
            let g = block.gate.forward(&x)?.gelu()?;
            let u = block.up.forward(&x)?;
            let m = block.down.forward(&g.broadcast_mul(&u)?)?;
            let m = rms_norm(&m, Some(&block.post_ff_norm))?;
            h = h.broadcast_add(&m)?;
        }

        // 3x3 평균 풀링 (칸마다 1/9), 그다음 sqrt(768) 배율 — dense sparse-zeros matmul 대신 exact reshape+mean
        let (ph, pw) = prep.grid;
        let (oh, ow) = (ph / POOLING_KERNEL, pw / POOLING_KERNEL);
        let cells = oh * ow;
        let pooled = h
            .reshape((oh, POOLING_KERNEL, ow, POOLING_KERNEL, HIDDEN))?
            .mean(3)?
            .mean(1)?
            .reshape((cells, HIDDEN))?
            .affine((HIDDEN as f64).sqrt(), 0.0)?;
        let normed = rms_norm(&pooled, None)?;
        self.embed_proj.forward(&normed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn formula(width: usize, height: usize) -> RgbImage {
        let mut pixels = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                for c in 0..3 {
                    pixels.push(((x * 7 + y * 3 + c * 50) % 256) as u8);
                }
            }
        }
        RgbImage::new(width, height, pixels).unwrap()
    }

    #[test]
    fn square_384_fits_budget_without_resize() {
        assert_eq!(target_size(384, 384, 70).unwrap(), (384, 384));
        assert_eq!(soft_token_count(384, 384, 70).unwrap(), 64);
    }

    #[test]
    fn small_image_is_upscaled_to_fill_budget() {
        assert_eq!(target_size(200, 300, 70).unwrap(), (288, 480));
        assert_eq!(soft_token_count(200, 300, 70).unwrap(), 60);
    }

    #[test]
    fn large_image_is_downscaled_to_budget() {
        assert_eq!(target_size(700, 1000, 70).unwrap(), (336, 480));
        assert_eq!(soft_token_count(700, 1000, 70).unwrap(), 70);
    }

    #[test]
    fn video_frame_budget_is_140_tokens() {
        assert_eq!(target_size(200, 300, VIDEO_MAX_SOFT_TOKENS).unwrap(), (432, 672));
        assert_eq!(soft_token_count(200, 300, VIDEO_MAX_SOFT_TOKENS).unwrap(), 126);
    }

    #[test]
    fn target_sizes_are_multiples_and_within_budget() {
        for (h, w) in [(1, 1000), (1000, 1), (37, 91), (512, 512), (4000, 3000), (48, 4000)] {
            for max in [70, 140, 280] {
                let Ok((th, tw)) = target_size(h, w, max) else { continue };
                assert_eq!(th % SIDE_MULTIPLE, 0, "{h}x{w} -> {th}x{tw}");
                assert_eq!(tw % SIDE_MULTIPLE, 0, "{h}x{w} -> {th}x{tw}");
                assert!(th * tw <= max * 9 * 256, "{h}x{w} -> {th}x{tw} over budget {max}");
            }
        }
    }

    #[test]
    fn rgb_image_rejects_wrong_byte_count() {
        assert!(RgbImage::new(2, 2, vec![0; 11]).is_err());
        assert!(RgbImage::new(2, 2, vec![0; 12]).is_ok());
        assert!(RgbImage::new(0, 2, Vec::new()).is_err());
    }

    #[test]
    fn same_size_resize_is_identity() {
        let img = formula(5, 4);
        assert_eq!(resize_bicubic(&img, 4, 5), img);
    }

    #[test]
    fn resize_matches_torch_antialias_bicubic_golden() {
        // torch._C._nn._upsample_bicubic2d_aa(uint8, align_corners=False)로 만든 값 (HWC)
        let up = resize_bicubic(&formula(5, 3), 4, 7);
        let up_golden: Vec<u8> = vec![
            0, 50, 100, 3, 53, 103, 9, 59, 109, 14, 64, 114, 19, 69, 119, 25, 75, 125, 28, 78, 128, 2, 52, 102, 5, 55, 105,
            11, 61, 111, 16, 66, 116, 21, 71, 121, 27, 77, 127, 30, 80, 130, 4, 54, 104, 7, 57, 107, 13, 63, 113, 18, 68,
            118, 23, 73, 123, 29, 79, 129, 32, 82, 132, 6, 56, 106, 9, 59, 109, 15, 65, 115, 20, 70, 120, 25, 75, 125, 31,
            81, 131, 34, 84, 134,
        ];
        assert_eq!(up.pixels, up_golden);

        let down = resize_bicubic(&formula(9, 8), 3, 4);
        let down_golden: Vec<u8> = vec![
            8, 58, 108, 23, 73, 123, 39, 89, 139, 54, 104, 154, 15, 65, 115, 30, 80, 130, 46, 96, 146, 61, 111, 161, 23, 73,
            123, 38, 88, 138, 54, 104, 154, 69, 119, 169,
        ];
        assert_eq!(down.pixels, down_golden);
    }

    #[test]
    fn preprocess_orders_patches_row_major_with_xy_positions() {
        // 384x384은 크기 조정이 없고, 패치는 24x24. 두 번째 패치는 (x=1, y=0), 25번째 패치(행 바뀜)는 (x=0, y=1)
        let prep = preprocess(&formula(384, 384), 70).unwrap();
        assert_eq!(prep.grid, (24, 24));
        assert_eq!(prep.positions.len(), 576);
        assert_eq!(prep.positions[0], [0, 0]);
        assert_eq!(prep.positions[1], [1, 0]);
        assert_eq!(prep.positions[24], [0, 1]);
        // 첫 패치의 첫 픽셀 (0,0): RGB = (0, 50, 100) -> 2 * (v - 0.5)
        let expect = |p: u8| 2.0 * (p as f32 * RESCALE - 0.5);
        assert!((prep.pixels[0] - expect(0)).abs() < 1e-6);
        assert!((prep.pixels[1] - expect(50)).abs() < 1e-6);
        assert!((prep.pixels[2] - expect(100)).abs() < 1e-6);
        // 패치 안 순서는 (행, 열, 채널): 두 번째 픽셀(열 1)은 3번째 원소부터
        assert!((prep.pixels[3] - expect(7)).abs() < 1e-6);
    }

    #[test]
    fn preprocess_matches_patch_count_formula() {
        let prep = preprocess(&formula(300, 200), 70).unwrap();
        assert_eq!(prep.grid, (18, 30));
        assert_eq!(prep.pixels.len(), 18 * 30 * PATCH_DIM);
    }

    #[test]
    fn video_frames_at_one_fps_for_short_clips() {
        // 10초, 30fps -> 10프레임, 30프레임 간격
        assert_eq!(video_frame_indices(300, 30.0), vec![0, 30, 60, 90, 120, 150, 180, 210, 240, 270]);
        // 4초, 25fps -> 4프레임
        assert_eq!(video_frame_indices(100, 25.0), vec![0, 25, 50, 75]);
    }

    #[test]
    fn video_frames_are_capped_and_spread_uniformly() {
        // 400초 -> 32프레임으로 제한, 간격 312.5
        let idx = video_frame_indices(10_000, 25.0);
        assert_eq!(idx.len(), VIDEO_MAX_FRAMES);
        assert_eq!(idx[0], 0);
        assert_eq!(idx[1], 312);
        assert_eq!(idx[2], 625);
        assert!(idx.iter().all(|&i| i < 10_000));
    }

    #[test]
    fn video_frames_edge_cases() {
        assert!(video_frame_indices(0, 30.0).is_empty());
        assert!(video_frame_indices(10, 0.0).is_empty());
        // 0.1초짜리 클립도 한 프레임은 가져온다
        assert_eq!(video_frame_indices(3, 30.0), vec![0]);
    }
}
