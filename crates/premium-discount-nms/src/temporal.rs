//! 1D Multi-Scale Temporal IoU (`tIoU`) Non-Maximum Suppression for audio / time-series spans.
//!
//! Pure arithmetic, `wasm32-unknown-unknown` clean, zero domain literals.
//! Supports 3 canonical temporal scales (`L0` 40ms token, `L1` 200ms short, `L2` 1000ms medium).

use serde::{Deserialize, Serialize};

/// Canonical temporal granularity levels for 1D audio / sequence span detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleLevel {
    /// Fine token resolution (40 ms = 1 audio token at 25 tok/s).
    L0,
    /// Short event window (200 ms = 5 audio tokens).
    L1,
    /// Medium phrase/event window (1,000 ms = 25 audio tokens).
    L2,
}

impl ScaleLevel {
    pub const fn window_ms(self) -> u64 {
        match self {
            Self::L0 => 40,
            Self::L1 => 200,
            Self::L2 => 1_000,
        }
    }
}

/// Candidate 1D temporal span `[start_ms, end_ms)` with confidence score and optional class id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub score: f32,
    pub label_id: u32,
    pub scale: ScaleLevel,
}

impl TemporalSpan {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// 1D Temporal Intersection-over-Union `tIoU(a, b) in [0.0, 1.0]`.
#[inline]
pub fn tiou_1d(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> f64 {
    if a_end <= a_start || b_end <= b_start {
        return 0.0;
    }
    let inter_lo = a_start.max(b_start);
    let inter_hi = a_end.min(b_end);
    let inter = inter_hi.saturating_sub(inter_lo) as f64;
    if inter == 0.0 {
        return 0.0;
    }
    let union = (a_end - a_start) as f64 + (b_end - b_start) as f64 - inter;
    if union <= 0.0 {
        0.0
    } else {
        (inter / union).clamp(0.0, 1.0)
    }
}

/// Class-aware 1D Temporal NMS: keeps highest-scoring spans and suppresses overlapping spans
/// of the same `label_id` whose `tiou_1d >= iou_threshold`.
pub fn temporal_nms_1d(mut candidates: Vec<TemporalSpan>, iou_threshold: f64, score_floor: f32) -> Vec<TemporalSpan> {
    candidates.retain(|c| c.end_ms > c.start_ms && c.score.is_finite() && c.score >= score_floor);
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.start_ms.cmp(&b.start_ms))
            .then_with(|| a.end_ms.cmp(&b.end_ms))
    });
    let mut kept: Vec<TemporalSpan> = Vec::new();
    for cand in candidates {
        let suppressed = kept.iter().any(|k| {
            k.label_id == cand.label_id
                && tiou_1d(k.start_ms, k.end_ms, cand.start_ms, cand.end_ms) >= iou_threshold
        });
        if !suppressed {
            kept.push(cand);
        }
    }
    kept.sort_by_key(|s| (s.start_ms, s.end_ms, s.label_id));
    kept
}

/// Multi-scale (L0/L1/L2) span extraction from per-token scores (`token_ms = 40` by default)
/// followed by unified 1D `tIoU` NMS.
pub fn multiscale_temporal_nms(
    token_scores: &[f32],
    token_ms: u64,
    label_id: u32,
    score_floor: f32,
    iou_threshold: f64,
) -> Vec<TemporalSpan> {
    if token_scores.is_empty() || token_ms == 0 {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let scales = [
        (ScaleLevel::L0, 1usize),
        (ScaleLevel::L1, (ScaleLevel::L1.window_ms() / token_ms).max(1) as usize),
        (ScaleLevel::L2, (ScaleLevel::L2.window_ms() / token_ms).max(1) as usize),
    ];
    for (scale, win) in scales {
        let w = win.min(token_scores.len());
        if w == 0 {
            continue;
        }
        // Sliding window mean over contiguous tokens
        let mut sum = 0.0f32;
        for (i, &s) in token_scores.iter().enumerate() {
            sum += s;
            if i >= w {
                sum -= token_scores[i - w];
            }
            if i + 1 >= w {
                let start_tok = i + 1 - w;
                let mean = sum / (w as f32);
                if mean >= score_floor {
                    candidates.push(TemporalSpan {
                        start_ms: (start_tok as u64) * token_ms,
                        end_ms: ((i + 1) as u64) * token_ms,
                        score: mean,
                        label_id,
                        scale,
                    });
                }
            }
        }
    }
    // Merge contiguous active L0 runs into exact support spans before NMS so boundary alignment stays crisp
    let mut run_start: Option<usize> = None;
    for (i, &s) in token_scores.iter().enumerate() {
        if s >= score_floor {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else if let Some(st) = run_start.take() {
            let mean = token_scores[st..i].iter().sum::<f32>() / ((i - st) as f32);
            candidates.push(TemporalSpan {
                start_ms: (st as u64) * token_ms,
                end_ms: (i as u64) * token_ms,
                score: mean + 1e-4, // slight preference for tight support span on exact ties
                label_id,
                scale: ScaleLevel::L1,
            });
        }
    }
    if let Some(st) = run_start {
        let n = token_scores.len();
        let mean = token_scores[st..n].iter().sum::<f32>() / ((n - st) as f32);
        candidates.push(TemporalSpan {
            start_ms: (st as u64) * token_ms,
            end_ms: (n as u64) * token_ms,
            score: mean + 1e-4,
            label_id,
            scale: ScaleLevel::L1,
        });
    }
    temporal_nms_1d(candidates, iou_threshold, score_floor)
}

/// Mean best-match `tIoU` of ground-truth spans `expected` against predicted spans `predicted`.
pub fn mean_matched_tiou(predicted: &[(u64, u64)], expected: &[(u64, u64)]) -> f64 {
    if expected.is_empty() {
        return if predicted.is_empty() { 1.0 } else { 0.0 };
    }
    let sum: f64 = expected
        .iter()
        .map(|&(es, ee)| {
            predicted
                .iter()
                .map(|&(ps, pe)| tiou_1d(ps, pe, es, ee))
                .fold(0.0f64, f64::max)
        })
        .sum();
    sum / expected.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiscale_nms_recovers_active_event_with_high_tiou() {
        // 50 tokens * 40ms = 2000ms audio; active event at [400ms, 1000ms) -> tokens 10..25
        let mut scores = vec![0.05f32; 50];
        for s in &mut scores[10..25] {
            *s = 0.92;
        }
        let spans = multiscale_temporal_nms(&scores, 40, 1, 0.5, 0.35);
        assert!(!spans.is_empty());
        let pred: Vec<(u64, u64)> = spans.iter().map(|s| (s.start_ms, s.end_ms)).collect();
        let m = mean_matched_tiou(&pred, &[(400, 1000)]);
        assert!(m >= 0.70, "mean_tiou={m}, pred={pred:?}");
    }
}
