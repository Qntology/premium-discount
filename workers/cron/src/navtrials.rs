//! 원본 `navtrials.ts` 의 cron 쪽: 기한이 지난 지갑(익명 계정) 타임트라이얼을 지운다.
//!
//! 지갑 기록은 마지막으로 달린 날짜 기준 30일(오늘 포함)까지 둔다. 같은 규칙의 나머지(TRIAL_POLICY · tier)는 www 가 쓴다
//! (crates/premium-discount-nav trial.rs 에 기기 쪽 규칙이 있지만 지갑 보관 기한은 거기 없어 여기 그대로 둔다).

pub const DAY_MS: f64 = 86_400_000.0;
/// `TRIAL_POLICY.walletMaxAgeDays`
pub const WALLET_MAX_AGE_DAYS: f64 = 30.0;

pub const EXPIRE_SQL: &str = "DELETE FROM nav_trials WHERE \"kind\" = 'wallet' AND \"last_day\" < ?1";

/// `dayOf(ms)`
pub fn day_of(ms: f64) -> f64 {
    (ms / DAY_MS).floor()
}

/// `walletOldestDay(nowMs)` — 마지막으로 달린 날이 이보다 앞선 지갑 코스는 지운다
pub fn wallet_oldest_day(now_ms: f64) -> f64 {
    day_of(now_ms) - WALLET_MAX_AGE_DAYS + 1.0
}

/// `expireWalletTrials(db, nowMs)` — 지운 행 수 (`Number(res.meta?.changes ?? 0)`)
#[cfg(target_arch = "wasm32")]
pub async fn expire_wallet_trials(db: &premium_discount_worker_common::V, now_ms: f64) -> premium_discount_worker_common::R<f64> {
    use premium_discount_worker_common::js::{self, Js};
    use premium_discount_worker_common::V;
    let stmt = V::ext(Js(js::call(db, "prepare", &[EXPIRE_SQL.into()])?));
    let res = js::run(&js::bind_exact(&stmt, &[V::Num(wallet_oldest_day(now_ms))])?).await?;
    let meta = res.g("meta")?;
    let changes = if meta.is_nullish() { V::Undef } else { meta.get("changes") };
    Ok(if changes.is_nullish() { 0.0 } else { changes.to_num() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oldest_kept_day_includes_today() {
        assert_eq!(day_of(0.0), 0.0);
        assert_eq!(day_of(DAY_MS - 1.0), 0.0);
        // 2026-01-01 = 20454
        assert_eq!(day_of(20454.0 * DAY_MS + 5.0), 20454.0);
        assert_eq!(wallet_oldest_day(20454.0 * DAY_MS), 20454.0 - 29.0);
    }
}
