use std::sync::Arc;

use crate::error::AppError;

/// 사전 필터와 판정 결과는 공용 crate(`crates/premium-discount-filter`)에 있다. Raon Zena 도 같은 사전을 쓴다
pub use premium_discount_filter::{LocalFilter, Verdict};

/// 욕설 필터. 로컬에서 먼저 걸러, 막힌 입력은 임베딩도 로컬 저장도 서버 전송도 하지 않는다.
/// 기본 구현은 `LocalFilter`이고, 모델 기반 필터(예: convaiinnovations/laya-multilingual)도 이 trait만 구현하면 바꿔 끼울 수 있다.
pub trait ProfanityFilter: Send + Sync {
    fn check(&self, text: &str) -> Result<Verdict, AppError>;
}

impl<T: ProfanityFilter + ?Sized> ProfanityFilter for Arc<T> {
    fn check(&self, text: &str) -> Result<Verdict, AppError> {
        (**self).check(text)
    }
}

/// 실행 중에 바꿔 끼울 수 있는 필터. 셸은 사전 필터로 시작하고, 모델 필터(laya)를 받거나 찾으면 바꾼다.
/// 검사 중인 요청은 바꾸기 전 필터로 끝난다 (Arc를 쥐고 검사한다).
pub struct SwitchFilter {
    inner: std::sync::RwLock<(Arc<dyn ProfanityFilter>, &'static str)>,
}

impl Default for SwitchFilter {
    fn default() -> Self {
        Self::dictionary()
    }
}

impl SwitchFilter {
    /// 사전 필터(LocalFilter)로 시작한다
    pub fn dictionary() -> Self {
        Self { inner: std::sync::RwLock::new((Arc::new(LocalFilter::default()), "dictionary")) }
    }

    pub fn set(&self, filter: Arc<dyn ProfanityFilter>, name: &'static str) {
        *self.inner.write().expect("filter lock") = (filter, name);
    }

    /// 지금 쓰는 필터 이름 ("dictionary" | "laya")
    pub fn name(&self) -> &'static str {
        self.inner.read().expect("filter lock").1
    }
}

impl ProfanityFilter for SwitchFilter {
    fn check(&self, text: &str) -> Result<Verdict, AppError> {
        let filter = self.inner.read().expect("filter lock").0.clone();
        filter.check(text)
    }
}

/// 설정 `filter`: 어떤 욕설 필터를 쓸지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterSetting {
    /// laya 모델이 설치되어 있으면 laya, 없으면 사전 (기본값)
    #[default]
    Auto,
    Dictionary,
    Laya,
}

impl FilterSetting {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "dictionary" | "dict" => Ok(Self::Dictionary),
            "laya" | "model" => Ok(Self::Laya),
            other => Err(format!("filter 값 '{other}'를 알 수 없습니다 (auto, dictionary, laya)")),
        }
    }
}

impl ProfanityFilter for LocalFilter {
    fn check(&self, text: &str) -> Result<Verdict, AppError> {
        Ok(self.judge(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_is_usable_through_arc_and_trait_object() {
        let f: Arc<dyn ProfanityFilter> = Arc::new(LocalFilter::default());
        assert_eq!(f.check("시발").unwrap(), Verdict::Profane);
        assert_eq!(f.check("안녕하세요").unwrap(), Verdict::Clean);
    }

    #[test]
    fn a_switch_filter_starts_with_the_dictionary_and_can_be_replaced_at_run_time() {
        struct Everything;
        impl ProfanityFilter for Everything {
            fn check(&self, _: &str) -> Result<Verdict, AppError> {
                Ok(Verdict::Profane)
            }
        }
        let f = SwitchFilter::dictionary();
        assert_eq!(f.name(), "dictionary");
        assert_eq!(f.check("배송이 빨라요").unwrap(), Verdict::Clean);
        f.set(Arc::new(Everything), "laya");
        assert_eq!(f.name(), "laya");
        assert_eq!(f.check("배송이 빨라요").unwrap(), Verdict::Profane);
        assert_eq!(FilterSetting::parse("").unwrap(), FilterSetting::Auto);
        assert_eq!(FilterSetting::parse("LAYA").unwrap(), FilterSetting::Laya);
        assert!(FilterSetting::parse("strict").is_err());
    }
}
