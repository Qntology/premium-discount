//! 로그인 세션을 이 기기에 저장한다 (앱을 껐다 켜도 로그인이 유지된다).
//!
//! 파일에는 hash와 token이 평문으로 들어간다. 그래서 다른 사용자가 읽지 못하게 파일 권한을 소유자 전용(0600)으로 만든다 (Unix).
//! 운영체제 키체인에 넣는 것이 더 낫지만 이 저장소에는 아직 없다. 디스크 암호화는 사용자 환경에 맡긴다.
//! 쓰기는 임시 파일에 쓴 뒤 이름을 바꾸므로, 도중에 멈춰도 이전 파일이나 새 파일 중 하나만 남는다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::Session;
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    pub hash: String,
    pub token: String,
    /// 로그인한 주소 (표시용)
    #[serde(default)]
    pub address: Option<String>,
}

impl SavedSession {
    pub fn session(&self) -> Session {
        Session { hash: self.hash.clone(), token: self.token.clone() }
    }

    /// 서버 계약(hash 40자리, token 64자리 hex, 전부 0이 아님)에 맞는지 확인한다
    fn validate(&self) -> Result<(), AppError> {
        let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex(&self.hash, 40) || !hex(&self.token, 64) {
            return Err(AppError::Storage("saved session has a malformed hash or token".into()));
        }
        if self.session().is_anonymous() {
            return Err(AppError::Storage("saved session is the anonymous placeholder".into()));
        }
        Ok(())
    }
}

pub struct SessionStore {
    path: PathBuf,
}

fn storage(e: impl std::fmt::Display) -> AppError {
    AppError::Storage(e.to_string())
}

impl SessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 저장된 세션. 파일이 없으면 None이다. 깨졌거나 형식이 틀리면 오류다 (호출하는 쪽이 로그아웃으로 시작한다)
    pub fn load(&self) -> Result<Option<SavedSession>, AppError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(storage(e)),
        };
        let saved: SavedSession = serde_json::from_str(&text).map_err(|e| AppError::Storage(format!("saved session is corrupt: {e}")))?;
        saved.validate()?;
        Ok(Some(saved))
    }

    pub fn save(&self, session: &SavedSession) -> Result<(), AppError> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        session.validate()?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(storage)?;
        }
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut tmp_os = self.path.as_os_str().to_os_string();
        tmp_os.push(format!(".tmp.{}.{seq}", std::process::id()));
        let tmp = PathBuf::from(tmp_os);
        let json = serde_json::to_string(session).map_err(storage)?;
        write_private(&tmp, json.as_bytes()).map_err(storage)?;
        if let Err(e) = crate::utils::atomic_replace(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(storage(e));
        }
        Ok(())
    }

    /// 저장된 세션을 지운다. 이미 없으면 아무것도 하지 않는다
    pub fn clear(&self) -> Result<(), AppError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage(e)),
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::test_support::temp_dir;

    fn sample() -> SavedSession {
        SavedSession { hash: "ab".repeat(20), token: "cd".repeat(32), address: Some("0x00000000000000000000000000000000000000ab".into()) }
    }

    #[test]
    fn a_missing_file_means_no_session() {
        let store = SessionStore::new(temp_dir("sess-missing").join("session.json"));
        assert_eq!(store.load().unwrap(), None);
        store.clear().unwrap();
    }

    #[test]
    fn save_then_load_round_trips_and_clear_removes_it() {
        let dir = temp_dir("sess-roundtrip");
        let store = SessionStore::new(dir.join("nested").join("session.json"));
        store.save(&sample()).unwrap();
        assert_eq!(store.load().unwrap(), Some(sample()));
        assert!(!dir.join("nested").join("session.json.tmp").exists(), "임시 파일이 남지 않는다");
        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_only_by_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("sess-perm");
        let store = SessionStore::new(dir.join("session.json"));
        store.save(&sample()).unwrap();
        let mode = std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode {mode:o}");
        // 덮어써도 권한이 그대로다
        store.save(&sample()).unwrap();
        assert_eq!(std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_or_malformed_files_are_errors_not_sessions() {
        let dir = temp_dir("sess-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let store = SessionStore::new(&path);
        for bad in [
            "not json".to_string(),
            r#"{"hash":"ab","token":"cd"}"#.to_string(),
            serde_json::to_string(&SavedSession { hash: "0".repeat(40), token: "0".repeat(64), address: None }).unwrap(),
            serde_json::to_string(&SavedSession { hash: "zz".repeat(20), token: "cd".repeat(32), address: None }).unwrap(),
        ] {
            std::fs::write(&path, &bad).unwrap();
            assert!(matches!(store.load(), Err(AppError::Storage(_))), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_malformed_session_is_refused() {
        let dir = temp_dir("sess-refuse");
        let store = SessionStore::new(dir.join("session.json"));
        let bad = SavedSession { hash: "short".into(), token: "cd".repeat(32), address: None };
        assert!(store.save(&bad).is_err());
        assert!(!store.path().exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
