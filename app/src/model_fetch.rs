//! 앱이 모델(google/embeddinggemma-2)을 내려받는다. Hugging Face 리비전을 고정하고 모든 바이트를 SHA-256으로 확인한다.
//!
//! 두 가지 설치:
//! - **텍스트 전용** (`model-text.safetensors`, 약 574MB): 원본 파일의 헤더(171KB)와 `language_model.*` 텐서 구간
//!   (원본 안에서 한 덩어리, 542MB)만 Range 요청으로 받아 새 safetensors 파일로 조립한다. 이미지·오디오 타워 945MB는 받지 않는다.
//! - **전체** (`model.safetensors`, 1.49GB): 원본 그대로.
//!
//! 둘 다 `tokenizer.json`(32MB)을 함께 받는다.
//!
//! 받는 중에는 `<파일>.part`에 쓰고, 끝까지 받아 해시가 맞을 때만 이름을 바꾼다 (반쯤 받은 파일을 모델로 열 일이 없다).
//! 끊기면 같은 `.part`에서 이어받는다. 취소해도 `.part`는 남아 다음에 이어받는다.
//! 받는 주소는 바꿀 수 있지만(`PD_MODEL_BASE_URL`, 시험용 서버) 해시가 고정이라 다른 내용은 설치되지 않는다.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::embeddinggemma::{TEXT_WEIGHTS_FILE, TOKENIZER_FILE, WEIGHTS_FILE};
use crate::error::AppError;

pub const DEFAULT_BASE_URL: &str = "https://huggingface.co";

/// 받을 파일 하나 (크기와 SHA-256 고정)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// 텍스트 전용 파일을 원본의 어느 구간에서 만드는지
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpec {
    /// 옮길 텐서 이름의 접두어 (embeddinggemma-2: "language_model.", Qwen3.5: "model.language_model.")
    pub prefix: String,
    /// 원본 헤더 길이 (앞 8바이트 포함)
    pub header_len: u64,
    pub header_sha256: String,
    /// 원본 안의 `language_model.*` 데이터 구간 [start, end) (파일 기준 위치)
    pub range_start: u64,
    pub range_end: u64,
    pub range_sha256: String,
}

impl TextSpec {
    pub fn range_len(&self) -> u64 {
        self.range_end - self.range_start
    }
}

/// 고정한 모델 리비전과 파일 목록
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub repo: String,
    pub revision: String,
    pub tokenizer: FileSpec,
    pub full: FileSpec,
    /// 텍스트 전용 설치를 지원하면 그 구간 (embeddinggemma-2만)
    pub text: Option<TextSpec>,
}

/// 텍스트 전용 파일에 옮기는 텐서
pub const TEXT_PREFIX: &str = "language_model.";

impl Manifest {
    /// google/embeddinggemma-2 @ f9972ac (2026-11-14 확인: HF API의 LFS sha256과 내려받은 파일의 sha256이 같다)
    pub fn embeddinggemma2() -> Self {
        Self {
            repo: "google/embeddinggemma-2".into(),
            revision: "f9972ac22e696a105c482202646979e33e4b5a1e".into(),
            tokenizer: FileSpec {
                name: TOKENIZER_FILE.into(),
                size: 32_170_510,
                sha256: "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4".into(),
            },
            full: FileSpec {
                name: WEIGHTS_FILE.into(),
                size: 1_488_915_288,
                sha256: "197a32965d4b1105faf060417baa899e193fb73cd401f42ec9295234d5553d79".into(),
            },
            text: Some(TextSpec {
                prefix: TEXT_PREFIX.into(),
                header_len: 171_304,
                header_sha256: "75e406407f6c97cc0576e839499c190c5182b27f20d0e774636ef7bef0bf9ac8".into(),
                range_start: 612_180_776,
                range_end: 1_154_186_072,
                range_sha256: "acc894b834ecd41e6c0c02c03155ff6e7290bb3b7b518cd3526c88f41e646115".into(),
            }),
        }
    }

    /// convaiinnovations/laya-multilingual @ 1720e3e (욕설 필터, 2026-11-14 확인: HF LFS 해시와 같다).
    /// 실행에 필요한 두 파일만 받는다 (인코더 설정은 laya.rs에 상수로 있다)
    pub fn laya_multilingual() -> Self {
        Self {
            repo: "convaiinnovations/laya-multilingual".into(),
            revision: "1720e3e3357cfe1e281542e223f8273b0890ca34".into(),
            tokenizer: FileSpec {
                name: crate::laya::TOKENIZER_FILE.into(),
                size: 34_363_188,
                sha256: "609d8f4c067cd3950f88594c5a802616cea245823836ef5848ee4fc40aab5b6f".into(),
            },
            full: FileSpec {
                name: crate::laya::WEIGHTS_FILE.into(),
                size: 643_835_514,
                sha256: "9d628fd971b700382ac6f65920a86f149777b2e748e0c955fb3b19695aa8f204".into(),
            },
            text: None,
        }
    }

    /// Qwen/Qwen3.5-0.8B @ 2fc0636 (items 초안 스케줄러). 텍스트 가중치 320개가 원본 앞쪽 한 덩어리(1,504.8MB)라
    /// 비전 타워(201MB)·MTP(41MB) 없이 받을 수 있다 (2026-11-14 로컬 파일에서 계산, HF LFS 해시와 같은 파일)
    pub fn qwen35_08b() -> Self {
        Self {
            repo: "Qwen/Qwen3.5-0.8B".into(),
            revision: "2fc06364715b967f1860aea9cf38778875588b17".into(),
            tokenizer: FileSpec {
                name: crate::qwen35::TOKENIZER_FILE.into(),
                size: 12_807_982,
                sha256: "5f9e4d4901a92b997e463c1f46055088b6cca5ca61a6522d1b9f64c4bb81cb42".into(),
            },
            full: FileSpec {
                name: crate::qwen35::WEIGHTS_FILE.into(),
                size: 1_746_942_600,
                sha256: "04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696".into(),
            },
            text: Some(TextSpec {
                prefix: "model.language_model.".into(),
                header_len: 59_848,
                header_sha256: "b30e0e919db8a7d195d077a74673d1075e43bc55aece77656e075d95b5dcdf0d".into(),
                range_start: 59_848,
                range_end: 1_504_851_080,
                range_sha256: "85a0a2c3e02fbc2714d85143b99cbc4029ebaafe52decb7131cae68a0a7e6703".into(),
            }),
        }
    }

    /// 이 폴더에 설치된 것
    pub fn installed(&self, dir: &Path) -> Installed {
        if !dir.join(&self.tokenizer.name).is_file() {
            return Installed::None;
        }
        if dir.join(&self.full.name).is_file() {
            Installed::Full
        } else if self.text.is_some() && dir.join(TEXT_WEIGHTS_FILE).is_file() {
            Installed::Text
        } else {
            Installed::None
        }
    }

    fn url(&self, base: &str, name: &str) -> String {
        format!("{}/{}/resolve/{}/{}", base.trim_end_matches('/'), self.repo, self.revision, name)
    }

    /// 이 설치에 받을 바이트 수
    pub fn download_bytes(&self, variant: Variant) -> u64 {
        self.tokenizer.size
            + match (variant, &self.text) {
                (Variant::Text | Variant::Draft, Some(t)) => t.header_len + t.range_len(),
                (Variant::Text | Variant::Draft, None) => 0,
                (Variant::Full | Variant::Filter, _) => self.full.size,
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    Text,
    Full,
    /// 욕설 필터 모델 (laya-multilingual). 셸이 필터용 내려받기 관리자로 보낸다
    Filter,
    /// items 초안 모델 (Qwen3.5-0.8B 텍스트 부분). 셸이 초안용 내려받기 관리자로 보낸다
    Draft,
}

/// 모델 폴더에 설치된 것
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Installed {
    None,
    Text,
    Full,
}

/// embeddinggemma-2 폴더에 설치된 것
pub fn installed(dir: &Path) -> Installed {
    Manifest::embeddinggemma2().installed(dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchState {
    Idle,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// 내려받기 진행 상황 (화면이 1초마다 읽는다)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FetchProgress {
    pub state: FetchState,
    pub variant: Option<Variant>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// 지금 받는 파일
    pub file: Option<String>,
    pub error: Option<String>,
}

impl FetchProgress {
    fn idle() -> Self {
        Self { state: FetchState::Idle, variant: None, done_bytes: 0, total_bytes: 0, file: None, error: None }
    }
}

/// `cmd_model_status` 결과
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelStatus {
    pub installed: Installed,
    pub model_dir: String,
    pub download: FetchProgress,
    /// 설치별 내려받을 크기 (바이트)
    pub text_bytes: u64,
    pub full_bytes: u64,
    pub revision: String,
    /// 코어가 올라와 있으면 타워 상주 상태 (셸이 채운다)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residency: Option<crate::residency::ResidencyReport>,
    /// 욕설 필터 모델(laya-multilingual) 상태 (셸이 필터 내려받기를 가질 때)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<FilterInfo>,
    /// items 초안 모델(Qwen3.5-0.8B) 상태. active는 "" (안 올림) 또는 "qwen" (메모리에 있음)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<FilterInfo>,
}

/// 욕설 필터 모델 상태
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FilterInfo {
    pub installed: bool,
    /// 지금 쓰는 필터 ("dictionary" | "laya"). 셸이 채운다
    pub active: String,
    pub download: FetchProgress,
    /// 내려받을 크기 (바이트)
    pub bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadArgs {
    pub variant: Variant,
}

/// 내려받기 관리자. 셸(Tauri, 표준 입출력)이 하나씩 가진다. 코어(모델)가 없어도 동작한다.
pub struct ModelFetcher {
    dir: PathBuf,
    base_url: String,
    manifest: Manifest,
    progress: Arc<Mutex<FetchProgress>>,
    cancel: Arc<AtomicBool>,
    /// 설치를 끝낼 때마다 1씩 는다. 셸은 이 값이 바뀌면 실패했던 코어 만들기를 다시 시도한다
    generation: Arc<AtomicU64>,
    client: reqwest::Client,
}

impl ModelFetcher {
    pub fn new(dir: impl Into<PathBuf>, base_url: impl Into<String>) -> Result<Self, AppError> {
        Self::with_manifest(dir, base_url, Manifest::embeddinggemma2())
    }

    pub fn with_manifest(dir: impl Into<PathBuf>, base_url: impl Into<String>, manifest: Manifest) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("premium-discount-app/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AppError::Offline(format!("HTTP 클라이언트를 만들지 못했습니다: {e}")))?;
        Ok(Self {
            dir: dir.into(),
            base_url: base_url.into(),
            manifest,
            progress: Arc::new(Mutex::new(FetchProgress::idle())),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            client,
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn progress(&self) -> FetchProgress {
        self.progress.lock().expect("progress").clone()
    }

    pub fn status(&self) -> ModelStatus {
        ModelStatus {
            installed: self.manifest.installed(&self.dir),
            model_dir: self.dir.display().to_string(),
            download: self.progress(),
            text_bytes: if self.manifest.text.is_some() { self.manifest.download_bytes(Variant::Text) } else { 0 },
            full_bytes: self.manifest.download_bytes(Variant::Full),
            revision: self.manifest.revision.clone(),
            residency: None,
            filter: None,
            draft: None,
        }
    }

    /// 보조 모델 관리자로서의 상태 (`FilterInfo`). active는 셸이 채운다
    pub fn filter_info(&self) -> FilterInfo {
        let installed = self.manifest.installed(&self.dir);
        FilterInfo {
            installed: installed == Installed::Full || (self.manifest.text.is_some() && installed == Installed::Text),
            active: "dictionary".into(),
            download: self.progress(),
            bytes: self.manifest.download_bytes(if self.manifest.text.is_some() { Variant::Text } else { Variant::Full }),
        }
    }

    /// 내려받기를 시작한다 (이미 받는 중이면 그 진행 상황을 돌려준다). 실제 작업은 비동기 작업으로 돈다.
    pub fn start(&self, variant: Variant) -> FetchProgress {
        {
            let mut p = self.progress.lock().expect("progress");
            if p.state == FetchState::Running {
                return p.clone();
            }
            *p = FetchProgress {
                state: FetchState::Running,
                variant: Some(variant),
                done_bytes: 0,
                total_bytes: self.manifest.download_bytes(variant),
                file: None,
                error: None,
            };
        }
        self.cancel.store(false, Ordering::SeqCst);
        let job = Job {
            dir: self.dir.clone(),
            base_url: self.base_url.clone(),
            manifest: self.manifest.clone(),
            progress: self.progress.clone(),
            cancel: self.cancel.clone(),
            client: self.client.clone(),
        };
        let generation = self.generation.clone();
        let progress = self.progress.clone();
        tokio::spawn(async move {
            let result = job.run(variant).await;
            let mut p = progress.lock().expect("progress");
            match result {
                Ok(()) => {
                    p.state = FetchState::Done;
                    p.file = None;
                    generation.fetch_add(1, Ordering::SeqCst);
                }
                Err(Stop::Cancelled) => p.state = FetchState::Cancelled,
                Err(Stop::Failed(e)) => {
                    p.state = FetchState::Failed;
                    p.error = Some(e);
                }
            }
        });
        self.progress()
    }

    /// 받는 중이면 멈춘다. 받은 부분(.part)은 남겨 다음에 이어받는다.
    pub fn cancel(&self) -> FetchProgress {
        self.cancel.store(true, Ordering::SeqCst);
        self.progress()
    }

    /// 다 끝날 때까지 기다린다 (시험·명령줄용)
    pub async fn wait(&self) -> FetchProgress {
        loop {
            let p = self.progress();
            if p.state != FetchState::Running {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

enum Stop {
    Cancelled,
    Failed(String),
}

impl From<std::io::Error> for Stop {
    fn from(e: std::io::Error) -> Self {
        // 28 = ENOSPC (Linux·Android·macOS 공통)
        if e.raw_os_error() == Some(28) {
            Stop::Failed("저장 공간이 부족합니다. 공간을 비운 뒤 다시 시도하세요 (받은 부분은 남아 이어받습니다)".into())
        } else {
            Stop::Failed(format!("파일을 쓰지 못했습니다: {e}"))
        }
    }
}

/// 한 번의 내려받기 작업
struct Job {
    dir: PathBuf,
    base_url: String,
    manifest: Manifest,
    progress: Arc<Mutex<FetchProgress>>,
    cancel: Arc<AtomicBool>,
    client: reqwest::Client,
}

/// 끊긴 연결을 이어받는 횟수 (요청마다)
const RETRIES: u32 = 4;

impl Job {
    async fn run(&self, variant: Variant) -> Result<(), Stop> {
        std::fs::create_dir_all(&self.dir)?;
        let tokenizer = self.manifest.tokenizer.clone();
        self.fetch_whole(&tokenizer).await?;
        match variant {
            Variant::Full | Variant::Filter => {
                let full = self.manifest.full.clone();
                self.fetch_whole(&full).await?;
                // 전체 파일이 텍스트 전용 파일을 포함하므로 지운다 (공간 542MB)
                let _ = std::fs::remove_file(self.dir.join(TEXT_WEIGHTS_FILE));
                let _ = std::fs::remove_file(part_path(&self.dir.join(TEXT_WEIGHTS_FILE)));
            }
            Variant::Text | Variant::Draft => self.fetch_text().await?,
        }
        Ok(())
    }

    fn set_file(&self, name: &str) {
        self.progress.lock().expect("progress").file = Some(name.to_string());
    }

    fn add_done(&self, n: u64) {
        self.progress.lock().expect("progress").done_bytes += n;
    }

    fn check_cancel(&self) -> Result<(), Stop> {
        if self.cancel.load(Ordering::SeqCst) {
            Err(Stop::Cancelled)
        } else {
            Ok(())
        }
    }

    /// 파일 하나를 통째로 받는다. 이미 맞는 파일이 있으면 건너뛴다.
    async fn fetch_whole(&self, spec: &FileSpec) -> Result<(), Stop> {
        self.set_file(&spec.name);
        let target = self.dir.join(&spec.name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if target.is_file() && std::fs::metadata(&target)?.len() == spec.size && sha256_file(&target, 0)? == spec.sha256 {
            self.add_done(spec.size);
            return Ok(());
        }
        let part = part_path(&target);
        let mut have = part.metadata().map(|m| m.len()).unwrap_or(0);
        if have > spec.size {
            std::fs::remove_file(&part)?;
            have = 0;
        }
        let mut hasher = Sha256::new();
        if have > 0 {
            hash_prefix(&part, have, &mut hasher)?;
        }
        self.add_done(have);
        let url = self.manifest.url(&self.base_url, &spec.name);
        self.stream_into(&url, &part, have, spec.size, 0, &mut hasher).await?;
        let got = hex(hasher.finalize().as_slice());
        if got != spec.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(Stop::Failed(format!("{}: 받은 내용의 해시가 다릅니다 (다시 받으세요)", spec.name)));
        }
        std::fs::rename(&part, &target)?;
        Ok(())
    }

    /// 텍스트 전용 파일: 원본 헤더를 받아 `language_model.*`만 남긴 새 헤더를 쓰고, 그 텐서 구간을 이어 붙인다.
    async fn fetch_text(&self) -> Result<(), Stop> {
        let Some(spec) = self.manifest.text.clone() else {
            return Err(Stop::Failed("이 모델은 텍스트 전용 설치가 없습니다".into()));
        };
        let target = self.dir.join(TEXT_WEIGHTS_FILE);
        self.set_file(TEXT_WEIGHTS_FILE);
        let url = self.manifest.url(&self.base_url, &self.manifest.full.name);

        // 1. 원본 헤더 (작다: 메모리로)
        let header = self.get_range(&url, 0, spec.header_len).await?;
        if hex(&Sha256::digest(&header)) != spec.header_sha256 {
            return Err(Stop::Failed("모델 헤더의 해시가 다릅니다".into()));
        }
        let prefix = text_header(&header, &spec).map_err(Stop::Failed)?;
        self.add_done(spec.header_len);

        // 2. 이미 끝난 파일이면 해시만 확인
        if target.is_file() {
            if std::fs::metadata(&target)?.len() == prefix.len() as u64 + spec.range_len()
                && sha256_file(&target, prefix.len() as u64)? == spec.range_sha256
            {
                self.add_done(spec.range_len());
                return Ok(());
            }
            std::fs::remove_file(&target)?;
        }

        // 3. .part = 새 헤더 + 받은 데이터. 헤더가 다르면(다른 판) 처음부터
        let part = part_path(&target);
        let prefix_ok = part.is_file() && {
            let mut head = vec![0u8; prefix.len()];
            std::fs::File::open(&part).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == prefix
        };
        if !prefix_ok {
            let mut f = std::fs::File::create(&part)?;
            f.write_all(&prefix)?;
            f.sync_all()?;
        }
        let total = prefix.len() as u64 + spec.range_len();
        let mut have = part.metadata()?.len();
        if have > total {
            // 잘못 붙은 꼬리: 헤더만 남기고 다시
            let f = std::fs::OpenOptions::new().write(true).open(&part)?;
            f.set_len(prefix.len() as u64)?;
            have = prefix.len() as u64;
        }
        let mut hasher = Sha256::new();
        let data_have = have - prefix.len() as u64;
        if data_have > 0 {
            hash_range(&part, prefix.len() as u64, have, &mut hasher)?;
        }
        self.add_done(data_have);
        self.stream_into(&url, &part, have, total, spec.range_start - prefix.len() as u64, &mut hasher).await?;
        if hex(hasher.finalize().as_slice()) != spec.range_sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(Stop::Failed("텍스트 모델 구간의 해시가 다릅니다 (다시 받으세요)".into()));
        }
        std::fs::rename(&part, &target)?;
        Ok(())
    }

    /// 작은 구간 하나를 메모리로 받는다
    async fn get_range(&self, url: &str, start: u64, len: u64) -> Result<Vec<u8>, Stop> {
        let mut last = String::new();
        for attempt in 0..=RETRIES {
            self.check_cancel()?;
            if attempt > 0 {
                tokio::time::sleep(backoff(attempt)).await;
            }
            let resp = self
                .client
                .get(url)
                .header(reqwest::header::RANGE, format!("bytes={}-{}", start, start + len - 1))
                .send()
                .await;
            match resp {
                Ok(r) if r.status() == reqwest::StatusCode::PARTIAL_CONTENT => match r.bytes().await {
                    Ok(b) if b.len() as u64 == len => return Ok(b.to_vec()),
                    Ok(b) => last = format!("길이가 다릅니다 ({} / {len})", b.len()),
                    Err(e) => last = e.to_string(),
                },
                Ok(r) if r.status().is_success() => {
                    return Err(Stop::Failed("서버가 부분 요청(Range)을 지원하지 않아 텍스트 전용으로 받을 수 없습니다".into()))
                }
                Ok(r) => return Err(Stop::Failed(format!("모델 서버 응답 {}", r.status()))),
                Err(e) => last = e.to_string(),
            }
        }
        Err(Stop::Failed(format!("모델 서버에 연결하지 못했습니다: {last}")))
    }

    /// `part`의 `have`부터 `total`까지 받아 붙인다. 원격 위치는 (로컬 위치 + `remote_offset`).
    /// 끊기면 이어받는다. 받은 바이트는 `hasher`에 넣는다.
    async fn stream_into(&self, url: &str, part: &Path, mut have: u64, total: u64, remote_offset: u64, hasher: &mut Sha256) -> Result<(), Stop> {
        let mut failures = 0;
        while have < total {
            self.check_cancel()?;
            let from = have + remote_offset;
            let to = total + remote_offset - 1;
            let resp = self.client.get(url).header(reqwest::header::RANGE, format!("bytes={from}-{to}")).send().await;
            let mut resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    failures += 1;
                    if failures > RETRIES {
                        return Err(Stop::Failed(format!("모델 서버에 연결하지 못했습니다: {e}")));
                    }
                    tokio::time::sleep(backoff(failures)).await;
                    continue;
                }
            };
            let status = resp.status();
            if status == reqwest::StatusCode::OK && from == 0 {
                // 처음부터 받는 요청에 서버가 전체로 답했다: 그대로 쓴다
            } else if status != reqwest::StatusCode::PARTIAL_CONTENT {
                if status.is_success() {
                    return Err(Stop::Failed("서버가 부분 요청(Range)을 지원하지 않아 이어받을 수 없습니다".into()));
                }
                return Err(Stop::Failed(format!("모델 서버 응답 {status}")));
            }
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(part)?;
            loop {
                self.check_cancel()?;
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        let room = (total - have) as usize;
                        let chunk = &chunk[..chunk.len().min(room)];
                        file.write_all(chunk)?;
                        hasher.update(chunk);
                        have += chunk.len() as u64;
                        self.add_done(chunk.len() as u64);
                        failures = 0;
                        if have >= total {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            file.sync_data()?;
            if have < total {
                // 연결이 중간에 끊겼다: 이어받는다
                failures += 1;
                if failures > RETRIES {
                    return Err(Stop::Failed("연결이 계속 끊깁니다. 잠시 뒤 다시 시도하세요 (받은 부분은 남아 이어받습니다)".into()));
                }
                tokio::time::sleep(backoff(failures)).await;
            }
        }
        Ok(())
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * 2u64.pow(attempt.min(5)))
}

fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_prefix(path: &Path, len: u64, hasher: &mut Sha256) -> std::io::Result<()> {
    hash_range(path, 0, len, hasher)
}

fn hash_range(path: &Path, start: u64, end: u64, hasher: &mut Sha256) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut left = end - start;
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

/// 파일의 `skip` 바이트 뒤부터 끝까지의 SHA-256
fn sha256_file(path: &Path, skip: u64) -> std::io::Result<String> {
    let len = std::fs::metadata(path)?.len();
    let mut hasher = Sha256::new();
    hash_range(path, skip.min(len), len, &mut hasher)?;
    Ok(hex(hasher.finalize().as_slice()))
}

/// 원본 헤더(앞 8바이트 + JSON)에서 `language_model.*`만 남긴 새 헤더(앞 8바이트 + JSON, 8의 배수로 맞춤)를 만든다.
/// 남긴 텐서들이 `spec`의 구간을 빈틈없이 채우는지 확인하고, 위치를 구간 시작 기준으로 옮긴다.
pub fn text_header(original: &[u8], spec: &TextSpec) -> Result<Vec<u8>, String> {
    if original.len() < 8 {
        return Err("모델 헤더가 짧습니다".into());
    }
    let n = u64::from_le_bytes(original[..8].try_into().expect("8 bytes")) as usize;
    let json = original.get(8..8 + n).ok_or("모델 헤더가 잘렸습니다")?;
    let map: BTreeMap<String, Value> = serde_json::from_slice(json).map_err(|e| format!("모델 헤더를 읽지 못했습니다: {e}"))?;
    let data_start = (8 + n) as u64;
    if spec.range_start < data_start {
        return Err("텍스트 구간이 헤더와 겹칩니다".into());
    }
    let base = spec.range_start - data_start;
    let end = spec.range_end - data_start;

    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    let mut spans: Vec<(u64, u64)> = Vec::new();
    for (name, entry) in map {
        if name == "__metadata__" {
            out.insert(name, entry);
            continue;
        }
        if !name.starts_with(&spec.prefix) {
            continue;
        }
        let offsets = entry
            .get("data_offsets")
            .and_then(Value::as_array)
            .and_then(|a| Some((a.first()?.as_u64()?, a.get(1)?.as_u64()?)))
            .ok_or_else(|| format!("{name}: data_offsets가 없습니다"))?;
        if offsets.0 < base || offsets.1 > end {
            return Err(format!("{name}이(가) 텍스트 구간 밖에 있습니다"));
        }
        spans.push(offsets);
        let mut entry = entry;
        entry["data_offsets"] = serde_json::json!([offsets.0 - base, offsets.1 - base]);
        out.insert(name, entry);
    }
    spans.sort_unstable();
    let mut cursor = base;
    for (a, b) in &spans {
        if *a != cursor {
            return Err("텍스트 텐서들이 구간을 빈틈없이 채우지 않습니다".into());
        }
        cursor = *b;
    }
    if cursor != end || spans.is_empty() {
        return Err("텍스트 텐서들이 구간을 빈틈없이 채우지 않습니다".into());
    }

    let mut json = serde_json::to_vec(&out).map_err(|e| e.to_string())?;
    // 데이터가 8바이트 경계에서 시작하도록 공백으로 채운다 (safetensors 관례)
    while (8 + json.len()) % 8 != 0 {
        json.push(b' ');
    }
    let mut header = (json.len() as u64).to_le_bytes().to_vec();
    header.extend_from_slice(&json);
    Ok(header)
}

/// 로컬의 원본 파일에서 텍스트 구간 정보를 만든다 (매니페스트 값을 만들고 검증하는 데 쓴다)
pub fn describe_text_spec(full_file: &Path) -> std::io::Result<TextSpec> {
    describe_span(full_file, TEXT_PREFIX)
}

/// `prefix`로 시작하는 텐서들의 구간 정보
pub fn describe_span(full_file: &Path, prefix: &str) -> std::io::Result<TextSpec> {
    let mut f = std::fs::File::open(full_file)?;
    let mut len = [0u8; 8];
    f.read_exact(&mut len)?;
    let n = u64::from_le_bytes(len);
    let mut json = vec![0u8; n as usize];
    f.read_exact(&mut json)?;
    let map: BTreeMap<String, Value> =
        serde_json::from_slice(&json).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let (mut lo, mut hi) = (u64::MAX, 0u64);
    for (name, entry) in &map {
        if !name.starts_with(prefix) {
            continue;
        }
        let a = entry["data_offsets"][0].as_u64().unwrap_or(u64::MAX);
        let b = entry["data_offsets"][1].as_u64().unwrap_or(0);
        lo = lo.min(a);
        hi = hi.max(b);
    }
    let data_start = 8 + n;
    let mut header = len.to_vec();
    header.extend_from_slice(&json);
    let mut hasher = Sha256::new();
    hash_range(full_file, data_start + lo, data_start + hi, &mut hasher)?;
    Ok(TextSpec {
        prefix: prefix.to_string(),
        header_len: data_start,
        header_sha256: hex(&Sha256::digest(&header)),
        range_start: data_start + lo,
        range_end: data_start + hi,
        range_sha256: hex(hasher.finalize().as_slice()),
    })
}

#[cfg(test)]
pub(crate) mod test_server {
    //! 시험용 HTTP 서버: GET과 Range(bytes=a-b)만 안다. 첫 요청을 중간에 끊거나, Range를 무시하게 할 수 있다.
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[derive(Default)]
    pub struct Behavior {
        /// 처음 n번의 응답은 본문을 이만큼만 보내고 끊는다
        pub cut_first: usize,
        pub cut_after: usize,
        pub ignore_range: bool,
        /// 요청 한 번마다 쉰다 (취소 시험)
        pub slow_ms: u64,
    }

    pub struct Server {
        pub base: String,
        pub requests: Arc<AtomicUsize>,
    }

    pub async fn start(files: HashMap<String, Vec<u8>>, behavior: Behavior) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(files);
        let behavior = Arc::new(behavior);
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let files = files.clone();
                let behavior = behavior.clone();
                let n = count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut got = 0;
                    loop {
                        if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        let k = match sock.read(&mut buf[got..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(k) => k,
                        };
                        got += k;
                    }
                    let req = String::from_utf8_lossy(&buf[..got]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let name = path.rsplit('/').next().unwrap_or("").to_string();
                    let Some(body) = files.get(&name) else {
                        let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        return;
                    };
                    let range = req
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(|s| s.to_string()))
                        .and_then(|r| {
                            let (a, b) = r.trim().split_once('-')?;
                            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                        });
                    if behavior.slow_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(behavior.slow_ms)).await;
                    }
                    let (status, slice) = match range {
                        Some((a, b)) if !behavior.ignore_range => ("206 Partial Content", &body[a..=b.min(body.len() - 1)]),
                        _ => ("200 OK", &body[..]),
                    };
                    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len());
                    let _ = sock.write_all(head.as_bytes()).await;
                    let send = if n < behavior.cut_first { &slice[..behavior.cut_after.min(slice.len())] } else { slice };
                    for chunk in send.chunks(4096) {
                        if sock.write_all(chunk).await.is_err() {
                            return;
                        }
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        Server { base, requests }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use candle_core::{DType, Device, Tensor};

    use crate::local::test_support::temp_dir;

    /// 원본 모양을 흉내 낸 작은 safetensors: 오디오·비전 타워 사이에 language_model.* 가 한 덩어리로 있다
    fn synthetic_model() -> Vec<u8> {
        let t = |n: usize, s: f32| Tensor::from_vec((0..n).map(|i| i as f32 * s).collect::<Vec<f32>>(), (n,), &Device::Cpu).unwrap().to_dtype(DType::BF16).unwrap();
        let tensors = HashMap::from([
            ("audio_tower.layers.0.w".to_string(), t(4000, 0.1)),
            ("embed_audio.embedding_projection.weight".to_string(), t(64, 0.2)),
            ("embed_vision.embedding_projection.weight".to_string(), t(64, 0.3)),
            ("language_model.embed_tokens.weight".to_string(), t(3000, 0.4)),
            ("language_model.layers.0.mlp.up_proj.weight".to_string(), t(2048, 0.5)),
            ("language_model.norm.weight".to_string(), t(16, 0.6)),
            ("vision_tower.layers.0.w".to_string(), t(2500, 0.7)),
        ]);
        let dir = temp_dir("synthetic-model");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.safetensors");
        candle_core::safetensors::save(&tensors, &path).unwrap();
        std::fs::read(&path).unwrap()
    }

    fn manifest_for(model: &[u8], tokenizer: &[u8]) -> Manifest {
        let dir = temp_dir("synthetic-spec");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.safetensors");
        std::fs::write(&path, model).unwrap();
        Manifest {
            repo: "test/model".into(),
            revision: "abc123".into(),
            tokenizer: FileSpec { name: TOKENIZER_FILE.into(), size: tokenizer.len() as u64, sha256: hex(&Sha256::digest(tokenizer)) },
            full: FileSpec { name: WEIGHTS_FILE.into(), size: model.len() as u64, sha256: hex(&Sha256::digest(model)) },
            text: Some(describe_text_spec(&path).unwrap()),
        }
    }

    fn files(model: &[u8], tokenizer: &[u8]) -> HashMap<String, Vec<u8>> {
        HashMap::from([(WEIGHTS_FILE.to_string(), model.to_vec()), (TOKENIZER_FILE.to_string(), tokenizer.to_vec())])
    }

    #[tokio::test]
    async fn text_only_download_fetches_only_the_text_span_and_keeps_every_text_tensor_byte_for_byte() {
        let model = synthetic_model();
        let tokenizer = b"{\"tokenizer\": true}".to_vec();
        let manifest = manifest_for(&model, &tokenizer);
        let server = test_server::start(files(&model, &tokenizer), Default::default()).await;
        let dir = temp_dir("fetch-text");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest.clone()).unwrap();
        assert_eq!(fetcher.status().installed, Installed::None);

        fetcher.start(Variant::Text);
        let done = fetcher.wait().await;
        assert_eq!(done.state, FetchState::Done, "{done:?}");
        assert_eq!(done.done_bytes, done.total_bytes);
        assert_eq!(done.total_bytes, manifest.download_bytes(Variant::Text));
        assert!(manifest.download_bytes(Variant::Text) < manifest.download_bytes(Variant::Full));
        assert_eq!(fetcher.status().installed, Installed::Text);
        assert_eq!(fetcher.generation(), 1);
        assert!(!dir.join(WEIGHTS_FILE).exists() && !dir.join(format!("{TEXT_WEIGHTS_FILE}.part")).exists());

        // 조립한 파일은 safetensors로 열리고, language_model.* 만 원본과 같은 값으로 들어 있다
        let original = candle_core::safetensors::load_buffer(&model, &Device::Cpu).unwrap();
        let text = candle_core::safetensors::load(dir.join(TEXT_WEIGHTS_FILE), &Device::Cpu).unwrap();
        let mut names: Vec<_> = text.keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["language_model.embed_tokens.weight", "language_model.layers.0.mlp.up_proj.weight", "language_model.norm.weight"]);
        for name in names {
            let a: Vec<f32> = original[&name].to_dtype(DType::F32).unwrap().to_vec1().unwrap();
            let b: Vec<f32> = text[&name].to_dtype(DType::F32).unwrap().to_vec1().unwrap();
            assert_eq!(a, b, "{name}");
            assert_eq!(text[&name].dtype(), DType::BF16);
        }
    }

    #[tokio::test]
    async fn full_download_installs_the_original_file_and_removes_the_text_only_file() {
        let model = synthetic_model();
        let tokenizer = b"tok".to_vec();
        let manifest = manifest_for(&model, &tokenizer);
        let server = test_server::start(files(&model, &tokenizer), Default::default()).await;
        let dir = temp_dir("fetch-full");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest).unwrap();
        fetcher.start(Variant::Text);
        assert_eq!(fetcher.wait().await.state, FetchState::Done);
        fetcher.start(Variant::Full);
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Done, "{p:?}");
        assert_eq!(std::fs::read(dir.join(WEIGHTS_FILE)).unwrap(), model);
        assert!(!dir.join(TEXT_WEIGHTS_FILE).exists(), "전체가 텍스트 전용을 대신한다");
        assert_eq!(fetcher.status().installed, Installed::Full);
        assert_eq!(fetcher.generation(), 2);
    }

    #[tokio::test]
    async fn broken_connections_are_resumed_from_where_they_stopped() {
        let model = synthetic_model();
        let tokenizer = vec![7u8; 30_000];
        let manifest = manifest_for(&model, &tokenizer);
        // 처음 세 응답은 본문 5000바이트에서 끊긴다 (토크나이저, 원본 헤더 요청, 텍스트 구간)
        let server = test_server::start(files(&model, &tokenizer), test_server::Behavior { cut_first: 3, cut_after: 5000, ..Default::default() }).await;
        let dir = temp_dir("fetch-resume");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest.clone()).unwrap();
        fetcher.start(Variant::Text);
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Done, "{p:?}");
        assert_eq!(p.done_bytes, manifest.download_bytes(Variant::Text), "이어받아도 진행률이 두 번 세지 않는다");
        assert_eq!(std::fs::read(dir.join(TOKENIZER_FILE)).unwrap(), tokenizer);
        assert!(server.requests.load(std::sync::atomic::Ordering::SeqCst) > 3, "끊긴 뒤 다시 요청했다");
    }

    #[tokio::test]
    async fn a_hash_mismatch_installs_nothing_and_a_server_without_range_cannot_do_text_only() {
        let model = synthetic_model();
        let tokenizer = b"tok".to_vec();
        let mut manifest = manifest_for(&model, &tokenizer);
        manifest.text.as_mut().unwrap().range_sha256 = "0".repeat(64);
        let server = test_server::start(files(&model, &tokenizer), Default::default()).await;
        let dir = temp_dir("fetch-badhash");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest.clone()).unwrap();
        fetcher.start(Variant::Text);
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Failed);
        assert!(p.error.unwrap().contains("해시"));
        assert!(!dir.join(TEXT_WEIGHTS_FILE).exists() && !dir.join(format!("{TEXT_WEIGHTS_FILE}.part")).exists());
        assert_eq!(fetcher.status().installed, Installed::None);
        assert_eq!(fetcher.generation(), 0);

        let manifest = manifest_for(&model, &tokenizer);
        let server = test_server::start(files(&model, &tokenizer), test_server::Behavior { ignore_range: true, ..Default::default() }).await;
        let dir = temp_dir("fetch-norange");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest).unwrap();
        fetcher.start(Variant::Text);
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Failed);
        assert!(p.error.unwrap().contains("Range"));
    }

    #[tokio::test]
    async fn cancel_stops_and_keeps_the_partial_file_for_the_next_attempt() {
        let model = synthetic_model();
        let tokenizer = vec![1u8; 20_000];
        let manifest = manifest_for(&model, &tokenizer);
        let server = test_server::start(files(&model, &tokenizer), test_server::Behavior { slow_ms: 300, ..Default::default() }).await;
        let dir = temp_dir("fetch-cancel");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest).unwrap();
        fetcher.start(Variant::Text);
        assert_eq!(fetcher.start(Variant::Full).variant, Some(Variant::Text), "받는 중에는 새로 시작하지 않는다");
        tokio::time::sleep(Duration::from_millis(100)).await;
        fetcher.cancel();
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Cancelled);
        assert_eq!(fetcher.status().installed, Installed::None);

        fetcher.start(Variant::Text);
        assert_eq!(fetcher.wait().await.state, FetchState::Done, "다시 시작하면 끝까지 받는다");
        assert_eq!(fetcher.status().installed, Installed::Text);
    }

    #[tokio::test]
    async fn a_manifest_without_a_text_span_installs_whole_files_including_ones_in_subfolders() {
        // laya처럼: 토크나이저가 하위 폴더에 있고 텍스트 전용 설치가 없다
        let model = synthetic_model();
        let tokenizer = b"{\"laya\": true}".to_vec();
        let mut manifest = manifest_for(&model, &tokenizer);
        manifest.tokenizer.name = "tokenizer/tokenizer.json".into();
        manifest.text = None;
        let server = test_server::start(
            HashMap::from([(WEIGHTS_FILE.to_string(), model.clone()), ("tokenizer.json".to_string(), tokenizer.clone())]),
            Default::default(),
        )
        .await;
        let dir = temp_dir("fetch-laya-like");
        let fetcher = ModelFetcher::with_manifest(&dir, &server.base, manifest).unwrap();
        assert_eq!(fetcher.status().text_bytes, 0);
        fetcher.start(Variant::Text);
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Failed, "텍스트 전용이 없는 모델");
        fetcher.start(Variant::Filter);
        let p = fetcher.wait().await;
        assert_eq!(p.state, FetchState::Done, "{p:?}");
        assert_eq!(std::fs::read(dir.join("tokenizer/tokenizer.json")).unwrap(), tokenizer);
        assert_eq!(fetcher.status().installed, Installed::Full);
    }

    #[test]
    fn text_header_rejects_tensors_outside_or_not_filling_the_span() {
        let model = synthetic_model();
        let dir = temp_dir("header-check");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.safetensors");
        std::fs::write(&path, &model).unwrap();
        let spec = describe_text_spec(&path).unwrap();
        let header = &model[..spec.header_len as usize];
        let ok = text_header(header, &spec).unwrap();
        assert_eq!(ok.len() % 8, 0);

        let mut narrow = spec.clone();
        narrow.range_end -= 2;
        assert!(text_header(header, &narrow).unwrap_err().contains("구간 밖"));
        let mut wide = spec.clone();
        wide.range_end += 2;
        assert!(text_header(header, &wide).unwrap_err().contains("빈틈없이"));
    }

    /// Qwen3.5 매니페스트의 텍스트 구간이 실제 파일과 같은지 (QWEN_DIR이 있을 때만)
    #[test]
    #[ignore = "모델 파일이 필요하다. QWEN_DIR을 지정하고 --ignored로 실행한다"]
    fn pinned_qwen_manifest_matches_the_real_model_file() {
        let dir = PathBuf::from(std::env::var("QWEN_DIR").expect("QWEN_DIR"));
        let m = Manifest::qwen35_08b();
        let file = dir.join(&m.full.name);
        assert_eq!(Some(describe_span(&file, "model.language_model.").unwrap()), m.text);
        assert_eq!(std::fs::metadata(&file).unwrap().len(), m.full.size);
        assert_eq!(sha256_file(&dir.join(&m.tokenizer.name), 0).unwrap(), m.tokenizer.sha256);
    }

    /// 고정한 매니페스트 값이 실제 모델 파일에서 계산한 값과 같은지 (모델 파일이 있을 때만)
    #[test]
    #[ignore = "모델 파일이 필요하다. EMBEDDINGGEMMA_DIR을 지정하고 --ignored로 실행한다"]
    fn pinned_manifest_matches_the_real_model_file() {
        let dir = PathBuf::from(std::env::var("EMBEDDINGGEMMA_DIR").expect("EMBEDDINGGEMMA_DIR"));
        let m = Manifest::embeddinggemma2();
        assert_eq!(Some(describe_text_spec(&dir.join(WEIGHTS_FILE)).unwrap()), m.text);
        assert_eq!(std::fs::metadata(dir.join(WEIGHTS_FILE)).unwrap().len(), m.full.size);
        assert_eq!(sha256_file(&dir.join(TOKENIZER_FILE), 0).unwrap(), m.tokenizer.sha256);
    }
}
