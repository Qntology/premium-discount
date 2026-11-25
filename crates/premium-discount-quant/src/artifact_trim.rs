//! Trimmed artifact builder: ID remapping, reachability-guaranteed tokenizer pruning
//! (tree BPE, DAG BPE, SentencePiece Unigram), config synchronization, and SHA-256 manifest
//! verification (Phase 2 – Part 2).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use premium_discount_manifest as manifest;
use serde_json::Value;

use crate::trim::{detect_flavor_from_json, is_placeholder_token, TokenizerFlavor, ALIGN};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberingMode {
    Contiguous,
    SparseMapped,
}

/// Bidirectional token ID mapping between original vocabulary `[0..V)` and trimmed `[0..K)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdRemap {
    pub mode: NumberingMode,
    pub orig_vocab: usize,
    /// Length `K`, strictly ascending original IDs.
    pub new_to_old: Vec<u32>,
    /// Length `V`, `-1` for dropped rows, `0..K-1` for kept rows.
    pub old_to_new: Vec<i32>,
}

impl IdRemap {
    pub fn from_kept_ids(orig_vocab: usize, kept_ids: &[u32], mode: NumberingMode) -> Result<Self> {
        ensure!(orig_vocab > 0, "orig_vocab must be > 0");
        ensure!(!kept_ids.is_empty(), "kept_ids must not be empty");
        ensure!(kept_ids.len() <= orig_vocab, "kept_ids > orig_vocab");
        for w in kept_ids.windows(2) {
            if w[0] >= w[1] {
                bail!("IdRemap kept_ids must be strictly ascending: {} >= {}", w[0], w[1]);
            }
        }
        let mut old_to_new = vec![-1i32; orig_vocab];
        for (new_id, &old_id) in kept_ids.iter().enumerate() {
            ensure!(
                (old_id as usize) < orig_vocab,
                "IdRemap old_id {old_id} >= orig_vocab {orig_vocab}"
            );
            old_to_new[old_id as usize] = new_id as i32;
        }
        let remap = Self {
            mode,
            orig_vocab,
            new_to_old: kept_ids.to_vec(),
            old_to_new,
        };
        remap.verify_bijection()?;
        Ok(remap)
    }

    /// Construct a bijective `IdRemap` preserving the exact caller-supplied token ID ordering
    /// (for priority-bucketed artifacts where lower IDs sit inside runtime `K <= 12,288`).
    pub fn from_ordered_ids(orig_vocab: usize, ordered_ids: &[u32], mode: NumberingMode) -> Result<Self> {
        ensure!(orig_vocab > 0, "orig_vocab must be > 0");
        ensure!(!ordered_ids.is_empty(), "ordered_ids must not be empty");
        ensure!(ordered_ids.len() <= orig_vocab, "ordered_ids > orig_vocab");
        let mut old_to_new = vec![-1i32; orig_vocab];
        for (new_id, &old_id) in ordered_ids.iter().enumerate() {
            ensure!(
                (old_id as usize) < orig_vocab,
                "IdRemap old_id {old_id} >= orig_vocab {orig_vocab}"
            );
            ensure!(
                old_to_new[old_id as usize] == -1,
                "IdRemap duplicate old_id {old_id}"
            );
            old_to_new[old_id as usize] = new_id as i32;
        }
        let remap = Self {
            mode,
            orig_vocab,
            new_to_old: ordered_ids.to_vec(),
            old_to_new,
        };
        remap.verify_bijection()?;
        Ok(remap)
    }

    pub fn trimmed_vocab(&self) -> usize {
        self.new_to_old.len()
    }

    pub fn map_old(&self, old_id: u32) -> Option<u32> {
        self.old_to_new
            .get(old_id as usize)
            .copied()
            .filter(|&v| v >= 0)
            .map(|v| v as u32)
    }

    pub fn verify_bijection(&self) -> Result<()> {
        ensure!(self.old_to_new.len() == self.orig_vocab, "old_to_new len mismatch");
        let k = self.new_to_old.len();
        let mut mapped_count = 0usize;
        for (new_id, &old_id) in self.new_to_old.iter().enumerate() {
            ensure!((old_id as usize) < self.orig_vocab, "old_id out of bounds");
            ensure!(
                self.old_to_new[old_id as usize] == new_id as i32,
                "bijection broken at new={new_id} old={old_id}"
            );
        }
        for &n in &self.old_to_new {
            if n >= 0 {
                ensure!((n as usize) < k, "mapped id {n} >= K {k}");
                mapped_count += 1;
            }
        }
        ensure!(mapped_count == k, "mapped_count {mapped_count} != K {k}");
        Ok(())
    }

    pub fn save_orig_ids_bin(&self, path: &Path) -> Result<()> {
        let mut buf = Vec::with_capacity(self.new_to_old.len() * 4);
        for &id in &self.new_to_old {
            buf.extend_from_slice(&id.to_le_bytes());
        }
        std::fs::write(path, buf).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn load_orig_ids_bin(path: &Path, orig_vocab: usize) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        ensure!(bytes.len() % 4 == 0, "orig_ids.bin length not multiple of 4");
        let kept: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Self::from_ordered_ids(orig_vocab, &kept, NumberingMode::Contiguous)
    }
}

/// Returns `true` for ASCII multi-byte programming-code syntax, multi-space indentation,
/// or camelCase tokens placed into Tier 3 so runtime `K <= 12,288` keeps natural-language prose.
pub fn is_code_syntax_noise(piece: &[u8]) -> bool {
    if piece.len() <= 1 {
        return false;
    }
    let Ok(s) = std::str::from_utf8(piece) else {
        return false;
    };
    if !s.is_ascii() {
        return false;
    }
    const ALLOW: &[&str] = &[
        "{\"", "\":", ": ", ", ", "\"}", "\"\n", "\"}\n", "\"},\n", "\"},", "{\n", "}\n", "\n\n",
        "[{", "}]", "```", "  ", " \"", "\",", "\"", "\": \"", "\", \"", " {\n", " }\n", ":\n",
        ".\n", ",\n", ". ", "- ", " -", " /", "/ ", "\" ", " '", "'s", "'t", "'re", "'ve",
        "'ll", "'d", "'m",
    ];
    if ALLOW.contains(&s) {
        return false;
    }
    if s.contains('\t')
        || s.contains('\r')
        || s.contains('\n')
        || (s.trim().is_empty() && s.len() >= 3)
    {
        return true;
    }
    if s.bytes().any(|b| {
        matches!(
            b,
            b';' | b'{'
                | b'}'
                | b'['
                | b']'
                | b'('
                | b')'
                | b'<'
                | b'>'
                | b'\\'
                | b'|'
                | b'~'
                | b'^'
                | b'`'
                | b'_'
                | b'='
                | b'*'
                | b'!'
                | b'@'
                | b'#'
                | b'$'
                | b'&'
                | b'+'
                | b'?'
        )
    }) {
        return true;
    }
    if s.contains("//")
        || s.contains("--")
        || s.contains("..")
        || s.contains("::")
        || s.contains("''")
        || s.contains("\"\"")
    {
        return true;
    }
    let core = s.strip_prefix(' ').unwrap_or(s);
    if core.is_empty() {
        return false;
    }
    if core.bytes().all(|b| b.is_ascii_alphabetic()) {
        if core
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
        {
            return true;
        }
        return false;
    }
    if core.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrimTokenizerStats {
    pub flavor: String,
    pub orig_vocab: usize,
    pub kept_vocab: usize,
    pub orig_merges: usize,
    pub kept_merges: usize,
    pub ancestors_added: usize,
    pub orphan_multi: usize,
}

fn parse_merge_entry(m: &Value) -> Option<(String, String, bool)> {
    if let Some(s) = m.as_str() {
        s.split_once(' ')
            .map(|(a, b)| (a.to_string(), b.to_string(), false))
    } else if let Some(arr) = m.as_array() {
        if arr.len() == 2 {
            arr[0]
                .as_str()
                .zip(arr[1].as_str())
                .map(|(a, b)| (a.to_string(), b.to_string(), true))
        } else {
            None
        }
    } else {
        None
    }
}

fn reachable_bpe(base: &HashSet<u32>, merges: &[(u32, u32, u32)], kept: &HashSet<u32>) -> HashSet<u32> {
    let mut reach: HashSet<u32> = base.iter().copied().filter(|id| kept.contains(id)).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &(a, b, ab) in merges {
            if kept.contains(&ab) && reach.contains(&a) && reach.contains(&b) && reach.insert(ab) {
                changed = true;
            }
        }
    }
    reach
}

/// Prune a Hugging Face `tokenizer.json` to `kept_ids` (re-numbered contiguously `0..K-1`),
/// enforcing BPE ancestor reachability (`orphan_multi == 0` when `ignore_merges == false`),
/// preserving Unigram log-probabilities, and updating `added_tokens`, `post_processor`, and
/// `padding.pad_id`.
pub fn trim_tokenizer_json(
    raw_json: &str,
    orig_vocab: usize,
    kept_ids: &[u32],
    ancestor_max: usize,
) -> Result<(String, IdRemap, TrimTokenizerStats)> {
    trim_tokenizer_json_impl(raw_json, orig_vocab, kept_ids, ancestor_max, None)
}

/// Priority-ordered variant of [`trim_tokenizer_json`] placing single bytes (`Tier 0`),
/// structural JSON/doc tokens (`Tier 1`), natural-language prose & Core-12 scripts (`Tier 2`),
/// programming-code syntax noise (`Tier 3`), and added special tokens (`Tier 4`) in contiguous bands
/// while preserving original merge order and `orphan_multi == 0`.
pub fn trim_tokenizer_json_prioritized(
    raw_json: &str,
    orig_vocab: usize,
    kept_ids: &[u32],
    ancestor_max: usize,
    struct_priority_ids: &BTreeSet<u32>,
) -> Result<(String, IdRemap, TrimTokenizerStats)> {
    trim_tokenizer_json_impl(
        raw_json,
        orig_vocab,
        kept_ids,
        ancestor_max,
        Some(struct_priority_ids),
    )
}

fn trim_tokenizer_json_impl(
    raw_json: &str,
    orig_vocab: usize,
    kept_ids: &[u32],
    ancestor_max: usize,
    struct_priority_ids: Option<&BTreeSet<u32>>,
) -> Result<(String, IdRemap, TrimTokenizerStats)> {
    let mut doc: Value = serde_json::from_str(raw_json).context("parse tokenizer.json")?;
    let flavor = detect_flavor_from_json(&doc);
    let mut kept_set: BTreeSet<u32> = kept_ids.iter().copied().collect();
    let target_k = kept_set.len();
    let mut ancestors_added = 0usize;
    let mut orphan_multi = 0usize;

    if flavor == TokenizerFlavor::SentencePieceUnigram {
        // SentencePiece Unigram: preserve unk_id, single-char/byte pieces, and log-prob scores.
        let unk_id = doc["model"]["unk_id"].as_u64().map(|x| x as u32);
        if let Some(u) = unk_id {
            kept_set.insert(u);
        }
        let final_ids: Vec<u32> = kept_set.iter().copied().collect();
        let remap = IdRemap::from_kept_ids(orig_vocab, &final_ids, NumberingMode::Contiguous)?;
        if let Some(arr) = doc["model"]["vocab"].as_array().cloned() {
            let new_vocab: Vec<Value> = remap
                .new_to_old
                .iter()
                .filter_map(|&old| arr.get(old as usize).cloned())
                .collect();
            doc["model"]["vocab"] = Value::Array(new_vocab);
        }
        if let Some(u) = unk_id {
            let nu = remap.map_old(u).context("unk_id dropped during Unigram trim")?;
            doc["model"]["unk_id"] = Value::from(nu);
        }
        remap_aux_tokenizer_fields(&mut doc, &remap)?;
        let out = serde_json::to_string(&doc)?;
        return Ok((
            out,
            remap,
            TrimTokenizerStats {
                flavor: flavor.as_str().into(),
                orig_vocab,
                kept_vocab: final_ids.len(),
                orig_merges: 0,
                kept_merges: 0,
                ancestors_added: 0,
                orphan_multi: 0,
            },
        ));
    }

    // BPE path (Tree BPE for Qwen3/Qwen3.5 and DAG BPE for Granite/SigLIP2)
    let vocab_obj = doc["model"]["vocab"]
        .as_object()
        .context("BPE tokenizer missing model.vocab object")?
        .clone();
    let mut id_to_tok: HashMap<u32, String> = HashMap::with_capacity(vocab_obj.len());
    let mut tok_to_id: HashMap<String, u32> = HashMap::with_capacity(vocab_obj.len());
    let mut base_tokens: HashSet<u32> = HashSet::new();
    for (tok, v) in &vocab_obj {
        if let Some(id) = v.as_u64() {
            let id = id as u32;
            id_to_tok.insert(id, tok.clone());
            tok_to_id.insert(tok.clone(), id);
            let is_single_char = tok.chars().count() == 1
                || (tok.len() == 6 && tok.starts_with("<0x") && tok.ends_with('>'))
                || tok == "\t";
            if is_single_char {
                base_tokens.insert(id);
            }
        }
    }
    let mut added_ids: HashSet<u32> = HashSet::new();
    if let Some(arr) = doc["added_tokens"].as_array() {
        for a in arr {
            if let Some(id) = a["id"].as_u64() {
                let content = a["content"].as_str().unwrap_or("");
                if !is_placeholder_token(content) {
                    added_ids.insert(id as u32);
                }
            }
        }
    }

    let raw_merges = doc["model"]["merges"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let orig_merges = raw_merges.len();
    let mut is_array_merge = false;
    let mut merges_triples: Vec<(u32, u32, u32)> = Vec::with_capacity(raw_merges.len());
    let mut parents_by_product: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    for m in &raw_merges {
        if let Some((a, b, arr_fmt)) = parse_merge_entry(m) {
            is_array_merge |= arr_fmt;
            if let (Some(&aid), Some(&bid)) = (tok_to_id.get(&a), tok_to_id.get(&b)) {
                let mut ab = String::with_capacity(a.len() + b.len());
                ab.push_str(&a);
                ab.push_str(&b);
                if let Some(&abid) = tok_to_id.get(&ab) {
                    merges_triples.push((aid, bid, abid));
                    parents_by_product.entry(abid).or_default().push((aid, bid));
                }
            }
        }
    }

    // Tokens that are single-char or added or never appear as a merge product are base tokens.
    let mut all_base = base_tokens.clone();
    all_base.extend(added_ids.iter().copied());

    // Reachability closure: ensure multi-char kept tokens have an unbroken ancestor chain.
    // For DAG BPE (multiple parent pairs), pick the parent pair with minimum missing ancestors.
    let protected: HashSet<u32> = all_base.clone();
    let mut kept_hash: HashSet<u32> = kept_set.iter().copied().collect();

    // Drop any multi-char tokens that have no merge rule in the source tokenizer (upstream Qwen3.5 quirks).
    let raw_orphans: Vec<u32> = kept_set
        .iter()
        .copied()
        .filter(|id| !all_base.contains(id) && !parents_by_product.contains_key(id))
        .collect();
    for id in raw_orphans {
        kept_set.remove(&id);
        kept_hash.remove(&id);
    }

    // Iteratively resolve unreachable tokens (up to 4 passes for chained ancestors)
    for _ in 0..4 {
        let reach = reachable_bpe(&all_base, &merges_triples, &kept_hash);
        let unreachable: Vec<u32> = kept_set
            .iter()
            .copied()
            .filter(|id| !reach.contains(id) && parents_by_product.contains_key(id))
            .collect();
        if unreachable.is_empty() {
            break;
        }
        let mut to_add: Vec<u32> = Vec::new();
        let mut to_drop: Vec<u32> = Vec::new();
        for id in unreachable {
            // Find minimal missing ancestor set across available parent paths (DAG min-path)
            let mut best_missing: Option<Vec<u32>> = None;
            if let Some(pairs) = parents_by_product.get(&id) {
                for &(a, b) in pairs {
                    let mut missing = Vec::new();
                    let mut stack = vec![a, b];
                    let mut visited = HashSet::new();
                    let mut exceeded = false;
                    while let Some(cur) = stack.pop() {
                        if kept_hash.contains(&cur) || reach.contains(&cur) || !visited.insert(cur) {
                            continue;
                        }
                        missing.push(cur);
                        if missing.len() > ancestor_max {
                            exceeded = true;
                            break;
                        }
                        if let Some(sub_pairs) = parents_by_product.get(&cur) {
                            if let Some(&(sa, sb)) = sub_pairs.first() {
                                stack.push(sa);
                                stack.push(sb);
                            }
                        }
                    }
                    if !exceeded {
                        if best_missing.as_ref().is_none_or(|m| missing.len() < m.len()) {
                            best_missing = Some(missing);
                        }
                    }
                }
            }
            if let Some(missing) = best_missing {
                to_add.extend(missing);
            } else {
                // Exceeded ancestor_max -> drop this unreachable tail token
                to_drop.push(id);
            }
        }
        for id in to_drop {
            if !protected.contains(&id) {
                kept_set.remove(&id);
                kept_hash.remove(&id);
            }
        }
        for id in to_add {
            if kept_set.insert(id) {
                kept_hash.insert(id);
                ancestors_added += 1;
            }
        }
    }

    // Re-align back to target_k (multiple of ALIGN) by trimming highest-ID non-protected leaves
    // or padding with reachable lowest-ID tokens.
    while kept_set.len() > target_k {
        // Build set of tokens needed as parents of other kept tokens
        let mut needed_as_parent: HashSet<u32> = HashSet::new();
        for &(a, b, ab) in &merges_triples {
            if kept_hash.contains(&ab) && kept_hash.contains(&a) && kept_hash.contains(&b) {
                needed_as_parent.insert(a);
                needed_as_parent.insert(b);
            }
        }
        let victim = kept_set
            .iter()
            .rev()
            .copied()
            .find(|id| !protected.contains(id) && !needed_as_parent.contains(id));
        match victim {
            Some(v) => {
                kept_set.remove(&v);
                kept_hash.remove(&v);
            }
            None => break,
        }
    }
    let aligned_target = target_k.next_multiple_of(ALIGN).min(orig_vocab);
    if kept_set.len() < aligned_target {
        let mut reach = reachable_bpe(&all_base, &merges_triples, &kept_hash);
        for &(a, b, ab) in &merges_triples {
            if kept_set.len() >= aligned_target {
                break;
            }
            if !kept_hash.contains(&ab) && reach.contains(&a) && reach.contains(&b) {
                kept_set.insert(ab);
                kept_hash.insert(ab);
                reach.insert(ab);
            }
        }
        for id in 0..orig_vocab as u32 {
            if kept_set.len() >= aligned_target {
                break;
            }
            if !kept_hash.contains(&id) && all_base.contains(&id) {
                kept_set.insert(id);
                kept_hash.insert(id);
            }
        }
    }

    // Final reachability audit
    let final_reach = reachable_bpe(&all_base, &merges_triples, &kept_hash);
    for &id in &kept_set {
        if !final_reach.contains(&id) && parents_by_product.contains_key(&id) {
            orphan_multi += 1;
        }
    }

    let final_ids: Vec<u32> = if let Some(prio) = struct_priority_ids {
        let byte_dec = crate::vocab::byte_decoder();
        let mut tier0 = Vec::new();
        let mut tier1 = Vec::new();
        let mut tier2 = Vec::new();
        let mut tier3 = Vec::new();
        let mut tier4 = Vec::new();
        for &id in &kept_set {
            if added_ids.contains(&id) {
                tier4.push(id);
            } else if base_tokens.contains(&id) {
                tier0.push(id);
            } else if prio.contains(&id) {
                tier1.push(id);
            } else {
                let piece = id_to_tok
                    .get(&id)
                    .map(|t| crate::trim::decode_piece_for_flavor(t, flavor, &byte_dec))
                    .unwrap_or_default();
                if is_code_syntax_noise(&piece) {
                    tier3.push(id);
                } else {
                    tier2.push(id);
                }
            }
        }
        tier0.extend(tier1);
        tier0.extend(tier2);
        tier0.extend(tier3);
        tier0.extend(tier4);
        tier0
    } else {
        kept_set.iter().copied().collect()
    };
    let remap = if struct_priority_ids.is_some() {
        IdRemap::from_ordered_ids(orig_vocab, &final_ids, NumberingMode::Contiguous)?
    } else {
        IdRemap::from_kept_ids(orig_vocab, &final_ids, NumberingMode::Contiguous)?
    };

    // Rebuild model.vocab preserving original ID order (0..K-1)
    let mut new_vocab = serde_json::Map::with_capacity(final_ids.len());
    for (new_id, &old_id) in remap.new_to_old.iter().enumerate() {
        if let Some(tok) = id_to_tok.get(&old_id) {
            new_vocab.insert(tok.clone(), Value::from(new_id as u64));
        }
    }
    doc["model"]["vocab"] = Value::Object(new_vocab);

    // Rebuild model.merges keeping exact format and relative rank order
    let mut new_merges: Vec<Value> = Vec::new();
    for &(a, b, ab) in &merges_triples {
        if kept_hash.contains(&a) && kept_hash.contains(&b) && kept_hash.contains(&ab) {
            if let (Some(sa), Some(sb)) = (id_to_tok.get(&a), id_to_tok.get(&b)) {
                if is_array_merge {
                    new_merges.push(Value::Array(vec![Value::String(sa.clone()), Value::String(sb.clone())]));
                } else {
                    new_merges.push(Value::String(format!("{sa} {sb}")));
                }
            }
        }
    }
    let kept_merges = new_merges.len();
    doc["model"]["merges"] = Value::Array(new_merges);

    remap_aux_tokenizer_fields(&mut doc, &remap)?;

    let out = serde_json::to_string(&doc)?;
    Ok((
        out,
        remap,
        TrimTokenizerStats {
            flavor: flavor.as_str().into(),
            orig_vocab,
            kept_vocab: final_ids.len(),
            orig_merges,
            kept_merges,
            ancestors_added,
            orphan_multi,
        },
    ))
}

fn remap_aux_tokenizer_fields(doc: &mut Value, remap: &IdRemap) -> Result<()> {
    // 1. added_tokens
    if let Some(arr) = doc.get_mut("added_tokens").and_then(|v| v.as_array_mut()) {
        let mut kept_added = Vec::with_capacity(arr.len());
        for mut item in arr.drain(..) {
            if let Some(old_id) = item.get("id").and_then(|v| v.as_u64()) {
                if let Some(new_id) = remap.map_old(old_id as u32) {
                    item["id"] = Value::from(new_id as u64);
                    kept_added.push(item);
                }
            }
        }
        *arr = kept_added;
    }

    // 2. padding.pad_id
    if let Some(pad_obj) = doc.get_mut("padding").and_then(|v| v.as_object_mut()) {
        if let Some(old_pad) = pad_obj.get("pad_id").and_then(|v| v.as_u64()) {
            if let Some(new_pad) = remap.map_old(old_pad as u32) {
                pad_obj.insert("pad_id".into(), Value::from(new_pad as u64));
            }
        }
    }

    // 3. post_processor special_tokens[*].ids & single/pair template IDs
    if let Some(pp) = doc.get_mut("post_processor") {
        remap_post_processor_value(pp, remap)?;
    }
    Ok(())
}

fn remap_post_processor_value(v: &mut Value, remap: &IdRemap) -> Result<()> {
    match v {
        Value::Object(map) => {
            if let Some(st) = map.get_mut("special_tokens").and_then(|x| x.as_object_mut()) {
                for (_name, entry) in st.iter_mut() {
                    if let Some(ids) = entry.get_mut("ids").and_then(|x| x.as_array_mut()) {
                        for id_v in ids.iter_mut() {
                            if let Some(old) = id_v.as_u64() {
                                let new_id = remap
                                    .map_old(old as u32)
                                    .with_context(|| format!("post_processor special token {old} dropped"))?;
                                *id_v = Value::from(new_id as u64);
                            }
                        }
                    }
                }
            }
            if let Some(cls) = map.get_mut("cls").and_then(|x| x.as_array_mut()) {
                if cls.len() == 2 {
                    if let Some(old) = cls[1].as_u64() {
                        let new_id = remap.map_old(old as u32).context("post_processor cls dropped")?;
                        cls[1] = Value::from(new_id as u64);
                    }
                }
            }
            if let Some(sep) = map.get_mut("sep").and_then(|x| x.as_array_mut()) {
                if sep.len() == 2 {
                    if let Some(old) = sep[1].as_u64() {
                        let new_id = remap.map_old(old as u32).context("post_processor sep dropped")?;
                        sep[1] = Value::from(new_id as u64);
                    }
                }
            }
            if let Some(procs) = map.get_mut("processors").and_then(|x| x.as_array_mut()) {
                for p in procs.iter_mut() {
                    remap_post_processor_value(p, remap)?;
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                remap_post_processor_value(item, remap)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Remap `config.json` / `generation_config.json` vocabulary sizes and special/vision token IDs,
/// failing if any active special token ID falls outside `[0..K)`.
pub fn remap_config_json(raw_json: &str, remap: &IdRemap) -> Result<String> {
    let mut doc: Value = serde_json::from_str(raw_json).context("parse config.json")?;
    let k = remap.trimmed_vocab();
    let id_keys = [
        "bos_token_id",
        "eos_token_id",
        "pad_token_id",
        "unk_token_id",
        "cls_token_id",
        "sep_token_id",
        "image_token_id",
        "video_token_id",
        "vision_start_token_id",
        "vision_end_token_id",
    ];
    let remap_section = |obj: &mut serde_json::Map<String, Value>| -> Result<()> {
        if obj.contains_key("vocab_size") {
            obj.insert("vocab_size".into(), Value::from(k as u64));
        }
        for key in id_keys {
            let Some(val) = obj.get_mut(key) else { continue };
            match val {
                Value::Null => {}
                Value::Number(n) => {
                    if let Some(old) = n.as_u64() {
                        let new_id = remap
                            .map_old(old as u32)
                            .with_context(|| format!("config special token {key}={old} not in kept set"))?;
                        ensure!((new_id as usize) < k, "{key}={new_id} >= K={k}");
                        *val = Value::from(new_id as u64);
                    }
                }
                Value::Array(arr) => {
                    for item in arr.iter_mut() {
                        if let Some(old) = item.as_u64() {
                            let new_id = remap
                                .map_old(old as u32)
                                .with_context(|| format!("config special token {key}[]={old} not in kept set"))?;
                            ensure!((new_id as usize) < k, "{key}[]={new_id} >= K={k}");
                            *item = Value::from(new_id as u64);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    };

    if let Some(root) = doc.as_object_mut() {
        remap_section(root)?;
        if let Some(tc) = root.get_mut("text_config").and_then(|v| v.as_object_mut()) {
            remap_section(tc)?;
        }
    }
    Ok(serde_json::to_string_pretty(&doc)?)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManifestFileEntry {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TrimManifest {
    pub schema_version: u32,
    pub base_repo: String,
    pub base_revision: String,
    pub vocab_plan_sha256: String,
    pub lang_bundle: String,
    pub numbering_mode: NumberingMode,
    pub orig_vocab_size: usize,
    pub trimmed_vocab_size: usize,
    pub orphan_multi_count: usize,
    pub space_tag: Option<String>,
    pub special_tokens: BTreeMap<String, [u32; 2]>,
    pub files: Vec<ManifestFileEntry>,
}

impl TrimManifest {
    pub fn build_file_entries(dir: &Path, rel_names: &[&str]) -> Result<Vec<ManifestFileEntry>> {
        let mut entries = Vec::with_capacity(rel_names.len());
        for &name in rel_names {
            manifest::safe_relative(name).map_err(|e| anyhow::anyhow!(e))?;
            let p = dir.join(name);
            let size = std::fs::metadata(&p)
                .with_context(|| format!("stat {}", p.display()))?
                .len();
            let sha256 = manifest::sha256_file(&p, 0)
                .with_context(|| format!("sha256 {}", p.display()))?;
            entries.push(ManifestFileEntry {
                name: name.to_string(),
                size,
                sha256,
            });
        }
        Ok(entries)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join("trim_manifest.json");
        std::fs::write(&path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("trim_manifest.json");
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }

    /// Verify all files listed in the manifest.
    /// `full == true`: full SHA-256 of every file (required on promotion).
    /// `full == false` (`fast` startup check): exact byte size for all files + full SHA-256 for
    /// metadata/JSON/`orig_ids.bin` (< 64 MiB), ensuring corrupted files are always rejected.
    pub fn verify_dir(&self, dir: &Path, full: bool) -> Result<()> {
        ensure!(self.schema_version == 1, "unsupported trim_manifest schema_version {}", self.schema_version);
        ensure!(self.orphan_multi_count == 0, "trim_manifest has {} orphan tokens", self.orphan_multi_count);
        for (name, &[old_id, new_id]) in &self.special_tokens {
            ensure!(
                (old_id as usize) < self.orig_vocab_size && (new_id as usize) < self.trimmed_vocab_size,
                "special token {name} mapping ({old_id}->{new_id}) out of bounds"
            );
        }
        for entry in &self.files {
            manifest::safe_relative(&entry.name).map_err(|e| anyhow::anyhow!(e))?;
            let p = dir.join(&entry.name);
            let meta = std::fs::metadata(&p).with_context(|| format!("missing {}", p.display()))?;
            ensure!(
                meta.len() == entry.size,
                "{}: size {} != manifest {}",
                entry.name,
                meta.len(),
                entry.size
            );
            let check_full_hash = full || entry.size <= 64 * 1024 * 1024;
            if check_full_hash {
                ensure!(
                    manifest::verified(&p, entry.size, &entry.sha256),
                    "{}: SHA-256 mismatch against trim_manifest.json",
                    entry.name
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_id_remap_bijection_and_special_bounds() {
        let kept: Vec<u32> = (0..128).map(|i| i * 4).collect();
        let remap = IdRemap::from_kept_ids(512, &kept, NumberingMode::Contiguous).unwrap();
        assert_eq!(remap.trimmed_vocab(), 128);
        assert_eq!(remap.map_old(0), Some(0));
        assert_eq!(remap.map_old(4), Some(1));
        assert_eq!(remap.map_old(1), None);

        // Out-of-bounds special token (granite-4.0-h eos=100257 bug) must error
        let bad_cfg = r#"{"vocab_size": 512, "eos_token_id": 100257}"#;
        assert!(remap_config_json(bad_cfg, &remap).is_err());

        // Valid config remaps cleanly
        let ok_cfg = r#"{"vocab_size": 512, "eos_token_id": 8, "image_token_id": 12}"#;
        let out: Value = serde_json::from_str(&remap_config_json(ok_cfg, &remap).unwrap()).unwrap();
        assert_eq!(out["vocab_size"].as_u64(), Some(128));
        assert_eq!(out["eos_token_id"].as_u64(), Some(2));
        assert_eq!(out["image_token_id"].as_u64(), Some(3));
    }

    #[test]
    fn unit_bpe_closure_zero_orphans_tree_and_dag() {
        // Tree BPE: "con" + "dition" -> "condition"; drop "dition" initially, closure must recover it!
        let mut vocab = serde_json::Map::new();
        let base_chars: Vec<String> = (0..64u8).map(|i| format!("{}", (b'!' + i) as char)).collect();
        for (i, c) in base_chars.iter().enumerate() {
            vocab.insert(c.clone(), Value::from(i as u64));
        }
        // 64: "co", 65: "di", 66: "codi", 67: "extra"
        vocab.insert("!\"#".into(), Value::from(64));
        vocab.insert("!\"$".into(), Value::from(65));
        vocab.insert("!\"#!\"$".into(), Value::from(66));
        let tok_json = serde_json::json!({
            "version": "1.0",
            "added_tokens": [{"id": 0, "content": "!", "special": true}],
            "model": {
                "type": "BPE",
                "vocab": vocab,
                "merges": [
                    "! \"",
                    "!\" #",
                    "!\" $",
                    "!\"# !\"$"
                ]
            }
        });
        // Keep base 0..63 plus 66 (dropping intermediate 64, 65 -> closure should recover or drop cleanly with 0 orphans)
        let mut kept: Vec<u32> = (0..63).collect();
        kept.push(66);
        let (_trimmed, _remap, stats) =
            trim_tokenizer_json(&tok_json.to_string(), 67, &kept, 4).unwrap();
        assert_eq!(stats.orphan_multi, 0);
    }

    #[test]
    fn unit_prioritized_trim_tiers_bijection_and_roundtrip() {
        assert!(is_code_syntax_noise(b"////////"));
        assert!(is_code_syntax_noise(b"));\n"));
        assert!(is_code_syntax_noise(b"myVar"));
        assert!(is_code_syntax_noise(b"    "));
        assert!(!is_code_syntax_noise(b"\"}\n"));
        assert!(!is_code_syntax_noise(b"```"));
        assert!(!is_code_syntax_noise(b" industrial"));
        assert!(!is_code_syntax_noise(b"micro"));

        // Minimal valid GPT-2 byte-BPE tokenizer across Tier 0..4
        let b2c: HashMap<u8, char> = crate::vocab::byte_decoder()
            .into_iter()
            .map(|(c, b)| (b, c))
            .collect();
        let enc = |bs: &[u8]| -> String { bs.iter().map(|b| b2c[b]).collect() };

        let mut vocab = serde_json::Map::new();
        for b in 0..=255u8 {
            vocab.insert(b2c[&b].to_string(), Value::from(b as u64));
        }
        let mut merges = Vec::new();
        let mut next_id = 256u64;
        let mut push_merge = |a: String, b: String, vocab: &mut serde_json::Map<String, Value>| -> u32 {
            let id = next_id as u32;
            next_id += 1;
            merges.push(format!("{a} {b}"));
            vocab.insert(format!("{a}{b}"), Value::from(id as u64));
            id
        };

        // Mix Tier 3 noise BEFORE Tier 2 word and Tier 1 struct in original IDs
        let id_noise_paren = push_merge(b2c[&b')'].to_string(), b2c[&b';'].to_string(), &mut vocab); // 256 Tier 3 ");"
        let id_noise = push_merge(enc(b");"), b2c[&b'\n'].to_string(), &mut vocab); // 257 Tier 3 ");\n"
        let id_wo = push_merge(b2c[&b'w'].to_string(), b2c[&b'o'].to_string(), &mut vocab); // 258 Tier 2 "wo"
        let id_rd = push_merge(b2c[&b'r'].to_string(), b2c[&b'd'].to_string(), &mut vocab); // 259 Tier 2 "rd"
        let id_word = push_merge(enc(b"wo"), enc(b"rd"), &mut vocab); // 260 Tier 2 "word"
        let id_close = push_merge(b2c[&b'"'].to_string(), b2c[&b'}'].to_string(), &mut vocab); // 261 Tier 1 "\"}"
        let id_close_nl = push_merge(enc(b"\"}"), b2c[&b'\n'].to_string(), &mut vocab); // 262 Tier 1 "\"}\n"
        let id_special = 263u32;

        // Pad up to 320 (multiple of 64) with reachable clean filler tokens
        let mut pad_ids = Vec::new();
        for pfx in [b'x', b'y'] {
            for ch in (b'0'..=b'9').chain(b'a'..=b'z') {
                if vocab.len() + 1 >= 320 {
                    break;
                }
                let pid = push_merge(b2c[&pfx].to_string(), b2c[&ch].to_string(), &mut vocab);
                pad_ids.push(pid);
            }
        }
        // Shift special token to end id_special = 263 wasn't in vocab, put special cleanly at next_id
        let special_id = next_id as u32;
        let _ = id_special;
        let tok_json = serde_json::json!({
            "version": "1.0",
            "added_tokens": [{
                "id": special_id,
                "content": "<|im_end|>",
                "single_word": false,
                "lstrip": false,
                "rstrip": false,
                "normalized": false,
                "special": true
            }],
            "pre_tokenizer": {
                "type": "ByteLevel",
                "add_prefix_space": false,
                "trim_offsets": true,
                "use_regex": false
            },
            "decoder": {
                "type": "ByteLevel",
                "add_prefix_space": true,
                "trim_offsets": true,
                "use_regex": true
            },
            "model": {
                "type": "BPE",
                "vocab": vocab,
                "merges": merges
            }
        })
        .to_string();
        let orig_vocab = special_id as usize + 1;
        let mut kept: Vec<u32> = (0..256).collect();
        kept.extend([id_noise_paren, id_noise, id_wo, id_rd, id_word, id_close, id_close_nl, special_id]);
        // Fill cleanly to multiple of 64 (320)
        for pid in pad_ids {
            if kept.len() >= 320 {
                break;
            }
            kept.push(pid);
        }
        kept.sort_unstable();
        kept.dedup();
        assert_eq!(kept.len() % ALIGN, 0);

        let struct_prio: BTreeSet<u32> = [id_close, id_close_nl].into_iter().collect();
        let (trimmed_json, remap, stats) =
            trim_tokenizer_json_prioritized(&tok_json, orig_vocab, &kept, 4, &struct_prio).unwrap();
        assert_eq!(stats.orphan_multi, 0);
        remap.verify_bijection().unwrap();

        let new_byte = remap.map_old(b'a' as u32).unwrap();
        let new_struct = remap.map_old(id_close_nl).unwrap();
        let new_word = remap.map_old(id_word).unwrap();
        let new_noise = remap.map_old(id_noise).unwrap();
        let new_special = remap.map_old(special_id).unwrap();

        // Strict band order: Tier 0 (byte) < Tier 1 (struct) < Tier 2 (word) < Tier 3 (noise) < Tier 4 (special)
        assert!(new_byte < new_struct, "tier0 {new_byte} < tier1 {new_struct}");
        assert!(new_struct < new_word, "tier1 {new_struct} < tier2 {new_word}");
        assert!(new_word < new_noise, "tier2 {new_word} < tier3 {new_noise}");
        assert!(new_noise < new_special, "tier3 {new_noise} < tier4 {new_special}");
        assert_eq!(new_special as usize, remap.trimmed_vocab() - 1);

        #[cfg(feature = "mine")]
        {
            let rt_tok = tokenizers::Tokenizer::from_bytes(trimmed_json.as_bytes()).unwrap();
            let sample = "word\"}\n";
            let enc_ids = rt_tok.encode(sample, false).unwrap();
            let dec = rt_tok.decode(enc_ids.get_ids(), false).unwrap();
            assert_eq!(dec, sample);
            assert!(enc_ids.get_ids().contains(&new_word));
            assert!(enc_ids.get_ids().contains(&new_struct));
        }
        let _ = trimmed_json;
    }
}
