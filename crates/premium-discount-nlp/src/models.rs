//! 이 기기에서 텍스트 모델을 찾는 곳.
//!
//! 모델마다 순서: 직접 지정한 환경 변수(`PD_*`, 예전 이름 `RAON_*`), 그다음 모델 폴더들 — premium.discount
//! (`%LOCALAPPDATA%\premium.discount\models\…`), Raon Zena(`%LOCALAPPDATA%\RaonZena\models\…`, 앱의 내려받기가 두는 곳),
//! Qntology/logis-center(`%LOCALAPPDATA%\logis-center\models\…`). 이미 같은 파일을 가진 기기에서 두 번 받지 않게 한다.

use std::path::{Path, PathBuf};

/// granite 임베딩 모델 폴더 이름 (Hugging Face 저장소 이름)
pub const GRANITE_DIR: &str = "granite-embedding-97m-multilingual-r2";

/// 모델 폴더를 둘 수 있는 앱 이름 (`%LOCALAPPDATA%\<앱>\models`), 먼저 볼 것부터
pub const APPS: [&str; 3] = ["premium.discount", "RaonZena", "logis-center"];

/// 사용자 데이터 폴더 (`%LOCALAPPDATA%`, `$XDG_DATA_HOME`, `~/.local/share`)
fn data_local() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
}

fn env_path(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|n| std::env::var_os(n).filter(|v| !v.is_empty()).map(PathBuf::from))
}

/// 앱 하나의 모델 폴더 (`%LOCALAPPDATA%\<app>\models`)
pub fn app_models_dir(app: &str) -> Option<PathBuf> {
    data_local().map(|d| d.join(app).join("models"))
}

/// Raon Zena 의 모델 폴더 (예전 이름, [`app_models_dir`]`("RaonZena")`)
pub fn raon_models_dir() -> Option<PathBuf> {
    app_models_dir("RaonZena")
}

/// 찾아볼 모델 폴더, 먼저 볼 것부터: `PD_MODELS_DIR`, `RAON_MODELS_DIR`, 그다음 [`APPS`] 순서
pub fn search_roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["PD_MODELS_DIR", "RAON_MODELS_DIR"].iter().filter_map(|n| env_path(&[n])).collect();
    v.extend(APPS.iter().filter_map(|a| app_models_dir(a)));
    v
}

fn first_with(candidates: impl IntoIterator<Item = PathBuf>, marker: &str) -> Option<PathBuf> {
    candidates.into_iter().find(|d| d.join(marker).is_file())
}

/// granite 폴더(`model.safetensors` 가 있는 곳): `PD_GRANITE_DIR` · `RAON_GRANITE_DIR`, 그다음 찾아볼 폴더들
pub fn granite_dir() -> Option<PathBuf> {
    let env = env_path(&["PD_GRANITE_DIR", "RAON_GRANITE_DIR"]);
    first_with(
        env.into_iter().chain(search_roots().into_iter().map(|r| r.join(GRANITE_DIR))),
        "model.safetensors",
    )
}

/// 언어 `lang` 의 Stanza 폴더(`vocab.json` 이 있는 곳): `PD_STANZA_DIR/<lang>` · `RAON_STANZA_DIR/<lang>`, 그다음 `<root>/stanza/<lang>`
pub fn stanza_dir(lang: &str) -> Option<PathBuf> {
    let env = env_path(&["PD_STANZA_DIR", "RAON_STANZA_DIR"]).map(|d| d.join(lang));
    first_with(
        env.into_iter()
            .chain(search_roots().into_iter().map(|r| r.join("stanza").join(lang))),
        "vocab.json",
    )
}

/// `dir` 에 granite 모델이 있는지
pub fn is_granite_dir(dir: &Path) -> bool {
    ["config.json", "model.safetensors", "tokenizer.json"]
        .iter()
        .all(|f| dir.join(f).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_are_ordered_and_lookups_need_their_marker_file() {
        let roots = search_roots();
        assert!(roots.len() >= APPS.len());
        let pos = |app: &str| roots.iter().position(|r| r.ends_with(PathBuf::from(app).join("models")));
        assert!(pos("premium.discount") < pos("RaonZena"));
        assert!(pos("RaonZena") < pos("logis-center"));
        assert_eq!(raon_models_dir(), app_models_dir("RaonZena"));
        if let Some(d) = granite_dir() {
            assert!(is_granite_dir(&d), "{}", d.display());
        }
        assert!(stanza_dir("no-such-language").is_none());
    }
}
