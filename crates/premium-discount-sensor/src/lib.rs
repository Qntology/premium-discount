//! # premium-discount-sensor
//!
//! 화면을 보는 센서 층 (raon-zena 의 `zena-sensor` 와 `zena-core::geometry` 를 옮겨 합침, 2026-11-17). raon-zena 의
//! 화면 감시기와 premium-discount 의 접근성 내비게이션(`Region.source = event` 후보, 비전 패치 전처리)이 함께 쓴다.
//!
//! * [`EVENT_DIFF_WGSL`] – *가상 이벤트 카메라*: 픽셀 로그 휘도 변화 `|log(Y+ε) − L_prev| ≥ C` 를 세는 WGSL 컴퓨트 셰이더.
//! * [`LogLumSensor`]    – 같은 알고리즘의 CPU 참조 구현 (시험, CPU 대체).
//! * [`extract_rois`]    – 변화 타일을 관심 영역(ROI)으로 묶고 극좌표 통계를 붙임.
//! * [`RoiTracker`]      – 프레임 사이 대응과 Inward / Outward 흐름.
//! * [`geometry`]        – 화면 좌표 상자([`geometry::BBox`]), 화면 중심 기준 극좌표, 흐름 분류.
//! * [`VisionKind`]      – 화면 종류 (장면 · 순간 · 개인 화면).
//! * [`naflex`]          – NaFlex 가변 종횡비 16×16 패치 (Raon-VisionEncoder: bicubic · CLIP, google SigLIP2: bilinear · 0.5).
//!   알림 토스트 자리만 가리는 [`naflex::redact_rgb_for_naflex`] 도 여기 있다.
//! * [`thumb`]           – 화면 감시기의 32 × 18 휘도 썸네일과 깨우기 규칙 (전체 평균 변화 또는 국소 블록 변화).
//!
//! **개인 화면 잔상 지우기.** 민감한 화면(비밀번호 · 결제)이 잡히면 부르는 쪽이 [`PurgableSensor::purge_privacy_residue`]
//! 를 부른다. CPU 센서는 이전 프레임 휘도 기준을, 추적기는 ROI 트랙을 0 으로 덮고 버린다. GPU 셰이더의 `prev_log` 는 앱이
//! `clear_buffer` 로 비우고([`EVENT_DIFF_PURGE_BINDINGS`]), [`EventDiffState`] 가 돌아온 첫 프레임을
//! [`REPRIME_THRESHOLD_MILLI`] 로 디스패치하게 해 그 프레임은 기준만 다시 잡고 이벤트를 하나도 세지 않는다.
//! 지우기는 `zeroize`(premium-discount-identity 와 같은 crate)로 한다.

pub mod geometry;
pub mod kind;

pub use kind::VisionKind;
pub mod naflex;
pub mod thumb;

use crate::geometry::{classify_flow, BBox, FlowDirection, FlowThresholds, Polar, ScreenGeometry};
use zeroize::Zeroize;

pub const EVENT_DIFF_WGSL: &str = include_str!("event_diff.wgsl");
pub const TILE: usize = 32;
const EPS: f32 = 1.0 / 255.0;

/// event_diff.wgsl 의 `threshold_milli` 에 넣는 센티널. 이 값으로 디스패치한 프레임은 `prev_log` 만 채우고 타일
/// 이벤트를 하나도 세지 않는다 (셰이더의 `REPRIME_THRESHOLD`). 0 으로 비운 `prev_log` 를 그냥 돌리면 로그 휘도가
/// 늘 음수(−5.54 ~ +0.004)라 거의 모든 픽셀이 한꺼번에 임계를 넘는다
pub const REPRIME_THRESHOLD_MILLI: u32 = u32::MAX;

/// 개인 화면 잔상 지우기 때 앱이 0 으로 비울 event_diff.wgsl 버퍼의 바인딩 번호: `prev_log`(1), `tile_count`(2),
/// `tile_energy_milli`(3), `tile_on`(4). `CommandEncoder::clear_buffer` 를 쓰려면 버퍼를 `STORAGE | COPY_DST` 로 만든다
pub const EVENT_DIFF_PURGE_BINDINGS: [u32; 4] = [1, 2, 3, 4];

/// 개인 화면 잔상 지우기. 민감한 화면이 잡히는 순간(개인 화면 잠금) 부른다: 이전 프레임 휘도 기준과 ROI 트랙을 0 으로
/// 덮어 버리고, 돌아온 첫 프레임이 차분 이벤트 없이 기준만 다시 잡게 한다
pub trait PurgableSensor {
    fn purge_privacy_residue(&mut self);
}

/// event_diff.wgsl 의 유니폼 `Params` (u32 넷, 16바이트). 버퍼에는 [`EventDiffParams::to_le_bytes`] 를 쓴다
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventDiffParams {
    pub width: u32,
    pub height: u32,
    pub tiles_x: u32,
    /// 임계 `C` × 1000. [`REPRIME_THRESHOLD_MILLI`] 면 재프라이밍 프레임
    pub threshold_milli: u32,
}

impl EventDiffParams {
    /// 프레임 크기와 임계 `C`(|Δ log I|). 임계는 천분의 일로 반올림하고, 센티널과 겹치지 않게 자른다
    pub fn new(width: u32, height: u32, threshold: f32) -> EventDiffParams {
        let milli = (threshold.max(0.0) as f64 * 1000.0).round().min((REPRIME_THRESHOLD_MILLI - 1) as f64) as u32;
        EventDiffParams {
            width,
            height,
            tiles_x: width.div_ceil(TILE as u32),
            threshold_milli: milli,
        }
    }

    /// 같은 크기의 재프라이밍 프레임
    pub fn reprime(self) -> EventDiffParams {
        EventDiffParams {
            threshold_milli: REPRIME_THRESHOLD_MILLI,
            ..self
        }
    }

    pub fn is_reprime(&self) -> bool {
        self.threshold_milli == REPRIME_THRESHOLD_MILLI
    }

    pub fn tiles_y(&self) -> u32 {
        self.height.div_ceil(TILE as u32)
    }

    /// `prev_log` 버퍼의 바이트 수 (픽셀마다 f32 하나)
    pub fn prev_log_bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * 4
    }

    /// 타일 버퍼(`tile_count` · `tile_energy_milli` · `tile_on`) 하나의 바이트 수 (타일마다 u32 하나)
    pub fn tile_buffer_bytes(&self) -> u64 {
        self.tiles_x as u64 * self.tiles_y() as u64 * 4
    }

    /// 유니폼 버퍼에 그대로 쓸 16바이트 (리틀 엔디언, 필드 순서 그대로)
    pub fn to_le_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, v) in [self.width, self.height, self.tiles_x, self.threshold_milli].into_iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// GPU 이벤트 카메라를 돌리는 쪽(raon-zena capture)이 CPU 에 들고 있는 상태: 다음 디스패치의 유니폼과 재프라이밍 여부.
/// 셰이더 버퍼는 앱이 가진다.
///
/// 처음 만들 때와 [`PurgableSensor::purge_privacy_residue`] 뒤에는 다음 [`EventDiffState::next_params`] 하나가
/// [`REPRIME_THRESHOLD_MILLI`] 를 싣는다 (새 버퍼도, 지운 버퍼도 `prev_log` 가 0 이다). 개인 화면 잠금 때 앱은
/// 같은 커맨드 버퍼에서 [`EVENT_DIFF_PURGE_BINDINGS`] 의 버퍼를 `clear_buffer` 로 비우고 이 상태의 purge 를 부른다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventDiffState {
    params: EventDiffParams,
    reprime: bool,
}

impl EventDiffState {
    pub fn new(width: u32, height: u32, threshold: f32) -> EventDiffState {
        EventDiffState {
            params: EventDiffParams::new(width, height, threshold),
            reprime: true,
        }
    }

    /// 보통 프레임의 유니폼 (재프라이밍과 상관없이)
    pub fn params(&self) -> EventDiffParams {
        self.params
    }

    /// 다음 디스패치가 재프라이밍 프레임인가
    pub fn reprime_pending(&self) -> bool {
        self.reprime
    }

    /// 이번 디스패치에 쓸 유니폼. 재프라이밍이 걸려 있으면 그것을 한 번 주고 푼다
    pub fn next_params(&mut self) -> EventDiffParams {
        if std::mem::take(&mut self.reprime) {
            self.params.reprime()
        } else {
            self.params
        }
    }
}

impl PurgableSensor for EventDiffState {
    fn purge_privacy_residue(&mut self) {
        self.reprime = true;
    }
}

/// Per-tile accumulators (same layout as the shader's storage buffers).
#[derive(Debug, Clone, Default)]
pub struct TileStats {
    pub tiles_x: usize,
    pub tiles_y: usize,
    pub count: Vec<u32>,
    /// Σ|ΔL| (f32 here; the shader stores milli-units in u32).
    pub energy: Vec<f32>,
    /// Number of positive-polarity (brightening) events.
    pub on: Vec<u32>,
}

/// CPU reference sensor holding the previous log-luminance frame.
pub struct LogLumSensor {
    pub width: usize,
    pub height: usize,
    /// Contrast threshold `C` on |Δ log I|.
    pub threshold: f32,
    prev: Option<Vec<f32>>,
}

#[inline]
pub fn log_luma(r: u8, g: u8, b: u8) -> f32 {
    let y = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
    (y + EPS).ln()
}

impl LogLumSensor {
    pub fn new(width: usize, height: usize, threshold: f32) -> Self {
        Self {
            width,
            height,
            threshold,
            prev: None,
        }
    }

    /// Process one BGRA8 frame (`DXGI_FORMAT_B8G8R8A8_UNORM`, tightly packed). The first frame
    /// only primes the reference and yields no events.
    pub fn process_bgra(&mut self, bgra: &[u8]) -> TileStats {
        assert_eq!(bgra.len(), self.width * self.height * 4, "frame size mismatch");
        let tx = self.width.div_ceil(TILE);
        let ty = self.height.div_ceil(TILE);
        let mut st = TileStats {
            tiles_x: tx,
            tiles_y: ty,
            count: vec![0; tx * ty],
            energy: vec![0.0; tx * ty],
            on: vec![0; tx * ty],
        };
        let first = self.prev.is_none();
        let prev = self.prev.get_or_insert_with(|| vec![0.0; self.width * self.height]);
        for y in 0..self.height {
            for x in 0..self.width {
                let i = y * self.width + x;
                let p = &bgra[i * 4..i * 4 + 4];
                let l = log_luma(p[2], p[1], p[0]);
                let d = l - prev[i];
                prev[i] = l;
                if !first && d.abs() >= self.threshold {
                    let t = (y / TILE) * tx + x / TILE;
                    st.count[t] += 1;
                    st.energy[t] += d.abs().min(16.0);
                    if d > 0.0 {
                        st.on[t] += 1;
                    }
                }
            }
        }
        st
    }

    /// 이전 프레임 기준이 있는가. 없으면 다음 프레임은 기준만 잡고 이벤트를 내지 않는다 (처음, 또는 잔상 지우기 뒤)
    pub fn is_primed(&self) -> bool {
        self.prev.is_some()
    }
}

impl PurgableSensor for LogLumSensor {
    /// 이전 프레임 로그 휘도를 0 으로 덮고 버린다. 다음 [`LogLumSensor::process_bgra`] 는 첫 프레임처럼 기준만 잡는다
    fn purge_privacy_residue(&mut self) {
        if let Some(mut prev) = self.prev.take() {
            prev.zeroize();
        }
    }
}

/// A connected cluster of active tiles.
#[derive(Debug, Clone)]
pub struct Roi {
    pub bbox: BBox,
    /// Energy-weighted centroid in pixels.
    pub centroid: (f32, f32),
    pub polar: Polar,
    /// Energy-weighted mean normalised radius (used for burst expansion/contraction).
    pub mean_r: f32,
    pub events: u32,
    pub energy: f32,
    /// Fraction of brightening events.
    pub on_ratio: f32,
}

impl Zeroize for Roi {
    fn zeroize(&mut self) {
        self.bbox.zeroize();
        self.centroid.zeroize();
        self.polar.zeroize();
        self.mean_r.zeroize();
        self.events.zeroize();
        self.energy.zeroize();
        self.on_ratio.zeroize();
    }
}

/// Cluster tiles with at least `min_events` events (8-connectivity) into ROIs, largest energy first.
pub fn extract_rois(st: &TileStats, screen: &ScreenGeometry, min_events: u32) -> Vec<Roi> {
    let (tx, ty) = (st.tiles_x, st.tiles_y);
    let active = |t: usize| st.count[t] >= min_events;
    let mut seen = vec![false; tx * ty];
    let mut out = Vec::new();
    for start in 0..tx * ty {
        if seen[start] || !active(start) {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        let (mut e, mut n, mut on, mut cx, mut cy, mut rr) = (0f32, 0u32, 0u32, 0f32, 0f32, 0f32);
        while let Some(t) = stack.pop() {
            let (x, y) = (t % tx, t / tx);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            let w = st.energy[t];
            let (px, py) = ((x * TILE + TILE / 2) as f32, (y * TILE + TILE / 2) as f32);
            e += w;
            n += st.count[t];
            on += st.on[t];
            cx += w * px;
            cy += w * py;
            rr += w * screen.to_polar(px, py).r;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= tx as i32 || ny >= ty as i32 {
                        continue;
                    }
                    let nt = ny as usize * tx + nx as usize;
                    if !seen[nt] && active(nt) {
                        seen[nt] = true;
                        stack.push(nt);
                    }
                }
            }
        }
        let w = e.max(f32::MIN_POSITIVE);
        let centroid = (cx / w, cy / w);
        out.push(Roi {
            bbox: BBox::new(
                (x0 * TILE) as f32,
                (y0 * TILE) as f32,
                (((x1 + 1) * TILE) as f32).min(screen.width),
                (((y1 + 1) * TILE) as f32).min(screen.height),
            ),
            centroid,
            polar: screen.to_polar(centroid.0, centroid.1),
            mean_r: rr / w,
            events: n,
            energy: e,
            on_ratio: if n > 0 { on as f32 / n as f32 } else { 0.0 },
        });
    }
    out.sort_by(|a, b| b.energy.total_cmp(&a.energy));
    out
}

/// 가린 자리(0..1 정규화 `(x, y, w, h)`: 개인 화면 판정이 국소 가림으로 정한 알림 토스트)와 조금이라도 겹치는 ROI 를 뺀다.
/// 토스트가 뜨며 생긴 변화가 시선 · 반응 후보가 되지 않게 하고, 본 화면의 ROI 는 그대로 둔다
pub fn drop_rois_in_zones(rois: Vec<Roi>, zones: &[(f32, f32, f32, f32)], screen: &ScreenGeometry) -> Vec<Roi> {
    let boxes: Vec<BBox> = zones
        .iter()
        .map(|&(x, y, w, h)| BBox::new(x * screen.width, y * screen.height, (x + w) * screen.width, (y + h) * screen.height))
        .collect();
    rois.into_iter().filter(|r| !boxes.iter().any(|b| r.bbox.iou(b) > 0.0)).collect()
}

/// A tracked ROI with its classified flow.
#[derive(Debug, Clone)]
pub struct Track {
    pub id: u64,
    pub roi: Roi,
    pub flow: FlowDirection,
    pub radial_velocity: f32,
    pub age: u32,
    last_ms: u64,
}

impl Zeroize for Track {
    fn zeroize(&mut self) {
        self.id.zeroize();
        self.roi.zeroize();
        self.flow = FlowDirection::Static;
        self.radial_velocity.zeroize();
        self.age.zeroize();
        self.last_ms.zeroize();
    }
}

/// Greedy nearest-centroid tracker.
pub struct RoiTracker {
    pub max_match_px: f32,
    pub thresholds: FlowThresholds,
    /// Tracks not seen for this long are dropped.
    pub ttl_ms: u64,
    tracks: Vec<Track>,
    next_id: u64,
}

impl Default for RoiTracker {
    fn default() -> Self {
        Self {
            max_match_px: 160.0,
            thresholds: FlowThresholds::default(),
            ttl_ms: 400,
            tracks: Vec::new(),
            next_id: 1,
        }
    }
}

impl RoiTracker {
    pub fn update(&mut self, rois: Vec<Roi>, now_ms: u64) -> &[Track] {
        let mut used = vec![false; self.tracks.len()];
        let mut fresh = Vec::new();
        for roi in rois {
            let best = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(i, _)| !used[*i])
                .map(|(i, t)| {
                    let d = ((t.roi.centroid.0 - roi.centroid.0).powi(2) + (t.roi.centroid.1 - roi.centroid.1).powi(2)).sqrt();
                    (i, d)
                })
                .filter(|(_, d)| *d <= self.max_match_px)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((i, _)) => {
                    used[i] = true;
                    let t = &mut self.tracks[i];
                    let dt = (now_ms.saturating_sub(t.last_ms)) as f32 / 1000.0;
                    let (flow, vr) = classify_flow(t.roi.polar, roi.polar, dt, self.thresholds);
                    t.flow = flow;
                    t.radial_velocity = vr;
                    t.roi = roi;
                    t.age += 1;
                    t.last_ms = now_ms;
                }
                None => {
                    fresh.push(Track {
                        id: self.next_id,
                        roi,
                        flow: FlowDirection::Static,
                        radial_velocity: 0.0,
                        age: 0,
                        last_ms: now_ms,
                    });
                    self.next_id += 1;
                }
            }
        }
        let ttl = self.ttl_ms;
        self.tracks.retain(|t| now_ms.saturating_sub(t.last_ms) <= ttl);
        self.tracks.extend(fresh);
        &self.tracks
    }
}

impl PurgableSensor for RoiTracker {
    /// 모든 트랙(상자 · 무게 중심 · 극좌표 · 속도)을 0 으로 덮고 비운다. 트랙 번호는 이어서 매긴다
    fn purge_privacy_residue(&mut self) {
        self.tracks.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, sq: Option<(usize, usize, usize)>) -> Vec<u8> {
        let mut f = vec![20u8; w * h * 4];
        if let Some((sx, sy, s)) = sq {
            for y in sy..(sy + s).min(h) {
                for x in sx..(sx + s).min(w) {
                    let i = (y * w + x) * 4;
                    f[i..i + 3].copy_from_slice(&[230, 230, 230]);
                }
            }
        }
        f
    }

    #[test]
    fn first_frame_primes_only_and_static_scene_is_silent() {
        let (w, h) = (256, 192);
        let mut s = LogLumSensor::new(w, h, 0.15);
        assert!(s.process_bgra(&frame(w, h, None)).count.iter().all(|&c| c == 0));
        assert!(s.process_bgra(&frame(w, h, None)).count.iter().all(|&c| c == 0));
    }

    #[test]
    fn approaching_object_is_classified_inward() {
        let (w, h) = (640, 480);
        let geo = ScreenGeometry::new(w as u32, h as u32);
        let mut s = LogLumSensor::new(w, h, 0.15);
        let mut tr = RoiTracker::default();
        s.process_bgra(&frame(w, h, None));
        // A bright square moves from the top-right corner toward the centre.
        let path = [(540, 20), (480, 70), (420, 120), (370, 160)];
        let mut last_flow = FlowDirection::Static;
        for (k, &(x, y)) in path.iter().enumerate() {
            let st = s.process_bgra(&frame(w, h, Some((x, y, 48))));
            let rois = extract_rois(&st, &geo, 8);
            assert!(!rois.is_empty());
            let tracks = tr.update(rois, (k as u64 + 1) * 100);
            if k > 0 {
                last_flow = tracks.iter().max_by(|a, b| a.roi.energy.total_cmp(&b.roi.energy)).unwrap().flow;
            }
        }
        assert_eq!(last_flow, FlowDirection::Inward);
    }

    #[test]
    fn roi_bbox_covers_change() {
        let (w, h) = (320, 240);
        let geo = ScreenGeometry::new(w as u32, h as u32);
        let mut s = LogLumSensor::new(w, h, 0.15);
        s.process_bgra(&frame(w, h, None));
        let st = s.process_bgra(&frame(w, h, Some((100, 60, 40))));
        let r = &extract_rois(&st, &geo, 4)[0];
        assert!(
            r.bbox.x0 <= 100.0 && r.bbox.x1 >= 140.0 && r.bbox.y0 <= 60.0 && r.bbox.y1 >= 100.0,
            "{:?}",
            r.bbox
        );
        assert!(r.on_ratio > 0.99);
    }

    #[test]
    fn shader_source_is_embedded() {
        assert!(EVENT_DIFF_WGSL.contains("@compute"));
        assert!(EVENT_DIFF_WGSL.contains("const TILE: u32 = 32u;"));
        assert!(EVENT_DIFF_WGSL.contains("params.threshold_milli != REPRIME_THRESHOLD && abs(d) >= thr"));
    }

    #[test]
    fn ps06_purge_zeroes_prev_log_and_reprimes() {
        let (w, h) = (320, 240);
        let geo = ScreenGeometry::new(w as u32, h as u32);
        // 어두운 게임 화면 (휘도 4/255: l = ln(5/255) ≈ −3.93)
        let dark = vec![4u8; w * h * 4];
        assert!((log_luma(4, 4, 4) + 3.93).abs() < 0.01);
        let mut s = LogLumSensor::new(w, h, 0.15);
        let mut tr = RoiTracker::default();
        s.process_bgra(&dark);
        // 민감 화면 한 프레임 (밝은 결제 창)이 들어와 이벤트 · 트랙이 생긴다
        let mut secret = dark.clone();
        for y in 40..200 {
            for x in 60..260 {
                secret[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(240);
            }
        }
        let st = s.process_bgra(&secret);
        assert!(st.count.iter().sum::<u32>() > 0);
        tr.update(extract_rois(&st, &geo, 4), 100);
        assert!(!tr.tracks.is_empty());
        // 개인 화면 잠금: 잔상 지우기
        s.purge_privacy_residue();
        tr.purge_privacy_residue();
        assert!(s.prev.is_none() && !s.is_primed());
        assert!(tr.tracks.is_empty());
        // 복귀 첫 프레임(어두운 화면)은 기준만 다시 잡는다: 타일 차분 이벤트 0건 (지운 기준과 견준 역상 폭발 없음)
        let back = s.process_bgra(&dark);
        assert!(back.count.iter().all(|&c| c == 0) && back.energy.iter().all(|&e| e == 0.0));
        assert!(s.is_primed());
        // 그다음부터는 보통대로
        assert!(s.process_bgra(&dark).count.iter().all(|&c| c == 0));
        assert!(s.process_bgra(&secret).count.iter().sum::<u32>() > 0);
        // GPU 쪽: 처음 · 지운 뒤 한 번만 REPRIME_THRESHOLD_MILLI 로 디스패치한다
        let mut gpu = EventDiffState::new(w as u32, h as u32, 0.15);
        assert_eq!(gpu.next_params().threshold_milli, REPRIME_THRESHOLD_MILLI, "새 prev_log 도 0 이다");
        assert_eq!(gpu.next_params().threshold_milli, 150);
        gpu.purge_privacy_residue();
        assert!(gpu.reprime_pending());
        let p = gpu.next_params();
        assert!(p.is_reprime() && p.to_le_bytes()[12..] == [0xFF; 4]);
        assert_eq!(gpu.next_params(), gpu.params());
        assert!(!gpu.params().is_reprime());
        // 센티널은 어떤 프레임의 |Δ log I| (최대 ≈ 5.55) 보다 크다: 셰이더의 비교만으로도 이벤트가 0 이다
        let span = log_luma(255, 255, 255) - log_luma(0, 0, 0);
        assert!(span < REPRIME_THRESHOLD_MILLI as f32 / 1000.0);
    }

    #[test]
    fn ps05_event_rois_under_the_toast_are_dropped_and_the_rest_kept() {
        let (w, h) = (640, 360);
        let geo = ScreenGeometry::new(w as u32, h as u32);
        let mut s = LogLumSensor::new(w, h, 0.15);
        s.process_bgra(&frame(w, h, None));
        // 가운데 보스(움직임)와 오른쪽 아래 알림 토스트가 한 프레임에 함께 바뀐다
        let mut f = frame(w, h, Some((280, 140, 64)));
        for y in 300..350 {
            for x in 500..630 {
                f[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(250);
            }
        }
        let rois = extract_rois(&s.process_bgra(&f), &geo, 4);
        assert_eq!(rois.len(), 2);
        let toast = (500.0 / 640.0, 300.0 / 360.0, 130.0 / 640.0, 50.0 / 360.0);
        let kept = drop_rois_in_zones(rois, &[toast], &geo);
        assert_eq!(kept.len(), 1, "토스트와 겹친 후보만 뺀다");
        assert!(kept[0].bbox.x0 <= 280.0 && kept[0].bbox.x1 >= 344.0, "보스 ROI 는 그대로: {:?}", kept[0].bbox);
        assert_eq!(drop_rois_in_zones(kept.clone(), &[], &geo).len(), 1);
    }

    #[test]
    fn event_diff_params_match_the_shader_layout() {
        let p = EventDiffParams::new(1920, 1080, 0.15);
        assert_eq!((p.tiles_x, p.tiles_y(), p.threshold_milli), (60, 34, 150));
        assert_eq!(p.prev_log_bytes(), 1920 * 1080 * 4);
        assert_eq!(p.tile_buffer_bytes(), 60 * 34 * 4);
        let b = p.to_le_bytes();
        assert_eq!(b[..4], 1920u32.to_le_bytes());
        assert_eq!(b[4..8], 1080u32.to_le_bytes());
        assert_eq!(b[8..12], 60u32.to_le_bytes());
        assert_eq!(b[12..], 150u32.to_le_bytes());
        // 보통 임계는 센티널과 겹치지 않는다
        assert!(!EventDiffParams::new(4, 4, f32::MAX).is_reprime());
        assert_eq!(EventDiffParams::new(4, 4, -1.0).threshold_milli, 0);
        assert_eq!(p.reprime().width, 1920);
    }
}

#[cfg(test)]
mod shader_validation {
    #[test]
    fn wgsl_parses_and_validates_with_naga() {
        let module = naga::front::wgsl::parse_str(super::EVENT_DIFF_WGSL).expect("WGSL parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .expect("WGSL validation");
        assert_eq!(module.entry_points[0].workgroup_size, [16, 16, 1]);
    }

    #[test]
    fn the_reprime_sentinel_keeps_the_16_byte_uniform() {
        let module = naga::front::wgsl::parse_str(super::EVENT_DIFF_WGSL).expect("WGSL parse");
        let reprime = module
            .constants
            .iter()
            .find(|(_, c)| c.name.as_deref() == Some("REPRIME_THRESHOLD"))
            .map(|(_, c)| &module.global_expressions[c.init])
            .expect("REPRIME_THRESHOLD");
        assert!(matches!(reprime, naga::Expression::Literal(naga::Literal::U32(v)) if *v == super::REPRIME_THRESHOLD_MILLI));
        let params = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("Params"))
            .map(|(_, t)| &t.inner)
            .expect("Params");
        match params {
            naga::TypeInner::Struct { members, span } => assert_eq!((members.len(), *span), (4, 16)),
            other => panic!("{other:?}"),
        }
    }
}
