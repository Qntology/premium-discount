//! 지갑: 시드 구문 → 경로 m/44'/60'/0'/0/0 의 개인 키 → 주소

use std::fmt;

use k256::ecdsa::SigningKey;
use zeroize::Zeroizing;

use crate::bip32::{derive, HARDENED};
use crate::bip39::{generate_phrase, normalize_phrase, to_seed, validate, PhraseError};
use crate::eth::{address_of, hash_message, sign_hash};
use crate::request::{request_message, SignedRequest};

/// ethers 의 기본 경로 (`Wallet.createRandom` · `Wallet.fromPhrase`)
pub const DERIVATION_PATH: &str = "m/44'/60'/0'/0/0";
const PATH: [u32; 5] = [44 | HARDENED, 60 | HARDENED, HARDENED, 0, 0];

/// 익명 계정. 시드 구문과 개인 키는 메모리에서 지울 때 0 으로 덮는다
pub struct Wallet {
    phrase: Zeroizing<String>,
    key: SigningKey,
    address: String,
}

impl fmt::Debug for Wallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 시드 구문 · 개인 키는 찍지 않는다
        f.debug_struct("Wallet").field("address", &self.address).finish_non_exhaustive()
    }
}

impl Wallet {
    /// 새 익명 계정 (12단어). 사용자가 시드 구문을 보관해야 한다: 잃으면 되찾을 수 없다
    pub fn create() -> Wallet {
        Wallet::from_phrase(&generate_phrase()).expect("새로 만든 시드 구문은 올바르다")
    }

    /// 시드 구문으로 계정을 되살린다 (다른 기기에서 같은 주소). 대소문자 · 공백은 너그럽게 받는다
    pub fn from_phrase(phrase: &str) -> Result<Wallet, PhraseError> {
        let phrase = Zeroizing::new(normalize_phrase(phrase));
        validate(&phrase)?;
        let seed = to_seed(&phrase);
        let key_bytes = derive(seed.as_ref(), &PATH).ok_or(PhraseError::Derivation)?;
        let key = SigningKey::from_slice(key_bytes.as_ref()).map_err(|_| PhraseError::Derivation)?;
        let address = address_of(key.verifying_key());
        Ok(Wallet { phrase, key, address })
    }

    /// 계정 주소 (`0x` + 소문자 40자)
    pub fn address(&self) -> &str {
        &self.address
    }

    /// 보관용 시드 구문. 만들 때 한 번 보여 주고 화면에 오래 두지 않는다
    pub fn phrase(&self) -> &str {
        &self.phrase
    }

    /// EIP-191 서명 (`ethers` 의 `signMessage` 와 같은 값)
    pub fn sign_message(&self, text: &str) -> String {
        sign_hash(&self.key, &hash_message(text))
    }

    /// 요청 서명: 헤더 `x-pd-address` · `x-pd-time` · `x-pd-signature` 의 값
    pub fn sign_request(&self, time_ms: i64, method: &str, path: &str, body: &[u8]) -> SignedRequest {
        let message = request_message(&self.address, time_ms, method, path, body);
        SignedRequest {
            address: self.address.clone(),
            time_ms,
            signature: self.sign_message(&message),
        }
    }

    #[cfg(test)]
    fn private_key_hex(&self) -> String {
        format!("0x{}", crate::eth::hex(&self.key.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::recover_address;
    use crate::request::{request_message, MAX_SKEW_MS};
    use serde_json::Value;

    // 기준: ethers 로 뽑은 계약 (contracts/gen_wallet_v1.mjs → wallet_v1.json)
    const CONTRACT: &str = include_str!("../../../contracts/wallet_v1.json");

    fn contract() -> Value {
        serde_json::from_str(CONTRACT).unwrap()
    }

    #[test]
    fn phrases_give_the_ethers_keys_and_addresses() {
        let c = contract();
        assert_eq!(c["path"], DERIVATION_PATH);
        for w in c["wallets"].as_array().unwrap() {
            let wallet = Wallet::from_phrase(w["phrase"].as_str().unwrap()).unwrap();
            assert_eq!(wallet.address(), w["address"].as_str().unwrap());
            assert_eq!(wallet.private_key_hex(), w["private_key"].as_str().unwrap());
        }
        // 옮겨 적으며 생긴 대소문자 · 공백은 받는다
        let loose = Wallet::from_phrase("  TEST test test test test test test test test test test   Junk ").unwrap();
        assert_eq!(loose.address(), c["wallets"][0]["address"].as_str().unwrap());
    }

    #[test]
    fn signatures_are_byte_identical_to_ethers() {
        let c = contract();
        let wallet = Wallet::from_phrase(c["wallets"][0]["phrase"].as_str().unwrap()).unwrap();
        for m in c["messages"].as_array().unwrap() {
            let text = m["text"].as_str().unwrap();
            let signature = m["signature"].as_str().unwrap();
            assert_eq!(wallet.sign_message(text), signature, "{text:?}");
            assert_eq!(recover_address(text, signature).as_deref(), Some(wallet.address()));
        }
    }

    #[test]
    fn requests_sign_and_verify_like_the_server() {
        let c = contract();
        assert_eq!(c["max_skew_ms"].as_i64(), Some(MAX_SKEW_MS));
        let wallet = Wallet::from_phrase(c["wallets"][0]["phrase"].as_str().unwrap()).unwrap();
        for r in c["requests"].as_array().unwrap() {
            let (time, method, path) = (r["time_ms"].as_i64().unwrap(), r["method"].as_str().unwrap(), r["path"].as_str().unwrap());
            let body = r["body"].as_str().unwrap().as_bytes();
            assert_eq!(request_message(wallet.address(), time, method, path, body), r["message"].as_str().unwrap());
            let signed = wallet.sign_request(time, method, path, body);
            assert_eq!(signed.signature, r["signature"].as_str().unwrap());
            assert!(signed.verify(time + MAX_SKEW_MS, method, path, body));
            assert!(!signed.verify(time + MAX_SKEW_MS + 1, method, path, body), "5분이 넘으면 받지 않는다");
            assert!(!signed.verify(time, method, path, b"tampered"), "본문이 바뀌면 받지 않는다");
            assert!(!signed.verify(time, method, "/other", body), "경로가 바뀌면 받지 않는다");
            let other = Wallet::from_phrase(c["wallets"][1]["phrase"].as_str().unwrap()).unwrap();
            let forged = SignedRequest { address: other.address().to_string(), ..signed.clone() };
            assert!(!forged.verify(time, method, path, body), "다른 주소를 내세우면 받지 않는다");
            assert_eq!(signed.headers()[0], ("x-pd-address", wallet.address().to_string()));
        }
    }

    #[test]
    fn bad_phrases_are_refused_like_ethers() {
        let c = contract();
        for p in c["invalid_phrases"].as_array().unwrap() {
            assert!(Wallet::from_phrase(p.as_str().unwrap()).is_err(), "{p}");
        }
        assert_eq!(Wallet::from_phrase("test test test").unwrap_err(), PhraseError::WordCount);
        assert_eq!(
            Wallet::from_phrase("test test test test test test test test test test test 장바구니").unwrap_err(),
            PhraseError::UnknownWord
        );
        assert_eq!(
            Wallet::from_phrase("test test test test test test test test test test test test").unwrap_err(),
            PhraseError::Checksum
        );
    }

    #[test]
    fn a_new_wallet_round_trips_through_its_phrase() {
        let a = Wallet::create();
        assert_eq!(a.phrase().split(' ').count(), 12);
        let again = Wallet::from_phrase(a.phrase()).unwrap();
        assert_eq!(again.address(), a.address());
        assert_ne!(Wallet::create().address(), a.address());
        let shown = format!("{a:?}");
        assert!(shown.contains(a.address()));
        assert!(!shown.contains(a.phrase()), "Debug 에 시드 구문이 나오지 않는다");
        assert!(!shown.contains(&a.private_key_hex()[2..]), "Debug 에 개인 키가 나오지 않는다");
    }
}
