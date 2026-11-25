//! Shared accelerator numerical parity tolerance checks without candle dependencies.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToleranceTier {
    BitExact,
    QuantReconstruct,
    F32Compute,
    HalfCompute,
}

impl ToleranceTier {
    pub const fn max_rel_l2(&self) -> f32 {
        match self {
            ToleranceTier::BitExact => 0.0,
            ToleranceTier::QuantReconstruct => 1e-3,
            ToleranceTier::F32Compute => 1e-2,
            ToleranceTier::HalfCompute => 2.5e-2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RelErrorReport {
    pub rel_l2: f32,
    pub max_abs: f32,
    pub non_finite: usize,
    pub tier: ToleranceTier,
    pub passed: bool,
}

/// Verifies relative L2 error and finiteness between a CPU reference slice and candidate slice.
pub fn verify_tensor_parity(ref_f32: &[f32], cand_f32: &[f32], tier: ToleranceTier) -> Result<RelErrorReport> {
    if ref_f32.len() != cand_f32.len() {
        bail!("tensor parity length mismatch: ref={} cand={}", ref_f32.len(), cand_f32.len());
    }
    let mut non_finite = 0usize;
    let mut diff_sq = 0.0f64;
    let mut ref_sq = 0.0f64;
    let mut max_abs = 0.0f32;
    for (&r, &c) in ref_f32.iter().zip(cand_f32.iter()) {
        if !r.is_finite() || !c.is_finite() {
            non_finite += 1;
            continue;
        }
        let d = (r as f64) - (c as f64);
        diff_sq += d * d;
        ref_sq += (r as f64) * (r as f64);
        let ad = d.abs() as f32;
        if ad > max_abs {
            max_abs = ad;
        }
    }
    let rel_l2 = if non_finite > 0 {
        f32::INFINITY
    } else if diff_sq == 0.0 {
        0.0
    } else {
        let r = (diff_sq / ref_sq.max(1e-30)).sqrt();
        if r.is_finite() { r as f32 } else { f32::INFINITY }
    };
    let passed = non_finite == 0 && rel_l2.is_finite() && rel_l2 <= tier.max_rel_l2();
    Ok(RelErrorReport {
        rel_l2,
        max_abs,
        non_finite,
        tier,
        passed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerance_tiers_and_nan_rejection() {
        let r = [1.0f32, -2.0, 3.0, -4.0];
        assert!(verify_tensor_parity(&r, &r, ToleranceTier::BitExact).unwrap().passed);

        let f32_cand = [1.005f32, -2.0, 3.0, -4.0];
        let rep = verify_tensor_parity(&r, &f32_cand, ToleranceTier::F32Compute).unwrap();
        assert!(rep.passed);
        assert!(!verify_tensor_parity(&r, &f32_cand, ToleranceTier::BitExact).unwrap().passed);

        let half_cand = [1.08f32, -2.0, 3.0, -4.0];
        assert!(verify_tensor_parity(&r, &half_cand, ToleranceTier::HalfCompute).unwrap().passed);
        assert!(!verify_tensor_parity(&r, &half_cand, ToleranceTier::F32Compute).unwrap().passed);

        let nan_cand = [1.0f32, f32::NAN, 3.0, -4.0];
        let nan_rep = verify_tensor_parity(&r, &nan_cand, ToleranceTier::HalfCompute).unwrap();
        assert!(!nan_rep.passed && nan_rep.non_finite == 1);

        assert!(verify_tensor_parity(&r, &[1.0], ToleranceTier::F32Compute).is_err());
    }
}
