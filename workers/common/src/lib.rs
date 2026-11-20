//! premium-discount Cloudflare Workers 공용 층 (`premium-discount-worker-common`).
//!
//! 출처: logis-center `workers/common`(logis-worker-common), 커밋 370d67a3. 이제 premium-discount 가 이 사본의 주인이다.
//! logis-center 는 다른 엔진 crate 처럼 나중에 git 리비전으로 이 crate 를 가져다 쓸 예정이다(그때까지 logis-center 쪽은 그대로 둔다).
//!
//! 원본 TypeScript 워커를 같은 동작으로 옮기기 위한 것이다. 동적인 JS 객체를 [`jsv::V`] 로 다루고(참조 공유 · undefined/null ·
//! 던지는 속성 읽기), 바인딩은 [`js`] 가 JS 그대로 부른다.
//!
//! 가져온 것 (시험 · 시험 벡터 포함, 아래 말고는 바이트 그대로):
//! - `jsv` · `platform` · `web` · `aws` · `eth` · `gzip` · `date` · `util`, `tests/aws_eth.rs` · `tests/pako_array.rs` 와 그 JSON
//! - `util::hash_str` 는 logis-schema 를 부르던 것을 같은 코드(premium-discount-identity + 다시 해시)로 옮겨 넣었다
//! - `js`: `bind_exact` · `first` 를 더했고, `to_throw` 는 Error 가 아닌 던진 값을 이름 없는 Throw 로 둔다
//!
//! 가져오지 않은 것: `tables`(logis 지역 · 언어 표) · `dirty_json`(LLM 출력 고치기) — logis 워커에만 쓰인다.

pub mod aws;
pub mod date;
pub mod eth;
pub mod gzip;
pub mod jsv;
pub mod platform;
pub mod util;
pub mod web;

#[cfg(target_arch = "wasm32")]
pub mod js;

pub use jsv::{add, loose_eq, parse, parse_str, strict_eq, stringify, stringify_str, Throw, R, V};
