//! 익명 계정: 사용자가 시드 구문을 보관하는 ethers 지갑
//!
//! 인증은 두 갈래다. 클라우드 세션(익명 아님: 지금의 QR 메일 로그인)과, 이 crate 가 맡는 지갑 갈래(익명)다.
//! 지갑은 기기에서 만들고(BIP-39 영어 12단어, 경로 m/44'/60'/0'/0/0 — ethers `Wallet.createRandom` ·
//! `Wallet.fromPhrase` 와 같다), 사용자가 시드 구문을 직접 보관한다. 다른 기기에서는 시드 구문으로 같은 계정을 되살린다.
//! 요청마다 EIP-191 서명을 붙이고([`Wallet::sign_request`]), 서버는 서명에서 되살린 주소로 확인한다.
//! 이메일 · 전화번호 같은 식별 정보는 없다.
//!
//! 기준은 premium-discount 의 `contracts/wallet_v1.json`(ethers 로 뽑은 값)이다. 시험이 그 값을 그대로 낸다.
//! 서버(TypeScript)는 같은 계약으로 `workers/www/src/auth.ts` 의 `verifyWalletRequest` 를 시험한다.
//!
//! 개인 화면 보호도 이 crate 의 단어표와 지우개를 쓴다: [`detect_seed_phrase_span`] 은 글 속 시드 구문을 공식 2048단어표로
//! 찾고(영어 문장 12낱말은 걸리지 않는다), [`is_bip39_word`] · [`validate_phrase`] 는 낱말 하나 · 구문 하나(체크섬 포함)를
//! 본다. [`secret`] 은 지갑이 쓰는 `zeroize` 로 민감한 글을 0 으로 덮는다 ([`redact_seed_phrases`] 는 시드 구문만 가린다).

mod bip32;
pub mod bip39;
mod eth;
mod request;
pub mod secret;
mod wallet;

pub use bip39::{
    detect_seed_phrase_span, generate_phrase, is_bip39_word, is_valid_phrase, normalize_phrase, seed_phrase_spans, validate_phrase,
    PhraseError, PHRASE_LENGTHS, PHRASE_WORDS, SEED_PHRASE_MIN_WORDS,
};
pub use eth::{address_of, hash_id, hash_message, recover_address};
pub use request::{request_message, SignedRequest, HEADER_ADDRESS, HEADER_SIGNATURE, HEADER_TIME, MAX_SKEW_MS, REQUEST_VERSION};
pub use secret::{redact_seed_phrases, redact_span, wipe_bytes, wipe_string, Zeroize, Zeroizing};
pub use wallet::{Wallet, DERIVATION_PATH};
