//! Source tree identifier hygiene scan and LF-normalized contract SHA-256 verification.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HygieneFinding {
    pub path: PathBuf,
    pub line: usize,
    pub ident: String,
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Checks whether `needle` appears in `haystack` bounded on both sides by non-identifier characters.
///
/// Prevents false-positive substring hits such as `"Johanson"` matching `"Hanson"`.
pub fn contains_ident_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    for (idx, _) in haystack.match_indices(needle) {
        let left_ok = haystack[..idx].chars().next_back().is_none_or(|c| !is_ident_char(c));
        let right_idx = idx + needle.len();
        let right_ok = haystack[right_idx..].chars().next().is_none_or(|c| !is_ident_char(c));
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Recursively scans files matching `exts` under `root` for bounded identifier hits in `forbidden`.
pub fn scan_engine_tree(root: &Path, forbidden: &[&str], exts: &[&str]) -> Result<Vec<HygieneFinding>> {
    let mut out = Vec::new();
    scan_dir(root, forbidden, exts, &mut out)?;
    Ok(out)
}

fn scan_dir(dir: &Path, forbidden: &[&str], exts: &[&str], out: &mut Vec<HygieneFinding>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(name, "target" | ".git" | "node_modules") {
                continue;
            }
            scan_dir(&path, forbidden, exts, out)?;
        } else if path.is_file() {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !exts.is_empty() && !exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
                continue;
            }
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            for (idx, line) in text.lines().enumerate() {
                for &ident in forbidden {
                    if contains_ident_bounded(line, ident) {
                        out.push(HygieneFinding {
                            path: path.clone(),
                            line: idx + 1,
                            ident: ident.to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Computes lowercase hex SHA-256 after normalizing CRLF (`\r\n`) line endings to LF (`\n`).
pub fn sha256_lf_normalized(bytes: &[u8]) -> String {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            i += 2;
        } else {
            normalized.push(bytes[i]);
            i += 1;
        }
    }
    let mut h = Sha256::new();
    h.update(&normalized);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Verifies `primary` (and optional `mirror`) SHA-256 after LF normalization against `expected_hex`.
pub fn verify_contract_sha256(primary: &Path, mirror: Option<&Path>, expected_hex: &str) -> Result<()> {
    let expected = expected_hex.trim().to_ascii_lowercase();
    let p_bytes = std::fs::read(primary).with_context(|| format!("reading {}", primary.display()))?;
    let p_hash = sha256_lf_normalized(&p_bytes);
    if p_hash != expected {
        bail!("contract sha256 mismatch at {}: got {p_hash}, expected {expected}", primary.display());
    }
    if let Some(m_path) = mirror {
        let m_bytes = std::fs::read(m_path).with_context(|| format!("reading mirror {}", m_path.display()))?;
        let m_hash = sha256_lf_normalized(&m_bytes);
        if m_hash != expected {
            bail!("mirror sha256 mismatch at {}: got {m_hash}, expected {expected}", m_path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_boundary_ignores_substrings_and_catches_exact_tokens() {
        assert!(!contains_ident_bounded("customer Johanson submitted form", "Hanson"));
        assert!(!contains_ident_bounded("Johanson_id = 42", "Hanson"));
        assert!(contains_ident_bounded("let client = Hanson;", "Hanson"));
        assert!(contains_ident_bounded("Hanson company", "Hanson"));
        assert!(contains_ident_bounded("name=\"Hanson\"", "Hanson"));
    }

    #[test]
    fn crlf_and_lf_hash_identically() {
        let tmp = std::env::temp_dir().join(format!("pd-eval-hygiene-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let lf = tmp.join("primary.json");
        let crlf = tmp.join("mirror.json");
        std::fs::write(&lf, b"{\n  \"k\": 1\n}\n").unwrap();
        std::fs::write(&crlf, b"{\r\n  \"k\": 1\r\n}\r\n").unwrap();
        let hex = sha256_lf_normalized(b"{\n  \"k\": 1\n}\n");
        verify_contract_sha256(&lf, Some(&crlf), &hex).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
