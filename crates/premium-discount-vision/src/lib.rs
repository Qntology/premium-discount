//! # premium-discount-vision
//!
//! SigLIP2 So400m/16 NaFlex screen encoder on candle (moved from raon-zena's `raon-vision`, 2026-11-17): KRAFTON
//! **Raon-VisionEncoder** (LocCa ViT-SO400M-16-SigLIP2) for Raon Zena's periodic screen watcher and premium-discount's
//! accessibility regions —
//! or, with the same code, **`google/siglip2-so400m-patch16-naflex`** (the identical network in the Hugging Face
//! layout; [`layout`] maps its names, [`preprocess_for`] its pixel processing).
//!
//! * [`layout`]    – the two checkpoint layouts read under one set of names.
//! * [`tower`]     – the vision tower and MAP head, dense pass plus noto-sandbox's sparse passes
//!   (`forward_sparse` / `extend_sparse` / `merge_sparse`).
//! * [`perforate`] – checkerboard / fovea first passes and the minesweeper opening (port of noto-sandbox).
//! * [`text`]      – the text tower and SigLIP2 tokenizer, used when the dictionary changes to compute anchors.
//! * [`bank`]      – zero-shot screen domains from the dictionary's `vision` nodes and their anchors.
//! * [`screen`]    – one screen reading: encode, score, locate.
//!
//! 개인 화면 보호: 천공 패스의 확장 씨앗에는 개인 화면 도메인의 최고점이 늘 들어가고([`screen::seed_labels`]),
//! [`screen::read_pass_cancellable`] 은 프레임을 받을 때의 개인 화면 세대가 바뀌면 다음 블록 경계에서 멈춰 패치를 0 으로
//! 지우고 아무것도 내지 않는다 ([`EpochWatch`] · [`VisionAbort`]). 가린 알림 토스트 칸은 [`PassGuard::masked`] 로 뺀다.

pub mod bank;
pub mod layout;
pub mod perforate;
pub mod screen;
pub mod text;
pub mod tower;

use anyhow::{Context, Result};
use candle_core::Device;
use premium_discount_quant::safetensors::SafeTensors;
use std::path::{Path, PathBuf};

pub use bank::{Acceptance, AnchorBank, VisionDomainSpec, VisionKind, VisionScore};
pub use layout::Layout;
pub use premium_discount_sensor::naflex::{Preprocess, Resample};
pub use perforate::{ExpandCfg, PerfPattern, PerfReport, PerforatedCfg};
pub use screen::{PassGuard, ScreenConfig, ScreenEncoder, ScreenReading};
pub use tower::{EpochWatch, VisionAbort, VisionTower, WeightFormat};

/// The checkpoint file of a Raon-VisionEncoder directory (or the file itself).
pub fn checkpoint(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join("model.safetensors")
    } else {
        path.to_path_buf()
    }
}

/// The layout of a checkpoint (reads only the safetensors header); Raon-VE when the file cannot be read.
pub fn layout_of(path: &Path) -> Layout {
    let f = checkpoint(path);
    SafeTensors::open(std::slice::from_ref(&f)).map(|st| Layout::detect(&st)).unwrap_or(Layout::RaonVe)
}

/// A name for the checkpoint that changes when the weights change (anchors computed by another checkpoint are
/// stale): the layout's family and the file size (`raon-ve:<bytes>`, `siglip2-hf:<bytes>`). Swapping Raon-VE for
/// google's SigLIP2 always changes it, even if a size ever matched.
pub fn fingerprint(path: &Path) -> String {
    let f = checkpoint(path);
    let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
    format!("{}:{size}", layout_of(path).tag())
}

/// How the checkpoint's processor treats pixels. `preprocessor_config.json` next to the checkpoint wins
/// (`image_mean`, `image_std`, `resample`); without one, the layout decides: google's SigLIP2 → mean = std = 0.5,
/// bilinear; Raon-VE → CLIP mean / std, bicubic.
pub fn preprocess_for(path: &Path) -> Preprocess {
    let layout_default = match layout_of(path) {
        Layout::Siglip2Hf => Preprocess::SIGLIP2,
        Layout::RaonVe => Preprocess::RAON_VE,
    };
    let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    let Ok(text) = std::fs::read_to_string(dir.join("preprocessor_config.json")) else {
        return layout_default;
    };
    let Ok(cfg) = serde_json::from_str::<serde_json::Value>(&text) else {
        return layout_default;
    };
    let triple = |key: &str| -> Option<[f32; 3]> {
        let a = cfg.get(key)?.as_array()?;
        if a.len() != 3 {
            return None;
        }
        Some([a[0].as_f64()? as f32, a[1].as_f64()? as f32, a[2].as_f64()? as f32])
    };
    Preprocess {
        mean: triple("image_mean").unwrap_or(layout_default.mean),
        std: triple("image_std").unwrap_or(layout_default.std),
        resample: cfg
            .get("resample")
            .and_then(|r| r.as_u64())
            .and_then(Resample::from_pil)
            .unwrap_or(layout_default.resample),
    }
}

/// The vision tower and SigLIP's calibration `(exp(logit_scale), logit_bias)`.
pub fn load_vision(path: &Path, device: &Device, format: WeightFormat) -> Result<(VisionTower, f32, f32)> {
    let f = checkpoint(path);
    let st = SafeTensors::open(std::slice::from_ref(&f)).with_context(|| f.display().to_string())?;
    let tower = VisionTower::load(&st, device, format)?;
    let w = layout::Weights::new(&st);
    let scale = w.all_f32("logit_scale")?[0].exp();
    let bias = w.all_f32("logit_bias")?[0];
    Ok((tower, scale, bias))
}
