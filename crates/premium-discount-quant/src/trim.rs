//! 어휘 잘라 내기: 생성 모델의 **출력 투영**에서 남길 행 고르기. 입력 임베딩은 어휘 전체 그대로 둔다(읽기는 다국어 전부).
//!
//! 고르는 규칙은 Qntology/logis-center 의 `models/common/vocab_trim.rs`(PR #84, 전체 어휘와 결과가 같음을 검색 96/96 ·
//! 텍스트 538/538 · 이미지 50/50 으로 확인한 규칙)를 따라 이 crate 에서 다시 썼다. 남기는 것:
//!
//! - **공통 코어**: 추가 · 특수 토큰, 한 바이트 토큰(바이트 수준 BPE 의 바이트 폴백), 혼자서는 UTF-8 이 아닌 바이트 조각,
//!   기호 · 단위 · 그리스 문자 토큰.
//! - **문서 언어의 문자 체계 전부**: 한글(ko) · 라틴 확장(de · fr · es · it · pt · nl · cs) · 가나(ja) · 아랍 문자(ar) · 한자(ja · zh).
//!   한자를 쓰지 않는 문서에 섞인 한자(한국어 문서의 한자)는 도메인 글에 나온 것 + 병합 순 상위 [`CJK_COMMON`] 개.
//!   12개 언어 밖 문자(키릴 · 타이 …)는 그 언어의 문서이거나 글에 나왔을 때만.
//! - **글에 실제로 나온 문자 체계**: 부르는 쪽이 [`Langs::present`] 로 더한다(판정 언어만 따르면 문서에 있는 글자를 잘라 낸다).
//! - **도메인 글**: 부르는 쪽 사전 · 표 태그 등의 토큰 번호.
//! - **ASCII 전부**: 영어 · 숫자 · JSON 키 · 코드는 어느 문서에서나 전부 디코딩한다. 행 수 예산을 두는 규칙(남은 자리를 문자 체계와
//!   영어가 나눠 가짐)은 영어 단어를 빼므로 두지 않는다(logis-center 에서 실측으로 지운 규칙).
//! - 남긴 수는 64 의 배수.
//!
//! **정확성**: 잘린 행이 이기면 결과가 달라진다. logis-center 는 잘린 행을 −∞ 로 두고, 규칙이 필요한 행을 다 남긴다고 본다.
//! 이 crate 는 [`crate::lmhead::ColdGuard`] 로 잘린 행을 묶어 두고 이길 수 있는 묶음만 다시 계산하므로, 규칙이 무엇을 놓쳐도
//! 탐욕 디코딩의 결과는 전체 어휘와 같다([`slice`] → [`crate::vocab::VocabSlice`] 의 `cold`). 샘플링(온도 > 0)은 남은 확률이 다시
//! 정규화되어 결과가 바뀌므로 전체 행으로 곱한다(logis-center 와 같음).
//!
//! 문서 언어 판정(whatlang · Stanza)은 이 crate 밖(premium-discount-nlp)이다: 부르는 쪽이 언어 코드를 [`Langs::from_codes`] 로 넘긴다.

use std::collections::BTreeSet;

use crate::vocab::VocabSlice;

/// 남길 한자 토큰 수 (도메인 글 밖으로, 병합 순 상위)
pub const CJK_COMMON: usize = 4096;
/// 남긴 행 수를 맞추는 배수
pub const ALIGN: usize = 64;

/// 토큰 한 개의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Special,
    Byte,
    Partial,
    Ascii,
    Neutral,
    Script { hangul: bool, kana: bool, cjk: bool, arabic: bool, latin: bool },
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Ascii,
    Hangul,
    Kana,
    Cjk,
    Arabic,
    Latin,
    Neutral,
    Other,
}

fn script(c: char) -> Script {
    match c as u32 {
        0..=0x7F => Script::Ascii,
        0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xD7B0..=0xD7FF => Script::Hangul,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF65..=0xFF9F => Script::Kana,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF => Script::Cjk,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arabic,
        0x00C0..=0x024F | 0x1E00..=0x1EFF | 0x0300..=0x036F => Script::Latin,
        0x0370..=0x03FF | 0x00A0..=0x00BF | 0x2000..=0x2BFF | 0x3000..=0x303F | 0x3200..=0x33FF | 0xFF00..=0xFFEF => Script::Neutral,
        _ => Script::Other,
    }
}

/// 토큰의 원래 바이트(`piece`)와 추가 토큰 여부로 종류를 정한다.
pub fn kind_of(piece: &[u8], added: bool) -> Kind {
    if added {
        return Kind::Special;
    }
    if piece.len() == 1 {
        return Kind::Byte;
    }
    let Ok(s) = std::str::from_utf8(piece) else { return Kind::Partial };
    let (mut hangul, mut kana, mut cjk, mut arabic, mut latin, mut ascii_only) = (false, false, false, false, false, true);
    for c in s.chars() {
        match script(c) {
            Script::Ascii => {}
            Script::Neutral => ascii_only = false,
            Script::Hangul => hangul = true,
            Script::Kana => kana = true,
            Script::Cjk => cjk = true,
            Script::Arabic => arabic = true,
            Script::Latin => latin = true,
            Script::Other => return Kind::Other,
        }
    }
    if hangul || kana || cjk || arabic || latin {
        Kind::Script { hangul, kana, cjk, arabic, latin }
    } else if ascii_only {
        Kind::Ascii
    } else {
        Kind::Neutral
    }
}

/// 남길 문자 체계
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Langs {
    pub hangul: bool,
    pub kana: bool,
    pub cjk: bool,
    pub arabic: bool,
    pub latin: bool,
    /// 12개 언어 밖의 문자 체계 (키릴 · 타이 · 데바나가리 · 벵골 · 히브리 문자 등)
    pub other: bool,
    /// 한자를 쓰지 않는 문서에 섞인 한자: 도메인 글 + 상위 [`CJK_COMMON`] 개 (`cjk` 는 한자를 쓰는 언어 ja · zh: 한자 토큰 전부)
    pub hanja: bool,
}

impl Langs {
    fn none() -> Self {
        Self { hangul: false, kana: false, cjk: false, arabic: false, latin: false, other: false, hanja: false }
    }

    /// 12개 언어 (ko · en · ja · zh · de · fr · es · it · pt · nl · cs · ar)
    pub fn all() -> Self {
        Self { other: false, ..Self::any() }
    }

    /// 어떤 문자 체계도 빼지 않음: 언어를 아직 모를 때
    pub fn any() -> Self {
        Self { hangul: true, kana: true, cjk: true, arabic: true, latin: true, other: true, hanja: false }
    }

    /// 글에 실제로 나온 문자 체계. 한자는 섞인 한자([`Self::hanja`])로 본다: 한자를 쓰는 언어인지는 언어 코드(ja · zh)가 정한다.
    pub fn present(text: &str) -> Self {
        let mut l = Self::none();
        for c in text.chars() {
            match script(c) {
                Script::Hangul => l.hangul = true,
                Script::Kana => l.kana = true,
                Script::Cjk => l.hanja = true,
                Script::Arabic => l.arabic = true,
                Script::Latin => l.latin = true,
                Script::Other if c.is_alphabetic() => l.other = true,
                _ => {}
            }
        }
        l
    }

    pub fn union(self, o: Self) -> Self {
        let cjk = self.cjk || o.cjk;
        Self {
            hangul: self.hangul || o.hangul,
            kana: self.kana || o.kana,
            cjk,
            arabic: self.arabic || o.arabic,
            latin: self.latin || o.latin,
            other: self.other || o.other,
            hanja: !cjk && (self.hanja || o.hanja),
        }
    }

    /// 언어 코드에서 (ko · en · ja · zh · zh-hans · de · fr · es · it · pt · nl · cs · ar, all · any). 그 밖의 코드(ru · th · hi · pl …)는
    /// 문자 체계를 모르므로 라틴 확장과 12개 언어 밖 문자를 함께 남긴다. 빈 목록이면 `None`.
    pub fn from_codes<S: AsRef<str>>(codes: &[S]) -> Option<Self> {
        if codes.is_empty() {
            return None;
        }
        let mut l = Self::none();
        for code in codes {
            match code.as_ref().trim().to_ascii_lowercase().as_str() {
                "ko" | "kor" | "korean" => l.hangul = true,
                "en" | "eng" | "english" | "" => {}
                "ja" | "jpn" | "japanese" => {
                    l.kana = true;
                    l.cjk = true;
                }
                "zh" | "zh-hans" | "zh-cn" | "zh-tw" | "zh-hk" | "zh-hant" | "zho" | "cmn" | "chinese" => l.cjk = true,
                "de" | "deu" | "fr" | "fra" | "fre" | "es" | "spa" | "it" | "ita" | "pt" | "por" | "nl" | "nld" | "dut" | "cs" | "ces" | "cze" => l.latin = true,
                "ar" | "ara" | "arabic" => l.arabic = true,
                "all" => l = l.union(Self::all()),
                "any" => l = l.union(Self::any()),
                _ => {
                    l.latin = true;
                    l.other = true;
                }
            }
        }
        Some(l)
    }

    /// 문서 언어 하나(현지 언어)에서: 현지 언어 + 영어
    pub fn of_doc_lang(doc_lang: &str) -> Self {
        Self::from_codes(&[doc_lang, "en"]).unwrap_or_else(Self::any)
    }

    pub fn is_all(&self) -> bool {
        *self == Self::all()
    }

    pub fn keeps_script(&self, k: Kind) -> bool {
        match k {
            Kind::Script { hangul, kana, cjk, arabic, latin } => {
                let allowed = (!hangul || self.hangul)
                    && (!kana || self.kana)
                    && (!cjk || self.cjk || self.hanja)
                    && (!arabic || self.arabic)
                    && (!latin || self.latin);
                // 한자를 쓰는 언어면 한자만 쓰는 토큰도 전부. 섞인 한자면 한자만 쓰는 토큰은 상위 한자 단계에서
                allowed && (hangul || kana || arabic || latin || (cjk && self.cjk))
            }
            Kind::Other => self.other,
            _ => false,
        }
    }

    pub fn label(&self) -> String {
        if self.is_all() {
            return "all".into();
        }
        if *self == Self::any() {
            return "any".into();
        }
        let mut s = Vec::new();
        for (on, name) in [
            (self.hangul, "hangul"),
            (self.kana, "kana"),
            (self.cjk, "cjk"),
            (self.arabic, "arabic"),
            (self.latin, "latin"),
            (self.other, "other"),
            (self.hanja, "hanja"),
        ] {
            if on {
                s.push(name);
            }
        }
        if s.is_empty() {
            "ascii".into()
        } else {
            s.join("+")
        }
    }
}

/// 고르는 규칙. `Full` 하나뿐이다: 행 수에 예산을 두는 규칙은 영어 단어를 빼므로 두지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimRule {
    Off,
    Full,
}

impl TrimRule {
    pub fn parse(spec: &str) -> Result<Self, String> {
        match spec.trim().to_ascii_lowercase().as_str() {
            "" | "full" => Ok(Self::Full),
            "off" | "none" | "0" => Ok(Self::Off),
            other => Err(format!("어휘 잘라 내기 규칙 {other:?} 를 모릅니다 (full · off)")),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Full => "full",
        }
    }
}

/// 남길 행 고르기의 본체 (토큰 종류 · 도메인 토큰 번호가 주어졌을 때). 오름차순 번호와 단계별 수.
pub fn select_from(kinds: &[Kind], domain: &BTreeSet<u32>, rule: TrimRule, langs: Langs) -> (Vec<u32>, Vec<(&'static str, usize)>) {
    let vocab = kinds.len();
    if rule == TrimRule::Off {
        return ((0..vocab as u32).collect(), vec![("off", vocab)]);
    }
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    let mut tiers: Vec<(&'static str, usize)> = Vec::new();
    let mut add = |name: &'static str, ids: &mut dyn Iterator<Item = u32>, keep: &mut BTreeSet<u32>, cap: Option<usize>| {
        let before = keep.len();
        for i in ids {
            if cap.is_some_and(|c| keep.len() >= c) {
                break;
            }
            keep.insert(i);
        }
        tiers.push((name, keep.len() - before));
    };
    let all = || 0..vocab as u32;
    add(
        "core",
        &mut all().filter(|&i| matches!(kinds[i as usize], Kind::Special | Kind::Byte | Kind::Partial | Kind::Neutral)),
        &mut keep,
        None,
    );
    add("scripts", &mut all().filter(|&i| langs.keeps_script(kinds[i as usize])), &mut keep, None);
    add("domain", &mut domain.iter().copied().filter(|&i| (i as usize) < vocab), &mut keep, None);
    add("ascii", &mut all().filter(|&i| kinds[i as usize] == Kind::Ascii), &mut keep, None);
    // 섞인 한자: 도메인 밖의 상위 한자 (병합 순). 한자를 쓰는 언어면 문자 체계 단계에서 이미 전부
    if langs.hanja && !langs.cjk {
        let common: Vec<u32> = all()
            .filter(|&i| {
                matches!(kinds[i as usize], Kind::Script { cjk: true, hangul: false, kana: false, arabic: false, latin: false })
                    && !keep.contains(&i)
            })
            .take(CJK_COMMON)
            .collect();
        add("cjk", &mut common.into_iter(), &mut keep, None);
    }
    let target = keep.len().next_multiple_of(ALIGN).min(vocab);
    let pad_cjk = langs.cjk || langs.hanja;
    add(
        "pad",
        &mut all()
            .filter(|&i| pad_cjk && matches!(kinds[i as usize], Kind::Script { cjk: true, .. }))
            .chain(all().filter(|&i| kinds[i as usize] == Kind::Ascii))
            .chain(all()),
        &mut keep,
        Some(target),
    );
    (keep.into_iter().collect(), tiers)
}

/// `a` (오름차순)가 `b` (오름차순)에 모두 들어 있는지
pub fn sorted_subset(a: &[u32], b: &[u32]) -> bool {
    let mut j = 0;
    for x in a {
        while j < b.len() && b[j] < *x {
            j += 1;
        }
        if j == b.len() || b[j] != *x {
            return false;
        }
    }
    true
}

/// 고른 행과 그 나머지(`cold`, [`crate::lmhead::ColdGuard`] 로 묶을 행)를 [`VocabSlice`] 로.
pub fn slice(kinds: &[Kind], domain: &BTreeSet<u32>, rule: TrimRule, langs: Langs) -> VocabSlice {
    let (kept, tiers) = select_from(kinds, domain, rule, langs);
    let mut cold = Vec::with_capacity(kinds.len() - kept.len());
    let mut k = kept.iter().peekable();
    for i in 0..kinds.len() as u32 {
        if k.peek() == Some(&&i) {
            k.next();
        } else {
            cold.push(i);
        }
    }
    let stats: std::collections::BTreeMap<String, usize> = tiers.into_iter().map(|(n, c)| (format!("trim_{n}"), c)).collect();
    VocabSlice {
        vocab_size: kinds.len(),
        kept,
        cold,
        stats,
    }
}

/// Four tokenizer families supported by the vocabulary mining and artifact trimming pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenizerFlavor {
    /// GPT-2 byte-level BPE (Qwen3, Qwen3.5): 256 byte fallback, `"a b"` merges.
    Gpt2ByteBpe,
    /// Granite/ModernBERT byte-level BPE (`granite-97m-r2`): 216 byte base, `["a","b"]` merges.
    GraniteByteBpe,
    /// SentencePiece-style BPE (`siglip2-so400m-patch16-naflex`): `▁` word prefix, `<0xXX>` byte fallback.
    SentencePieceBpe,
    /// SentencePiece Unigram model with log-probability scores.
    SentencePieceUnigram,
}

impl TokenizerFlavor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gpt2ByteBpe => "gpt2_byte_bpe",
            Self::GraniteByteBpe => "granite_byte_bpe",
            Self::SentencePieceBpe => "sentencepiece_bpe",
            Self::SentencePieceUnigram => "sentencepiece_unigram",
        }
    }
}

/// Detect the tokenizer flavor from a parsed Hugging Face `tokenizer.json`.
pub fn detect_flavor_from_json(t: &serde_json::Value) -> TokenizerFlavor {
    let mtype = t["model"]["type"].as_str().unwrap_or("");
    if mtype.eq_ignore_ascii_case("Unigram") {
        return TokenizerFlavor::SentencePieceUnigram;
    }
    let has_byte_fallback = t["model"]["byte_fallback"].as_bool().unwrap_or(false);
    let vocab_obj = t["model"]["vocab"].as_object();
    let has_sp_hex = vocab_obj.is_some_and(|v| v.contains_key("<0x00>") || v.contains_key("<0x20>"));
    let has_metaspace = vocab_obj.is_some_and(|v| v.keys().any(|k| k.starts_with('\u{2581}')));
    if has_byte_fallback || has_sp_hex || has_metaspace {
        return TokenizerFlavor::SentencePieceBpe;
    }
    let has_array_merges = t["model"]["merges"]
        .as_array()
        .and_then(|a| a.first())
        .is_some_and(|v| v.is_array());
    let has_granite_reserved = t["added_tokens"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x["content"].as_str().is_some_and(|s| s.starts_with("<|reserved_200"))));
    let has_granite_cls = t["added_tokens"]
        .as_array()
        .is_some_and(|a| a.iter().any(|x| x["content"].as_str() == Some("<s>") || x["content"].as_str() == Some("<mask>")));
    if has_granite_reserved || (has_array_merges && has_granite_cls) {
        TokenizerFlavor::GraniteByteBpe
    } else {
        TokenizerFlavor::Gpt2ByteBpe
    }
}

/// Decode a single vocabulary token string into raw bytes according to its tokenizer flavor.
pub fn decode_piece_for_flavor(
    tok: &str,
    flavor: TokenizerFlavor,
    byte_dec: &std::collections::HashMap<char, u8>,
) -> Vec<u8> {
    match flavor {
        TokenizerFlavor::SentencePieceBpe | TokenizerFlavor::SentencePieceUnigram => {
            if tok.len() == 6 && tok.starts_with("<0x") && tok.ends_with('>') {
                if let Ok(b) = u8::from_str_radix(&tok[3..5], 16) {
                    return vec![b];
                }
            }
            if tok == "\t" {
                return vec![b'\t'];
            }
            tok.replace('\u{2581}', " ").into_bytes()
        }
        TokenizerFlavor::Gpt2ByteBpe | TokenizerFlavor::GraniteByteBpe => {
            let mut out = Vec::with_capacity(tok.len());
            for c in tok.chars() {
                match byte_dec.get(&c) {
                    Some(&b) => out.push(b),
                    None => return tok.as_bytes().to_vec(),
                }
            }
            out
        }
    }
}

/// Returns whether an added/special token is an unused placeholder (`<|reserved_N|>`, `<unusedN>`).
pub fn is_placeholder_token(content: &str) -> bool {
    (content.starts_with("<|reserved_") && content.ends_with("|>"))
        || (content.starts_with("<unused") && content.ends_with('>') && content[7..content.len() - 1].chars().all(|c| c.is_ascii_digit()))
}

/// Flavor-aware parser for `tokenizer.json`, also reporting whether each row is assigned and
/// excluding placeholder tokens (`<|reserved_N|>`, `<unusedN>`).
pub fn pieces_from_tokenizer_json_typed(
    json: &str,
    vocab_size: usize,
) -> anyhow::Result<(TokenizerFlavor, Vec<Vec<u8>>, Vec<bool>, Vec<bool>)> {
    use anyhow::Context;
    let t: serde_json::Value = serde_json::from_str(json).context("tokenizer.json")?;
    let flavor = detect_flavor_from_json(&t);
    let dec = crate::vocab::byte_decoder();
    let mut pieces: Vec<Vec<u8>> = vec![Vec::new(); vocab_size];
    let mut added = vec![false; vocab_size];
    let mut assigned = vec![false; vocab_size];

    if flavor == TokenizerFlavor::SentencePieceUnigram {
        if let Some(arr) = t["model"]["vocab"].as_array() {
            for (id, entry) in arr.iter().enumerate() {
                if id >= vocab_size {
                    break;
                }
                if let Some(tok) = entry.as_array().and_then(|a| a.first()).and_then(|v| v.as_str()) {
                    pieces[id] = decode_piece_for_flavor(tok, flavor, &dec);
                    assigned[id] = !is_placeholder_token(tok);
                }
            }
        }
    } else if let Some(obj) = t["model"]["vocab"].as_object() {
        for (tok, id) in obj {
            let id = id.as_u64().context("vocab id")? as usize;
            if id >= vocab_size {
                continue;
            }
            pieces[id] = decode_piece_for_flavor(tok, flavor, &dec);
            assigned[id] = !is_placeholder_token(tok);
        }
    }
    if let Some(arr) = t["added_tokens"].as_array() {
        for a in arr {
            let id = a["id"].as_u64().context("added id")? as usize;
            if id < vocab_size {
                let content = a["content"].as_str().unwrap_or("");
                pieces[id] = content.as_bytes().to_vec();
                let placeholder = is_placeholder_token(content);
                added[id] = !placeholder;
                assigned[id] = !placeholder;
            }
        }
    }
    Ok((flavor, pieces, added, assigned))
}

/// Select rows within an optional `universe` (e.g. a statically trimmed artifact's active subset).
pub fn select_from_universe(
    kinds: &[Kind],
    domain: &BTreeSet<u32>,
    rule: TrimRule,
    langs: Langs,
    universe: Option<&[u32]>,
) -> (Vec<u32>, Vec<(&'static str, usize)>) {
    let Some(uni) = universe else {
        return select_from(kinds, domain, rule, langs);
    };
    let mut in_uni = vec![false; kinds.len()];
    for &id in uni {
        if (id as usize) < kinds.len() {
            in_uni[id as usize] = true;
        }
    }
    if rule == TrimRule::Off {
        let mut all_u: Vec<u32> = uni.iter().copied().filter(|&id| (id as usize) < kinds.len()).collect();
        all_u.sort_unstable();
        all_u.dedup();
        let n = all_u.len();
        return (all_u, vec![("off", n)]);
    }
    let (base, mut tiers) = select_from(kinds, domain, rule, langs);
    let mut keep: BTreeSet<u32> = base.into_iter().filter(|&i| in_uni[i as usize]).collect();
    let target = keep.len().next_multiple_of(ALIGN).min(uni.len());
    let before_pad = keep.len();
    for &i in uni {
        if keep.len() >= target {
            break;
        }
        if (i as usize) < kinds.len() {
            keep.insert(i);
        }
    }
    tiers.push(("universe_pad", keep.len().saturating_sub(before_pad)));
    (keep.into_iter().collect(), tiers)
}

/// Compact 3-tier ASCII + frequency-ranked selection (Phase 1-1 D3 / Phase 2-1 D3).
/// Starvation-free proportional 3-tier allocation preventing zeroed domain, CJK, or ASCII quotas.
pub fn select_compact_ranked(
    kinds: &[Kind],
    domain: &BTreeSet<u32>,
    prompt_ids: &BTreeSet<u32>,
    ascii_core: &BTreeSet<u32>,
    langs: Langs,
    target_k: usize,
    freq_order: Option<&[u32]>,
) -> (Vec<u32>, Vec<(&'static str, usize)>) {
    let vocab = kinds.len();
    if vocab == 0 {
        return (Vec::new(), Vec::new());
    }
    let target_k = target_k.max(ALIGN).next_multiple_of(ALIGN).min(vocab);
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    let mut tiers: Vec<(&'static str, usize)> = Vec::new();
    let mut add = |name: &'static str, iter: &mut dyn Iterator<Item = u32>, keep: &mut BTreeSet<u32>, cap: Option<usize>| {
        let before = keep.len();
        for i in iter {
            if cap.is_some_and(|c| keep.len() >= c) {
                break;
            }
            if (i as usize) < vocab {
                keep.insert(i);
            }
        }
        tiers.push((name, keep.len() - before));
    };

    let cjk_only = |k: Kind| matches!(k, Kind::Script { cjk: true, hangul: false, kana: false, arabic: false, latin: false });
    let has_core12_non_cjk = |k: Kind| match k {
        Kind::Script { hangul, kana, arabic, latin, .. } => {
            (langs.hangul && hangul) || (langs.kana && kana) || (langs.arabic && arabic) || (langs.latin && latin)
        }
        _ => false,
    };

    // Tier 1: Special + Byte(256) + Partial + prompt_ids + ascii_core + active non-CJK script rows
    add(
        "tier1_core",
        &mut (0..vocab as u32).filter(|&i| matches!(kinds[i as usize], Kind::Special | Kind::Byte | Kind::Partial)),
        &mut keep,
        None,
    );
    add("tier1_prompt", &mut prompt_ids.iter().copied(), &mut keep, None);
    add("tier1_ascii_core", &mut ascii_core.iter().copied(), &mut keep, None);
    add(
        "tier1_script_non_cjk",
        &mut (0..vocab as u32).filter(|&i| {
            let k = kinds[i as usize];
            if langs.other {
                has_core12_non_cjk(k)
            } else {
                langs.keeps_script(k) && !cjk_only(k)
            }
        }),
        &mut keep,
        None,
    );

    let effective_target = target_k.max(keep.len()).next_multiple_of(ALIGN).min(vocab);
    let headroom = effective_target.saturating_sub(keep.len());
    let desired_en = if langs.cjk { 8_192 } else { 6_144 }.min(effective_target / 4);
    let en_reserve = desired_en.min(headroom * 2 / 5);
    let tier2_cap = effective_target.saturating_sub(en_reserve);
    let tier2_room = tier2_cap.saturating_sub(keep.len());

    // Tier 2: Proportional Domain + CJK guarantee (zero starvation even at K=12,288)
    let cjk_min_reserve = if langs.cjk {
        tier2_room / 2
    } else if langs.hanja {
        CJK_COMMON.min(tier2_room / 3)
    } else {
        0
    };
    let domain_first_cap = tier2_cap.saturating_sub(cjk_min_reserve);
    add("tier2_domain", &mut domain.iter().copied(), &mut keep, Some(domain_first_cap));
    if langs.cjk || langs.hanja {
        let cjk_cap = if langs.cjk {
            tier2_cap
        } else {
            (keep.len() + CJK_COMMON).min(tier2_cap)
        };
        if let Some(order) = freq_order {
            add(
                "tier2_cjk_ranked",
                &mut order.iter().copied().filter(|&i| (i as usize) < vocab && cjk_only(kinds[i as usize])),
                &mut keep,
                Some(cjk_cap),
            );
        } else {
            add(
                "tier2_cjk_merge",
                &mut (0..vocab as u32).filter(|&i| cjk_only(kinds[i as usize])),
                &mut keep,
                Some(cjk_cap),
            );
        }
    }
    if keep.len() < tier2_cap {
        add("tier2_domain_spill", &mut domain.iter().copied(), &mut keep, Some(tier2_cap));
    }

    // Tier 3: Remaining Ascii + Neutral + fallback scripts up to effective_target
    if let Some(order) = freq_order {
        add(
            "tier3_ascii_ranked",
            &mut order.iter().copied().filter(|&i| {
                (i as usize) < vocab && (matches!(kinds[i as usize], Kind::Ascii | Kind::Neutral) || langs.keeps_script(kinds[i as usize]))
            }),
            &mut keep,
            Some(effective_target),
        );
    } else {
        add(
            "tier3_ascii_merge",
            &mut (0..vocab as u32).filter(|&i| matches!(kinds[i as usize], Kind::Ascii | Kind::Neutral) || langs.keeps_script(kinds[i as usize])),
            &mut keep,
            Some(effective_target),
        );
    }

    let aligned = keep.len().next_multiple_of(ALIGN).min(vocab);
    add(
        "pad",
        &mut (0..vocab as u32).filter(|&i| kinds[i as usize] == Kind::Ascii).chain(0..vocab as u32),
        &mut keep,
        Some(aligned),
    );
    (keep.into_iter().collect(), tiers)
}

fn partial_matches_langs(piece: &[u8], langs: Langs) -> bool {
    if langs.other || (langs.hangul && langs.kana && langs.cjk && langs.arabic && langs.latin) {
        return true;
    }
    let is_cont_pair = piece.len() <= 2 && piece.iter().all(|&b| (0x80..=0xBF).contains(&b));
    if (langs.hangul || langs.cjk || langs.kana || langs.hanja) && is_cont_pair {
        return true;
    }
    if langs.hangul && piece.iter().any(|&b| (0xEA..=0xED).contains(&b)) {
        return true;
    }
    if (langs.cjk || langs.kana || langs.hanja) && piece.iter().any(|&b| (0xE3..=0xE9).contains(&b) || b == 0xEF) {
        return true;
    }
    if langs.arabic && piece.iter().any(|&b| (0xD8..=0xDB).contains(&b)) {
        return true;
    }
    if langs.latin && piece.iter().any(|&b| (0xC2..=0xC6).contains(&b)) {
        return true;
    }
    false
}

fn latin_diacritics_for(doc_lang: Option<&str>) -> Option<&'static str> {
    match doc_lang?.trim().to_ascii_lowercase().as_str() {
        "de" | "deu" | "german" => Some("ÄÖÜäöüß"),
        "fr" | "fra" | "fre" | "french" => Some("ÀÂÆÇÉÈÊËÎÏÔŒÙÛÜŸàâæçéèêëîïôœùûüÿ"),
        "es" | "spa" | "spanish" => Some("ÁÉÍÓÚÜÑáéíóúüñ¡¿"),
        "it" | "ita" | "italian" => Some("ÀÈÉÌÍÎÒÓÙÚàèéìíîòóùú"),
        "pt" | "por" | "portuguese" | "pt-br" => Some("ÁÂÃÀÇÉÊÍÓÔÕÚáâãàçéêíóôõú"),
        "nl" | "nld" | "dut" | "dutch" => Some("ÄËÏÖÜÉÈÊÍÓÚäëïöüéèêíóú"),
        "cs" | "ces" | "cze" | "czech" => Some("ÁČĎÉĚÍŇÓŘŠŤÚŮÝŽáčďéěíňóřšťúůýž"),
        _ => None,
    }
}

fn is_doc_latin_token(piece: &[u8], kind: Kind, allowed: &str) -> bool {
    if !matches!(kind, Kind::Script { latin: true, hangul: false, kana: false, cjk: false, arabic: false }) {
        return false;
    }
    let Ok(s) = std::str::from_utf8(piece) else { return false };
    let mut has_non_ascii = false;
    for c in s.chars() {
        if !c.is_ascii() {
            has_non_ascii = true;
            if !allowed.contains(c) {
                return false;
            }
        }
    }
    has_non_ascii
}

fn push_quota<F>(
    name: &'static str,
    vocab: usize,
    freq_order: Option<&[u32]>,
    pred: F,
    keep: &mut BTreeSet<u32>,
    tiers: &mut Vec<(&'static str, usize)>,
    add_limit: usize,
    global_cap: usize,
) where
    F: Fn(u32) -> bool,
{
    let target = keep.len().saturating_add(add_limit).min(global_cap);
    let before = keep.len();
    if let Some(order) = freq_order {
        for &id in order {
            if keep.len() >= target {
                break;
            }
            if (id as usize) < vocab && pred(id) {
                keep.insert(id);
            }
        }
    } else {
        for id in 0..vocab as u32 {
            if keep.len() >= target {
                break;
            }
            if pred(id) {
                keep.insert(id);
            }
        }
    }
    tiers.push((name, keep.len() - before));
}

/// Track M precision-preserving compact selector targeting K=10,240..12,288 (< 16,384)
/// with script-filtered partials, diacritic-specific Latin isolation, capped domain priority,
/// and proportional script+ASCII quotas in BPE merge/frequency order.
pub fn select_compact_track_m(
    kinds: &[Kind],
    pieces: &[Vec<u8>],
    domain: &BTreeSet<u32>,
    prompt_ids: &BTreeSet<u32>,
    ascii_core: &BTreeSet<u32>,
    langs: Langs,
    doc_lang: Option<&str>,
    target_k: usize,
    freq_order: Option<&[u32]>,
) -> (Vec<u32>, Vec<(&'static str, usize)>) {
    let vocab = kinds.len();
    if vocab == 0 {
        return (Vec::new(), Vec::new());
    }
    let target_k = target_k.max(ALIGN).next_multiple_of(ALIGN).min(vocab);
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    let mut tiers: Vec<(&'static str, usize)> = Vec::new();

    // Tier 0: Special + Byte(256) + script-filtered Partial + context prompt + 5-variant structural ASCII
    push_quota(
        "tm_tier0_core",
        vocab,
        None,
        |i| {
            matches!(kinds[i as usize], Kind::Special | Kind::Byte)
                || (kinds[i as usize] == Kind::Partial && pieces.get(i as usize).is_some_and(|p| partial_matches_langs(p, langs)))
        },
        &mut keep,
        &mut tiers,
        vocab,
        vocab,
    );
    let before_pr = keep.len();
    for &id in prompt_ids {
        if (id as usize) < vocab {
            keep.insert(id);
        }
    }
    tiers.push(("tm_tier0_prompt", keep.len() - before_pr));
    let before_ac = keep.len();
    for &id in ascii_core {
        if (id as usize) < vocab {
            keep.insert(id);
        }
    }
    tiers.push(("tm_tier0_ascii_core", keep.len() - before_ac));

    // Reserve head cap: at least `target_k` (never truncate Tier 0) aligned to 64
    let eff_k = target_k.max(keep.len()).next_multiple_of(ALIGN).min(vocab);
    let rem0 = eff_k.saturating_sub(keep.len());
    let domain_cap = (rem0 / 16).clamp(384, 768).min(rem0);
    push_quota(
        "tm_domain_prio",
        vocab,
        freq_order,
        |i| {
            domain.contains(&i)
                && (matches!(kinds[i as usize], Kind::Ascii | Kind::Neutral | Kind::Special | Kind::Byte | Kind::Partial)
                    || langs.keeps_script(kinds[i as usize]))
        },
        &mut keep,
        &mut tiers,
        domain_cap,
        eff_k,
    );

    let rem = eff_k.saturating_sub(keep.len());
    let cjk_only = |k: Kind| matches!(k, Kind::Script { cjk: true, hangul: false, kana: false, arabic: false, latin: false });
    let is_ascii_neutral = |i: u32| matches!(kinds[i as usize], Kind::Ascii | Kind::Neutral);

    if !langs.hangul && !langs.kana && !langs.cjk && !langs.arabic && !langs.latin && !langs.other {
        // English / pure ASCII
        push_quota("tm_en_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, rem, eff_k);
    } else if langs.latin && !langs.hangul && !langs.kana && !langs.cjk && !langs.arabic && !langs.other {
        let lat_cap = rem * 25 / 100;
        if let Some(allowed_ch) = latin_diacritics_for(doc_lang) {
            push_quota(
                "tm_latin_specific",
                vocab,
                freq_order,
                |i| pieces.get(i as usize).is_some_and(|p| is_doc_latin_token(p, kinds[i as usize], allowed_ch)),
                &mut keep,
                &mut tiers,
                lat_cap,
                eff_k,
            );
        } else {
            push_quota(
                "tm_latin_all",
                vocab,
                freq_order,
                |i| matches!(kinds[i as usize], Kind::Script { latin: true, hangul: false, kana: false, cjk: false, arabic: false }),
                &mut keep,
                &mut tiers,
                lat_cap,
                eff_k,
            );
        }
        push_quota("tm_latin_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, eff_k, eff_k);
    } else if langs.kana && langs.cjk && !langs.hangul && !langs.arabic && !langs.other {
        // Japanese: 25% Kana/mixed, 40% pure CJK, >=35% ASCII
        push_quota(
            "tm_ja_kana",
            vocab,
            freq_order,
            |i| matches!(kinds[i as usize], Kind::Script { kana: true, hangul: false, arabic: false, latin: false, .. }),
            &mut keep,
            &mut tiers,
            rem * 25 / 100,
            eff_k,
        );
        push_quota(
            "tm_ja_cjk",
            vocab,
            freq_order,
            |i| cjk_only(kinds[i as usize]),
            &mut keep,
            &mut tiers,
            rem * 40 / 100,
            eff_k,
        );
        push_quota("tm_ja_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, eff_k, eff_k);
    } else if langs.cjk && !langs.kana && !langs.hangul && !langs.arabic && !langs.other {
        // Chinese (zh / zh-tw): 60% CJK, 40% ASCII
        push_quota("tm_zh_cjk", vocab, freq_order, |i| cjk_only(kinds[i as usize]), &mut keep, &mut tiers, rem * 60 / 100, eff_k);
        push_quota("tm_zh_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, eff_k, eff_k);
    } else if langs.hangul && !langs.cjk && !langs.kana && !langs.arabic && !langs.other {
        // Korean: 55% Hangul, small Hanja slice if active, 40-45% ASCII
        push_quota(
            "tm_ko_hangul",
            vocab,
            freq_order,
            |i| matches!(kinds[i as usize], Kind::Script { hangul: true, kana: false, arabic: false, latin: false, .. }),
            &mut keep,
            &mut tiers,
            rem * 55 / 100,
            eff_k,
        );
        if langs.hanja {
            push_quota("tm_ko_hanja", vocab, freq_order, |i| cjk_only(kinds[i as usize]), &mut keep, &mut tiers, 512.min(rem / 12), eff_k);
        }
        push_quota("tm_ko_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, eff_k, eff_k);
    } else if langs.arabic && !langs.hangul && !langs.kana && !langs.cjk && !langs.other {
        // Arabic: 55% Arabic, 45% ASCII
        push_quota(
            "tm_ar_arabic",
            vocab,
            freq_order,
            |i| matches!(kinds[i as usize], Kind::Script { arabic: true, .. }),
            &mut keep,
            &mut tiers,
            rem * 55 / 100,
            eff_k,
        );
        push_quota("tm_ar_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, eff_k, eff_k);
    } else {
        // Multi-script / all12 / any fallback: reserve >=35% ASCII, split rest fairly across active Core-12 scripts
        let script_total = rem * 65 / 100;
        let active_n = [langs.hangul, langs.kana, langs.cjk || langs.hanja, langs.arabic, langs.latin]
            .into_iter()
            .filter(|&b| b)
            .count()
            .max(1);
        let per_script = script_total / active_n;
        if langs.hangul {
            push_quota("tm_multi_ko", vocab, freq_order, |i| matches!(kinds[i as usize], Kind::Script { hangul: true, .. }), &mut keep, &mut tiers, per_script, eff_k);
        }
        if langs.kana {
            push_quota("tm_multi_ja", vocab, freq_order, |i| matches!(kinds[i as usize], Kind::Script { kana: true, .. }), &mut keep, &mut tiers, per_script, eff_k);
        }
        if langs.cjk || langs.hanja {
            let cjk_q = if langs.cjk { per_script } else { per_script.min(CJK_COMMON) };
            push_quota("tm_multi_cjk", vocab, freq_order, |i| cjk_only(kinds[i as usize]), &mut keep, &mut tiers, cjk_q, eff_k);
        }
        if langs.arabic {
            push_quota("tm_multi_ar", vocab, freq_order, |i| matches!(kinds[i as usize], Kind::Script { arabic: true, .. }), &mut keep, &mut tiers, per_script, eff_k);
        }
        if langs.latin {
            push_quota("tm_multi_lat", vocab, freq_order, |i| matches!(kinds[i as usize], Kind::Script { latin: true, .. }), &mut keep, &mut tiers, per_script, eff_k);
        }
        push_quota("tm_multi_ascii", vocab, freq_order, is_ascii_neutral, &mut keep, &mut tiers, eff_k, eff_k);
    }

    // Spill remaining active scripts / domain / ASCII up to eff_k, then align to 64
    if keep.len() < eff_k {
        let spill_room = eff_k.saturating_sub(keep.len());
        push_quota(
            "tm_spill",
            vocab,
            freq_order,
            |i| {
                langs.keeps_script(kinds[i as usize])
                    || domain.contains(&i)
                    || is_ascii_neutral(i)
            },
            &mut keep,
            &mut tiers,
            spill_room,
            eff_k,
        );
    }
    let aligned = keep.len().next_multiple_of(ALIGN).min(vocab);
    let before_pad = keep.len();
    for i in (0..vocab as u32).filter(|&i| kinds[i as usize] == Kind::Ascii).chain(0..vocab as u32) {
        if keep.len() >= aligned {
            break;
        }
        keep.insert(i);
    }
    tiers.push(("pad", keep.len() - before_pad));
    (keep.into_iter().collect(), tiers)
}

/// Hugging Face `tokenizer.json`(바이트 수준 BPE)의 토큰마다 원래 바이트와 추가 토큰 여부. 어휘 크기(`vocab_size`, 임베딩 행 수)
/// 밖의 번호는 버리고, 비어 있는 번호(행은 있는데 토큰이 없음)는 빈 조각 · 특수로 둔다(잘라 내지 않는다).
pub fn pieces_from_tokenizer_json(json: &str, vocab_size: usize) -> anyhow::Result<(Vec<Vec<u8>>, Vec<bool>)> {
    use anyhow::Context;
    let t: serde_json::Value = serde_json::from_str(json).context("tokenizer.json")?;
    let dec = crate::vocab::byte_decoder();
    let mut pieces: Vec<Vec<u8>> = vec![Vec::new(); vocab_size];
    let mut added = vec![true; vocab_size];
    for (tok, id) in t["model"]["vocab"].as_object().context("model.vocab")? {
        let id = id.as_u64().context("vocab id")? as usize;
        if id >= vocab_size {
            continue;
        }
        let bytes: Option<Vec<u8>> = tok.chars().map(|c| dec.get(&c).copied()).collect();
        pieces[id] = bytes.unwrap_or_else(|| tok.as_bytes().to_vec());
        added[id] = false;
    }
    for a in t["added_tokens"].as_array().context("added_tokens")? {
        let id = a["id"].as_u64().context("added id")? as usize;
        if id < vocab_size {
            pieces[id] = a["content"].as_str().unwrap_or("").as_bytes().to_vec();
            added[id] = true;
        }
    }
    Ok((pieces, added))
}

/// 토큰마다의 종류 (`pieces_from_tokenizer_json` 의 결과에서)
pub fn kinds(pieces: &[Vec<u8>], added: &[bool]) -> Vec<Kind> {
    pieces.iter().zip(added).map(|(p, a)| kind_of(p, *a)).collect()
}

/// Deterministic FNV-1a 64-bit tokenizer vocabulary fingerprint used as trim cache key.
pub fn tok_fingerprint(pieces: &[Vec<u8>]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (idx, p) in pieces.iter().enumerate() {
        for &b in &(idx as u32).to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for &b in &(p.len() as u32).to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for &b in p {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_languages() {
        assert_eq!(kind_of(b"a", false), Kind::Byte);
        assert_eq!(kind_of(&[0xEC, 0x9E], false), Kind::Partial);
        assert_eq!(kind_of(" ORDER".as_bytes(), false), Kind::Ascii);
        assert!(matches!(kind_of("주문".as_bytes(), false), Kind::Script { hangul: true, .. }));
        assert_eq!(kind_of("заказ".as_bytes(), false), Kind::Other);
        assert_eq!(kind_of("<|im_start|>".as_bytes(), true), Kind::Special);
        assert_eq!(Langs::of_doc_lang("ko"), Langs::from_codes(&["ko", "en"]).unwrap());
        let ru = Langs::of_doc_lang("ru");
        assert!(ru.other && ru.latin && !ru.hangul && !ru.cjk, "12개 밖 언어는 라틴 확장 + 그 밖 문자: {ru:?}");
        // 판정이 en 이어도(폴란드어) 글의 라틴 확장 글자는 남김
        let pl = Langs::of_doc_lang("en").union(Langs::present("Zamówienie nr 20231, łączna wartość"));
        assert!(pl.latin && !pl.hangul, "{pl:?}");
        assert_eq!(Langs::from_codes(&["all"]).unwrap(), Langs::all());
        let ko_hanja = Langs::of_doc_lang("ko").union(Langs::present("급수 펌프(給水) 압력 주문"));
        assert!(ko_hanja.hangul && ko_hanja.hanja && !ko_hanja.cjk, "{ko_hanja:?}");
        let zh = Langs::of_doc_lang("zh").union(Langs::present("给水泵压力"));
        assert!(zh.cjk && !zh.hanja, "{zh:?}");
        assert!(Langs::any().keeps_script(Kind::Other) && !Langs::all().keeps_script(Kind::Other));
        assert_eq!(Langs::from_codes::<&str>(&[]), None);
        assert_eq!(Langs::of_doc_lang("ko").label(), "hangul");
    }

    #[test]
    fn selection_keeps_bytes_scripts_and_ascii_and_drops_other_scripts() {
        let pieces: Vec<Vec<u8>> = vec![
            b"<s>".to_vec(),
            b"a".to_vec(),
            vec![0xEC, 0x9E],
            b" ORDER".to_vec(),
            b" the".to_vec(),
            "주문".as_bytes().to_vec(),
            "заказ".as_bytes().to_vec(),
            "Größe".as_bytes().to_vec(),
        ];
        let kinds: Vec<Kind> = pieces.iter().enumerate().map(|(i, p)| kind_of(p, i == 0)).collect();
        let (ids, tiers) = select_from(&kinds, &BTreeSet::new(), TrimRule::Full, Langs::from_codes(&["ko", "en"]).unwrap());
        let before_pad: usize = tiers.iter().filter(|(n, _)| *n != "pad").map(|(_, k)| k).sum();
        assert_eq!(before_pad, 6, "ko,en: 특수 · 바이트 · 조각 · ASCII 둘 · 한글 (키릴 · 라틴 확장 빠짐) {tiers:?}");
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        assert!(sorted_subset(&[1, 3], &[0, 1, 2, 3]) && sorted_subset(&[], &[5]) && !sorted_subset(&[1, 4], &[0, 1, 2, 3]) && !sorted_subset(&[2], &[]));
        assert_eq!(TrimRule::parse("full").unwrap(), TrimRule::Full);
        assert_eq!(TrimRule::parse("off").unwrap(), TrimRule::Off);
        assert!(TrimRule::parse("compact").is_err(), "예산을 둔 규칙은 없다 (영어 전부 디코딩)");
        // 도메인 글의 토큰은 문자 체계와 상관없이 남는다 (키릴 6번)
        let domain: BTreeSet<u32> = [6].into_iter().collect();
        let (with_domain, _) = select_from(&kinds, &domain, TrimRule::Full, Langs::from_codes(&["ko", "en"]).unwrap());
        assert!(with_domain.contains(&6));
    }

    #[test]
    fn a_slice_splits_kept_and_cold_rows() {
        let pieces: Vec<Vec<u8>> = (0..200)
            .map(|i| if i % 3 == 0 { "заказ".as_bytes().to_vec() } else { format!(" w{i}").into_bytes() })
            .collect();
        let added = vec![false; pieces.len()];
        let k = kinds(&pieces, &added);
        let s = slice(&k, &BTreeSet::new(), TrimRule::Full, Langs::of_doc_lang("ko"));
        assert_eq!(s.kept.len() + s.cold.len(), 200);
        assert_eq!(s.kept.len() % ALIGN, 0);
        assert!(s.cold.iter().all(|c| !s.kept.contains(c)));
        assert!(s.stats.contains_key("trim_ascii"));
        let off = slice(&k, &BTreeSet::new(), TrimRule::Off, Langs::any());
        assert!(off.cold.is_empty());
    }

    /// 실제 Qwen3.5 토크나이저(어휘 248,320)로 언어마다 남는 행 수 (logis-center 모델 폴더가 있을 때)
    #[test]
    fn qwen35_tokenizer_rows_per_language() {
        let Some(path) = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|d| d.join("logis-center").join("models").join("Qwen3.5-0.8B-Instruct-gguf").join("tokenizer.json"))
            .filter(|p| p.is_file())
        else {
            eprintln!("skipped: no Qwen3.5 tokenizer.json");
            return;
        };
        let (pieces, added) = pieces_from_tokenizer_json(&std::fs::read_to_string(&path).unwrap(), 248_320).unwrap();
        let k = kinds(&pieces, &added);
        let rows = |codes: &[&str]| select_from(&k, &BTreeSet::new(), TrimRule::Full, Langs::from_codes(codes).unwrap()).0.len();
        let (ko, en, ja, all) = (rows(&["ko", "en"]), rows(&["en"]), rows(&["ja", "en"]), rows(&["all"]));
        eprintln!("Qwen3.5 rows: en {en} · ko+en {ko} · ja+en {ja} · all {all} of 248320");
        assert!(en < ko && ko < all && ja < all && all <= 248_320);
        assert_eq!(ko % ALIGN, 0);
    }

    #[test]
    fn langs_iso639_3_codes_do_not_bleed_into_other() {
        let ko = Langs::from_codes(&["kor", "eng"]).unwrap();
        assert!(ko.hangul && !ko.other && !ko.cjk);
        let ja = Langs::from_codes(&["jpn", "eng"]).unwrap();
        assert!(ja.kana && ja.cjk && !ja.other);
        let zh = Langs::from_codes(&["zho", "cmn", "zh-tw"]).unwrap();
        assert!(zh.cjk && !zh.other);
        let ar = Langs::from_codes(&["ara"]).unwrap();
        assert!(ar.arabic && !ar.other);
        let lat = Langs::from_codes(&["deu", "fra", "spa", "ita", "por", "nld", "ces"]).unwrap();
        assert!(lat.latin && !lat.other && !lat.hangul);
    }

    #[test]
    fn compact_track_m_balanced_ja_fr_ko() {
        let mut pieces: Vec<Vec<u8>> = vec![b"<s>".to_vec()];
        for b in 0..=255u8 {
            pieces.push(vec![b]);
        }
        // French + Polish latin tokens
        for w in ["café", "résumé", "Łódź", "ąęćż"] {
            pieces.push(w.as_bytes().to_vec());
        }
        // Kana, CJK, Hangul, and 400 ASCII words
        for i in 0..300usize {
            pieces.push(format!("カタ{i}").into_bytes());
            pieces.push(format!("漢{i}").into_bytes());
            pieces.push(format!("가{i}").into_bytes());
            pieces.push(format!("word{i}").into_bytes());
        }
        let added: Vec<bool> = (0..pieces.len()).map(|i| i == 0).collect();
        let k = kinds(&pieces, &added);

        // French should filter out Polish-specific diacritics when doc_lang="fr"
        let (fr_ids, _) = select_compact_track_m(
            &k,
            &pieces,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            Langs::of_doc_lang("fr"),
            Some("fr"),
            512,
            None,
        );
        let fr_set: BTreeSet<u32> = fr_ids.into_iter().collect();
        let has_pl = (0..pieces.len() as u32).any(|i| pieces[i as usize] == "Łódź".as_bytes() && fr_set.contains(&i));
        assert!(!has_pl, "doc_lang=fr must exclude Polish diacritics during specific quota pass");

        // Japanese K=640 must retain Kana, CJK, AND ASCII without zero starvation
        let (ja_ids, _) = select_compact_track_m(
            &k,
            &pieces,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            Langs::of_doc_lang("ja"),
            Some("ja"),
            640,
            None,
        );
        assert_eq!(ja_ids.len() % ALIGN, 0);
        let kana_kept = ja_ids.iter().filter(|&&id| matches!(k[id as usize], Kind::Script { kana: true, .. })).count();
        let cjk_kept = ja_ids.iter().filter(|&&id| matches!(k[id as usize], Kind::Script { cjk: true, kana: false, .. })).count();
        let asc_kept = ja_ids.iter().filter(|&&id| k[id as usize] == Kind::Ascii).count();
        assert!(kana_kept > 0 && cjk_kept > 0 && asc_kept > 0, "ja must keep kana={kana_kept} cjk={cjk_kept} asc={asc_kept}");

        // select_compact_ranked backward compatibility + no starvation on narrow K=512
        let dom: BTreeSet<u32> = (pieces.len() as u32 - 40..pieces.len() as u32).collect();
        let (cr_ids, _) = select_compact_ranked(&k, &dom, &BTreeSet::new(), &BTreeSet::new(), Langs::of_doc_lang("ja"), 512, None);
        assert_eq!(cr_ids.len() % ALIGN, 0);
        assert_ne!(tok_fingerprint(&pieces), 0);
        assert_eq!(tok_fingerprint(&pieces), tok_fingerprint(&pieces));
    }

    #[test]
    fn target_k_12288_preserves_hangul_without_truncation() {
        let mut pieces: Vec<Vec<u8>> = vec![b"<s>".to_vec()];
        for b in 0..=255u8 {
            pieces.push(vec![b]);
        }
        for i in 0..8000usize {
            pieces.push(format!("한글{i}").into_bytes());
        }
        for i in 0..8000usize {
            pieces.push(format!("tok{i}").into_bytes());
        }
        let added: Vec<bool> = (0..pieces.len()).map(|i| i == 0).collect();
        let k = kinds(&pieces, &added);
        let (ids, _) = select_compact_ranked(
            &k,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            Langs::of_doc_lang("ko"),
            12_288,
            None,
        );
        assert_eq!(ids.len() % ALIGN, 0);
        assert_eq!(ids.len(), 12_288);
        let ko_kept = ids
            .iter()
            .filter(|&&id| matches!(k[id as usize], Kind::Script { hangul: true, .. }))
            .count();
        assert!(ko_kept > 5000, "target_k=12288 truncated Hangul too aggressively: {ko_kept}");
        assert!(TrimRule::parse("compact").is_err());
    }
}
