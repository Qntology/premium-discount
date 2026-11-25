//! Universal model-free evaluation library for the Qntology ecosystem (`premium-discount`,
//! `louis-vision`, `logis-center`, and `noto-sandbox`).

pub mod accel_tol;
pub mod calib;
pub mod gate_ab;
#[cfg(feature = "hygiene")]
pub mod hygiene;
pub mod ranking;
pub mod rule;

pub use accel_tol::{verify_tensor_parity, RelErrorReport, ToleranceTier};
pub use calib::{
    auroc, average_precision, ece, fit_label, platt_fit, platt_fit_or_prior, threshold_for_fpr,
    threshold_for_precision, CalibTable, LabelCalib, ECE_BINS,
};
pub use gate_ab::{compare_gate_arms, ArmRunSummary, DetectionCounters, GateAbVerdict};
#[cfg(feature = "hygiene")]
pub use hygiene::{contains_ident_bounded, scan_engine_tree, verify_contract_sha256, HygieneFinding};
pub use ranking::{
    cer_char_level, check_e3_non_inferiority, check_expansion_and_non_reversal, effective_ms_1,
    group_means, mrr, ndcg_at_k, paired_bootstrap_ci95, recall_at_k, EquivalenceTier, GateExit,
    Polarity, SplitMix64,
};
pub use rule::{judge, likeness, median, sd, summarize_scenarios, EvalCell, Rule, RuleVerdict, ScenarioSummary, SdMode};
