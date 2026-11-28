//! premium.discount 데스크톱 앱: Tauri 창 하나가 화면(web/desktop/dist)을 보여 주고, 화면의 명령(`cmd_*`)을 앱 코어(상위 크레이트)에 연결한다.
//! 명령 계층(AppCore)은 프레임워크와 무관하므로 여기서는 감싸기만 한다. 설정은 settings.rs.
//! 데스크톱 창은 테두리 없는 투명 오버레이(tauri.conf.json)이고, 단계마다 크기 · 위치는 화면(web/desktop/stage.ts)이 바꾼다.
//!
//! 코어(모델 로드 약 2초)는 첫 명령이 올 때 만든다. 창은 바로 뜨고, 만들지 못하면(모델 폴더 없음 등)
//! 모델 명령(`cmd_model_*`) 말고는 not_ready 오류를 돌려주며, 화면이 원인과 "모델 내려받기"를 보여 준다.
//! 내려받기가 끝나면(설치 세대가 바뀌면) 다음 명령에서 코어를 다시 만든다.

mod settings;

use std::sync::Arc;

use premium_discount_app::{
    AppCore, AppError, AppInfo, AppStatus, CommandError, DownloadArgs, EmbeddingGemma, Evaluator, FetchProgress, FlushSummary, HttpBackend,
    FilterSetting, LoadOptions, LocalMatchDto, LoginStartResult, LoginStatus, MergedSearch, ModelFetcher, ModelStatus, Outbox, SearchArgs, SwitchFilter, Variant,
    SearchLocalArgs, SearchResponse, SessionArgs, SessionInfo, SessionStore, SubmitArgs, SubmitResult, SyncArgs,
};
use premium_discount_app::commands::{SupersedeArgs, SupersedeResult, VerdictsArgs, VerdictsResult, VoteArgs, VoteResult};
use serde_json::Value;
use settings::Settings;
use tauri::{Manager, State};

type Core = AppCore<HttpBackend, EmbeddingGemma, Arc<SwitchFilter>>;

/// 코어의 상태. 실패는 기억하되, 그 뒤에 모델 설치가 끝났으면(세대가 바뀌면) 다시 만든다.
enum CoreState {
    Empty,
    Ready(Arc<Core>),
    Failed { error: String, generation: u64 },
}

struct Shell {
    settings: Result<Settings, String>,
    /// 모델 내려받기 (설정을 읽지 못했으면 없다)
    fetcher: Option<ModelFetcher>,
    /// 욕설 필터 모델(laya) 내려받기
    filter_fetcher: Option<ModelFetcher>,
    /// 지금 쓰는 욕설 필터 (사전으로 시작, laya가 있으면 바꾼다)
    filter: Arc<SwitchFilter>,
    /// 필터를 마지막으로 확인한 설치 세대
    filter_checked: tokio::sync::Mutex<Option<u64>>,
    /// items 초안 모델 내려받기와 엔진 (첫 요청 때 올린다)
    draft_fetcher: Option<ModelFetcher>,
    draft: Option<premium_discount_app::items_draft::DraftEngine>,
    core: tokio::sync::Mutex<CoreState>,
}

impl Shell {
    fn info(&self) -> AppInfo {
        match &self.settings {
            Ok(s) => AppInfo { model_dir: s.model_dir.display().to_string(), data_dir: s.data_dir.display().to_string(), server_url: s.server_url.clone() },
            Err(_) => AppInfo::default(),
        }
    }

    fn generation(&self) -> u64 {
        self.fetcher.as_ref().map_or(0, |f| f.generation())
    }

    /// 설정이 허락하고 laya가 설치되어 있으면 필터를 laya로 바꾼다. 설치 세대가 바뀔 때만 다시 확인한다
    async fn refresh_filter(&self) {
        let (Ok(settings), Some(ff)) = (&self.settings, &self.filter_fetcher) else { return };
        if settings.filter == FilterSetting::Dictionary {
            return;
        }
        let generation = ff.generation();
        let mut checked = self.filter_checked.lock().await;
        if *checked == Some(generation) {
            return;
        }
        let dir = settings.filter_model_dir.clone();
        let bits = settings.quant.tower_bits(premium_discount_app::platform());
        match tauri::async_runtime::spawn_blocking(move || premium_discount_app::laya::open_filter(&dir, bits)).await {
            Ok(Some(Ok(f))) => self.filter.set(Arc::new(f), "laya"),
            Ok(Some(Err(e))) => eprintln!("laya 필터를 열지 못했습니다 (사전 필터를 씁니다): {e}"),
            Ok(None) => {}
            Err(e) => eprintln!("laya 필터 로드 작업이 끝나지 않았습니다: {e}"),
        }
        *checked = Some(generation);
    }

    /// 코어를 돌려준다. 처음 부를 때 만들고(동시에 여러 번 불려도 한 번만), 실패한 결과는 모델 설치가 끝날 때까지 기억한다.
    async fn core(&self) -> Result<Arc<Core>, CommandError> {
        self.refresh_filter().await;
        let mut state = self.core.lock().await;
        match &*state {
            CoreState::Ready(core) => return Ok(core.clone()),
            CoreState::Failed { error, generation } if *generation == self.generation() => {
                return Err(CommandError::new("not_ready", error.clone()))
            }
            _ => {}
        }
        let built = match &self.settings {
            Ok(settings) => build_core(settings, self.filter.clone()).await.map_err(|e| e.to_string()),
            Err(e) => Err(e.clone()),
        };
        match built {
            Ok(core) => {
                let core = Arc::new(core);
                *state = CoreState::Ready(core.clone());
                Ok(core)
            }
            Err(error) => {
                *state = CoreState::Failed { error: error.clone(), generation: self.generation() };
                Err(CommandError::new("not_ready", error))
            }
        }
    }

    /// 이미 만든 코어만 (만들고 있거나 없으면 None). 모델 상태 조회가 코어 만들기를 기다리지 않게 한다
    fn ready_core(&self) -> Option<Arc<Core>> {
        match &*self.core.try_lock().ok()? {
            CoreState::Ready(core) => Some(core.clone()),
            _ => None,
        }
    }

    fn fetcher(&self) -> Result<&ModelFetcher, CommandError> {
        self.fetcher.as_ref().ok_or_else(|| {
            CommandError::new("not_ready", self.settings.as_ref().err().cloned().unwrap_or_else(|| "설정을 읽지 못했습니다".into()))
        })
    }
}

async fn build_core(settings: &Settings, filter: Arc<SwitchFilter>) -> Result<Core, AppError> {
    let backend = HttpBackend::new(settings.server_url.clone())?;
    // 모델 로드는 CPU를 오래 쓰므로 비동기 작업 스레드를 막지 않게 따로 돌린다
    let model_dir = settings.model_dir.clone();
    let platform = premium_discount_app::platform();
    let options = LoadOptions {
        modalities: settings.modalities,
        quant: settings.quant.recipe(platform),
        encoder: settings.quant.encoder(),
        tower_bits: settings.quant.tower_bits(platform),
        vision_quant: Some(settings.quant.vision_plan(platform)),
        audio_quant: Some(settings.quant.audio_plan(platform)),
        ..Default::default()
    };
    let embedder = tauri::async_runtime::spawn_blocking(move || EmbeddingGemma::load_with(&model_dir, options))
        .await
        .map_err(|e| AppError::Embed(format!("모델 로드 작업이 끝나지 않았습니다: {e}")))??;
    std::fs::create_dir_all(&settings.data_dir).map_err(|e| AppError::Storage(format!("데이터 폴더를 만들 수 없습니다 ({}): {e}", settings.data_dir.display())))?;
    let outbox = Outbox::open(&settings.data_dir).await?;
    let store = SessionStore::new(settings.data_dir.join("session.json"));
    let info = AppInfo { model_dir: settings.model_dir.display().to_string(), data_dir: settings.data_dir.display().to_string(), server_url: settings.server_url.clone() };
    // 웹 행동 분석 저장소 (사이트 주인이 받아 온 사건). 헤드리스 코어(apps/core/src/main.rs)와 같은 자리
    let analytic = premium_discount_app::analytic::AnalyticStore::open(settings.data_dir.join("analytic")).await?;
    Ok(AppCore::new(Evaluator::new(backend, embedder, filter), outbox)
        .with_session_store(store)
        .with_info(info)
        .with_analytic(analytic))
}

// 웹 행동 분석 (사이트 주인): 사이트 확인 · 받기 + 구조화 · 리포트 · 지우기. 인자는 상위 크레이트(premium-discount-schema)의 형식 그대로다
#[tauri::command]
async fn cmd_analytic_site(shell: State<'_, Shell>, args: premium_discount_app::commands::AnalyticSiteArgs) -> Result<serde_json::Value, CommandError> {
    shell.core().await?.cmd_analytic_site(args).await
}

#[tauri::command]
async fn cmd_analytic_sync(shell: State<'_, Shell>, args: premium_discount_app::commands::AnalyticSyncArgs) -> Result<serde_json::Value, CommandError> {
    shell.core().await?.cmd_analytic_sync(args).await
}

#[tauri::command]
async fn cmd_analytic_report(
    shell: State<'_, Shell>,
    args: premium_discount_app::commands::AnalyticReportArgs,
) -> Result<premium_discount_app::analytic::AnalyticReport, CommandError> {
    shell.core().await?.cmd_analytic_report(args).await
}

#[tauri::command]
async fn cmd_analytic_forget(shell: State<'_, Shell>, args: premium_discount_app::commands::AnalyticForgetArgs) -> Result<serde_json::Value, CommandError> {
    shell.core().await?.cmd_analytic_forget(args).await
}

// ---- 명령: AppCore를 한 줄씩 감싼다. 이름과 인자(`args` 하나)는 화면의 web/desktop/commands.ts와 같다 ----

#[tauri::command]
async fn cmd_app_status(shell: State<'_, Shell>) -> Result<AppStatus, CommandError> {
    Ok(match shell.core().await {
        Ok(core) => core.cmd_app_status(),
        Err(e) => AppStatus::not_ready(e.message, shell.info()),
    })
}

// ---- 모델 명령: 코어(모델) 없이도 답한다 ----

#[tauri::command]
async fn cmd_model_status(shell: State<'_, Shell>) -> Result<ModelStatus, CommandError> {
    // 필터 모델을 막 받았으면 여기서 laya로 바꾼다 (세대가 같으면 바로 돌아온다)
    shell.refresh_filter().await;
    let mut status = shell.fetcher()?.status();
    status.residency = shell.ready_core().and_then(|c| c.residency());
    status.filter = shell.filter_fetcher.as_ref().map(|ff| {
        let mut info = ff.filter_info();
        info.active = shell.filter.name().into();
        info
    });
    status.draft = shell.draft_fetcher.as_ref().map(|d| {
        let mut info = d.filter_info();
        info.active = if shell.draft.as_ref().is_some_and(|e| e.loaded()) { "qwen".into() } else { String::new() };
        info
    });
    Ok(status)
}

#[tauri::command]
async fn cmd_model_download(shell: State<'_, Shell>, args: DownloadArgs) -> Result<FetchProgress, CommandError> {
    match args.variant {
        Variant::Filter => shell
            .filter_fetcher
            .as_ref()
            .map(|ff| ff.start(Variant::Filter))
            .ok_or_else(|| CommandError::new("not_ready", "필터 모델 내려받기를 쓸 수 없습니다 (설정 오류)")),
        Variant::Draft => shell
            .draft_fetcher
            .as_ref()
            .map(|d| d.start(Variant::Draft))
            .ok_or_else(|| CommandError::new("not_ready", "초안 모델 내려받기를 쓸 수 없습니다 (설정 오류)")),
        v => Ok(shell.fetcher()?.start(v)),
    }
}

#[tauri::command]
async fn cmd_model_download_cancel(shell: State<'_, Shell>) -> Result<FetchProgress, CommandError> {
    for extra in [&shell.filter_fetcher, &shell.draft_fetcher].into_iter().flatten() {
        extra.cancel();
    }
    Ok(shell.fetcher()?.cancel())
}

#[tauri::command]
async fn cmd_item_draft(
    shell: State<'_, Shell>,
    args: premium_discount_app::items_draft::DraftArgs,
) -> Result<premium_discount_app::items_draft::ItemDraft, CommandError> {
    match &shell.draft {
        Some(engine) => engine.run(args).await,
        None => Err(CommandError::new("not_ready", "초안 모델을 쓸 수 없습니다 (설정 오류)")),
    }
}

#[tauri::command]
async fn cmd_submit_evaluation(shell: State<'_, Shell>, args: SubmitArgs) -> Result<SubmitResult, CommandError> {
    shell.core().await?.cmd_submit_evaluation(args).await
}

#[tauri::command]
async fn cmd_sync_cache(shell: State<'_, Shell>, args: Option<SyncArgs>) -> Result<Vec<Value>, CommandError> {
    shell.core().await?.cmd_sync_cache(args.unwrap_or(SyncArgs { since: 0 })).await
}

#[tauri::command]
async fn cmd_search_local(shell: State<'_, Shell>, args: SearchLocalArgs) -> Result<Vec<LocalMatchDto>, CommandError> {
    shell.core().await?.cmd_search_local(args).await
}

#[tauri::command]
async fn cmd_search_remote(shell: State<'_, Shell>, args: SearchArgs) -> Result<SearchResponse, CommandError> {
    shell.core().await?.cmd_search_remote(args).await
}

#[tauri::command]
async fn cmd_search_merged(shell: State<'_, Shell>, args: SearchArgs) -> Result<MergedSearch, CommandError> {
    shell.core().await?.cmd_search_merged(args).await
}

#[tauri::command]
async fn cmd_flush_outbox(shell: State<'_, Shell>) -> Result<FlushSummary, CommandError> {
    shell.core().await?.cmd_flush_outbox().await
}

// 추천 / 비추천과 전역 판정. 코어(apps/core/src/commands.rs)와 화면(web/desktop/commands.ts)에는 있었지만 여기에 등록되지 않아
// Tauri 창에서는 투표가 unknown command 로 실패했다. 인자 형식은 상위 크레이트의 VoteArgs · VerdictsArgs 그대로다.
#[tauri::command]
async fn cmd_vote(shell: State<'_, Shell>, args: VoteArgs) -> Result<VoteResult, CommandError> {
    shell.core().await?.cmd_vote(args).await
}

/// 신구 대체 보고 (premium-discount-relay 의 비교 결과). 서버가 받으면 옛 평가의 판정 캐시가 곧바로 discount(−2, superseded)
#[tauri::command]
async fn cmd_supersede(shell: State<'_, Shell>, args: SupersedeArgs) -> Result<SupersedeResult, CommandError> {
    shell.core().await?.cmd_supersede(args).await
}

#[tauri::command]
async fn cmd_verdicts(shell: State<'_, Shell>, args: VerdictsArgs) -> Result<VerdictsResult, CommandError> {
    shell.core().await?.cmd_verdicts(args).await
}

#[tauri::command]
async fn cmd_session(shell: State<'_, Shell>, args: Option<SessionArgs>) -> Result<SessionInfo, CommandError> {
    shell.core().await?.cmd_session(args.unwrap_or_default()).await
}

#[tauri::command]
async fn cmd_login_start(shell: State<'_, Shell>) -> Result<LoginStartResult, CommandError> {
    shell.core().await?.cmd_login_start().await
}

#[tauri::command]
async fn cmd_login_status(shell: State<'_, Shell>) -> Result<LoginStatus, CommandError> {
    shell.core().await?.cmd_login_status().await
}

#[tauri::command]
async fn cmd_logout(shell: State<'_, Shell>) -> Result<SessionInfo, CommandError> {
    shell.core().await?.cmd_logout().await
}

/// 앱 실행. 데스크톱은 main.rs, 모바일은 Tauri가 만든 진입점이 부른다.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 메일 앱(mailto:)을 여는 데 쓴다. 허용 범위는 capabilities/default.json에서 mailto:로 제한한다
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_default = app.path().app_data_dir()?;
            let config_dir = app.path().app_config_dir()?;
            let settings = settings::resolve(&|k| std::env::var(k).ok(), &config_dir, &data_default);
            let fetcher = settings.as_ref().ok().and_then(|s| ModelFetcher::new(&s.model_dir, s.model_base_url.clone()).ok());
            let filter_fetcher = settings.as_ref().ok().and_then(|s| {
                ModelFetcher::with_manifest(
                    &s.filter_model_dir,
                    s.model_base_url.clone(),
                    premium_discount_app::model_fetch::Manifest::laya_multilingual(),
                )
                .ok()
            });
            let draft_fetcher = settings.as_ref().ok().and_then(|s| {
                ModelFetcher::with_manifest(&s.draft_model_dir, s.model_base_url.clone(), premium_discount_app::model_fetch::Manifest::qwen35_08b()).ok()
            });
            let draft = settings.as_ref().ok().map(|s| {
                premium_discount_app::items_draft::DraftEngine::new(&s.draft_model_dir, s.quant.tower_bits(premium_discount_app::platform()))
            });
            app.manage(Shell {
                draft_fetcher,
                draft,
                settings,
                fetcher,
                filter_fetcher,
                filter: Arc::new(SwitchFilter::dictionary()),
                filter_checked: tokio::sync::Mutex::new(None),
                core: tokio::sync::Mutex::new(CoreState::Empty),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            cmd_app_status,
            cmd_model_status,
            cmd_model_download,
            cmd_model_download_cancel,
            cmd_item_draft,
            cmd_submit_evaluation,
            cmd_sync_cache,
            cmd_search_local,
            cmd_search_remote,
            cmd_search_merged,
            cmd_flush_outbox,
            cmd_vote,
            cmd_supersede,
            cmd_verdicts,
            cmd_session,
            cmd_login_start,
            cmd_login_status,
            cmd_logout,
            cmd_analytic_site,
            cmd_analytic_sync,
            cmd_analytic_report,
            cmd_analytic_forget,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 앱을 초기화하지 못했습니다")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(shell) = app_handle.try_state::<Shell>() {
                    if let Ok(mut guard) = shell.core.try_lock() {
                        *guard = CoreState::Empty;
                    }
                    if let Some(draft) = &shell.draft {
                        let _ = draft.try_release();
                    }
                    shell.filter.set(Arc::new(premium_discount_app::filter::LocalFilter::default()), "dictionary");
                }
                let _ = premium_discount_app::utils::release_opened_gpu();
                premium_discount_app::utils::rocm_exit_now_if_used(0);
            }
        });
}
