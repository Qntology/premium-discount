//! The SigLIP2 So400m/16 NaFlex vision tower on candle — Raon-VisionEncoder's timm `vit_so400m_patch16_siglip_256`
//! trunk (27 pre-norm blocks, fused qkv, GELU-tanh MLP) and its MAP head (`attn_pool`: one latent query) — in the
//! released NaFlex 1-D mode: each 16×16 patch through the patch projection as a linear map, the 16×16 position table
//! bilinearly resized to the patch grid (`align_corners=False`; without antialias as timm's `_setup_1d_forward` does
//! for Raon-VE, antialiased as transformers does for `google/siglip2-so400m-patch16-naflex`). Both checkpoints load
//! through [`crate::layout::Weights`].
//!
//! Besides the dense pass it has the sparse passes of noto-sandbox's `noto-ml` (`VisionTower::forward_sparse /
//! extend_sparse / merge_sparse`): only some cells of the grid go through the encoder, each with its own cell's position
//! embedding (the holes are absent, not blacked out); cells opened later attend to every token computed so far through
//! the keys and values the earlier pass kept. With every cell in grid order the sparse pass *is* the dense pass.
//!
//! 개인 화면: 인코딩 한 번은 CPU 에서 수 초 걸린다. 그사이 화면이 민감해지면(개인 화면 잠금이 세대 `privacy_epoch` 를
//! 올리면) `*_cancellable` 패스는 다음 블록 경계에서 [`VisionAbort::PrivacyLocked`] 로 멈춰, 쌓던 토큰 · 층별 키 · 값을
//! 바로 놓는다 ([`EpochWatch`]).

use anyhow::{bail, Context, Result};
use candle_core::quantized::{ggml_file::qtensor_from_ggml, GgmlDType, QMatMul};
use candle_core::{DType, Device, DeviceLocation, Module, Tensor, D};
use candle_nn::LayerNorm;
use premium_discount_quant::kquant::{encode, Format};
use premium_discount_quant::safetensors::SafeTensors;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::layout::{Layout, Weights};
use std::sync::atomic::{AtomicU64, Ordering};
use premium_discount_sensor::naflex::PatchGrid;

/// 판독을 그만둔 까닭. `anyhow::Error` 로 감싸 올라오므로 [`VisionAbort::of`] 로 알아본다
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisionAbort {
    /// 인코딩 도중 개인 화면 세대가 바뀌었다: 이 프레임은 민감한 화면일 수 있으니 결과를 내지 않는다
    PrivacyLocked,
}

impl std::fmt::Display for VisionAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the privacy epoch changed during the vision pass")
    }
}

impl std::error::Error for VisionAbort {}

impl VisionAbort {
    /// `err` 가 이 중단인가
    pub fn of(err: &anyhow::Error) -> Option<VisionAbort> {
        err.downcast_ref::<VisionAbort>().copied()
    }
}

/// 개인 화면 세대 감시: 프레임을 받을 때 읽은 세대(`captured`)가 지금도 같은지 본다. 개인 화면 잠금이 `epoch` 를 올리면
/// 그 전에 받은 프레임의 인코딩은 다음 경계에서 멈춘다
#[derive(Clone, Copy, Debug)]
pub struct EpochWatch<'a> {
    pub epoch: &'a AtomicU64,
    pub captured: u64,
}

impl<'a> EpochWatch<'a> {
    pub fn new(epoch: &'a AtomicU64, captured: u64) -> EpochWatch<'a> {
        EpochWatch { epoch, captured }
    }

    /// 지금 세대를 읽어 둔다 (프레임을 받을 때 부른다)
    pub fn capture(epoch: &'a AtomicU64) -> EpochWatch<'a> {
        EpochWatch {
            epoch,
            captured: epoch.load(Ordering::Acquire),
        }
    }

    pub fn is_current(&self) -> bool {
        self.epoch.load(Ordering::Acquire) == self.captured
    }

    pub fn check(&self) -> std::result::Result<(), VisionAbort> {
        if self.is_current() {
            Ok(())
        } else {
            Err(VisionAbort::PrivacyLocked)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisionConfig {
    pub hidden: usize,
    pub heads: usize,
    pub layers: usize,
    pub mlp: usize,
    pub patch: usize,
    /// Side of the learned position table (16 → 256 positions).
    pub side: usize,
    pub eps: f64,
}

impl Default for VisionConfig {
    fn default() -> Self {
        VisionConfig {
            hidden: 1152,
            heads: 16,
            layers: 27,
            mlp: 4304,
            patch: 16,
            side: 16,
            eps: 1e-6,
        }
    }
}

/// How the large matrices are kept on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeightFormat {
    F32,
    F16,
    BF16,
    /// ggml Q8_0 blocks (the MLP width is padded 4304 → 4320 with zero rows / columns, which add nothing).
    Q8,
}

impl std::str::FromStr for WeightFormat {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "f32" => WeightFormat::F32,
            "f16" => WeightFormat::F16,
            "bf16" => WeightFormat::BF16,
            "q8" | "q8_0" => WeightFormat::Q8,
            _ => bail!("unknown weight format '{s}' (f32, f16, bf16, q8)"),
        })
    }
}

impl WeightFormat {
    /// The activation dtype.
    pub fn dtype(self) -> DType {
        match self {
            WeightFormat::F16 => DType::F16,
            WeightFormat::BF16 => DType::BF16,
            WeightFormat::F32 | WeightFormat::Q8 => DType::F32,
        }
    }
}

/// `y = x·Wᵀ (+ b)` with W dense or Q8_0.
pub(crate) struct Proj {
    inner: QMatMul,
    bias: Option<Tensor>,
}

impl Proj {
    pub(crate) fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let xc = x.contiguous()?;
        let y = match &self.inner {
            QMatMul::QTensor(_)
                if x.device().is_cuda() || x.device().is_metal() || x.device().is_rocm() =>
            {
                self.inner.forward_exact(&xc)?
            }
            _ => self.inner.forward(&xc)?,
        };
        match &self.bias {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }
}

/// Reads weights from the checkpoint (mmap, f32; Raon-VE names in either layout) into the chosen format.
pub(crate) struct Loader<'a> {
    pub st: &'a Weights<'a>,
    pub device: Device,
    pub format: WeightFormat,
}

impl Loader<'_> {
    pub(crate) fn dtype(&self) -> DType {
        self.format.dtype()
    }

    pub(crate) fn tensor(&self, name: &str, shape: &[usize]) -> Result<Tensor> {
        let data = self.st.all_f32(name).with_context(|| name.to_string())?;
        if data.len() != shape.iter().product::<usize>() {
            bail!("{name}: {} values for shape {shape:?}", data.len());
        }
        Ok(Tensor::from_vec(data, shape, &self.device)?.to_dtype(self.dtype())?)
    }

    pub(crate) fn norm(&self, prefix: &str, d: usize, eps: f64) -> Result<LayerNorm> {
        Ok(LayerNorm::new(
            self.tensor(&format!("{prefix}.weight"), &[d])?,
            self.tensor(&format!("{prefix}.bias"), &[d])?,
            eps,
        ))
    }

    /// A projection `[out, in]` (+ bias `[out]`), zero-padded to `[out_pad, in_pad]`; Q8_0 when the format says so
    /// and `in_pad` is a whole number of 32-wide blocks.
    pub(crate) fn proj(&self, weight: &str, bias: Option<&str>, out_pad: usize, in_pad: usize) -> Result<Proj> {
        let shape = self.st.shape(weight)?;
        let (out, inp) = match shape.len() {
            2 => (shape[0], shape[1]),
            4 => (shape[0], shape[1..].iter().product()), // the patch conv as a linear map
            _ => bail!("{weight}: unexpected shape {shape:?}"),
        };
        let mut w = self.st.all_f32(weight)?;
        if (out_pad, in_pad) != (out, inp) {
            let mut p = vec![0f32; out_pad * in_pad];
            for r in 0..out {
                p[r * in_pad..r * in_pad + inp].copy_from_slice(&w[r * inp..(r + 1) * inp]);
            }
            w = p;
        }
        let inner = if self.format == WeightFormat::Q8 && in_pad.is_multiple_of(32) {
            let bytes = encode(&w, in_pad, Format::Q8_0, None).map_err(|e| anyhow::anyhow!("{weight}: {e}"))?;
            QMatMul::from_qtensor(qtensor_from_ggml(GgmlDType::Q8_0, &bytes, vec![out_pad, in_pad], &self.device)?)?
        } else {
            QMatMul::Tensor(Tensor::from_vec(w, (out_pad, in_pad), &self.device)?.to_dtype(self.dtype())?)
        };
        let bias = match bias {
            Some(b) => {
                let mut v = self.st.all_f32(b)?;
                v.resize(out_pad, 0.0);
                Some(Tensor::from_vec(v, out_pad, &self.device)?.to_dtype(self.dtype())?)
            }
            None => None,
        };
        Ok(Proj { inner, bias })
    }
}

/// The MLP width the format stores (Q8_0 blocks are 32 wide).
pub(crate) fn mlp_width(mlp: usize, format: WeightFormat) -> usize {
    if format == WeightFormat::Q8 {
        mlp.div_ceil(32) * 32
    } else {
        mlp
    }
}

fn gpu_flash_attn_key(dev: &Device, hd: usize) -> Option<(u8, usize, usize)> {
    match dev.location() {
        DeviceLocation::Cpu => None,
        DeviceLocation::Cuda { gpu_id } => Some((1, gpu_id, hd)),
        DeviceLocation::Rocm { gpu_id } => Some((2, gpu_id, hd)),
        DeviceLocation::Vulkan { gpu_id } => Some((3, gpu_id, hd)),
        DeviceLocation::Metal { gpu_id } => Some((4, gpu_id, hd)),
        DeviceLocation::OpenCl { gpu_id } => Some((5, gpu_id, hd)),
    }
}

fn gpu_flash_attn_ok(dev: &Device, hd: usize) -> bool {
    if hd == 0 || hd > 256 {
        return false;
    }
    let Some(key) = gpu_flash_attn_key(dev, hd) else {
        return false;
    };
    static CACHE: OnceLock<Mutex<HashMap<(u8, usize, usize), bool>>> = OnceLock::new();
    let map = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(&ok) = map.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return ok;
    }
    let det = |n: usize, seed: u64| -> Vec<f32> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
            })
            .collect()
    };
    let scale = (hd as f32).powf(-0.5);
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> candle_core::Result<bool> {
        let (bh, n) = (2usize, 8usize);
        let q_cpu = Tensor::from_vec(det(bh * n * hd, 71), (1, bh, n, hd), &Device::Cpu)?;
        let k_cpu = Tensor::from_vec(det(bh * n * hd, 72), (1, bh, n, hd), &Device::Cpu)?;
        let v_cpu = Tensor::from_vec(det(bh * n * hd, 73), (1, bh, n, hd), &Device::Cpu)?;
        let scores = (q_cpu.matmul(&k_cpu.transpose(2, 3)?.contiguous()?)? * scale as f64)?;
        let want = candle_nn::ops::softmax_last_dim(&scores)?.matmul(&v_cpu)?;
        let cfg = candle_nn::flash_attn::FlashAttnConfig::new(scale);
        let got = candle_nn::flash_attn::flash_attn_bhsd(
            &q_cpu.to_device(dev)?,
            &k_cpu.to_device(dev)?,
            &v_cpu.to_device(dev)?,
            &cfg,
        )?
        .to_device(&Device::Cpu)?
        .to_dtype(DType::F32)?;
        let diff_sq = (&got - &want)?.sqr()?.sum_all()?.to_scalar::<f32>()?;
        let norm_sq = want.sqr()?.sum_all()?.to_scalar::<f32>()?;
        let rel = (diff_sq / norm_sq.max(1e-30)).sqrt();
        Ok(rel.is_finite() && rel <= 5e-3)
    }))
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or(false);
    map.lock().unwrap_or_else(|e| e.into_inner()).insert(key, ok);
    ok
}

pub(crate) fn attend(q: &Tensor, k: &Tensor, v: &Tensor, hd: usize) -> candle_core::Result<Tensor> {
    let dtype = q.dtype();
    if q.device().is_cpu() && dtype == DType::F32 {
        let (b, heads, nq, d) = q.dims4()?;
        let (_, kv_heads, nkv, _) = k.dims4()?;
        if b == 1 && d == hd && heads > 0 && kv_heads > 0 && heads % kv_heads == 0 {
            let q_shd = q.squeeze(0)?.transpose(0, 1)?.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let k_shd = k.squeeze(0)?.transpose(0, 1)?.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let v_shd = v.squeeze(0)?.transpose(0, 1)?.contiguous()?.flatten_all()?.to_vec1::<f32>()?;
            let mut out = vec![0.0f32; nq * heads * hd];
            let cfg = premium_discount_cpu_kernels::CpuStreamAttnCfg {
                num_heads: heads,
                num_kv_heads: kv_heads,
                head_dim: hd,
                scale: (hd as f32).powf(-0.5),
                causal: false,
                past_offset: 0,
            };
            premium_discount_cpu_kernels::stream_attention_f32(&q_shd, &k_shd, &v_shd, nq, nkv, &cfg, &mut out);
            return Tensor::from_vec(out, (1, nq, heads, hd), q.device())?.transpose(1, 2);
        }
    }
    if !q.device().is_cpu() && gpu_flash_attn_ok(q.device(), hd) {
        let cfg = candle_nn::flash_attn::FlashAttnConfig::new((hd as f32).powf(-0.5));
        if let Ok(out) = candle_nn::flash_attn::flash_attn_bhsd(q, k, v, &cfg) {
            return Ok(out);
        }
    }
    let scores = (q.matmul(&k.t()?)? * (hd as f64).powf(-0.5))?;
    let probs = candle_nn::ops::softmax_last_dim(&scores.to_dtype(DType::F32)?)?.to_dtype(dtype)?;
    probs.matmul(v)
}

/// A layer's own keys and values `(1, heads, n, head_dim)`, for later passes to attend to.
#[derive(Clone, Debug)]
pub struct LayerKv {
    pub k: Tensor,
    pub v: Tensor,
}

impl LayerKv {
    fn select(&self, rows: &Tensor) -> candle_core::Result<LayerKv> {
        Ok(LayerKv {
            k: self.k.index_select(rows, 2)?,
            v: self.v.index_select(rows, 2)?,
        })
    }
}

struct Block {
    norm1: LayerNorm,
    qkv: Proj,
    proj: Proj,
    norm2: LayerNorm,
    fc1: Proj,
    fc2: Proj,
    heads: usize,
    hd: usize,
}

impl Block {
    fn load(l: &Loader<'_>, i: usize, cfg: &VisionConfig) -> Result<Block> {
        let p = format!("visual.trunk.blocks.{i}");
        let (d, m) = (cfg.hidden, mlp_width(cfg.mlp, l.format));
        Ok(Block {
            norm1: l.norm(&format!("{p}.norm1"), d, cfg.eps)?,
            qkv: l.proj(&format!("{p}.attn.qkv.weight"), Some(&format!("{p}.attn.qkv.bias")), 3 * d, d)?,
            proj: l.proj(&format!("{p}.attn.proj.weight"), Some(&format!("{p}.attn.proj.bias")), d, d)?,
            norm2: l.norm(&format!("{p}.norm2"), d, cfg.eps)?,
            fc1: l.proj(&format!("{p}.mlp.fc1.weight"), Some(&format!("{p}.mlp.fc1.bias")), m, d)?,
            fc2: l.proj(&format!("{p}.mlp.fc2.weight"), Some(&format!("{p}.mlp.fc2.bias")), d, m)?,
            heads: cfg.heads,
            hd: d / cfg.heads,
        })
    }

    /// `x + attn(norm1 x)`, then `+ mlp(norm2 ·)`; the attention also sees `ctx`'s keys and values.
    fn forward_kv(&self, x: &Tensor, ctx: Option<&LayerKv>) -> candle_core::Result<(Tensor, LayerKv)> {
        let (b, n, d) = x.dims3()?;
        let h = self.norm1.forward(x)?;
        let qkv = self
            .qkv
            .forward(&h)?
            .reshape((b, n, 3, self.heads, self.hd))?
            .permute((2, 0, 3, 1, 4))?;
        let (q, k, v) = (qkv.get(0)?.contiguous()?, qkv.get(1)?.contiguous()?, qkv.get(2)?.contiguous()?);
        let (ka, va) = match ctx {
            Some(c) => (Tensor::cat(&[&c.k, &k], 2)?, Tensor::cat(&[&c.v, &v], 2)?),
            None => (k.clone(), v.clone()),
        };
        let o = attend(&q, &ka, &va, self.hd)?.transpose(1, 2)?.reshape((b, n, d))?;
        let x = (x + self.proj.forward(&o)?)?;
        let m = self.fc2.forward(&self.fc1.forward(&self.norm2.forward(&x)?)?.gelu()?)?;
        Ok(((x + m)?, LayerKv { k, v }))
    }
}

/// timm `AttentionPoolLatent`: one latent query attends over the tokens, then `proj`, then `+ mlp(norm ·)`.
struct MapHead {
    /// Precomputed `q.forward(latent)` shaped `(1, heads, 1, hd)` — avoids recomputing the constant latent projection every pass.
    q_latent: Tensor,
    kv: Proj,
    proj: Proj,
    norm: LayerNorm,
    fc1: Proj,
    fc2: Proj,
    heads: usize,
    hd: usize,
}

impl MapHead {
    fn load(l: &Loader<'_>, cfg: &VisionConfig) -> Result<MapHead> {
        let p = "visual.trunk.attn_pool";
        let (d, m) = (cfg.hidden, mlp_width(cfg.mlp, l.format));
        let heads = cfg.heads;
        let hd = d / heads;
        let latent = l.tensor(&format!("{p}.latent"), &[1, 1, d])?;
        let q = l.proj(&format!("{p}.q.weight"), Some(&format!("{p}.q.bias")), d, d)?;
        let q_latent = q
            .forward(&latent)?
            .reshape((1, 1, heads, hd))?
            .transpose(1, 2)?
            .contiguous()?;
        Ok(MapHead {
            q_latent,
            kv: l.proj(&format!("{p}.kv.weight"), Some(&format!("{p}.kv.bias")), 2 * d, d)?,
            proj: l.proj(&format!("{p}.proj.weight"), Some(&format!("{p}.proj.bias")), d, d)?,
            norm: l.norm(&format!("{p}.norm"), d, cfg.eps)?,
            fc1: l.proj(&format!("{p}.mlp.fc1.weight"), Some(&format!("{p}.mlp.fc1.bias")), m, d)?,
            fc2: l.proj(&format!("{p}.mlp.fc2.weight"), Some(&format!("{p}.mlp.fc2.bias")), d, m)?,
            heads,
            hd,
        })
    }

    fn residual_mlp(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        x + self.fc2.forward(&self.fc1.forward(&self.norm.forward(x)?)?.gelu()?)?
    }

    fn q_for_batch(&self, b: usize) -> candle_core::Result<Tensor> {
        if b == 1 {
            Ok(self.q_latent.clone())
        } else {
            self.q_latent.broadcast_as((b, self.heads, 1, self.hd))?.contiguous()
        }
    }

    /// The pooled embedding `(b, d)` of tokens `(b, n, d)`.
    fn pooled(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let (b, n, d) = x.dims3()?;
        let q = self.q_for_batch(b)?;
        let kv = self
            .kv
            .forward(x)?
            .reshape((b, n, 2, self.heads, self.hd))?
            .permute((2, 0, 3, 1, 4))?;
        let (k, v) = (kv.get(0)?.contiguous()?, kv.get(1)?.contiguous()?);
        let o = attend(&q, &k, &v, self.hd)?.transpose(1, 2)?.reshape((b, 1, d))?;
        self.residual_mlp(&self.proj.forward(&o)?)?.reshape((b, d))
    }

    /// Per-token features in the pooled space (the value path, as if each token attended only to itself; MaskCLIP).
    fn dense(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let d = x.dim(D::Minus1)?;
        let v = self.kv.forward(x)?.narrow(D::Minus1, d, d)?;
        self.residual_mlp(&self.proj.forward(&v)?)
    }

    /// Single-pass pooled `(b, d)` and per-token dense `(b, n, d)` features sharing one `kv.forward(x)` projection.
    fn pooled_and_dense(&self, x: &Tensor) -> candle_core::Result<(Tensor, Tensor)> {
        let (b, n, d) = x.dims3()?;
        let kv_raw = self.kv.forward(x)?;
        let q = self.q_for_batch(b)?;
        let kv = kv_raw
            .reshape((b, n, 2, self.heads, self.hd))?
            .permute((2, 0, 3, 1, 4))?;
        let (k, v) = (kv.get(0)?.contiguous()?, kv.get(1)?.contiguous()?);
        let o = attend(&q, &k, &v, self.hd)?.transpose(1, 2)?.reshape((b, 1, d))?;
        let pooled = self.residual_mlp(&self.proj.forward(&o)?)?.reshape((b, d))?;
        let v_flat = kv_raw.narrow(D::Minus1, d, d)?;
        let dense = self.residual_mlp(&self.proj.forward(&v_flat)?)?;
        Ok((pooled, dense))
    }
}

/// An encoder pass over some cells of a patch grid: the cells, their tokens after the final norm `(1, n, d)`, and
/// every layer's keys and values for a later pass to attend to.
#[derive(Clone, Debug)]
pub struct SparsePass {
    pub cells: Vec<u32>,
    pub tokens: Tensor,
    kv: Vec<LayerKv>,
}

pub struct VisionTower {
    /// Which checkpoint the weights came from.
    pub layout: Layout,
    patch: Proj,
    pos: Tensor,
    blocks: Vec<Block>,
    norm: LayerNorm,
    head: MapHead,
    pub cfg: VisionConfig,
    pub format: WeightFormat,
    device: Device,
    pos_cache: Mutex<HashMap<(usize, usize), Tensor>>,
}

/// PyTorch's bilinear weights for `src → dst` with `align_corners=False` and no antialias: `(dst, src)`, two taps a row.
pub fn bilinear_weights(src: usize, dst: usize) -> Vec<f32> {
    let scale = src as f64 / dst as f64;
    let mut w = vec![0f32; dst * src];
    for i in 0..dst {
        let x = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
        let i0 = (x.floor() as usize).min(src - 1);
        let i1 = (i0 + 1).min(src - 1);
        let l1 = (x - i0 as f64) as f32;
        w[i * src + i0] += 1.0 - l1;
        w[i * src + i1] += l1;
    }
    w
}

/// torch's bilinear weights with `antialias=True` (`align_corners=False`): when shrinking, the triangle filter is
/// stretched by the scale (more than two taps a row); when growing, the same as [`bilinear_weights`]. Port of
/// noto-sandbox's `noto-ml` `antialias_bilinear_weights`.
pub fn antialias_bilinear_weights(src: usize, dst: usize) -> Vec<f32> {
    let scale = src as f64 / dst as f64;
    let (support, inv_scale) = if scale >= 1.0 { (scale, 1.0 / scale) } else { (1.0, 1.0) };
    let triangle = |x: f64| (1.0 - x.abs()).max(0.0);
    let mut weights = vec![0f32; dst * src];
    for o in 0..dst {
        let center = scale * (o as f64 + 0.5);
        let lo = ((center - support + 0.5) as i64).max(0) as usize;
        let hi = ((center + support + 0.5) as i64).min(src as i64).max(lo as i64) as usize;
        let taps: Vec<f64> = (lo..hi).map(|j| triangle((j as f64 - center + 0.5) * inv_scale)).collect();
        let total: f64 = taps.iter().sum();
        if total > 0.0 {
            for (k, t) in taps.iter().enumerate() {
                weights[o * src + lo + k] = (t / total) as f32;
            }
        }
    }
    weights
}

impl VisionTower {
    /// Loads a Raon-VisionEncoder or a Hugging Face `google/siglip2-so400m-patch16-naflex` checkpoint (told apart by
    /// the tensor names, see [`crate::layout`]).
    pub fn load(st: &SafeTensors, device: &Device, format: WeightFormat) -> Result<VisionTower> {
        let cfg = VisionConfig::default();
        let w = Weights::new(st);
        let l = Loader {
            st: &w,
            device: device.clone(),
            format,
        };
        let d = cfg.hidden;
        let pd = 3 * cfg.patch * cfg.patch;
        let blocks = (0..cfg.layers).map(|i| Block::load(&l, i, &cfg)).collect::<Result<Vec<_>>>()?;
        Ok(VisionTower {
            layout: w.layout,
            patch: l.proj(
                "visual.trunk.patch_embed.proj.weight",
                Some("visual.trunk.patch_embed.proj.bias"),
                d,
                pd,
            )?,
            pos: Tensor::from_vec(w.all_f32("visual.trunk.pos_embed")?, (cfg.side * cfg.side, d), device)?,
            blocks,
            norm: l.norm("visual.trunk.norm", d, cfg.eps)?,
            head: MapHead::load(&l, &cfg)?,
            cfg,
            format,
            device: device.clone(),
            pos_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The position table resized to `gh × gw` (f32 resample, then the activation dtype), cached per grid.
    fn positions(&self, gh: usize, gw: usize) -> Result<Tensor> {
        if let Some(t) = self.pos_cache.lock().unwrap_or_else(|p| p.into_inner()).get(&(gh, gw)) {
            return Ok(t.clone());
        }
        let (s, d) = (self.cfg.side, self.cfg.hidden);
        let t = if (gh, gw) == (s, s) {
            self.pos.clone()
        } else {
            let weights = if self.layout.pos_antialias() {
                antialias_bilinear_weights
            } else {
                bilinear_weights
            };
            let wy = Tensor::from_vec(weights(s, gh), (gh, s), &self.device)?;
            let wx = Tensor::from_vec(weights(s, gw), (gw, s), &self.device)?;
            // rows: (gh, s) · (s, s·d) → (gh, s, d); columns: (gw, s) · (s, d) per row → (gh, gw, d)
            let by_rows = wy.matmul(&self.pos.reshape((s, s * d))?)?.reshape((gh, s, d))?;
            wx.broadcast_left(gh)?.contiguous()?.matmul(&by_rows)?.reshape((gh * gw, d))?
        }
        .to_dtype(self.format.dtype())?;
        let mut cache = self.pos_cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert((gh, gw), t.clone());
        Ok(t)
    }

    /// The patches of `cells` through the patch projection, each with its own cell's position: `(1, n, d)`.
    fn cell_tokens(&self, grid: &PatchGrid, cells: &[u32]) -> Result<Tensor> {
        let n = grid.cells();
        let mut flat = Vec::with_capacity(cells.len() * PatchGrid::DIM);
        for &c in cells {
            if c as usize >= n {
                bail!("cell {c} is outside the {}x{} grid", grid.grid_h, grid.grid_w);
            }
            flat.extend_from_slice(grid.patch(c as usize));
        }
        let patches = Tensor::from_vec(flat, (1, cells.len(), PatchGrid::DIM), &self.device)?.to_dtype(self.format.dtype())?;
        let rows = Tensor::from_slice(cells, cells.len(), &self.device)?;
        let pos = self.positions(grid.grid_h, grid.grid_w)?.index_select(&rows, 0)?.unsqueeze(0)?;
        Ok(self.patch.forward(&patches)?.broadcast_add(&pos)?)
    }

    /// The encoder over `cells` only: they attend to each other and nothing else.
    pub fn forward_sparse(&self, grid: &PatchGrid, cells: &[u32]) -> Result<SparsePass> {
        self.forward_sparse_gated(grid, cells, &mut || Ok(()))
    }

    /// [`VisionTower::forward_sparse`], 블록마다 `watch` 를 보며: 개인 화면 세대가 바뀌면 다음 블록에 들어가기 전에
    /// [`VisionAbort::PrivacyLocked`] 로 멈춘다 (그때까지의 토큰 · 키 · 값은 바로 놓는다)
    pub fn forward_sparse_cancellable(&self, grid: &PatchGrid, cells: &[u32], watch: &EpochWatch<'_>) -> Result<SparsePass> {
        self.forward_sparse_gated(grid, cells, &mut || Ok(watch.check()?))
    }

    /// 블록에 들어가기 전마다 `gate` 를 부르는 [`VisionTower::forward_sparse`]. `gate` 가 `Err` 면 그대로 멈춘다
    pub(crate) fn forward_sparse_gated(&self, grid: &PatchGrid, cells: &[u32], gate: &mut dyn FnMut() -> Result<()>) -> Result<SparsePass> {
        let mut x = self.cell_tokens(grid, cells)?;
        let mut kv = Vec::with_capacity(self.blocks.len());
        for b in &self.blocks {
            gate()?;
            let (y, layer_kv) = b.forward_kv(&x, None)?;
            x = y;
            kv.push(layer_kv);
        }
        Ok(SparsePass {
            cells: cells.to_vec(),
            tokens: self.norm.forward(&x)?,
            kv,
        })
    }

    /// Every cell in grid order: the dense pass.
    pub fn forward_full(&self, grid: &PatchGrid) -> Result<SparsePass> {
        let cells: Vec<u32> = (0..grid.cells() as u32).collect();
        self.forward_sparse(grid, &cells)
    }

    /// Opens `cells` on top of `first`: at every layer they attend to each other and to `first`'s tokens (keys and
    /// values as `first` computed them). A cell of `first` listed again is computed anew and leaves the context;
    /// `first`'s other tokens keep their features (the attention only runs one way).
    pub fn extend_sparse(&self, grid: &PatchGrid, first: &SparsePass, cells: &[u32]) -> Result<SparsePass> {
        self.extend_sparse_gated(grid, first, cells, &mut || Ok(()))
    }

    /// [`VisionTower::extend_sparse`], 블록마다 `watch` 를 보며 ([`VisionTower::forward_sparse_cancellable`] 처럼)
    pub fn extend_sparse_cancellable(
        &self,
        grid: &PatchGrid,
        first: &SparsePass,
        cells: &[u32],
        watch: &EpochWatch<'_>,
    ) -> Result<SparsePass> {
        self.extend_sparse_gated(grid, first, cells, &mut || Ok(watch.check()?))
    }

    /// 블록에 들어가기 전마다 `gate` 를 부르는 [`VisionTower::extend_sparse`]
    pub(crate) fn extend_sparse_gated(
        &self,
        grid: &PatchGrid,
        first: &SparsePass,
        cells: &[u32],
        gate: &mut dyn FnMut() -> Result<()>,
    ) -> Result<SparsePass> {
        let again: std::collections::HashSet<u32> = cells.iter().copied().collect();
        let keep: Vec<u32> = first
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| !again.contains(c))
            .map(|(i, _)| i as u32)
            .collect();
        let context: Option<Vec<LayerKv>> = if keep.len() == first.cells.len() {
            None
        } else if keep.is_empty() {
            Some(Vec::new())
        } else {
            let rows = Tensor::from_slice(&keep, keep.len(), &self.device)?;
            Some(first.kv.iter().map(|kv| kv.select(&rows)).collect::<candle_core::Result<_>>()?)
        };
        let mut x = self.cell_tokens(grid, cells)?;
        let mut kv = Vec::with_capacity(self.blocks.len());
        for (i, b) in self.blocks.iter().enumerate() {
            gate()?;
            let ctx = match &context {
                None => Some(&first.kv[i]),
                Some(v) => v.get(i),
            };
            let (y, layer_kv) = b.forward_kv(&x, ctx)?;
            x = y;
            kv.push(layer_kv);
        }
        Ok(SparsePass {
            cells: cells.to_vec(),
            tokens: self.norm.forward(&x)?,
            kv,
        })
    }

    /// `first` with `second` (an `extend_sparse` of it) folded in, so a further opening attends to every cell so far.
    pub fn merge_sparse(&self, first: &SparsePass, second: &SparsePass) -> Result<SparsePass> {
        let again: std::collections::HashSet<u32> = second.cells.iter().copied().collect();
        let keep: Vec<u32> = first
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| !again.contains(c))
            .map(|(i, _)| i as u32)
            .collect();
        let mut cells: Vec<u32> = keep.iter().map(|&i| first.cells[i as usize]).collect();
        cells.extend_from_slice(&second.cells);
        if keep.is_empty() {
            return Ok(SparsePass {
                cells,
                tokens: second.tokens.clone(),
                kv: second.kv.clone(),
            });
        }
        let rows = Tensor::from_slice(&keep, keep.len(), &self.device)?;
        let tokens = Tensor::cat(&[&first.tokens.index_select(&rows, 1)?, &second.tokens], 1)?;
        let kv = first
            .kv
            .iter()
            .zip(&second.kv)
            .map(|(a, b)| {
                let a = a.select(&rows)?;
                Ok(LayerKv {
                    k: Tensor::cat(&[&a.k, &b.k], 2)?,
                    v: Tensor::cat(&[&a.v, &b.v], 2)?,
                })
            })
            .collect::<candle_core::Result<_>>()?;
        Ok(SparsePass { cells, tokens, kv })
    }

    /// The MAP head's pooled embedding of a pass's tokens (unit length).
    pub fn pooled(&self, pass: &SparsePass) -> Result<Vec<f32>> {
        let v: Vec<f32> = self.head.pooled(&pass.tokens)?.to_dtype(DType::F32)?.flatten_all()?.to_vec1()?;
        Ok(unit(v))
    }

    /// Per-cell unit features of a pass's tokens, in `pass.cells` order.
    pub fn dense(&self, pass: &SparsePass) -> Result<Vec<Vec<f32>>> {
        let flat: Vec<f32> = self.head.dense(&pass.tokens)?.to_dtype(DType::F32)?.squeeze(0)?.flatten_all()?.to_vec1()?;
        let d = self.cfg.hidden;
        Ok(flat.chunks_exact(d).map(|row| unit(row.to_vec())).collect())
    }

    /// Single-pass pooled embedding (unit) and per-cell unit dense features (sharing one `MapHead::kv` projection).
    pub fn pooled_and_dense(&self, pass: &SparsePass) -> Result<(Vec<f32>, Vec<Vec<f32>>)> {
        let (pooled_t, dense_t) = self.head.pooled_and_dense(&pass.tokens)?;
        let pooled_v: Vec<f32> = pooled_t.to_dtype(DType::F32)?.flatten_all()?.to_vec1()?;
        let dense_flat: Vec<f32> = dense_t.to_dtype(DType::F32)?.squeeze(0)?.flatten_all()?.to_vec1()?;
        let d = self.cfg.hidden;
        let dense = dense_flat.chunks_exact(d).map(|row| unit(row.to_vec())).collect();
        Ok((unit(pooled_v), dense))
    }
}

pub fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = premium_discount_cpu_kernels::dot_f32_simd(&v, &v).sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    premium_discount_cpu_kernels::dot_f32_simd(a, b)
}

/// 시험용 작은 탑 (체크포인트 없이, CPU).
#[cfg(test)]
pub(crate) mod toy {
    use super::*;

    /// 숨은 차원 `hidden`(짝수), 블록 `layers` 개. 블록 · MAP 헤드의 가중치는 0 이고 값 경로만 항등이라 아무것도 섞지
    /// 않는다: 칸 하나의 토큰 = LayerNorm(패치의 앞 `hidden` 값), 풀링 = 토큰 평균, 칸 특징 = 그 칸의 토큰.
    /// 그래서 시험이 패치 값으로 칸 특징을 정할 수 있다.
    pub(crate) fn tower(hidden: usize, layers: usize) -> VisionTower {
        let dev = Device::Cpu;
        let (d, m, heads) = (hidden, hidden, 2);
        let zeros = |r: usize, c: usize| Tensor::zeros((r, c), DType::F32, &dev).unwrap();
        // `rows × cols` 에서 (offset + k, k) 칸만 1
        let eye = |rows: usize, cols: usize, offset: usize| {
            let mut v = vec![0f32; rows * cols];
            for k in 0..cols.min(rows - offset) {
                v[(offset + k) * cols + k] = 1.0;
            }
            Tensor::from_vec(v, (rows, cols), &dev).unwrap()
        };
        let proj = |w: Tensor| {
            let out = w.dims()[0];
            Proj {
                inner: QMatMul::Tensor(w),
                bias: Some(Tensor::zeros(out, DType::F32, &dev).unwrap()),
            }
        };
        let norm = || LayerNorm::new(Tensor::ones(d, DType::F32, &dev).unwrap(), Tensor::zeros(d, DType::F32, &dev).unwrap(), 1e-6);
        let blocks = (0..layers)
            .map(|_| Block {
                norm1: norm(),
                qkv: proj(zeros(3 * d, d)),
                proj: proj(zeros(d, d)),
                norm2: norm(),
                fc1: proj(zeros(m, d)),
                fc2: proj(zeros(d, m)),
                heads,
                hd: d / heads,
            })
            .collect();
        // 패치 투영: 패치의 앞 d 값을 그대로
        let mut patch = vec![0f32; d * PatchGrid::DIM];
        for k in 0..d {
            patch[k * PatchGrid::DIM + k] = 1.0;
        }
        VisionTower {
            layout: Layout::RaonVe,
            patch: proj(Tensor::from_vec(patch, (d, PatchGrid::DIM), &dev).unwrap()),
            pos: zeros(4, d),
            blocks,
            norm: norm(),
            head: MapHead {
                q_latent: Tensor::zeros((1, heads, 1, d / heads), DType::F32, &dev).unwrap(),
                kv: proj(eye(2 * d, d, d)),
                proj: proj(eye(d, d, 0)),
                norm: norm(),
                fc1: proj(zeros(m, d)),
                fc2: proj(zeros(d, m)),
                heads,
                hd: d / heads,
            },
            cfg: VisionConfig {
                hidden: d,
                heads,
                layers,
                mlp: m,
                patch: 16,
                side: 2,
                eps: 1e-6,
            },
            format: WeightFormat::F32,
            device: dev.clone(),
            pos_cache: Mutex::new(HashMap::new()),
        }
    }

    /// `grid_h × grid_w` 칸, 칸 `c` 의 패치 앞 값은 `class(c)` 번째만 1 인 원-핫 (나머지 패치 값 0)
    pub(crate) fn grid(grid_h: usize, grid_w: usize, class: impl Fn(usize) -> usize) -> PatchGrid {
        let mut patches = vec![0f32; grid_h * grid_w * PatchGrid::DIM];
        for c in 0..grid_h * grid_w {
            patches[c * PatchGrid::DIM + class(c)] = 1.0;
        }
        PatchGrid { grid_h, grid_w, patches }
    }

    /// [`tower`] 이 원-핫 `k` 칸에 주는 특징 (단위 벡터): LayerNorm 이 평균을 뺀 `e_k − 1/d`
    pub(crate) fn feature(hidden: usize, k: usize) -> Vec<f32> {
        unit((0..hidden).map(|i| (if i == k { 1.0 } else { 0.0 }) - 1.0 / hidden as f32).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilinear_weights_match_pytorch_align_corners_false() {
        // Upsampling 2 → 4: [1, .75/.25, .25/.75, 1] (edges clamp).
        let w = bilinear_weights(2, 4);
        assert_eq!(w, vec![1.0, 0.0, 0.75, 0.25, 0.25, 0.75, 0.0, 1.0]);
        // Same size is the identity; every row sums to one.
        let w = bilinear_weights(16, 16);
        assert!((0..16).all(|i| w[i * 16 + i] == 1.0));
        let w = bilinear_weights(16, 9);
        assert!(w.chunks(16).all(|r| (r.iter().sum::<f32>() - 1.0).abs() < 1e-6));
    }

    #[test]
    fn the_toy_tower_gives_each_cell_its_own_feature() {
        let t = toy::tower(8, 3);
        let g = toy::grid(2, 3, |c| c % 4);
        let full = t.forward_full(&g).unwrap();
        let dense = t.dense(&full).unwrap();
        for (c, v) in full.cells.iter().zip(&dense) {
            assert!(cosine(v, &toy::feature(8, *c as usize % 4)) > 0.999, "cell {c}");
        }
        // 세대가 그대로면 감시판도 같은 답을 낸다
        let epoch = AtomicU64::new(7);
        let watched = t.forward_sparse_cancellable(&g, &full.cells, &EpochWatch::capture(&epoch)).unwrap();
        assert_eq!(t.pooled(&watched).unwrap(), t.pooled(&full).unwrap());
    }

    #[test]
    fn ps07_epoch_bump_stops_the_tower_at_the_next_block() {
        let t = toy::tower(8, 6);
        let g = toy::grid(3, 4, |c| c % 8);
        let cells: Vec<u32> = (0..12).collect();
        let epoch = AtomicU64::new(1);
        let watch = EpochWatch::capture(&epoch);
        // 블록 2 에 들어가기 직전 개인 화면 잠금이 세대를 1 → 2 로 올린다
        let mut calls = 0;
        let r = t.forward_sparse_gated(&g, &cells, &mut || {
            calls += 1;
            if calls == 3 {
                epoch.fetch_add(1, Ordering::AcqRel);
            }
            Ok(watch.check()?)
        });
        let err = r.err().expect("멈춘다");
        assert_eq!(VisionAbort::of(&err), Some(VisionAbort::PrivacyLocked));
        assert_eq!(calls, 3, "남은 블록 4개는 돌지 않는다 (6블록 끝까지 기다리지 않음)");
        // 이미 바뀐 세대로는 첫 블록 전에 멈춘다 (확장 패스도)
        assert!(VisionAbort::of(&t.forward_sparse_cancellable(&g, &cells, &watch).err().unwrap()).is_some());
        let first = t.forward_sparse(&g, &cells[..6]).unwrap();
        assert!(VisionAbort::of(&t.extend_sparse_cancellable(&g, &first, &cells[6..], &watch).err().unwrap()).is_some());
        // 다른 오류는 중단이 아니다
        assert_eq!(VisionAbort::of(&anyhow::anyhow!("other")), None);
    }

    #[test]
    fn gpu_flash_attn_guard_and_attend_parity() {
        assert!(!gpu_flash_attn_ok(&Device::Cpu, 72));
        assert!(!gpu_flash_attn_ok(&Device::Cpu, 0));
        assert!(!gpu_flash_attn_ok(&Device::Cpu, 512));

        let dev = {
            #[cfg(feature = "cuda")]
            {
                Device::new_cuda(0).unwrap_or(Device::Cpu)
            }
            #[cfg(all(not(feature = "cuda"), feature = "rocm"))]
            {
                Device::new_rocm(0).unwrap_or(Device::Cpu)
            }
            #[cfg(all(not(feature = "cuda"), not(feature = "rocm"), feature = "vulkan"))]
            {
                Device::new_vulkan(0).unwrap_or(Device::Cpu)
            }
            #[cfg(all(not(feature = "cuda"), not(feature = "rocm"), not(feature = "vulkan"), feature = "metal"))]
            {
                Device::new_metal(0).unwrap_or(Device::Cpu)
            }
            #[cfg(all(
                not(feature = "cuda"),
                not(feature = "rocm"),
                not(feature = "vulkan"),
                not(feature = "metal")
            ))]
            {
                Device::Cpu
            }
        };
        if dev.is_cpu() {
            return;
        }
        assert!(!gpu_flash_attn_ok(&dev, 512), "hd > 256 must be rejected");
        assert!(gpu_flash_attn_ok(&dev, 72), "hd = 72 must pass non-trivial flash-attn probe");
        let (bh, n, hd) = (4usize, 16usize, 72usize);
        let mk = |seed: u64| -> Vec<f32> {
            let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            (0..bh * n * hd)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    ((s >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
                })
                .collect()
        };
        let q = Tensor::from_vec(mk(1), (1, bh, n, hd), &Device::Cpu).unwrap();
        let k = Tensor::from_vec(mk(2), (1, bh, n, hd), &Device::Cpu).unwrap();
        let v = Tensor::from_vec(mk(3), (1, bh, n, hd), &Device::Cpu).unwrap();
        let want = attend(&q, &k, &v, hd).unwrap();
        let got = attend(&q.to_device(&dev).unwrap(), &k.to_device(&dev).unwrap(), &v.to_device(&dev).unwrap(), hd)
            .unwrap()
            .to_device(&Device::Cpu)
            .unwrap();
        let diff = (&want - &got).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap();
        assert!(diff < 1e-4, "GPU attend max diff {diff}");
        let _ = dev.synchronize_all();
        let _ = dev.trim_memory_pool(0);
    }
}
