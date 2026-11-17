//! 고정한 파일(이름 · 바이트 수 · SHA-256)을 내려받을 때의 검증 규칙. HTTP 는 부르는 쪽이 고른다(premium-discount-app 은
//! reqwest 로 이어받기 · 구간 받기를 하고, raon-zena 의 아바타 카탈로그는 ureq 로 받는다). 두 쪽이 같이 지키는 것:
//!
//! 1. 받는 동안에는 `<파일>.part` 에 쓴다 ([`part_path`]).
//! 2. 크기와 SHA-256 이 고정값과 같을 때만 제 이름으로 옮긴다 ([`promote`]). 다르면 `.part` 를 지운다.
//! 3. 이미 있는 파일도 크기와 해시를 다시 보고 나서 믿는다 ([`verified`]).
//! 4. 목록이 적은 파일 경로는 그 폴더 안의 상대 경로여야 한다 ([`safe_relative`]).

use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub use sha2::{Digest, Sha256};

/// 받을 파일 하나 (크기와 SHA-256 고정)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub name: String,
    pub size: u64,
    /// 소문자 16진수 64자
    pub sha256: String,
}

/// `<target>.part`: 받는 중인 파일
pub fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 파일의 `[start, end)` 를 해시에 더한다. 파일이 짧으면 `UnexpectedEof`.
pub fn hash_range(path: &Path, start: u64, end: u64, hasher: &mut Sha256) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut left = end.saturating_sub(start);
    let mut buf = vec![0u8; 1 << 20];
    let cap = buf.len() as u64;
    while left > 0 {
        let n = f.read(&mut buf[..left.min(cap) as usize])?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "파일이 예상보다 짧습니다"));
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(())
}

/// 파일의 앞 `len` 바이트를 해시에 더한다 (끊긴 `.part` 를 이어받기 전에 쓴다)
pub fn hash_prefix(path: &Path, len: u64, hasher: &mut Sha256) -> std::io::Result<()> {
    hash_range(path, 0, len, hasher)
}

/// 파일의 `skip` 바이트 뒤부터 끝까지의 SHA-256
pub fn sha256_file(path: &Path, skip: u64) -> std::io::Result<String> {
    let len = std::fs::metadata(path)?.len();
    let mut hasher = Sha256::new();
    hash_range(path, skip.min(len), len, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

/// 파일이 있고 크기와 SHA-256 이 `size` · `sha256` 과 같은지 (해시는 대소문자를 가리지 않는다)
pub fn verified(path: &Path, size: u64, sha256: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_file() && m.len() == size).unwrap_or(false)
        && sha256_file(path, 0).map(|h| h.eq_ignore_ascii_case(sha256)).unwrap_or(false)
}

/// 다 받은 `.part` 를 확인하고 제 이름으로 옮긴다. 크기나 해시가 다르면 `.part` 를 지우고 실패한다.
pub fn promote(part: &Path, target: &Path, size: u64, sha256: &str) -> std::io::Result<()> {
    let len = std::fs::metadata(part)?.len();
    let fail = |msg: String| {
        let _ = std::fs::remove_file(part);
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, msg))
    };
    if len != size {
        return fail(format!("{}: {len} 바이트를 받았지만 고정값은 {size} 바이트입니다", target.display()));
    }
    let got = sha256_file(part, 0)?;
    if !got.eq_ignore_ascii_case(sha256) {
        return fail(format!("{}: SHA-256 {got} 이 고정값 {sha256} 과 다릅니다", target.display()));
    }
    if target.exists() {
        std::fs::remove_file(target)?;
    }
    std::fs::rename(part, target)
}

/// 목록이 적은 경로가 폴더 안의 평범한 상대 경로인지 (`..`, 루트, 드라이브, 역슬래시, 빈 문자열은 안 된다)
pub fn safe_relative(p: &str) -> Result<(), String> {
    let path = Path::new(p);
    if p.is_empty() || path.is_absolute() || p.contains(':') || p.contains('\\') {
        return Err(format!("{p}: 상대 경로가 아닙니다"));
    }
    if path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{p}: 평범한 상대 경로가 아닙니다"));
    }
    Ok(())
}

/// 소문자 · 대문자 16진수 64자인지
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pd-manifest-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    #[test]
    fn promote_checks_size_and_hash() {
        let d = temp("promote");
        let target = d.join("model.bin");
        let part = part_path(&target);
        assert!(part.ends_with("model.bin.part"));
        std::fs::write(&part, b"hello").unwrap();
        assert!(promote(&part, &target, 4, &sha(b"hello")).is_err());
        assert!(!part.exists(), "a wrong size removes the partial file");
        std::fs::write(&part, b"hello").unwrap();
        assert!(promote(&part, &target, 5, &sha(b"other")).is_err());
        assert!(!part.exists() && !target.exists());
        std::fs::write(&part, b"hello").unwrap();
        promote(&part, &target, 5, &sha(b"hello").to_uppercase()).unwrap();
        assert!(verified(&target, 5, &sha(b"hello")));
        assert!(!verified(&target, 6, &sha(b"hello")));
        assert!(!verified(&d.join("nope"), 5, &sha(b"hello")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ranges_and_prefixes_hash_like_the_whole_file() {
        let d = temp("range");
        let f = d.join("x");
        std::fs::write(&f, b"0123456789").unwrap();
        assert_eq!(sha256_file(&f, 0).unwrap(), sha(b"0123456789"));
        assert_eq!(sha256_file(&f, 4).unwrap(), sha(b"456789"));
        let mut h = Sha256::new();
        hash_prefix(&f, 3, &mut h).unwrap();
        assert_eq!(hex(&h.finalize()), sha(b"012"));
        let mut h = Sha256::new();
        assert!(hash_range(&f, 5, 20, &mut h).is_err(), "past the end");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn listed_paths_stay_inside_the_folder() {
        for ok in ["model.vrm", "eyes/happy.png", "a/b/c.json"] {
            assert!(safe_relative(ok).is_ok(), "{ok}");
        }
        for bad in ["", "../x", "a/../../x", "/etc/x", "C:\\x", "C:x", "a\\b"] {
            assert!(safe_relative(bad).is_err(), "{bad}");
        }
        assert!(is_sha256_hex(&sha(b"")));
        assert!(!is_sha256_hex("abc"));
    }
}
