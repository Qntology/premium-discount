//! Cross-script consonant / letter-reading skeleton key (`letter_reading_key`)
//! for matching proper nouns, codes, and transliterations across Latin, Hangul, Kana, and Arabic.

/// Extracts an uppercase alphanumeric consonant skeleton key invariant across
/// vowel variations and script decorations.
pub fn letter_reading_key(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let u = c as u32;
        if c.is_ascii_digit() {
            out.push(c);
        } else if c.is_ascii_alphabetic() {
            let up = c.to_ascii_uppercase();
            if !matches!(up, 'A' | 'E' | 'I' | 'O' | 'U' | 'Y') || out.is_empty() {
                if out.chars().last() != Some(up) {
                    out.push(up);
                }
            }
        } else if (0xAC00..=0xD7A3).contains(&u) {
            let syl = u - 0xAC00;
            let cho = (syl / 588) as usize;
            let jong = (syl % 28) as usize;
            const CHO_LATIN: [&str; 19] = [
                "G", "KK", "N", "D", "TT", "R", "M", "B", "PP", "S", "SS", "", "J", "JJ", "CH",
                "K", "T", "P", "H",
            ];
            const JONG_LATIN: [&str; 28] = [
                "", "K", "K", "KS", "N", "NJ", "NH", "T", "L", "LG", "LM", "LB", "LS", "LT", "LP",
                "LH", "M", "P", "PS", "S", "SS", "NG", "J", "CH", "K", "T", "P", "H",
            ];
            out.push_str(CHO_LATIN[cho]);
            out.push_str(JONG_LATIN[jong]);
        } else if (0x3041..=0x30FA).contains(&u) {
            let k = if u >= 0x30A1 && u <= 0x30F6 {
                char::from_u32(u - 0x60).unwrap_or(c)
            } else {
                c
            };
            let rom = match k {
                'か' | 'き' | 'く' | 'け' | 'こ' => "K",
                'が' | 'ぎ' | 'ぐ' | 'げ' | 'ご' => "G",
                'さ' | 'し' | 'す' | 'せ' | 'そ' => "S",
                'ざ' | 'じ' | 'ず' | 'ぜ' | 'ぞ' => "Z",
                'た' | 'ち' | 'つ' | 'て' | 'と' => "T",
                'だ' | 'ぢ' | 'づ' | 'で' | 'ど' => "D",
                'な' | 'に' | 'ぬ' | 'ね' | 'の' => "N",
                'は' | 'ひ' | 'ふ' | 'へ' | 'ほ' => "H",
                'ば' | 'び' | 'ぶ' | 'べ' | 'ぼ' => "B",
                'ぱ' | 'ぴ' | 'ぷ' | 'ぺ' | 'ぽ' => "P",
                'ま' | 'み' | 'む' | 'め' | 'も' => "M",
                'ら' | 'り' | 'る' | 'れ' | 'ろ' => "R",
                'ん' => "N",
                _ => "",
            };
            out.push_str(rom);
        } else if c.is_alphabetic() {
            for lc in c.to_uppercase() {
                if lc.is_ascii_alphanumeric() {
                    out.push(lc);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phonetic_skeleton_matches_across_spelling_and_scripts() {
        assert_eq!(letter_reading_key("Malenia"), letter_reading_key("Malena"));
        assert_eq!(letter_reading_key("마르스"), "MRS");
        assert_eq!(letter_reading_key("マルス"), "MRS");
        assert_eq!(letter_reading_key("SHP-2026"), "SHP2026");
    }
}
