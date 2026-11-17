//! SDS(상대 가치 판정)가 쓰는 통계 코어: 합칠 수 있고 뺄 수 있는 평균 · 분산(Welford)과, 스코프 백오프
//! (좁은 스코프 → 넓은 스코프 중 표본이 넉넉한 첫 번째).
//!
//! premium-discount-nav 의 마찰 기준선(`FrictionBaselines`: 영역 → 화면 → 앱 → 전역)과 raon-zena `zena-sds` 의
//! 점수 장부가 같은 수식을 쓴다. 두 쪽이 다른 것은 분산의 분모뿐이라 둘 다 둔다: 마찰 기준선은 모집단 표준편차
//! ([`Moments::population_sd`]), `zena-sds` 는 표본 표준편차([`Moments::sample_sd`])다.
//!
//! 직렬화 모양(`n` · `mean` · `m2`)은 premium-discount-nav 의 예전 `Welford` 와 같다. 기기에 저장된 기준선을 그대로 읽는다.

use serde::{Deserialize, Serialize};

/// 개수 · 평균 · 편차 제곱합 (Welford). 유한하지 않은 값은 넣지 않는다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Moments {
    pub n: u64,
    pub mean: f64,
    pub m2: f64,
}

impl Moments {
    pub fn of(values: &[f64]) -> Moments {
        let mut m = Moments::default();
        for v in values {
            m.push(*v);
        }
        m
    }

    /// 값 하나를 더한다. NaN · 무한대는 무시한다 (하나만 들어가도 평균과 분산이 망가진다).
    pub fn push(&mut self, x: f64) {
        if !x.is_finite() {
            return;
        }
        self.n += 1;
        let d = x - self.mean;
        self.mean += d / self.n as f64;
        self.m2 += d * (x - self.mean);
    }

    /// 다른 분포를 합친다 (Chan 외의 병렬 결합).
    pub fn merge(&mut self, other: &Moments) {
        if other.n == 0 {
            return;
        }
        if self.n == 0 {
            *self = *other;
            return;
        }
        let (na, nb) = (self.n as f64, other.n as f64);
        let n = na + nb;
        let delta = other.mean - self.mean;
        self.mean += delta * (nb / n);
        self.m2 += other.m2 + delta * delta * (na * nb / n);
        self.n += other.n;
    }

    /// 이 분포에서 그 일부(`part`)를 뺀 나머지 (병렬 결합을 거꾸로 푼다). 판정하는 항목 자신의 기록을 기준선에서 뺄 때 쓴다.
    pub fn without(&self, part: &Moments) -> Moments {
        if part.n == 0 {
            return *self;
        }
        if part.n >= self.n {
            return Moments::default();
        }
        let n = self.n - part.n;
        let mean = (self.n as f64 * self.mean - part.n as f64 * part.mean) / n as f64;
        let delta = part.mean - mean;
        let m2 = self.m2 - part.m2 - delta * delta * (n as f64 * part.n as f64) / self.n as f64;
        Moments { n, mean, m2: m2.max(0.0) }
    }

    /// 모집단 분산 (n 으로 나눔). 값이 없으면 0.
    pub fn population_variance(&self) -> f64 {
        if self.n == 0 {
            0.0
        } else {
            (self.m2 / self.n as f64).max(0.0)
        }
    }

    /// 표본 분산 (n − 1 로 나눔). 값이 둘 미만이면 0.
    pub fn sample_variance(&self) -> f64 {
        if self.n < 2 {
            0.0
        } else {
            (self.m2 / (self.n - 1) as f64).max(0.0)
        }
    }

    pub fn population_sd(&self) -> f64 {
        self.population_variance().sqrt()
    }

    pub fn sample_sd(&self) -> f64 {
        self.sample_variance().sqrt()
    }

    /// `(x − 평균) / (표준편차 + eps)` (모집단 표준편차). 값이 없으면 None.
    pub fn z(&self, x: f64, eps: f64) -> Option<f64> {
        (self.n > 0).then(|| (x - self.mean) / (self.population_sd() + eps))
    }
}

/// 스코프 백오프: 좁은 스코프부터 넓은 스코프 순서로 놓인 `levels` 중, 표본 수가 그 스코프의 하한(`min_n`) 이상인 첫 번째.
/// PD-Verdict 의 secondary → primary → global, 내비게이션 마찰의 영역 → 화면 → 앱 → 전역이 같은 규칙이다.
pub fn backoff<L: Copy>(levels: &[(L, Moments)], min_n: impl Fn(L) -> u64) -> Option<(L, Moments)> {
    levels.iter().find(|(l, m)| m.n >= min_n(*l)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn push_matches_the_two_pass_formulas() {
        let xs = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let m = Moments::of(&xs);
        assert_eq!(m.n, 8);
        assert!(close(m.mean, 5.0));
        assert!(close(m.population_sd(), 2.0));
        assert!(close(m.sample_variance(), 32.0 / 7.0));
        assert_eq!(Moments::of(&[3.0]).sample_variance(), 0.0);
        assert_eq!(Moments::default().population_sd(), 0.0);
        assert_eq!(Moments::default().z(1.0, 0.1), None);
        assert!(close(m.z(7.0, 0.0).unwrap(), 1.0));
    }

    #[test]
    fn merge_and_without_are_inverse() {
        let (a, b) = ([1.0, 2.0, 3.0, 10.0], [4.0, 4.5, -2.0]);
        let all: Vec<f64> = a.iter().chain(&b).copied().collect();
        let (ma, mb, mall) = (Moments::of(&a), Moments::of(&b), Moments::of(&all));
        let mut merged = ma;
        merged.merge(&mb);
        assert_eq!(merged.n, mall.n);
        assert!(close(merged.mean, mall.mean) && close(merged.m2, mall.m2));
        let rest = mall.without(&mb);
        assert_eq!(rest.n, ma.n);
        assert!(close(rest.mean, ma.mean) && close(rest.m2, ma.m2));
        assert_eq!(mall.without(&Moments::default()), mall);
        assert_eq!(ma.without(&mall), Moments::default());
        let mut empty = Moments::default();
        empty.merge(&ma);
        assert_eq!(empty, ma);
    }

    #[test]
    fn non_finite_values_are_ignored() {
        let mut m = Moments::of(&[1.0, 2.0]);
        m.push(f64::NAN);
        m.push(f64::INFINITY);
        assert_eq!(m.n, 2);
        assert!(close(m.mean, 1.5));
    }

    #[test]
    fn backoff_takes_the_first_scope_with_enough_samples() {
        let (small, big) = (Moments::of(&[1.0, 2.0]), Moments::of(&[1.0; 30]));
        let levels = [("region", small), ("screen", small), ("global", big)];
        assert_eq!(backoff(&levels, |_| 3).map(|(l, _)| l), Some("global"));
        assert_eq!(backoff(&levels, |l| if l == "screen" { 2 } else { 5 }).map(|(l, _)| l), Some("screen"));
        assert_eq!(backoff(&levels, |_| 100), None);
    }

    #[test]
    fn serialises_like_the_old_welford() {
        let m = Moments { n: 2, mean: 1.5, m2: 0.5 };
        assert_eq!(serde_json::to_value(m).unwrap(), serde_json::json!({"n": 2, "mean": 1.5, "m2": 0.5}));
    }
}
