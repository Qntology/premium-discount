//! Single-source-of-truth vocabulary trimming plan (`VocabPlan`, Phase 2 – Part 1).
//!
//! Shared across offline artifact trimming (`artifact_trim`), runtime output-head slicing
//! (`vocab_trim`), embedding/vision trimming, and reflection audit records.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};

use crate::trim::ALIGN;

pub const VOCAB_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanTier {
    MandatorySpecial,
    MandatoryByte,
    MandatoryAsciiStruct,
    DomainLexicon,
    CorpusFreq,
    MergeClosure,
    AlignPad,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VocabPlan {
    pub schema_version: u32,
    pub model_family: String,
    pub tokenizer_flavor: String,
    pub tokenizer_sha256: String,
    pub corpus_manifest_sha256: String,
    pub leak_blocklist_sha256: String,
    pub lexicon_sha256: String,
    pub bundle: String,
    pub orig_vocab_size: usize,
    pub kept_vocab_size: usize,
    pub orphan_bpe_tokens: usize,
    pub per_lang_coverage: BTreeMap<String, f64>,
    pub per_lang_fertility: BTreeMap<String, f64>,
    pub kept_ids: Vec<u32>,
    pub reasons: Vec<PlanTier>,
    pub plan_sha256: String,
}

impl VocabPlan {
    /// Compute canonical SHA-256 over the plan contents (excluding `plan_sha256` itself).
    pub fn compute_sha256(&self) -> Result<String> {
        let mut clone = self.clone();
        clone.plan_sha256.clear();
        let bytes = serde_json::to_vec(&clone)?;
        let digest = Sha256::digest(&bytes);
        Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// Populate `kept_vocab_size` and `plan_sha256` deterministically.
    pub fn finalize(&mut self) -> Result<()> {
        self.schema_version = VOCAB_PLAN_SCHEMA_VERSION;
        self.kept_vocab_size = self.kept_ids.len();
        self.plan_sha256 = self.compute_sha256()?;
        self.verify()
    }

    /// Validate all structural and integrity invariants of the vocabulary plan.
    pub fn verify(&self) -> Result<()> {
        ensure!(
            self.schema_version == VOCAB_PLAN_SCHEMA_VERSION,
            "unsupported VocabPlan schema_version {}",
            self.schema_version
        );
        ensure!(self.orig_vocab_size > 0, "orig_vocab_size must be > 0");
        ensure!(
            self.kept_vocab_size == self.kept_ids.len(),
            "kept_vocab_size ({}) != kept_ids.len() ({})",
            self.kept_vocab_size,
            self.kept_ids.len()
        );
        ensure!(
            self.reasons.len() == self.kept_ids.len(),
            "reasons.len() ({}) != kept_ids.len() ({})",
            self.reasons.len(),
            self.kept_ids.len()
        );
        ensure!(
            self.kept_vocab_size <= self.orig_vocab_size,
            "kept_vocab_size ({}) > orig_vocab_size ({})",
            self.kept_vocab_size,
            self.orig_vocab_size
        );
        if self.kept_vocab_size < self.orig_vocab_size {
            ensure!(
                self.kept_vocab_size % ALIGN == 0,
                "kept_vocab_size ({}) must be a multiple of {ALIGN}",
                self.kept_vocab_size
            );
        }
        ensure!(
            self.orphan_bpe_tokens == 0,
            "VocabPlan has {} orphan BPE tokens (must be 0)",
            self.orphan_bpe_tokens
        );
        for w in self.kept_ids.windows(2) {
            if w[0] >= w[1] {
                bail!("kept_ids must be strictly ascending: {} >= {}", w[0], w[1]);
            }
        }
        if let Some(&last) = self.kept_ids.last() {
            ensure!(
                (last as usize) < self.orig_vocab_size,
                "kept_id {last} >= orig_vocab_size {}",
                self.orig_vocab_size
            );
        }
        let expected = self.compute_sha256()?;
        ensure!(
            self.plan_sha256.eq_ignore_ascii_case(&expected),
            "plan_sha256 mismatch: stored={} computed={expected}",
            self.plan_sha256
        );
        Ok(())
    }

    pub fn tier_counts(&self) -> BTreeMap<PlanTier, usize> {
        let mut counts = BTreeMap::new();
        for &r in &self.reasons {
            *counts.entry(r).or_insert(0) += 1;
        }
        counts
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.verify()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let plan: Self = serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        plan.verify()?;
        Ok(plan)
    }
}

/// Helper to compute hex SHA-256 of arbitrary bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_plan_verify_and_roundtrip() {
        let mut plan = VocabPlan {
            schema_version: 1,
            model_family: "qwen3_5".into(),
            tokenizer_flavor: "gpt2_byte_bpe".into(),
            tokenizer_sha256: sha256_hex(b"tok"),
            corpus_manifest_sha256: sha256_hex(b"manifest"),
            leak_blocklist_sha256: sha256_hex(b"leak"),
            lexicon_sha256: sha256_hex(b"lex"),
            bundle: "ko+en".into(),
            orig_vocab_size: 256,
            kept_vocab_size: 64,
            orphan_bpe_tokens: 0,
            per_lang_coverage: BTreeMap::from([("ko".into(), 0.998), ("en".into(), 0.994)]),
            per_lang_fertility: BTreeMap::from([("ko".into(), 1.01), ("en".into(), 1.02)]),
            kept_ids: (0..64).collect(),
            reasons: vec![PlanTier::MandatoryByte; 64],
            plan_sha256: String::new(),
        };
        plan.finalize().unwrap();
        let tmp = std::env::temp_dir().join(format!("pd_vocab_plan_{}.json", std::process::id()));
        plan.save(&tmp).unwrap();
        let loaded = VocabPlan::load(&tmp).unwrap();
        assert_eq!(loaded, plan);
        let _ = std::fs::remove_file(&tmp);

        // Tamper detection
        let mut bad = plan.clone();
        bad.kept_ids[0] = 5;
        assert!(bad.verify().is_err());

        // Non-64-aligned rejection
        let mut bad_align = plan.clone();
        bad_align.kept_ids.pop();
        bad_align.reasons.pop();
        assert!(bad_align.finalize().is_err());
    }
}
