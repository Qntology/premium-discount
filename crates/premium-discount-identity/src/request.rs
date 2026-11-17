//! 요청 서명 (지갑 갈래의 인증)
//!
//! 서명하는 글은 아래 여섯 줄이다(줄바꿈 `\n`, 끝 줄바꿈 없음). 서버는 시각이 [`MAX_SKEW_MS`] 안이고,
//! 서명에서 되살린 주소가 [`HEADER_ADDRESS`] 와 같을 때만 받는다. 본문은 SHA-256 으로만 들어가므로 글 자체는 짧다.
//!
//! ```text
//! premium.discount request v1
//! address: <소문자 주소>
//! time: <유닉스 ms>
//! method: <대문자 메서드>
//! path: <경로, 쿼리 없이>
//! body-sha256: <본문 바이트의 SHA-256, 소문자 hex>
//! ```

use sha2::{Digest, Sha256};

use crate::eth::{hex, recover_address};

pub const REQUEST_VERSION: &str = "premium.discount request v1";
pub const HEADER_ADDRESS: &str = "x-pd-address";
pub const HEADER_TIME: &str = "x-pd-time";
pub const HEADER_SIGNATURE: &str = "x-pd-signature";
/// 서명 시각과 서버 시각의 허용 차이 (5분)
pub const MAX_SKEW_MS: i64 = 300_000;

/// 서명하는 글
pub fn request_message(address: &str, time_ms: i64, method: &str, path: &str, body: &[u8]) -> String {
    format!(
        "{REQUEST_VERSION}\naddress: {}\ntime: {time_ms}\nmethod: {}\npath: {path}\nbody-sha256: {}",
        address.trim().to_ascii_lowercase(),
        method.trim().to_ascii_uppercase(),
        hex(&Sha256::digest(body)),
    )
}

/// 서명한 요청의 헤더 값
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRequest {
    pub address: String,
    pub time_ms: i64,
    pub signature: String,
}

impl SignedRequest {
    /// (헤더 이름, 값) 셋
    pub fn headers(&self) -> [(&'static str, String); 3] {
        [
            (HEADER_ADDRESS, self.address.clone()),
            (HEADER_TIME, self.time_ms.to_string()),
            (HEADER_SIGNATURE, self.signature.clone()),
        ]
    }

    /// 서버와 같은 확인: 시각 차이가 5분 안이고, 서명에서 되살린 주소가 헤더의 주소와 같다
    pub fn verify(&self, now_ms: i64, method: &str, path: &str, body: &[u8]) -> bool {
        if (now_ms - self.time_ms).abs() > MAX_SKEW_MS {
            return false;
        }
        let message = request_message(&self.address, self.time_ms, method, path, body);
        recover_address(&message, &self.signature).is_some_and(|a| a == self.address.trim().to_ascii_lowercase())
    }
}
