//! 실행 진입점. 평가 파이프라인과 로컬 보관함을 조립하고, UI 명령(`src/commands.rs`)을 표준 입출력으로 연다.
//!
//! 한 줄에 JSON 하나씩 읽고, 같은 `id`를 단 JSON 한 줄로 답한다 (형식은 `serve_lines` 참고).
//! 로그와 오류는 표준 오류로만 쓴다. 표준 출력은 응답 전용이다.
//!
//! 모델이 없어도 시작한다: `cmd_app_status`는 ready=false와 이유를 돌려주고, 모델 명령(`cmd_model_*`)으로 내려받을 수 있다.
//! 내려받기가 끝나면 다음 명령에서 코어(모델)를 다시 만든다. Tauri 셸(apps/desktop/src/lib.rs)과 같은 동작이다.
//!
//! 설정 (환경 변수)
//!   PD_MODEL_DIR       embeddinggemma-2 모델 폴더 (model.safetensors 또는 model-text.safetensors, tokenizer.json). 필수.
//!                      없으면 앱의 모델 내려받기가 여기에 받는다
//!   PD_DATA_DIR        로컬 보관함(LanceDB) 폴더. 필수. 없으면 만든다
//!   PD_SERVER_URL      서버 주소. 기본값 https://premium.discount
//!   PD_MODALITIES      auto(기본: 이미지·오디오 타워는 그 입력이 올 때 올림) | text(텍스트만, 타워를 올리지 않음)
//!   PD_QUANT           auto(기본: 모바일은 Q8_0, 데스크톱은 F32) | off | q8_0 (텍스트 백본 층 투영 양자화, quant.rs)
//!   PD_FILTER          auto(기본: laya 모델이 있으면 laya, 없으면 사전) | dictionary | laya (욕설 필터)
//!   PD_FILTER_MODEL_DIR  laya-multilingual 폴더. 기본값 <PD_MODEL_DIR의 부모>/laya-multilingual
//!   PD_DRAFT_MODEL_DIR   items 초안 모델(Qwen3.5-0.8B) 폴더. 기본값 <PD_MODEL_DIR의 부모>/qwen3.5-0.8b
//!   PD_MODEL_BASE_URL  모델을 받는 주소. 기본값 https://huggingface.co (리비전·해시는 고정이라 바꿔도 내용은 같아야 한다)
//!   PD_OAUTH_HASH      개발용: 로그인 세션의 hash (선택). PD_OAUTH_TOKEN과 함께 줘야 한다
//!   PD_OAUTH_TOKEN     개발용: 로그인 세션의 token (선택)
//! QR 로그인으로 받은 세션은 PD_DATA_DIR/session.json에 저장되고(소유자만 읽을 수 있음), 다음에 켤 때 복원된다.

use std::sync::Arc;

use premium_discount_app::commands::{model_command, serve_lines, Fetchers, SHUTDOWN_COMMAND};
use premium_discount_app::model_fetch::{Manifest, DEFAULT_BASE_URL};
use premium_discount_app::{
    platform, AppCore, AppError, AppInfo, AppStatus, CommandError, EmbeddingGemma, Evaluator, FilterSetting, HttpBackend, LoadOptions,
    Modalities, ModelFetcher, Outbox, QuantSetting, SessionStore, SwitchFilter,
};
use serde_json::Value;

const DEFAULT_SERVER_URL: &str = "https://premium.discount";

type Core = AppCore<HttpBackend, EmbeddingGemma, Arc<SwitchFilter>>;

fn usage() -> &'static str {
    "premium-discount-app: UI 명령을 표준 입출력(JSON 한 줄씩)으로 여는 실행 파일\n\
     환경 변수: PD_MODEL_DIR(필수) PD_DATA_DIR(필수) PD_SERVER_URL PD_MODALITIES PD_QUANT PD_FILTER PD_FILTER_MODEL_DIR PD_MODEL_BASE_URL PD_OAUTH_HASH PD_OAUTH_TOKEN\n\
     요청: {\"id\":1,\"cmd\":\"cmd_search_local\",\"args\":{\"query\":\"...\"}}\n\
     명령: cmd_submit_evaluation cmd_sync_cache cmd_search_local cmd_search_remote cmd_search_merged cmd_flush_outbox\n\
           cmd_session cmd_login_start cmd_login_status cmd_logout cmd_app_status cmd_queue_counts\n\
           cmd_model_status cmd_model_download cmd_model_download_cancel (모델이 없어도 동작)\n\
           cmd_shutdown (코어와 GPU 풀을 놓고 끝낸다)"
}

fn required(name: &str) -> Result<String, AppError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(AppError::InvalidInput(format!("{name} is required (see --help)"))),
    }
}

/// 환경 변수에서 읽은 설정
struct Config {
    model_dir: String,
    data_dir: String,
    server: String,
    modalities: Modalities,
    quant: QuantSetting,
    filter: FilterSetting,
    filter_dir: std::path::PathBuf,
    draft_dir: std::path::PathBuf,
    session: Option<(String, String)>,
}

impl Config {
    fn from_env() -> Result<Self, AppError> {
        let session = match (std::env::var("PD_OAUTH_HASH"), std::env::var("PD_OAUTH_TOKEN")) {
            (Ok(hash), Ok(token)) => Some((hash, token)),
            (Err(_), Err(_)) => None,
            _ => return Err(AppError::InvalidInput("PD_OAUTH_HASH and PD_OAUTH_TOKEN must be set together".into())),
        };
        Ok(Self {
            model_dir: required("PD_MODEL_DIR")?,
            data_dir: required("PD_DATA_DIR")?,
            server: std::env::var("PD_SERVER_URL").unwrap_or_else(|_| DEFAULT_SERVER_URL.to_string()),
            modalities: Modalities::parse(&std::env::var("PD_MODALITIES").unwrap_or_default()).map_err(AppError::InvalidInput)?,
            quant: QuantSetting::parse(&std::env::var("PD_QUANT").unwrap_or_default()).map_err(AppError::InvalidInput)?,
            filter: FilterSetting::parse(&std::env::var("PD_FILTER").unwrap_or_default()).map_err(AppError::InvalidInput)?,
            draft_dir: match std::env::var("PD_DRAFT_MODEL_DIR") {
                Ok(d) if !d.trim().is_empty() => d.into(),
                _ => {
                    let model = std::path::PathBuf::from(required("PD_MODEL_DIR")?);
                    model.parent().unwrap_or(std::path::Path::new(".")).join("qwen3.5-0.8b")
                }
            },
            filter_dir: match std::env::var("PD_FILTER_MODEL_DIR") {
                Ok(d) if !d.trim().is_empty() => d.into(),
                _ => {
                    let model = std::path::PathBuf::from(required("PD_MODEL_DIR")?);
                    model.parent().unwrap_or(std::path::Path::new(".")).join("laya-multilingual")
                }
            },
            session,
        })
    }

    fn info(&self) -> AppInfo {
        AppInfo { model_dir: self.model_dir.clone(), data_dir: self.data_dir.clone(), server_url: self.server.clone() }
    }
}

async fn build_core(config: &Config, filter: Arc<SwitchFilter>) -> Result<Core, AppError> {
    let mut backend = HttpBackend::new(config.server.clone())?;
    if let Some((hash, token)) = &config.session {
        backend = backend.with_session(hash.clone(), token.clone());
    }
    let model_dir = std::path::PathBuf::from(&config.model_dir);
    let options = LoadOptions {
        modalities: config.modalities,
        quant: config.quant.recipe(platform()),
        tower_bits: config.quant.tower_bits(platform()),
        ..Default::default()
    };
    // 모델 로드는 CPU를 몇 초 쓰므로 비동기 작업 스레드(내려받기도 여기서 돈다)를 막지 않게 따로 돌린다
    let embedder = tokio::task::spawn_blocking(move || EmbeddingGemma::load_with(&model_dir, options))
        .await
        .map_err(|e| AppError::Embed(format!("모델 로드 작업이 끝나지 않았습니다: {e}")))??;
    std::fs::create_dir_all(&config.data_dir).map_err(|e| AppError::Storage(format!("cannot create {}: {e}", config.data_dir)))?;
    let outbox = Outbox::open(&config.data_dir).await?;
    let store = SessionStore::new(std::path::Path::new(&config.data_dir).join("session.json"));
    // 웹 행동 분석 (사이트 주인이 받아 온 사건, 기기에서 구조화). 평가 보관함과 따로 둔다
    let analytic = premium_discount_app::analytic::AnalyticStore::open(std::path::Path::new(&config.data_dir).join("analytic")).await?;
    Ok(AppCore::new(Evaluator::new(backend, embedder, filter), outbox)
        .with_session_store(store)
        .with_info(config.info())
        .with_analytic(analytic))
}

/// 코어의 상태. 실패는 기억하되, 그 뒤에 모델 설치가 끝났으면(세대가 바뀌면) 다시 만든다.
enum CoreState {
    Empty,
    Ready(Arc<Core>),
    Failed { error: String, generation: u64 },
}

struct Shell {
    config: Config,
    fetcher: ModelFetcher,
    /// 욕설 필터 모델(laya) 내려받기
    filter_fetcher: ModelFetcher,
    /// 지금 쓰는 욕설 필터 (사전으로 시작, laya가 있으면 바꾼다)
    filter: Arc<SwitchFilter>,
    /// 필터를 마지막으로 확인한 설치 세대 (laya를 받으면 바뀐다)
    filter_checked: tokio::sync::Mutex<Option<u64>>,
    /// items 초안 모델 내려받기와 엔진 (첫 요청 때 올린다)
    draft_fetcher: ModelFetcher,
    draft: premium_discount_app::items_draft::DraftEngine,
    core: tokio::sync::Mutex<CoreState>,
}

impl Shell {
    /// 우아한 종료 (`cmd_shutdown`): 코어(모델 텐서 · LanceDB)를 먼저 놓고, GPU 가 쥔 캐시 풀을 드라이버에 돌려준다.
    /// Windows HIP 는 풀이 남은 채 프로세스가 끝나면 종료 단계에서 멈춘다(utils::release_gpu). 답한 뒤 serve_lines 가 끝난다
    async fn release(&self) -> Value {
        let had_core = {
            let mut state = self.core.lock().await;
            let previous = std::mem::replace(&mut *state, CoreState::Empty);
            matches!(previous, CoreState::Ready(_))
        };
        let released = match premium_discount_app::utils::device_from_env() {
            Ok(device) => premium_discount_app::utils::release_gpu(&device).is_ok(),
            Err(_) => false,
        };
        eprintln!("shutdown: core released ({had_core}), gpu pool released ({released})");
        serde_json::json!({ "shutdown": true, "core_released": had_core, "gpu_released": released })
    }

    async fn core(&self) -> Result<Arc<Core>, String> {
        let mut state = self.core.lock().await;
        match &*state {
            CoreState::Ready(core) => return Ok(core.clone()),
            CoreState::Failed { error, generation } if *generation == self.fetcher.generation() => return Err(error.clone()),
            _ => {}
        }
        match build_core(&self.config, self.filter.clone()).await {
            Ok(core) => {
                let core = Arc::new(core);
                *state = CoreState::Ready(core.clone());
                eprintln!("ready");
                Ok(core)
            }
            Err(e) => {
                let error = e.to_string();
                eprintln!("not ready: {error}");
                *state = CoreState::Failed { error: error.clone(), generation: self.fetcher.generation() };
                Err(error)
            }
        }
    }

    /// 설정이 허락하고 laya가 설치되어 있으면 필터를 laya로 바꾼다. 설치 세대가 바뀔 때만 다시 확인한다
    async fn refresh_filter(&self) {
        if self.config.filter == FilterSetting::Dictionary {
            return;
        }
        let generation = self.filter_fetcher.generation();
        let mut checked = self.filter_checked.lock().await;
        if *checked == Some(generation) {
            return;
        }
        let dir = self.config.filter_dir.clone();
        let bits = self.config.quant.tower_bits(platform());
        match tokio::task::spawn_blocking(move || premium_discount_app::laya::open_filter(&dir, bits)).await {
            Ok(Some(Ok(f))) => {
                self.filter.set(Arc::new(f), "laya");
                eprintln!("filter: laya-multilingual");
            }
            Ok(Some(Err(e))) => eprintln!("laya 필터를 열지 못했습니다 (사전 필터를 씁니다): {e}"),
            Ok(None) => {}
            Err(e) => eprintln!("laya 필터 로드 작업이 끝나지 않았습니다: {e}"),
        }
        *checked = Some(generation);
    }

    /// 이미 만든 코어만 (만들고 있거나 없으면 None). 모델 상태 조회가 코어 만들기를 기다리지 않게 한다
    fn ready_core(&self) -> Option<Arc<Core>> {
        match &*self.core.try_lock().ok()? {
            CoreState::Ready(core) => Some(core.clone()),
            _ => None,
        }
    }

    async fn handle(&self, cmd: String, args: Value) -> Result<Value, CommandError> {
        if cmd == SHUTDOWN_COMMAND {
            return Ok(self.release().await);
        }
        // 필터 모델을 막 받았으면 여기서 laya로 바꾼다 (세대가 같으면 바로 돌아온다)
        self.refresh_filter().await;
        if cmd == "cmd_item_draft" {
            let args = serde_json::from_value(if args.is_null() { Value::Object(Default::default()) } else { args })
                .map_err(|e| CommandError::new("invalid_input", format!("bad arguments: {e}")))?;
            let draft = self.draft.run(args).await?;
            return serde_json::to_value(draft).map_err(|e| CommandError::new("internal", e.to_string()));
        }
        let fetchers = Fetchers { model: &self.fetcher, filter: Some(&self.filter_fetcher), draft: Some(&self.draft_fetcher) };
        if let Some(result) = model_command(&fetchers, &cmd, args.clone()) {
            let mut value = result?;
            if cmd == "cmd_model_status" {
                if let Some(report) = self.ready_core().and_then(|c| c.residency()) {
                    value["residency"] = serde_json::to_value(report).unwrap_or(Value::Null);
                }
                value["filter"]["active"] = Value::from(self.filter.name());
                value["draft"]["active"] = Value::from(if self.draft.loaded() { "qwen" } else { "" });
            }
            return Ok(value);
        }
        match self.core().await {
            Ok(core) => core.dispatch(&cmd, args).await,
            Err(error) if cmd == "cmd_app_status" => {
                serde_json::to_value(AppStatus::not_ready(error, self.config.info())).map_err(|e| CommandError::new("internal", e.to_string()))
            }
            Err(error) => Err(CommandError::new("not_ready", error)),
        }
    }
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return;
    }
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let base = std::env::var("PD_MODEL_BASE_URL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let fetcher = match ModelFetcher::new(&config.model_dir, base.clone()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let filter_fetcher = match ModelFetcher::with_manifest(&config.filter_dir, base.clone(), Manifest::laya_multilingual()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let draft_fetcher = match ModelFetcher::with_manifest(&config.draft_dir, base.clone(), Manifest::qwen35_08b()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let draft = premium_discount_app::items_draft::DraftEngine::new(&config.draft_dir, config.quant.tower_bits(platform()));
    let shell = Shell {
        draft_fetcher,
        draft,
        config,
        fetcher,
        filter_fetcher,
        filter: Arc::new(SwitchFilter::dictionary()),
        filter_checked: tokio::sync::Mutex::new(None),
        core: tokio::sync::Mutex::new(CoreState::Empty),
    };
    // 시작할 때 한 번 만들어 본다 (모델이 있으면 첫 명령이 기다리지 않게). 실패해도 계속 연다
    let _ = shell.core().await;
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let shell = &shell;
    if let Err(e) = serve_lines(stdin, tokio::io::stdout(), |cmd, args| async move { shell.handle(cmd, args).await }).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
