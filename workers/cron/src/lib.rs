//! premium-discount-cron — 원본 `workers/cron/src/*.ts`(커밋 0538044)를 같은 동작으로 옮긴 것.
//!
//! 1분마다 `scheduled` 가
//! 1. 평가 대기열을 처리하고([`queue`]: 욕설 검사 → evaluations · Vectorize, 거부 · 재시도 · 실패)
//! 2. 판정을 다시 계산하고([`verdicts`]: PD-Verdict v1, 규칙은 premium-discount-verdict crate)
//! 3. 기한이 지난 지갑 타임트라이얼을 지운다([`navtrials`]).
//!
//! 같은 SQL · 같은 D1 호출 순서 · 같은 JSON(키 순서 · 수 표기) · 같은 예외 경계를 지킨다. 원본에서 밖으로 새던 예외는
//! `scheduled` 의 거부(rejected promise)로 그대로 샌다 — 그래서 `#[event(scheduled)]`(늘 성공으로 끝냄) 대신
//! 같은 이름의 wasm-bindgen 내보내기를 직접 쓴다. 원본을 그대로 옮긴 판은 커밋 56705a3 이고, 그 뒤 고친 버그는 `고침(id):`
//! 주석과 docs/workers-rs.md 의 "고친 버그" 표에 있다(fix-return-await · fix-weight-own).

pub mod moderation;
pub mod nav_verdicts;
pub mod navtrials;
pub mod vector;
pub mod verdict;

#[cfg(target_arch = "wasm32")]
pub mod queue;
#[cfg(target_arch = "wasm32")]
pub mod verdicts;

#[cfg(target_arch = "wasm32")]
mod worker_main {
    use premium_discount_worker_common::js::Env;
    use premium_discount_worker_common::platform::now;
    use premium_discount_worker_common::{console_log, R};

    use crate::nav_verdicts::wasm::{cleanup_batch_receipts, process_nav_verdicts};
    use crate::navtrials::expire_wallet_trials;
    use crate::queue::process_queue;
    use crate::verdicts::process_verdicts;

    /// 원본 `scheduled(_controller, env, _ctx)` + Phase 5 PD-Verdict v2 (`nav` 트랙) 및 멱등 영수증 정리
    pub async fn scheduled(env: &Env) -> R<()> {
        let processed = process_queue(env, now()).await?;
        console_log!("eval queue processed", processed);
        // 대기열 다음에 판정을 처리한다
        let (computed, judged) = process_verdicts(env, now()).await?;
        console_log!("verdicts computed", computed, "judged", judged);
        // Phase 5 Part 1: 타임트라이얼 코스·체크포인트 PD-Verdict v2 (nav 트랙) 판정
        let (nav_computed, nav_judged) = process_nav_verdicts(env, now()).await?;
        if nav_computed > 0.0 || nav_judged > 0.0 {
            console_log!("nav verdicts computed", nav_computed, "judged", nav_judged);
        }
        // 접근성 내비게이션: 기한(30일)이 지난 지갑 코스와 24시간 지난 배치 멱등 영수증을 지운다
        let expired = expire_wallet_trials(&env.get("DB"), now()).await?;
        if expired > 0.0 {
            console_log!("wallet nav trials expired", expired);
        }
        let cleaned = cleanup_batch_receipts(&env.get("DB"), now()).await?;
        if cleaned > 0.0 {
            console_log!("nav batch receipts cleaned", cleaned);
        }
        Ok(())
    }
}

/// worker-build 의 진입 모듈이 부르는 `scheduled` 내보내기 (worker-macros 0.8.7 의 `#[event(scheduled)]` 가 만드는 것과 같은 꼴,
/// 다만 예외를 삼키지 않고 원본처럼 거부로 돌려준다)
#[cfg(target_arch = "wasm32")]
mod _worker_scheduled {
    use ::worker::wasm_bindgen::prelude::wasm_bindgen;
    use ::worker::wasm_bindgen::JsValue;

    #[wasm_bindgen(wasm_bindgen=::worker::wasm_bindgen, wasm_bindgen_futures=::worker::wasm_bindgen_futures, js_sys=::worker::js_sys)]
    pub async fn scheduled(
        _event: ::worker::worker_sys::ScheduledEvent,
        env: ::worker::Env,
        _ctx: ::worker::worker_sys::ScheduleContext,
    ) -> ::std::result::Result<JsValue, JsValue> {
        ::std::panic::AssertUnwindSafe(async move {
            premium_discount_worker_common::platform::install_panic_hook();
            let env = premium_discount_worker_common::js::Env(env);
            match super::worker_main::scheduled(&env).await {
                Ok(()) => Ok(JsValue::UNDEFINED),
                Err(t) if t.name.is_empty() => Err(JsValue::from_str(&t.message)),
                Err(t) => {
                    let e = ::worker::js_sys::Error::new(&t.message);
                    e.set_name(t.name);
                    Err(e.into())
                }
            }
        })
        .await
    }
}
