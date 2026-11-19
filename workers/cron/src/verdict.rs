//! 원본 `verdict.ts`(PD-Verdict v1) 자리: 규칙은 `premium-discount-verdict` crate 를 그대로 쓴다.
//!
//! 원본 verdict.ts 와 crate 는 같은 계약(contracts/verdict_v1.json)을 통과하는 같은 식이다(E · v · 기준선 · 등급 · grade · since).
//! 다른 점 둘을 여기서 맞춘다:
//! - 초월 함수: wasm32 의 libm `pow` · `tanh` 는 V8 과 마지막 자리가 다를 때가 있다. 원본과 같은 비트를 내려고 JS 의
//!   `Math.pow` · `Math.tanh` 를 [`Math`] 로 넘긴다(V8 에서 `2 ** x` 와 `Math.pow(2, x)` 는 같은 값).
//! - 가중치: 원본은 `PARAMS.weights[v.kind] ?? 0` 이라 `constructor` · `toString` 같은 Object.prototype 이름이면 함수가 나와
//!   E · v 가 NaN 이 된다. crate 의 `Params::weight` 는 모르는 종류를 0 으로 본다. 원본 그대로 옮기려고 [`weight_of`] 가 그 이름에 NaN 을 준다.

use premium_discount_verdict::{Math, Params, Tier, TrackParams};
use premium_discount_worker_common::jsv::V;

pub use premium_discount_verdict::RECIPE_V1 as RECIPE;

/// 판정 트랙 (`TRACK`)
pub const TRACK: &str = "pd";

/// `PARAMS` (contracts/verdict_v1.json 의 params — crate 의 시험이 대조한다)
pub fn params() -> &'static Params {
    static P: std::sync::OnceLock<Params> = std::sync::OnceLock::new();
    P.get_or_init(Params::v1)
}

/// `PARAMS.tracks.pd`
pub fn track() -> &'static TrackParams {
    params().track(TRACK).expect("pd 트랙")
}

/// 원본이 쓰는 초월 함수 (wasm: JS 의 Math, 호스트 시험: Rust 표준)
pub fn math() -> &'static Math {
    #[cfg(all(target_arch = "wasm32", not(feature = "libm-probe")))]
    {
        static JS: Math = Math {
            pow: js_sys::Math::pow,
            tanh: js_sys::Math::tanh,
        };
        &JS
    }
    #[cfg(any(not(target_arch = "wasm32"), feature = "libm-probe"))]
    {
        &Math::STD
    }
}

/// Object.prototype 의 속성 이름 (V8). `PARAMS.weights[이 이름]` 은 undefined 가 아니라 함수 · 객체다
const PROTOTYPE_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// `PARAMS.weights[kind] ?? 0` 을 수로. 원본 그대로: Object.prototype 이름이면 함수(또는 객체)가 나와 `weight > 0` 은 거짓,
/// `weight * credit * …` 은 NaN — 그래서 NaN 으로 둔다 (www 는 vote · vote_outcome 만 받으므로 www 를 거친 표에는 생기지 않는다)
pub fn weight_of(kind: &V) -> f64 {
    let key = kind.to_key();
    if let Some(w) = params().weights.get(&key) {
        return *w;
    }
    if PROTOTYPE_KEYS.contains(&key.as_str()) {
        return f64::NAN;
    }
    0.0
}

/// D1 의 tier 글 → 등급. 모르는 글이면 None (원본에서는 어떤 등급과도 같지 않다)
pub fn tier_of(v: &V) -> Option<Tier> {
    match v.as_str()? {
        "premium" => Some(Tier::Premium),
        "neutral" => Some(Tier::Neutral),
        "discount" => Some(Tier::Discount),
        "unrated" => Some(Tier::Unrated),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_follow_the_original_lookup() {
        assert_eq!(weight_of(&V::from("vote")), 1.0);
        assert_eq!(weight_of(&V::from("vote_outcome")), 0.5);
        assert_eq!(weight_of(&V::from("nope")), 0.0);
        assert_eq!(weight_of(&V::Null), 0.0);
        assert!(weight_of(&V::from("constructor")).is_nan());
        assert!(weight_of(&V::from("__proto__")).is_nan());
    }

    #[test]
    fn params_are_v1() {
        assert_eq!(RECIPE, "pd-verdict/1");
        assert_eq!(params().min_evidence, 2.0);
        assert_eq!(track().half_life_ms, 90.0 * 86_400_000.0);
        assert_eq!(tier_of(&V::from("held")), None);
        assert_eq!(tier_of(&V::from("premium")), Some(Tier::Premium));
    }
}
