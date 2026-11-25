//! Cross-ecosystem engine tree hygiene audit using `premium-discount-eval::hygiene`.

use std::path::{Path, PathBuf};

use premium_discount_eval::hygiene::{scan_engine_tree, HygieneFinding};

const FORBIDDEN_BENCH_IDENTS: &[&str] = &[
    "nn_scenarios",
    "ScreenSpot",
    "OmniDocBench",
    "OSWorld",
    "Mind2Web",
    "NYUv2",
    "ScanNet",
];

/// Audits `lv-core`, `lv-screen`, `lv-doc`, and `lv-spatial` source trees under `lv_root` for
/// benchmark contamination literals.
pub fn audit_louis_vision_crates(lv_root: &Path) -> anyhow::Result<Vec<HygieneFinding>> {
    let mut findings = Vec::new();
    for krate in ["lv-core", "lv-screen", "lv-doc", "lv-spatial"] {
        let src = lv_root.join("crates").join(krate).join("src");
        if src.exists() {
            findings.extend(scan_engine_tree(&src, FORBIDDEN_BENCH_IDENTS, &["rs"])?);
        }
    }
    Ok(findings)
}

pub fn sibling_louis_vision_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("louis-vision")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn louis_vision_engine_trees_contain_zero_benchmark_contamination() {
        let lv = sibling_louis_vision_root();
        if !lv.join("Cargo.toml").exists() {
            // Standalone checkout without sibling louis-vision repository.
            return;
        }
        let findings = audit_louis_vision_crates(&lv).unwrap();
        assert!(findings.is_empty(), "hygiene findings in louis-vision: {findings:?}");
    }
}
