//! Exact decoding with a sliced output head.
//!
//! Slicing `lm_head` to `kept_ids` (PRD §3.4) is only safe if the arg-max (or top-k) never sits
//! in a dropped row. The reference implementation this project grew out of measured slicing and
//! rejected it for exactly that reason. Here the dropped ("cold") rows are clustered offline;
//! each cluster stores its centre `c` and radius `ρ = max ‖w − c‖`. For a hidden state `h`,
//! every cold logit obeys `w·h ≤ c·h + ρ‖h‖` (Cauchy–Schwarz), so after scoring the resident
//! `[K, D]` head we only re-score the clusters whose bound beats the current best — gathered row
//! by row from the on-disk table. The result equals a full `[V, D]` arg-max over the same
//! quantized rows.

use rayon::prelude::*;

#[derive(Debug, Clone)]
pub struct ColdGuard {
    pub dim: usize,
    /// `[C, D]` cluster centres.
    pub centers: Vec<f32>,
    /// `[C]` radii (already inflated by a rounding margin).
    pub radius: Vec<f32>,
    /// Cold token ids grouped by cluster.
    pub members: Vec<u32>,
    /// `[C + 1]` offsets into `members`.
    pub offsets: Vec<u32>,
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    premium_discount_cpu_kernels::dot_f32_simd(a, b)
}

/// Mean-centered Cauchy-Schwarz upper-bound guard (Pack A/C) over cold vocabulary rows:
///
/// For every cold row `w_i` with optional bias `b_i`:
///   `w_i · h + b_i = mu_rest · h + (w_i - mu_rest) · h + b_i`
///   `<= mu_rest · h + w_rest_bound * ||h||_2 + b_rest_max`
/// where `mu_rest = mean(w_i)` and `w_rest_bound = max_i ||w_i - mu_rest||_2` (inflated by an
/// F32 rounding margin). When `upper_bound(h) < kept_max`, no cold row can beat the kept top-1,
/// guaranteeing 100% lossless exact argmax without evaluating cold rows.
#[derive(Debug, Clone)]
pub struct MeanCenteredCsGuard {
    pub dim: usize,
    /// Mean of all cold weight rows `[dim]`.
    pub mu_rest: Vec<f32>,
    /// `max_i ||w_i - mu_rest||_2` inflated for F32 rounding safety.
    pub w_rest_bound: f32,
    /// `max_i b_i` (0.0 when unbiased).
    pub b_rest_max: f32,
    /// Cold token ids in row order.
    pub ids: Vec<u32>,
}

impl MeanCenteredCsGuard {
    /// Builds a guard from dense F32 cold weight rows `[ids.len(), dim]` and optional biases `[ids.len()]`.
    pub fn from_rows(rows: &[f32], ids: &[u32], bias: Option<&[f32]>, dim: usize) -> Self {
        let n = ids.len();
        assert_eq!(rows.len(), n * dim);
        if let Some(b) = bias {
            assert_eq!(b.len(), n);
        }
        if n == 0 {
            return Self {
                dim,
                mu_rest: vec![0.0; dim],
                w_rest_bound: 0.0,
                b_rest_max: f32::NEG_INFINITY,
                ids: Vec::new(),
            };
        }
        let mut sum = vec![0.0f64; dim];
        for r in rows.chunks_exact(dim) {
            for (s, &v) in sum.iter_mut().zip(r) {
                *s += v as f64;
            }
        }
        let inv_n = 1.0 / n as f64;
        let mu_rest: Vec<f32> = sum.iter().map(|&s| (s * inv_n) as f32).collect();
        let max_dev_sq: f64 = rows
            .par_chunks_exact(dim)
            .map(|r| {
                r.iter()
                    .zip(&mu_rest)
                    .map(|(&w, &m)| {
                        let d = w as f64 - m as f64;
                        d * d
                    })
                    .sum::<f64>()
            })
            .reduce(|| 0.0f64, f64::max);
        let w_rest_bound = (max_dev_sq.sqrt() as f32) * (1.0 + 1e-4) + 1e-6;
        let b_rest_max = bias
            .map(|b| b.iter().copied().fold(f32::NEG_INFINITY, f32::max))
            .unwrap_or(0.0);
        Self {
            dim,
            mu_rest,
            w_rest_bound,
            b_rest_max,
            ids: ids.to_vec(),
        }
    }

    /// Strict upper bound on `max_{i in cold} (w_i · h + b_i)` with F32 rounding margin.
    #[inline]
    pub fn upper_bound(&self, h: &[f32], h_norm: f32) -> f32 {
        if self.ids.is_empty() {
            return f32::NEG_INFINITY;
        }
        let mu_h = dot(&self.mu_rest, h);
        let dev = self.w_rest_bound * h_norm;
        let ub = mu_h + dev + self.b_rest_max;
        ub + 1e-3 * (mu_h.abs() + dev) + 1e-4
    }

    /// Returns `true` iff any cold token could possibly tie or beat `kept_max`.
    #[inline]
    pub fn needs_rescue(&self, kept_max: f32, h: &[f32], h_norm: f32) -> bool {
        !self.ids.is_empty() && self.upper_bound(h, h_norm) >= kept_max
    }

    /// Exact top-1 over `kept_ids` union `self.ids` using the single-vector mean-centered CS guard.
    /// Returns `(best_id, best_logit, rescued)` where `rescued == true` iff cold rows were evaluated.
    pub fn exact_argmax(
        &self,
        kept_logits: &[f32],
        kept_ids: &[u32],
        h: &[f32],
        cold_logit: impl Fn(u32) -> f32,
    ) -> (u32, f32, bool) {
        let (mut best_id, mut best) = (u32::MAX, f32::NEG_INFINITY);
        for (&l, &id) in kept_logits.iter().zip(kept_ids) {
            if l > best || (l == best && id < best_id) {
                best = l;
                best_id = id;
            }
        }
        let h_norm = dot(h, h).sqrt();
        if !self.needs_rescue(best, h, h_norm) {
            return (best_id, best, false);
        }
        for &id in &self.ids {
            let l = cold_logit(id);
            if l > best || (l == best && id < best_id) {
                best = l;
                best_id = id;
            }
        }
        (best_id, best, true)
    }
}

/// Lloyd k-means (Euclidean) over the decoded cold rows `rows` (`n × dim`), seeded with evenly
/// strided rows. Deterministic.
pub fn build_cold_guard(rows: &[f32], ids: &[u32], dim: usize, clusters: usize, iters: usize) -> ColdGuard {
    let n = ids.len();
    assert_eq!(rows.len(), n * dim);
    if n == 0 {
        return ColdGuard {
            dim,
            centers: vec![],
            radius: vec![],
            members: vec![],
            offsets: vec![0],
        };
    }
    let k = clusters.clamp(1, n);
    let mut centers: Vec<f32> = (0..k)
        .flat_map(|c| rows[(c * n / k) * dim..(c * n / k + 1) * dim].to_vec())
        .collect();
    let mut assign = vec![0u32; n];
    for it in 0..=iters {
        let cn: Vec<f32> = centers.chunks_exact(dim).map(|c| dot(c, c)).collect();
        assign.par_iter_mut().enumerate().for_each(|(i, a)| {
            let w = &rows[i * dim..(i + 1) * dim];
            let mut best = (f32::INFINITY, 0u32);
            for (c, cv) in centers.chunks_exact(dim).enumerate() {
                let d = cn[c] - 2.0 * dot(w, cv);
                if d < best.0 {
                    best = (d, c as u32);
                }
            }
            *a = best.1;
        });
        if it == iters {
            break;
        }
        let mut sums = vec![0f64; k * dim];
        let mut cnt = vec![0usize; k];
        for (i, &a) in assign.iter().enumerate() {
            cnt[a as usize] += 1;
            for (s, &v) in sums[a as usize * dim..(a as usize + 1) * dim]
                .iter_mut()
                .zip(&rows[i * dim..(i + 1) * dim])
            {
                *s += v as f64;
            }
        }
        for c in 0..k {
            if cnt[c] > 0 {
                for d in 0..dim {
                    centers[c * dim + d] = (sums[c * dim + d] / cnt[c] as f64) as f32;
                }
            }
        }
    }
    let mut radius = vec![0f32; k];
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); k];
    for (i, &a) in assign.iter().enumerate() {
        let (w, c) = (&rows[i * dim..(i + 1) * dim], &centers[a as usize * dim..(a as usize + 1) * dim]);
        let r = w.iter().zip(c).map(|(x, y)| ((x - y) as f64).powi(2)).sum::<f64>().sqrt() as f32;
        radius[a as usize] = radius[a as usize].max(r);
        buckets[a as usize].push(ids[i]);
    }
    for r in &mut radius {
        *r = *r * (1.0 + 1e-4) + 1e-6;
    }
    let mut members = Vec::with_capacity(n);
    let mut offsets = vec![0u32];
    for b in buckets {
        members.extend(b);
        offsets.push(members.len() as u32);
    }
    ColdGuard {
        dim,
        centers,
        radius,
        members,
        offsets,
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ArgmaxStats {
    pub clusters: usize,
    pub clusters_refined: usize,
    pub rows_refined: usize,
}

/// Exact top-1 over the full vocabulary.
///
/// * `kept_logits[i]` is the logit of token `kept_ids[i]` (from the resident sliced head);
/// * `cold_logit(id)` scores one dropped row from the on-disk table (same quantization).
pub fn exact_argmax(
    kept_logits: &[f32],
    kept_ids: &[u32],
    guard: &ColdGuard,
    h: &[f32],
    cold_logit: impl Fn(u32) -> f32,
) -> (u32, f32, ArgmaxStats) {
    let (mut best_id, mut best) = (u32::MAX, f32::NEG_INFINITY);
    for (&l, &id) in kept_logits.iter().zip(kept_ids) {
        if l > best {
            best = l;
            best_id = id;
        }
    }
    let hn = dot(h, h).sqrt();
    let c = guard.radius.len();
    let mut bounds: Vec<(f32, usize)> = (0..c)
        .map(|j| {
            let ch = dot(&guard.centers[j * guard.dim..(j + 1) * guard.dim], h);
            let ub = ch + guard.radius[j] * hn;
            // Margin for f32 rounding differences between the bound and the row kernels.
            (ub + 1e-3 * (ch.abs() + guard.radius[j] * hn) + 1e-4, j)
        })
        .collect();
    bounds.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut st = ArgmaxStats {
        clusters: c,
        ..Default::default()
    };
    for (ub, j) in bounds {
        if ub < best {
            break;
        }
        st.clusters_refined += 1;
        for &id in &guard.members[guard.offsets[j] as usize..guard.offsets[j + 1] as usize] {
            st.rows_refined += 1;
            let l = cold_logit(id);
            if l > best || (l == best && id < best_id) {
                best = l;
                best_id = id;
            }
        }
    }
    (best_id, best, st)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        ((*seed >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0
    }

    #[test]
    fn sliced_argmax_equals_full_argmax() {
        let (v, d) = (3000usize, 64usize);
        let mut s = 42u64;
        let table: Vec<f32> = (0..v * d).map(|_| rng(&mut s)).collect();
        let kept: Vec<u32> = (0..v as u32).filter(|i| i % 3 != 0).collect();
        let cold: Vec<u32> = (0..v as u32).filter(|i| i % 3 == 0).collect();
        let cold_rows: Vec<f32> = cold
            .iter()
            .flat_map(|&i| table[i as usize * d..(i as usize + 1) * d].to_vec())
            .collect();
        let g = build_cold_guard(&cold_rows, &cold, d, 32, 4);
        assert_eq!(g.members.len(), cold.len());
        let mut refined = 0;
        for trial in 0..200 {
            let mut h: Vec<f32> = (0..d).map(|_| rng(&mut s)).collect();
            if trial % 2 == 0 {
                // Point h at a cold row so the true arg-max is outside the kept set.
                let t = cold[trial % cold.len()] as usize;
                h.iter_mut().zip(&table[t * d..(t + 1) * d]).for_each(|(a, b)| *a = 0.1 * *a + b);
            }
            let logit = |id: u32| dot(&table[id as usize * d..(id as usize + 1) * d], &h);
            let kept_logits: Vec<f32> = kept.iter().map(|&i| logit(i)).collect();
            let (id, l, st) = exact_argmax(&kept_logits, &kept, &g, &h, logit);
            let (full_id, full_l) = (0..v as u32)
                .map(|i| (i, logit(i)))
                .fold((0, f32::NEG_INFINITY), |a, b| if b.1 > a.1 { b } else { a });
            assert_eq!((id, l), (full_id, full_l), "trial {trial}");
            refined += st.rows_refined;
        }
        // The bound prunes: on average far fewer rows than the cold set are re-scored.
        assert!(refined / 200 < cold.len(), "{refined}");
    }

    #[test]
    fn mean_centered_cs_guard_is_exact_and_prunes() {
        let (v, d) = (1024usize, 64usize);
        let mut s = 123u64;
        // Cold rows clustered tightly around a shared mean, hot rows with wider directional spread.
        let mut table = vec![0.0f32; v * d];
        let cold_mean: Vec<f32> = (0..d).map(|_| rng(&mut s) * 0.5).collect();
        let kept: Vec<u32> = (0..256u32).collect();
        let cold: Vec<u32> = (256..v as u32).collect();
        for &id in &kept {
            for j in 0..d {
                table[id as usize * d + j] = rng(&mut s);
            }
        }
        for &id in &cold {
            for j in 0..d {
                table[id as usize * d + j] = cold_mean[j] + 0.05 * rng(&mut s);
            }
        }
        let cold_rows: Vec<f32> = cold
            .iter()
            .flat_map(|&i| table[i as usize * d..(i as usize + 1) * d].to_vec())
            .collect();
        let guard = MeanCenteredCsGuard::from_rows(&cold_rows, &cold, None, d);
        let mut rescues = 0usize;
        for trial in 0..100 {
            let mut h: Vec<f32> = (0..d).map(|_| rng(&mut s)).collect();
            if trial % 4 == 0 {
                // Force a cold token to win.
                let t = cold[trial % cold.len()] as usize;
                for j in 0..d {
                    h[j] = table[t * d + j] * 4.0 + 0.05 * h[j];
                }
            } else {
                // Point strongly at a kept token so CS guard prunes cleanly.
                let t = kept[trial % kept.len()] as usize;
                for j in 0..d {
                    h[j] = table[t * d + j] * 3.0;
                }
            }
            let logit = |id: u32| dot(&table[id as usize * d..(id as usize + 1) * d], &h);
            let kept_logits: Vec<f32> = kept.iter().map(|&i| logit(i)).collect();
            let (id, l, rescued) = guard.exact_argmax(&kept_logits, &kept, &h, logit);
            if rescued {
                rescues += 1;
            }
            let (full_id, full_l) = (0..v as u32).map(|i| (i, logit(i))).fold(
                (u32::MAX, f32::NEG_INFINITY),
                |a, b| if b.1 > a.1 || (b.1 == a.1 && b.0 < a.0) { b } else { a },
            );
            assert_eq!((id, l), (full_id, full_l), "trial {trial}");
        }
        assert!(rescues < 100, "guard should prune hot-winning trials (rescues={rescues})");
        assert!(rescues >= 25, "cold-winning trials must rescue (rescues={rescues})");
    }
}
