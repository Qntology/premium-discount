//! 원본 `index.ts` 의 `export default { async fetch(request, env, ctx) }` 뼈대 (370~404): 모듈 처리기를 정해진 차례로 부르고,
//! 처음으로 응답을 돌려준 것을 낸다. 모두 null 이면 레거시 처리기(legacy.rs, 405~1128)로 간다.
//!
//! 차례 (원본 그대로): session(/session*) → evaluations(/evaluations*) → navtrials(/nav/trials) → analytic(/analytic/*) →
//! graph(/graph*) → legacy. 원본은 이 앞에서 아무것도 하지 않는다(요청을 손대지 않고 그대로 넘긴다).
//! 처리기가 던지면(`Err`) 원본처럼 fetch 밖으로 샌다 — lib.rs 가 JS 예외로 바꿔 거부한다.

#[cfg(target_arch = "wasm32")]
pub async fn fetch(req: &premium_discount_worker_common::web::Req, env: &premium_discount_worker_common::js::Env) -> premium_discount_worker_common::R<premium_discount_worker_common::web::Resp> {
    // 앱 로그인 API (/session*): 기존 로직보다 먼저 처리
    if let Some(r) = crate::session::handle(req, env).await? {
        return Ok(r);
    }
    // 평가 시스템 API (/evaluations): 기존 로직보다 먼저 처리
    if let Some(r) = crate::evaluations::handle(req, env).await? {
        return Ok(r);
    }
    // 접근성 내비게이션 타임트라이얼 API (/nav/trials): 세션 또는 지갑 서명
    if let Some(r) = crate::navtrials::handle(req, env).await? {
        return Ok(r);
    }
    // 웹 행동 분석 수집 (/analytic/events: 수집 스크립트, /analytic/sites: 사이트 주인의 지갑 서명)
    if let Some(r) = crate::analytic::handle(req, env).await? {
        return Ok(r);
    }
    // 그래프 API (/graph*, 공개)
    if let Some(r) = crate::graph::handle(req, env).await? {
        return Ok(r);
    }
    crate::legacy::handle(req, env).await
}
