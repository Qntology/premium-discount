//! 원본 `ratelimit.ts` — Cloudflare Workers Rate Limiting 바인딩(wrangler.json 의 ratelimits)으로 IP 마다 요청을 센다.
//!
//! - 대기열 적재(POST /evaluations) · 투표(POST /evaluations/votes)는 저장 제한기(EVAL_WRITE_LIMITER)를 같이 쓴다.
//! - 검색 · 판정 조회 · 그래프는 읽기 제한기(EVAL_READ_LIMITER), 수집(/analytic/events)은 ANALYTIC_WRITE_LIMITER(없으면 저장 제한기).
//!
//! 키는 클라이언트 IP(CF-Connecting-IP)다. 바인딩이 없거나 IP 를 모르면 제한하지 않고, 바인딩 호출이 실패해도 막지 않는다.
//! 쓰는 쪽: `is_rate_limited(req, &env.get("EVAL_WRITE_LIMITER")).await` (원본 `isRateLimited(request, env.EVAL_WRITE_LIMITER)`).

/// 429 응답의 Retry-After (초). 바인딩의 period 와 같게 둔다
pub const RETRY_AFTER_SECONDS: u32 = 60;

/// `isRateLimited(request, limiter)` — limiter 는 `env[이름]` 값 그대로(없으면 undefined)
#[cfg(target_arch = "wasm32")]
pub async fn is_rate_limited(req: &premium_discount_worker_common::web::Req, limiter: &premium_discount_worker_common::V) -> bool {
    use premium_discount_worker_common::js;
    use premium_discount_worker_common::{console_log, obj, V};

    if !limiter.truthy() {
        return false;
    }
    let ip = crate::util::client_ip(req);
    if ip.is_empty() {
        return false;
    }
    let r: premium_discount_worker_common::R<bool> = async {
        // const { success } = await limiter.limit({ key: ip })
        let out = js::call_async(limiter, "limit", &[js::to_js(&obj! { "key" => V::from(ip.as_str()) })]).await?;
        let success = out.g("success")?;
        Ok(!success.truthy())
    }
    .await;
    match r {
        Ok(limited) => limited,
        Err(err) => {
            console_log!("rate limiter error", err);
            false
        }
    }
}
