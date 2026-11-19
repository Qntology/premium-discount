//! 원본 `navtrials.ts` (698줄) — 접근성 내비게이션 타임트라이얼 (세션 또는 지갑 서명). **1단계에서는 비어 있는 자리(stub)다.**
//!
//! 경로: /nav/trials
//!
//! ## 이음매 (2단계에서 이 파일 하나만 갈아 끼운다)
//!
//! ```ignore
//! #[cfg(target_arch = "wasm32")]
//! pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>>
//! ```
//!
//! 원본 `handleNavTrials(request, env): Promise<Response | null>` 과 같은 뜻이다:
//! - `Ok(Some(resp))` = 응답을 돌려줌, `Ok(None)` = `return null` (이 모듈의 경로가 아님 → handler.rs 가 다음 처리기로 넘긴다),
//! - `Err(Throw)` = 프라미스가 거부됨(원본에서 try 밖으로 샌 예외) → fetch 밖으로 새어 런타임이 500 으로 끝낸다(handler.rs).
//!
//! `req` 는 들어온 요청 전부다(메서드 · 주소 · 머리글 · `cf` · 본문 바이트를 handler 가 미리 읽어 둠). 원본의 `request.json()` ·
//! `request.arrayBuffer()` 는 `req.body` 를 읽으면 된다. 원본의 기본 인자 `nowMs = Date.now()` 는 `platform::now()` 로.
//! 공용 도구: `crate::util`(cors_headers · json_response · empty_response · client_ip · pathname ·
//! evaluations.ts 에서 graph.ts 도 쓰던 validation_error / is_validation · parse_domain · parse_uuid · binds · verdict_summaries), `crate::auth`
//! (read_session_credentials · verified_address · verify_wallet_request · address_from_email), `crate::ratelimit`
//! (is_rate_limited · RETRY_AFTER_SECONDS), `crate::projection::author_key`, `crate::blob`.
//! 차분 시나리오는 `scripts/workers-diff/scenarios/www_navtrials.mjs` 에 넣는다(지금은 빈 목록).

#[cfg(target_arch = "wasm32")]
use premium_discount_worker_common::{js::Env, web::{Req, Resp}, R};

/// 원본 `handleNavTrials(request, env)` — 1단계: 아직 옮기지 않아 늘 `Ok(None)` (요청은 레거시 처리기로 넘어간다)
#[cfg(target_arch = "wasm32")]
pub async fn handle(_req: &Req, _env: &Env) -> R<Option<Resp>> {
    Ok(None)
}
