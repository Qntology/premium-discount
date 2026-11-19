//! premium-discount www — 원본 `workers/www/src/*.ts`(커밋 35781c0)를 같은 동작으로 옮긴 것 (workers-rs 0.8.7, wasm32).
//!
//! 모듈은 원본 파일과 1:1 이다:
//!
//! | Rust | 원본 | 옮긴 단계 |
//! | --- | --- | --- |
//! | [`handler`] | index.ts 370~404 (처리기 차례) | 1단계 |
//! | [`legacy`] | index.ts 405~1128 (레거시 처리기 — 늘 418) | 1단계 |
//! | [`session`] · [`auth`] · [`ratelimit`] · [`blob`] · [`projection`] · [`render`] | session · auth · ratelimit · blob · projection · renderHtml | 1단계 |
//! | [`util`] | evaluations.ts 의 corsHeaders 등 여러 모듈이 같이 쓰는 것 | 1단계 |
//! | [`evaluations`] | evaluations.ts (저장 대기열 · 검색 · status · 투표 · 대체 보고 · 판정 조회) | 2단계 |
//! | [`navtrials`] | navtrials.ts (/nav/trials 타임트라이얼) | 2단계 |
//! | [`graph`] (+ `graph/view.rs`) | graph.ts (/graph · local · similar · view) | 2단계 |
//! | [`analytic`] (+ `analytic/` event · json · inflate) | analytic.ts (/analytic/events · /analytic/sites) | 2단계 |
//!
//! 같은 SQL · 같은 D1 호출 순서 · 같은 JSON(키 순서 · 수 표기) · 같은 예외 경계를 지킨다. 원본의 버그는 먼저 그대로 옮기고
//! (`// 원본 그대로`), 고친 것은 `고침(id):` 주석과 docs/workers-rs.md 의 "고친 버그" 표에 있다 — 보안 · 경합 고침 둘은
//! 원본과 D1 호출이 다르다: 지갑 서명 한 번만 쓰기(fix-wallet-replay, [`auth`] · migrations/0010)와 타임트라이얼의 조건 건 쓰기
//! (fix-nav-lost-update, [`navtrials`]). 같은지는 scripts/workers-diff/www.mjs 가 고친 원본과 견준다.
//!
//! 진입점: `#[event(fetch)]` 는 처리기의 오류를 500 응답으로 바꿔 삼킨다. 원본은 모듈 처리기 · 레거시의 try 밖에서 던진 예외가
//! fetch 의 거부로 새므로, worker-macros 0.8.7 이 만드는 것과 같은 꼴의 wasm-bindgen 내보내기 `fetch` 를 직접 써서 거부를 돌려준다
//! (cron 의 `scheduled` 와 같은 방식).

pub mod analytic;
pub mod auth;
pub mod blob;
pub mod evaluations;
pub mod graph;
pub mod handler;
pub mod legacy;
pub mod navtrials;
pub mod projection;
pub mod ratelimit;
pub mod render;
pub mod session;
pub mod util;

/// worker-build 의 진입 모듈이 부르는 `fetch` 내보내기 (worker-macros 0.8.7 의 `#[event(fetch)]` 와 같은 꼴,
/// 다만 처리기의 예외를 500 으로 삼키지 않고 원본처럼 거부로 돌려준다)
#[cfg(target_arch = "wasm32")]
mod _worker_fetch {
    use ::worker::wasm_bindgen::prelude::wasm_bindgen;
    use ::worker::wasm_bindgen::JsValue;

    #[wasm_bindgen(wasm_bindgen=::worker::wasm_bindgen, wasm_bindgen_futures=::worker::wasm_bindgen_futures, js_sys=::worker::js_sys)]
    pub async fn fetch(
        req: ::worker::worker_sys::web_sys::Request,
        env: ::worker::Env,
        _ctx: ::worker::worker_sys::Context,
    ) -> ::std::result::Result<JsValue, JsValue> {
        ::std::panic::AssertUnwindSafe(async move {
            premium_discount_worker_common::platform::install_panic_hook();
            let env = premium_discount_worker_common::js::Env(env);
            let req = premium_discount_worker_common::js::req_from(::worker::Request::from(req)).await;
            let thrown = match super::handler::fetch(&req, &env).await {
                Ok(resp) => {
                    return premium_discount_worker_common::js::resp_into(resp)
                        .map(|r| JsValue::from(::worker::worker_sys::web_sys::Response::from(r)))
                        .map_err(|e| JsValue::from(::worker::js_sys::Error::new(&e.to_string())));
                }
                Err(t) => t,
            };
            if thrown.name.is_empty() {
                return Err(JsValue::from_str(&thrown.message));
            }
            let e = ::worker::js_sys::Error::new(&thrown.message);
            e.set_name(thrown.name);
            Err(e.into())
        })
        .await
    }
}
