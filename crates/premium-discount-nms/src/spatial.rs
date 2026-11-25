//! Spatial-text NMS: pick the screen region a natural-language request refers to.
//!
//! * Confidence of each candidate box = cosine similarity between the request embedding and
//!   the box's feature (a Raon-VisionEncoder ROI embedding, or the embedding of its label).
//! * Greedy suppression in descending confidence, with logis-center's crop rules: a lower box
//!   is a duplicate when it is identical, has IoU ≥ 0.90 ("twin"), or has IoU ≥ 0.85 and is
//!   covered by the higher one ("snap"); same-label boxes additionally merge at IoU ≥ `iou_same`.
//! * [`exclusive_assign`] ports `exclusive_assign_by_score`: one-to-one query↔box assignment by
//!   own score, then margin over the best qualifying rival.

use crate::embed::cosine;
use serde::Serialize;
use premium_discount_sds::geometry::{BBox, Polar};

pub const SNAP_IOU: f32 = 0.85;
pub const TWIN_IOU: f32 = 0.90;

#[derive(Debug, Clone, Serialize)]
pub struct BoxCand {
    pub bbox: BBox,
    pub label: String,
    #[serde(skip)]
    pub feature: Vec<f32>,
    pub polar: Polar,
    pub confidence: f32,
}

fn covers(a: &BBox, b: &BBox) -> bool {
    a.x0 <= b.x0 && a.y0 <= b.y0 && a.x1 >= b.x1 && a.y1 >= b.y1
}

/// Set each candidate's confidence from its similarity to `query`.
pub fn score_by_text(cands: &mut [BoxCand], query: &[f32]) {
    for c in cands {
        c.confidence = cosine(&c.feature, query);
    }
}

/// Indices of the surviving boxes, best first, and `(suppressed, by)` pairs.
pub fn greedy_nms(cands: &[BoxCand], iou_same: f32) -> (Vec<usize>, Vec<(usize, usize)>) {
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| cands[b].confidence.total_cmp(&cands[a].confidence));
    let mut kept: Vec<usize> = Vec::new();
    let mut sup = Vec::new();
    for i in order {
        let c = &cands[i];
        let hit = kept.iter().copied().find(|&k| {
            let o = &cands[k];
            let iou = o.bbox.iou(&c.bbox);
            let same = o.bbox == c.bbox;
            same || iou >= TWIN_IOU || (iou >= SNAP_IOU && covers(&o.bbox, &c.bbox)) || (o.label == c.label && iou >= iou_same)
        });
        match hit {
            Some(k) => sup.push((i, k)),
            None => kept.push(i),
        }
    }
    (kept, sup)
}

/// One-to-one assignment of rows (queries) to columns (boxes). A claim needs `own ≥ abs` and a
/// margin over the best *qualifying* rival row (`≥ abs`) of at least `margin`.
pub fn exclusive_assign(matrix: &[Vec<f32>], abs: f32, margin: f32) -> Vec<Option<(usize, f32, f32)>> {
    let rows = matrix.len();
    let cols = matrix.iter().map(Vec::len).max().unwrap_or(0);
    let get = |r: usize, c: usize| matrix.get(r).and_then(|x| x.get(c)).copied().unwrap_or(-1.0);
    let mut claims = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let own = get(r, c);
            if own < abs {
                continue;
            }
            let rival = (0..rows)
                .filter(|&o| o != r)
                .map(|o| get(o, c))
                .filter(|&s| s >= abs)
                .fold(f32::MIN, f32::max);
            let rival = if rival == f32::MIN { abs } else { rival };
            let m = own - rival;
            if m >= margin {
                claims.push((r, c, own, m));
            }
        }
    }
    claims.sort_by(|a, b| b.2.total_cmp(&a.2).then(b.3.total_cmp(&a.3)));
    let mut out = vec![None; rows];
    let mut taken = vec![false; cols];
    for (r, c, own, m) in claims {
        if out[r].is_none() && !taken[c] {
            out[r] = Some((c, own, m));
            taken[c] = true;
        }
    }
    out
}

/// Score by `query`, suppress duplicates, and return the single best target (index, polar).
pub fn select_target(cands: &mut [BoxCand], query: &[f32], min_conf: f32) -> Option<(usize, Polar)> {
    score_by_text(cands, query);
    let (kept, _) = greedy_nms(cands, 0.5);
    kept.into_iter()
        .find(|&i| cands[i].confidence >= min_conf)
        .map(|i| (i, cands[i].polar))
}

/// Phase 5 Part 3: 구조 해시 단절 시 `nav_vectors` 대표 벡터와 현재 화면 영역을 잇는 다중 모달 후보.
#[derive(Debug, Clone, PartialEq)]
pub struct SoftCheckpointCandidate {
    pub id: String,
    pub label_vec: Option<Vec<f32>>,
    pub vision_vec: Option<Vec<f32>>,
    pub role_id: u8,
    pub ord_norm: f32,
}

/// Phase 5 Part 3: 구조 해시 불일치 시 이중 센터링 + 마진 게이트 1:1 소프트 체크포인트 브리지 (`Soft-Anchor Alias`).
///
/// * 라벨 + 비전 공간 둘 다 존재: `0.50 * cos_lbl + 0.25 * cos_vis + 0.15 * role + 0.10 * ord`
/// * 라벨만 공유: `0.65 * cos_lbl + 0.20 * role + 0.15 * ord` (browser_uia.rs 와 동일)
/// * 비전 ROI 만 존재(텍스트 없는 인게임 오브젝트): `0.75 * cos_vis + 0.15 * role + 0.10 * ord`
/// * 원점수 >= `min_raw`(기본 0.78) AND 2위 대비 잔차 마진 >= `min_margin`(기본 0.08)일 때만 유일 1:1 짝을 확정한다.
pub fn soft_checkpoint_bridge(
    ghost_cps: &[SoftCheckpointCandidate],
    live_regions: &[SoftCheckpointCandidate],
    min_raw: f32,
    min_margin: f32,
) -> Vec<Option<(usize, f32, f32)>> {
    use premium_discount_sds::matrix::{double_center, exclusive_assign_by_score, ABSENT};
    if ghost_cps.is_empty() || live_regions.is_empty() {
        return vec![None; ghost_cps.len()];
    }
    let mut raw = vec![vec![ABSENT; live_regions.len()]; ghost_cps.len()];
    for (i, g) in ghost_cps.iter().enumerate() {
        for (j, r) in live_regions.iter().enumerate() {
            let lbl = match (&g.label_vec, &r.label_vec) {
                (Some(a), Some(b)) if !a.is_empty() && a.len() == b.len() => Some(cosine(a, b).clamp(0.0, 1.0)),
                _ => None,
            };
            let vis = match (&g.vision_vec, &r.vision_vec) {
                (Some(a), Some(b)) if !a.is_empty() && a.len() == b.len() => Some(cosine(a, b).clamp(0.0, 1.0)),
                _ => None,
            };
            let role_eq = if g.role_id == r.role_id { 1.0 } else { 0.0 };
            let ord_sim = (1.0 - (g.ord_norm - r.ord_norm).abs()).clamp(0.0, 1.0);
            let score = match (lbl, vis) {
                (Some(l), Some(v)) => 0.50 * l + 0.25 * v + 0.15 * role_eq + 0.10 * ord_sim,
                (Some(l), None) => 0.65 * l + 0.20 * role_eq + 0.15 * ord_sim,
                (None, Some(v)) => 0.75 * v + 0.15 * role_eq + 0.10 * ord_sim,
                (None, None) => continue,
            };
            raw[i][j] = score;
        }
    }
    if ghost_cps.len() == 1 || live_regions.len() == 1 {
        // 1행 또는 1열일 때는 이중 센터링이 0으로 퇴화하므로 원점수 + 마진 직접 1:1 배정.
        return exclusive_assign(&raw, min_raw, min_margin);
    }
    let centered = double_center(&raw);
    let mut gated = vec![vec![ABSENT; live_regions.len()]; ghost_cps.len()];
    for i in 0..ghost_cps.len() {
        for j in 0..live_regions.len() {
            if raw[i][j] >= min_raw {
                gated[i][j] = centered[i][j];
            }
        }
    }
    let assigned = exclusive_assign_by_score(&gated, f32::NEG_INFINITY, f32::NEG_INFINITY);
    assigned
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            let (j, res, _) = slot?;
            let score = raw[i][j];
            let second_res = (0..live_regions.len())
                .filter(|&k| k != j && raw[i][k] > ABSENT)
                .map(|k| centered[i][k])
                .fold(f32::NEG_INFINITY, f32::max);
            let row_margin = if second_res.is_finite() { res - second_res } else { res };
            (score >= min_raw && row_margin >= min_margin).then_some((j, score, row_margin))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{HashedNgramEmbedder, TextEmbedder};

    fn cand(e: &HashedNgramEmbedder, label: &str, b: (f32, f32, f32, f32), theta: f32) -> BoxCand {
        BoxCand {
            bbox: BBox::new(b.0, b.1, b.2, b.3),
            label: label.into(),
            feature: e.embed(label),
            polar: Polar { r: 0.3, theta_deg: theta },
            confidence: 0.0,
        }
    }

    #[test]
    fn picks_the_box_the_request_names() {
        let e = HashedNgramEmbedder::default();
        let mut c = vec![
            cand(&e, "슬라임 몬스터", (0.0, 0.0, 50.0, 50.0), 10.0),
            cand(&e, "보스 몬스터", (100.0, 100.0, 200.0, 200.0), 45.0),
            cand(&e, "보스 몬스터", (102.0, 101.0, 201.0, 199.0), 46.0), // duplicate detection
            cand(&e, "체력 바", (0.0, 400.0, 300.0, 420.0), 250.0),
        ];
        let q = e.embed("저 보스 어디 있어?");
        let (i, p) = select_target(&mut c, &q, 0.2).unwrap();
        assert_eq!(c[i].label, "보스 몬스터");
        assert!((p.theta_deg - 45.0).abs() < 1.5);
        let (kept, sup) = greedy_nms(&c, 0.5);
        assert_eq!(kept.len(), 3);
        assert_eq!(sup.len(), 1);
    }

    #[test]
    fn exclusive_assignment_is_one_to_one() {
        // Two queries both like box 0; query 1 likes it more, so query 0 gets box 1.
        let m = vec![vec![0.7, 0.6], vec![0.9, 0.1]];
        let a = exclusive_assign(&m, 0.3, 0.0);
        assert_eq!(a[1].map(|x| x.0), Some(0));
        assert_eq!(a[0].map(|x| x.0), Some(1));
    }

    #[test]
    fn cn268_soft_checkpoint_bridge_recovers_refactored_dom_and_ingame_roi_vectors() {
        let ghosts = vec![
            SoftCheckpointCandidate {
                id: "0xold_pay".into(),
                label_vec: Some(vec![1.0, 0.0, 0.0]),
                vision_vec: Some(vec![0.9, 0.4359, 0.0]),
                role_id: 1,
                ord_norm: 0.2,
            },
            SoftCheckpointCandidate {
                id: "0xold_cancel".into(),
                label_vec: Some(vec![0.0, 1.0, 0.0]),
                vision_vec: Some(vec![0.0, 0.0, 1.0]),
                role_id: 1,
                ord_norm: 0.8,
            },
        ];
        // Live UI after DOM wrapper refactor & ultrawide resolution shift (different element_hash, similar embeddings).
        let live = vec![
            SoftCheckpointCandidate {
                id: "0xnew_cancel".into(),
                label_vec: Some(vec![0.05, 0.998, 0.0]),
                vision_vec: Some(vec![0.0, 0.05, 0.998]),
                role_id: 1,
                ord_norm: 0.78,
            },
            SoftCheckpointCandidate {
                id: "0xnew_pay".into(),
                label_vec: Some(vec![0.995, 0.1, 0.0]),
                vision_vec: Some(vec![0.91, 0.414, 0.0]),
                role_id: 1,
                ord_norm: 0.22,
            },
        ];
        let matched = soft_checkpoint_bridge(&ghosts, &live, 0.78, 0.08);
        assert_eq!(matched[0].map(|x| x.0), Some(1), "0xold_pay must alias to 0xnew_pay");
        assert_eq!(matched[1].map(|x| x.0), Some(0), "0xold_cancel must alias to 0xnew_cancel");

        // Vision-only in-game ROI without text labels (e.g. Raon-VE patch vector).
        let g_vis = vec![SoftCheckpointCandidate {
            id: "0xlever".into(),
            label_vec: None,
            vision_vec: Some(vec![0.6, 0.8, 0.0]),
            role_id: 4,
            ord_norm: 0.5,
        }];
        let l_vis = vec![
            SoftCheckpointCandidate {
                id: "0xwall".into(),
                label_vec: None,
                vision_vec: Some(vec![0.0, 0.0, 1.0]),
                role_id: 4,
                ord_norm: 0.1,
            },
            SoftCheckpointCandidate {
                id: "0xlever_shifted".into(),
                label_vec: None,
                vision_vec: Some(vec![0.62, 0.784, 0.0]),
                role_id: 4,
                ord_norm: 0.52,
            },
        ];
        let vis_match = soft_checkpoint_bridge(&g_vis, &l_vis, 0.78, 0.08);
        assert_eq!(vis_match[0].map(|x| x.0), Some(1));
    }
}
