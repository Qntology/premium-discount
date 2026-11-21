//! One screen reading: the patch grid through the vision tower (full, or perforated with the minesweeper opening),
//! the pooled embedding scored against the dictionary's screen domains, and where on the screen the winning domain is
//! (the peak of its heat over the cells, for Zena's gaze). Only scores and one cell position leave this module.
//!
//! 개인 화면: 확장(지뢰찾기)의 씨앗 라벨에는 개인 화면(`VisionKind::Privacy`) 도메인 가운데 1차 점수가 가장 높은 것이
//! 늘 들어간다 ([`seed_labels`]). 배경 장면 셋이 위를 차지해도 구석에 작게 뜬 로그인 창의 칸이 열린다.
//! [`PassGuard`] 를 주는 판독은 블록 · 확장 라운드 경계마다 개인 화면 세대를 보고(바뀌면 멈추고 패치를 지운다),
//! 가린 칸(알림 토스트)을 인코더에 넣지 않는다.

use crate::bank::{AnchorBank, VisionKind, VisionScore};
use crate::perforate::{cells_to_open, fill_holes, first_cells, seeds, PerfReport, PerforatedCfg};
use crate::tower::{EpochWatch, SparsePass, VisionAbort, VisionTower};
use anyhow::{bail, Result};
use premium_discount_sensor::naflex::PatchGrid;
use std::sync::atomic::AtomicU64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenConfig {
    /// NaFlex budget of the screen grid.
    pub max_patches: usize,
    /// `None`: the full pass.
    pub perforation: Option<PerforatedCfg>,
    /// Labels whose heat seeds the minesweeper opening (the best domains of the first pass).
    pub seed_labels: usize,
}

impl Default for ScreenConfig {
    fn default() -> Self {
        ScreenConfig {
            max_patches: 256,
            perforation: None,
            seed_labels: 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScreenReading {
    /// `(grid_h, grid_w)`.
    pub grid: (usize, usize),
    pub scores: Vec<VisionScore>,
    /// Centre of the winning domain's hottest cell, in 0..1 screen coordinates.
    pub peak: Option<(f32, f32)>,
    pub report: PerfReport,
    pub encode_ms: f32,
    /// The pooled embedding (unit).
    pub pooled: Vec<f32>,
}

/// 판독을 바깥에서 거드는 것. 기본값은 아무것도 하지 않는다 ([`encode_pass`] · [`read_pass`] 와 같다).
#[derive(Clone, Copy, Debug, Default)]
pub struct PassGuard<'a> {
    /// 프레임을 받을 때 읽은 개인 화면 세대. 블록 · 확장 라운드 경계마다 지금 세대와 견주어, 다르면
    /// [`VisionAbort::PrivacyLocked`] 로 멈춘다
    pub epoch: Option<EpochWatch<'a>>,
    /// 인코더에 넣지 않을 칸 (`naflex::redact_rgb_for_naflex` 가 가린 알림 토스트 칸). 확장이 열지 않고 시선 봉우리도
    /// 서지 않는다. 비어 있지 않으면 전체 패스도 그 칸을 뺀 희소 패스가 된다
    pub masked: &'a [u32],
}

impl PassGuard<'_> {
    fn check(&self) -> Result<()> {
        match &self.epoch {
            Some(w) => Ok(w.check()?),
            None => Ok(()),
        }
    }

    fn is_current(&self) -> bool {
        self.epoch.is_none_or(|w| w.is_current())
    }
}

pub struct ScreenEncoder {
    pub tower: VisionTower,
    pub cfg: ScreenConfig,
}

impl ScreenEncoder {
    pub fn new(tower: VisionTower, cfg: ScreenConfig) -> ScreenEncoder {
        ScreenEncoder { tower, cfg }
    }

    /// The pass over `grid` this encoder is configured for.
    pub fn encode(&self, grid: &PatchGrid, bank: &AnchorBank) -> Result<(SparsePass, PerfReport)> {
        encode_pass(&self.tower, &self.cfg, grid, bank)
    }

    /// Encodes, scores and locates.
    pub fn read(&self, grid: &PatchGrid, bank: &AnchorBank) -> Result<ScreenReading> {
        read_pass(&self.tower, &self.cfg, grid, bank)
    }

    /// [`read_pass_cancellable`] with this encoder.
    pub fn read_cancellable(
        &self,
        grid: &mut PatchGrid,
        bank: &AnchorBank,
        privacy_epoch: &AtomicU64,
        captured_epoch: u64,
    ) -> Result<Option<ScreenReading>> {
        read_pass_cancellable(&self.tower, &self.cfg, grid, bank, privacy_epoch, captured_epoch)
    }

    /// [`read_pass_guarded`] with this encoder.
    pub fn read_guarded(&self, grid: &mut PatchGrid, bank: &AnchorBank, guard: &PassGuard<'_>) -> Result<Option<ScreenReading>> {
        read_pass_guarded(&self.tower, &self.cfg, grid, bank, guard)
    }
}

/// 확장(지뢰찾기)의 씨앗이 될 라벨(은행 안 번호): 1차 패스 점수(`scores`, 높은 것부터) 상위 `n` 개. 그 안에 개인 화면
/// 도메인이 없으면 개인 화면 도메인 가운데 점수가 가장 높은 하나를 더한다 (배경 게임 장면이 위를 차지해도 구석에 작게
/// 뜬 로그인 · 2단계 인증 창의 칸이 열리게). 히트맵이 하나 늘 뿐이라 비용은 작다
pub fn seed_labels(bank: &AnchorBank, scores: &[VisionScore], n: usize) -> Vec<usize> {
    let mut labels: Vec<usize> = scores.iter().take(n).filter_map(|s| bank.index_of(&s.key)).collect();
    if !labels.iter().any(|&i| bank.domains[i].kind == VisionKind::Privacy) {
        if let Some(i) = scores
            .iter()
            .filter(|s| s.kind == VisionKind::Privacy)
            .find_map(|s| bank.index_of(&s.key))
        {
            labels.push(i);
        }
    }
    labels
}

/// Contrast heats of `labels` on already-computed dense features (NaN on unvisited cells).
fn heats_from_dense(cells: &[u32], dense: &[Vec<f32>], bank: &AnchorBank, labels: &[usize], n: usize) -> Vec<Vec<f32>> {
    let mut out = vec![vec![f32::NAN; n]; labels.len()];
    for (c, v) in cells.iter().zip(dense) {
        for (k, h) in bank.contrast(labels, v).into_iter().enumerate() {
            out[k][*c as usize] = h;
        }
    }
    out
}

/// Contrast heats of `labels` on the cells a pass evaluated (NaN on the others).
fn label_heats(tower: &VisionTower, pass: &SparsePass, bank: &AnchorBank, labels: &[usize], n: usize) -> Result<Vec<Vec<f32>>> {
    let dense = tower.dense(pass)?;
    Ok(heats_from_dense(&pass.cells, &dense, bank, labels, n))
}

/// The pass over `grid` that `cfg` asks for: full, or perforated with the minesweeper opening.
pub fn encode_pass(tower: &VisionTower, cfg: &ScreenConfig, grid: &PatchGrid, bank: &AnchorBank) -> Result<(SparsePass, PerfReport)> {
    encode_gated(tower, cfg, grid, bank, &[], &mut || Ok(()))
}

/// [`encode_pass`] with a [`PassGuard`]. 개인 화면 세대가 바뀌면 `Err` 다 ([`VisionAbort::of`] 가 알아본다).
pub fn encode_pass_guarded(
    tower: &VisionTower,
    cfg: &ScreenConfig,
    grid: &PatchGrid,
    bank: &AnchorBank,
    guard: &PassGuard<'_>,
) -> Result<(SparsePass, PerfReport)> {
    encode_gated(tower, cfg, grid, bank, guard.masked, &mut || guard.check())
}

/// 블록 · 확장 라운드에 들어가기 전마다 `gate` 를 부르고, `masked` 칸은 인코더에 넣지 않는 [`encode_pass`].
fn encode_gated(
    tower: &VisionTower,
    cfg: &ScreenConfig,
    grid: &PatchGrid,
    bank: &AnchorBank,
    masked: &[u32],
    gate: &mut dyn FnMut() -> Result<()>,
) -> Result<(SparsePass, PerfReport)> {
    let n = grid.cells();
    let (cols, rows) = (grid.grid_w as u32, grid.grid_h as u32);
    let mut hidden = vec![false; n];
    for &c in masked {
        if let Some(h) = hidden.get_mut(c as usize) {
            *h = true;
        }
    }
    let keep = |cells: Vec<u32>| -> Vec<u32> { cells.into_iter().filter(|&c| !hidden[c as usize]).collect() };
    let Some(perf) = cfg.perforation else {
        let cells = keep((0..n as u32).collect());
        if cells.is_empty() {
            bail!("every cell of the {}x{} grid is masked", grid.grid_h, grid.grid_w);
        }
        return Ok((
            tower.forward_sparse_gated(grid, &cells, gate)?,
            PerfReport {
                cells: n,
                first: cells.len(),
                tokens_run: cells.len(),
                ..PerfReport::default()
            },
        ));
    };
    let first = keep(first_cells(cols, rows, perf.pattern));
    if first.is_empty() {
        bail!("every first-pass cell of the {}x{} grid is masked", grid.grid_h, grid.grid_w);
    }
    let mut pass = tower.forward_sparse_gated(grid, &first, gate)?;
    let mut rep = PerfReport {
        cells: n,
        first: first.len(),
        tokens_run: first.len(),
        ..PerfReport::default()
    };
    if let (Some(x), false) = (perf.expand, bank.is_empty()) {
        let (pooled, dense) = tower.pooled_and_dense(&pass)?;
        let labels = seed_labels(bank, &bank.score(&pooled), cfg.seed_labels);
        let mut heats = heats_from_dense(&pass.cells, &dense, bank, &labels, n);
        let seed_cells = seeds(&heats, cols, rows, &x);
        rep.seeds = seed_cells.len();
        for round in 0..x.rounds {
            gate()?;
            let mut open = vec![false; n];
            for &c in &pass.cells {
                open[c as usize] = true;
            }
            let add = keep(cells_to_open(&heats, &open, &seed_cells, cols, rows, round, &x));
            if add.is_empty() {
                break;
            }
            let more = tower.extend_sparse_gated(grid, &pass, &add, gate)?;
            pass = tower.merge_sparse(&pass, &more)?;
            rep.opened += add.len();
            rep.tokens_run += add.len();
            heats = label_heats(tower, &pass, bank, &labels, n)?;
        }
    }
    Ok((pass, rep))
}

/// Encodes, scores and locates.
pub fn read_pass(tower: &VisionTower, cfg: &ScreenConfig, grid: &PatchGrid, bank: &AnchorBank) -> Result<ScreenReading> {
    read_gated(tower, cfg, grid, bank, &[], &mut || Ok(()))
}

/// [`read_pass`] with a [`PassGuard`]. 개인 화면 세대가 바뀌면(블록 경계에서 멈췄거나, 다 읽고 보니 바뀌었으면)
/// `grid` 의 패치를 0 으로 지우고 `Ok(None)`: 그 판독은 아무 데도 가지 않는다. 다른 오류는 그대로 `Err`.
pub fn read_pass_guarded(
    tower: &VisionTower,
    cfg: &ScreenConfig,
    grid: &mut PatchGrid,
    bank: &AnchorBank,
    guard: &PassGuard<'_>,
) -> Result<Option<ScreenReading>> {
    let result = read_gated(tower, cfg, grid, bank, guard.masked, &mut || guard.check());
    settle(result, guard.is_current(), grid)
}

/// 개인 화면 세대를 보며 읽는다 ([`read_pass_guarded`] 에 세대만 준 것). `captured_epoch` 는 프레임을 받을 때 읽은
/// `privacy_epoch` 값이다. 인코딩 도중 세대가 바뀌면 다음 블록 경계에서 멈추고(수 초 걸리는 패스를 끝까지 기다리지 않는다)
/// `grid` 를 0 으로 지운 뒤 `Ok(None)`.
pub fn read_pass_cancellable(
    tower: &VisionTower,
    cfg: &ScreenConfig,
    grid: &mut PatchGrid,
    bank: &AnchorBank,
    privacy_epoch: &AtomicU64,
    captured_epoch: u64,
) -> Result<Option<ScreenReading>> {
    let guard = PassGuard {
        epoch: Some(EpochWatch::new(privacy_epoch, captured_epoch)),
        masked: &[],
    };
    read_pass_guarded(tower, cfg, grid, bank, &guard)
}

/// 판독 결과를 세대로 거른다: 멈췄거나(중단) 다 읽은 뒤 세대가 바뀌었으면(`current` 가 아니면) 패치를 지우고 `None`.
fn settle(result: Result<ScreenReading>, current: bool, grid: &mut PatchGrid) -> Result<Option<ScreenReading>> {
    match result {
        Ok(reading) if current => Ok(Some(reading)),
        Ok(_) => {
            grid.wipe();
            Ok(None)
        }
        Err(e) if VisionAbort::of(&e).is_some() => {
            grid.wipe();
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// [`read_pass`] over [`encode_gated`]; `gate` runs once more before the scoring.
fn read_gated(
    tower: &VisionTower,
    cfg: &ScreenConfig,
    grid: &PatchGrid,
    bank: &AnchorBank,
    masked: &[u32],
    gate: &mut dyn FnMut() -> Result<()>,
) -> Result<ScreenReading> {
    let t0 = std::time::Instant::now();
    let (pass, report) = encode_gated(tower, cfg, grid, bank, masked, gate)?;
    gate()?;
    let (pooled, dense_opt) = if bank.is_empty() {
        (tower.pooled(&pass)?, None)
    } else {
        let (p, d) = tower.pooled_and_dense(&pass)?;
        (p, Some(d))
    };
    let scores = bank.score(&pooled);
    let n = grid.cells();
    let (cols, rows) = (grid.grid_w as u32, grid.grid_h as u32);
    let top: Vec<usize> = scores
        .iter()
        .take(cfg.seed_labels.max(1))
        .filter_map(|s| bank.index_of(&s.key))
        .collect();
    let peak = match top.first() {
        Some(_) => {
            let mut h = match dense_opt {
                Some(ref dense) => heats_from_dense(&pass.cells, dense, bank, &top, n).remove(0),
                None => label_heats(tower, &pass, bank, &top, n)?.remove(0),
            };
            let mut open = vec![false; n];
            for &c in &pass.cells {
                open[c as usize] = true;
            }
            fill_holes(&mut h, &open, cols, rows);
            // 가린 칸(알림 토스트)에는 시선이 가지 않는다
            for &c in masked {
                if let Some(v) = h.get_mut(c as usize) {
                    *v = f32::NEG_INFINITY;
                }
            }
            h.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(c, _)| {
                let (c, r) = (c as u32 % cols, c as u32 / cols);
                ((c as f32 + 0.5) / cols as f32, (r as f32 + 0.5) / rows as f32)
            })
        }
        None => None,
    };
    Ok(ScreenReading {
        grid: (grid.grid_h, grid.grid_w),
        scores,
        peak,
        report,
        encode_ms: t0.elapsed().as_secs_f32() * 1000.0,
        pooled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bank::VisionDomainSpec;
    use crate::perforate::{ExpandCfg, PerfPattern};
    use crate::tower::toy;
    use premium_discount_sensor::thumb::LumaThumb;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;

    const D: usize = 8;
    const COLS: usize = 16;
    const ROWS: usize = 9;

    /// 장면 셋(a · b · c)과 로그인 도메인 하나. `login` 이 `Scene` 이면 로그인을 보통 장면으로 둔다 (예전 규칙과 견줄 때)
    fn bank(login: VisionKind) -> AnchorBank {
        let spec = |key: &str, kind: VisionKind, prompt: &str| VisionDomainSpec {
            key: key.into(),
            kind,
            link: String::new(),
            cooldown_s: 0,
            bias: vec![(prompt.into(), 1.0)],
            prejudice: vec![],
        };
        let specs = [
            spec("vision.scene_a", VisionKind::Scene, "a"),
            spec("vision.scene_b", VisionKind::Scene, "b"),
            spec("vision.scene_c", VisionKind::Scene, "c"),
            spec("vision.login", login, "login"),
        ];
        let anchors: HashMap<String, Vec<f32>> = [("a", 0), ("b", 1), ("c", 2), ("login", 3)]
            .into_iter()
            .map(|(p, k)| (p.to_string(), toy::feature(D, k)))
            .collect();
        AnchorBank::from_specs(&specs, &[], |_| false, &anchors, 10.0, -2.0)
    }

    /// 오른쪽 아래 구석 3 × 2 칸 (16 × 9 화면의 4 %)
    fn corner(c: usize) -> bool {
        (13..16).contains(&(c % COLS)) && (7..9).contains(&(c / COLS))
    }

    /// 16 × 9 칸 게임 화면: 왼쪽부터 장면 a · b · c, 오른쪽 아래 구석에 작은 로그인 창
    fn screen() -> PatchGrid {
        toy::grid(ROWS, COLS, |c| {
            if corner(c) {
                3
            } else {
                match c % COLS {
                    0..=4 => 0,
                    5..=9 => 1,
                    _ => 2,
                }
            }
        })
    }

    fn corner_cells() -> Vec<u32> {
        (0..COLS * ROWS).filter(|&c| corner(c)).map(|c| c as u32).collect()
    }

    fn perforated() -> ScreenConfig {
        ScreenConfig {
            max_patches: 256,
            perforation: Some(PerforatedCfg {
                pattern: PerfPattern::Checker,
                expand: Some(ExpandCfg::default()),
            }),
            seed_labels: 3,
        }
    }

    #[test]
    fn ps04_privacy_seed_is_forced_into_the_perforated_pass() {
        let t = toy::tower(D, 2);
        let g = screen();
        let b = bank(VisionKind::Privacy);
        // 1차(체커보드) 패스에서 로그인 창은 장면 셋보다 점수가 낮아 상위 3 개에 들지 못한다
        let first = t
            .forward_sparse(&g, &first_cells(COLS as u32, ROWS as u32, PerfPattern::Checker))
            .unwrap();
        let scores = b.score(&t.pooled(&first).unwrap());
        assert_eq!(scores[3].key, "vision.login");
        let labels = seed_labels(&b, &scores, 3);
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[3], b.index_of("vision.login").unwrap(), "개인 화면 최고점 하나를 더한다");
        // 그래서 구멍 확장이 구석 로그인 창 칸을 모두 연다
        let (pass, rep) = encode_pass(&t, &perforated(), &g, &b).unwrap();
        assert!(corner_cells().iter().all(|c| pass.cells.contains(c)), "{:?}", pass.cells);
        // 로그인을 보통 장면으로 두면(예전 규칙) 구석 칸이 끝내 닫혀 있다
        let (old, old_rep) = encode_pass(&t, &perforated(), &g, &bank(VisionKind::Scene)).unwrap();
        assert!(!corner_cells().iter().all(|c| old.cells.contains(c)));
        assert!(rep.seeds > old_rep.seeds);
        // 열린 칸 덕분에 개인 화면 도메인이 더 잘 보인다
        let login_cos = |p: &SparsePass| {
            b.score(&t.pooled(p).unwrap())
                .into_iter()
                .find(|s| s.key == "vision.login")
                .unwrap()
                .cos
        };
        assert!(login_cos(&pass) > login_cos(&old));
    }

    #[test]
    fn seed_labels_keep_the_top_n_and_add_only_a_missing_privacy_domain() {
        let b = bank(VisionKind::Privacy);
        let login = b.index_of("vision.login").unwrap();
        let on_login = b.score(&toy::feature(D, 3));
        assert_eq!(on_login[0].key, "vision.login");
        assert_eq!(seed_labels(&b, &on_login, 3).len(), 3, "이미 들어 있으면 더하지 않는다");
        assert_eq!(seed_labels(&b, &on_login, 0), vec![login]);
        let no_privacy = bank(VisionKind::Scene);
        assert_eq!(seed_labels(&no_privacy, &no_privacy.score(&toy::feature(D, 0)), 3).len(), 3);
    }

    #[test]
    fn ps07_privacy_epoch_change_aborts_the_read_and_wipes_the_grid() {
        let t = toy::tower(D, 4);
        let b = bank(VisionKind::Privacy);
        let cfg = perforated();
        let epoch = AtomicU64::new(1);
        // 세대가 그대로면 보통 판독과 같은 답을 내고 패치를 그대로 둔다
        let mut g = screen();
        let plain = read_pass(&t, &cfg, &g, &b).unwrap();
        let same = read_pass_cancellable(&t, &cfg, &mut g, &b, &epoch, 1)
            .unwrap()
            .expect("세대 그대로");
        assert_eq!((&same.scores, same.peak, same.report), (&plain.scores, plain.peak, plain.report));
        assert_eq!(g.cells(), COLS * ROWS);
        // 1차 패스 블록 2 에 들어가기 직전 개인 화면 잠금이 세대를 1 → 2 로 올린다: 그 경계에서 바로 멈춘다
        let watch = EpochWatch::capture(&epoch);
        let mut calls = 0;
        let r = encode_gated(&t, &cfg, &g, &b, &[], &mut || {
            calls += 1;
            if calls == 3 {
                epoch.fetch_add(1, Ordering::AcqRel);
            }
            Ok(watch.check()?)
        });
        assert_eq!(VisionAbort::of(&r.err().unwrap()), Some(VisionAbort::PrivacyLocked));
        assert_eq!(calls, 3, "남은 블록 · 확장 라운드는 돌지 않는다");
        // 그 프레임(세대 1 에 받은)의 판독은 아무 데도 가지 않는다: None, 패치는 0 으로 지운다
        let gone = read_pass_cancellable(&t, &cfg, &mut g, &b, &epoch, 1).unwrap();
        assert!(gone.is_none());
        assert_eq!((g.cells(), g.patches.len()), (0, 0));
        // 다 읽은 뒤에 세대가 바뀌었어도 내지 않는다
        let mut late = screen();
        let reading = read_pass(&t, &cfg, &late, &b);
        assert!(settle(reading, false, &mut late).unwrap().is_none());
        assert!(late.patches.is_empty());
        // 감시기의 32 × 18 썸네일도 같은 자리에서 지운다
        let mut thumb = LumaThumb::from_rgb(&[90; 64 * 36 * 3], 64, 36);
        thumb.wipe();
        assert!(thumb.values().is_empty());
        // 중단이 아닌 오류는 그대로 올라온다 (패치는 남는다)
        let mut empty = screen();
        let all: Vec<u32> = (0..(COLS * ROWS) as u32).collect();
        let guard = PassGuard {
            epoch: Some(EpochWatch::new(&epoch, 2)),
            masked: &all,
        };
        assert!(read_pass_guarded(&t, &ScreenConfig::default(), &mut empty, &b, &guard).is_err());
        assert_eq!(empty.cells(), COLS * ROWS);
    }

    #[test]
    fn ps05_masked_toast_cells_are_never_encoded_or_peaked() {
        let t = toy::tower(D, 2);
        let b = bank(VisionKind::Privacy);
        // 구석 칸을 알림 토스트로 가렸다 (naflex::redact_rgb_for_naflex 가 돌려준 칸)
        let masked = corner_cells();
        let guard = PassGuard {
            epoch: None,
            masked: &masked,
        };
        for cfg in [ScreenConfig::default(), perforated()] {
            let mut g = screen();
            let (pass, rep) = encode_pass_guarded(&t, &cfg, &g, &b, &guard).unwrap();
            assert!(pass.cells.iter().all(|c| !masked.contains(c)), "{:?}", cfg.perforation);
            assert_eq!(rep.cells, COLS * ROWS);
            let reading = read_pass_guarded(&t, &cfg, &mut g, &b, &guard).unwrap().expect("세대 감시 없음");
            // 시선 봉우리는 가린 칸 밖, 1등은 본 화면 장면 (토스트 속 글은 판독에 섞이지 않는다)
            let (px, py) = reading.peak.unwrap();
            let cell = (py * ROWS as f32) as usize * COLS + (px * COLS as f32) as usize;
            assert!(!masked.contains(&(cell as u32)));
            assert_ne!(reading.scores[0].key, "vision.login");
            assert_eq!(reading.scores.last().unwrap().key, "vision.login");
        }
        // 가린 칸이 없으면 전체 패스는 예전처럼 모든 칸이다
        let (full, rep) = encode_pass_guarded(&t, &ScreenConfig::default(), &screen(), &b, &PassGuard::default()).unwrap();
        assert_eq!((full.cells.len(), rep.first), (COLS * ROWS, COLS * ROWS));
    }
}
