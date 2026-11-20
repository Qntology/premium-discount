//! 앱이 정하는 Stanza 폴더 규칙 (`set_dir_resolver`). 프로세스 전역이라 시험 바이너리를 따로 둡니다.

#![cfg(feature = "stanza")]

use premium_discount_nlp::stanza::{lang_dir, pipeline, set_dir_resolver};
use std::path::PathBuf;

fn app_models(lang: &str) -> PathBuf {
    PathBuf::from("app-models").join("stanza").join(lang)
}

#[test]
fn app_resolver_decides_the_language_folder() {
    assert!(set_dir_resolver(app_models));
    assert_eq!(lang_dir("ko"), app_models("ko"));
    assert!(!set_dir_resolver(app_models), "규칙은 프로세스에서 한 번만 정합니다");
    // 그 폴더에 변환본이 없으면 다른 모델 폴더로 넘어가지 않고 그 자리를 알려 줍니다
    let err = pipeline("ko").err().expect("변환본 없음").to_string();
    assert!(err.contains("app-models"), "{err}");
}
