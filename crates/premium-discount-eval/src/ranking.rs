//! Ranking metrics, character error rate, deterministic SplitMix64 paired bootstrap CIs,
//! effective decode timing without double multiplication, and E3 / non-reversal gates.

use std::collections::{BTreeMap, HashSet};

/// Metric optimization direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    HigherIsBetter,
    LowerIsBetter,
}

/// Multi-level equivalence certification tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EquivalenceTier {
    E0,
    E1,
    PartialE2,
    FullE2,
    E3,
}

/// Gate exit codes matching CI scripts: 0 pass, 1 regression, 2 audit error.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateExit {
    Pass = 0,
    Regressed = 1,
    AuditError = 2,
}

impl GateExit {
    pub const fn code(self) -> i32 {
        self as i32
    }
}

/// Mean Reciprocal Rank for one query (`0.0` if no relevant key appears).
pub fn mrr(ranked: &[&str], relevant: &[&str]) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let rel: HashSet<&str> = relevant.iter().copied().collect();
    for (idx, &k) in ranked.iter().enumerate() {
        if rel.contains(k) {
            return 1.0 / (idx as f64 + 1.0);
        }
    }
    0.0
}

/// Recall@k computed identically to `score_search.py`: `hits_in_top_k / min(k, unique_relevant)`.
pub fn recall_at_k(ranked: &[&str], relevant: &[&str], k: usize) -> f64 {
    if k == 0 || relevant.is_empty() {
        return 0.0;
    }
    let rel: HashSet<&str> = relevant.iter().copied().collect();
    let denom = k.min(rel.len()).max(1);
    let hits: HashSet<&str> = ranked.iter().take(k).copied().filter(|x| rel.contains(x)).collect();
    hits.len() as f64 / denom as f64
}

/// Normalized Discounted Cumulative Gain @ k (`1 / log2(rank + 2)`).
pub fn ndcg_at_k(ranked: &[&str], relevant: &[&str], k: usize) -> f64 {
    if k == 0 || relevant.is_empty() {
        return 0.0;
    }
    let rel: HashSet<&str> = relevant.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut dcg = 0.0;
    for (i, &item) in ranked.iter().take(k).enumerate() {
        if rel.contains(item) && seen.insert(item) {
            dcg += 1.0 / ((i + 2) as f64).log2();
        }
    }
    let ideal_len = k.min(rel.len());
    let ideal: f64 = (0..ideal_len).map(|i| 1.0 / ((i + 2) as f64).log2()).sum();
    if ideal > 0.0 {
        dcg / ideal
    } else {
        0.0
    }
}

/// Character-level error rate: Levenshtein edit distance divided by `max(reference_chars, 1)`.
pub fn cer_char_level(reference: &str, candidate: &str) -> f64 {
    let a: Vec<char> = reference.chars().collect();
    let b: Vec<char> = candidate.chars().collect();
    let denom = a.len().max(1) as f64;
    if a.is_empty() {
        return b.len() as f64;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as f64 / denom
}

/// Deterministic SplitMix64 PRNG so bootstrap intervals are bit-identical across OSes.
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn next_usize(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0);
        (self.next_u64() % (bound as u64)) as usize
    }
}

/// Paired bootstrap 95% empirical confidence interval `(ci_lo, ci_hi)` over `diffs = candidate - baseline`.
pub fn paired_bootstrap_ci95(diffs: &[f64], n_boot: usize, seed: u64) -> (f64, f64) {
    if diffs.is_empty() {
        return (0.0, 0.0);
    }
    let n = diffs.len();
    if n_boot == 0 {
        let m = diffs.iter().sum::<f64>() / n as f64;
        return (m, m);
    }
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(n_boot);
    for _ in 0..n_boot {
        let mut acc = 0.0;
        for _ in 0..n {
            acc += diffs[rng.next_usize(n)];
        }
        means.push(acc / n as f64);
    }
    means.sort_by(|a, b| a.total_cmp(b));
    let lo_idx = ((n_boot as f64 * 0.025).floor() as usize).min(n_boot - 1);
    let hi_idx = ((n_boot as f64 * 0.975).floor() as usize).min(n_boot - 1);
    (means[lo_idx], means[hi_idx])
}

/// Effective decode ms per reference token `sum(decode_ms_1) / sum(n_gen_0)` without double-multiplying.
pub fn effective_ms_1(decode_ms_1: &[f64], n_gen_0: &[u64]) -> Result<f64, GateExit> {
    if decode_ms_1.is_empty() || decode_ms_1.len() != n_gen_0.len() {
        return Err(GateExit::AuditError);
    }
    let mut tot_ms = 0.0f64;
    let mut tot_tok = 0u64;
    for (&ms, &tok) in decode_ms_1.iter().zip(n_gen_0.iter()) {
        if !ms.is_finite() || ms < 0.0 {
            return Err(GateExit::AuditError);
        }
        tot_ms += ms;
        tot_tok = tot_tok.saturating_add(tok);
    }
    if tot_tok == 0 {
        return Err(GateExit::AuditError);
    }
    Ok(tot_ms / tot_tok as f64)
}

/// Checks paired E3 non-inferiority respecting [`Polarity`].
///
/// For [`Polarity::HigherIsBetter`], requires `ci_lo >= -delta_max`.
/// For [`Polarity::LowerIsBetter`], prevents sign inversion by requiring `ci_hi <= +delta_max`.
pub fn check_e3_non_inferiority(
    baseline: &[f64],
    candidate: &[f64],
    polarity: Polarity,
    delta_max: f64,
    n_boot: usize,
    seed: u64,
) -> GateExit {
    if baseline.is_empty() || baseline.len() != candidate.len() || !delta_max.is_finite() || delta_max < 0.0 {
        return GateExit::AuditError;
    }
    let mut diffs = Vec::with_capacity(baseline.len());
    for (&b, &c) in baseline.iter().zip(candidate.iter()) {
        if !b.is_finite() || !c.is_finite() {
            return GateExit::AuditError;
        }
        diffs.push(c - b);
    }
    let (ci_lo, ci_hi) = paired_bootstrap_ci95(&diffs, n_boot, seed);
    let ok = match polarity {
        Polarity::HigherIsBetter => ci_lo >= -delta_max,
        Polarity::LowerIsBetter => ci_hi <= delta_max,
    };
    if ok {
        GateExit::Pass
    } else {
        GateExit::Regressed
    }
}

/// Ensures candidate neither expands beyond `max_ratio` relative tolerance nor reverses polarity on mean.
pub fn check_expansion_and_non_reversal(
    baseline: &[f64],
    candidate: &[f64],
    polarity: Polarity,
    max_ratio: f64,
) -> GateExit {
    if baseline.is_empty() || baseline.len() != candidate.len() || !max_ratio.is_finite() || max_ratio < 0.0 {
        return GateExit::AuditError;
    }
    let mut sum_b = 0.0;
    let mut sum_c = 0.0;
    for (&b, &c) in baseline.iter().zip(candidate.iter()) {
        if !b.is_finite() || !c.is_finite() {
            return GateExit::AuditError;
        }
        sum_b += b;
        sum_c += c;
    }
    let mean_b = sum_b / baseline.len() as f64;
    let mean_c = sum_c / candidate.len() as f64;
    let scale = mean_b.abs().max(1e-12);
    let ok = match polarity {
        Polarity::HigherIsBetter => mean_c >= mean_b || ((mean_b - mean_c) / scale) <= max_ratio,
        Polarity::LowerIsBetter => mean_c <= mean_b || ((mean_c - mean_b) / scale) <= max_ratio,
    };
    if ok {
        GateExit::Pass
    } else {
        GateExit::Regressed
    }
}

/// Groups slice metrics in deterministic [`BTreeMap`] key order.
pub fn group_means(rows: &[(&str, f64)]) -> BTreeMap<String, f64> {
    let mut acc: BTreeMap<String, (f64, usize)> = BTreeMap::new();
    for &(k, v) in rows {
        if v.is_finite() {
            let e = acc.entry(k.to_string()).or_insert((0.0, 0));
            e.0 += v;
            e.1 += 1;
        }
    }
    acc.into_iter().map(|(k, (s, n))| (k, s / n.max(1) as f64)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cer_and_lower_is_better_non_inferiority_polarity() {
        assert_eq!(cer_char_level("hello", "hello"), 0.0);
        assert!((cer_char_level("abcd", "abxd") - 0.25).abs() < 1e-9);

        // Regression on LowerIsBetter (CER rises 0.002 -> 0.250) MUST be caught as Regressed!
        let base_bad = vec![0.002; 16];
        let cand_bad = vec![0.250; 16];
        assert_eq!(
            check_e3_non_inferiority(&base_bad, &cand_bad, Polarity::LowerIsBetter, 0.01, 2000, 7),
            GateExit::Regressed
        );

        // Improvement on LowerIsBetter (CER drops 0.015 -> 0.005) MUST Pass!
        let base_ok = vec![0.015; 16];
        let cand_ok = vec![0.005; 16];
        assert_eq!(
            check_e3_non_inferiority(&base_ok, &cand_ok, Polarity::LowerIsBetter, 0.01, 2000, 7),
            GateExit::Pass
        );

        // Sample count mismatch MUST return AuditError (2), not Regressed (1)
        assert_eq!(
            check_e3_non_inferiority(&[0.1, 0.2], &[0.1], Polarity::HigherIsBetter, 0.01, 2000, 7),
            GateExit::AuditError
        );
    }

    #[test]
    fn effective_ms_and_ranking_metrics_are_exact() {
        // 300ms + 300ms over 100 + 200 baseline tokens = 2.0 ms/tok (no double multiplication)
        let eff = effective_ms_1(&[300.0, 300.0], &[100, 200]).unwrap();
        assert!((eff - 2.0).abs() < 1e-12);
        assert_eq!(effective_ms_1(&[300.0], &[100, 200]), Err(GateExit::AuditError));
        assert_eq!(effective_ms_1(&[300.0], &[0]), Err(GateExit::AuditError));

        assert!((mrr(&["b", "a", "c"], &["a"]) - 0.5).abs() < 1e-12);
        assert!((recall_at_k(&["a", "x", "b"], &["a", "b"], 3) - 1.0).abs() < 1e-12);
        assert!(ndcg_at_k(&["a", "b"], &["a", "b"], 2) > 0.999);
    }

    #[test]
    fn bootstrap_is_bit_deterministic_and_expansion_checks_polarity() {
        let diffs = [0.02, -0.01, 0.03, 0.01, 0.0, 0.04, -0.005, 0.015];
        let a = paired_bootstrap_ci95(&diffs, 2000, 7);
        let b = paired_bootstrap_ci95(&diffs, 2000, 7);
        assert_eq!(a.0.to_bits(), b.0.to_bits());
        assert_eq!(a.1.to_bits(), b.1.to_bits());

        assert_eq!(
            check_expansion_and_non_reversal(&[10.0, 10.0], &[10.4, 10.4], Polarity::LowerIsBetter, 0.05),
            GateExit::Pass
        );
        assert_eq!(
            check_expansion_and_non_reversal(&[10.0, 10.0], &[12.0, 12.0], Polarity::LowerIsBetter, 0.05),
            GateExit::Regressed
        );
    }
}
