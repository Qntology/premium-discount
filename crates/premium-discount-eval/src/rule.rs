//! Scenario grading rules, likeness scoring, and cell/scenario aggregation.

use serde::{Deserialize, Serialize};

/// Dispersion denominator mode for summary statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdMode {
    Population,
    Sample,
}

/// Computes population or sample standard deviation over finite values.
pub fn sd(xs: &[f32], mode: SdMode) -> Option<f32> {
    let vals: Vec<f64> = xs.iter().copied().filter(|x| x.is_finite()).map(f64::from).collect();
    let n = vals.len();
    if n == 0 || (mode == SdMode::Sample && n < 2) {
        return None;
    }
    let mean = vals.iter().sum::<f64>() / n as f64;
    let denom = match mode {
        SdMode::Population => n as f64,
        SdMode::Sample => (n - 1) as f64,
    };
    let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / denom;
    Some(var.sqrt() as f32)
}

/// Returns the median of finite values, or `None` if no finite values are present.
pub fn median(xs: &[f32]) -> Option<f32> {
    let mut vals: Vec<f32> = xs.iter().copied().filter(|x| x.is_finite()).collect();
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.total_cmp(b));
    let n = vals.len();
    Some(if n % 2 == 1 {
        vals[n / 2]
    } else {
        0.5 * (vals[n / 2 - 1] + vals[n / 2])
    })
}

/// Universal scenario grading rule with serde tag serialization parity.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum Rule {
    Like { reference: f32, tolerance: f32 },
    AtLeast { reference: f32, tolerance: f32 },
    NoWorse { reference: f32, tolerance: f32 },
    AtMost { limit: f32 },
    Within { min: f32, max: f32 },
    Holds { expect: bool },
    Raw,
}

impl Rule {
    pub fn is_reference(&self) -> bool {
        matches!(self, Rule::Like { .. } | Rule::AtLeast { .. } | Rule::NoWorse { .. })
    }

    pub fn reference(&self) -> Option<(f32, f32)> {
        match *self {
            Rule::Like { reference, tolerance }
            | Rule::AtLeast { reference, tolerance }
            | Rule::NoWorse { reference, tolerance } => Some((reference, tolerance)),
            _ => None,
        }
    }
}

/// Result of judging a single scalar observation against a [`Rule`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleVerdict {
    pub z: Option<f32>,
    pub score: Option<f32>,
    pub broken: bool,
}

/// Maps standardized deviation `z` onto `[0, 100]` likeness score.
pub fn likeness(z: f32) -> f32 {
    if !z.is_finite() {
        return 0.0;
    }
    (100.0 - 10.0 * z.abs()).max(0.0)
}

/// Evaluates an observation against a [`Rule`].
///
/// Missing non-`Raw` values, non-finite inputs, and invalid parameters (`tolerance <= 0.0`,
/// `min > max`, non-finite bounds) force `broken = true, score = Some(0.0)`.
pub fn judge(value: Option<f32>, rule: Rule) -> RuleVerdict {
    let Some(v) = value else {
        return match rule {
            Rule::Raw => RuleVerdict::default(),
            _ => RuleVerdict {
                z: None,
                score: Some(0.0),
                broken: true,
            },
        };
    };
    if !v.is_finite() {
        return RuleVerdict {
            z: None,
            score: Some(0.0),
            broken: true,
        };
    }
    match rule {
        Rule::Like { reference, tolerance } => eval_ref(v, reference, tolerance, |z| z),
        Rule::AtLeast { reference, tolerance } => eval_ref(v, reference, tolerance, |z| z.min(0.0)),
        Rule::NoWorse { reference, tolerance } => eval_ref(v, reference, tolerance, |z| z.max(0.0)),
        Rule::AtMost { limit } => {
            if !limit.is_finite() || v > limit {
                RuleVerdict { z: None, score: Some(0.0), broken: true }
            } else {
                RuleVerdict { z: None, score: Some(100.0), broken: false }
            }
        }
        Rule::Within { min, max } => {
            if !min.is_finite() || !max.is_finite() || min > max || v < min || v > max {
                RuleVerdict { z: None, score: Some(0.0), broken: true }
            } else {
                RuleVerdict { z: None, score: Some(100.0), broken: false }
            }
        }
        Rule::Holds { expect } => {
            let ok = (v != 0.0) == expect;
            RuleVerdict {
                z: None,
                score: Some(if ok { 100.0 } else { 0.0 }),
                broken: !ok,
            }
        }
        Rule::Raw => RuleVerdict::default(),
    }
}

fn eval_ref(v: f32, reference: f32, tolerance: f32, shape: impl FnOnce(f32) -> f32) -> RuleVerdict {
    if !reference.is_finite() || !tolerance.is_finite() || tolerance <= 0.0 {
        return RuleVerdict {
            z: None,
            score: Some(0.0),
            broken: true,
        };
    }
    let z = shape((v - reference) / tolerance);
    RuleVerdict {
        z: Some(z),
        score: Some(likeness(z)),
        broken: false,
    }
}

/// Graded metric cell within a scenario.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalCell {
    pub scenario: String,
    pub layer: String,
    pub axis: String,
    pub value: Option<f32>,
    pub rule: Rule,
    pub z: Option<f32>,
    pub score: Option<f32>,
    pub broken: bool,
    pub gate: Option<String>,
    pub holdout: bool,
}

impl EvalCell {
    pub fn evaluate(
        scenario: impl Into<String>,
        layer: impl Into<String>,
        axis: impl Into<String>,
        value: Option<f32>,
        rule: Rule,
        holdout: bool,
    ) -> Self {
        let axis_s = axis.into();
        let v = judge(value, rule);
        let gate = if v.broken {
            Some(match (value, rule) {
                (Some(x), Rule::AtMost { limit }) => format!("{axis_s} {x:.3} > {limit:.3}"),
                (Some(x), Rule::Within { min, max }) => format!("{axis_s} {x:.3} not in [{min:.3}, {max:.3}]"),
                (Some(x), _) if !x.is_finite() => format!("{axis_s} non-finite"),
                (None, _) => format!("{axis_s} missing"),
                _ => format!("{axis_s} gate failed"),
            })
        } else {
            None
        };
        Self {
            scenario: scenario.into(),
            layer: layer.into(),
            axis: axis_s,
            value,
            rule,
            z: v.z,
            score: v.score,
            broken: v.broken,
            gate,
            holdout,
        }
    }
}

/// Aggregated statistics across graded scenario cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenarioSummary {
    pub area: String,
    pub cells: usize,
    pub scored: usize,
    pub broken: usize,
    pub median_abs_z: Option<f32>,
    pub rms_z: Option<f32>,
    pub max_abs_z: Option<f32>,
    pub within1: Option<f32>,
    pub within2: Option<f32>,
    pub mean_score: Option<f32>,
    pub passed: bool,
}

/// Summarizes a slice of [`EvalCell`] items for an evaluation area.
pub fn summarize_scenarios(area: impl Into<String>, cells: &[EvalCell]) -> ScenarioSummary {
    let broken = cells.iter().filter(|c| c.broken).count();
    let scores: Vec<f32> = cells.iter().filter_map(|c| c.score).collect();
    let mean_score = (!scores.is_empty()).then(|| scores.iter().sum::<f32>() / scores.len() as f32);
    let abs_z: Vec<f32> = cells
        .iter()
        .filter(|c| c.rule.is_reference() && c.value.is_some())
        .filter_map(|c| c.z.map(f32::abs))
        .collect();
    let nz = abs_z.len();
    let share = |lim: f32| (nz > 0).then(|| abs_z.iter().filter(|z| **z <= lim).count() as f32 / nz as f32);
    let rms_z = (nz > 0).then(|| (abs_z.iter().map(|z| (*z as f64).powi(2)).sum::<f64>() / nz as f64).sqrt() as f32);
    let max_abs_z = abs_z.iter().copied().reduce(f32::max);
    let within_all = cells
        .iter()
        .all(|c| c.score.is_none() || c.z.map_or(c.score.unwrap_or(0.0) > 0.0, |z| z.abs() <= 2.0));
    ScenarioSummary {
        area: area.into(),
        cells: cells.len(),
        scored: scores.len(),
        broken,
        median_abs_z: median(&abs_z),
        rms_z,
        max_abs_z,
        within1: share(1.0),
        within2: share(2.0),
        mean_score,
        passed: broken == 0 && within_all && !scores.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_boundary_nan_zero_tol_and_raw_missing() {
        // Negative / zero tolerance -> broken = true, score = Some(0.0)
        let v = judge(Some(1.0), Rule::Like { reference: 1.0, tolerance: 0.0 });
        assert!(v.broken && v.score == Some(0.0) && v.z.is_none());
        let v = judge(Some(1.0), Rule::Like { reference: 1.0, tolerance: -0.5 });
        assert!(v.broken && v.score == Some(0.0));

        // Non-finite input -> broken = true, score = Some(0.0)
        let v = judge(Some(f32::NAN), Rule::Raw);
        assert!(v.broken && v.score == Some(0.0));
        let v = judge(Some(f32::INFINITY), Rule::AtMost { limit: 10.0 });
        assert!(v.broken && v.score == Some(0.0));

        // Raw missing -> score = None, broken = false; non-Raw missing -> broken = true, score = Some(0.0)
        let raw_none = judge(None, Rule::Raw);
        assert!(!raw_none.broken && raw_none.score.is_none());
        let req_none = judge(None, Rule::AtMost { limit: 1.0 });
        assert!(req_none.broken && req_none.score == Some(0.0));

        // Within & Holds
        assert!(!judge(Some(0.5), Rule::Within { min: 0.0, max: 1.0 }).broken);
        assert!(judge(Some(1.5), Rule::Within { min: 0.0, max: 1.0 }).broken);
        assert!(!judge(Some(1.0), Rule::Holds { expect: true }).broken);
        assert!(judge(Some(0.0), Rule::Holds { expect: true }).broken);

        // Serde tag stability
        let json = serde_json::to_string(&Rule::NoWorse { reference: 0.5, tolerance: 0.1 }).unwrap();
        assert!(json.contains("\"rule\":\"no_worse\""), "{json}");
        let round: Rule = serde_json::from_str(&json).unwrap();
        assert_eq!(round, Rule::NoWorse { reference: 0.5, tolerance: 0.1 });
    }

    #[test]
    fn summarize_scenarios_works() {
        let cells = vec![
            EvalCell::evaluate("s1", "l1", "err", Some(0.02), Rule::AtMost { limit: 0.05 }, false),
            EvalCell::evaluate("s1", "l1", "acc", Some(0.95), Rule::AtLeast { reference: 0.90, tolerance: 0.05 }, false),
        ];
        let sum = summarize_scenarios("test", &cells);
        assert!(sum.passed);
        assert_eq!((sum.cells, sum.scored, sum.broken), (2, 2, 0));
    }
}
