//! Gate A/B comparison accounting for completion rate, waste reduction, precision, and recall.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionCounters {
    pub tp: u64,
    pub fp: u64,
    pub fn_: u64,
    pub tn: u64,
}

impl DetectionCounters {
    pub fn precision(&self) -> f64 {
        let denom = self.tp + self.fp;
        if denom == 0 {
            1.0
        } else {
            self.tp as f64 / denom as f64
        }
    }

    pub fn recall(&self) -> f64 {
        let denom = self.tp + self.fn_;
        if denom == 0 {
            1.0
        } else {
            self.tp as f64 / denom as f64
        }
    }

    pub fn f1(&self) -> f64 {
        let p = self.precision();
        let r = self.recall();
        if p + r <= 0.0 {
            0.0
        } else {
            2.0 * p * r / (p + r)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmRunSummary {
    pub runs: u64,
    pub done: u64,
    pub wasted: u64,
    pub ticks: u64,
    pub counters: DetectionCounters,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateAbVerdict {
    pub passed: bool,
    pub completed_all: bool,
    pub waste_reduction: f64,
    pub precision_delta: f64,
    pub recall_delta: f64,
    pub reasons: Vec<String>,
}

/// Evaluates an A/B gate comparison: requires 100% run completion on the gated arm,
/// non-negative waste reduction, non-decreasing precision, and non-degrading recall.
pub fn compare_gate_arms(gated_on: &ArmRunSummary, gated_off: &ArmRunSummary) -> GateAbVerdict {
    let completed_all = gated_on.runs > 0 && gated_on.done == gated_on.runs;
    let waste_reduction = if gated_off.wasted > 0 {
        (gated_off.wasted as f64 - gated_on.wasted as f64) / gated_off.wasted as f64
    } else if gated_on.wasted == 0 {
        0.0
    } else {
        -1.0
    };
    let precision_delta = gated_on.counters.precision() - gated_off.counters.precision();
    let recall_delta = gated_on.counters.recall() - gated_off.counters.recall();

    let mut reasons = Vec::new();
    if !completed_all {
        reasons.push(format!("incomplete gated runs: {}/{}", gated_on.done, gated_on.runs));
    }
    if gated_on.wasted > gated_off.wasted {
        reasons.push(format!("wasted operations increased: {} > {}", gated_on.wasted, gated_off.wasted));
    }
    if precision_delta < -1e-6 {
        reasons.push(format!("precision regressed by {precision_delta:.4}"));
    }
    if recall_delta < -1e-6 {
        reasons.push(format!("recall regressed by {recall_delta:.4}"));
    }

    GateAbVerdict {
        passed: reasons.is_empty(),
        completed_all,
        waste_reduction,
        precision_delta,
        recall_delta,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_ab_verdict_checks_completion_waste_precision_and_recall() {
        let off = ArmRunSummary {
            runs: 10,
            done: 10,
            wasted: 100,
            ticks: 400,
            counters: DetectionCounters { tp: 18, fp: 6, fn_: 2, tn: 74 },
        };
        let on_good = ArmRunSummary {
            runs: 10,
            done: 10,
            wasted: 25,
            ticks: 210,
            counters: DetectionCounters { tp: 18, fp: 1, fn_: 2, tn: 79 },
        };
        let ok = compare_gate_arms(&on_good, &off);
        assert!(ok.passed, "{ok:?}");
        assert!((ok.waste_reduction - 0.75).abs() < 1e-9);
        assert!(ok.precision_delta > 0.0);

        let mut on_drop_recall = on_good;
        on_drop_recall.counters.tp = 15;
        on_drop_recall.counters.fn_ = 5;
        assert!(!compare_gate_arms(&on_drop_recall, &off).passed);

        let mut on_incomplete = on_good;
        on_incomplete.done = 9;
        assert!(!compare_gate_arms(&on_incomplete, &off).passed);
    }
}
