//! premium.discount 의 도메인 규칙과 명령 계약. 저장소 · HTTP · 모델에 의존하지 않는다.
//!
//! * [`envelope`] – 서버가 받는 `cc`(등록 도메인) 만들기: 도메인, URL 의 호스트, 데스크톱 프로그램 `app://<exe>[/<scene>]`.
//! * [`address`]  – `to` · `bcc` 주소: 웹 클라이언트와 같은 `hashId`(premium-discount-identity), 내용 정규화, 주소 검사.
//! * [`contract`] – premium-discount-app 명령의 인자 · 결과 · 오류 종류 (`app/ui/fixtures/command_contract.json`).

pub mod address;
pub mod contract;
pub mod envelope;
