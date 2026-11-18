//! 앱 설정. 우선순위: 환경 변수 > 설정 파일(config.json) > 기본값.
//! 모델은 앱에 넣지 않는다. 화면의 "모델 내려받기"가 `model_dir`에 받거나(텍스트 전용 574MB / 전체 1.52GB), 사용자가 폴더를 지정한다.
//!
//! config.json 예 (앱 설정 폴더, 예: ~/.config/discount.premium.app/config.json):
//!   { "model_dir": "/home/me/models/embeddinggemma-2", "server_url": "https://premium.discount", "modalities": "text" }
//! modalities: auto(기본: 이미지·오디오 타워는 그 입력이 올 때 올림) | text(텍스트만, 메모리가 적은 기기용)
//! quant: auto(기본: 모바일은 Q8_0, 데스크톱은 F32) | off | q8_0 (텍스트 백본 층 투영 양자화)
//! filter: auto(기본: laya 모델이 있으면 laya, 없으면 사전) | dictionary | laya, filter_model_dir (기본 <model_dir의 부모>/laya-multilingual)

use std::path::{Path, PathBuf};

use premium_discount_app::model_fetch::DEFAULT_BASE_URL;
use premium_discount_app::{FilterSetting, Modalities, QuantSetting};
use serde::Deserialize;

pub const DEFAULT_SERVER_URL: &str = "https://premium.discount";

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub model_dir: PathBuf,
    pub data_dir: PathBuf,
    pub server_url: String,
    /// 어떤 입력을 처리할지 (텍스트 전용이면 이미지·오디오 타워를 올리지 않는다)
    pub modalities: Modalities,
    /// 텍스트 백본 양자화
    pub quant: QuantSetting,
    /// 욕설 필터
    pub filter: FilterSetting,
    /// laya-multilingual 폴더 (화면의 "필터 모델 받기"가 여기에 받는다)
    pub filter_model_dir: PathBuf,
    /// items 초안 모델(Qwen3.5-0.8B) 폴더 (기본 <model_dir의 부모>/qwen3.5-0.8b)
    pub draft_model_dir: PathBuf,
    /// 모델을 받는 주소 (기본 Hugging Face. 리비전·해시는 고정)
    pub model_base_url: String,
    /// 읽은(또는 읽을) 설정 파일. 안내문에 보여 준다
    pub config_path: PathBuf,
}

#[derive(Deserialize, Default)]
struct FileConfig {
    model_dir: Option<String>,
    data_dir: Option<String>,
    server_url: Option<String>,
    modalities: Option<String>,
    quant: Option<String>,
    filter: Option<String>,
    filter_model_dir: Option<String>,
    draft_model_dir: Option<String>,
    model_base_url: Option<String>,
}

/// 빈 문자열은 지정하지 않은 것으로 본다
fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// env는 환경 변수 조회 함수 (테스트에서 바꿔 끼운다). 설정 파일이 깨졌으면 파일 경로가 든 오류 문구를 돌려준다.
pub fn resolve(env: &dyn Fn(&str) -> Option<String>, config_dir: &Path, default_data_dir: &Path) -> Result<Settings, String> {
    let config_path = config_dir.join("config.json");
    let file: FileConfig = match std::fs::read_to_string(&config_path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("설정 파일을 읽지 못했습니다 ({}): {e}", config_path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileConfig::default(),
        Err(e) => return Err(format!("설정 파일을 열지 못했습니다 ({}): {e}", config_path.display())),
    };
    let data_dir = non_empty(env("PD_DATA_DIR")).or(non_empty(file.data_dir)).map(PathBuf::from).unwrap_or_else(|| default_data_dir.to_path_buf());
    let model_dir = non_empty(env("PD_MODEL_DIR"))
        .or(non_empty(file.model_dir))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("models").join("embeddinggemma-2"));
    let server_url = non_empty(env("PD_SERVER_URL")).or(non_empty(file.server_url)).unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    let modalities = Modalities::parse(&non_empty(env("PD_MODALITIES")).or(non_empty(file.modalities)).unwrap_or_default())
        .map_err(|e| format!("{e} ({})", config_path.display()))?;
    let quant = QuantSetting::parse(&non_empty(env("PD_QUANT")).or(non_empty(file.quant)).unwrap_or_default())
        .map_err(|e| format!("{e} ({})", config_path.display()))?;
    let filter = FilterSetting::parse(&non_empty(env("PD_FILTER")).or(non_empty(file.filter)).unwrap_or_default())
        .map_err(|e| format!("{e} ({})", config_path.display()))?;
    let filter_model_dir = non_empty(env("PD_FILTER_MODEL_DIR"))
        .or(non_empty(file.filter_model_dir))
        .map(PathBuf::from)
        .unwrap_or_else(|| model_dir.parent().unwrap_or(Path::new(".")).join("laya-multilingual"));
    let draft_model_dir = non_empty(env("PD_DRAFT_MODEL_DIR"))
        .or(non_empty(file.draft_model_dir))
        .map(PathBuf::from)
        .unwrap_or_else(|| model_dir.parent().unwrap_or(Path::new(".")).join("qwen3.5-0.8b"));
    let model_base_url =
        non_empty(env("PD_MODEL_BASE_URL")).or(non_empty(file.model_base_url)).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    Ok(Settings { model_dir, data_dir, server_url, modalities, quant, filter, filter_model_dir, draft_model_dir, model_base_url, config_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("pd-desktop-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_put_the_model_under_the_data_dir() {
        let (cfg, data) = (temp("def-cfg"), temp("def-data"));
        let s = resolve(&no_env, &cfg, &data).unwrap();
        assert_eq!(s.data_dir, data);
        assert_eq!(s.model_dir, data.join("models").join("embeddinggemma-2"));
        assert_eq!(s.server_url, DEFAULT_SERVER_URL);
        assert_eq!(s.config_path, cfg.join("config.json"));
    }

    #[test]
    fn the_config_file_overrides_defaults_and_the_environment_overrides_the_file() {
        let (cfg, data) = (temp("file-cfg"), temp("file-data"));
        std::fs::write(cfg.join("config.json"), r#"{"model_dir":"/from/file","server_url":"https://file.example","data_dir":"/data/file"}"#).unwrap();
        let s = resolve(&no_env, &cfg, &data).unwrap();
        assert_eq!((s.model_dir.to_str(), s.server_url.as_str(), s.data_dir.to_str()), (Some("/from/file"), "https://file.example", Some("/data/file")));

        let env = |k: &str| match k {
            "PD_MODEL_DIR" => Some("/from/env".to_string()),
            "PD_SERVER_URL" => Some("https://env.example".to_string()),
            _ => None,
        };
        let s = resolve(&env, &cfg, &data).unwrap();
        assert_eq!((s.model_dir.to_str(), s.server_url.as_str()), (Some("/from/env"), "https://env.example"));
        assert_eq!(s.data_dir.to_str(), Some("/data/file"), "환경 변수가 없는 항목은 파일 값");
    }

    #[test]
    fn empty_values_are_treated_as_unset() {
        let (cfg, data) = (temp("empty-cfg"), temp("empty-data"));
        std::fs::write(cfg.join("config.json"), r#"{"model_dir":"  ","server_url":""}"#).unwrap();
        let env = |k: &str| (k == "PD_MODEL_DIR").then(|| "".to_string());
        let s = resolve(&env, &cfg, &data).unwrap();
        assert_eq!(s.model_dir, data.join("models").join("embeddinggemma-2"));
        assert_eq!(s.server_url, DEFAULT_SERVER_URL);
    }

    #[test]
    fn text_only_and_the_model_source_come_from_the_file_or_the_environment() {
        let (cfg, data) = (temp("mod-cfg"), temp("mod-data"));
        let s = resolve(&no_env, &cfg, &data).unwrap();
        assert_eq!((s.modalities, s.model_base_url.as_str()), (Modalities::Auto, DEFAULT_BASE_URL));

        std::fs::write(cfg.join("config.json"), r#"{"modalities":"text","model_base_url":"http://mirror.example"}"#).unwrap();
        let s = resolve(&no_env, &cfg, &data).unwrap();
        assert_eq!((s.modalities, s.model_base_url.as_str()), (Modalities::Text, "http://mirror.example"));

        let env = |k: &str| (k == "PD_MODALITIES").then(|| "auto".to_string());
        assert_eq!(resolve(&env, &cfg, &data).unwrap().modalities, Modalities::Auto, "환경 변수가 파일보다 먼저");

        std::fs::write(cfg.join("config.json"), r#"{"quant":"q8_0"}"#).unwrap();
        assert_eq!(resolve(&no_env, &cfg, &data).unwrap().quant, QuantSetting::Q8_0);
        let env = |k: &str| (k == "PD_QUANT").then(|| "off".to_string());
        assert_eq!(resolve(&env, &cfg, &data).unwrap().quant, QuantSetting::Off);

        // 욕설 필터: 기본은 auto, 모델 폴더는 임베딩 모델 폴더의 형제
        std::fs::write(cfg.join("config.json"), r#"{"model_dir":"/m/embeddinggemma-2"}"#).unwrap();
        let s = resolve(&no_env, &cfg, &data).unwrap();
        assert_eq!(s.filter, FilterSetting::Auto);
        assert_eq!(s.filter_model_dir, PathBuf::from("/m/laya-multilingual"));
        std::fs::write(cfg.join("config.json"), r#"{"filter":"dictionary","filter_model_dir":"/f"}"#).unwrap();
        let s = resolve(&no_env, &cfg, &data).unwrap();
        assert_eq!((s.filter, s.filter_model_dir), (FilterSetting::Dictionary, PathBuf::from("/f")));

        std::fs::write(cfg.join("config.json"), r#"{"modalities":"vision"}"#).unwrap();
        let err = resolve(&no_env, &cfg, &data).unwrap_err();
        assert!(err.contains("modalities") && err.contains("config.json"), "{err}");
    }

    #[test]
    fn a_broken_config_file_is_an_error_that_names_the_file() {
        let (cfg, data) = (temp("bad-cfg"), temp("bad-data"));
        std::fs::write(cfg.join("config.json"), "{ not json").unwrap();
        let err = resolve(&no_env, &cfg, &data).unwrap_err();
        assert!(err.contains("config.json") && err.contains("설정 파일을 읽지 못했습니다"), "{err}");
    }
}
