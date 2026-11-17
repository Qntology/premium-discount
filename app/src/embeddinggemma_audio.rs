//! embeddinggemma-2 오디오 경로: 16kHz 모노 PCM -> log-mel 특징 -> 오디오 타워 -> 512차원 토큰.
//!
//! 구조와 연산은 transformers 5.17의 Gemma4AudioFeatureExtractor, Gemma4AudioModel, Gemma4MultimodalEmbedder를 따른다.
//! 토큰 하나는 40ms (25토큰/초). 가중치는 `audio_tower.*`와 `embed_audio.*`만 읽는다.
//! 참조 비교는 tests/embeddinggemma_audio.rs. 입력이 30초를 넘으면 processor처럼 앞 30초만 쓴다.

use std::f64::consts::{LN_2, PI};

use candle_core::quantized::QTensor;
use candle_core::{DType, Device, Module, Result, Tensor, D};
use candle_nn::VarBuilder;

use crate::quant::{Lin, TowerQuant};

/// 입력 샘플레이트 (Hz)
pub const SAMPLE_RATE: usize = 16_000;
/// 입력 샘플 상한 (30초). feature extractor 호출의 기본 max_length
pub const MAX_SAMPLES: usize = 480_000;
/// 토큰 출력 차원 (embed_audio 출력 = 텍스트 hidden size)
pub const OUTPUT_DIM: usize = 512;
/// mel 빈 개수
pub const NUM_MEL_BINS: usize = 128;

const FRAME_LENGTH: usize = 320; // 20ms
const HOP_LENGTH: usize = 160; // 10ms
const FFT_LENGTH: usize = 512;
const NUM_BINS: usize = FFT_LENGTH / 2 + 1;
const MEL_FLOOR: f64 = 1e-3;
const PAD_MULTIPLE: usize = 128;
/// 10ms 프레임 4개가 토큰 1개 (40ms)
const FRAMES_PER_TOKEN: usize = 4;

const HIDDEN: usize = 1024;
const HEADS: usize = 8;
const HEAD_DIM: usize = HIDDEN / HEADS;
const LAYERS: usize = 12;
const FF_DIM: usize = 4096;
const CONV_KERNEL: usize = 5;
/// 왼쪽 문맥 (attention_context_left - 1). 쿼리는 자기 자신과 앞 11프레임만 본다
const CONTEXT_LEFT: usize = 12;
/// 상대 위치 임베딩 길이 (거리 0..=12)
const POS_LEN: usize = CONTEXT_LEFT + 1;
const LOGIT_CAP: f32 = 50.0;
const PROJ_DIM: usize = 1536;
const RESIDUAL_WEIGHT: f64 = 0.5;
const GRADIENT_CLIP: f64 = 1e10;
const RMS_EPS: f64 = 1e-6;
const LN_EPS: f64 = 1e-6;

/// log-mel 계산 결과
struct Mel {
    /// [frames x NUM_MEL_BINS], 행 우선. 유효하지 않은 프레임은 0
    data: Vec<f32>,
    frames: usize,
    /// 앞에서부터 연속으로 유효한 프레임 수
    valid: usize,
}

/// 아핀 변환 없는 RMS norm 또는 가중치가 있는 RMS norm. x / sqrt(mean(x^2) + eps) * w
fn rms_norm(x: &Tensor, weight: Option<&Tensor>) -> Result<Tensor> {
    let ms = x.sqr()?.mean_keepdim(D::Minus1)?.affine(1.0, RMS_EPS)?;
    let y = x.broadcast_mul(&ms.powf(-0.5)?)?;
    match weight {
        Some(w) => y.broadcast_mul(w),
        None => Ok(y),
    }
}

/// 마지막 차원에 대한 LayerNorm (편향 없음, 분산은 나누기 n)
fn layer_norm_rows(x: &Tensor, weight: &Tensor) -> Result<Tensor> {
    let mean = x.mean_keepdim(D::Minus1)?;
    let centered = x.broadcast_sub(&mean)?;
    let var = centered.sqr()?.mean_keepdim(D::Minus1)?.affine(1.0, LN_EPS)?;
    centered.broadcast_div(&var.sqrt()?)?.broadcast_mul(weight)
}

/// [1, C, H, W] 텐서에서 채널 방향 LayerNorm 후 ReLU (Gemma4AudioSubSampleConvProjectionLayer)
fn channel_norm_relu(x: &Tensor, weight: &Tensor) -> Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    let y = x.permute((0, 2, 3, 1))?.contiguous()?.reshape((b * h * w, c))?;
    let y = layer_norm_rows(&y, weight)?;
    y.reshape((b, h, w, c))?.permute((0, 3, 1, 2))?.contiguous()?.relu()
}

/// 시간 축(dim 2)에서 앞 `valid`개만 남기고 나머지를 0으로 만든다
fn mask_time(x: &Tensor, valid: usize) -> Result<Tensor> {
    let t = x.dim(2)?;
    if valid >= t {
        return Ok(x.clone());
    }
    let mut m = vec![0f32; t];
    m[..valid].fill(1.0);
    x.broadcast_mul(&Tensor::from_vec(m, (1, 1, t, 1), x.device())?)
}

fn sigmoid(x: &Tensor) -> Result<Tensor> {
    x.neg()?.exp()?.affine(1.0, 1.0)?.recip()
}

fn clip_gradient(x: &Tensor) -> Result<Tensor> {
    x.clamp(-GRADIENT_CLIP, GRADIENT_CLIP)
}

// --- log-mel 특징 (Gemma4AudioFeatureExtractor와 같은 수치) ---

fn hz_to_mel(hz: f64) -> f64 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f64) -> f64 {
    700.0 * (10f64.powf(mel / 2595.0) - 1.0)
}

/// numpy.linspace와 같다 (마지막 원소는 stop으로 맞춘다)
fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    let step = (stop - start) / (num - 1) as f64;
    let mut v: Vec<f64> = (0..num).map(|i| i as f64 * step + start).collect();
    v[num - 1] = stop;
    v
}

/// HTK 삼각 mel 필터 [NUM_BINS x NUM_MEL_BINS], 행 우선. norm 없음 (transformers mel_filter_bank와 같다)
fn mel_filter_bank() -> Vec<f64> {
    let mel_points = linspace(hz_to_mel(0.0), hz_to_mel(8000.0), NUM_MEL_BINS + 2);
    let freqs: Vec<f64> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();
    let fft_freqs = linspace(0.0, 8000.0, NUM_BINS);
    let diff: Vec<f64> = freqs.windows(2).map(|w| w[1] - w[0]).collect();
    let mut out = vec![0f64; NUM_BINS * NUM_MEL_BINS];
    for (k, &f) in fft_freqs.iter().enumerate() {
        for m in 0..NUM_MEL_BINS {
            let rise = (f - freqs[m]) / diff[m];
            let fall = (freqs[m + 2] - f) / diff[m + 1];
            out[k * NUM_MEL_BINS + m] = rise.min(fall).max(0.0);
        }
    }
    out
}

/// 주기적 Hann 창 (np.hanning(321)[:-1]을 float32로 저장한 값)
fn hann_window() -> Vec<f64> {
    (0..FRAME_LENGTH)
        .map(|n| (0.5 - 0.5 * (2.0 * PI * n as f64 / FRAME_LENGTH as f64).cos()) as f32 as f64)
        .collect()
}

/// 길이가 2의 거듭제곱인 복소 FFT (제자리, radix-2)
fn fft_in_place(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let (s, c) = (-2.0 * PI * k as f64 / len as f64).sin_cos();
                let (a, b) = (start + k, start + k + half);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// 파형 -> log-mel. 왼쪽에 160개 0을 붙이고 (semicausal), 128의 배수로 오른쪽을 0으로 채운다.
/// 프레임의 마지막 샘플 위치가 실제 음성 안에 있을 때만 유효하다 (feature extractor의 attention mask와 같다).
fn compute_mel(samples: &[f32]) -> Mel {
    let n = samples.len().min(MAX_SAMPLES);
    let padded = n.div_ceil(PAD_MULTIPLE) * PAD_MULTIPLE;
    let total = HOP_LENGTH + padded;
    let frames = if total >= FRAME_LENGTH + 1 {
        (total - (FRAME_LENGTH + 1)) / HOP_LENGTH + 1
    } else {
        0
    };
    // 왼쪽 패딩을 포함한 좌표에서 샘플 e (실제 음성은 [160, 160 + n))
    let wave = |e: usize| -> f32 {
        if e >= HOP_LENGTH && e - HOP_LENGTH < n {
            samples[e - HOP_LENGTH]
        } else {
            0.0
        }
    };

    let window = hann_window();
    let filters = mel_filter_bank();
    let mut data = vec![0f32; frames * NUM_MEL_BINS];
    let mut valid = 0;
    let mut re = vec![0f64; FFT_LENGTH];
    let mut im = vec![0f64; FFT_LENGTH];
    for i in 0..frames {
        // 프레임 끝 위치 (321 샘플 창의 마지막 칸). 실제 음성 안에 있으면 유효
        let end = i * HOP_LENGTH + FRAME_LENGTH;
        if !(end >= HOP_LENGTH && end - HOP_LENGTH < n) {
            continue;
        }
        valid += 1;

        re.fill(0.0);
        im.fill(0.0);
        // 참조는 파형과 창의 곱을 float32로 계산한 뒤 FFT에 넣는다. 같은 반올림을 따라야 저에너지 빈의 로그 차이가 줄어든다
        for k in 0..FRAME_LENGTH {
            re[k] = (wave(i * HOP_LENGTH + k) * window[k] as f32) as f64;
        }
        fft_in_place(&mut re, &mut im);
        let mag: Vec<f64> = (0..NUM_BINS).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        for m in 0..NUM_MEL_BINS {
            let mut acc = 0f64;
            for (k, &mk) in mag.iter().enumerate() {
                acc += mk * filters[k * NUM_MEL_BINS + m];
            }
            data[i * NUM_MEL_BINS + m] = (acc + MEL_FLOOR).ln() as f32;
        }
    }
    Mel { data, frames, valid }
}

/// 파형 (16kHz 모노) -> log-mel [프레임 수, 128]. 유효하지 않은 프레임은 0이다.
pub fn log_mel_spectrogram(samples: &[f32]) -> Result<Tensor> {
    let mel = compute_mel(samples);
    Tensor::from_vec(mel.data, (mel.frames, NUM_MEL_BINS), &Device::Cpu)
}

/// 입력 샘플 수가 만드는 오디오 토큰 수 (40ms당 1개, 30초 상한)
pub fn audio_token_count(samples_len: usize) -> usize {
    let n = samples_len.min(MAX_SAMPLES);
    if n == 0 {
        return 0;
    }
    // 프레임 i는 (i + 1) * 160 < n일 때 유효하다. 유효 프레임은 floor((n - 1) / 160)개다
    let valid_frames = (n - 1) / HOP_LENGTH;
    valid_frames.div_ceil(FRAMES_PER_TOKEN)
}

// --- 오디오 타워 ---

/// 클리핑이 있는 선형층 (Gemma4ClippableLinear). 입력과 출력을 학습된 범위로 자른다.
/// 가중치는 양자화할 수 있지만 클리핑 경계(input_min/max, output_min/max)는 F32 그대로다.
struct ClipLinear {
    lin: Lin,
    input_min: f64,
    input_max: f64,
    output_min: f64,
    output_max: f64,
}

impl ClipLinear {
    fn load(vb: &VarBuilder, in_dim: usize, out_dim: usize, q: &mut TowerQuant, layer: usize, key: String) -> Result<Self> {
        let scalar = |name: &str| -> Result<f64> { Ok(vb.get((), name)?.to_scalar::<f32>()? as f64) };
        Ok(Self {
            lin: q.lin(&vb.pp("linear"), layer, (out_dim, in_dim), key)?,
            input_min: scalar("input_min")?,
            input_max: scalar("input_max")?,
            output_min: scalar("output_min")?,
            output_max: scalar("output_max")?,
        })
    }

    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = x.clamp(self.input_min, self.input_max)?;
        let y = self.lin.forward(&x)?;
        y.clamp(self.output_min, self.output_max)
    }
}

struct FeedForward {
    up: ClipLinear,
    down: ClipLinear,
    pre_norm: Tensor,
    post_norm: Tensor,
}

impl FeedForward {
    fn load(vb: &VarBuilder, q: &mut TowerQuant, layer: usize, key: &str) -> Result<Self> {
        Ok(Self {
            up: ClipLinear::load(&vb.pp("ffw_layer_1"), HIDDEN, FF_DIM, q, layer, format!("{key}.ffw_layer_1"))?,
            down: ClipLinear::load(&vb.pp("ffw_layer_2"), FF_DIM, HIDDEN, q, layer, format!("{key}.ffw_layer_2"))?,
            pre_norm: vb.pp("pre_layer_norm").get(HIDDEN, "weight")?,
            post_norm: vb.pp("post_layer_norm").get(HIDDEN, "weight")?,
        })
    }

    /// 반 스텝 잔차 (residual_weight = 0.5)
    fn forward(&self, h: &Tensor) -> Result<Tensor> {
        let x = clip_gradient(h)?;
        let x = rms_norm(&x, Some(&self.pre_norm))?;
        let x = self.down.forward(&self.up.forward(&x)?.silu()?)?;
        let x = clip_gradient(&x)?;
        let x = rms_norm(&x, Some(&self.post_norm))?.affine(RESIDUAL_WEIGHT, 0.0)?;
        x + h
    }
}

/// 상대 위치 어텐션. 쿼리 t는 키 t-11..=t만 본다 (chunk 12, 왼쪽 문맥 12, 오른쪽 문맥 0과 같다)
struct ChunkAttention {
    q: ClipLinear,
    k: ClipLinear,
    v: ClipLinear,
    post: ClipLinear,
    /// 상대 위치 키 투영 [POS_LEN x HIDDEN], 행은 거리 역순 (index e는 거리 12 - e)
    relative_keys: Vec<f32>,
    /// softplus(per_dim_scale) * q_scale, 차원별 [HEAD_DIM]
    query_scale: Vec<f32>,
    key_scale: f32,
}

impl ChunkAttention {
    fn load(vb: &VarBuilder, positions: &Tensor, q: &mut TowerQuant, layer: usize, key: &str) -> Result<Self> {
        let relative_k_proj = vb.pp("relative_k_proj").get((HIDDEN, HIDDEN), "weight")?;
        let relative_keys = positions
            .matmul(&relative_k_proj.t()?)?
            .flatten_all()?
            .to_vec1::<f32>()?;

        let per_dim_scale = vb.get(HEAD_DIM, "per_dim_scale")?.to_vec1::<f32>()?;
        let q_scale = (HEAD_DIM as f64).powf(-0.5) / LN_2;
        let k_scale = (1.0 + std::f64::consts::E).ln() / LN_2;
        let query_scale = per_dim_scale
            .iter()
            .map(|&s| (q_scale * softplus(s as f64)) as f32)
            .collect();

        Ok(Self {
            q: ClipLinear::load(&vb.pp("q_proj"), HIDDEN, HIDDEN, q, layer, format!("{key}.q_proj"))?,
            k: ClipLinear::load(&vb.pp("k_proj"), HIDDEN, HIDDEN, q, layer, format!("{key}.k_proj"))?,
            v: ClipLinear::load(&vb.pp("v_proj"), HIDDEN, HIDDEN, q, layer, format!("{key}.v_proj"))?,
            post: ClipLinear::load(&vb.pp("post"), HIDDEN, HIDDEN, q, layer, format!("{key}.post"))?,
            relative_keys,
            query_scale,
            key_scale: k_scale as f32,
        })
    }

    fn forward(&self, h: &Tensor) -> Result<Tensor> {
        let t = h.dim(0)?;
        let q = self.q.forward(h)?.flatten_all()?.to_vec1::<f32>()?;
        let k: Vec<f32> = self
            .k
            .forward(h)?
            .flatten_all()?
            .to_vec1::<f32>()?
            .iter()
            .map(|x| x * self.key_scale)
            .collect();
        let v = self.v.forward(h)?.flatten_all()?.to_vec1::<f32>()?;

        let mut out = vec![0f32; t * HIDDEN];
        let cap = LOGIT_CAP;
        for qi in 0..t {
            for hd in 0..HEADS {
                let qb = qi * HIDDEN + hd * HEAD_DIM;
                let query: Vec<f32> = (0..HEAD_DIM).map(|x| q[qb + x] * self.query_scale[x]).collect();

                // 거리 d = 0..min(12, qi+1) 인 키만 쓴다. 나머지는 마스크로 0이 된다
                let span = CONTEXT_LEFT.min(qi + 1);
                let mut logits = Vec::with_capacity(span);
                for d in 0..span {
                    let kb = (qi - d) * HIDDEN + hd * HEAD_DIM;
                    let rb = (POS_LEN - 1 - d) * HIDDEN + hd * HEAD_DIM;
                    let mut ac = 0f32;
                    let mut bd = 0f32;
                    for x in 0..HEAD_DIM {
                        ac += query[x] * k[kb + x];
                        bd += query[x] * self.relative_keys[rb + x];
                    }
                    logits.push(cap * ((ac + bd) / cap).tanh());
                }

                let peak = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let weights: Vec<f32> = logits.iter().map(|l| (l - peak).exp()).collect();
                let total: f32 = weights.iter().sum();
                for (d, w) in weights.iter().enumerate() {
                    let vb_ = (qi - d) * HIDDEN + hd * HEAD_DIM;
                    let w = w / total;
                    for x in 0..HEAD_DIM {
                        out[qb + x] += w * v[vb_ + x];
                    }
                }
            }
        }
        let out = Tensor::from_vec(out, (t, HIDDEN), h.device())?;
        self.post.forward(&out)
    }
}

fn softplus(x: f64) -> f64 {
    if x > 20.0 {
        x
    } else {
        (1.0 + x.exp()).ln()
    }
}

/// 경량 컨볼루션 블록 (Gemma4AudioLightConv1d): GLU, 왼쪽 패딩 깊이별 합성곱, SiLU
struct LightConv {
    pre_norm: Tensor,
    start: ClipLinear,
    end: ClipLinear,
    depthwise: Tensor,
    conv_norm: Tensor,
}

impl LightConv {
    fn load(vb: &VarBuilder, q: &mut TowerQuant, layer: usize, key: &str) -> Result<Self> {
        Ok(Self {
            pre_norm: vb.pp("pre_layer_norm").get(HIDDEN, "weight")?,
            start: ClipLinear::load(&vb.pp("linear_start"), HIDDEN, 2 * HIDDEN, q, layer, format!("{key}.linear_start"))?,
            end: ClipLinear::load(&vb.pp("linear_end"), HIDDEN, HIDDEN, q, layer, format!("{key}.linear_end"))?,
            depthwise: vb.pp("depthwise_conv1d").get((HIDDEN, 1, CONV_KERNEL), "weight")?,
            conv_norm: vb.pp("conv_norm").get(HIDDEN, "weight")?,
        })
    }

    fn forward(&self, h: &Tensor) -> Result<Tensor> {
        let x = rms_norm(h, Some(&self.pre_norm))?;
        let x = self.start.forward(&x)?;
        let gate = sigmoid(&x.narrow(1, HIDDEN, HIDDEN)?)?;
        let x = x.narrow(1, 0, HIDDEN)?.broadcast_mul(&gate)?;

        // 시간 축 왼쪽만 패딩 (인과 합성곱)
        let x = x.t()?.contiguous()?.unsqueeze(0)?.pad_with_zeros(2, CONV_KERNEL - 1, 0)?;
        let x = x.conv1d(&self.depthwise, 0, 1, 1, HIDDEN)?;
        let x = x.squeeze(0)?.t()?.contiguous()?;

        let x = clip_gradient(&x)?;
        let x = rms_norm(&x, Some(&self.conv_norm))?.silu()?;
        let x = self.end.forward(&x)?;
        x + h
    }
}

struct AudioLayer {
    ff1: FeedForward,
    attn: ChunkAttention,
    lconv: LightConv,
    ff2: FeedForward,
    norm_pre_attn: Tensor,
    norm_post_attn: Tensor,
    norm_out: Tensor,
}

impl AudioLayer {
    fn forward(&self, h: &Tensor) -> Result<Tensor> {
        let h = self.ff1.forward(h)?;

        let x = clip_gradient(&h)?;
        let x = rms_norm(&x, Some(&self.norm_pre_attn))?;
        let x = self.attn.forward(&x)?;
        let x = clip_gradient(&x)?;
        let x = rms_norm(&x, Some(&self.norm_post_attn))?;
        let h = (x + &h)?;

        let h = self.lconv.forward(&h)?;
        let h = self.ff2.forward(&h)?;
        let h = clip_gradient(&h)?;
        rms_norm(&h, Some(&self.norm_out))
    }
}

/// 오디오 인코더 + 출력 투영 (embed_audio). 가중치는 `audio_tower.*`와 `embed_audio.*`에서 읽는다
pub struct AudioTower {
    conv0: Tensor,
    norm0: Tensor,
    conv1: Tensor,
    norm1: Tensor,
    input_proj: Tensor,
    layers: Vec<AudioLayer>,
    output_proj_w: Tensor,
    output_proj_b: Tensor,
    embed_proj: Tensor,
}

/// 상대 위치 sin/cos 표 [POS_LEN x HIDDEN]. 위치 p = 12 - e, 앞 절반은 sin, 뒤 절반은 cos
fn relative_positions(device: &Device) -> Result<Tensor> {
    let half = HIDDEN / 2;
    let log_inc = (10000f64).ln() / (half - 1) as f64;
    let inv: Vec<f32> = (0..half).map(|k| (-(k as f64) * log_inc).exp() as f32).collect();
    let mut table = vec![0f32; POS_LEN * HIDDEN];
    for e in 0..POS_LEN {
        let p = (CONTEXT_LEFT - e) as f32;
        for k in 0..half {
            let angle = p * inv[k];
            table[e * HIDDEN + k] = angle.sin();
            table[e * HIDDEN + half + k] = angle.cos();
        }
    }
    Tensor::from_vec(table, (POS_LEN, HIDDEN), device)
}

impl AudioTower {
    /// `vb`는 체크포인트 루트 VarBuilder여야 한다 (`audio_tower.*`, `embed_audio.*`를 읽는다). F32.
    pub fn load(vb: &VarBuilder, device: &Device) -> Result<Self> {
        Self::load_with(vb, device, TowerQuant::off())
    }

    /// 콘포머 층의 ClipLinear 가중치(FFN, 어텐션 q·k·v·post, LightConv start·end)만 `quant`대로 양자화한다.
    /// 클리핑 경계, 서브샘플 합성곱, 깊이별 합성곱, 상대 위치 투영, 출력 투영, 관문 투영(embed_audio), 정규화는 F32 그대로다.
    pub fn load_with(vb: &VarBuilder, device: &Device, mut quant: TowerQuant) -> Result<Self> {
        let at = vb.pp("audio_tower");
        let sub = at.pp("subsample_conv_projection");
        let positions = relative_positions(device)?;

        let mut layers = Vec::new();
        for i in 0..LAYERS {
            let l = at.pp("layers").pp(i.to_string());
            layers.push(AudioLayer {
                ff1: FeedForward::load(&l.pp("feed_forward1"), &mut quant, i, &format!("layers.{i}.feed_forward1"))?,
                attn: ChunkAttention::load(&l.pp("self_attn"), &positions, &mut quant, i, &format!("layers.{i}.self_attn"))?,
                lconv: LightConv::load(&l.pp("lconv1d"), &mut quant, i, &format!("layers.{i}.lconv1d"))?,
                ff2: FeedForward::load(&l.pp("feed_forward2"), &mut quant, i, &format!("layers.{i}.feed_forward2"))?,
                norm_pre_attn: l.pp("norm_pre_attn").get(HIDDEN, "weight")?,
                norm_post_attn: l.pp("norm_post_attn").get(HIDDEN, "weight")?,
                norm_out: l.pp("norm_out").get(HIDDEN, "weight")?,
            });
        }

        Ok(Self {
            conv0: sub.pp("layer0").pp("conv").get((128, 1, 3, 3), "weight")?,
            norm0: sub.pp("layer0").pp("norm").get(128, "weight")?,
            conv1: sub.pp("layer1").pp("conv").get((32, 128, 3, 3), "weight")?,
            norm1: sub.pp("layer1").pp("norm").get(32, "weight")?,
            input_proj: sub.pp("input_proj_linear").get((HIDDEN, HIDDEN), "weight")?,
            layers,
            output_proj_w: at.pp("output_proj").get((PROJ_DIM, HIDDEN), "weight")?,
            output_proj_b: at.pp("output_proj").get(PROJ_DIM, "bias")?,
            embed_proj: vb.pp("embed_audio").pp("embedding_projection").get((OUTPUT_DIM, PROJ_DIM), "weight")?,
        })
    }

    fn clip_linears(&self) -> Vec<(String, &ClipLinear)> {
        let mut out = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            for (part, ff) in [("feed_forward1", &l.ff1), ("feed_forward2", &l.ff2)] {
                out.push((format!("layers.{i}.{part}.ffw_layer_1"), &ff.up));
                out.push((format!("layers.{i}.{part}.ffw_layer_2"), &ff.down));
            }
            for (name, c) in [("q_proj", &l.attn.q), ("k_proj", &l.attn.k), ("v_proj", &l.attn.v), ("post", &l.attn.post)] {
                out.push((format!("layers.{i}.self_attn.{name}"), c));
            }
            out.push((format!("layers.{i}.lconv1d.linear_start"), &l.lconv.start));
            out.push((format!("layers.{i}.lconv1d.linear_end"), &l.lconv.end));
        }
        out
    }

    /// 양자화된 선형층 (캐시에 쓸 때)
    pub fn quantized_tensors(&self) -> Vec<(String, &QTensor)> {
        self.clip_linears().into_iter().filter_map(|(k, c)| c.lin.qtensor().map(|q| (k, q))).collect()
    }

    /// 선형층 가중치 크기 (바이트)
    pub fn linear_bytes(&self) -> usize {
        self.clip_linears().iter().map(|(_, c)| c.lin.weight_bytes()).sum()
    }

    /// 16kHz 모노 PCM -> [토큰 수, 512]. 토큰 수는 `audio_token_count(samples.len())`와 같다.
    pub fn encode_pcm(&self, samples: &[f32]) -> Result<Tensor> {
        let mel = compute_mel(samples);
        if mel.valid == 0 {
            return Tensor::zeros((0, OUTPUT_DIM), DType::F32, &Device::Cpu);
        }
        let tokens = mel.valid.div_ceil(FRAMES_PER_TOKEN);

        let mut h = self.subsample(&mel)?.narrow(0, 0, tokens)?;
        for layer in &self.layers {
            h = layer.forward(&h)?;
        }
        let h = h.matmul(&self.output_proj_w.t()?)?.broadcast_add(&self.output_proj_b)?;
        let h = rms_norm(&h, None)?;
        h.matmul(&self.embed_proj.t()?)
    }

    /// 두 번의 2배 다운샘플링 합성곱 (프레임 4개 -> 1개) 후 입력 투영. 결과는 [ceil(frames / 4), 1024]
    fn subsample(&self, mel: &Mel) -> Result<Tensor> {
        // Python은 [B, F, 128]을 [B, 1, F(시간), 128(주파수)]로 본다
        let x = Tensor::from_slice(&mel.data, (1, 1, mel.frames, NUM_MEL_BINS), &Device::Cpu)?;
        let x = x.conv2d(&self.conv0, 1, 2, 1, 1)?;
        let x = channel_norm_relu(&x, &self.norm0)?;
        let x = mask_time(&x, mel.valid.div_ceil(2))?;
        let x = x.conv2d(&self.conv1, 1, 2, 1, 1)?;
        let x = channel_norm_relu(&x, &self.norm1)?;

        let (_, _, t2, _) = x.dims4()?;
        let x = x.permute((0, 2, 3, 1))?.contiguous()?.reshape((t2, 32 * 32))?;
        x.matmul(&self.input_proj.t()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_matches_naive_dft() {
        let input: Vec<f64> = (0..FFT_LENGTH)
            .map(|i| 0.3 * (i as f64 * 0.37).sin() + 0.1 * (i as f64 * 1.9).cos())
            .collect();
        let mut re = input.clone();
        let mut im = vec![0f64; FFT_LENGTH];
        fft_in_place(&mut re, &mut im);
        for k in (0..FFT_LENGTH).step_by(37) {
            let (mut sr, mut si) = (0f64, 0f64);
            for (t, x) in input.iter().enumerate() {
                let (s, c) = (-2.0 * PI * (k * t) as f64 / FFT_LENGTH as f64).sin_cos();
                sr += x * c;
                si += x * s;
            }
            assert!((re[k] - sr).abs() < 1e-9 && (im[k] - si).abs() < 1e-9, "bin {k}");
        }
    }

    #[test]
    fn mel_filters_are_triangular_weights() {
        let filters = mel_filter_bank();
        assert_eq!(filters.len(), NUM_BINS * NUM_MEL_BINS);
        assert!(filters.iter().all(|&v| (0.0..=1.0).contains(&v)));
        // HF도 경고하는 상단 필터 하나는 FFT 빈 사이에 떨어져 0일 수 있다
        let empty = (0..NUM_MEL_BINS)
            .filter(|&m| (0..NUM_BINS).all(|k| filters[k * NUM_MEL_BINS + m] == 0.0))
            .count();
        assert!(empty <= 1, "empty filters: {empty}");
    }

    #[test]
    fn hann_window_is_periodic() {
        let w = hann_window();
        assert_eq!(w.len(), FRAME_LENGTH);
        assert_eq!(w[0], 0.0);
        assert!((w[HOP_LENGTH] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn token_count_matches_reference_table() {
        // transformers 5.17 참조 구현이 낸 토큰 수 (tests/fixtures/audio_reference.json의 token_counts)
        for (n, tokens) in [
            (1, 0),
            (100, 0),
            (640, 1),
            (1000, 2),
            (12345, 20),
            (16000, 25),
            (48000, 75),
            (500_000, 750),
        ] {
            assert_eq!(audio_token_count(n), tokens, "samples {n}");
        }
    }

    #[test]
    fn one_second_gives_99_valid_frames() {
        let mel = compute_mel(&vec![0.1f32; 16_000]);
        assert_eq!((mel.frames, mel.valid), (99, 99));
        assert_eq!(mel.data.len(), 99 * NUM_MEL_BINS);
    }

    #[test]
    fn short_input_has_no_frames() {
        let mel = compute_mel(&vec![0.1f32; 100]);
        assert_eq!((mel.frames, mel.valid), (0, 0));
    }

    #[test]
    fn padded_tail_frame_is_zeroed() {
        // 16129 샘플: 128의 배수로 채우면 프레임 101개가 되지만, 마지막 프레임은 실제 음성 밖이다
        let mel = compute_mel(&vec![0.1f32; 16_129]);
        assert_eq!((mel.frames, mel.valid), (101, 100));
        let last = &mel.data[100 * NUM_MEL_BINS..];
        assert!(last.iter().all(|&v| v == 0.0));
        assert!(mel.data[99 * NUM_MEL_BINS..100 * NUM_MEL_BINS].iter().any(|&v| v != 0.0));
    }
}
