//! Model-free 12-language morphological stem peeling, Arabic orthography normalization,
//! and numeric-density pseudo-POS tagging when Stanza ONNX models are absent.

/// Normalizes common 12-language language identifiers to short ISO-639-1 codes (`"zh"` for Chinese).
pub fn normalize_core12(lang: &str) -> &'static str {
    match lang.trim().to_ascii_lowercase().as_str() {
        "ko" | "kor" | "korean" => "ko",
        "ja" | "jpn" | "japanese" => "ja",
        "zh" | "zh-hans" | "zh-cn" | "zh-tw" | "zh-hk" | "zh-hant" | "zho" | "cmn"
        | "chinese" | "traditional chinese" | "simplified chinese" => "zh",
        "de" | "deu" | "german" => "de",
        "fr" | "fra" | "fre" | "french" => "fr",
        "es" | "spa" | "spanish" => "es",
        "it" | "ita" | "italian" => "it",
        "pt" | "por" | "portuguese" => "pt",
        "nl" | "nld" | "dut" | "dutch" => "nl",
        "cs" | "ces" | "cze" | "czech" => "cs",
        "ar" | "ara" | "arabic" => "ar",
        _ => "en",
    }
}

/// Peels closed-class inflectional/case suffixes across all 12 core languages (`ko`, `en`,
/// `ja`, `zh`, `de`, `fr`, `es`, `it`, `pt`, `nl`, `cs`, `ar`), preserving minimum stem length.
pub fn closed_tail_peel(word: &str, lang: &str) -> String {
    let w = word.trim();
    if w.is_empty() {
        return String::new();
    }
    let code = normalize_core12(lang);
    let min_chars = match code {
        "zh" => 1,
        "ko" | "ja" | "ar" => 2,
        _ => 3,
    };
    let suffixes: &[&str] = match code {
        "ko" => &[
            "으로부터", "에서부터", "이라고", "입니다", "습니다", "ㅂ니다", "으로써", "으로서",
            "에게는", "한테는", "에서도", "에서는", "까지도", "까지는", "이라는", "이라도",
            "으로", "에서", "에게", "한테", "까지", "부터", "처럼", "보다", "마저", "조차",
            "이랑", "하고", "은", "는", "이", "가", "을", "를", "의", "에", "로", "와", "과", "도", "만",
        ],
        "ja" => &[
            "について", "によって", "からは", "までは", "でした", "ました", "ません",
            "から", "まで", "より", "だけ", "ばかり", "など",
            "は", "が", "を", "に", "へ", "で", "と", "の", "も", "か",
        ],
        "zh" => &["的时候", "的话", "们", "的", "了", "着", "过", "吗", "呢", "吧"],
        "ar" => arabic_suffixes(),
        "de" => &["ungen", "heiten", "keiten", "lichen", "isch", "ern", "ten", "ung", "es", "en", "em", "er", "e", "s"],
        "fr" => &["ements", "ations", "ement", "ation", "euses", "aient", "ions", "es", "er", "ez", "ent", "e", "s"],
        "es" => &["aciones", "amientos", "mente", "ando", "iendo", "aron", "ieron", "es", "as", "os", "s"],
        "it" => &["azioni", "mente", "anno", "iamo", "ato", "ito", "uto", "he", "hi", "e", "i", "a", "o"],
        "pt" => &["acoes", "ações", "mente", "aram", "eram", "iram", "ando", "endo", "es", "as", "os", "s"],
        "nl" => &["heden", "lijke", "lijk", "baar", "heid", "den", "ten", "en", "de", "te", "s"],
        "cs" => &["ostmi", "ovych", "ými", "ami", "ech", "ich", "ích", "ovi", "em", "ou", "ym", "ý", "é", "í", "á", "u", "y", "a", "e"],
        _ => &["ations", "ingly", "edly", "iness", "ment", "tion", "ness", "able", "ible", "ing", "ies", "ied", "ed", "ly", "es", "s"],
    };

    // Arabic also benefits from stripping definite article `ال` / `و` conjunction prefixes if long enough.
    if code == "ar" {
        for form in arabic_bare_forms(w) {
            if let Some(peeled) = peel_once(&form, suffixes, min_chars) {
                return peeled;
            }
        }
    }

    peel_once(w, suffixes, min_chars).unwrap_or_else(|| w.to_string())
}

fn arabic_suffixes() -> &'static [&'static str] {
    &["هما", "كما", "ون", "ين", "ات", "ان", "تم", "تن", "نا", "هم", "هن", "كم", "كن", "ها", "ية", "ه", "ك", "ي"]
}

fn peel_once(w: &str, suffixes: &[&str], min_chars: usize) -> Option<String> {
    let lower = w.to_lowercase();
    for s in suffixes {
        if let Some(stem) = lower.strip_suffix(s) {
            if stem.chars().count() >= min_chars {
                return Some(stem.to_string());
            }
        }
    }
    None
}

/// Strips Arabic diacritics (tashkeel), tatweel, and normalizes hamza/alef/ya/ta-marbuta,
/// returning candidate orthographic bare forms (with optional proclitic `ال` / `و` stripping).
pub fn arabic_bare_forms(word: &str) -> Vec<String> {
    let mut base = String::with_capacity(word.len());
    for c in word.chars() {
        let u = c as u32;
        if (0x064B..=0x065F).contains(&u) || u == 0x0670 || u == 0x0640 {
            continue;
        }
        let norm = match c {
            'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
            'ى' => 'ي',
            'ة' => 'ه',
            'ؤ' => 'و',
            'ئ' => 'ي',
            other => other,
        };
        base.push(norm);
    }
    let mut forms = Vec::new();
    if base.is_empty() {
        return forms;
    }
    forms.push(base.clone());
    for pref in ["وال", "بال", "كال", "فال", "ال", "و", "ب", "ف", "ك", "ل"] {
        if let Some(rest) = base.strip_prefix(pref) {
            if rest.chars().count() >= 2 && !forms.iter().any(|f| f == rest) {
                forms.push(rest.to_string());
            }
        }
    }
    forms
}

/// Lightweight model-free pseudo-POS tag (`"NUM"`, `"PUNCT"`, `"ADP"`, `"NOUN"`) used
/// when ONNX Runtime / Stanza models are absent.
pub fn pseudo_pos_tag(token: &str, _lang: &str) -> &'static str {
    let t = token.trim();
    if t.is_empty() || t.chars().all(|c| !c.is_alphanumeric()) {
        return "PUNCT";
    }
    let total = t.chars().count();
    let digits = t.chars().filter(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits * 2 >= total {
        return "NUM";
    }
    if matches!(
        t.to_ascii_lowercase().as_str(),
        "in" | "on" | "at" | "of" | "for" | "with" | "to" | "from" | "by" | "de" | "du" | "en" | "con" | "mit" | "für"
    ) {
        return "ADP";
    }
    "NOUN"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_tail_peel_covers_12_languages_and_arabic_bare() {
        assert_eq!(normalize_core12("Traditional Chinese"), "zh");
        assert_eq!(closed_tail_peel("말레니아를", "ko"), "말레니아");
        assert_eq!(closed_tail_peel("running", "en"), "runn");
        assert_eq!(closed_tail_peel("注文について", "ja"), "注文");
        assert_eq!(closed_tail_peel("我的", "zh"), "我");
        // Minimum stem length protects short tokens from over-stripping
        assert_eq!(closed_tail_peel("的", "zh"), "的");
        assert_eq!(closed_tail_peel("bestellungen", "de"), "bestell");
        assert_eq!(closed_tail_peel("commandes", "fr"), "command");
        assert_eq!(closed_tail_peel("pedidos", "es"), "pedid");
        assert_eq!(closed_tail_peel("ordini", "it"), "ordin");
        assert_eq!(closed_tail_peel("pedidos", "pt"), "pedid");
        assert_eq!(closed_tail_peel("bestellingen", "nl"), "bestelling");
        assert_eq!(closed_tail_peel("objednávky", "cs"), "objednávk");

        let bare = arabic_bare_forms("وَالْطَّلَبَات");
        assert!(bare.iter().any(|b| b == "طلبات"), "bare forms={bare:?}");
        assert_eq!(pseudo_pos_tag("2026-11", "en"), "NUM");
        assert_eq!(pseudo_pos_tag("---", "en"), "PUNCT");
    }
}
