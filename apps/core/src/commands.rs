//! UI가 부르는 명령 계층.
//!
//! 프레임워크에 묶이지 않는다. 명령마다 직렬화 가능한 인자(`args`) 하나를 받아 직렬화 가능한 결과를 돌려준다.
//! Tauri 셸은 명령마다 `#[tauri::command]` 함수 한 줄로 이 계층을 부르면 되고(`apps/core/README.md`의 "Tauri 셸 연결"),
//! 지금 실행 파일(`src/main.rs`)은 같은 명령을 표준 입출력 JSON 한 줄씩으로 열어 둔다.
//!
//! 명령 (이름은 UI의 `web/desktop/commands.ts`와 같다)
//! - `cmd_submit_evaluation`: 욕설 필터 → 임베딩 → 로컬 저장소 기록 → 서버 전송 (`Evaluator::submit_outbox`)
//! - `cmd_sync_cache`: `since` 이후 바뀐 로컬 행을 화면 캐시(Dexie) 형태로 (`Outbox::cache_rows_since`)
//! - `cmd_search_local`: 내 글 검색 (로컬 벡터 검색, 원문 포함)
//! - `cmd_search_remote`: 서버 벡터 검색
//! - `cmd_search_merged`: 위 둘을 한 번의 임베딩으로 합쳐서 (방금 저장한 글이 서버 검색에 아직 없는 동안에도 보인다)
//! - `cmd_flush_outbox`: 못 보낸 항목(평가와 투표) 전송 + 접수만 된 항목의 최종 결과 확인. 앱을 켤 때와 연결이 돌아왔을 때 부른다
//! - `cmd_vote`: 남의 평가에 추천/비추천(또는 철회). 투표 보관함에 먼저 쓰고 바로 보낸다 (오프라인이면 queued)
//! - `cmd_verdicts`: 평가들의 전역 판정 (premium | neutral | discount | unrated). 캐시가 오래됐으면 서버에 묻는다
//! - `cmd_app_status`: 앱이 준비되었는지와 설정 경로 (시작에 실패했을 때 화면이 원인을 보여 주는 데 쓴다)
//! - `cmd_session`, `cmd_login_start`, `cmd_login_status`, `cmd_logout`: QR 로그인. 서버가 세션을 발급하고(`/session/start`),
//!   사용자가 QR의 주소로 메일을 보내면 로그인이 끝난다. 세션(hash, token)은 Rust 안에만 있고 화면에는 주소만 넘긴다
//!
//! 읽기 명령(캐시 동기화, 검색)은 서로 막지 않는다. 저장과 전송은 보관함을 독점한다.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock};

use crate::api::{EvalBackend, SearchResponse, Session};
use crate::embedding::Embedder;
use crate::error::AppError;
use crate::filter::ProfanityFilter;
use crate::local::LocalMatch;
use crate::outbox::Outbox;
use crate::pipeline::{Evaluator, MergedSearch, SearchOptions, SubmitInput, SubmitOutcome};
use crate::queue::RejectedItem;
use crate::session::{SavedSession, SessionStore};

// 명령의 인자 · 결과 · 오류 타입은 공용 crate(`crates/premium-discount-schema` 의 `contract`)에 있다. premium-discount-mcp
// 와 raon-zena 가 같은 정의를 쓴다. 앱 안의 타입에 기대는 결과(`VerdictsResult`, `AppStatus`)만 여기에 남는다.
use premium_discount_schema::contract::blank_to_none;
pub use premium_discount_schema::contract::{
    AnalyticForgetArgs, AnalyticReportArgs, AnalyticSiteArgs, AnalyticSyncArgs,
    AppInfo, CommandError, FlushSummary, LocalMatchDto, LoginStartResult, LoginStatus, RejectedDto, SearchArgs, SearchLocalArgs,
    SessionArgs, SessionInfo, SubmitArgs, SubmitResult, SupersedeArgs, SupersedeResult, SyncArgs, VerdictsArgs, VoteArgs,
    VoteRejectedDto, VoteResult, COMMANDS,
    DEFAULT_TOP_K, ERROR_KINDS,
};

// ---- 인자 ----


impl From<SubmitArgs> for SubmitInput {
    fn from(a: SubmitArgs) -> Self {
        SubmitInput {
            text: a.text,
            cc: a.cc,
            from: blank_to_none(a.from),
            to: blank_to_none(a.to),
            bcc: blank_to_none(a.bcc),
            ref_: blank_to_none(a.ref_),
            local_parent: blank_to_none(a.local_parent),
        }
    }
}




fn search_options(a: &SearchArgs) -> SearchOptions {
    SearchOptions {
        top_k: a.top_k,
        cc: blank_to_none(a.cc.clone()),
        root: blank_to_none(a.root.clone()),
        ref_: blank_to_none(a.ref_.clone()),
        to: blank_to_none(a.to.clone()),
        tier: blank_to_none(a.tier.clone()),
    }
}



// ---- 결과 ----


impl From<SubmitOutcome> for SubmitResult {
    fn from(o: SubmitOutcome) -> Self {
        match o {
            SubmitOutcome::Blocked => SubmitResult::Blocked,
            SubmitOutcome::Saved(r) => SubmitResult::Saved { id: r.id, root: r.root, ref_: r.ref_, from: r.from },
            SubmitOutcome::Accepted(r) => SubmitResult::Accepted { id: r.id, root: r.root, ref_: r.ref_, from: r.from },
            SubmitOutcome::Queued { local_id } => SubmitResult::Queued { local_id },
        }
    }
}


impl From<LocalMatch> for LocalMatchDto {
    fn from(m: LocalMatch) -> Self {
        LocalMatchDto {
            request_id: m.request_id,
            local_id: m.local_id,
            id: m.id,
            cc: m.cc,
            state: m.state.as_str().to_string(),
            text: m.text,
            score: m.score,
        }
    }
}


impl From<RejectedItem> for RejectedDto {
    fn from(r: RejectedItem) -> Self {
        RejectedDto { local_id: r.local_id, reason: r.reason }
    }
}


/// 신구 대체 보고의 멱등 키: `sup-` + sha256(old | new) 앞 48자 (서버 규칙 16~64자 [A-Za-z0-9_-])
fn supersede_request_id(old: &str, new: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{old}|{new}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sup-{}", &hex[..48])
}

/// `cmd_verdicts` 결과
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerdictsResult {
    /// 요청한 순서. 한 번도 받은 적 없는 id는 빠지고 `missing`에 있다
    pub verdicts: Vec<crate::votes::CachedVerdict>,
    pub missing: Vec<String>,
    /// 서버에 묻지 못해 캐시 값을 그대로 돌려줬는지
    pub stale: bool,
    pub remote_error: Option<String>,
}



// ---- 로그인 ----

/// QR 로그인을 기다리는 시간 (이 시간이 지나면 새로 시작해야 한다)
pub const LOGIN_TTL: Duration = Duration::from_secs(15 * 60);


/// 앱이 실행 중인 플랫폼 종류. 화면이 로그인 방식(QR 또는 메일 링크)을 고르는 데 쓴다.
/// 같은 기기의 메일 앱으로 바로 갈 수 있는 모바일(android, ios)과 그 밖(desktop)을 구분한다.
pub fn platform() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        "desktop"
    }
}

/// `cmd_app_status` 결과. ready가 false이면 error에 이유가 있다 (셸이 코어를 만들지 못한 경우).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppStatus {
    pub ready: bool,
    pub error: Option<String>,
    #[serde(flatten)]
    pub info: AppInfo,
    pub version: &'static str,
    /// android | ios | desktop
    pub platform: &'static str,
    /// 모델 타워의 상주 상태 (코어가 있을 때만)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<crate::residency::ResidencyReport>,
}

impl AppStatus {
    pub fn ready(info: AppInfo) -> Self {
        Self { ready: true, error: None, info, version: env!("CARGO_PKG_VERSION"), platform: platform(), model: None }
    }

    /// 코어를 만들지 못했을 때 (예: 모델 폴더가 없다). 모델 명령(cmd_model_*) 말고는 모두 not_ready 오류가 된다
    pub fn not_ready(error: impl Into<String>, info: AppInfo) -> Self {
        Self { ready: false, error: Some(error.into()), info, version: env!("CARGO_PKG_VERSION"), platform: platform(), model: None }
    }
}

/// 인자를 생략하면 빈 객체로 본다 (기본값만 쓰는 명령)
fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, CommandError> {
    let args = if args.is_null() { Value::Object(Default::default()) } else { args };
    serde_json::from_value(args).map_err(|e| CommandError::new("invalid_input", format!("bad arguments: {e}")))
}

fn out<T: Serialize>(v: T) -> Result<Value, CommandError> {
    serde_json::to_value(v).map_err(|e| CommandError::new("internal", e.to_string()))
}

/// 셸이 가진 내려받기 관리자들: 임베딩 모델과 (있으면) 욕설 필터 모델
pub struct Fetchers<'a> {
    pub model: &'a crate::model_fetch::ModelFetcher,
    pub filter: Option<&'a crate::model_fetch::ModelFetcher>,
    /// items 초안 모델 (Qwen3.5-0.8B)
    pub draft: Option<&'a crate::model_fetch::ModelFetcher>,
}

/// 모델(코어) 없이도 답하는 명령: 모델 상태와 내려받기. 셸(Tauri, 표준 입출력)이 코어보다 먼저 확인한다.
/// `variant: "filter"`는 욕설 필터 모델을 받는다. 모델 명령이 아니면 None.
pub fn model_command(f: &Fetchers, cmd: &str, args: Value) -> Option<Result<Value, CommandError>> {
    use crate::model_fetch::Variant;
    match cmd {
        "cmd_model_status" => {
            let mut status = f.model.status();
            status.filter = f.filter.map(|ff| ff.filter_info());
            status.draft = f.draft.map(|d| {
                let mut info = d.filter_info();
                info.active = String::new();
                info
            });
            Some(out(status))
        }
        "cmd_model_download" => Some(parse::<crate::model_fetch::DownloadArgs>(args).and_then(|a| match a.variant {
            Variant::Filter => match f.filter {
                Some(ff) => out(ff.start(Variant::Filter)),
                None => Err(CommandError::new("invalid_input", "이 셸에는 필터 모델 내려받기가 없습니다")),
            },
            Variant::Draft => match f.draft {
                Some(d) => out(d.start(Variant::Draft)),
                None => Err(CommandError::new("invalid_input", "이 셸에는 초안 모델 내려받기가 없습니다")),
            },
            v => out(f.model.start(v)),
        })),
        "cmd_model_download_cancel" => {
            for extra in [f.filter, f.draft].into_iter().flatten() {
                extra.cancel();
            }
            Some(out(f.model.cancel()))
        }
        _ => None,
    }
}

/// 한 줄에 JSON 하나씩 읽어 `handle(명령, 인자)`로 처리하고, 같은 `id`를 달아 한 줄로 답한다. 입력이 끝나면 돌아온다.
///
/// 요청: `{"id": 1, "cmd": "cmd_search_local", "args": {"query": "..."}}`
/// 응답: `{"id": 1, "ok": true, "result": ...}` 또는 `{"id": 1, "ok": false, "error": {"kind": "...", "message": "..."}}`
/// 요청은 차례로 처리한다. 형식이 틀린 줄에는 `id: null`로 오류를 답하고 계속 읽는다.
pub async fn serve_lines<R, W, F, Fut>(reader: R, mut writer: W, mut handle: F) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(String, Value) -> Fut,
    Fut: std::future::Future<Output = Result<Value, CommandError>>,
{
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Err(e) => error_reply(Value::Null, CommandError::new("invalid_input", format!("not valid JSON: {e}"))),
            Ok(req) => {
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                match req.get("cmd").and_then(Value::as_str) {
                    None => error_reply(id, CommandError::new("invalid_input", "request needs a string \"cmd\"")),
                    Some(cmd) => {
                        let args = req.get("args").cloned().unwrap_or(Value::Null);
                        match handle(cmd.to_string(), args).await {
                            Ok(result) => serde_json::json!({ "id": id, "ok": true, "result": result }),
                            Err(e) => error_reply(id, e),
                        }
                    }
                }
            }
        };
        writer.write_all(reply.to_string().as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}





struct PendingLogin {
    session: Session,
    started: Instant,
}

#[derive(Default)]
struct Account {
    address: Option<String>,
    warning: Option<String>,
}


impl From<AppError> for CommandError {
    fn from(e: AppError) -> Self {
        let kind = match &e {
            AppError::InvalidInput(_) => "invalid_input",
            AppError::Embed(_) => "embed",
            AppError::Filter(_) => "filter",
            AppError::Offline(_) => "offline",
            AppError::Http(_) => "http",
            AppError::BadResponse(_) => "bad_response",
            AppError::Server { .. } => "server",
            AppError::Rejected(_) => "rejected",
            AppError::QueueFull(_) => "queue_full",
            AppError::Storage(_) => "storage",
        };
        CommandError::new(kind, e.to_string())
    }
}

// ---- 명령 계층 ----

/// 평가 파이프라인과 로컬 보관함을 함께 들고 있는 앱 코어
pub struct AppCore<B, E, F> {
    evaluator: Evaluator<B, E, F>,
    outbox: RwLock<Outbox>,
    store: Option<SessionStore>,
    login_ttl: Duration,
    pending: Mutex<Option<PendingLogin>>,
    account: RwLock<Account>,
    info: AppInfo,
    /// 웹 행동 분석 저장소 (사이트 주인 쪽). 없으면 분석 명령은 not_ready
    analytic: Option<crate::analytic::AnalyticStore>,
}

impl<B: EvalBackend, E: Embedder, F: ProfanityFilter> AppCore<B, E, F> {
    pub fn new(evaluator: Evaluator<B, E, F>, outbox: Outbox) -> Self {
        Self {
            evaluator,
            outbox: RwLock::new(outbox),
            store: None,
            login_ttl: LOGIN_TTL,
            pending: Mutex::new(None),
            account: RwLock::new(Account::default()),
            info: AppInfo::default(),
            analytic: None,
        }
    }

    /// 웹 행동 분석 저장소를 붙인다 (`<data_dir>/analytic`)
    pub fn with_analytic(mut self, store: crate::analytic::AnalyticStore) -> Self {
        self.analytic = Some(store);
        self
    }

    fn analytic_store(&self) -> Result<&crate::analytic::AnalyticStore, CommandError> {
        self.analytic.as_ref().ok_or_else(|| CommandError::new("not_ready", "web analytics store is not open"))
    }

    fn analytic_host(raw: &str) -> Result<String, CommandError> {
        let raw = raw.trim();
        let host = premium_discount_analytic::event::host_of(raw)
            .or_else(|| premium_discount_analytic::event::host_of(&format!("https://{raw}")))
            .filter(|h| h.contains('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'));
        host.ok_or_else(|| CommandError::new("invalid_input", "host must be a domain name (shop.example.com)"))
    }

    /// 웹 행동 분석: 사이트 확인. 서버가 사이트 첫 화면의 `<meta name="premium-discount-site" content="<로그인 주소>">` 를 본다
    pub async fn cmd_analytic_site(&self, args: AnalyticSiteArgs) -> Result<Value, CommandError> {
        let host = Self::analytic_host(&args.host)?;
        Ok(self.evaluator.backend().analytic_site(&host).await?)
    }

    /// 웹 행동 분석: 서버에서 원시 사건을 받아 넣고(DELTA · TOMBSTONE GUARD), 기기에서 구조화한다
    pub async fn cmd_analytic_sync(&self, args: AnalyticSyncArgs) -> Result<Value, CommandError> {
        let store = self.analytic_store()?;
        let host = Self::analytic_host(&args.host)?;
        let max_pages = args.max_pages.map(|p| p.clamp(1, crate::analytic::MAX_PAGES as u32) as usize).unwrap_or(crate::analytic::MAX_PAGES);
        let mut total = crate::analytic::IngestReport::default();
        let mut pages = 0;
        let mut cursor = store.cursor(&host).await?;
        loop {
            let page = self.evaluator.backend().analytic_events(&host, &cursor, 500).await?;
            pages += 1;
            let r = store.ingest(&host, &page.rows).await?;
            total.received += r.received;
            total.inserted += r.inserted;
            total.kept_structured += r.kept_structured;
            total.blocked_tombstoned += r.blocked_tombstoned;
            total.broken += r.broken;
            match page.next {
                Some(next) if page.more && pages < max_pages => cursor = next,
                _ => break,
            }
        }
        let structured = store.structure(self.evaluator.embedder(), Some(&host), 2_000, crate::analytic::now_ms()).await?;
        Ok(serde_json::json!({ "host": host, "pages": pages, "ingest": total, "structured": structured }))
    }

    /// 웹 행동 분석: 자연어 질의 리포트 (기간 재계산 · 퍼널 · 병목 · 선택 대조 · 요약글 검색)
    pub async fn cmd_analytic_report(&self, args: AnalyticReportArgs) -> Result<crate::analytic::AnalyticReport, CommandError> {
        let store = self.analytic_store()?;
        let host = Self::analytic_host(&args.host)?;
        let query = crate::analytic::parse_query(&args.query, args.lang.as_deref().unwrap_or("ko"), crate::analytic::now_ms());
        let range = query.range.as_ref().map(|r| (r.start_ms, r.end_ms));
        let events = store.events(&host, range).await?;
        let top_k = args.top_k.unwrap_or(5).min(50) as usize;
        let prompted = crate::embeddinggemma::with_prompt(crate::embeddinggemma::Role::Query, &args.query);
        let vector = crate::embedding::l2_normalize(self.evaluator.embedder().embed(&prompted)?)?;
        let matches = store.search(&vector, top_k, &host, range).await?;
        Ok(crate::analytic::build_report(&host, query, &events, matches))
    }

    /// 웹 행동 분석: 사건 지우기. 묘비를 남겨 다시 받지 않는다
    pub async fn cmd_analytic_forget(&self, args: AnalyticForgetArgs) -> Result<Value, CommandError> {
        let store = self.analytic_store()?;
        let deleted = store.forget(&args.ids, crate::analytic::now_ms()).await?;
        Ok(serde_json::json!({ "deleted": deleted }))
    }

    /// 화면에 보여 줄 경로와 서버 주소
    pub fn with_info(mut self, info: AppInfo) -> Self {
        self.info = info;
        self
    }

    pub fn cmd_app_status(&self) -> AppStatus {
        AppStatus { model: self.residency(), ..AppStatus::ready(self.info.clone()) }
    }

    /// 모델 타워의 상주 상태 (임베더가 알려 줄 때만)
    pub fn residency(&self) -> Option<crate::residency::ResidencyReport> {
        self.evaluator.embedder().residency()
    }

    /// 로그인 세션을 이 저장소에 남기고, 저장된 세션이 있으면 복원한다.
    /// 읽지 못하면(깨진 파일 등) 로그아웃 상태로 시작하고, 그 이유를 `cmd_session`의 warning으로 알린다.
    pub fn with_session_store(mut self, store: SessionStore) -> Self {
        let account = self.account.get_mut();
        match store.load() {
            Ok(Some(saved)) => {
                self.evaluator.backend().set_session(saved.session());
                account.address = saved.address;
            }
            Ok(None) => {}
            Err(e) => account.warning = Some(format!("저장된 로그인 정보를 읽지 못해 로그아웃 상태로 시작했습니다 ({e})")),
        }
        self.store = Some(store);
        self
    }

    /// 로그인 대기 시간을 바꾼다 (테스트용)
    pub fn with_login_ttl(mut self, ttl: Duration) -> Self {
        self.login_ttl = ttl;
        self
    }

    pub async fn cmd_submit_evaluation(&self, args: SubmitArgs) -> Result<SubmitResult, CommandError> {
        let mut outbox = self.outbox.write().await;
        let outcome = self.evaluator.submit_outbox(args.into(), &mut outbox).await?;
        Ok(outcome.into())
    }

    /// `since` 이후 바뀐 행을 화면 캐시 형태로 돌려준다. 화면은 이 행을 그대로 `bulkPut`한다 (`web/desktop/localdb.ts`의 `syncFromRust`)
    pub async fn cmd_sync_cache(&self, args: SyncArgs) -> Result<Vec<Value>, CommandError> {
        Ok(self.outbox.read().await.cache_rows_since(args.since).await?)
    }

    pub async fn cmd_search_local(&self, args: SearchLocalArgs) -> Result<Vec<LocalMatchDto>, CommandError> {
        let outbox = self.outbox.read().await;
        let cc = blank_to_none(args.cc);
        let found = self.evaluator.search_local(&args.query, args.top_k, cc.as_deref(), &outbox).await?;
        Ok(found.into_iter().map(Into::into).collect())
    }

    pub async fn cmd_search_remote(&self, args: SearchArgs) -> Result<SearchResponse, CommandError> {
        let mut found = self.evaluator.search(&args.query, search_options(&args)).await?;
        let ids: Vec<String> = found.matches.iter().map(|m| m.id.clone()).collect();
        let mine = self.outbox.read().await.votes().my_votes(&ids).await?;
        for m in &mut found.matches {
            m.my_vote = mine.get(&m.id).copied();
        }
        Ok(found)
    }

    pub async fn cmd_search_merged(&self, args: SearchArgs) -> Result<MergedSearch, CommandError> {
        let outbox = self.outbox.read().await;
        let mut found = self.evaluator.search_merged(&args.query, search_options(&args), &outbox).await?;
        let ids: Vec<String> = found.matches.iter().filter_map(|m| m.id.clone()).collect();
        let mine = outbox.votes().my_votes(&ids).await?;
        for m in &mut found.matches {
            m.my_vote = m.id.as_ref().and_then(|id| mine.get(id).copied());
        }
        Ok(found)
    }

    /// 추천/비추천. 보관함에 먼저 쓰고(같은 평가에 못 보낸 표가 있으면 바꾼다) 바로 보낸다
    pub async fn cmd_vote(&self, args: VoteArgs) -> Result<VoteResult, CommandError> {
        let id = crate::pipeline::validate_uuid("id", args.id.trim())?;
        let kind = args.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()).unwrap_or("vote");
        let kind = crate::votes::VoteKind::parse(kind)?;
        let outbox = self.outbox.write().await;
        let rec = outbox.votes().put_vote(&id, kind, args.value).await?;
        let report = outbox.votes().flush(self.evaluator.backend()).await;
        if let Some((_, resp)) = report.sent.iter().find(|(rid, _)| *rid == rec.request_id) {
            return Ok(VoteResult::Sent { request_id: rec.request_id, counted: resp.counted });
        }
        if let Some(r) = report.rejected.iter().find(|r| r.request_id == rec.request_id) {
            return Err(CommandError::new("rejected", r.reason.clone()));
        }
        Ok(VoteResult::Queued { request_id: rec.request_id })
    }

    /// 신구 대체 보고 (premium-discount-relay 의 비교 결과, 보통 Raon Zena 의 판정 보조가 보낸다). 서버가 받으면(202 · 같은 보고 200)
    /// 옛 평가의 판정 캐시를 곧바로 discount(−2, superseded)로 바꾼다. request_id 는 쌍에서 정해져 다시 보내도 서버에는 한 번이고,
    /// 연결이 없으면 offline 오류라 부른 쪽이 다음 동기화 때 다시 보낸다. 서버의 거절(400 · 401 · 403 · 404)은 그대로 오류다.
    pub async fn cmd_supersede(&self, args: SupersedeArgs) -> Result<SupersedeResult, CommandError> {
        let old = crate::pipeline::validate_uuid("old", args.old.trim())?;
        let new = crate::pipeline::validate_uuid("new", args.new.trim())?;
        if old == new {
            return Err(CommandError::new("invalid_input", "old and new must be different evaluations"));
        }
        let request_id = supersede_request_id(&old, &new);
        let req = crate::api::SupersedeRequest { request_id: request_id.clone(), old: old.clone(), new: new.clone() };
        match self.evaluator.backend().supersede(&req).await {
            Ok(_) => {}
            // 서버가 받지 않은 보고(형식 · 장소 · 대상 · 시각 · 보고자 · 없는 평가)는 다시 보내도 같다
            Err(AppError::Server { status, message }) if (400..500).contains(&status) && status != 408 && status != 429 => {
                return Err(CommandError::new("rejected", format!("server returned {status}: {message}")));
            }
            Err(e) => return Err(e.into()),
        }
        self.outbox.read().await.votes().mark_superseded(&old, crate::queue::now_ms()).await?;
        Ok(SupersedeResult { old, new, request_id })
    }

    /// 전역 판정. 캐시에 없거나 오래된(VERDICT_TTL_MS) 것이 있거나 refresh면 서버에 묻는다 (50개씩)
    pub async fn cmd_verdicts(&self, args: VerdictsArgs) -> Result<VerdictsResult, CommandError> {
        let mut ids: Vec<String> = Vec::new();
        for raw in &args.ids {
            let id = crate::pipeline::validate_uuid("ids", raw.trim())?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let max = 4 * crate::api::VERDICT_IDS_MAX;
        if ids.len() > max {
            return Err(CommandError::new("invalid_input", format!("at most {max} ids")));
        }
        let outbox = self.outbox.read().await;
        let votes = outbox.votes();
        let now = crate::queue::now_ms();
        let cached = votes.cached_verdicts(&ids).await?;
        let fresh = |id: &String| cached.get(id).is_some_and(|c| now - c.fetched_at < crate::votes::VERDICT_TTL_MS);
        let mut remote_error = None;
        if args.refresh || !ids.iter().all(fresh) {
            for chunk in ids.chunks(crate::api::VERDICT_IDS_MAX) {
                match self.evaluator.backend().verdicts(chunk).await {
                    Ok(resp) => votes.store_verdicts(&resp.verdicts, now).await?,
                    Err(e) => {
                        remote_error = Some(e.to_string());
                        break;
                    }
                }
            }
        }
        let mut cached = votes.cached_verdicts(&ids).await?;
        let mut verdicts = Vec::new();
        let mut missing = Vec::new();
        for id in ids {
            match cached.remove(&id) {
                Some(c) => verdicts.push(c),
                None => missing.push(id),
            }
        }
        Ok(VerdictsResult { verdicts, missing, stale: remote_error.is_some(), remote_error })
    }

    pub async fn cmd_flush_outbox(&self) -> Result<FlushSummary, CommandError> {
        let mut outbox = self.outbox.write().await;
        let flushed = self.evaluator.flush_outbox(&mut outbox).await;
        let synced = self.evaluator.sync_outbox_status(&mut outbox).await;
        let voted = outbox.votes().flush(self.evaluator.backend()).await;
        Ok(FlushSummary {
            sent: flushed.sent.len(),
            rejected: flushed.rejected.into_iter().map(Into::into).collect(),
            remaining: flushed.remaining,
            stopped: flushed.stopped,
            stored: synced.stored.len(),
            sync_rejected: synced.rejected.into_iter().map(Into::into).collect(),
            missing: synced.missing.len(),
            still_pending: synced.still_pending,
            sync_stopped: synced.stopped,
            persist_error: flushed.persist_error.or(synced.persist_error).or(voted.persist_error),
            votes_sent: voted.sent.len(),
            votes_remaining: voted.remaining,
            votes_rejected: voted
                .rejected
                .into_iter()
                .map(|r| VoteRejectedDto { request_id: r.request_id, eval_id: r.eval_id, reason: r.reason })
                .collect(),
            votes_stopped: voted.stopped,
        })
    }

    // ---- 로그인 명령 ----

    async fn signed_in_info(&self) -> SessionInfo {
        let account = self.account.read().await;
        let signed_in = !self.evaluator.backend().session().is_anonymous();
        SessionInfo {
            state: if signed_in { "signed_in" } else { "anonymous" },
            address: if signed_in { account.address.clone() } else { None },
            warning: account.warning.clone(),
        }
    }

    pub async fn cmd_session(&self, args: SessionArgs) -> Result<SessionInfo, CommandError> {
        let backend = self.evaluator.backend();
        if args.verify && !backend.session().is_anonymous() {
            // 연결이 없거나 서버 오류면 로그인을 그대로 둔다. 서버가 이 세션을 모른다고 할 때만 로그아웃한다
            if let Ok(status) = backend.session_status(&backend.session()).await {
                if status.state == "signed_in" {
                    if let Some(address) = status.address {
                        self.account.write().await.address = Some(address);
                    }
                } else {
                    self.sign_out().await?;
                    self.account.write().await.warning = Some("로그인이 만료되어 로그아웃했습니다. 다시 로그인하세요.".into());
                }
            }
        }
        Ok(self.signed_in_info().await)
    }

    /// 서버에서 세션을 발급받아 QR 로그인을 시작한다. 이전에 시작한 로그인은 버린다.
    pub async fn cmd_login_start(&self) -> Result<LoginStartResult, CommandError> {
        let issued = self.evaluator.backend().session_start().await?;
        *self.pending.lock().await = Some(PendingLogin { session: Session { hash: issued.hash, token: issued.token }, started: Instant::now() });
        Ok(LoginStartResult { address: issued.address, mailto: issued.mailto, expires_in_sec: self.login_ttl.as_secs() })
    }

    /// 로그인 진행 상태를 서버에 묻는다. 화면이 몇 초마다 부른다. 메일이 도착했으면 로그인하고 세션을 저장한다.
    pub async fn cmd_login_status(&self) -> Result<LoginStatus, CommandError> {
        let status = |state: &'static str, address: Option<String>, remaining: Option<u64>, warning: Option<String>| LoginStatus { state, address, remaining_sec: remaining, warning };
        let mut pending = self.pending.lock().await;
        let Some(p) = pending.as_ref() else {
            return Ok(status("none", None, None, None));
        };
        let elapsed = p.started.elapsed();
        if elapsed >= self.login_ttl {
            *pending = None;
            return Ok(status("expired", None, None, None));
        }
        let remaining = (self.login_ttl - elapsed).as_secs();
        let session = p.session.clone();
        let st = self.evaluator.backend().session_status(&session).await?;
        match st.state.as_str() {
            "signed_in" => {
                let address = st.address;
                self.evaluator.backend().set_session(session.clone());
                let mut warning = None;
                if let Some(store) = &self.store {
                    let saved = SavedSession { hash: session.hash.clone(), token: session.token.clone(), address: address.clone() };
                    if let Err(e) = store.save(&saved) {
                        warning = Some(format!("로그인했지만 이 기기에 저장하지 못했습니다. 앱을 다시 켜면 로그아웃됩니다 ({e})"));
                    }
                }
                let mut account = self.account.write().await;
                account.address = address.clone();
                account.warning = None;
                *pending = None;
                Ok(status("signed_in", address, None, warning))
            }
            "pending" => Ok(status("pending", None, Some(remaining), None)),
            // 서버가 이 세션을 모른다 (비밀 키가 바뀌었거나 값이 틀림). 새로 시작해야 한다
            _ => {
                *pending = None;
                Ok(status("expired", None, None, None))
            }
        }
    }

    pub async fn cmd_logout(&self) -> Result<SessionInfo, CommandError> {
        self.sign_out().await?;
        Ok(self.signed_in_info().await)
    }

    async fn sign_out(&self) -> Result<(), CommandError> {
        self.evaluator.backend().set_session(Session::anonymous());
        *self.pending.lock().await = None;
        {
            let mut account = self.account.write().await;
            account.address = None;
            account.warning = None;
        }
        if let Some(store) = &self.store {
            store.clear()?;
        }
        Ok(())
    }

    /// 명령 이름과 JSON 인자로 부른다. Tauri 셸이 없는 환경(표준 입출력, 테스트)에서 쓴다.
    pub async fn dispatch(&self, cmd: &str, args: Value) -> Result<Value, CommandError> {
        match cmd {
            "cmd_submit_evaluation" => out(self.cmd_submit_evaluation(parse(args)?).await?),
            "cmd_sync_cache" => out(self.cmd_sync_cache(parse(args)?).await?),
            "cmd_search_local" => out(self.cmd_search_local(parse(args)?).await?),
            "cmd_search_remote" => out(self.cmd_search_remote(parse(args)?).await?),
            "cmd_search_merged" => out(self.cmd_search_merged(parse(args)?).await?),
            "cmd_flush_outbox" => out(self.cmd_flush_outbox().await?),
            "cmd_vote" => out(self.cmd_vote(parse(args)?).await?),
            "cmd_supersede" => out(self.cmd_supersede(parse(args)?).await?),
            "cmd_verdicts" => out(self.cmd_verdicts(parse(args)?).await?),
            "cmd_app_status" => out(self.cmd_app_status()),
            "cmd_session" => out(self.cmd_session(parse(args)?).await?),
            "cmd_login_start" => out(self.cmd_login_start().await?),
            "cmd_login_status" => out(self.cmd_login_status().await?),
            "cmd_logout" => out(self.cmd_logout().await?),
            "cmd_analytic_site" => out(self.cmd_analytic_site(parse(args)?).await?),
            "cmd_analytic_sync" => out(self.cmd_analytic_sync(parse(args)?).await?),
            "cmd_analytic_report" => out(self.cmd_analytic_report(parse(args)?).await?),
            "cmd_analytic_forget" => out(self.cmd_analytic_forget(parse(args)?).await?),
            other => Err(CommandError::new("unknown_command", format!("unknown command: {other}"))),
        }
    }

    /// 한 줄에 JSON 하나씩 읽어 명령을 실행하고, 같은 `id`를 달아 한 줄로 답한다 (형식은 `serve_lines`).
    pub async fn serve<R, W>(&self, reader: R, writer: W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        serve_lines(reader, writer, |cmd, args| async move { self.dispatch(&cmd, args).await }).await
    }
}

fn error_reply(id: Value, e: CommandError) -> Value {
    serde_json::json!({ "id": id, "ok": false, "error": { "kind": e.kind, "message": e.message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::json;

    use crate::api::{Match, SaveRequest, SaveResponse, SearchRequest, SessionStart, SessionStatus, StatusResponse};
    use crate::embedding::EMBEDDING_DIM;
    use crate::embeddinggemma::{PROMPT_DOCUMENT, PROMPT_QUERY};
    use crate::filter::LocalFilter;
    use crate::local::test_support::temp_dir;

    const SERVER_ID: &str = "0190f0a0-0000-7000-8000-0000000000aa";
    const OTHER_ID: &str = "0190f0a0-0000-7000-8000-0000000000bb";
    const GUEST: &str = "0x00000000000000000000000000000000000000ab";
    const BCC: &str = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd";

    /// 같은 본문이면 같은 방향의 단위 벡터를 만든다. 역할 접두어(질의·문서)는 떼고 본문만 본다.
    struct AxisEmbedder;
    impl Embedder for AxisEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
            let body = text
                .strip_prefix(PROMPT_QUERY)
                .or_else(|| text.strip_prefix(PROMPT_DOCUMENT))
                .unwrap_or(text);
            let axis = body.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % EMBEDDING_DIM;
            let mut v = vec![0.0; EMBEDDING_DIM];
            v[axis] = 1.0;
            Ok(v)
        }
    }

    /// 저장 요청을 기록하고, 검색에는 정해 둔 결과를 돌려주는 가짜 서버
    struct FakeServer {
        saved: Mutex<Vec<SaveRequest>>,
        offline: AtomicBool,
        status: Mutex<Option<String>>,
        remote_matches: Mutex<Vec<Match>>,
        /// 지금 요청에 쓰는 세션과, 서버가 로그인 상태를 어떻게 답할지
        session: Mutex<Session>,
        login_state: Mutex<&'static str>,
        login_error: AtomicBool,
        starts: std::sync::atomic::AtomicUsize,
        /// 받은 투표와, 판정 조회에 돌려줄 값 (없는 id는 unrated)
        votes: Mutex<Vec<crate::api::VoteRequest>>,
        /// 받은 신구 대체 보고
        supersedes: Mutex<Vec<crate::api::SupersedeRequest>>,
        verdicts: Mutex<std::collections::HashMap<String, crate::api::VerdictDto>>,
        verdict_calls: std::sync::atomic::AtomicUsize,
        /// 웹 분석: 서버에 쌓인 원시 사건 (커서대로 돌려준다)
        analytic_rows: Mutex<Vec<crate::analytic::RawEventRow>>,
    }
    impl FakeServer {
        fn new() -> Self {
            Self {
                votes: Mutex::new(Vec::new()),
                supersedes: Mutex::new(Vec::new()),
                verdicts: Mutex::new(std::collections::HashMap::new()),
                verdict_calls: std::sync::atomic::AtomicUsize::new(0),
                saved: Mutex::new(Vec::new()),
                offline: AtomicBool::new(false),
                status: Mutex::new(Some("pending".into())),
                remote_matches: Mutex::new(Vec::new()),
                session: Mutex::new(Session::anonymous()),
                login_state: Mutex::new("pending"),
                login_error: AtomicBool::new(false),
                starts: std::sync::atomic::AtomicUsize::new(0),
                analytic_rows: Mutex::new(Vec::new()),
            }
        }
    }
    const ISSUED_HASH: &str = "ab";
    fn issued(n: usize) -> Session {
        Session { hash: format!("{:0>40}", format!("{ISSUED_HASH}{n}")), token: format!("{:0>64}", format!("cd{n}")) }
    }
    #[async_trait]
    impl EvalBackend for FakeServer {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(AppError::Offline("no network".into()));
            }
            self.saved.lock().unwrap().push(req.clone());
            Ok(SaveResponse {
                id: SERVER_ID.into(),
                root: SERVER_ID.into(),
                ref_: req.ref_.clone(),
                from: req.from.clone().unwrap_or_else(|| GUEST.into()),
                status: self.status.lock().unwrap().clone(),
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(AppError::Offline("no network".into()));
            }
            Ok(SearchResponse { matches: self.remote_matches.lock().unwrap().clone() })
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            Ok(StatusResponse { id: SERVER_ID.into(), root: SERVER_ID.into(), ref_: None, status: "stored".into(), reason: None })
        }
        async fn vote(&self, req: &crate::api::VoteRequest) -> Result<crate::api::VoteResponse, AppError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(AppError::Offline("no network".into()));
            }
            if req.id == MISSING_ID {
                return Err(AppError::Server { status: 404, message: r#"{"error":"evaluation not found"}"#.into() });
            }
            self.votes.lock().unwrap().push(req.clone());
            // 로그인하지 않았으면 서버는 표를 저장만 하고 세지 않는다
            let counted = !self.session.lock().unwrap().is_anonymous();
            Ok(crate::api::VoteResponse { id: req.id.clone(), value: req.value, kind: req.kind.clone(), counted, status: Some("accepted".into()) })
        }
        async fn supersede(&self, req: &crate::api::SupersedeRequest) -> Result<crate::api::SupersedeResponse, AppError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(AppError::Offline("no network".into()));
            }
            if req.old == MISSING_ID || req.new == MISSING_ID {
                return Err(AppError::Server { status: 404, message: r#"{"error":"evaluation not found"}"#.into() });
            }
            self.supersedes.lock().unwrap().push(req.clone());
            Ok(crate::api::SupersedeResponse { old: req.old.clone(), new: req.new.clone(), status: Some("accepted".into()) })
        }
        async fn verdicts(&self, ids: &[String]) -> Result<crate::api::VerdictsResponse, AppError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(AppError::Offline("no network".into()));
            }
            self.verdict_calls.fetch_add(1, Ordering::SeqCst);
            let known = self.verdicts.lock().unwrap();
            let verdicts = ids
                .iter()
                .map(|id| known.get(id).cloned().unwrap_or_else(|| verdict_of(id, "unrated", None)))
                .collect();
            Ok(crate::api::VerdictsResponse { verdicts })
        }
        async fn session_start(&self) -> Result<SessionStart, AppError> {
            if self.login_error.load(Ordering::SeqCst) {
                return Err(AppError::Server { status: 503, message: "login is not configured".into() });
            }
            let n = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
            let s = issued(n);
            let address = format!("{}.premium.discount@oauth.email", s.hash);
            Ok(SessionStart { mailto: format!("mailto:{}", address.replace('@', "%40")), address, hash: s.hash, token: s.token })
        }
        async fn session_status(&self, session: &Session) -> Result<SessionStatus, AppError> {
            if self.offline.load(Ordering::SeqCst) {
                return Err(AppError::Offline("no network".into()));
            }
            let state = *self.login_state.lock().unwrap();
            // 서버가 발급한 세션(issued)만 안다. 그 밖의 값은 익명이다
            let known = (1..=self.starts.load(Ordering::SeqCst).max(1)).any(|n| issued(n) == *session) || *session == restored_session();
            Ok(match (known, state) {
                (true, "signed_in") => SessionStatus { state: "signed_in".into(), address: Some(USER_ADDRESS.into()) },
                (true, "pending") => SessionStatus { state: "pending".into(), address: None },
                _ => SessionStatus { state: "anonymous".into(), address: None },
            })
        }
        fn set_session(&self, session: Session) {
            *self.session.lock().unwrap() = session;
        }
        fn session(&self) -> Session {
            self.session.lock().unwrap().clone()
        }
        async fn analytic_site(&self, host: &str) -> Result<Value, AppError> {
            Ok(json!({"host": host, "verified_at": 1}))
        }
        async fn analytic_events(&self, _host: &str, cursor: &crate::analytic::Cursor, limit: u32) -> Result<crate::analytic::EventsPage, AppError> {
            let mut rows: Vec<crate::analytic::RawEventRow> = self
                .analytic_rows
                .lock()
                .unwrap()
                .iter()
                .filter(|r| (r.created_at, r.id.as_str()) > (cursor.since, cursor.after_id.as_str()))
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
            rows.truncate(limit as usize);
            let next = rows.last().map(|r| crate::analytic::Cursor { since: r.created_at, after_id: r.id.clone() });
            Ok(crate::analytic::EventsPage { more: rows.len() == limit as usize, rows, next })
        }
    }
    const USER_ADDRESS: &str = "0x00000000000000000000000000000000000000cc";
    const MISSING_ID: &str = "0190f0a0-0000-7000-8000-0000000000ee";

    fn verdict_of(id: &str, tier: &str, grade: Option<i32>) -> crate::api::VerdictDto {
        crate::api::VerdictDto {
            id: id.into(),
            tier: tier.into(),
            grade,
            rank: grade.map(|g| g as f64 * 0.4),
            value: grade.map_or(0.0, |g| g as f64 * 0.3),
            evidence: if grade.is_some() { 3.0 } else { 0.0 },
            up: 0,
            down: 0,
            level: grade.map(|_| "secondary".into()),
            reasons: vec![if grade.is_some() { "rank_high" } else { "low_evidence" }.into()],
            recipe: "pd-verdict/1".into(),
            updated_at: grade.map(|_| 1_000),
        }
    }
    fn restored_session() -> Session {
        Session { hash: "12".repeat(20), token: "34".repeat(32) }
    }

    type TestCore = AppCore<std::sync::Arc<FakeServer>, AxisEmbedder, LocalFilter>;

    async fn core() -> (TestCore, std::sync::Arc<FakeServer>) {
        let server = std::sync::Arc::new(FakeServer::new());
        let outbox = Outbox::open(temp_dir("commands")).await.unwrap();
        let evaluator = Evaluator::new(server.clone(), AxisEmbedder, LocalFilter::default());
        let analytic = crate::analytic::AnalyticStore::open(temp_dir("commands-analytic")).await.unwrap();
        (AppCore::new(evaluator, outbox).with_analytic(analytic), server)
    }

    fn raw_click(id: &str, created_at: i64, text: &str) -> crate::analytic::RawEventRow {
        let event = json!({"schema": "web-event/1", "type": "click", "href": "https://shop.example.com/order/cart", "seq": 1, "t_ms": 1000,
                           "target": {"chain": [{"tag": "button", "id": "pay"}], "pug": format!("button | {text}")}});
        crate::analytic::RawEventRow {
            id: id.into(),
            kind: "click".into(),
            session: "0xsession".into(),
            seq: 1,
            data: premium_discount_analytic::blob::encode_base64(event.as_object().unwrap()),
            created_at,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn web_analytics_sync_report_and_forget_go_through_the_core() {
        let (core, server) = core().await;
        *server.analytic_rows.lock().unwrap() = vec![raw_click("e1", 1_000, "간편결제"), raw_click("e2", 2_000, "쿠폰 적용"), raw_click("e3", 2_000, "주문하기")];
        // 주소를 넣어도 호스트만 쓴다
        let site = core.cmd_analytic_site(AnalyticSiteArgs { host: "https://Shop.Example.com/any".into() }).await.unwrap();
        assert_eq!(site["host"], "shop.example.com");
        assert_eq!(core.cmd_analytic_site(AnalyticSiteArgs { host: "not a host".into() }).await.unwrap_err().kind, "invalid_input");

        let first = core.cmd_analytic_sync(AnalyticSyncArgs { host: "shop.example.com".into(), max_pages: None }).await.unwrap();
        assert_eq!((first["ingest"]["inserted"].as_u64(), first["structured"].as_u64()), (Some(3), Some(3)), "{first}");
        // 다음 받기는 커서 뒤부터: 새 사건만
        server.analytic_rows.lock().unwrap().push(raw_click("e4", 3_000, "간편결제"));
        let second = core.cmd_analytic_sync(AnalyticSyncArgs { host: "shop.example.com".into(), max_pages: Some(1) }).await.unwrap();
        assert_eq!((second["ingest"]["received"].as_u64(), second["structured"].as_u64()), (Some(1), Some(1)), "{second}");

        let report = core
            .cmd_analytic_report(AnalyticReportArgs { host: "shop.example.com".into(), query: "장바구니 페이지 클릭".into(), lang: Some("ko".into()), top_k: Some(2) })
            .await
            .unwrap();
        assert_eq!(report.events, 4);
        assert!(report.query.page_kinds.contains(&"cart".to_string()));
        assert_eq!(report.matches.len(), 2);

        let forgot = core.cmd_analytic_forget(AnalyticForgetArgs { ids: vec!["e1".into()] }).await.unwrap();
        assert_eq!(forgot["deleted"], 1);
        // 분석 저장소가 없는 코어는 not_ready
        let outbox = Outbox::open(temp_dir("commands-no-analytic")).await.unwrap();
        let bare = AppCore::new(Evaluator::new(server.clone(), AxisEmbedder, LocalFilter::default()), outbox);
        assert_eq!(bare.cmd_analytic_forget(AnalyticForgetArgs { ids: vec![] }).await.unwrap_err().kind, "not_ready");
    }

    fn submit(text: &str) -> SubmitArgs {
        SubmitArgs {
            text: text.into(),
            cc: "example.com".into(),
            from: None,
            to: None,
            bcc: Some(BCC.into()),
            ref_: None,
            local_parent: None,
        }
    }

    #[tokio::test]
    async fn submit_is_accepted_and_bcc_reaches_the_server_and_the_local_cache() {
        let (core, server) = core().await;
        let res = core.cmd_submit_evaluation(submit("좋은 평가입니다")).await.unwrap();
        assert!(matches!(&res, SubmitResult::Accepted { id, .. } if id == SERVER_ID), "{res:?}");

        let sent = server.saved.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].bcc.as_deref(), Some(BCC), "서버로 간 요청에 bcc가 있다");
        assert_eq!(sent[0].text, "좋은 평가입니다");

        let rows = core.cmd_sync_cache(SyncArgs { since: 0 }).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["bcc"], BCC, "화면 캐시 행에도 같은 bcc가 남는다");
        assert_eq!(rows[0]["state"], "pending");
        assert_eq!(rows[0]["data"]["text"], "좋은 평가입니다");
        assert_eq!(rows[0]["id"], SERVER_ID);
    }

    #[tokio::test]
    async fn blank_optional_fields_from_the_ui_are_treated_as_absent() {
        let (core, server) = core().await;
        let mut args = submit("공백 필드");
        args.from = Some("".into());
        args.to = Some("  ".into());
        args.bcc = Some("".into());
        args.ref_ = Some("".into());
        args.local_parent = Some("".into());
        core.cmd_submit_evaluation(args).await.unwrap();
        let sent = server.saved.lock().unwrap()[0].clone();
        assert_eq!((sent.from, sent.to, sent.bcc, sent.ref_), (None, None, None, None));
    }

    #[tokio::test]
    async fn invalid_bcc_is_rejected_before_anything_is_stored() {
        let (core, server) = core().await;
        let mut args = submit("나쁜 bcc");
        args.bcc = Some("0x12".into());
        let err = core.cmd_submit_evaluation(args).await.unwrap_err();
        assert_eq!(err.kind, "invalid_input");
        assert!(err.message.contains("bcc"), "{err:?}");
        assert!(server.saved.lock().unwrap().is_empty());
        assert!(core.cmd_sync_cache(SyncArgs { since: 0 }).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn profane_input_leaves_no_trace_locally() {
        let (core, server) = core().await;
        let res = core.cmd_submit_evaluation(submit("시.발 이게 뭐야")).await.unwrap();
        assert_eq!(res, SubmitResult::Blocked);
        assert!(server.saved.lock().unwrap().is_empty(), "서버로 가지 않았다");
        assert!(core.cmd_sync_cache(SyncArgs { since: 0 }).await.unwrap().is_empty(), "로컬 저장소에도 없다");
        let found = core.cmd_search_local(SearchLocalArgs { query: "시.발 이게 뭐야".into(), top_k: 5, cc: None }).await.unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn offline_submit_is_queued_locally_and_flush_sends_it_later() {
        let (core, server) = core().await;
        server.offline.store(true, Ordering::SeqCst);
        let res = core.cmd_submit_evaluation(submit("오프라인에서 쓴 글")).await.unwrap();
        assert!(matches!(res, SubmitResult::Queued { .. }), "{res:?}");
        let rows = core.cmd_sync_cache(SyncArgs { since: 0 }).await.unwrap();
        assert_eq!(rows[0]["state"], "queued");
        assert_eq!(rows[0]["bcc"], BCC, "보내기 전에도 bcc가 로컬에 있다");

        let still = core.cmd_flush_outbox().await.unwrap();
        assert_eq!((still.sent, still.remaining), (0, 1));
        assert!(still.stopped.is_some());

        server.offline.store(false, Ordering::SeqCst);
        *server.status.lock().unwrap() = Some("stored".into());
        let done = core.cmd_flush_outbox().await.unwrap();
        assert_eq!((done.sent, done.remaining), (1, 0));
        assert_eq!(server.saved.lock().unwrap()[0].bcc.as_deref(), Some(BCC), "다시 보낼 때도 같은 bcc");
    }

    #[tokio::test]
    async fn flush_resolves_pending_rows_to_stored() {
        let (core, _server) = core().await;
        core.cmd_submit_evaluation(submit("접수된 글")).await.unwrap();
        let summary = core.cmd_flush_outbox().await.unwrap();
        assert_eq!(summary.stored, 1);
        assert_eq!(summary.still_pending, 0);
        let rows = core.cmd_sync_cache(SyncArgs { since: 0 }).await.unwrap();
        assert_eq!(rows[0]["state"], "stored");
    }

    #[tokio::test]
    async fn sync_cache_returns_only_rows_changed_after_since() {
        let (core, _server) = core().await;
        core.cmd_submit_evaluation(submit("첫 글")).await.unwrap();
        let all = core.cmd_sync_cache(SyncArgs { since: 0 }).await.unwrap();
        let cursor = all.iter().map(|r| r["updated_at"].as_i64().unwrap()).max().unwrap();
        assert!(core.cmd_sync_cache(SyncArgs { since: cursor }).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_search_finds_my_text_and_validates_top_k() {
        let (core, _server) = core().await;
        core.cmd_submit_evaluation(submit("내가 쓴 글")).await.unwrap();
        let found = core.cmd_search_local(SearchLocalArgs { query: "내가 쓴 글".into(), top_k: 3, cc: Some("Example.COM".into()) }).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "내가 쓴 글");
        assert_eq!(found[0].state, "pending");
        assert!(found[0].score > 0.99);

        let err = core.cmd_search_local(SearchLocalArgs { query: "x".into(), top_k: 0, cc: None }).await.unwrap_err();
        assert_eq!(err.kind, "invalid_input");
    }

    #[tokio::test]
    async fn supersede_reports_go_out_and_mark_the_old_verdict_discount_at_once() {
        let (core, server) = core().await;
        const NEWER: &str = "0190f0a0-0000-7000-8000-0000000000cc";
        let args = |old: &str, new: &str| SupersedeArgs { old: old.into(), new: new.into() };
        // 서버 판정은 premium 이었다
        server.verdicts.lock().unwrap().insert(OTHER_ID.into(), verdict_of(OTHER_ID, "premium", Some(2)));
        let before = core.cmd_verdicts(VerdictsArgs { ids: vec![OTHER_ID.into()], refresh: false }).await.unwrap();
        assert_eq!(before.verdicts[0].verdict.tier, "premium");

        // 오프라인: 오류이고 아무것도 바뀌지 않는다 (부른 쪽이 다시 보낸다)
        server.offline.store(true, Ordering::SeqCst);
        assert_eq!(core.cmd_supersede(args(OTHER_ID, NEWER)).await.unwrap_err().kind, "offline");
        server.offline.store(false, Ordering::SeqCst);

        let r = core.cmd_supersede(args(OTHER_ID, NEWER)).await.unwrap();
        assert_eq!((r.old.as_str(), r.new.as_str()), (OTHER_ID, NEWER));
        assert!(r.request_id.starts_with("sup-") && (16..=64).contains(&r.request_id.len()), "{}", r.request_id);
        // 같은 보고는 같은 멱등 키
        let again = core.cmd_supersede(args(OTHER_ID, NEWER)).await.unwrap();
        assert_eq!(again.request_id, r.request_id);
        let sent = server.supersedes.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!((sent[0].old.as_str(), sent[0].new.as_str()), (OTHER_ID, NEWER));
        assert_eq!(sent[0].request_id, sent[1].request_id);

        // 캐시의 판정은 곧바로 discount −2 (서버에 다시 묻지 않아도)
        let calls = server.verdict_calls.load(Ordering::SeqCst);
        let after = core.cmd_verdicts(VerdictsArgs { ids: vec![OTHER_ID.into()], refresh: false }).await.unwrap();
        assert_eq!(server.verdict_calls.load(Ordering::SeqCst), calls, "served from the cache");
        let v = &after.verdicts[0].verdict;
        assert_eq!((v.tier.as_str(), v.grade, v.reasons.clone()), ("discount", Some(-2), vec!["superseded".to_string()]));
        assert_eq!(v.evidence, 3.0, "the server's evidence stays");

        // 캐시에 없던 평가도 표시된다. 잘못된 입력 · 서버에 없는 평가는 오류
        let lone = core.cmd_supersede(args(SERVER_ID, NEWER)).await.unwrap();
        assert_eq!(lone.old, SERVER_ID);
        assert_eq!(core.cmd_supersede(args(OTHER_ID, OTHER_ID)).await.unwrap_err().kind, "invalid_input");
        assert_eq!(core.cmd_supersede(args("nope", NEWER)).await.unwrap_err().kind, "invalid_input");
        assert_eq!(core.cmd_supersede(args(MISSING_ID, NEWER)).await.unwrap_err().kind, "rejected");
    }

    #[tokio::test]
    async fn votes_go_out_at_once_or_wait_offline_and_show_up_in_search() {
        let (core, server) = core().await;
        let vote = |id: &str, value: i32| VoteArgs { id: id.into(), value, kind: None };

        // 로그인 전: 서버가 받지만 세지 않는다
        let r = core.cmd_vote(vote(OTHER_ID, 1)).await.unwrap();
        assert!(matches!(r, VoteResult::Sent { counted: false, .. }), "{r:?}");
        assert_eq!(server.votes.lock().unwrap()[0].kind, "vote");

        // 오프라인: 보관함에 남고, 마음을 바꾸면 마지막 값만 나간다
        server.offline.store(true, Ordering::SeqCst);
        assert!(matches!(core.cmd_vote(vote(OTHER_ID, -1)).await.unwrap(), VoteResult::Queued { .. }));
        assert!(matches!(core.cmd_vote(vote(OTHER_ID, 0)).await.unwrap(), VoteResult::Queued { .. }));
        assert!(matches!(core.cmd_vote(vote(OTHER_ID, 1)).await.unwrap(), VoteResult::Queued { .. }));
        let summary = core.cmd_flush_outbox().await.unwrap();
        assert_eq!((summary.votes_sent, summary.votes_remaining), (0, 1));
        assert!(summary.votes_stopped.unwrap().starts_with("offline"));

        server.offline.store(false, Ordering::SeqCst);
        let summary = core.cmd_flush_outbox().await.unwrap();
        assert_eq!((summary.votes_sent, summary.votes_remaining), (1, 0));
        let sent = server.votes.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "오프라인에서 바꾼 세 번 중 마지막 하나만");
        assert_eq!(sent[1].value, 1);
        assert_ne!(sent[0].request_id, sent[1].request_id);

        // 검색 결과에 내 표가 붙는다
        *server.remote_matches.lock().unwrap() = vec![remote(OTHER_ID, 0.4)];
        let found = core
            .cmd_search_remote(SearchArgs { query: "아무거나".into(), top_k: 5, cc: None, root: None, ref_: None, to: None, tier: None })
            .await
            .unwrap();
        assert_eq!(found.matches[0].my_vote, Some(1));

        // 서버에 없는 평가는 바로 거부. 값과 id도 검사한다
        let err = core.cmd_vote(vote(MISSING_ID, 1)).await.unwrap_err();
        assert_eq!(err.kind, "rejected");
        assert_eq!(core.cmd_vote(vote("not-a-uuid", 1)).await.unwrap_err().kind, "invalid_input");
        assert_eq!(core.cmd_vote(vote(OTHER_ID, 3)).await.unwrap_err().kind, "invalid_input");
        let bad_kind = VoteArgs { id: OTHER_ID.into(), value: 1, kind: Some("like".into()) };
        assert_eq!(core.cmd_vote(bad_kind).await.unwrap_err().kind, "invalid_input");
    }

    #[tokio::test]
    async fn verdicts_come_from_the_cache_until_they_are_old_or_refreshed() {
        let (core, server) = core().await;
        server.verdicts.lock().unwrap().insert(OTHER_ID.into(), verdict_of(OTHER_ID, "premium", Some(2)));
        let ask = |refresh: bool| VerdictsArgs { ids: vec![OTHER_ID.into(), SERVER_ID.into(), OTHER_ID.to_uppercase()], refresh };

        let r = core.cmd_verdicts(ask(false)).await.unwrap();
        assert_eq!(r.verdicts.len(), 2, "같은 id(대소문자만 다름)는 한 번");
        assert_eq!((r.verdicts[0].verdict.tier.as_str(), r.verdicts[0].verdict.grade), ("premium", Some(2)));
        assert_eq!(r.verdicts[1].verdict.tier, "unrated");
        assert!(!r.stale && r.missing.is_empty());
        assert_eq!(server.verdict_calls.load(Ordering::SeqCst), 1);

        // 캐시가 새것이면 묻지 않는다. refresh면 묻는다
        core.cmd_verdicts(ask(false)).await.unwrap();
        assert_eq!(server.verdict_calls.load(Ordering::SeqCst), 1);
        core.cmd_verdicts(ask(true)).await.unwrap();
        assert_eq!(server.verdict_calls.load(Ordering::SeqCst), 2);

        // 오프라인: 캐시 값을 그대로 주고, 받은 적 없는 id는 missing
        server.offline.store(true, Ordering::SeqCst);
        let r = core
            .cmd_verdicts(VerdictsArgs { ids: vec![OTHER_ID.into(), MISSING_ID.into()], refresh: true })
            .await
            .unwrap();
        assert!(r.stale);
        assert!(r.remote_error.unwrap().starts_with("offline"));
        assert_eq!(r.verdicts[0].verdict.tier, "premium");
        assert_eq!(r.missing, vec![MISSING_ID.to_string()]);

        let err = core.cmd_verdicts(VerdictsArgs { ids: vec!["nope".into()], refresh: false }).await.unwrap_err();
        assert_eq!(err.kind, "invalid_input");
    }

    #[tokio::test]
    async fn merged_search_carries_the_server_verdict_and_a_tier_filter_skips_local_rows() {
        let (core, server) = core().await;
        core.cmd_submit_evaluation(submit("등급 검색")).await.unwrap();
        let mut m = remote(OTHER_ID, 0.4);
        m.verdict = Some(crate::api::MatchVerdict { tier: "premium".into(), grade: Some(1), rank: Some(0.6), evidence: 4.0 });
        *server.remote_matches.lock().unwrap() = vec![m];

        let args = |tier: Option<&str>| SearchArgs { query: "등급 검색".into(), top_k: 5, cc: None, root: None, ref_: None, to: None, tier: tier.map(Into::into) };
        let all = core.cmd_search_merged(args(None)).await.unwrap();
        assert_eq!(all.matches.len(), 2);
        let other = all.matches.iter().find(|m| m.id.as_deref() == Some(OTHER_ID)).unwrap();
        assert_eq!(other.verdict.as_ref().unwrap().tier, "premium");

        let premium = core.cmd_search_merged(args(Some("premium"))).await.unwrap();
        assert_eq!(premium.matches.len(), 1, "등급은 서버에만 있으므로 로컬 결과를 섞지 않는다");
        assert_eq!(premium.matches[0].source, crate::pipeline::MatchSource::Remote);
        let err = core.cmd_search_merged(args(Some("gold"))).await.unwrap_err();
        assert_eq!(err.kind, "invalid_input");
        // 빈 문자열은 필터 없음
        assert_eq!(core.cmd_search_merged(args(Some(""))).await.unwrap().matches.len(), 2);
    }

    fn remote(id: &str, score: f32) -> Match {
        Match { id: id.into(), score, cc: Some("example.com".into()), root: Some(id.into()), ref_: None, to: None, verdict: None, my_vote: None }
    }

    #[tokio::test]
    async fn merged_search_shows_a_just_saved_post_before_the_server_indexes_it() {
        let (core, server) = core().await;
        core.cmd_submit_evaluation(submit("방금 쓴 글")).await.unwrap();
        // 서버 검색에는 아직 내 글이 없고 남의 글만 있다 (cron이 처리하기 전)
        *server.remote_matches.lock().unwrap() = vec![remote(OTHER_ID, 0.4)];

        let args = SearchArgs { query: "방금 쓴 글".into(), top_k: 10, cc: None, root: None, ref_: None, to: None, tier: None };
        let merged = core.cmd_search_merged(args).await.unwrap();
        assert_eq!(merged.remote_error, None);
        assert_eq!(merged.matches.len(), 2);
        assert_eq!(merged.matches[0].text.as_deref(), Some("방금 쓴 글"), "내 글이 먼저 (점수가 더 높다)");
        assert_eq!(merged.matches[0].source, crate::pipeline::MatchSource::Local);
        assert_eq!(merged.matches[1].id.as_deref(), Some(OTHER_ID));
        assert_eq!(merged.matches[1].text, None, "남의 글에는 원문이 없다");
    }

    #[tokio::test]
    async fn merged_search_dedupes_the_same_post_found_on_both_sides() {
        let (core, server) = core().await;
        core.cmd_submit_evaluation(submit("양쪽에 있는 글")).await.unwrap();
        *server.remote_matches.lock().unwrap() = vec![remote(SERVER_ID, 0.97), remote(OTHER_ID, 0.2)];
        let args = SearchArgs { query: "양쪽에 있는 글".into(), top_k: 10, cc: None, root: None, ref_: None, to: None, tier: None };
        let merged = core.cmd_search_merged(args).await.unwrap();
        assert_eq!(merged.matches.len(), 2, "같은 서버 id는 하나로 합친다");
        let mine = &merged.matches[0];
        assert_eq!(mine.id.as_deref(), Some(SERVER_ID));
        assert_eq!(mine.source, crate::pipeline::MatchSource::Both);
        assert_eq!(mine.text.as_deref(), Some("양쪽에 있는 글"));
        assert_eq!(mine.root.as_deref(), Some(SERVER_ID), "스레드 정보는 서버에서");
    }

    #[tokio::test]
    async fn merged_search_falls_back_to_local_results_when_the_server_is_unreachable() {
        let (core, server) = core().await;
        core.cmd_submit_evaluation(submit("오프라인 검색")).await.unwrap();
        server.offline.store(true, Ordering::SeqCst);
        let args = SearchArgs { query: "오프라인 검색".into(), top_k: 5, cc: None, root: None, ref_: None, to: None, tier: None };
        let merged = core.cmd_search_merged(args).await.unwrap();
        assert_eq!(merged.matches.len(), 1);
        assert!(merged.remote_error.as_deref().unwrap_or("").contains("offline"), "{merged:?}");
    }

    #[tokio::test]
    async fn merged_search_skips_local_results_when_a_server_only_filter_is_given() {
        let (core, server) = core().await;
        core.cmd_submit_evaluation(submit("필터 검색")).await.unwrap();
        *server.remote_matches.lock().unwrap() = vec![remote(OTHER_ID, 0.3)];
        let args = SearchArgs { query: "필터 검색".into(), top_k: 5, cc: None, root: Some(OTHER_ID.into()), ref_: None, to: None, tier: None };
        let merged = core.cmd_search_merged(args).await.unwrap();
        assert_eq!(merged.matches.len(), 1, "root 필터를 어기는 로컬 결과는 섞지 않는다");
        assert_eq!(merged.matches[0].id.as_deref(), Some(OTHER_ID));
    }

    #[tokio::test]
    async fn remote_search_passes_the_server_response_through() {
        let (core, server) = core().await;
        *server.remote_matches.lock().unwrap() = vec![remote(OTHER_ID, 0.8)];
        let res = core
            .cmd_search_remote(SearchArgs { query: "아무거나".into(), top_k: 5, cc: None, root: None, ref_: None, to: None, tier: None })
            .await
            .unwrap();
        assert_eq!(res.matches, vec![remote(OTHER_ID, 0.8)]);
    }

    #[tokio::test]
    async fn dispatch_routes_commands_and_reports_errors_by_kind() {
        let (core, _server) = core().await;
        let ok = core.dispatch("cmd_submit_evaluation", json!({"text": "디스패치", "cc": "example.com"})).await.unwrap();
        assert_eq!(ok["status"], "accepted");
        let rows = core.dispatch("cmd_sync_cache", Value::Null).await.unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1, "인자를 생략하면 since=0");

        let bad = core.dispatch("cmd_submit_evaluation", json!({"cc": "example.com"})).await.unwrap_err();
        assert_eq!(bad.kind, "invalid_input", "text가 없다");
        let unknown = core.dispatch("cmd_nope", Value::Null).await.unwrap_err();
        assert_eq!(unknown.kind, "unknown_command");
        let invalid_cc = core.dispatch("cmd_submit_evaluation", json!({"text": "x", "cc": "localhost"})).await.unwrap_err();
        assert_eq!(invalid_cc.kind, "invalid_input");
    }

    /// 화면(web/desktop/commands.ts)과 공유하는 명령 계약 파일의 모든 명령·인자를 그대로 받아들인다
    #[tokio::test]
    async fn every_command_in_the_ui_contract_accepts_the_agreed_arguments() {
        let contract: Value = serde_json::from_str(include_str!("../../../web/desktop/fixtures/command_contract.json")).unwrap();
        let (core, _server) = core().await;
        // 모델 명령은 코어가 아니라 셸이 받는다 (모델이 없어도 답해야 하므로). 받는 주소는 아무것도 없는 곳
        let fetcher = crate::model_fetch::ModelFetcher::new(temp_dir("contract-model"), "http://127.0.0.1:9").unwrap();
        let filter = crate::model_fetch::ModelFetcher::with_manifest(
            temp_dir("contract-filter"),
            "http://127.0.0.1:9",
            crate::model_fetch::Manifest::laya_multilingual(),
        )
        .unwrap();
        let draft = crate::model_fetch::ModelFetcher::with_manifest(
            temp_dir("contract-draft"),
            "http://127.0.0.1:9",
            crate::model_fetch::Manifest::qwen35_08b(),
        )
        .unwrap();
        let engine = crate::items_draft::DraftEngine::new(temp_dir("contract-draft"), None);
        let fetcher = Fetchers { model: &fetcher, filter: Some(&filter), draft: Some(&draft) };
        let mut seen = Vec::new();
        for (cmd, spec) in contract.as_object().unwrap() {
            if cmd.starts_with('_') {
                continue;
            }
            if cmd == "cmd_item_draft" {
                // 셸의 초안 엔진이 받는다. 모델이 없으면 not_ready (인자는 받아들였다는 뜻)
                let args: crate::items_draft::DraftArgs = serde_json::from_value(spec["args"].clone()).expect("초안 인자");
                assert_eq!(engine.run(args).await.unwrap_err().kind, "not_ready");
                seen.push(cmd.clone());
                continue;
            }
            let res = match model_command(&fetcher, cmd, spec["args"].clone()) {
                Some(res) => res,
                None => core.dispatch(cmd, spec["args"].clone()).await,
            };
            assert!(res.is_ok(), "{cmd}: {res:?}");
            seen.push(cmd.clone());
        }
        assert_eq!(seen.len(), 22, "계약에 있는 명령 수: {seen:?}");
        assert!(model_command(&fetcher, "cmd_submit_evaluation", Value::Null).is_none(), "모델 명령이 아니면 코어로 간다");
        let bad = model_command(&fetcher, "cmd_model_download", json!({"variant": "huge"})).unwrap().unwrap_err();
        assert_eq!(bad.kind, "invalid_input");
        // 필터 모델은 필터 관리자로 간다 (받는 주소가 없어 곧 실패하지만 시작은 된다)
        let started = model_command(&fetcher, "cmd_model_download", json!({"variant": "filter"})).unwrap().unwrap();
        assert_eq!(started["total_bytes"], 643_835_514u64 + 34_363_188);
        let status = model_command(&fetcher, "cmd_model_status", Value::Null).unwrap().unwrap();
        assert_eq!(status["filter"]["installed"], false);
        assert_eq!(status["filter"]["bytes"], 643_835_514u64 + 34_363_188);
        let status = model_command(&fetcher, "cmd_model_status", Value::Null).unwrap().unwrap();
        assert_eq!(status["draft"]["bytes"], 12_807_982u64 + 59_848 + 1_504_791_232, "초안 모델은 텍스트 부분만 받는다");
        let no_filter = Fetchers { model: fetcher.model, filter: None, draft: None };
        assert_eq!(model_command(&no_filter, "cmd_model_download", json!({"variant": "filter"})).unwrap().unwrap_err().kind, "invalid_input");
    }

    #[tokio::test]
    async fn serve_answers_one_json_line_per_request_and_keeps_going_after_bad_lines() {
        let (core, _server) = core().await;
        let input = [
            r#"{"id": 1, "cmd": "cmd_submit_evaluation", "args": {"text": "표준 입출력", "cc": "example.com"}}"#,
            "not json",
            "",
            r#"{"id": "x", "args": {}}"#,
            r#"{"id": 2, "cmd": "cmd_search_local", "args": {"query": "표준 입출력", "top_k": 2}}"#,
            r#"{"id": 3, "cmd": "cmd_nope"}"#,
        ]
        .join("\n");
        let mut out: Vec<u8> = Vec::new();
        core.serve(tokio::io::BufReader::new(input.as_bytes()), &mut out).await.unwrap();

        let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(replies.len(), 5, "빈 줄에는 답하지 않는다");
        assert_eq!((replies[0]["id"].clone(), replies[0]["ok"].clone()), (json!(1), json!(true)));
        assert_eq!(replies[0]["result"]["status"], "accepted");
        assert_eq!((replies[1]["id"].clone(), replies[1]["ok"].clone()), (Value::Null, json!(false)));
        assert_eq!(replies[2]["id"], "x");
        assert_eq!(replies[2]["error"]["kind"], "invalid_input");
        assert_eq!(replies[3]["result"][0]["text"], "표준 입출력");
        assert_eq!(replies[4]["error"]["kind"], "unknown_command");
    }

    // ---- 로그인 ----

    async fn core_with_store(dir: &std::path::Path) -> (TestCore, std::sync::Arc<FakeServer>) {
        let (core, server) = core().await;
        (core.with_session_store(SessionStore::new(dir.join("session.json"))), server)
    }

    fn session_file(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("session.json")
    }

    #[tokio::test]
    async fn starting_a_login_returns_the_mail_address_and_keeps_the_session_secret() {
        let (core, server) = core().await;
        let started = core.cmd_login_start().await.unwrap();
        let expect = issued(1);
        assert_eq!(started.address, format!("{}.premium.discount@oauth.email", expect.hash));
        assert!(started.mailto.starts_with("mailto:") && started.mailto.contains("%40"), "{}", started.mailto);
        assert_eq!(started.expires_in_sec, 15 * 60);
        // token은 화면으로 가지 않는다 (직렬화한 결과에도 없다)
        assert!(!serde_json::to_string(&started).unwrap().contains(&expect.token));
        assert!(server.session().is_anonymous(), "메일이 도착하기 전에는 로그인하지 않는다");
        assert_eq!(core.cmd_session(SessionArgs::default()).await.unwrap().state, "anonymous");
    }

    #[tokio::test]
    async fn the_login_completes_when_the_mail_arrives_and_survives_a_restart() {
        let dir = temp_dir("login-flow");
        let (core, server) = core_with_store(&dir).await;
        assert_eq!(core.cmd_login_status().await.unwrap().state, "none", "시작하지 않았다");
        core.cmd_login_start().await.unwrap();

        let waiting = core.cmd_login_status().await.unwrap();
        assert_eq!((waiting.state, waiting.address), ("pending", None));
        assert!(waiting.remaining_sec.unwrap() > 0);
        assert!(server.session().is_anonymous());
        assert!(!session_file(&dir).exists(), "대기 중에는 저장하지 않는다");

        *server.login_state.lock().unwrap() = "signed_in";
        let done = core.cmd_login_status().await.unwrap();
        assert_eq!((done.state, done.address.as_deref()), ("signed_in", Some(USER_ADDRESS)));
        assert_eq!(done.warning, None);
        assert_eq!(server.session(), issued(1), "이후 요청은 발급받은 세션으로 나간다");
        let info = core.cmd_session(SessionArgs::default()).await.unwrap();
        assert_eq!((info.state, info.address.as_deref()), ("signed_in", Some(USER_ADDRESS)));

        // 파일에는 세션과 주소가 있다
        let saved = SessionStore::new(session_file(&dir)).load().unwrap().unwrap();
        assert_eq!((saved.hash, saved.token), (issued(1).hash, issued(1).token));
        assert_eq!(saved.address.as_deref(), Some(USER_ADDRESS));
        // 끝난 로그인은 다시 완료되지 않는다
        assert_eq!(core.cmd_login_status().await.unwrap().state, "none");

        // 앱을 다시 켜면 (같은 파일) 로그인이 복원된다
        let (again, server2) = core_with_store(&dir).await;
        assert_eq!(server2.session(), issued(1));
        let restored = again.cmd_session(SessionArgs::default()).await.unwrap();
        assert_eq!((restored.state, restored.address.as_deref()), ("signed_in", Some(USER_ADDRESS)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn logout_forgets_the_session_everywhere() {
        let dir = temp_dir("login-logout");
        let (core, server) = core_with_store(&dir).await;
        core.cmd_login_start().await.unwrap();
        *server.login_state.lock().unwrap() = "signed_in";
        core.cmd_login_status().await.unwrap();
        assert!(session_file(&dir).exists());

        let info = core.cmd_logout().await.unwrap();
        assert_eq!((info.state, info.address), ("anonymous", None));
        assert!(server.session().is_anonymous());
        assert!(!session_file(&dir).exists());
        core.cmd_logout().await.unwrap(); // 두 번 해도 오류가 아니다

        // 로그인 대기 중에 로그아웃하면 대기도 버린다
        core.cmd_login_start().await.unwrap();
        core.cmd_logout().await.unwrap();
        assert_eq!(core.cmd_login_status().await.unwrap().state, "none");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_login_that_takes_too_long_expires() {
        let (core, server) = core().await;
        let core = core.with_login_ttl(Duration::ZERO);
        core.cmd_login_start().await.unwrap();
        *server.login_state.lock().unwrap() = "signed_in";
        assert_eq!(core.cmd_login_status().await.unwrap().state, "expired", "시간이 지난 뒤 도착한 메일은 받지 않는다");
        assert!(server.session().is_anonymous());
        assert_eq!(core.cmd_login_status().await.unwrap().state, "none");
    }

    #[tokio::test]
    async fn starting_again_replaces_the_pending_login() {
        let (core, server) = core().await;
        let first = core.cmd_login_start().await.unwrap();
        let second = core.cmd_login_start().await.unwrap();
        assert_ne!(first.address, second.address);
        *server.login_state.lock().unwrap() = "signed_in";
        core.cmd_login_status().await.unwrap();
        assert_eq!(server.session(), issued(2), "나중에 시작한 로그인이 이긴다");
    }

    #[tokio::test]
    async fn login_errors_are_reported_and_the_login_stays_pending() {
        let (core, server) = core().await;
        server.login_error.store(true, Ordering::SeqCst);
        let err = core.cmd_login_start().await.unwrap_err();
        assert_eq!(err.kind, "server");
        assert!(err.message.contains("login is not configured"), "{err:?}");

        server.login_error.store(false, Ordering::SeqCst);
        core.cmd_login_start().await.unwrap();
        server.offline.store(true, Ordering::SeqCst);
        assert_eq!(core.cmd_login_status().await.unwrap_err().kind, "offline");
        server.offline.store(false, Ordering::SeqCst);
        assert_eq!(core.cmd_login_status().await.unwrap().state, "pending", "연결이 돌아오면 이어서 기다린다");
    }

    #[tokio::test]
    async fn a_session_the_server_does_not_know_expires_the_login() {
        let (core, server) = core().await;
        core.cmd_login_start().await.unwrap();
        // 서버가 이 세션을 모른다고 답한다 (비밀 키가 바뀐 경우 등): 발급 횟수를 바꿔 모르는 세션으로 만든다
        server.starts.store(0, Ordering::SeqCst);
        *server.login_state.lock().unwrap() = "signed_in";
        server.set_session(Session::anonymous());
        let p = core.pending.lock().await.take().unwrap();
        *core.pending.lock().await = Some(PendingLogin { session: Session { hash: "99".repeat(20), token: "88".repeat(32) }, started: p.started });
        assert_eq!(core.cmd_login_status().await.unwrap().state, "expired");
        assert_eq!(core.cmd_login_status().await.unwrap().state, "none");
    }

    #[tokio::test]
    async fn a_corrupt_session_file_starts_logged_out_with_a_warning() {
        let dir = temp_dir("login-corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(session_file(&dir), "garbage").unwrap();
        let (core, server) = core_with_store(&dir).await;
        assert!(server.session().is_anonymous());
        let info = core.cmd_session(SessionArgs::default()).await.unwrap();
        assert_eq!(info.state, "anonymous");
        assert!(info.warning.unwrap().contains("읽지 못해"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verifying_a_restored_login_keeps_it_offline_and_drops_it_when_the_server_disagrees() {
        let dir = temp_dir("login-verify");
        std::fs::create_dir_all(&dir).unwrap();
        let saved = SavedSession { hash: restored_session().hash, token: restored_session().token, address: Some(USER_ADDRESS.into()) };
        SessionStore::new(session_file(&dir)).save(&saved).unwrap();

        // 연결이 없으면 확인하지 못하므로 로그인을 그대로 둔다
        let (core, server) = core_with_store(&dir).await;
        server.offline.store(true, Ordering::SeqCst);
        assert_eq!(core.cmd_session(SessionArgs { verify: true }).await.unwrap().state, "signed_in");
        // 서버가 유효하다고 하면 그대로
        server.offline.store(false, Ordering::SeqCst);
        *server.login_state.lock().unwrap() = "signed_in";
        assert_eq!(core.cmd_session(SessionArgs { verify: true }).await.unwrap().state, "signed_in");
        // 서버가 더 이상 모른다고 하면 로그아웃하고 저장된 세션도 지운다
        *server.login_state.lock().unwrap() = "pending";
        let info = core.cmd_session(SessionArgs { verify: true }).await.unwrap();
        assert_eq!(info.state, "anonymous");
        assert!(info.warning.unwrap().contains("만료"));
        assert!(!session_file(&dir).exists());
        assert!(server.session().is_anonymous());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_login_commands_are_reachable_through_dispatch() {
        let (core, _server) = core().await;
        let started = core.dispatch("cmd_login_start", Value::Null).await.unwrap();
        assert!(started["mailto"].as_str().unwrap().starts_with("mailto:"));
        assert_eq!(core.dispatch("cmd_login_status", Value::Null).await.unwrap()["state"], "pending");
        assert_eq!(core.dispatch("cmd_session", json!({"verify": false})).await.unwrap()["state"], "anonymous");
        assert_eq!(core.dispatch("cmd_logout", Value::Null).await.unwrap()["state"], "anonymous");
    }

    #[tokio::test]
    async fn app_status_reports_ready_with_the_paths_and_the_not_ready_shape_for_shells() {
        let info = AppInfo { model_dir: "/m".into(), data_dir: "/d".into(), server_url: "https://s.example".into() };
        let (core, _server) = core().await;
        let status = core.with_info(info.clone()).cmd_app_status();
        assert_eq!((status.ready, status.error.clone()), (true, None));
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!((json["ready"].clone(), json["model_dir"].clone(), json["server_url"].clone()), (json!(true), json!("/m"), json!("https://s.example")));
        assert!(json["version"].as_str().unwrap().contains('.'));
        assert_eq!(json["platform"], platform(), "플랫폼 종류가 함께 간다");
        assert!(["android", "ios", "desktop"].contains(&platform()));

        let failed = serde_json::to_value(AppStatus::not_ready("모델 폴더가 없습니다", info)).unwrap();
        assert_eq!((failed["ready"].clone(), failed["error"].clone()), (json!(false), json!("모델 폴더가 없습니다")));
        assert_eq!(failed["data_dir"], "/d", "경로는 같은 자리에 나란히 있다 (flatten)");
        let err = CommandError::new("not_ready", "x");
        assert_eq!(err.kind, "not_ready");
    }
}
