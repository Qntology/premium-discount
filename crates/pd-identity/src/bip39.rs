//! BIP-39 시드 구문 (영어 단어표)

use std::fmt;
use std::sync::OnceLock;

use sha2::{Digest, Sha256, Sha512};
use zeroize::Zeroizing;

/// BIP-39 영어 단어표 2048단어 (공식 english.txt 와 같은 파일. 시험이 SHA-256 을 확인한다)
const WORDLIST: &str = include_str!("english.txt");

/// 새로 만드는 시드 구문의 단어 수 (엔트로피 128비트, ethers 기본값)
pub const PHRASE_WORDS: usize = 12;

fn words() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| WORDLIST.lines().collect())
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
    let list = words();
    let indexes = phrase
        .split(' ')
        .map(|w| list.binary_search(&w).map(|i| i as u16).map_err(|_| PhraseError::UnknownWord))
        .collect::<Result<Vec<u16>, _>>()?;
    if ![12, 15, 18, 21, 24].contains(&indexes.len()) {
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
        let digest: String = Sha256::digest(WORDLIST.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda");
        let list = words();
        assert_eq!(list.len(), 2048);
        assert!(list.windows(2).all(|w| w[0] < w[1]), "이진 탐색을 위해 정렬되어 있어야 한다");
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
