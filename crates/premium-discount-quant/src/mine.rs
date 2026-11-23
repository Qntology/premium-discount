//! Corpus token frequency mining, leak filtering, BPE merge-parent closure, and hybrid
//! stratified `VocabPlan` construction (Phase 2 – Part 1).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use rayon::prelude::*;
use tokenizers::Tokenizer;
use unicode_normalization::UnicodeNormalization;

use crate::trim::{
    detect_flavor_from_json, is_placeholder_token, kind_of, pieces_from_tokenizer_json_typed, Kind,
    Langs, TokenizerFlavor, ALIGN,
};
use crate::vocab_plan::{sha256_hex, PlanTier, VocabPlan, VOCAB_PLAN_SCHEMA_VERSION};

/// Normalized SHA-256 leak blocklist (test sets / eval queries excluded from mining).
#[derive(Debug, Clone, Default)]
pub struct LeakBlocklist {
    hashes: HashSet<String>,
    digest: String,
}

impl LeakBlocklist {
    pub fn normalize_line(s: &str) -> String {
        s.nfc()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    pub fn from_texts<S: AsRef<str>>(texts: &[S]) -> Self {
        let mut hashes = HashSet::with_capacity(texts.len());
        let mut sorted = Vec::with_capacity(texts.len());
        for t in texts {
            let norm = Self::normalize_line(t.as_ref());
            if norm.is_empty() {
                continue;
            }
            let h = sha256_hex(norm.as_bytes());
            if hashes.insert(h.clone()) {
                sorted.push(h);
            }
        }
        sorted.sort();
        let digest = sha256_hex(sorted.join("\n").as_bytes());
        Self { hashes, digest }
    }

    pub fn is_blocked(&self, text: &str) -> bool {
        if self.hashes.is_empty() {
            return false;
        }
        let norm = Self::normalize_line(text);
        self.hashes.contains(&sha256_hex(norm.as_bytes()))
    }

    pub fn sha256(&self) -> &str {
        &self.digest
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
}

/// Deterministic tokenizer-keyed frequency cache (`tokenizer_sha256` -> per-lang/layer counts).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FreqCache {
    pub tokenizer_sha256: String,
    pub vocab_size: usize,
    pub normalization: String,
    pub leak_blocklist_sha256: String,
    /// `web_counts[lang][token_id]`
    pub web_counts: BTreeMap<String, Vec<u64>>,
    /// `domain_counts[lang][token_id]`
    pub domain_counts: BTreeMap<String, Vec<u64>>,
    pub blocked_docs: usize,
}

impl FreqCache {
    pub fn new(tokenizer_sha256: String, vocab_size: usize, leak_blocklist_sha256: String) -> Self {
        Self {
            tokenizer_sha256,
            vocab_size,
            normalization: "nfc".into(),
            leak_blocklist_sha256,
            web_counts: BTreeMap::new(),
            domain_counts: BTreeMap::new(),
            blocked_docs: 0,
        }
    }

    /// Deterministically count token IDs over `texts` in parallel (thread-count independent u64 sum).
    pub fn mine_texts(
        &mut self,
        tok: &Tokenizer,
        lang: &str,
        texts: &[String],
        is_domain: bool,
        blocklist: &LeakBlocklist,
        siglip_canonicalize: bool,
    ) -> Result<usize> {
        let vocab = self.vocab_size;
        let filtered: Vec<&String> = texts
            .iter()
            .filter(|t| !blocklist.is_blocked(t.as_str()))
            .collect();
        let blocked = texts.len() - filtered.len();
        self.blocked_docs += blocked;

        let counts = filtered
            .par_chunks(256)
            .map(|chunk| {
                let mut local = vec![0u64; vocab];
                for text in chunk {
                    let s = if siglip_canonicalize {
                        siglip2_canonicalize_safe(text)
                    } else {
                        text.nfc().collect::<String>()
                    };
                    if let Ok(enc) = tok.encode(s.as_str(), false) {
                        for &id in enc.get_ids() {
                            if (id as usize) < vocab {
                                local[id as usize] += 1;
                            }
                        }
                    }
                }
                local
            })
            .reduce(
                || vec![0u64; vocab],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b.iter()) {
                        *x += *y;
                    }
                    a
                },
            );

        let map = if is_domain {
            &mut self.domain_counts
        } else {
            &mut self.web_counts
        };
        let entry = map.entry(lang.to_string()).or_insert_with(|| vec![0u64; vocab]);
        for (dst, src) in entry.iter_mut().zip(counts.iter()) {
            *dst += *src;
        }
        Ok(filtered.len())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(self)?;
        std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
    }
}

/// Combining-mark-safe canonicalization for SigLIP2 text mining (Phase 4 – Part 1 D3).
fn siglip2_canonicalize_safe(text: &str) -> String {
    text.chars()
        .filter(|&c| c != '\u{0640}') // strip Arabic tatweel
        .map(|c| {
            let u = c as u32;
            let is_combining = (0x0300..=0x036F).contains(&u)
                || (0x064B..=0x065F).contains(&u)
                || u == 0x0670
                || (0x0E31..=0x0E3A).contains(&u)
                || (0x0E47..=0x0E4E).contains(&u)
                || (0x0900..=0x0903).contains(&u)
                || (0x093A..=0x094F).contains(&u);
            if c.is_alphanumeric() || c.is_whitespace() || is_combining {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse first merge parents `(parent_a, parent_b)` per token ID from a BPE `tokenizer.json`.
/// Supports both `"a b"` string merges (Qwen3/Qwen3.5) and `["a", "b"]` array merges (Granite/SigLIP2).
pub fn parse_first_bpe_parents(tokenizer_json: &str, vocab_size: usize) -> Result<Vec<Option<(u32, u32)>>> {
    let t: serde_json::Value = serde_json::from_str(tokenizer_json).context("parse tokenizer.json")?;
    let mut parents = vec![None; vocab_size];
    let Some(vocab_obj) = t["model"]["vocab"].as_object() else {
        return Ok(parents);
    };
    let mut tok_to_id: HashMap<&str, u32> = HashMap::with_capacity(vocab_obj.len());
    for (k, v) in vocab_obj {
        if let Some(id) = v.as_u64() {
            if (id as usize) < vocab_size {
                tok_to_id.insert(k.as_str(), id as u32);
            }
        }
    }
    let Some(merges) = t["model"]["merges"].as_array() else {
        return Ok(parents);
    };
    for m in merges {
        let pair = if let Some(s) = m.as_str() {
            s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string()))
        } else if let Some(arr) = m.as_array() {
            if arr.len() == 2 {
                arr[0]
                    .as_str()
                    .zip(arr[1].as_str())
                    .map(|(a, b)| (a.to_string(), b.to_string()))
            } else {
                None
            }
        } else {
            None
        };
        let Some((a, b)) = pair else { continue };
        let Some(&aid) = tok_to_id.get(a.as_str()) else { continue };
        let Some(&bid) = tok_to_id.get(b.as_str()) else { continue };
        let mut ab = String::with_capacity(a.len() + b.len());
        ab.push_str(&a);
        ab.push_str(&b);
        if let Some(&abid) = tok_to_id.get(ab.as_str()) {
            if parents[abid as usize].is_none() {
                parents[abid as usize] = Some((aid, bid));
            }
        }
    }
    Ok(parents)
}

/// Recursively include BPE merge parents so every kept multi-byte token is reachable (0 orphans).
pub fn close_bpe_merges(
    kept: &mut BTreeMap<u32, PlanTier>,
    first_parents: &[Option<(u32, u32)>],
) {
    let mut stack: Vec<u32> = kept.keys().copied().collect();
    while let Some(id) = stack.pop() {
        if let Some((a, b)) = first_parents.get(id as usize).copied().flatten() {
            for p in [a, b] {
                if !kept.contains_key(&p) {
                    kept.insert(p, PlanTier::MergeClosure);
                    stack.push(p);
                }
            }
        }
    }
}

/// Collect token IDs for BOTH bare (`"goods"`) and space-prefixed (`" goods"`) forms of schema/JSON keys.
pub fn collect_space_pair_tokens(tok: &Tokenizer, keys: &[&str], vocab_size: usize) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for &k in keys {
        for form in [
            k.to_string(),
            format!(" {k}"),
            format!("\"{k}\""),
            format!("\"{k}\":"),
        ] {
            if let Ok(enc) = tok.encode(form.as_str(), false) {
                for &id in enc.get_ids() {
                    if (id as usize) < vocab_size {
                        out.insert(id);
                    }
                }
            }
        }
    }
    out
}

/// Default structural JSON/HTML/number/currency/unit strings preserved in Tier 0.
pub fn default_ascii_struct_corpus() -> Vec<String> {
    let mut v: Vec<String> = [
        "<table>", "</table>", "<tr>", "</tr>", "<td>", "</td>", "<th>", "</th>",
        "<thead>", "<tbody>", "<br>", "{\"", "\":", "\": \"", "\", \"", "\"}", "[{", "}]",
        "null", "true", "false", "USD", "KRW", "JPY", "CNY", "EUR", "GBP", "AED", "SAR", "CZK",
        "FOB", "CIF", "CFR", "EXW", "DDP", "DAP", "FCA", "CPT", "CIP", "HS", "SKU", "B/L", "L/C",
        "PCS", "CTN", "KG", "MT", "CBM", "20FT", "40FT", "40HC", "LCL", "FCL", "ETA", "ETD",
        "goods", "condition", "status", "order", "orders", "shipping", "delivery", "invoice",
        "packing", "shipper", "consignee", "port", "quantity", "amount", "price", "currency",
        "title", "name", "date", "number", "code", "description", "total", "subtotal", "tax",
        "discount", "seller", "buyer", "address", "phone", "email", "tracking", "carrier",
        "transcription", "product", "item", "items", "category", "brand", "color", "size",
        "weight", "unit", "origin", "destination", "container", "seal", "vessel", "voyage",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    for n in 0..=999u32 {
        v.push(n.to_string());
    }
    v
}

#[derive(Debug, Clone)]
pub struct HybridPlanConfig {
    pub model_family: String,
    pub bundle: String,
    pub target_k: usize,
    pub langs: Langs,
    pub lang_codes: Vec<String>,
    pub domain_alpha: f64,
    pub hanja_cap: usize,
    pub corpus_manifest_sha256: String,
    pub lexicon_sha256: String,
}

/// Build a deterministic `VocabPlan` combining:
///   1. Tier 0 mandatory tokens (specials, single-byte base, ASCII structural core + space pairs, domain lexicon)
///   2. Script gating (`Langs::keeps_script`)
///   3. Stratified weighted corpus score `log(1 + f_web) + alpha * log(1 + f_dom)` per language
///   4. Recursive BPE merge-parent closure (`orphan_bpe_tokens == 0`)
///   5. 64-multiple alignment padding (deterministic tie-break by ascending original ID).
pub fn select_hybrid_plan(
    tokenizer_json: &str,
    tok: &Tokenizer,
    vocab_size: usize,
    cache: &FreqCache,
    lexicon_texts: &[String],
    schema_keys: &[&str],
    cfg: &HybridPlanConfig,
    holdout_by_lang: &BTreeMap<String, Vec<String>>,
) -> Result<VocabPlan> {
    let target_k = cfg.target_k.next_multiple_of(ALIGN).min(vocab_size);
    ensure!(target_k >= ALIGN, "target_k ({target_k}) too small");

    let parsed: serde_json::Value = serde_json::from_str(tokenizer_json)?;
    let flavor = detect_flavor_from_json(&parsed);
    let (_fl, pieces, added, assigned) = pieces_from_tokenizer_json_typed(tokenizer_json, vocab_size)?;
    let kinds: Vec<Kind> = pieces
        .iter()
        .zip(added.iter())
        .map(|(p, &a)| kind_of(p, a))
        .collect();
    let first_parents = parse_first_bpe_parents(tokenizer_json, vocab_size)?;
    // Detect source-level orphan multi-byte tokens (e.g. 201 unmerged entries in Qwen3.5 raw tokenizer.json)
    let is_source_orphan = |idx: usize| -> bool {
        if flavor == TokenizerFlavor::SentencePieceUnigram {
            return false;
        }
        !added[idx] && pieces[idx].len() > 1 && first_parents[idx].is_none()
    };

    let mut kept: BTreeMap<u32, PlanTier> = BTreeMap::new();

    // Tier 0a: Mandatory special & single-byte base tokens (skip unassigned & placeholder rows)
    for id in 0..vocab_size as u32 {
        let idx = id as usize;
        if !assigned[idx] {
            continue;
        }
        if added[idx] {
            kept.insert(id, PlanTier::MandatorySpecial);
        } else if pieces[idx].len() == 1 {
            kept.insert(id, PlanTier::MandatoryByte);
        } else if flavor == TokenizerFlavor::SentencePieceBpe
            && pieces[idx] == b" "
        {
            kept.insert(id, PlanTier::MandatoryByte);
        }
    }

    // Tier 0b: Structural ASCII + JSON/HTML tags + 0..999 + space-pair schema keys
    let struct_corpus = default_ascii_struct_corpus();
    for s in &struct_corpus {
        if let Ok(enc) = tok.encode(s.as_str(), false) {
            for &id in enc.get_ids() {
                if (id as usize) < vocab_size && assigned[id as usize] {
                    kept.entry(id).or_insert(PlanTier::MandatoryAsciiStruct);
                }
            }
        }
    }
    let key_refs: Vec<&str> = schema_keys.to_vec();
    for id in collect_space_pair_tokens(tok, &key_refs, vocab_size) {
        if assigned[id as usize] {
            kept.entry(id).or_insert(PlanTier::MandatoryAsciiStruct);
        }
    }
    // Also preserve all bare & space-prefixed default structural English keywords
    let default_keys = [
        "goods", "condition", "status", "order", "invoice", "packing", "shipper", "consignee",
        "quantity", "amount", "price", "currency", "date", "number", "code", "total", "title",
        "name", "description", "transcription", "SKU", "HS", "FOB", "CIF",
    ];
    for id in collect_space_pair_tokens(tok, &default_keys, vocab_size) {
        if assigned[id as usize] {
            kept.entry(id).or_insert(PlanTier::MandatoryAsciiStruct);
        }
    }

    // Tier 0c: Bundle-scoped domain lexicon
    for s in lexicon_texts {
        if let Ok(enc) = tok.encode(s.as_str(), false) {
            for &id in enc.get_ids() {
                if (id as usize) < vocab_size && assigned[id as usize] {
                    kept.entry(id).or_insert(PlanTier::DomainLexicon);
                }
            }
        }
    }

    // Close BPE parents on Tier 0 right away so base budget is accurate
    close_bpe_merges(&mut kept, &first_parents);

    // Build per-token score across target languages (with script gate)
    let allowed_id = |id: u32| -> bool {
        let idx = id as usize;
        if idx >= vocab_size || !assigned[idx] || is_source_orphan(idx) {
            return false;
        }
        match kinds[idx] {
            Kind::Special | Kind::Byte | Kind::Partial | Kind::Ascii | Kind::Neutral => true,
            Kind::Script { cjk: true, hangul: false, kana: false, arabic: false, latin: false } => {
                cfg.langs.cjk || cfg.langs.hanja
            }
            k => cfg.langs.keeps_script(k),
        }
    };

    // Stratified per-language coverage pass: guarantee every target language gets its top
    // frequency tokens before global fill, preventing English/CJK from starving cs/ar/ko.
    let n_langs = cfg.lang_codes.len().max(1);
    let remaining_budget = target_k.saturating_sub(kept.len());
    let per_lang_floor = (remaining_budget / (n_langs * 2)).max(256);

    let mut global_scores = vec![0.0f64; vocab_size];
    for lang in &cfg.lang_codes {
        let web = cache.web_counts.get(lang);
        let dom = cache.domain_counts.get(lang);
        let mut lang_scored: Vec<(f64, u32)> = Vec::new();
        for id in 0..vocab_size as u32 {
            if !allowed_id(id) {
                continue;
            }
            let fw = web.and_then(|v| v.get(id as usize)).copied().unwrap_or(0) as f64;
            let fd = dom.and_then(|v| v.get(id as usize)).copied().unwrap_or(0) as f64;
            if fw > 0.0 || fd > 0.0 {
                let s = (1.0 + fw).ln() + cfg.domain_alpha * (1.0 + fd).ln();
                global_scores[id as usize] += s;
                lang_scored.push((s, id));
            }
        }
        // Deterministic sort: descending score, tie-break ascending original token ID
        lang_scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut added_for_lang = 0usize;
        for &(_score, id) in &lang_scored {
            if added_for_lang >= per_lang_floor || kept.len() >= target_k {
                break;
            }
            if !kept.contains_key(&id) {
                kept.insert(id, PlanTier::CorpusFreq);
                added_for_lang += 1;
            }
        }
    }

    // Close BPE parents on stratified tokens
    close_bpe_merges(&mut kept, &first_parents);

    // Helper: insert `id` together with its full BPE merge-closure chain if it fits in `target_k`.
    let try_insert_with_closure = |kept: &mut BTreeMap<u32, PlanTier>, id: u32, limit: usize| -> usize {
        if kept.contains_key(&id) || kept.len() >= limit {
            return 0;
        }
        let mut chain = Vec::new();
        let mut stack = vec![id];
        let mut seen = HashSet::new();
        while let Some(cur) = stack.pop() {
            if kept.contains_key(&cur) || !seen.insert(cur) {
                continue;
            }
            chain.push(cur);
            if let Some((a, b)) = first_parents[cur as usize] {
                stack.push(a);
                stack.push(b);
            }
        }
        if kept.len() + chain.len() <= limit {
            let added = chain.len();
            for c in chain {
                let tier = if c == id {
                    if global_scores[id as usize] > 0.0 {
                        PlanTier::CorpusFreq
                    } else {
                        PlanTier::AlignPad
                    }
                } else {
                    PlanTier::MergeClosure
                };
                kept.insert(c, tier);
            }
            added
        } else {
            0
        }
    };

    // Per-script stratified merge-order quota pass: prevents low-ID ASCII tokens from starving
    // non-ASCII scripts (Hangul, Kana, CJK, Arabic, European Latin) when corpus counts are sparse.
    {
        let rem_for_scripts = (target_k.saturating_sub(kept.len()) * 60) / 100;
        let mut buckets: Vec<(&str, usize, fn(Kind) -> bool)> = Vec::new();
        if cfg.langs.hangul {
            buckets.push(("hangul", 18, |k| matches!(k, Kind::Script { hangul: true, .. })));
        }
        if cfg.langs.kana {
            buckets.push(("kana", 11, |k| matches!(k, Kind::Script { kana: true, .. })));
        }
        if cfg.langs.cjk {
            buckets.push(("cjk", 29, |k| matches!(k, Kind::Script { cjk: true, hangul: false, kana: false, .. })));
        }
        if cfg.langs.arabic {
            buckets.push(("arabic", 18, |k| matches!(k, Kind::Script { arabic: true, .. })));
        }
        if cfg.langs.latin {
            buckets.push(("latin", 18, |k| matches!(k, Kind::Script { latin: true, hangul: false, kana: false, cjk: false, arabic: false })));
        }
        let w_sum: usize = buckets.iter().map(|b| b.1).sum();
        if w_sum > 0 && rem_for_scripts > 0 {
            for &(_name, w, pred) in &buckets {
                let quota = (rem_for_scripts * w) / w_sum;
                let mut cands: Vec<(f64, u32)> = (0..vocab_size as u32)
                    .filter(|&id| allowed_id(id) && !kept.contains_key(&id) && pred(kinds[id as usize]))
                    .map(|id| (global_scores[id as usize] + 1e-6 / (1.0 + id as f64), id))
                    .collect();
                cands.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                let mut added_bucket = 0usize;
                for &(_sc, id) in &cands {
                    if added_bucket >= quota || kept.len() >= target_k {
                        break;
                    }
                    let n = try_insert_with_closure(&mut kept, id, target_k);
                    if n > 0 {
                        added_bucket += 1;
                    }
                }
            }
        }
    }

    // Global ranked fill up to target_k (with reserve for merge closure)
    let mut candidates: Vec<(f64, u32)> = (0..vocab_size as u32)
        .filter(|&id| allowed_id(id) && !kept.contains_key(&id))
        .map(|id| {
            // If corpus frequency is 0, fall back to tiny inverse-ID merge-order preference
            let base = global_scores[id as usize];
            let merge_prior = 1e-6 / (1.0 + id as f64);
            (base + merge_prior, id)
        })
        .collect();
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut hanja_added = 0usize;
    let is_hanja_only = |id: u32| -> bool {
        !cfg.langs.cjk
            && cfg.langs.hanja
            && matches!(
                kinds[id as usize],
                Kind::Script { cjk: true, hangul: false, kana: false, arabic: false, latin: false }
            )
    };

    for &(_score, id) in &candidates {
        if kept.len() >= target_k {
            break;
        }
        if is_hanja_only(id) && hanja_added >= cfg.hanja_cap {
            continue;
        }
        // Compute closure chain for this candidate before inserting so we never overshoot target_k
        let mut chain = Vec::new();
        let mut stack = vec![id];
        let mut seen = HashSet::new();
        while let Some(cur) = stack.pop() {
            if kept.contains_key(&cur) || !seen.insert(cur) {
                continue;
            }
            chain.push(cur);
            if let Some((a, b)) = first_parents[cur as usize] {
                stack.push(a);
                stack.push(b);
            }
        }
        if kept.len() + chain.len() <= target_k {
            for c in chain {
                let tier = if c == id {
                    if global_scores[id as usize] > 0.0 {
                        PlanTier::CorpusFreq
                    } else {
                        PlanTier::AlignPad
                    }
                } else {
                    PlanTier::MergeClosure
                };
                kept.insert(c, tier);
                if is_hanja_only(c) {
                    hanja_added += 1;
                }
            }
        }
    }

    // If still below target_k (e.g. small script bundle), fill with lowest-ID closure-safe assigned tokens
    if kept.len() < target_k {
        for id in 0..vocab_size as u32 {
            if kept.len() >= target_k {
                break;
            }
            if !assigned[id as usize] || kept.contains_key(&id) {
                continue;
            }
            let mut chain = Vec::new();
            let mut stack = vec![id];
            let mut seen = HashSet::new();
            while let Some(cur) = stack.pop() {
                if kept.contains_key(&cur) || !seen.insert(cur) {
                    continue;
                }
                chain.push(cur);
                if let Some((a, b)) = first_parents[cur as usize] {
                    stack.push(a);
                    stack.push(b);
                }
            }
            if kept.len() + chain.len() <= target_k {
                for c in chain {
                    kept.insert(c, if c == id { PlanTier::AlignPad } else { PlanTier::MergeClosure });
                }
            }
        }
    }

    // If closure pushed kept slightly over target_k (only possible if Tier 0 itself > target_k),
    // align up to next multiple of 64 within vocab_size.
    let final_target = kept.len().next_multiple_of(ALIGN).min(vocab_size);
    if kept.len() < final_target {
        for id in 0..vocab_size as u32 {
            if kept.len() >= final_target {
                break;
            }
            if !assigned[id as usize] || kept.contains_key(&id) {
                continue;
            }
            let can_direct = first_parents[id as usize]
                .map_or(true, |(a, b)| kept.contains_key(&a) && kept.contains_key(&b));
            if can_direct {
                kept.insert(id, PlanTier::AlignPad);
            }
        }
    }

    // Verify 0 orphan tokens wrt first_parents
    let mut orphan_bpe_tokens = 0usize;
    for &id in kept.keys() {
        if let Some((a, b)) = first_parents[id as usize] {
            if !kept.contains_key(&a) || !kept.contains_key(&b) {
                orphan_bpe_tokens += 1;
            }
        }
    }

    // Measure holdout per-language coverage and estimated fertility
    let mut per_lang_coverage = BTreeMap::new();
    let mut per_lang_fertility = BTreeMap::new();
    for (lang, samples) in holdout_by_lang {
        let mut total_toks = 0usize;
        let mut covered_toks = 0usize;
        let mut est_trimmed_toks = 0usize;
        for s in samples {
            if let Ok(enc) = tok.encode(s.as_str(), false) {
                for &id in enc.get_ids() {
                    total_toks += 1;
                    if kept.contains_key(&id) {
                        covered_toks += 1;
                        est_trimmed_toks += 1;
                    } else {
                        // Conservative upper bound: unkept token splits into its raw bytes (or ~2 subwords)
                        let blen = pieces.get(id as usize).map(|p| p.len().max(2)).unwrap_or(2);
                        est_trimmed_toks += blen.min(3);
                    }
                }
            }
        }
        if total_toks > 0 {
            per_lang_coverage.insert(lang.clone(), covered_toks as f64 / total_toks as f64);
            per_lang_fertility.insert(lang.clone(), est_trimmed_toks as f64 / total_toks as f64);
        }
    }

    let kept_ids: Vec<u32> = kept.keys().copied().collect();
    let reasons: Vec<PlanTier> = kept.values().copied().collect();
    let _ = is_placeholder_token;

    let mut plan = VocabPlan {
        schema_version: VOCAB_PLAN_SCHEMA_VERSION,
        model_family: cfg.model_family.clone(),
        tokenizer_flavor: flavor.as_str().to_string(),
        tokenizer_sha256: cache.tokenizer_sha256.clone(),
        corpus_manifest_sha256: cfg.corpus_manifest_sha256.clone(),
        leak_blocklist_sha256: cache.leak_blocklist_sha256.clone(),
        lexicon_sha256: cfg.lexicon_sha256.clone(),
        bundle: cfg.bundle.clone(),
        orig_vocab_size: vocab_size,
        kept_vocab_size: kept_ids.len(),
        orphan_bpe_tokens,
        per_lang_coverage,
        per_lang_fertility,
        kept_ids,
        reasons,
        plan_sha256: String::new(),
    };
    plan.finalize()?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpe_closure_recursively_adds_ancestors() {
        // 0='a', 1='b', 2='c', 3='ab'(0,1), 4='abc'(3,2)
        let parents = vec![None, None, None, Some((0, 1)), Some((3, 2))];
        let mut kept = BTreeMap::from([(4u32, PlanTier::CorpusFreq)]);
        close_bpe_merges(&mut kept, &parents);
        assert!(kept.contains_key(&0));
        assert!(kept.contains_key(&1));
        assert!(kept.contains_key(&2));
        assert!(kept.contains_key(&3));
        assert!(kept.contains_key(&4));
        assert_eq!(kept[&3], PlanTier::MergeClosure);
    }

    #[test]
    fn leak_blocklist_normalizes_and_blocks() {
        let bl = LeakBlocklist::from_texts(&["  Hello   World  ", "BL-2026-08814"]);
        assert!(bl.is_blocked("hello world"));
        assert!(bl.is_blocked("bl-2026-08814"));
        assert!(!bl.is_blocked("different document"));
    }
}
