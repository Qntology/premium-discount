//! NaFlex patchification for the SigLIP2 So400m/16 NaFlex encoders.
//!
//! Port of the released processors: Raon-VisionEncoder's (`modeling_raonve.py::RaonVEProcessor` and
//! `raon_vision_encoder/transform.py::get_image_size_for_max_num_patches`: PIL bicubic, CLIP mean/std) and
//! transformers' `Siglip2ImageProcessor` for `google/siglip2-so400m-patch16-naflex` (bilinear, mean = std = 0.5,
//! i.e. `pixel / 127.5 − 1`). Both resize while keeping aspect ratio so `ceil(h/16)*ceil(w/16) <= max_patches`
//! (the same search) and cut 16×16 patches. Patches are flattened here as `[C, ph, pw]` (channel-major) for both;
//! a Hugging Face checkpoint's patch projection is reordered to match when it is loaded (`raon_vision::layout`).
//! [`Preprocess`] holds what differs.
//!
//! 개인 화면: [`PatchGrid`] · [`NaFlexInput`] 은 버릴 때 패치 값을 0 으로 덮는다. 알림 토스트처럼 화면 일부만 가릴 때는
//! [`redact_rgb_for_naflex`] 가 그 자리를 리샘플 커널이 닿는 거리보다 넓게, 패치 경계에 맞춰 0 으로 칠한다.

use zeroize::Zeroize;

pub const PATCH: usize = 16;
pub const DEFAULT_MAX_PATCHES: usize = 256;
pub const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
pub const STD: [f32; 3] = [0.268_629_5, 0.261_302_6, 0.275_777_1];

/// The resampling filter of the processor (PIL's, with the kernel stretched when shrinking).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resample {
    /// `PIL.Image.BICUBIC` (3): Raon-VisionEncoder.
    Bicubic,
    /// `PIL.Image.BILINEAR` (2): `Siglip2ImageProcessor`.
    Bilinear,
}

impl Resample {
    /// PIL's numbering, as `preprocessor_config.json` writes it (`"resample": 2`).
    pub fn from_pil(code: u64) -> Option<Resample> {
        match code {
            2 => Some(Resample::Bilinear),
            3 => Some(Resample::Bicubic),
            _ => None,
        }
    }
}

/// What a checkpoint's image processor does to pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preprocess {
    pub mean: [f32; 3],
    pub std: [f32; 3],
    pub resample: Resample,
}

impl Preprocess {
    /// Raon-VisionEncoder: PIL bicubic, CLIP mean / std.
    pub const RAON_VE: Preprocess = Preprocess {
        mean: MEAN,
        std: STD,
        resample: Resample::Bicubic,
    };
    /// `google/siglip2-*` (`Siglip2ImageProcessor`): bilinear, mean = std = 0.5.
    pub const SIGLIP2: Preprocess = Preprocess {
        mean: [0.5; 3],
        std: [0.5; 3],
        resample: Resample::Bilinear,
    };
}

/// Binary search identical to the Python reference (same eps and bounds).
pub fn image_size_for_max_patches(h: usize, w: usize, patch: usize, max_patches: usize) -> (usize, usize) {
    let (mut lo, mut hi) = (1e-6f64, 100.0f64);
    let size = |s: f64| {
        let th = patch.max(((h as f64 * s / patch as f64).ceil() as usize) * patch);
        let tw = patch.max(((w as f64 * s / patch as f64).ceil() as usize) * patch);
        (th, tw)
    };
    while hi - lo >= 1e-5 {
        let mid = (lo + hi) / 2.0;
        let (th, tw) = size(mid);
        if (th / patch) * (tw / patch) <= max_patches {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    size(lo)
}

impl Resample {
    /// 커널의 기본 지지 폭 (입력 픽셀). 줄일 때는 축소 배율만큼 늘어난다 (PIL `precompute_coeffs`)
    pub fn base_support(self) -> f64 {
        match self {
            Resample::Bicubic => 2.0,
            Resample::Bilinear => 1.0,
        }
    }
}

/// PIL's bicubic kernel (`a = -0.5`), support 2.
fn bicubic(x: f64) -> f64 {
    let a = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * a
    } else {
        0.0
    }
}

/// PIL's bilinear (triangle) kernel, support 1.
fn triangle(x: f64) -> f64 {
    (1.0 - x.abs()).max(0.0)
}

/// Per output pixel: the first input index and the normalised weights (PIL `precompute_coeffs`: the kernel is
/// stretched by the scale when shrinking, which is what makes PIL's resize antialiased).
fn coeffs(in_size: usize, out_size: usize, resample: Resample) -> Vec<(usize, Vec<f64>)> {
    let kernel: fn(f64) -> f64 = match resample {
        Resample::Bicubic => bicubic,
        Resample::Bilinear => triangle,
    };
    let base_support = resample.base_support();
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = base_support * filterscale;
    (0..out_size)
        .map(|xx| {
            let center = (xx as f64 + 0.5) * scale;
            let xmin = ((center - support + 0.5).floor().max(0.0)) as usize;
            let xmax = ((center + support + 0.5).floor() as usize).min(in_size);
            let mut w: Vec<f64> = (xmin..xmax).map(|x| kernel((x as f64 - center + 0.5) / filterscale)).collect();
            let sum: f64 = w.iter().sum();
            if sum != 0.0 {
                w.iter_mut().for_each(|v| *v /= sum);
            }
            (xmin, w)
        })
        .collect()
}

/// Resizes interleaved 8-bit pixels (`ch` channels) like `PIL.Image.resize(..., BICUBIC)`: horizontal pass, round
/// and clip to 8 bits, vertical pass (the order and the intermediate rounding PIL uses).
pub fn resize_bicubic(src: &[u8], w: usize, h: usize, ch: usize, tw: usize, th: usize) -> Vec<u8> {
    resize(src, w, h, ch, tw, th, Resample::Bicubic)
}

/// [`resize_bicubic`] with the filter chosen (`PIL.Image.resize(..., BILINEAR)` for [`Resample::Bilinear`]).
pub fn resize(src: &[u8], w: usize, h: usize, ch: usize, tw: usize, th: usize, resample: Resample) -> Vec<u8> {
    let clip = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    let horizontal = if tw == w {
        src[..w * h * ch].to_vec()
    } else {
        let cx = coeffs(w, tw, resample);
        let mut out = vec![0u8; tw * h * ch];
        for y in 0..h {
            for (ox, (x0, ws)) in cx.iter().enumerate() {
                for c in 0..ch {
                    let v: f64 = ws
                        .iter()
                        .enumerate()
                        .map(|(k, wt)| wt * src[(y * w + x0 + k) * ch + c] as f64)
                        .sum();
                    out[(y * tw + ox) * ch + c] = clip(v);
                }
            }
        }
        out
    };
    if th == h {
        return horizontal;
    }
    let mut horizontal = horizontal;
    let cy = coeffs(h, th, resample);
    let mut out = vec![0u8; tw * th * ch];
    for (oy, (y0, ws)) in cy.iter().enumerate() {
        for x in 0..tw {
            for c in 0..ch {
                let v: f64 = ws
                    .iter()
                    .enumerate()
                    .map(|(k, wt)| wt * horizontal[((y0 + k) * tw + x) * ch + c] as f64)
                    .sum();
                out[(oy * tw + x) * ch + c] = clip(v);
            }
        }
    }
    // 가운데 그림(화면을 가로로만 줄인 것)도 화면이다: 버리기 전에 0 으로 덮는다
    horizontal.zeroize();
    out
}

/// The patches of a whole image on its NaFlex grid, without padding: `patches[cell * 768..]` is cell `cell`
/// (row-major), channel-major inside the patch like the released processor.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchGrid {
    pub grid_h: usize,
    pub grid_w: usize,
    pub patches: Vec<f32>,
}

impl PatchGrid {
    pub const DIM: usize = 3 * PATCH * PATCH;
    pub fn cells(&self) -> usize {
        self.grid_h * self.grid_w
    }
    pub fn patch(&self, cell: usize) -> &[f32] {
        &self.patches[cell * Self::DIM..(cell + 1) * Self::DIM]
    }

    /// 패치 값을 0 으로 덮고 비운다 (칸도 0 × 0 이 된다). 판독 중에 개인 화면으로 바뀌어 그만둘 때 부른다.
    /// 버릴 때(Drop)도 패치 값을 0 으로 덮는다
    pub fn wipe(&mut self) {
        self.patches.zeroize();
        self.grid_h = 0;
        self.grid_w = 0;
    }
}

impl Drop for PatchGrid {
    fn drop(&mut self) {
        self.patches.zeroize();
    }
}

/// `RaonVEProcessor` on an RGB8 image: PIL-bicubic resize to the largest grid within `max_patches`, CLIP mean/std,
/// `[C, ph, pw]` patches.
pub fn patchify_frame(rgb: &[u8], w: usize, h: usize, max_patches: usize) -> PatchGrid {
    patchify_frame_with(rgb, w, h, max_patches, &Preprocess::RAON_VE)
}

/// [`patchify_frame`] with the checkpoint's own processor settings ([`Preprocess::SIGLIP2`] for google's).
pub fn patchify_frame_with(rgb: &[u8], w: usize, h: usize, max_patches: usize, pre: &Preprocess) -> PatchGrid {
    let (th, tw) = image_size_for_max_patches(h, w, PATCH, max_patches);
    let img = resize(rgb, w, h, 3, tw, th, pre.resample);
    let (gh, gw) = (th / PATCH, tw / PATCH);
    // 256-entry LUT per channel: bit-identical to `(u as f32 / 255.0 - mean[c]) / std[c]` with zero per-pixel divisions.
    let mut lut = [[0f32; 256]; 3];
    for c in 0..3 {
        let mean = pre.mean[c];
        let std = pre.std[c];
        for v in 0..256 {
            lut[c][v] = (v as f32 / 255.0 - mean) / std;
        }
    }
    let mut patches = vec![0f32; gh * gw * PatchGrid::DIM];
    for py in 0..gh {
        for px in 0..gw {
            let base = (py * gw + px) * PatchGrid::DIM;
            for c in 0..3 {
                let table = &lut[c];
                let ch_base = base + c * PATCH * PATCH;
                for yy in 0..PATCH {
                    let row_off = ((py * PATCH + yy) * tw + px * PATCH) * 3 + c;
                    let dst_off = ch_base + yy * PATCH;
                    for xx in 0..PATCH {
                        patches[dst_off + xx] = table[img[row_off + xx * 3] as usize];
                    }
                }
            }
        }
    }
    let mut img = img;
    img.zeroize();
    PatchGrid {
        grid_h: gh,
        grid_w: gw,
        patches,
    }
}

/// BGRA8 (screen capture) to RGB8.
pub fn bgra_to_rgb(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect()
}

pub struct NaFlexInput {
    /// `[max_patches, 3*16*16]`, row-major.
    pub patches: Vec<f32>,
    /// `true` for real patches, `false` for padding.
    pub valid: Vec<bool>,
    /// `(grid_h, grid_w)`.
    pub spatial_shape: (usize, usize),
}

impl Drop for NaFlexInput {
    fn drop(&mut self) {
        self.patches.zeroize();
    }
}

/// 알림 토스트처럼 화면 일부만 가린다 (개인 화면 판정의 `PartialRedaction` 자리들). `zones` 는 0..1 정규화 `(x, y, w, h)`.
///
/// 각 자리를 화면의 2 % 와 "리샘플 커널 지지 폭 × 축소 배율 + 4픽셀" 가운데 큰 만큼 넓힌 뒤, 출력 16 × 16 패치 경계에
/// 맞춰 바깥으로 넓혀 RGB 를 0 으로 칠한다. 돌려주는 것은 그렇게 칠한 자리에 통째로 든 패치 칸(행 우선
/// `row * grid_w + col`, 오름차순)이다. 인코더에서 빼도 된다.
///
/// 나머지 칸에는 가린 자리의 원래 픽셀이 하나도 닿지 않는다: 칠하기 전 그림으로 패치를 만들어도 그 칸들의 값은 토스트가
/// 있든 없든 같다 (넓힌 거리가 bicubic(2) · bilinear(1) 커널이 닿는 거리보다 4픽셀 이상 길다). 2 % 여유는 창 사각형
/// 밖으로 번지는 그림자 몫이다. `max_patches` · `pre` 는 이 그림을 패치로 만들 때([`patchify_frame_with`])와 같은 값을 넣는다.
pub fn redact_rgb_for_naflex(
    rgb: &mut [u8],
    w: usize,
    h: usize,
    zones: &[(f32, f32, f32, f32)],
    max_patches: usize,
    pre: &Preprocess,
) -> Vec<u32> {
    let (th, tw) = image_size_for_max_patches(h, w, PATCH, max_patches);
    let gw = tw / PATCH;
    let mut masked = vec![false; (th / PATCH) * gw];
    for &(zx, zy, zw, zh) in zones {
        let (Some((c0, c1)), Some((r0, r1))) = (masked_cells(zx, zw, w, tw, pre.resample), masked_cells(zy, zh, h, th, pre.resample)) else {
            continue;
        };
        let (x0, x1) = (cell_start(c0, w, tw), cell_end(c1, w, tw));
        for y in cell_start(r0, h, th)..cell_end(r1, h, th) {
            rgb[(y * w + x0) * 3..(y * w + x1) * 3].fill(0);
        }
        for r in r0..r1 {
            masked[r * gw + c0..r * gw + c1].fill(true);
        }
    }
    (0..masked.len() as u32).filter(|&i| masked[i as usize]).collect()
}

/// 한 축에서 정규화 시작 · 길이의 자리를 넓혀 덮는 패치 칸 범위 `[first, last)`. 빈 자리 · 화면 밖이면 `None`
fn masked_cells(start: f32, len: f32, size: usize, out: usize, resample: Resample) -> Option<(usize, usize)> {
    if !(start.is_finite() && len.is_finite() && len > 0.0) || size == 0 || out < PATCH {
        return None;
    }
    let a = (start as f64 * size as f64).floor().max(0.0);
    let b = ((start as f64 + len as f64) * size as f64).ceil().min(size as f64);
    if b <= a {
        return None;
    }
    let scale = size as f64 / out as f64;
    let margin = (0.02 * size as f64).ceil().max((resample.base_support() * scale.max(1.0)).ceil() + 4.0);
    let (lo, hi) = ((a - margin).max(0.0), (b + margin).min(size as f64));
    let cell = PATCH as f64 * scale;
    let first = (lo / cell).floor() as usize;
    let last = ((hi / cell).ceil() as usize).min(out / PATCH);
    (first < last).then_some((first, last))
}

/// 패치 칸 경계 `k` 의 입력 픽셀 위치. 칸을 통째로 덮도록 시작은 내리고 끝은 올린다
fn cell_start(k: usize, size: usize, out: usize) -> usize {
    ((k * PATCH) as f64 * size as f64 / out as f64).floor() as usize
}

fn cell_end(k: usize, size: usize, out: usize) -> usize {
    (((k * PATCH) as f64 * size as f64 / out as f64).ceil() as usize).min(size)
}

/// Crop `(x0,y0,x1,y1)` out of an RGB8 image and patchify it (bilinear resize, half-pixel centres).
pub fn patchify_rgb(rgb: &[u8], img_w: usize, img_h: usize, crop: (usize, usize, usize, usize), max_patches: usize) -> NaFlexInput {
    let (x0, y0, x1, y1) = crop;
    let (cw, ch) = ((x1 - x0).max(1), (y1 - y0).max(1));
    let (th, tw) = image_size_for_max_patches(ch, cw, PATCH, max_patches);
    let (gh, gw) = (th / PATCH, tw / PATCH);
    let mut resized = vec![0f32; 3 * th * tw];
    let sx = cw as f32 / tw as f32;
    let sy = ch as f32 / th as f32;
    for oy in 0..th {
        let fy = ((oy as f32 + 0.5) * sy - 0.5).clamp(0.0, (ch - 1) as f32);
        let (iy, wy) = (fy.floor() as usize, fy.fract());
        let iy1 = (iy + 1).min(ch - 1);
        for ox in 0..tw {
            let fx = ((ox as f32 + 0.5) * sx - 0.5).clamp(0.0, (cw - 1) as f32);
            let (ix, wx) = (fx.floor() as usize, fx.fract());
            let ix1 = (ix + 1).min(cw - 1);
            for c in 0..3 {
                let px = |x: usize, y: usize| rgb[((y0 + y).min(img_h - 1) * img_w + (x0 + x).min(img_w - 1)) * 3 + c] as f32 / 255.0;
                let top = px(ix, iy) * (1.0 - wx) + px(ix1, iy) * wx;
                let bot = px(ix, iy1) * (1.0 - wx) + px(ix1, iy1) * wx;
                let v = top * (1.0 - wy) + bot * wy;
                resized[c * th * tw + oy * tw + ox] = (v - MEAN[c]) / STD[c];
            }
        }
    }
    let n = gh * gw;
    let dim = 3 * PATCH * PATCH;
    let mut patches = vec![0f32; max_patches * dim];
    // t.reshape(3, gh, ps, gw, ps).permute(1, 3, 0, 2, 4).reshape(n, 3*ps*ps)
    for py in 0..gh {
        for px in 0..gw {
            let p = py * gw + px;
            let mut k = 0;
            for c in 0..3 {
                for yy in 0..PATCH {
                    for xx in 0..PATCH {
                        patches[p * dim + k] = resized[c * th * tw + (py * PATCH + yy) * tw + px * PATCH + xx];
                        k += 1;
                    }
                }
            }
        }
    }
    resized.zeroize();
    let valid = (0..max_patches).map(|i| i < n).collect();
    NaFlexInput {
        patches,
        valid,
        spatial_shape: (gh, gw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_respect_budget_and_aspect() {
        assert_eq!(image_size_for_max_patches(256, 256, 16, 256), (256, 256));
        let (h, w) = image_size_for_max_patches(100, 400, 16, 256);
        assert!((h / 16) * (w / 16) <= 256);
        assert!(w > 3 * h);
        let (h, w) = image_size_for_max_patches(1080, 1920, 16, 256);
        assert!((h / 16) * (w / 16) <= 256 && (h / 16) * (w / 16) >= 230, "{h}x{w}");
    }

    #[test]
    fn bicubic_is_identity_at_scale_one_and_averages_when_shrinking() {
        let src: Vec<u8> = (0..4 * 3 * 3).map(|i| (i * 7 % 256) as u8).collect();
        assert_eq!(resize_bicubic(&src, 4, 3, 3, 4, 3), src);
        // A 1-pixel checkerboard shrunk 4x is flat grey (the stretched kernel averages, no aliasing).
        let (w, h) = (64, 64);
        let cb: Vec<u8> = (0..w * h).map(|i| if (i % w + i / w) % 2 == 0 { 255 } else { 0 }).collect();
        let small = resize_bicubic(&cb, w, h, 1, 16, 16);
        assert!(small[1..15].iter().all(|&v| (120..=135).contains(&v)), "{:?}", &small[..16]);
    }

    #[test]
    fn frames_patchify_without_padding() {
        let (w, h) = (480, 270);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i % 251) as u8).collect();
        let g = patchify_frame(&rgb, w, h, 144);
        assert!(g.cells() <= 144 && g.cells() >= 120, "{}x{}", g.grid_h, g.grid_w);
        assert_eq!(g.patches.len(), g.cells() * PatchGrid::DIM);
        assert!(g.grid_w > g.grid_h);
        assert_eq!(bgra_to_rgb(&[1, 2, 3, 255, 4, 5, 6, 255]), vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn patch_layout_is_channel_major() {
        // 32x16 image: left half red, right half blue → 2 patches.
        let (w, h) = (32, 16);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                if x < 16 {
                    rgb[i] = 255
                } else {
                    rgb[i + 2] = 255
                }
            }
        }
        let inp = patchify_rgb(&rgb, w, h, (0, 0, w, h), 4);
        assert_eq!(inp.spatial_shape, (1, 2));
        assert_eq!(inp.valid, vec![true, true, false, false]);
        let red0 = inp.patches[0];
        assert!((red0 - (1.0 - MEAN[0]) / STD[0]).abs() < 1e-5);
        let blue_in_p1 = inp.patches[768 + 2 * 256];
        assert!((blue_in_p1 - (1.0 - MEAN[2]) / STD[2]).abs() < 1e-5);
        assert!(inp.patches[2 * 768..].iter().all(|&v| v == 0.0));
    }

    /// 결이 있는 게임 화면 (가운데에 보스)
    fn boss_screen(w: usize, h: usize) -> Vec<u8> {
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                rgb[i] = ((x * 7 + y * 13) % 251) as u8;
                rgb[i + 1] = ((x * 3 + y * 5 + 40) % 241) as u8;
                rgb[i + 2] = ((x ^ y) % 199) as u8;
            }
        }
        for y in h * 7 / 20..h * 13 / 20 {
            for x in w * 2 / 5..w * 3 / 5 {
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[200, 30, 40]);
            }
        }
        rgb
    }

    /// `a` 위에 알림 토스트(밝은 바탕 · 글줄)와 창 밖으로 6픽셀 번진 그림자를 그린다
    fn with_toast(a: &[u8], w: usize, h: usize, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> Vec<u8> {
        let mut b = a.to_vec();
        for y in y0.saturating_sub(6)..(y1 + 6).min(h) {
            for x in x0.saturating_sub(6)..(x1 + 6).min(w) {
                let i = (y * w + x) * 3;
                let inside = (x0..x1).contains(&x) && (y0..y1).contains(&y);
                let v = if !inside {
                    b[i] / 2
                } else if (y - y0) % 8 < 2 {
                    20
                } else {
                    245
                };
                b[i..i + 3].fill(v);
            }
        }
        b
    }

    /// 토스트를 가린 결과: (가린 칸, 칠하기 전 · 뒤 패치 넷: 토스트 없음 · 있음 · 없음 칠함 · 있음 칠함)
    fn redact_case(
        w: usize,
        h: usize,
        toast: (usize, usize, usize, usize),
        budget: usize,
        pre: &Preprocess,
    ) -> (Vec<u32>, [PatchGrid; 4], Vec<u8>) {
        let a = boss_screen(w, h);
        let b = with_toast(&a, w, h, toast);
        let (x0, y0, x1, y1) = toast;
        let zone = (x0 as f32 / w as f32, y0 as f32 / h as f32, (x1 - x0) as f32 / w as f32, (y1 - y0) as f32 / h as f32);
        let (mut a2, mut b2) = (a.clone(), b.clone());
        let masked = redact_rgb_for_naflex(&mut a2, w, h, &[zone], budget, pre);
        assert_eq!(redact_rgb_for_naflex(&mut b2, w, h, &[zone], budget, pre), masked, "같은 자리면 같은 칸");
        let grids = [
            patchify_frame_with(&a, w, h, budget, pre),
            patchify_frame_with(&b, w, h, budget, pre),
            patchify_frame_with(&a2, w, h, budget, pre),
            patchify_frame_with(&b2, w, h, budget, pre),
        ];
        (masked, grids, a2)
    }

    /// 가린 칸 밖으로 토스트가 한 픽셀도 번지지 않는다: 칠하기 전에도 가리지 않은 칸은 같고, 칠한 뒤에는 모든 칸이 같다
    fn assert_no_bleed(masked: &[u32], [a, b, a2, b2]: &[PatchGrid; 4]) {
        let is_masked = |c: usize| masked.binary_search(&(c as u32)).is_ok();
        assert!((0..a.cells()).any(|c| a.patch(c) != b.patch(c)), "토스트가 무언가는 바꾼다");
        for c in (0..a.cells()).filter(|&c| !is_masked(c)) {
            assert_eq!(a.patch(c), b.patch(c), "칠하기 전, 가리지 않은 칸 {c}");
        }
        assert_eq!(a2.patches, b2.patches, "칠한 뒤에는 토스트의 흔적이 없다 (번짐 0.0)");
    }

    #[test]
    fn ps05_toast_redaction_is_patch_aligned_and_never_bleeds() {
        // 1920 × 1080 보스전 화면, 오른쪽 아래 알림 토스트 400 × 160, Raon-VE(bicubic) 256패치
        let (w, h, budget, pre) = (1920, 1080, 256, Preprocess::RAON_VE);
        let toast = (1500, 900, 1900, 1060);
        let (masked, grids, a2) = redact_case(w, h, toast, budget, &pre);
        assert_no_bleed(&masked, &grids);
        let [a, _, a_redacted, _] = &grids;
        let (gw, cells) = (a.grid_w, a.cells());
        // 토스트 밑 칸은 가렸고, 가린 칸은 오른쪽 아래 일부뿐이다
        let center = ((toast.1 + toast.3) / 2 * a.grid_h / h) * gw + (toast.0 + toast.2) / 2 * gw / w;
        assert!(masked.binary_search(&(center as u32)).is_ok());
        assert!(masked.len() * 5 < cells, "{} of {cells}", masked.len());
        // 가린 칸의 입력 자리는 통째로 0 이다 (패치 경계에 맞춰 칠했다)
        let (th, tw) = (a.grid_h * PATCH, gw * PATCH);
        for &c in &masked {
            let (r, col) = (c as usize / gw, c as usize % gw);
            for y in cell_start(r, h, th)..cell_end(r + 1, h, th) {
                let row = &a2[(y * w + cell_start(col, w, tw)) * 3..(y * w + cell_end(col + 1, w, tw)) * 3];
                assert!(row.iter().all(|&v| v == 0), "cell {c} row {y}");
            }
        }
        // 가린 칸의 이웃도 아닌 칸은 칠하기 전과 비트까지 같다: 본 화면(보스) 코칭은 그대로
        let near = |c: usize| {
            masked.iter().any(|&m| (m as usize / gw).abs_diff(c / gw) <= 1 && (m as usize % gw).abs_diff(c % gw) <= 1)
        };
        let far: Vec<usize> = (0..cells).filter(|&c| !near(c)).collect();
        assert!(far.len() * 10 >= cells * 7, "{} of {cells}", far.len());
        for c in far {
            assert_eq!(a_redacted.patch(c), a.patch(c), "cell {c}");
        }
    }

    #[test]
    fn bilinear_redaction_keeps_the_same_guarantee() {
        // google SigLIP2 (bilinear, 커널 지지 1) · 작은 화면 · 왼쪽 위 토스트
        let (masked, grids, _) = redact_case(640, 360, (20, 16, 200, 80), 144, &Preprocess::SIGLIP2);
        assert!(!masked.is_empty());
        assert_no_bleed(&masked, &grids);
    }

    #[test]
    fn empty_or_offscreen_zones_change_nothing() {
        let (w, h) = (160, 90);
        let a = boss_screen(w, h);
        let mut b = a.clone();
        let none = [(0.5, 0.5, 0.0, 0.1), (f32::NAN, 0.1, 0.2, 0.2), (1.2, 0.1, 0.2, 0.2), (0.1, -0.5, 0.2, 0.3), (0.2, 0.2, -0.1, 0.1)];
        assert!(redact_rgb_for_naflex(&mut b, w, h, &none, 64, &Preprocess::RAON_VE).is_empty());
        assert_eq!(a, b);
        // 화면 밖으로 걸친 자리는 화면 안쪽만 칠한다
        let cells = redact_rgb_for_naflex(&mut b, w, h, &[(0.9, 0.9, 0.5, 0.5)], 64, &Preprocess::RAON_VE);
        assert!(!cells.is_empty());
        assert_eq!(b[(h * w - 1) * 3..], [0, 0, 0]);
        assert_eq!(b[..3], a[..3]);
    }

    #[test]
    fn grids_wipe_their_patches() {
        let mut g = patchify_frame(&boss_screen(64, 32), 64, 32, 8);
        assert!(g.patches.iter().any(|&v| v != 0.0));
        let (ptr, cap) = (g.patches.as_ptr(), g.patches.capacity());
        g.wipe();
        assert_eq!((g.cells(), g.patches.len()), (0, 0));
        // SAFETY: 할당은 아직 g.patches 가 갖고 있고(zeroize 는 용량을 줄이지 않는다), cap 개를 모두 0 으로 썼다
        let all = unsafe { std::slice::from_raw_parts(ptr, cap) };
        assert!(all.iter().all(|&v| v == 0.0));
    }
}
