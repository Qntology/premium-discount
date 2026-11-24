//! `ibm-granite/granite-embedding-97m-multilingual-r2` sentence embeddings on candle.
//!
//! The model is a 12-layer ModernBERT encoder (hidden 384, 12 heads of 32, GLU MLP of 1536, bias-free LayerNorm,
//! no linear biases) with a 180k-token multilingual byte-level BPE vocabulary. Its official sentence-transformers
//! pipeline (the Hub's `modules.json`: Transformer → Pooling with `pooling_mode_cls_token` → Normalize; `config.json`
//! also says `classifier_pooling: "cls"`) is the L2-normalised final hidden state of the first token, the
//! tokenizer's `<|startoftext|>` [CLS]. [`Granite`] computes exactly that in F32; against Hugging Face transformers
//! (float32, CPU) every reference sentence has cosine ≥ 0.9999 (`tests/granite_reference.rs`, fixture written by
//! `scripts/granite_reference.py`).
//!
//! # Forward pass
//!
//! A port of the `ForwardMode::Exact` path of Qntology/logis-center (`commerce/src-tauri/src/models/embedding.rs`),
//! which mirrors transformers' `ModernBertModel`:
//!
//! * embeddings: token embedding, then LayerNorm;
//! * each layer: `x += Wo(attention(attn_norm(x)))`, then `x += Wo(act(a) * b)` where `[a | b] = Wi(mlp_norm(x))`;
//!   layer 0 has no `attn_norm` (identity), as in transformers;
//! * attention: layers with `index % global_attn_every_n_layers == 0` are global and use RoPE with
//!   `global_rope_theta`; the others are local — a token sees only tokens at most `local_attention / 2` positions away
//!   — and use `local_rope_theta` (both tables are F32, like transformers);
//! * `final_norm`, then the [CLS] row, L2-normalised.
//!
//! # Truncation
//!
//! Input longer than [`MAX_TOKENS`] (512 tokens, [CLS] and [SEP] included) is cut from the right before [SEP] is
//! appended — the tokenizer's own right / longest-first truncation, i.e. sentence-transformers with
//! `max_seq_length = 512`. The checkpoint accepts far longer input (`max_position_embeddings` 32768), but attention
//! time and memory grow with the square of the length, Raon Zena embeds short spans, and 512 is logis-center's
//! forward length too (its Exact CPU vectors for inputs of at most 512 tokens are the same computation). Callers that
//! need a whole long document should split it themselves; [`Granite::token_count`] tells when truncation happens.
//!
//! # Batching
//!
//! [`Granite::embed_batch`] sorts the inputs by token count, packs neighbours into forward passes of at most
//! `BATCH_TOKENS` padded tokens, right-pads with the tokenizer's pad id and masks padded keys. Positions start at 0
//! in every row, so the real tokens see exactly what they see in a single-text pass and the vectors equal
//! [`Granite::embed`]'s up to float summation order.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, ensure, Context, Result};
use candle_core::{DType, Device, Module, Tensor, D};
use candle_nn::{Embedding, Linear, VarBuilder};
use serde::Deserialize;
use tokenizers::{Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy};

/// Embedding space tag stored next to every vector / SDS ledger made with this model.
pub const SPACE_TAG: &str = "granite-97m-r2-cls-v1";
/// Embedding dimension (the model's `hidden_size`).
pub const DIM: usize = 384;
/// Longest input the model sees, in tokens, [CLS] and [SEP] included; longer input is truncated from the right.
pub const MAX_TOKENS: usize = 512;
/// Most padded tokens (rows × longest row) in one batched forward pass. At 512 tokens that is 8 rows, whose
/// attention scores take 8 × 12 × 512 × 512 × 4 bytes = 100 MB.
const BATCH_TOKENS: usize = 4096;
/// Additive attention mask for keys a query must not see. Finite (transformers uses the F32 minimum) so that two
/// masks can be added and a padded query row whose whole window is padding stays finite; `exp` of it is exactly 0.
const MASKED: f32 = -1e30;

/// The `config.json` fields this forward pass reads. Defaults are transformers' `ModernBertConfig` defaults.
#[derive(Debug, Clone, Deserialize)]
struct Config {
    vocab_size: usize,
    hidden_size: usize,
    intermediate_size: usize,
    num_hidden_layers: usize,
    num_attention_heads: usize,
    #[serde(default = "Config::default_norm_eps")]
    norm_eps: f64,
    #[serde(default)]
    norm_bias: bool,
    #[serde(default)]
    attention_bias: bool,
    #[serde(default)]
    mlp_bias: bool,
    #[serde(default = "Config::default_activation")]
    hidden_activation: String,
    #[serde(default = "Config::default_global_rope_theta")]
    global_rope_theta: f64,
    #[serde(default = "Config::default_local_rope_theta")]
    local_rope_theta: f64,
    #[serde(default = "Config::default_local_attention")]
    local_attention: usize,
    #[serde(default = "Config::default_global_attn_every_n_layers")]
    global_attn_every_n_layers: usize,
    pad_token_id: Option<u32>,
}

impl Config {
    fn default_norm_eps() -> f64 {
        1e-5
    }
    fn default_activation() -> String {
        "gelu".into()
    }
    fn default_global_rope_theta() -> f64 {
        160_000.0
    }
    fn default_local_rope_theta() -> f64 {
        10_000.0
    }
    fn default_local_attention() -> usize {
        128
    }
    fn default_global_attn_every_n_layers() -> usize {
        3
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.hidden_size == DIM,
            "granite: hidden_size {} (expected {DIM})",
            self.hidden_size
        );
        ensure!(
            self.num_attention_heads > 0
                && self.hidden_size.is_multiple_of(self.num_attention_heads)
                && (self.hidden_size / self.num_attention_heads).is_multiple_of(2),
            "granite: hidden_size {} does not split into {} even-sized heads",
            self.hidden_size,
            self.num_attention_heads
        );
        ensure!(
            !self.norm_bias && !self.attention_bias && !self.mlp_bias,
            "granite: biased norms or linear layers are not supported (norm_bias {}, attention_bias {}, mlp_bias {})",
            self.norm_bias,
            self.attention_bias,
            self.mlp_bias
        );
        ensure!(self.global_attn_every_n_layers > 0, "granite: global_attn_every_n_layers is 0");
        Activation::parse(&self.hidden_activation)?;
        Ok(())
    }

    fn encoder_config(&self) -> Result<EncoderConfig> {
        Ok(EncoderConfig {
            vocab_size: self.vocab_size,
            hidden_size: self.hidden_size,
            intermediate_size: self.intermediate_size,
            num_hidden_layers: self.num_hidden_layers,
            num_attention_heads: self.num_attention_heads,
            norm_eps: self.norm_eps,
            activation: Activation::parse(&self.hidden_activation)?,
            global_rope_theta: self.global_rope_theta,
            local_rope_theta: self.local_rope_theta,
            global_attn_every_n_layers: self.global_attn_every_n_layers,
            local_window_half: self.local_attention / 2,
            max_positions: MAX_TOKENS,
        })
    }
}

/// The MLP gate activation (`hidden_activation`; granite uses `silu`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    Silu,
    /// transformers' `gelu`, the exact erf form.
    Gelu,
}

impl Activation {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "silu" | "swish" => Ok(Self::Silu),
            "gelu" => Ok(Self::Gelu),
            other => bail!("granite: unsupported hidden_activation {other:?}"),
        }
    }
}

/// The encoder's shape and attention layout, every value explicit. [`Granite::load`] fills it from `config.json` with
/// transformers' `ModernBertConfig` defaults; an app that reads `config.json` with defaults of its own (logis-center's
/// seeded mock model has no RoPE / window fields) builds it itself.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub norm_eps: f64,
    pub activation: Activation,
    /// RoPE θ of the global layers (`layer % global_attn_every_n_layers == 0`).
    pub global_rope_theta: f64,
    /// RoPE θ of the local (sliding-window) layers.
    pub local_rope_theta: f64,
    pub global_attn_every_n_layers: usize,
    /// How far a local layer sees: `local_attention / 2`; `usize::MAX / 2` for no window.
    pub local_window_half: usize,
    /// Positions in the RoPE tables (longest forward pass).
    pub max_positions: usize,
}

/// Replacement for the MLP's `silu(x[.., :I]) * x[.., I:]` on whole `Wi` outputs ([`set_glu_kernel`]).
pub type GluKernel = fn(&Tensor) -> candle_core::Result<Tensor>;

static GLU_KERNEL: std::sync::OnceLock<GluKernel> = std::sync::OnceLock::new();

/// Installs a faster implementation of the SiLU GLU (`silu(x[.., :I]) * x[.., I:]`). It must give the same bits as
/// candle's `silu(gate) * up` — logis-center installs its multi-core CPU kernel (`embed-cpu-kernels`, which falls back
/// to the candle ops off the CPU). Once per process; false when one is already installed.
pub fn set_glu_kernel(kernel: GluKernel) -> bool {
    GLU_KERNEL.set(kernel).is_ok()
}

/// `silu(x[.., :I]) * x[.., I:]` with candle ops (gate and up copied out first).
/// On CUDA / ROCm uses the single fused `silu_mul` kernel.
pub fn silu_glu(x: &Tensor) -> candle_core::Result<Tensor> {
    let i = x.dim(D::Minus1)? / 2;
    let gate = x.narrow(D::Minus1, 0, i)?.contiguous()?;
    let up = x.narrow(D::Minus1, i, i)?.contiguous()?;
    if x.device().is_cuda() || x.device().is_rocm() {
        if let Ok(y) = candle_core::decoder_ops::silu_mul(&gate, &up) {
            return Ok(y);
        }
    }
    (candle_nn::ops::silu(&gate)? * up)?.contiguous()
}

/// Softmax over the last axis. CPU F32 and CUDA / ROCm use candle's fused `softmax_last_dim` (F32 internal
/// accumulation on GPU avoids BF16 intermediate rounding and saves 4 kernel launches); other devices keep the
/// composed ops.
fn softmax_last(x: &Tensor) -> candle_core::Result<Tensor> {
    if x.device().is_cpu() && x.dtype() == DType::F32 && x.is_contiguous() {
        candle_nn::ops::softmax_last_dim(x)
    } else if x.device().is_cuda() || x.device().is_rocm() {
        let xc = if x.is_contiguous() { x.clone() } else { x.contiguous()? };
        candle_nn::ops::softmax_last_dim(&xc)
    } else {
        candle_nn::ops::softmax(x, D::Minus1)
    }
}

/// LayerNorm without bias. Mean and variance in F32 whatever the weights' dtype (two-pass, like PyTorch — not the
/// fused E[x²] − E[x]² kernel); the result goes back to the input dtype.
struct LayerNorm {
    weight_f32: Tensor,
    eps: f64,
}

impl LayerNorm {
    fn load(dim: usize, eps: f64, vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            weight_f32: vb.get(dim, "weight")?.to_dtype(DType::F32)?,
            eps,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let dtype = x.dtype();
        let x = x.to_dtype(DType::F32)?;
        let mean = x.mean_keepdim(D::Minus1)?;
        let centred = x.broadcast_sub(&mean)?;
        let var = centred.sqr()?.mean_keepdim(D::Minus1)?;
        let y = centred.broadcast_div(&(var + self.eps)?.sqrt()?)?;
        y.broadcast_mul(&self.weight_f32)?.to_dtype(dtype)
    }
}

/// F32 RoPE tables `[max_positions, head_dim / 2]` (rotate-half layout) plus pre-concatenated
/// `[1, 1, max_positions, head_dim]` views so `cos_sin(len)` is a zero-copy narrow.
struct Rope {
    #[allow(dead_code)]
    cos: Tensor,
    #[allow(dead_code)]
    sin: Tensor,
    cos_full: Tensor,
    sin_full: Tensor,
}

impl Rope {
    fn new(head_dim: usize, max_positions: usize, theta: f64, device: &Device) -> candle_core::Result<Self> {
        let inv_freq: Vec<f32> = (0..head_dim)
            .step_by(2)
            .map(|i| 1f32 / (theta.powf(i as f64 / head_dim as f64) as f32))
            .collect();
        let inv_freq = Tensor::new(inv_freq.as_slice(), device)?;
        let t = Tensor::arange(0u32, max_positions as u32, device)?.to_dtype(DType::F32)?.unsqueeze(1)?;
        let freqs = t.matmul(&inv_freq.unsqueeze(0)?)?;
        let cos = freqs.cos()?;
        let sin = freqs.sin()?;
        let cos_full = Tensor::cat(&[&cos, &cos], D::Minus1)?
            .unsqueeze(0)?
            .unsqueeze(0)?
            .contiguous()?;
        let sin_full = Tensor::cat(&[&sin, &sin], D::Minus1)?
            .unsqueeze(0)?
            .unsqueeze(0)?
            .contiguous()?;
        Ok(Self {
            cos,
            sin,
            cos_full,
            sin_full,
        })
    }

    /// `[1, 1, len, head_dim]` (cos, sin) and `[len, head_dim / 2]` half-tables, zero-copy slices of the prebuilt table.
    fn cos_sin(&self, len: usize) -> candle_core::Result<(Tensor, Tensor)> {
        Ok((
            self.cos_full.narrow(2, 0, len)?,
            self.sin_full.narrow(2, 0, len)?,
        ))
    }

    fn cos_sin_half(&self, len: usize) -> candle_core::Result<(Tensor, Tensor)> {
        Ok((self.cos.narrow(0, 0, len)?, self.sin.narrow(0, 0, len)?))
    }

    /// `x · cos + rotate_half(x) · sin`, in the tables' dtype (F32), back to `x`'s dtype.
    fn apply(x: &Tensor, cos: &Tensor, sin: &Tensor) -> candle_core::Result<Tensor> {
        if x.dtype() != cos.dtype() {
            return Self::apply(&x.to_dtype(cos.dtype())?, cos, sin)?.to_dtype(x.dtype());
        }
        let half = x.dim(D::Minus1)? / 2;
        let x1 = x.narrow(D::Minus1, 0, half)?.contiguous()?;
        let x2 = x.narrow(D::Minus1, half, half)?.contiguous()?;
        let rotated = Tensor::cat(&[&x2.neg()?, &x1], D::Minus1)?;
        x.contiguous()?.broadcast_mul(cos)? + rotated.broadcast_mul(sin)?
    }

    /// Uses fused `candle_nn::rotary_emb::rope` on CUDA / ROCm (single kernel in F32), falling back to [`Self::apply`].
    fn apply_fast(
        x: &Tensor,
        cos_half: &Tensor,
        sin_half: &Tensor,
        cos_full: &Tensor,
        sin_full: &Tensor,
    ) -> candle_core::Result<Tensor> {
        if x.device().is_cuda() || x.device().is_rocm() {
            let orig_dtype = x.dtype();
            let xf = if orig_dtype != cos_half.dtype() {
                x.to_dtype(cos_half.dtype())?.contiguous()?
            } else {
                x.contiguous()?
            };
            if let Ok(y) = candle_nn::rotary_emb::rope(&xf, &cos_half.contiguous()?, &sin_half.contiguous()?) {
                return if orig_dtype != y.dtype() {
                    y.to_dtype(orig_dtype)
                } else {
                    Ok(y)
                };
            }
        }
        Self::apply(x, cos_full, sin_full)
    }
}

/// Additive sliding-window mask `[1, 1, len, len]` in `dtype`: 0 where `|i − j| ≤ half`, [`MASKED`] elsewhere.
/// `None` when the window covers the whole sequence.
fn window_mask(len: usize, half: usize, device: &Device, dtype: DType) -> candle_core::Result<Option<Tensor>> {
    if len <= half.saturating_add(1) {
        return Ok(None);
    }
    let mask: Vec<f32> = (0..len)
        .flat_map(|i| (0..len).map(move |j| if i.abs_diff(j) > half { MASKED } else { 0.0 }))
        .collect();
    Tensor::from_vec(mask, (1, 1, len, len), device)?.to_dtype(dtype).map(Some)
}

struct Attention {
    wqkv: Linear,
    wo: Linear,
    heads: usize,
    head_dim: usize,
}

impl Attention {
    fn load(cfg: &EncoderConfig, vb: VarBuilder) -> candle_core::Result<Self> {
        let h = cfg.hidden_size;
        Ok(Self {
            wqkv: candle_nn::linear_no_bias(h, 3 * h, vb.pp("Wqkv"))?,
            wo: candle_nn::linear_no_bias(h, h, vb.pp("Wo"))?,
            heads: cfg.num_attention_heads,
            head_dim: h / cfg.num_attention_heads,
        })
    }

    /// `x` is `[batch, len, hidden]`; `mask` is additive and broadcasts to `[batch, heads, len, len]`.
    fn forward(
        &self,
        x: &Tensor,
        cos_half: &Tensor,
        sin_half: &Tensor,
        cos: &Tensor,
        sin: &Tensor,
        mask: Option<&Tensor>,
    ) -> candle_core::Result<Tensor> {
        let (b, len, _) = x.dims3()?;
        // [b, len, 3H] seen as [b, len, 3, heads, hd] (no copy); q / k / v each copied once to [b, heads, len, hd]
        let qkv = self.wqkv.forward(x)?.reshape((b, len, 3, self.heads, self.head_dim))?;
        let part = |i: usize| -> candle_core::Result<Tensor> { qkv.narrow(2, i, 1)?.squeeze(2)?.transpose(1, 2)?.contiguous() };
        let (q, k, v) = (part(0)?, part(1)?, part(2)?);
        let q = Rope::apply_fast(&q, cos_half, sin_half, cos, sin)?.contiguous()?;
        let k = Rope::apply_fast(&k, cos_half, sin_half, cos, sin)?.contiguous()?;
        if mask.is_none()
            && self.head_dim <= 256
            && (q.device().is_cuda() || q.device().is_rocm())
        {
            let cfg = candle_nn::flash_attn::FlashAttnConfig::new(1.0 / (self.head_dim as f32).sqrt());
            if let Ok(out) = candle_nn::flash_attn::flash_attn_bhsd(&q, &k, &v, &cfg) {
                let y = out
                    .transpose(1, 2)?
                    .contiguous()?
                    .reshape((b, len, self.heads * self.head_dim))?;
                return self.wo.forward(&y);
            }
        }
        let kt = k.transpose(2, 3)?.contiguous()?;
        let scores = (q.matmul(&kt)? / (self.head_dim as f64).sqrt())?;
        let scores = match mask {
            Some(m) => scores.broadcast_add(m)?,
            None => scores,
        };
        let probs = softmax_last(&scores)?;
        let y = probs
            .contiguous()?
            .matmul(&v.contiguous()?)?
            .transpose(1, 2)?
            .contiguous()?
            .reshape((b, len, self.heads * self.head_dim))?;
        self.wo.forward(&y)
    }
}

struct Mlp {
    wi: Linear,
    wo: Linear,
    act: Activation,
}

impl Mlp {
    fn load(cfg: &EncoderConfig, vb: VarBuilder) -> candle_core::Result<Self> {
        let (h, i) = (cfg.hidden_size, cfg.intermediate_size);
        Ok(Self {
            wi: candle_nn::linear_no_bias(h, 2 * i, vb.pp("Wi"))?,
            wo: candle_nn::linear_no_bias(i, h, vb.pp("Wo"))?,
            act: cfg.activation,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let wi = self.wi.forward(x)?;
        // transformers: `input, gate = Wi(x).chunk(2)`; `Wo(act(input) * gate)`
        // CPU-only custom GLU kernel; GPU uses fused `silu_glu` (`decoder_ops::silu_mul`).
        let glu = match self.act {
            Activation::Silu => {
                if wi.device().is_cpu() {
                    match GLU_KERNEL.get() {
                        Some(kernel) => kernel(&wi)?,
                        None => silu_glu(&wi)?,
                    }
                } else {
                    silu_glu(&wi)?
                }
            }
            Activation::Gelu => {
                let i = wi.dim(D::Minus1)? / 2;
                let input = wi.narrow(D::Minus1, 0, i)?.contiguous()?;
                let gate = wi.narrow(D::Minus1, i, i)?.contiguous()?;
                (input.gelu_erf()? * gate)?.contiguous()?
            }
        };
        self.wo.forward(&glu)
    }
}

struct Layer {
    /// `None` for layer 0 (transformers uses `nn.Identity` there).
    attn_norm: Option<LayerNorm>,
    attn: Attention,
    mlp_norm: LayerNorm,
    mlp: Mlp,
    global: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MappedDType {
    BF16,
    F16,
    F32,
}

impl MappedDType {
    fn elem_bytes(self) -> usize {
        match self {
            Self::BF16 | Self::F16 => 2,
            Self::F32 => 4,
        }
    }
}

/// Memory-mapped token embedding table that reads only referenced rows per forward pass.
/// Keeps GPU VRAM / host heap resident footprint of the 180,000 × 384 table at 0 bytes while
/// staying bit-identical (E0) to dense loading on every backend.
pub struct MappedTable {
    map: Arc<memmap2::Mmap>,
    offset: usize,
    rows: usize,
    cols: usize,
    src_dtype: MappedDType,
    target_dtype: DType,
}

impl MappedTable {
    pub fn from_safetensors(
        path: &Path,
        tensor_name: &str,
        target_dtype: DType,
    ) -> candle_core::Result<Self> {
        let file = std::fs::File::open(path).map_err(|e| {
            candle_core::Error::Msg(format!("granite MappedTable open {}: {e}", path.display()))
        })?;
        // SAFETY: read-only memory map of an immutable model safetensors file.
        let map = Arc::new(unsafe { memmap2::Mmap::map(&file) }.map_err(|e| {
            candle_core::Error::Msg(format!("granite MappedTable mmap {}: {e}", path.display()))
        })?);
        if map.len() < 8 {
            candle_core::bail!("granite MappedTable: {} too small", path.display());
        }
        let header_len = u64::from_le_bytes(map[..8].try_into().unwrap()) as usize;
        if header_len > 100 << 20 || 8 + header_len > map.len() {
            candle_core::bail!(
                "granite MappedTable: invalid header length {header_len} in {}",
                path.display()
            );
        }
        let header: serde_json::Value =
            serde_json::from_slice(&map[8..8 + header_len]).map_err(|e| {
                candle_core::Error::Msg(format!(
                    "granite MappedTable parse header {}: {e}",
                    path.display()
                ))
            })?;
        let entry = header.get(tensor_name).ok_or_else(|| {
            candle_core::Error::Msg(format!(
                "granite MappedTable: missing tensor '{tensor_name}' in {}",
                path.display()
            ))
        })?;
        let src_dtype = match entry.get("dtype").and_then(|v| v.as_str()) {
            Some("BF16") => MappedDType::BF16,
            Some("F16") => MappedDType::F16,
            Some("F32") => MappedDType::F32,
            other => candle_core::bail!(
                "granite MappedTable: unsupported dtype {other:?} for '{tensor_name}'"
            ),
        };
        let shape = entry
            .get("shape")
            .and_then(|v| v.as_array())
            .ok_or_else(|| candle_core::Error::Msg("granite MappedTable: missing shape".into()))?;
        if shape.len() != 2 {
            candle_core::bail!(
                "granite MappedTable: expected 2D tensor for '{tensor_name}', got {}D",
                shape.len()
            );
        }
        let rows = shape[0].as_u64().unwrap_or(0) as usize;
        let cols = shape[1].as_u64().unwrap_or(0) as usize;
        let offsets = entry
            .get("data_offsets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                candle_core::Error::Msg("granite MappedTable: missing data_offsets".into())
            })?;
        let begin = offsets[0].as_u64().unwrap_or(0) as usize;
        let end = offsets[1].as_u64().unwrap_or(0) as usize;
        let expected_bytes = rows
            .checked_mul(cols)
            .and_then(|n| n.checked_mul(src_dtype.elem_bytes()))
            .ok_or_else(|| candle_core::Error::Msg("granite MappedTable: byte size overflow".into()))?;
        let data_start = 8 + header_len;
        if end.saturating_sub(begin) != expected_bytes || data_start + end > map.len() {
            candle_core::bail!(
                "granite MappedTable: byte range [{begin}, {end}) mismatch for {rows}x{cols} ({expected_bytes} B)"
            );
        }
        Ok(Self {
            map,
            offset: data_start + begin,
            rows,
            cols,
            src_dtype,
            target_dtype,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn mapped_bytes(&self) -> usize {
        self.rows * self.cols * self.src_dtype.elem_bytes()
    }

    /// Gather rows for `ids` onto `dev` with bit-identical conversion to `self.target_dtype`.
    pub fn gather(&self, ids: &Tensor, dev: &Device) -> candle_core::Result<Tensor> {
        let flat = ids.to_device(&Device::Cpu)?.flatten_all()?.to_vec1::<u32>()?;
        if flat.is_empty() {
            return Tensor::zeros((0, self.cols), self.target_dtype, dev);
        }
        let mut uniq = flat.clone();
        uniq.sort_unstable();
        uniq.dedup();
        if let Some(&max_id) = uniq.last() {
            if (max_id as usize) >= self.rows {
                candle_core::bail!(
                    "granite MappedTable: token id {max_id} out of range (rows={})",
                    self.rows
                );
            }
        }
        let row_bytes = self.cols * self.src_dtype.elem_bytes();
        let sub = match self.src_dtype {
            MappedDType::BF16 => {
                if self.target_dtype == DType::F32 {
                    let mut buf = Vec::with_capacity(uniq.len() * self.cols);
                    for &id in &uniq {
                        let s = self.offset + (id as usize) * row_bytes;
                        buf.extend(
                            self.map[s..s + row_bytes]
                                .chunks_exact(2)
                                .map(|b| half::bf16::from_le_bytes([b[0], b[1]]).to_f32()),
                        );
                    }
                    Tensor::from_vec(buf, (uniq.len(), self.cols), dev)?
                } else {
                    let mut buf = Vec::with_capacity(uniq.len() * self.cols);
                    for &id in &uniq {
                        let s = self.offset + (id as usize) * row_bytes;
                        buf.extend(
                            self.map[s..s + row_bytes]
                                .chunks_exact(2)
                                .map(|b| half::bf16::from_le_bytes([b[0], b[1]])),
                        );
                    }
                    let t = Tensor::from_vec(buf, (uniq.len(), self.cols), dev)?;
                    if self.target_dtype == DType::BF16 {
                        t
                    } else {
                        t.to_dtype(self.target_dtype)?
                    }
                }
            }
            MappedDType::F16 => {
                let mut buf = Vec::with_capacity(uniq.len() * self.cols);
                for &id in &uniq {
                    let s = self.offset + (id as usize) * row_bytes;
                    buf.extend(
                        self.map[s..s + row_bytes]
                            .chunks_exact(2)
                            .map(|b| half::f16::from_le_bytes([b[0], b[1]])),
                    );
                }
                let t = Tensor::from_vec(buf, (uniq.len(), self.cols), dev)?;
                if self.target_dtype == DType::F16 {
                    t
                } else {
                    t.to_dtype(self.target_dtype)?
                }
            }
            MappedDType::F32 => {
                let mut buf = Vec::with_capacity(uniq.len() * self.cols);
                for &id in &uniq {
                    let s = self.offset + (id as usize) * row_bytes;
                    buf.extend(
                        self.map[s..s + row_bytes]
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    );
                }
                let t = Tensor::from_vec(buf, (uniq.len(), self.cols), dev)?;
                if self.target_dtype == DType::F32 {
                    t
                } else {
                    t.to_dtype(self.target_dtype)?
                }
            }
        };
        let idx: Vec<u32> = flat
            .iter()
            .map(|id| uniq.binary_search(id).unwrap() as u32)
            .collect();
        let idx_t = Tensor::from_vec(idx, flat.len(), dev)?;
        sub.index_select(&idx_t, 0)
    }
}

enum TokEmbeddings {
    Dense(Embedding),
    Mapped { table: MappedTable, device: Device },
}

impl TokEmbeddings {
    fn forward(&self, ids: &Tensor) -> candle_core::Result<Tensor> {
        match self {
            Self::Dense(emb) => emb.forward(ids),
            Self::Mapped { table, device } => table.gather(ids, device),
        }
    }

    fn is_mapped(&self) -> bool {
        matches!(self, Self::Mapped { .. })
    }

    fn mapped_bytes(&self) -> usize {
        match self {
            Self::Dense(_) => 0,
            Self::Mapped { table, .. } => table.mapped_bytes(),
        }
    }
}

/// The ModernBERT encoder of granite-embedding-97m-multilingual-r2 — the one forward pass premium-discount, Raon Zena
/// and logis-center share (logis-center's `ForwardMode::Exact`, op for op, so its vectors, text caches and per-text /
/// batch bit identity stay as they were).
///
/// Weights come in the [`VarBuilder`]'s dtype (F32 on CPU; logis-center uses BF16 on CUDA / ROCm); LayerNorm and RoPE run
/// in F32 either way.
pub struct Encoder {
    tok_embeddings: TokEmbeddings,
    embeddings_norm: LayerNorm,
    layers: Vec<Layer>,
    final_norm: LayerNorm,
    global_rope: Rope,
    local_rope: Rope,
    window_half: usize,
    hidden_size: usize,
    window_cache: Mutex<HashMap<(usize, DType), Tensor>>,
}

impl Encoder {
    pub fn load(cfg: &EncoderConfig, vb: VarBuilder, device: &Device) -> candle_core::Result<Self> {
        let tok_embeddings = TokEmbeddings::Dense(candle_nn::embedding(
            cfg.vocab_size,
            cfg.hidden_size,
            vb.pp("embeddings.tok_embeddings"),
        )?);
        Self::load_with_tok_embeddings(cfg, vb, device, tok_embeddings)
    }

    /// Loads the encoder while keeping `embeddings.tok_embeddings.weight` memory-mapped on host disk
    /// (0 bytes resident in GPU VRAM / host heap, bit-identical `E0` to [`Encoder::load`]).
    pub fn load_mapped(
        cfg: &EncoderConfig,
        vb: VarBuilder,
        safetensors_path: &Path,
        device: &Device,
    ) -> candle_core::Result<Self> {
        let table = MappedTable::from_safetensors(
            safetensors_path,
            "embeddings.tok_embeddings.weight",
            vb.dtype(),
        )?;
        if table.rows() != cfg.vocab_size || table.cols() != cfg.hidden_size {
            candle_core::bail!(
                "granite MappedTable shape {}x{} does not match config {}x{}",
                table.rows(),
                table.cols(),
                cfg.vocab_size,
                cfg.hidden_size
            );
        }
        let tok_embeddings = TokEmbeddings::Mapped {
            table,
            device: device.clone(),
        };
        Self::load_with_tok_embeddings(cfg, vb, device, tok_embeddings)
    }

    fn load_with_tok_embeddings(
        cfg: &EncoderConfig,
        vb: VarBuilder,
        device: &Device,
        tok_embeddings: TokEmbeddings,
    ) -> candle_core::Result<Self> {
        let (h, eps) = (cfg.hidden_size, cfg.norm_eps);
        let head_dim = h / cfg.num_attention_heads;
        let every_n = cfg.global_attn_every_n_layers.max(1);
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        for i in 0..cfg.num_hidden_layers {
            let vb = vb.pp(format!("layers.{i}"));
            layers.push(Layer {
                attn_norm: if i == 0 {
                    None
                } else {
                    Some(LayerNorm::load(h, eps, vb.pp("attn_norm"))?)
                },
                attn: Attention::load(cfg, vb.pp("attn"))?,
                mlp_norm: LayerNorm::load(h, eps, vb.pp("mlp_norm"))?,
                mlp: Mlp::load(cfg, vb.pp("mlp"))?,
                global: i % every_n == 0,
            });
        }
        Ok(Self {
            tok_embeddings,
            embeddings_norm: LayerNorm::load(h, eps, vb.pp("embeddings.norm"))?,
            layers,
            final_norm: LayerNorm::load(h, eps, vb.pp("final_norm"))?,
            global_rope: Rope::new(head_dim, cfg.max_positions, cfg.global_rope_theta, device)?,
            local_rope: Rope::new(head_dim, cfg.max_positions, cfg.local_rope_theta, device)?,
            window_half: cfg.local_window_half,
            hidden_size: h,
            window_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn is_mapped(&self) -> bool {
        self.tok_embeddings.is_mapped()
    }

    pub fn mapped_table_bytes(&self) -> usize {
        self.tok_embeddings.mapped_bytes()
    }

    pub fn hidden_size(&self) -> usize {
        self.hidden_size
    }

    fn cached_window_mask(&self, len: usize, device: &Device, dtype: DType) -> candle_core::Result<Option<Tensor>> {
        if len <= self.window_half.saturating_add(1) {
            return Ok(None);
        }
        if let Some(m) = self.window_cache.lock().unwrap_or_else(|e| e.into_inner()).get(&(len, dtype)) {
            return Ok(Some(m.clone()));
        }
        let Some(m) = window_mask(len, self.window_half, device, dtype)? else {
            return Ok(None);
        };
        let mut cache = self.window_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert((len, dtype), m.clone());
        Ok(Some(m))
    }

    /// Final-normed hidden states `[batch, len, hidden]` for `ids` = `[batch · len]` (row-major) or `[batch, len]`.
    /// `mask` is additive (`[batch, 1, 1, len]`: 0 for real keys, [`MASKED`] or −∞ for padding); `None` = no padding.
    /// One row without a mask is exactly the single-text pass.
    pub fn forward_batch(&self, ids: &Tensor, batch: usize, len: usize, mask: Option<&Tensor>) -> candle_core::Result<Tensor> {
        let x = self.tok_embeddings.forward(ids)?.reshape((batch, len, ()))?;
        let mut x = self.embeddings_norm.forward(&x)?;
        let (cg, sg) = self.global_rope.cos_sin(len)?;
        let (cgh, sgh) = self.global_rope.cos_sin_half(len)?;
        let (cl, sl) = self.local_rope.cos_sin(len)?;
        let (clh, slh) = self.local_rope.cos_sin_half(len)?;
        let local_mask = match (mask, self.cached_window_mask(len, x.device(), x.dtype())?) {
            (Some(m), Some(w)) => Some(m.broadcast_add(&w)?),
            (None, w) => w,
            (m, None) => m.cloned(),
        };
        for layer in &self.layers {
            let (ch, sh, c, s, m) = if layer.global {
                (&cgh, &sgh, &cg, &sg, mask)
            } else {
                (&clh, &slh, &cl, &sl, local_mask.as_ref())
            };
            let h = match &layer.attn_norm {
                Some(norm) => norm.forward(&x)?,
                None => x.clone(),
            };
            x = (&x + layer.attn.forward(&h, ch, sh, c, s, m)?)?;
            x = (&x + layer.mlp.forward(&layer.mlp_norm.forward(&x)?)?)?;
        }
        self.final_norm.forward(&x)
    }
}

/// granite-embedding-97m-multilingual-r2: L2-normalised CLS sentence embeddings, [`DIM`] floats, F32 on any candle
/// device. `Send + Sync`; `embed` / `embed_batch` take `&self`.
pub struct Granite {
    tokenizer: Tokenizer,
    encoder: Encoder,
    device: Device,
    pad_id: u32,
}

impl fmt::Debug for Granite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Granite")
            .field("space_tag", &SPACE_TAG)
            .field("dim", &DIM)
            .field("layers", &self.encoder.layers.len())
            .field("max_tokens", &MAX_TOKENS)
            .field("device", &self.device)
            .finish()
    }
}

impl Granite {
    /// `dir` has config.json, model.safetensors, tokenizer.json. Weights are loaded in F32 on `device`.
    pub fn load(dir: &Path, device: &Device) -> Result<Granite> {
        let config_path = dir.join("config.json");
        let weights_path = dir.join("model.safetensors");
        let tokenizer_path = dir.join("tokenizer.json");
        let cfg: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path).with_context(|| format!("granite: reading {}", config_path.display()))?,
        )
        .with_context(|| format!("granite: parsing {}", config_path.display()))?;
        cfg.validate()?;
        let enc_cfg = cfg.encoder_config()?;

        let mut tokenizer =
            Tokenizer::from_file(&tokenizer_path).map_err(|e| anyhow!("granite: loading {}: {e}", tokenizer_path.display()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                strategy: TruncationStrategy::LongestFirst,
                direction: TruncationDirection::Right,
                stride: 0,
            }))
            .map_err(|e| anyhow!("granite: setting truncation: {e}"))?;
        let pad_id = cfg
            .pad_token_id
            .or_else(|| tokenizer.get_padding().map(|p| p.pad_id))
            .context("granite: neither config.json nor tokenizer.json has a pad token id")?;
        // tokenizer.json carries batch padding; embed_ids pads batches itself.
        tokenizer.with_padding(None);

        ensure!(weights_path.is_file(), "granite: {} not found", weights_path.display());
        // SAFETY: the safetensors file is memory-mapped only while the weights are read and converted to owned F32
        // tensors below; nothing writes to the model folder meanwhile.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, device) }
            .with_context(|| format!("granite: mapping {}", weights_path.display()))?;
        let encoder = Encoder::load(&enc_cfg, vb, device).with_context(|| format!("granite: loading weights from {}", dir.display()))?;
        Ok(Granite {
            tokenizer,
            encoder,
            device: device.clone(),
            pad_id,
        })
    }

    pub fn dim(&self) -> usize {
        DIM
    }

    pub fn space_tag(&self) -> &'static str {
        SPACE_TAG
    }

    /// The device the weights live on.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Tokens the model sees for `text`: [CLS] and [SEP] included, at most [`MAX_TOKENS`] (so a result equal to
    /// `MAX_TOKENS` may mean the text was truncated).
    pub fn token_count(&self, text: &str) -> Result<usize> {
        Ok(self.encode(text)?.len())
    }

    /// L2-normalised CLS embedding of `text`, [`DIM`] floats.
    ///
    /// Input longer than [`MAX_TOKENS`] tokens (512, [CLS] and [SEP] included) is truncated from the right — see the
    /// module documentation. The empty string is embedded as `[CLS] [SEP]`, as transformers does.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let ids = self.encode(text)?;
        let mut v = self.embed_ids(&[&ids])?;
        Ok(v.pop().expect("one row in, one vector out"))
    }

    /// The vectors [`Granite::embed`] gives for each text, in input order, computed in padded batches.
    ///
    /// Texts are grouped by token count into forward passes of at most 4096 padded tokens; padding is right-sided
    /// and masked, so each vector equals the single-text one up to float summation order (cosine ≥ 0.9999,
    /// `tests/granite_reference.rs`). An empty slice gives an empty result.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let ids = texts.iter().map(|t| self.encode(t)).collect::<Result<Vec<_>>>()?;
        let lens: Vec<usize> = ids.iter().map(Vec::len).collect();
        let mut out = vec![Vec::new(); texts.len()];
        for group in batch_groups(&lens, BATCH_TOKENS) {
            let rows: Vec<&[u32]> = group.iter().map(|&i| ids[i].as_slice()).collect();
            for (&i, v) in group.iter().zip(self.embed_ids(&rows)?) {
                out[i] = v;
            }
        }
        Ok(out)
    }

    fn encode(&self, text: &str) -> Result<Vec<u32>> {
        let enc = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow!("granite: tokenizing failed: {e}"))?;
        ensure!(
            !enc.get_ids().is_empty(),
            "granite: the tokenizer produced no tokens (no [CLS] template?)"
        );
        Ok(enc.get_ids().to_vec())
    }

    /// One forward pass over `rows` (right-padded to the longest), returning their normalised CLS vectors.
    fn embed_ids(&self, rows: &[&[u32]]) -> Result<Vec<Vec<f32>>> {
        let (b, len) = (rows.len(), rows.iter().map(|r| r.len()).max().unwrap_or(0));
        ensure!(b > 0 && len > 0, "granite: empty forward pass");
        let mut ids = Vec::with_capacity(b * len);
        let mut mask = Vec::with_capacity(b * len);
        for r in rows {
            ids.extend_from_slice(r);
            ids.resize(ids.len() + len - r.len(), self.pad_id);
            mask.resize(mask.len() + r.len(), 0f32);
            mask.resize(mask.len() + len - r.len(), MASKED);
        }
        let ids = Tensor::from_vec(ids, b * len, &self.device)?;
        let pad_mask = if rows.iter().any(|r| r.len() < len) {
            Some(Tensor::from_vec(mask, (b, 1, 1, len), &self.device)?)
        } else {
            None
        };
        let hidden = self.encoder.forward_batch(&ids, b, len, pad_mask.as_ref())?;
        // Single batched D2H transfer for all [b, hidden] CLS vectors in F32 (avoids b GPU syncs and BF16 norm drift).
        let cls_rows: Vec<Vec<f32>> = hidden
            .narrow(1, 0, 1)?
            .squeeze(1)?
            .to_dtype(DType::F32)?
            .to_device(&Device::Cpu)?
            .to_vec2()?;
        Ok(cls_rows.iter().map(|row| renormalize(&[row.as_slice()])).collect())
    }
}

/// The normalised [CLS] vector of row `row` of `hidden` (`[batch, len, hidden]`), always accumulating its L2 norm
/// in F32/F64 even when `hidden` is BF16/F16 on GPU (`PRC-09`). A vector shorter than 1e-6 is left as it is.
pub fn cls_vector(hidden: &Tensor, row: usize) -> Result<Vec<f32>> {
    let cls = hidden
        .narrow(0, row, 1)?
        .narrow(1, 0, 1)?
        .to_dtype(DType::F32)?;
    let v: Vec<f32> = cls
        .to_device(&Device::Cpu)?
        .flatten_all()?
        .to_vec1()?;
    Ok(renormalize(&[v.as_slice()]))
}

/// Mean of `parts`, L2-normalised in F64/F32 (logis-center's way of joining the pieces of a long text; one part is just
/// renormalised). A result shorter than 1e-6 is left unnormalised.
pub fn renormalize(parts: &[&[f32]]) -> Vec<f32> {
    let dim = parts.first().map_or(0, |p| p.len());
    let mut acc = vec![0.0f64; dim];
    let mut count = 0.0f64;
    for p in parts {
        for (a, &x) in acc.iter_mut().zip(p.iter()) {
            *a += x as f64;
        }
        count += 1.0;
    }
    if count > 0.0 {
        for a in acc.iter_mut() {
            *a /= count;
        }
        let norm = acc.iter().map(|&v| v * v).sum::<f64>().sqrt();
        if norm > 1e-6 {
            for a in acc.iter_mut() {
                *a /= norm;
            }
        }
    }
    acc.into_iter().map(|v| v as f32).collect()
}

/// Splits row indices into forward passes: rows sorted by length (stable), neighbours packed while
/// `rows × longest row ≤ budget`; a row longer than the budget still gets a pass of its own.
fn batch_groups(lens: &[usize], budget: usize) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by_key(|&i| lens[i]);
    let mut groups = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    for i in order {
        // Sorted ascending: `lens[i]` would be the longest row of the grown group.
        if !current.is_empty() && (current.len() + 1) * lens[i] > budget {
            groups.push(std::mem::take(&mut current));
        }
        current.push(i);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granite_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Granite>();
    }

    #[test]
    fn batch_groups_cover_every_row_once_within_budget() {
        let lens = [9, 512, 3, 9, 130, 2, 512, 600, 64];
        let groups = batch_groups(&lens, 1024);
        let mut seen: Vec<usize> = groups.iter().flatten().copied().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..lens.len()).collect::<Vec<_>>());
        for g in &groups {
            let longest = g.iter().map(|&i| lens[i]).max().unwrap();
            assert!(g.len() == 1 || g.len() * longest <= 1024, "{g:?}");
            assert!(g.windows(2).all(|w| lens[w[0]] <= lens[w[1]]), "rows are sorted by length: {g:?}");
        }
        assert_eq!(groups.last().unwrap(), &vec![7], "an over-budget row gets its own pass");
        assert!(batch_groups(&[], 1024).is_empty());
        assert_eq!(batch_groups(&[5; 16], 4096), vec![(0..16).collect::<Vec<_>>()]);
    }

    #[test]
    fn window_mask_hides_tokens_beyond_half_the_window() {
        assert!(window_mask(65, 64, &Device::Cpu, DType::F32).unwrap().is_none(), "distance ≤ 64 everywhere");
        assert!(window_mask(8192, usize::MAX / 2, &Device::Cpu, DType::F32).unwrap().is_none(), "no window");
        let m = window_mask(70, 64, &Device::Cpu, DType::F32).unwrap().unwrap();
        assert_eq!(m.dims(), &[1, 1, 70, 70]);
        let m = m.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let at = |i: usize, j: usize| m[i * 70 + j];
        assert_eq!(at(0, 64), 0.0);
        assert_eq!(at(0, 65), MASKED);
        assert_eq!(at(69, 5), 0.0);
        assert_eq!(at(69, 4), MASKED);
        assert_eq!(at(30, 30), 0.0);
    }

    #[test]
    fn rope_tables_start_at_identity_and_follow_the_formula() {
        let rope = Rope::new(32, 4, 160_000.0, &Device::Cpu).unwrap();
        let cos = rope.cos.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let sin = rope.sin.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        assert_eq!(cos.len(), 4 * 16);
        assert!(cos[..16].iter().all(|&c| c == 1.0) && sin[..16].iter().all(|&s| s == 0.0));
        // position 3, frequency index 1: θ^(−2/32)
        let angle = 3.0 * 160_000f64.powf(-2.0 / 32.0);
        assert!((f64::from(cos[3 * 16 + 1]) - angle.cos()).abs() < 1e-6);
        assert!((f64::from(sin[3 * 16 + 1]) - angle.sin()).abs() < 1e-6);
    }

    #[test]
    fn rope_rotation_matches_candles_rotate_half_formula() {
        let rope = Rope::new(32, 70, 160_000.0, &Device::Cpu).unwrap();
        let x = Tensor::randn(0f32, 1.0, (2, 3, 70, 32), &Device::Cpu).unwrap();
        let (cos, sin) = rope.cos_sin(70).unwrap();
        let ours = Rope::apply(&x, &cos, &sin).unwrap();
        let reference = candle_nn::rotary_emb::rope_slow(&x, &rope.cos, &rope.sin).unwrap();
        let diff = (ours - reference).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap();
        assert!(diff.to_scalar::<f32>().unwrap() < 1e-6);
    }

    #[test]
    fn renormalize_gives_unit_vectors_averages_parts_and_keeps_zero() {
        let v = renormalize(&[&[3.0, 4.0]]);
        assert!((v[0] - 0.6).abs() < 1e-7 && (v[1] - 0.8).abs() < 1e-7);
        let m = renormalize(&[&[1.0, 0.0], &[0.0, 1.0]]);
        assert!((m[0] - m[1]).abs() < 1e-7 && (m[0] * m[0] + m[1] * m[1] - 1.0).abs() < 1e-6);
        assert_eq!(renormalize(&[&[0.0; 4]]), vec![0.0; 4]);
        assert!(renormalize(&[]).is_empty());
    }

    #[test]
    fn silu_glu_is_silu_of_the_first_half_times_the_second() {
        let x = Tensor::new(&[[1f32, -2.0, 3.0, 0.5]], &Device::Cpu).unwrap();
        let y = silu_glu(&x).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let silu = |v: f32| v / (1.0 + (-v).exp());
        assert!((y[0] - silu(1.0) * 3.0).abs() < 1e-6 && (y[1] - silu(-2.0) * 0.5).abs() < 1e-6);
    }

    #[test]
    fn config_reads_the_granite_fields_and_rejects_what_the_port_does_not_do() {
        let json = r#"{"vocab_size": 180000, "hidden_size": 384, "intermediate_size": 1536, "num_hidden_layers": 12,
            "num_attention_heads": 12, "norm_eps": 1e-05, "norm_bias": false, "attention_bias": false, "mlp_bias": false,
            "hidden_activation": "silu", "global_rope_theta": 150000.0, "local_rope_theta": 160000.0,
            "local_attention": 128, "global_attn_every_n_layers": 3, "pad_token_id": 179935}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        cfg.validate().unwrap();
        assert_eq!(
            (cfg.global_rope_theta, cfg.local_rope_theta, cfg.local_attention),
            (150_000.0, 160_000.0, 128)
        );
        assert_eq!(cfg.pad_token_id, Some(179_935));
        let biased: Config = serde_json::from_str(&json.replace(r#""mlp_bias": false"#, r#""mlp_bias": true"#)).unwrap();
        assert!(biased.validate().is_err());
        let wide: Config = serde_json::from_str(&json.replace(r#""hidden_size": 384"#, r#""hidden_size": 768"#)).unwrap();
        assert!(wide.validate().is_err());
        let enc = cfg.encoder_config().unwrap();
        assert_eq!((enc.activation, enc.global_attn_every_n_layers, enc.local_window_half), (Activation::Silu, 3, 64));
        assert_eq!((enc.global_rope_theta, enc.local_rope_theta, enc.max_positions), (150_000.0, 160_000.0, MAX_TOKENS));
    }

    #[cfg(feature = "cuda")]
    #[test]
    fn cuda_rope_softmax_glu_and_bf16_cls_norm_match_cpu() {
        let Ok(dev) = Device::new_cuda(0) else {
            return;
        };
        // Fused RoPE parity (F32 & BF16)
        let rope_cpu = Rope::new(32, 64, 160_000.0, &Device::Cpu).unwrap();
        let rope_gpu = Rope::new(32, 64, 160_000.0, &dev).unwrap();
        let x_cpu = Tensor::randn(0f32, 1.0, (2, 12, 64, 32), &Device::Cpu).unwrap();
        let (cf, sf) = rope_cpu.cos_sin(64).unwrap();
        let want = Rope::apply(&x_cpu, &cf, &sf).unwrap();
        let (ch, sh) = rope_gpu.cos_sin_half(64).unwrap();
        let (cfg, sfg) = rope_gpu.cos_sin(64).unwrap();
        let got = Rope::apply_fast(&x_cpu.to_device(&dev).unwrap(), &ch, &sh, &cfg, &sfg)
            .unwrap()
            .to_device(&Device::Cpu)
            .unwrap();
        let max_rope = (&got - &want).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap();
        assert!(max_rope < 1e-5, "CUDA fused RoPE max diff {max_rope}");

        // Fused SiLU-GLU parity
        let wi_cpu = Tensor::randn(0f32, 1.0, (2, 16, 64), &Device::Cpu).unwrap();
        let want_glu = silu_glu(&wi_cpu).unwrap();
        let got_glu = silu_glu(&wi_cpu.to_device(&dev).unwrap()).unwrap().to_device(&Device::Cpu).unwrap();
        let max_glu = (&got_glu - &want_glu).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap();
        assert!(max_glu < 1e-5, "CUDA fused silu_glu max diff {max_glu}");

        // Fused softmax_last_dim parity on CUDA (F32 & BF16)
        let scores_cpu = Tensor::randn(0f32, 2.0, (2, 12, 32, 32), &Device::Cpu).unwrap();
        let want_sm = softmax_last(&scores_cpu).unwrap();
        let got_sm = softmax_last(&scores_cpu.to_device(&dev).unwrap()).unwrap().to_device(&Device::Cpu).unwrap();
        let max_sm = (&got_sm - &want_sm).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap();
        assert!(max_sm < 1e-5, "CUDA softmax_last max diff {max_sm}");

        // BF16 CLS vector norm accumulation in F32/F64 (PRC-09)
        let hidden_bf16 = Tensor::randn(0f32, 1.0, (2, 8, 384), &dev).unwrap().to_dtype(DType::BF16).unwrap();
        let v0 = cls_vector(&hidden_bf16, 0).unwrap();
        let norm: f64 = v0.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "BF16 CLS norm drift: {norm}");
    }
}
