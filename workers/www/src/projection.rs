//! 원본 `projection.ts` — 공개 응답용 투영. 원본 주소(from, to)는 D1 에 그대로 두고, 웹에는 표시용 키만 내려준다.

use hmac::{Hmac, Mac};
use premium_discount_worker_common::{Throw, R};
use sha2::Sha256;

/// 작성자 주소 → 표시용 키 (`authorKey(address, secret)`).
/// 서버 비밀 키(AUTHOR_KEY_SECRET)로 HMAC-SHA256 을 계산해 앞 12자리만 쓴다. 비밀이 비었으면(빈 글 · 없음) 원본처럼 던진다.
/// 원본은 async 지만 crypto.subtle 말고는 기다리는 것이 없어 Rust 에서는 바로 계산한다(밖에서 보이는 차이 없음)
pub fn author_key(address: &str, secret: &str) -> R<String> {
    if secret.is_empty() {
        return Err(Throw::error("AUTHOR_KEY_SECRET is not configured"));
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC 는 어떤 길이의 키도 받는다");
    mac.update(premium_discount_worker_common::jsv::to_lower(address).as_bytes());
    let hex = crate::util::hex(&mac.finalize().into_bytes());
    Ok(hex[..12].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_hex_of_the_hmac_of_the_lowercased_address() {
        // node: createHmac('sha256', 'k').update('0xabc').digest('hex').slice(0, 12)
        let a = author_key("0xABC", "k").unwrap();
        assert_eq!(a, author_key("0xabc", "k").unwrap());
        assert_eq!(a, "95ebc8f2605c");
        assert_eq!(author_key("0xabc", "").unwrap_err().message, "AUTHOR_KEY_SECRET is not configured");
    }
}
