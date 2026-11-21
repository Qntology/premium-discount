//! BIP-39 시드 구문 (영어 단어표)
//!
//! 지갑을 만들고 되살리는 데 더해, 글 속에 시드 구문이 보이는지 알아보는 데도 쓴다 ([`detect_seed_phrase_span`]).
//! 화면 라벨 · 창 제목 · 채팅 입력에서 시드 구문을 찾아 지우는 쪽(내비게이션의 개인 화면 판정, raon-zena 의 채팅
//! 세탁기)이 "영단어 12개가 이어지면 시드" 같은 거친 규칙 대신 공식 2048단어표를 그대로 쓰게 하려는 것이다.

use std::fmt;
use std::sync::OnceLock;

use sha2::{Digest, Sha256, Sha512};
use zeroize::{Zeroize, Zeroizing};

/// BIP-39 영어 단어표 2048단어 (공식 english.txt 와 같은 파일. 시험이 SHA-256 을 확인한다)
const WORDLIST: &str = include_str!("english.txt");

/// 새로 만드는 시드 구문의 단어 수 (엔트로피 128비트, ethers 기본값)
pub const PHRASE_WORDS: usize = 12;

/// BIP-39 시드 구문이 가질 수 있는 단어 수
pub const PHRASE_LENGTHS: [usize; 5] = [12, 15, 18, 21, 24];

/// [`detect_seed_phrase_span`] 이 시드 구문으로 보는 가장 짧은 낱말 열 (가장 짧은 시드 구문)
pub const SEED_PHRASE_MIN_WORDS: usize = 12;

/// 영어 단어표 낱말의 가장 긴 길이 (3 ~ 8글자, 모두 ASCII 소문자)
const WORD_MAX: usize = 8;

fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| WORDLIST.lines().collect())
}

/// 낱말 하나의 단어표 번호. ASCII 대소문자는 가리지 않는다. 소문자 사본은 스택에 두고 다 쓰면 0 으로 덮는다
/// (시드 낱말이 힙에 남지 않게)
fn word_index(word: &str) -> Option<u16> {
    let bytes = word.as_bytes();
    if !(3..=WORD_MAX).contains(&bytes.len()) || !bytes.iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    let mut lower = [0u8; WORD_MAX];
    for (d, s) in lower.iter_mut().zip(bytes) {
        *d = s.to_ascii_lowercase();
    }
    let found = std::str::from_utf8(&lower[..bytes.len()]).ok().and_then(|w| words().binary_search(&w).ok());
    lower.zeroize();
    found.map(|i| i as u16)
}

/// `word` 가 BIP-39 영어 단어표의 낱말인가. ASCII 대소문자는 가리지 않는다 (`Abandon` · `ZOO` 도 맞다).
/// 공백 · 문장 부호가 붙어 있거나 한글이 섞이면 아니다
pub fn is_bip39_word(word: &str) -> bool {
    word_index(word).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseError {
    /// 12 · 15 · 18 · 21 · 24 단어가 아니다
    WordCount,
    /// 단어표에 없는 낱말이 있다
    UnknownWord,
    /// 체크섬이 맞지 않는다 (낱말을 잘못 옮겨 적었다)
    Checksum,
    /// 경로 파생이 실패했다 (확률 약 2^-127)
    Derivation,
}

impl fmt::Display for PhraseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PhraseError::WordCount => "a seed phrase has 12, 15, 18, 21 or 24 words",
            PhraseError::UnknownWord => "a word is not in the BIP-39 English wordlist",
            PhraseError::Checksum => "the seed phrase checksum does not match",
            PhraseError::Derivation => "the key derivation failed",
        })
    }
}

impl std::error::Error for PhraseError {}

/// 소문자로 바꾸고 공백을 한 칸으로 줄인다 (사용자가 옮겨 적은 시드 구문)
pub fn normalize_phrase(phrase: &str) -> String {
    phrase.split_whitespace().map(|w| w.to_lowercase()).collect::<Vec<_>>().join(" ")
}

/// 정규화한 시드 구문이 단어표 · 단어 수 · 체크섬에 맞는지 본다
pub(crate) fn validate(phrase: &str) -> Result<(), PhraseError> {
    check_indexes(phrase.split(' '))
}

/// 시드 구문을 BIP-39 로 검사한다: 단어 수(12 · 15 · 18 · 21 · 24), 단어표, 체크섬. 대소문자 · 공백은 너그럽게 받는다.
/// 지갑을 만들지 않고 맞는지만 볼 때 쓴다 (채팅 입력의 시드 구문 차단 등). 정규화한 사본을 만들지 않는다
pub fn validate_phrase(phrase: &str) -> Result<(), PhraseError> {
    check_indexes(phrase.split_whitespace())
}

/// [`validate_phrase`] 가 통과하는가
pub fn is_valid_phrase(phrase: &str) -> bool {
    validate_phrase(phrase).is_ok()
}

/// 낱말들을 단어표 번호로 바꿔 단어 수 · 체크섬을 본다. 번호 목록도 시드와 같으므로 다 쓰면 0 으로 덮는다
fn check_indexes<'a>(phrase_words: impl Iterator<Item = &'a str>) -> Result<(), PhraseError> {
    let mut indexes = Zeroizing::new(Vec::with_capacity(24));
    for w in phrase_words {
        indexes.push(word_index(w).ok_or(PhraseError::UnknownWord)?);
    }
    if !PHRASE_LENGTHS.contains(&indexes.len()) {
        return Err(PhraseError::WordCount);
    }
    let total = indexes.len() * 11;
    let checksum_bits = total / 33;
    let entropy_bits = total - checksum_bits;
    let bit = |k: usize| (indexes[k / 11] >> (10 - k % 11)) & 1 == 1;
    let mut entropy = Zeroizing::new(vec![0u8; entropy_bits / 8]);
    for k in 0..entropy_bits {
        if bit(k) {
            entropy[k / 8] |= 0x80 >> (k % 8);
        }
    }
    let hash = Sha256::digest(entropy.as_slice());
    for k in 0..checksum_bits {
        if bit(entropy_bits + k) != ((hash[k / 8] >> (7 - k % 8)) & 1 == 1) {
            return Err(PhraseError::Checksum);
        }
    }
    Ok(())
}

/// 글 속 시드 구문의 바이트 범위 `(시작, 끝)`: BIP-39 단어표 낱말이 12개([`SEED_PHRASE_MIN_WORDS`]) 이상 이어진 첫 구간.
///
/// 낱말 사이에는 공백 · 줄바꿈 · 숫자 · 문장 부호가 끼어도 된다 (지갑 앱이 보여 주는 `1. abandon 2. ability …` 꼴).
/// 단어표에 없는 낱말(한글 낱말 포함)이 하나라도 끼면 거기서 끊긴다. 보통 영어 문장에는 단어표에 없는 `the` · `and` ·
/// `is` · `to` · `of` · `in` 이 거의 늘 끼므로 영어 게임 대사 · 쇼핑몰 설명은 걸리지 않는다.
///
/// 체크섬은 보지 않는다: 한두 낱말을 잘못 옮겨 적은 시드 구문도, 앞뒤에 단어표 낱말이 더 붙은 구문도 비밀이다
/// (체크섬까지 맞는 구문인지는 [`is_valid_phrase`]). 낱말 12개가 모두 단어표에 있는 글만 걸리므로, 체크섬이 맞는
/// 구문은 늘 걸린다.
pub fn detect_seed_phrase_span(text: &str) -> Option<(usize, usize)> {
    phrase_runs(text, true).into_iter().next()
}

/// [`detect_seed_phrase_span`] 의 구간을 모두, 앞에서부터. 겹치지 않는다
pub fn seed_phrase_spans(text: &str) -> Vec<(usize, usize)> {
    phrase_runs(text, false)
}

/// 단어표 낱말이 [`SEED_PHRASE_MIN_WORDS`] 개 이상 이어진 구간들 (`first_only` 면 첫 구간에서 멈춘다)
fn phrase_runs(text: &str, first_only: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut count, mut start, mut end) = (0usize, 0usize, 0usize);
    for (s, e) in word_spans(text) {
        if word_index(&text[s..e]).is_some() {
            if count == 0 {
                start = s;
            }
            count += 1;
            end = e;
            continue;
        }
        if count >= SEED_PHRASE_MIN_WORDS {
            out.push((start, end));
            if first_only {
                return out;
            }
        }
        count = 0;
    }
    if count >= SEED_PHRASE_MIN_WORDS {
        out.push((start, end));
    }
    out
}

/// 낱말의 바이트 범위들: 공백 · ASCII 숫자 · ASCII 문장 부호가 낱말을 가른다 (그 밖의 글자는 낱말에 붙는다)
fn word_spans(text: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let gap = |c: char| c.is_whitespace() || c.is_ascii_digit() || c.is_ascii_punctuation();
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || {
        while chars.next_if(|&(_, c)| gap(c)).is_some() {}
        let &(start, _) = chars.peek()?;
        let mut end = start;
        while let Some((i, c)) = chars.next_if(|&(_, c)| !gap(c)) {
            end = i + c.len_utf8();
        }
        Some((start, end))
    })
}

/// 엔트로피(16 · 20 · 24 · 28 · 32바이트)에서 시드 구문을 만든다
pub(crate) fn phrase_from_entropy(entropy: &[u8]) -> Zeroizing<String> {
    let hash = Sha256::digest(entropy);
    let entropy_bits = entropy.len() * 8;
    let bit = |k: usize| -> u16 {
        if k < entropy_bits {
            ((entropy[k / 8] >> (7 - k % 8)) & 1) as u16
        } else {
            let j = k - entropy_bits;
            ((hash[j / 8] >> (7 - j % 8)) & 1) as u16
        }
    };
    let list = words();
    let count = (entropy_bits + entropy_bits / 32) / 11;
    let mut out = Zeroizing::new(String::new());
    for w in 0..count {
        let index = (0..11).fold(0u16, |acc, b| (acc << 1) | bit(w * 11 + b));
        if w > 0 {
            out.push(' ');
        }
        out.push_str(list[index as usize]);
    }
    out
}

/// 새 시드 구문 (운영체제 난수 128비트 → 12단어)
pub fn generate_phrase() -> Zeroizing<String> {
    let mut entropy = Zeroizing::new([0u8; 16]);
    getrandom::fill(entropy.as_mut()).expect("운영체제 난수를 읽을 수 없다");
    phrase_from_entropy(entropy.as_ref())
}

/// BIP-39 시드: PBKDF2-HMAC-SHA512(시드 구문, "mnemonic", 2048번, 64바이트). 영어 단어표는 NFKD 정규화가 필요 없다
pub(crate) fn to_seed(phrase: &str) -> Zeroizing<[u8; 64]> {
    let mut seed = Zeroizing::new([0u8; 64]);
    pbkdf2::pbkdf2_hmac::<Sha512>(phrase.as_bytes(), b"mnemonic", 2048, seed.as_mut());
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wordlist_is_the_official_english_list() {
        // 공식 english.txt 는 LF 다. Windows 체크아웃(core.autocrlf)이 CRLF 로 바꿔도 같은 목록으로 본다
        let digest: String = Sha256::digest(WORDLIST.replace("\r\n", "\n").as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda");
        let list = words();
        assert_eq!(list.len(), 2048);
        assert!(list.windows(2).all(|w| w[0] < w[1]), "이진 탐색을 위해 정렬되어 있어야 한다");
    }

    const VECTOR: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";

    #[test]
    fn words_are_looked_up_in_the_official_list_ignoring_ascii_case() {
        assert!(is_bip39_word("abandon"));
        assert!(is_bip39_word("Abandon"));
        assert!(is_bip39_word("ZOO"));
        for w in ["the", "and", "is", "to", "of", "in", "abandons", "", "ab", "abandon.", " abandon", "장바구니", "zoo1", "ａｂａｎｄｏｎ"] {
            assert!(!is_bip39_word(w), "{w:?}");
        }
    }

    #[test]
    fn phrases_are_validated_with_their_checksum() {
        let twelve = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        assert_eq!(validate_phrase(twelve), Ok(()));
        assert!(is_valid_phrase(&twelve.to_uppercase().replace(' ', "  \n ")), "대소문자 · 공백은 너그럽게");
        // BIP-39 공식 시험 벡터: 엔트로피 0x00 × 32 → "abandon × 23 art", 0x7f × 16 → VECTOR
        let twenty_four = phrase_from_entropy(&[0u8; 32]);
        assert_eq!(twenty_four.split(' ').count(), 24);
        assert!(twenty_four.ends_with(" art"));
        assert!(is_valid_phrase(&twenty_four));
        assert!(is_valid_phrase(VECTOR));
        assert_eq!(validate_phrase("test test test test test test test test test test test test"), Err(PhraseError::Checksum));
        assert_eq!(validate_phrase("abandon abandon abandon"), Err(PhraseError::WordCount));
        assert_eq!(validate_phrase(""), Err(PhraseError::WordCount));
        assert_eq!(
            validate_phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon 장바구니"),
            Err(PhraseError::UnknownWord)
        );
    }

    #[test]
    fn seed_phrase_span_catches_bip39_runs_but_not_plain_english() {
        let text = format!("내 시드: {VECTOR} 꼭 보관");
        let (s, e) = detect_seed_phrase_span(&text).unwrap();
        assert_eq!(&text[s..e], VECTOR);
        // 지갑 앱이 보여 주는 번호 붙은 칸
        let grid = "1. abandon 2. abandon 3. abandon 4. abandon\n5. abandon 6. abandon 7. abandon 8. abandon\n9. abandon 10. abandon 11. abandon 12. about";
        let (s, e) = detect_seed_phrase_span(grid).unwrap();
        assert_eq!((&grid[s..s + 7], &grid[e - 5..e]), ("abandon", "about"));
        // 체크섬이 틀려도(옮겨 적다 틀림), 대문자로 시작해도 비밀이다
        assert!(detect_seed_phrase_span("Test test test test test test test test test test test test").is_some());
        // 보통 영어 문장은 단어표에 없는 낱말(the · and · your · defeat …)이 끼어 끊긴다
        for plain in [
            "The quick brown fox jumps over the lazy dog and then runs into the forest to hide",
            "Please enter your password to continue and click the button below to sign in now",
            "Defeat the boss to unlock the next level and earn a legendary sword for your hero",
        ] {
            assert_eq!(detect_seed_phrase_span(plain), None, "{plain}");
        }
        // 11낱말은 시드 구문이 아니다
        assert_eq!(detect_seed_phrase_span("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"), None);
        // 단어표에 없는 낱말이 끼면 두 구간이다
        let two = format!("{VECTOR} 그리고 {VECTOR}");
        let spans = seed_phrase_spans(&two);
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|&(s, e)| &two[s..e] == VECTOR));
        assert_eq!(seed_phrase_spans("no seed here"), vec![]);
    }

    #[test]
    fn entropy_round_trips_through_the_phrase() {
        // BIP-39 공식 시험 벡터: 엔트로피 0x00 × 16 → "abandon … about"
        let phrase = phrase_from_entropy(&[0u8; 16]);
        assert_eq!(phrase.as_str(), "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about");
        assert_eq!(validate(&phrase), Ok(()));
        let generated = generate_phrase();
        assert_eq!(generated.split(' ').count(), PHRASE_WORDS);
        assert_eq!(validate(&generated), Ok(()));
        assert_eq!(normalize_phrase("  Test   TEST\ttest "), "test test test");
    }
}
