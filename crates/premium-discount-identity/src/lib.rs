//! 익명 계정: 사용자가 시드 구문을 보관하는 ethers 지갑
//!
//! 인증은 두 갈래다. 클라우드 세션(익명 아님: 지금의 QR 메일 로그인)과, 이 crate 가 맡는 지갑 갈래(익명)다.
//! 지갑은 기기에서 만들고(BIP-39 영어 12단어, 경로 m/44'/60'/0'/0/0 — ethers `Wallet.createRandom` ·
//! `Wallet.fromPhrase` 와 같다), 사용자가 시드 구문을 직접 보관한다. 다른 기기에서는 시드 구문으로 같은 계정을 되살린다.
//! 요청마다 EIP-191 서명을 붙이고([`Wallet::sign_request`]), 서버는 서명에서 되살린 주소로 확인한다.
//! 이메일 · 전화번호 같은 식별 정보는 없다.
//!
//! 기준은 premium-discount 의 `contracts/wallet_v1.json`(ethers 로 뽑은 값)이다. 시험이 그 값을 그대로 낸다.
//! 서버(TypeScript)는 같은 계약으로 `www/src/auth.ts` 의 `verifyWalletRequest` 를 시험한다.

mod bip32;
mod bip39;
mod eth;
mod request;
mod wallet;

pub use bip39::{generate_phrase, normalize_phrase, PhraseError, PHRASE_WORDS};
pub use eth::{address_of, hash_id, hash_message, recover_address};
pub use request::{request_message, SignedRequest, HEADER_ADDRESS, HEADER_SIGNATURE, HEADER_TIME, MAX_SKEW_MS, REQUEST_VERSION};
pub use wallet::{Wallet, DERIVATION_PATH};
