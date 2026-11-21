//! 오프라인 저장 큐.
//! 인터넷이 없을 때 저장할 벡터를 쌓아 두었다가, 연결되면 들어온 순서대로 보낸다.
//!
//! - 답글은 부모의 서버 id가 필요하다. 부모가 아직 서버에 없으면 로컬 id(ParentRef::Local)로
//!   기억해 두고, 부모를 보낸 뒤 서버 id로 바꿔서 보낸다.
//! - 실패 처리 (다섯 갈래, [`SyncFailure`]):
//!   - 연결 실패(AppError::Offline): 큐를 멈추고 항목을 그대로 둔다. 시도 횟수를 세지 않는다.
//!   - 인증 보류(401 · 403): 큐를 멈추고 항목을 그대로 둔다. 시도 횟수를 세지 않는다. 다시 로그인할 때까지
//!     보내지 않는다(`auth_required`). 예전에는 일시 오류로 세어 10초 주기 5번(50초)이면 항목을 버렸다.
//!   - 속도 제한(429): 큐를 멈추고 항목을 그대로 둔다. 시도 횟수를 세지 않는다. `retry_after_ms` 뒤에 다시 보낸다.
//!   - 서버 일시 오류(5xx, 408, 대상이 밝혀지지 않은 404, 기타 HTTP 오류): MAX_SERVER_ATTEMPTS번까지 재시도한 뒤 버린다.
//!   - 그 밖의 4xx(400 · 409 · 422, 본문이 "evaluation not found"인 404): 영구 거부로 보고 바로 버린다.
//!   - 거부된 부모의 답글도 보내지 않고 함께 버린다.
//! - 상한: 못 보낸 항목은 [`MAX_PENDING_ITEMS`], 서버가 접수만 한 항목(결과 대기)은 [`MAX_TRACKING_ITEMS`] 까지다.
//!   결과를 [`RETAIN_MS`](7일) 넘게 못 받았거나 상한을 넘긴 오래된 항목은 추적을 끝내고 missing 으로 알린다
//!   (로컬 행의 원문과 벡터는 지우지 않는다: 내 메모로 남는다).
//! - 디스크 저장은 호출하는 쪽의 몫이다. to_json()으로 문자열을 얻어 저장하고, from_json()으로 되살린다.
//!   보낸 항목의 id 매핑도 함께 저장되므로, 재시작 뒤에도 답글이 부모 id를 찾을 수 있다.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::api::{EvalBackend, SaveRequest, SaveResponse};
use crate::error::AppError;

// 서버가 접수(202 pending)만 한 항목은 최종 결과를 알 수 없다. 큐가 request_id를 기억해 두고
// sync_status로 stored | rejected | failed가 될 때까지 서버에 물어본다.
// 큐가 파일 경로를 알고 있으면(open) enqueue와 flush, sync_status 뒤에 스스로 디스크에 저장한다.

/// 서버 일시 오류를 몇 번까지 재시도할지
pub const MAX_SERVER_ATTEMPTS: u32 = 5;

/// 대기열 상한 (항목 수). 임시값이다. 오프라인이 길어져도 메모리와 저장 파일이 무한히 커지지 않게 한다.
pub const MAX_PENDING_ITEMS: usize = 1000;

/// 서버가 접수(202 pending)만 하고 결과를 아직 모르는 항목의 상한. 넘치면 가장 오래 기다린 것부터 missing 으로 끝낸다
pub const MAX_TRACKING_ITEMS: usize = 2_000;

/// 접수된 항목의 결과를 기다리는 최대 시간 (7일). 서버는 거부 기록을 7일 뒤 지우므로 그 뒤로는 물어도 알 수 없다
pub const RETAIN_MS: i64 = 7 * 86_400_000;

/// 429 에 Retry-After 가 없을 때 기다리는 시간 (서버 쓰기 제한의 창이 60초다)
pub const DEFAULT_RETRY_AFTER_MS: u64 = 60_000;

/// 답글의 부모
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ParentRef {
    /// 이미 서버에 있는 평가의 id
    Server(String),
    /// 아직 큐에 있는 평가의 로컬 id (enqueue가 돌려준 값)
    Local(String),
}

/// 현재 시각 (밀리초, UTC epoch). 로컬 레코드의 created_at·updated_at에 쓴다.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 큐 항목 한 건. 로컬 저장소(local.rs, outbox.rs)가 행으로 바꿔 쓰는 데도 쓰인다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PendingSave {
    pub(crate) local_id: String,
    pub(crate) request_id: String,
    pub(crate) cc: String,
    /// 원문과 해시. 보내는 시점에 그대로 다시 보내야 하므로 큐에 함께 보관한다
    pub(crate) text: String,
    pub(crate) text_hash: String,
    pub(crate) vector: Vec<f32>,
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
    /// 콘텐츠 해시 주소. 이전 파일에는 없으므로 기본값은 None이다
    #[serde(default)]
    pub(crate) bcc: Option<String>,
    pub(crate) parent: Option<ParentRef>,
    #[serde(default)]
    pub(crate) attempts: u32,
    /// 큐에 넣은 시각 (밀리초). 이전 파일에는 없으므로 기본값 0이다
    #[serde(default)]
    pub(crate) created_at: i64,
}

/// 보낸 항목 또는 거부된 항목의 결과
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum Resolution {
    Sent { id: String },
    Rejected,
}

/// 서버가 접수만 하고 최종 결과를 아직 알려 주지 않은 항목
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tracked {
    pub(crate) local_id: String,
    /// 서버 id
    pub(crate) id: String,
    /// 서버가 접수한 시각 (밀리초). 이전 파일에는 없으므로 기본값 0 (0 은 기한을 따지지 않는다)
    #[serde(default)]
    pub(crate) since_ms: i64,
}

/// 오프라인 저장 큐. 평가 저장 요청이 서버에 닿기 전까지 여기에 머문다.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OfflineQueue {
    next_seq: u64,
    pending: VecDeque<PendingSave>,
    /// 로컬 id -> 보낸 결과. 나중에 생긴 답글이 부모의 서버 id를 찾는 데 쓴다.
    resolved: BTreeMap<String, Resolution>,
    /// request_id -> 서버가 접수만 한 항목. 최종 결과가 나오면 지운다.
    #[serde(default)]
    tracking: BTreeMap<String, Tracked>,
    /// 자동 저장 경로. 파일에는 쓰지 않는다.
    #[serde(skip)]
    path: Option<std::path::PathBuf>,
}

/// sync_status의 결과
#[derive(Debug, Default)]
pub struct SyncReport {
    /// 이번에 저장 완료로 확인된 항목
    pub stored: Vec<SyncedItem>,
    /// 서버가 욕설 등으로 거부했거나 실패로 끝낸 항목
    pub rejected: Vec<RejectedItem>,
    /// 서버가 기록을 지워서 결과를 알 수 없는 항목의 로컬 id (거부 기록은 7일 뒤 지워진다)
    pub missing: Vec<String>,
    /// 아직 결과를 기다리는 항목 수 (확인을 마친 뒤 추적 중인 전체 수)
    pub still_pending: usize,
    /// 연결이나 서버 일시 오류로 멈춘 이유. None이면 끝까지 확인했다.
    pub stopped: Option<String>,
    /// 401 · 403 으로 멈췄다: 다시 로그인할 때까지 확인을 미룬다
    pub auth_required: bool,
    /// 429 로 멈췄다: 이만큼 뒤에 다시 확인한다 (밀리초)
    pub retry_after_ms: Option<u64>,
    /// 디스크 저장에 실패한 이유 (메모리 상태는 이미 바뀌었다)
    pub persist_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedItem {
    pub local_id: String,
    pub id: String,
}

/// 한 번 보내기(flush)의 결과
#[derive(Debug, Default)]
pub struct FlushReport {
    /// 이번에 보낸 항목 (보낸 순서대로)
    pub sent: Vec<SentItem>,
    /// 서버가 영구적으로 거부해서 버린 항목
    pub rejected: Vec<RejectedItem>,
    /// 연결이나 서버 일시 오류로 멈춘 이유. None이면 큐를 끝까지 비웠다.
        pub stopped: Option<String>,
    /// 401 · 403 으로 멈췄다: 다시 로그인할 때까지 보내지 않는다 (항목은 그대로, 시도 횟수도 그대로)
    pub auth_required: bool,
    /// 429 로 멈췄다: 이만큼 뒤에 다시 보낸다 (밀리초, 항목과 시도 횟수는 그대로)
    pub retry_after_ms: Option<u64>,
    /// 보내기가 끝난 뒤 남은 항목 수
    pub remaining: usize,
    /// 디스크 저장에 실패한 이유 (메모리 상태는 이미 바뀌었다)
    pub persist_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SentItem {
    pub local_id: String,
    pub response: SaveResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedItem {
    pub local_id: String,
    pub reason: String,
}

/// 보내기 · 상태 확인 · 투표가 함께 쓰는 실패 갈래 (평가 큐와 투표 보관함이 같은 규칙을 쓴다)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncFailure {
    /// 연결이 없다. 멈추고 항목은 그대로 둔다. 시도 횟수를 세지 않는다.
    Offline,
    /// 401 · 403: 멈추고 항목은 그대로 둔다. 시도 횟수를 세지 않는다. 다시 로그인할 때까지 미룬다
    AuthHold { status: u16 },
    /// 429: 멈추고 항목은 그대로 둔다. 시도 횟수를 세지 않는다. `retry_after_ms` 뒤로 미룬다
    RateLimited { retry_after_ms: u64 },
    /// 서버 일시 오류 (408 · 5xx · 대상이 밝혀지지 않은 404 · 응답 오류). 횟수를 세고, 한도에 닿으면 버린다.
    Transient,
    /// 영구 거부 (400 · 409 · 422 · 본문이 "evaluation not found"인 404). 바로 버린다.
    Permanent,
}

/// 404 본문이 대상(평가 · 부모 평가)이 서버에 없다고 밝히는가 (서버 workers/www 의 메시지)
pub(crate) fn names_missing_target(status: u16, message: &str) -> bool {
    status == 404 && message.contains("evaluation not found")
}

/// 오류를 다섯 갈래로 나눈다 ([`SyncFailure`])
pub fn classify_failure(err: &AppError) -> SyncFailure {
    match err {
        AppError::Offline(_) => SyncFailure::Offline,
        AppError::Http(_) | AppError::BadResponse(_) => SyncFailure::Transient,
        AppError::Server { status: status @ (401 | 403), .. } => SyncFailure::AuthHold { status: *status },
        AppError::Server { status: 429, .. } => SyncFailure::RateLimited { retry_after_ms: DEFAULT_RETRY_AFTER_MS },
        AppError::Server { status, message } if names_missing_target(*status, message) => SyncFailure::Permanent,
        // 대상이 밝혀지지 않은 404 는 설정이나 라우팅 문제일 수 있다. 데이터를 바로 버리지 않고 재시도한다
        AppError::Server { status, .. } if matches!(*status, 404 | 408) || *status >= 500 => SyncFailure::Transient,
        _ => SyncFailure::Permanent,
    }
}

/// 서버 status가 거부 또는 실패인가 (pending·processing·stored는 보내기 성공으로 본다)
fn is_refused(status: Option<&str>) -> bool {
    matches!(status, Some("rejected") | Some("failed"))
}

/// 서버가 접수만 했고 처리가 끝나지 않은 status인가
fn is_in_progress(status: Option<&str>) -> bool {
    matches!(status, Some("pending") | Some("processing"))
}

impl OfflineQueue {
    /// 파일에서 불러오고(없으면 빈 큐) 이 경로에 자동 저장하도록 설정한다.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, AppError> {
        let path = path.into();
        let mut queue = Self::load_from(&path)?;
        queue.path = Some(path);
        Ok(queue)
    }

    /// 서버가 접수만 하고 최종 결과를 아직 모르는 항목 수
    pub fn awaiting_result(&self) -> usize {
        self.tracking.len()
    }

    /// 자동 저장 경로가 있으면 저장한다. 없으면 아무것도 하지 않는다.
    fn persist(&self) -> Result<(), AppError> {
        match &self.path {
            Some(path) => self.save_to(path),
            None => Ok(()),
        }
    }

    /// 대기 중인 항목 수
    pub fn len(&self) -> usize {
        self.pending.len()
    }

        pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    // ---- 로컬 저장소(outbox.rs)가 큐를 복원하고 상태를 맞추는 데 쓰는 내부 접근 ----

    /// 로컬 저장소의 행에서 큐를 다시 만든다. pending은 보낼 순서대로 넘긴다.
    pub(crate) fn restore(
        next_seq: u64,
        pending: Vec<PendingSave>,
        tracking: Vec<(String, Tracked)>,
        resolved: Vec<(String, Resolution)>,
    ) -> Self {
        Self {
            next_seq,
            pending: pending.into(),
            resolved: resolved.into_iter().collect(),
            tracking: tracking.into_iter().collect(),
            path: None,
        }
    }

    

    pub(crate) fn pending_items(&self) -> impl Iterator<Item = &PendingSave> {
        self.pending.iter()
    }

    pub(crate) fn tracked_items(&self) -> impl Iterator<Item = (&String, &Tracked)> {
        self.tracking.iter()
    }

    pub(crate) fn pending_item(&self, local_id: &str) -> Option<&PendingSave> {
        self.pending.iter().find(|p| p.local_id == local_id)
    }

    /// 큐가 모르는 부모의 결과를 알려 준다 (재시작 뒤 이미 처리된 부모에 답글을 다 때 로컬 저장소에서 찾아 넓는다)
    pub(crate) fn learn_resolution(&mut self, local_id: &str, resolution: Resolution) {
        self.resolved.entry(local_id.to_string()).or_insert(resolution);
    }

    /// 방금 enqueue한 항목을 되돌린다 (로컬 저장소에 쓰지 못했을 때). 마지막 항목이 local_id일 때만 동작한다.
    pub(crate) fn cancel_last(&mut self, local_id: &str) {
        if self.pending.back().is_some_and(|p| p.local_id == local_id) {
            self.pending.pop_back();
            self.next_seq = self.next_seq.saturating_sub(1);
        }
    }


    /// 저장 요청을 큐 끝에 넣고 로컬 id를 돌려준다.
    /// 답글이면 parent를 넘긴다. req.ref_는 비워 둬야 한다 (부모는 parent로만 전달한다).
    /// parent가 Local이면 그 항목이 큐에 있거나 이미 보내진 상태여야 한다.
    pub fn enqueue(&mut self, req: SaveRequest, parent: Option<ParentRef>) -> Result<String, AppError> {
        // 같은 request_id 가 이미 큐에 있으면 같은 저장이다 (5초 멱등 키, api::derive_idempotent_request_id): 하나만 둔다
        if let Some(existing) = self.pending.iter().find(|p| p.request_id == req.request_id) {
            return Ok(existing.local_id.clone());
        }
        if self.pending.len() >= MAX_PENDING_ITEMS {
            return Err(AppError::QueueFull(self.pending.len()));
        }
        if req.ref_.is_some() {
            return Err(AppError::InvalidInput(
                "pass the parent in `parent`, not in req.ref_".into(),
            ));
        }
        let parent = match parent {
            None => None,
            Some(ParentRef::Server(id)) => Some(ParentRef::Server(id)),
            Some(ParentRef::Local(local)) => match self.resolved.get(&local) {
                Some(Resolution::Sent { id }) => Some(ParentRef::Server(id.clone())),
                Some(Resolution::Rejected) => {
                    return Err(AppError::Rejected(format!("parent {local} was rejected")));
                }
                None if self.pending.iter().any(|p| p.local_id == local) => {
                    Some(ParentRef::Local(local))
                }
                None => return Err(AppError::InvalidInput(format!("unknown local parent: {local}"))),
            },
        };

        self.next_seq += 1;
        let local_id = format!("local-{}", self.next_seq);
        self.pending.push_back(PendingSave {
            local_id: local_id.clone(),
            request_id: req.request_id,
            cc: req.cc,
            text: req.text,
            text_hash: req.text_hash,
            vector: req.vector,
            from: req.from,
            to: req.to,
            bcc: req.bcc,
            parent,
            attempts: 0,
            created_at: now_ms(),
        });
        // 디스크에 쓰지 못하면 앱이 죽을 때 항목이 사라진다. 저장되지 않은 항목은 받지 않는다
        if let Err(err) = self.persist() {
            self.pending.pop_back();
            self.next_seq -= 1;
            return Err(err);
        }
        Ok(local_id)
    }

    /// 큐 앞에서부터 보낸다. 연결이 없거나 일시 오류가 나면 거기서 멈추고 나머지는 그대로 둔다.
        pub async fn flush<B: EvalBackend + ?Sized>(&mut self, backend: &B) -> FlushReport {
        let mut report = FlushReport::default();
        let mut changed = false;
        loop {
            let Some(front) = self.pending.front().cloned() else {
                break;
            };

            let ref_ = match self.resolve_parent(&front) {
                Ok(ref_) => ref_,
                Err(reason) => {
                    self.reject_front(reason, &mut report);
                    continue;
                }
            };

            let req = SaveRequest {
                request_id: front.request_id.clone(),
                from: front.from.clone(),
                to: front.to.clone(),
                bcc: front.bcc.clone(),
                cc: front.cc.clone(),
                text: front.text.clone(),
                text_hash: front.text_hash.clone(),
                vector: front.vector.clone(),
                ref_,
            };
            match backend.save(&req).await {
                                // 서버가 거부했거나 실패로 기록한 항목은 더 보내지 않고 버린다 (원문은 서버에서 이미 지워졌다)
                Ok(response) if is_refused(response.status.as_deref()) => {
                    changed = true;
                    let reason = format!("server status {}", response.status.as_deref().unwrap_or(""));
                    self.reject_front(reason, &mut report);
                }
                Ok(response) => {
                    changed = true;
                    self.pending.pop_front();
                    self.resolved.insert(
                        front.local_id.clone(),
                        Resolution::Sent { id: response.id.clone() },
                    );
                    // 접수(pending, processing)만 된 항목은 최종 결과를 나중에 물어봐야 한다
                    if is_in_progress(response.status.as_deref()) {
                        self.tracking.insert(
                            front.request_id.clone(),
                            Tracked {
                                local_id: front.local_id.clone(),
                                id: response.id.clone(),
                                since_ms: now_ms(),
                            },
                        );
                    }
                    report.sent.push(SentItem {
                        local_id: front.local_id,
                        response,
                    });
                }
                Err(err) => match classify_failure(&err) {
                    SyncFailure::Offline => {
                        report.stopped = Some(err.to_string());
                                                break;
                    }
                    // 인증 만료 · 속도 제한은 항목 탓이 아니다. 시도 횟수를 세지 않고 멈춘다 (예전에는 50초 만에 버렸다)
                    SyncFailure::AuthHold { .. } => {
                        report.auth_required = true;
                        report.stopped = Some(format!("auth_required: {err}"));
                        break;
                    }
                    SyncFailure::RateLimited { retry_after_ms } => {
                        report.retry_after_ms = Some(retry_after_ms);
                        report.stopped = Some(format!("rate_limited: {err}"));
                        break;
                    }
                    SyncFailure::Transient => {
                        changed = true;
                        if let Some(item) = self.pending.front_mut() {
                            item.attempts += 1;
                            if item.attempts < MAX_SERVER_ATTEMPTS {
                                report.stopped = Some(err.to_string());
                                break;
                            }
                        }
                        self.reject_front(err.to_string(), &mut report);
                    }
                    SyncFailure::Permanent => {
                        changed = true;
                        self.reject_front(err.to_string(), &mut report);
                    }
                },
            }
        }
        report.remaining = self.pending.len();
        // 항목마다 저장하면 벡터가 든 파일을 매번 다시 쓰게 된다. 보내기가 끝날 때 한 번만 쓴다.
        // 저장 전에 앱이 죽어도 같은 request_id로 다시 보내므로 서버가 중복 저장하지 않는다.
        if changed {
            report.persist_error = self.persist().err().map(|e| e.to_string());
        }
        report
    }

    /// 서버가 접수만 한 항목의 최종 결과를 물어본다. 앱을 시작할 때와 주기적으로(예: flush 뒤) 호출한다.
    /// - stored: 확인 완료. 더 이상 추적하지 않는다.
    /// - rejected, failed: 거부로 기록한다. 이 항목을 부모로 하는 답글은 이후 enqueue에서 막힌다.
    /// - pending, processing: 계속 추적한다.
    /// - 404: 서버가 기록을 지웠다. 더 물어볼 수 없으므로 추적을 끝내고 missing으로 알린다.
    /// - 연결 실패, 일시 오류: 거기서 멈추고 나머지는 다음에 확인한다.
    pub async fn sync_status<B: EvalBackend + ?Sized>(&mut self, backend: &B) -> SyncReport {
        let mut report = SyncReport::default();
        // 너무 오래 기다렸거나 상한을 넘긴 항목은 묻지 않고 missing 으로 끝낸다 (원문은 로컬 행에 남는다)
        let mut changed = self.expire_tracking(now_ms(), &mut report.missing);
        let request_ids: Vec<String> = self.tracking.keys().cloned().collect();

        for request_id in request_ids {
            let Some(tracked) = self.tracking.get(&request_id).cloned() else {
                continue;
            };
            match backend.status(&request_id).await {
                Ok(st) => match st.status.as_str() {
                    "stored" => {
                        changed = true;
                        self.tracking.remove(&request_id);
                        report.stored.push(SyncedItem {
                            local_id: tracked.local_id,
                            id: tracked.id,
                        });
                    }
                    "rejected" | "failed" => {
                        changed = true;
                        self.tracking.remove(&request_id);
                        self.resolved.insert(tracked.local_id.clone(), Resolution::Rejected);
                        let reason = match st.reason {
                            Some(r) => format!("server status {}: {r}", st.status),
                            None => format!("server status {}", st.status),
                        };
                        report.rejected.push(RejectedItem {
                            local_id: tracked.local_id,
                            reason,
                        });
                    }
                    // pending, processing, 알 수 없는 값은 계속 추적한다
                    _ => {}
                },
                Err(AppError::Server { status: 404, .. }) => {
                    changed = true;
                    self.tracking.remove(&request_id);
                    report.missing.push(tracked.local_id);
                }
                Err(err) => {
                    // 연결 실패, 5xx, 401 등은 데이터를 잃지 않게 추적을 유지하고 멈춘다
                    match classify_failure(&err) {
                        SyncFailure::AuthHold { .. } => {
                            report.auth_required = true;
                            report.stopped = Some(format!("auth_required: {err}"));
                        }
                        SyncFailure::RateLimited { retry_after_ms } => {
                            report.retry_after_ms = Some(retry_after_ms);
                            report.stopped = Some(format!("rate_limited: {err}"));
                        }
                        _ => report.stopped = Some(err.to_string()),
                    }
                    break;
                }
            }
        }

        report.still_pending = self.tracking.len();
        if changed {
            report.persist_error = self.persist().err().map(|e| e.to_string());
        }
        report
    }

    /// JSON 문자열로 직렬화한다. 대기 항목과 id 매핑이 모두 들어간다.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }

    /// 디스크에 저장한다. 임시 파일에 먼저 쓰고 이름을 바꾸므로, 쓰는 도중 끊겨도 이전 파일이 남는다.
    /// 호출하는 쪽이 큐가 바뀔 때마다(보내기 뒤, 저장 뒤) 호출해야 한다.
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), AppError> {
        let json = self.to_json().map_err(|e| AppError::Storage(e.to_string()))?;
        crate::utils::write_file_atomic(path, json.as_bytes()).map_err(|e| AppError::Storage(e.to_string()))
    }

    /// 파일에서 되살린다. 파일이 없으면 빈 큐다. 내용이 깨졌으면 에러를 돌려주고, 조용히 빈 큐로 바꾸지 않는다.
    pub fn load_from(path: &std::path::Path) -> Result<Self, AppError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text).map_err(|e| AppError::Storage(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(AppError::Storage(e.to_string())),
        }
    }

    /// 항목의 부모를 서버 id로 바꾼다. 부모가 거부됐으면 Err.
    fn resolve_parent(&self, item: &PendingSave) -> Result<Option<String>, String> {
        match &item.parent {
            None => Ok(None),
            Some(ParentRef::Server(id)) => Ok(Some(id.clone())),
            Some(ParentRef::Local(local)) => match self.resolved.get(local) {
                Some(Resolution::Sent { id }) => Ok(Some(id.clone())),
                Some(Resolution::Rejected) => Err(format!("parent {local} was rejected")),
                None => Err(format!("parent {local} is not resolved")),
            },
        }
    }

    /// 결과를 [`RETAIN_MS`] 넘게 못 받은 항목과, 추적이 [`MAX_TRACKING_ITEMS`] 를 넘을 때 가장 오래 기다린 항목의 추적을
    /// 끝낸다. 끝낸 항목의 로컬 id 를 `missing` 에 넣는다. 서버에 다시 묻지 않으므로 결과는 모르지만, 로컬 행(원문 · 벡터)은
    /// 지우지 않는다(내 메모로 남는다). 부모로는 쓸 수 없다(outbox 의 resolution_of 가 missing 을 거부로 본다)
    pub(crate) fn expire_tracking(&mut self, now: i64, missing: &mut Vec<String>) -> bool {
        let mut expired: Vec<String> = self
            .tracking
            .iter()
            .filter(|(_, t)| t.since_ms > 0 && now - t.since_ms > RETAIN_MS)
            .map(|(request_id, _)| request_id.clone())
            .collect();
        let over = (self.tracking.len() - expired.len()).saturating_sub(MAX_TRACKING_ITEMS);
        if over > 0 {
            let mut rest: Vec<(&String, &Tracked)> = self.tracking.iter().filter(|(r, _)| !expired.contains(r)).collect();
            rest.sort_by_key(|(r, t)| (t.since_ms, (*r).clone()));
            expired.extend(rest.into_iter().take(over).map(|(r, _)| r.clone()));
        }
        for request_id in &expired {
            if let Some(t) = self.tracking.remove(request_id) {
                self.resolved.insert(t.local_id.clone(), Resolution::Rejected);
                missing.push(t.local_id);
            }
        }
        !expired.is_empty()
    }

    fn reject_front(&mut self, reason: String, report: &mut FlushReport) {
        if let Some(item) = self.pending.pop_front() {
            self.resolved.insert(item.local_id.clone(), Resolution::Rejected);
            report.rejected.push(RejectedItem {
                local_id: item.local_id,
                reason,
            });
        }
    }
}

/// 테스트용 가짜 백엔드. 호출 한 번마다 시나리오에서 하나씩 소비하고, 시나리오가 비면 저장을 받아들인다.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use crate::api::{EvalBackend, SaveRequest, SaveResponse, SearchRequest, SearchResponse};
    use crate::error::AppError;

    #[derive(Debug, Clone, Copy)]
    pub enum Step {
        Accept,
        Offline,
        Status(u16),
        /// 저장은 됐지만 응답이 유실된 경우 (연결 실패로 보인다)
        AcceptButLose,
    }

    #[derive(Default)]
    pub struct ScriptedBackend {
        steps: Mutex<VecDeque<Step>>,
        pub saved: Mutex<Vec<SaveRequest>>,
        /// 서버가 저장한 평가 (request_id -> 응답). 같은 request_id는 다시 저장하지 않는다.
        stored: Mutex<HashMap<String, SaveResponse>>,
        accepted: AtomicUsize,
    }

    impl ScriptedBackend {
        pub fn with_steps(steps: Vec<Step>) -> Self {
            Self {
                steps: Mutex::new(steps.into()),
                ..Default::default()
            }
        }
    }

    #[async_trait::async_trait]
    impl EvalBackend for ScriptedBackend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            self.saved.lock().unwrap().push(req.clone());
            // 서버 쪽 멱등 처리: 이미 저장된 request_id면 저장된 결과를 돌려준다 (시나리오는 소비하지 않는다)
            let existing = self.stored.lock().unwrap().get(&req.request_id).cloned();
            if let Some(existing) = existing {
                return Ok(existing);
            }
            let step = self.steps.lock().unwrap().pop_front().unwrap_or(Step::Accept);
            match step {
                Step::Offline => Err(AppError::Offline("no network".into())),
                Step::Status(status) => Err(AppError::Server {
                    status,
                    message: "scripted".into(),
                }),
                Step::Accept | Step::AcceptButLose => {
                    // 서버 id는 받아들인 순서대로 srv-1, srv-2 ...
                    let n = self.accepted.fetch_add(1, Ordering::SeqCst) + 1;
                    let resp = SaveResponse {
                        id: format!("srv-{n}"),
                        root: format!("srv-{n}"),
                        ref_: req.ref_.clone(),
                        status: None,
                        from: req
                            .from
                            .clone()
                            .unwrap_or_else(|| "0x00000000000000000000000000000000000000ab".into()),
                    };
                    self.stored
                        .lock()
                        .unwrap()
                        .insert(req.request_id.clone(), resp.clone());
                    if matches!(step, Step::AcceptButLose) {
                        return Err(AppError::Offline("stored, but response lost".into()));
                    }
                    Ok(resp)
                }
            }
        }

        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, _request_id: &str) -> Result<crate::api::StatusResponse, AppError> {
            Err(AppError::Server {
                status: 404,
                message: "not used in this test".into(),
            })
        }
    }

    impl ScriptedBackend {
        pub fn stored_count(&self) -> usize {
            self.stored.lock().unwrap().len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{ScriptedBackend, Step};
    use super::*;

    fn req(cc: &str, seed: f32) -> SaveRequest {
        SaveRequest {
            bcc: None,
            request_id: format!("req-{cc}-{seed}"),
            from: None,
            to: None,
            cc: cc.into(),
            text: format!("text {seed}"),
            text_hash: crate::api::text_hash(&format!("text {seed}")),
            vector: vec![seed; 4],
            ref_: None,
        }
    }

    #[tokio::test]
    async fn offline_items_wait_and_go_out_in_order() {
        let mut q = OfflineQueue::default();
        let a = q.enqueue(req("a.com", 1.0), None).unwrap();
        let b = q.enqueue(req("b.com", 2.0), None).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("local-1", "local-2"));

        let offline = ScriptedBackend::with_steps(vec![Step::Offline]);
        let r1 = q.flush(&offline).await;
        assert!(r1.sent.is_empty() && r1.rejected.is_empty());
        assert!(r1.stopped.is_some());
        assert_eq!(r1.remaining, 2);
        assert_eq!(q.len(), 2);

        let online = ScriptedBackend::default();
        let r2 = q.flush(&online).await;
        let order: Vec<&str> = r2.sent.iter().map(|s| s.local_id.as_str()).collect();
        assert_eq!(order, ["local-1", "local-2"]);
        assert!(r2.stopped.is_none());
        assert_eq!(r2.remaining, 0);
        assert!(q.is_empty());

        let saved = online.saved.lock().unwrap();
        assert_eq!(saved[0].cc, "a.com");
        assert_eq!(saved[1].cc, "b.com");
    }

    #[tokio::test]
    async fn reply_is_sent_with_parent_server_id_after_parent() {
        let mut q = OfflineQueue::default();
        let root = q.enqueue(req("a.com", 1.0), None).unwrap();
        q.enqueue(req("a.com", 2.0), Some(ParentRef::Local(root))).unwrap();

        let backend = ScriptedBackend::default();
        let report = q.flush(&backend).await;
        assert_eq!(report.sent.len(), 2);
        let root_server = report.sent[0].response.id.clone();

        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].ref_, None);
        assert_eq!(saved[1].ref_.as_deref(), Some(root_server.as_str()));
    }

    #[tokio::test]
    async fn reply_to_already_sent_parent_uses_server_id_at_enqueue() {
        let mut q = OfflineQueue::default();
        let root = q.enqueue(req("a.com", 1.0), None).unwrap();
        let backend = ScriptedBackend::default();
        q.flush(&backend).await;

        q.enqueue(req("a.com", 2.0), Some(ParentRef::Local(root))).unwrap();
        // 부모가 이미 서버에 있으므로 큐 안에서는 서버 id로 바뀌어 있다
        assert_eq!(q.pending.back().unwrap().parent, Some(ParentRef::Server("srv-1".into())));
    }

    #[tokio::test]
    async fn server_errors_retry_then_reject_without_blocking_later_items() {
        let mut q = OfflineQueue::default();
        let bad = q.enqueue(req("a.com", 1.0), None).unwrap();
        let good = q.enqueue(req("b.com", 2.0), None).unwrap();
        let backend = ScriptedBackend::with_steps(vec![Step::Status(503); MAX_SERVER_ATTEMPTS as usize]);

        for _ in 1..MAX_SERVER_ATTEMPTS {
            let r = q.flush(&backend).await;
            assert!(r.stopped.is_some());
            assert_eq!(q.len(), 2, "still waiting for retry");
        }

        let last = q.flush(&backend).await;
        assert_eq!(last.rejected.len(), 1);
        assert_eq!(last.rejected[0].local_id, bad);
        assert_eq!(last.sent.len(), 1);
        assert_eq!(last.sent[0].local_id, good);
        assert!(q.is_empty());
    }

    #[tokio::test]
    async fn permanent_rejection_drops_item_and_its_replies() {
        let mut q = OfflineQueue::default();
        let root = q.enqueue(req("a.com", 1.0), None).unwrap();
        let reply = q.enqueue(req("a.com", 2.0), Some(ParentRef::Local(root.clone()))).unwrap();
        let other = q.enqueue(req("b.com", 3.0), None).unwrap();

        let backend = ScriptedBackend::with_steps(vec![Step::Status(400)]);
        let r = q.flush(&backend).await;

        let rejected: Vec<&str> = r.rejected.iter().map(|x| x.local_id.as_str()).collect();
        assert_eq!(rejected, [root.as_str(), reply.as_str()]);
        assert!(r.rejected[1].reason.contains("was rejected"));
        assert_eq!(r.sent.len(), 1);
        assert_eq!(r.sent[0].local_id, other);
        assert_eq!(backend.saved.lock().unwrap().len(), 2, "rejected reply must not reach the server");

        let late = q.enqueue(req("a.com", 4.0), Some(ParentRef::Local(root)));
        assert!(matches!(late, Err(AppError::Rejected(_))));
    }

    #[tokio::test]
    async fn enqueue_rejects_unknown_parent_and_ref_field() {
        let mut q = OfflineQueue::default();
        let unknown = q.enqueue(req("a.com", 1.0), Some(ParentRef::Local("local-99".into())));
        assert!(matches!(unknown, Err(AppError::InvalidInput(_))));

        let mut with_ref = req("a.com", 1.0);
        with_ref.ref_ = Some("0190f0a0-0000-7000-8000-000000000001".into());
        assert!(q.enqueue(with_ref, None).is_err());
        assert!(q.is_empty());
    }

    #[tokio::test]
    async fn lost_response_is_not_stored_twice() {
        // 서버는 저장했지만 응답이 유실된 경우: 큐가 같은 request_id로 다시 보내고, 서버는 한 번만 저장한다
        let mut q = OfflineQueue::default();
        q.enqueue(req("a.com", 1.0), None).unwrap();
        let backend = ScriptedBackend::with_steps(vec![Step::AcceptButLose]);
        let r1 = q.flush(&backend).await;
        assert!(r1.stopped.is_some());
        assert_eq!(q.len(), 1);
        let r2 = q.flush(&backend).await;
        assert_eq!(r2.sent.len(), 1);
        assert!(q.is_empty());
        assert_eq!(backend.stored_count(), 1);
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].request_id, saved[1].request_id);
    }

    #[tokio::test]
    async fn queue_survives_json_round_trip_with_id_mapping() {
        let mut q = OfflineQueue::default();
        let root = q.enqueue(req("a.com", 1.0), None).unwrap();
        let json = q.to_json().unwrap();
        let mut restored = OfflineQueue::from_json(&json).unwrap();
        assert_eq!(restored, q);

        restored.flush(&ScriptedBackend::default()).await;
        let reply = restored.enqueue(req("a.com", 2.0), Some(ParentRef::Local(root))).unwrap();
        assert_eq!(reply, "local-2", "sequence number survives restart");

        let backend = ScriptedBackend::default();
        restored.flush(&backend).await;
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].ref_.as_deref(), Some("srv-1"));
    }

    fn temp_queue_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pd-queue-{tag}-{}-{nanos}.json", std::process::id()))
    }

    #[test]
    fn queue_saved_to_disk_comes_back_with_same_items() {
        let path = temp_queue_path("roundtrip");
        let mut q = OfflineQueue::default();
        let root = q.enqueue(req("a.com", 1.0), None).unwrap();
        q.enqueue(req("a.com", 2.0), Some(ParentRef::Local(root))).unwrap();
        q.save_to(&path).unwrap();

        let restored = OfflineQueue::load_from(&path).unwrap();
        assert_eq!(restored, q);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn loading_missing_file_gives_empty_queue() {
        let path = temp_queue_path("missing");
        assert!(OfflineQueue::load_from(&path).unwrap().is_empty());
    }

    #[test]
    fn loading_corrupt_file_is_an_error_not_silent_loss() {
        let path = temp_queue_path("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(OfflineQueue::load_from(&path), Err(AppError::Storage(_))));
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn unauthorized_response_keeps_item_for_retry() {
        let mut q = OfflineQueue::default();
        q.enqueue(req("a.com", 1.0), None).unwrap();
        let backend = ScriptedBackend::with_steps(vec![Step::Status(401)]);
        let r1 = q.flush(&backend).await;
        assert!(r1.rejected.is_empty());
        assert_eq!(q.len(), 1, "401은 설정 문제일 수 있으므로 바로 버리지 않는다");
        let r2 = q.flush(&ScriptedBackend::default()).await;
        assert_eq!(r2.sent.len(), 1);
    }

    #[test]
    fn enqueue_rejects_when_queue_is_full() {
        let mut q = OfflineQueue::default();
        for i in 0..MAX_PENDING_ITEMS {
            q.enqueue(req("a.com", i as f32), None).unwrap();
        }
        assert!(matches!(
            q.enqueue(req("a.com", 0.5), None),
            Err(AppError::QueueFull(n)) if n == MAX_PENDING_ITEMS
        ));
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    use crate::api::{SearchRequest, SearchResponse, StatusResponse};

    /// 모든 저장에 같은 status를 돌려주는 가짜 백엔드
    struct AlwaysStatus(&'static str);

    #[async_trait::async_trait]
    impl EvalBackend for AlwaysStatus {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            Ok(SaveResponse {
                id: "srv-1".into(),
                root: "srv-1".into(),
                ref_: req.ref_.clone(),
                from: "0x00000000000000000000000000000000000000ab".into(),
                status: Some(self.0.into()),
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            Err(AppError::Server { status: 404, message: "unused".into() })
        }
    }

    fn req(seed: f32) -> SaveRequest {
        let text = format!("text {seed}");
        SaveRequest {
            bcc: None,
            request_id: format!("req-{seed}"),
            from: None,
            to: None,
            cc: "a.com".into(),
            text_hash: crate::api::text_hash(&text),
            text,
            vector: vec![seed; 4],
            ref_: None,
        }
    }

    #[tokio::test]
    async fn server_rejection_drops_item_and_is_reported() {
        let mut q = OfflineQueue::default();
        q.enqueue(req(1.0), None).unwrap();
        let report = q.flush(&AlwaysStatus("rejected")).await;
        assert!(report.sent.is_empty());
        assert_eq!(report.rejected.len(), 1);
        assert!(report.rejected[0].reason.contains("rejected"));
        assert!(q.is_empty());
    }

    #[tokio::test]
    async fn pending_response_counts_as_sent() {
        let mut q = OfflineQueue::default();
        q.enqueue(req(2.0), None).unwrap();
        let report = q.flush(&AlwaysStatus("pending")).await;
        assert_eq!(report.sent.len(), 1);
        assert!(report.rejected.is_empty());
        assert!(q.is_empty());
    }

    #[test]
    fn queue_keeps_text_and_hash_across_json_round_trip() {
        let mut q = OfflineQueue::default();
        q.enqueue(req(3.0), None).unwrap();
        let restored = OfflineQueue::from_json(&q.to_json().unwrap()).unwrap();
        assert_eq!(restored, q);
        assert_eq!(restored.pending[0].text, "text 3");
        assert_eq!(restored.pending[0].text_hash, crate::api::text_hash("text 3"));
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use crate::api::{SearchRequest, SearchResponse, StatusResponse};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// 저장은 항상 pending(202)으로 받고, 상태 조회는 정해 둔 답을 차례로 돌려준다
    struct PendingBackend {
        answers: Mutex<VecDeque<Result<(&'static str, Option<&'static str>), u16>>>,
        status_calls: Mutex<Vec<String>>,
    }

    impl PendingBackend {
        fn new(answers: Vec<Result<(&'static str, Option<&'static str>), u16>>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                status_calls: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait::async_trait]
    impl EvalBackend for PendingBackend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            Ok(SaveResponse {
                id: format!("srv-{}", req.request_id),
                root: format!("srv-{}", req.request_id),
                ref_: req.ref_.clone(),
                from: "0x00000000000000000000000000000000000000ab".into(),
                status: Some("pending".into()),
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, request_id: &str) -> Result<StatusResponse, AppError> {
            self.status_calls.lock().unwrap().push(request_id.to_string());
            match self.answers.lock().unwrap().pop_front().expect("unscripted status call") {
                Ok((status, reason)) => Ok(StatusResponse {
                    id: format!("srv-{request_id}"),
                    root: format!("srv-{request_id}"),
                    ref_: None,
                    status: status.into(),
                    reason: reason.map(String::from),
                }),
                Err(0) => Err(AppError::Offline("no network".into())),
                Err(status) => Err(AppError::Server { status, message: "scripted".into() }),
            }
        }
    }

    fn req(seed: f32) -> SaveRequest {
        let text = format!("text {seed}");
        SaveRequest {
            bcc: None,
            request_id: format!("request-id-{seed}-0000000"),
            from: None,
            to: None,
            cc: "a.com".into(),
            text_hash: crate::api::text_hash(&text),
            text,
            vector: vec![seed; 4],
            ref_: None,
        }
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pd-sync-{tag}-{}-{nanos}.json", std::process::id()))
    }

    #[tokio::test]
    async fn pending_items_are_tracked_until_the_server_says_stored() {
        let mut q = OfflineQueue::default();
        let local = q.enqueue(req(1.0), None).unwrap();
        let backend = PendingBackend::new(vec![Ok(("pending", None)), Ok(("processing", None)), Ok(("stored", None))]);
        q.flush(&backend).await;
        assert!(q.is_empty());
        assert_eq!(q.awaiting_result(), 1);

        let r1 = q.sync_status(&backend).await;
        assert_eq!((r1.still_pending, r1.stored.len()), (1, 0));
        let r2 = q.sync_status(&backend).await;
        assert_eq!(r2.still_pending, 1);
        let r3 = q.sync_status(&backend).await;
        assert_eq!(r3.stored.len(), 1);
        assert_eq!(r3.stored[0].local_id, local);
        assert_eq!(r3.still_pending, 0);
        assert_eq!(q.awaiting_result(), 0);
        assert!(r3.stopped.is_none());
    }

    #[tokio::test]
    async fn later_rejection_is_recorded_and_blocks_new_replies() {
        let mut q = OfflineQueue::default();
        let root = q.enqueue(req(1.0), None).unwrap();
        let backend = PendingBackend::new(vec![Ok(("rejected", Some("profanity")))]);
        q.flush(&backend).await;

        let report = q.sync_status(&backend).await;
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].local_id, root);
        assert!(report.rejected[0].reason.contains("profanity"));
        assert_eq!(q.awaiting_result(), 0);

        // 거부로 기록되었으므로 이 항목에 다는 답글은 받지 않는다
        let late = q.enqueue(req(2.0), Some(ParentRef::Local(root)));
        assert!(matches!(late, Err(AppError::Rejected(_))));
    }

    #[tokio::test]
    async fn offline_during_sync_keeps_tracking_and_stops() {
        let mut q = OfflineQueue::default();
        q.enqueue(req(1.0), None).unwrap();
        q.enqueue(req(2.0), None).unwrap();
        let backend = PendingBackend::new(vec![Err(0), Ok(("stored", None)), Ok(("stored", None))]);
        q.flush(&backend).await;
        assert_eq!(q.awaiting_result(), 2);

        let r1 = q.sync_status(&backend).await;
        assert!(r1.stopped.is_some());
        assert_eq!(r1.still_pending, 2, "연결이 없으면 아무것도 지우지 않는다");
        assert_eq!(backend.status_calls.lock().unwrap().len(), 1, "첫 실패에서 멈춘다");

        let r2 = q.sync_status(&backend).await;
        assert_eq!(r2.stored.len(), 2);
        assert!(r2.stopped.is_none());
    }

    #[tokio::test]
    async fn server_error_during_sync_does_not_drop_tracking() {
        let mut q = OfflineQueue::default();
        q.enqueue(req(1.0), None).unwrap();
        let backend = PendingBackend::new(vec![Err(503), Ok(("stored", None))]);
        q.flush(&backend).await;

        let r1 = q.sync_status(&backend).await;
        assert!(r1.stopped.is_some());
        assert_eq!(q.awaiting_result(), 1);
        assert_eq!(q.sync_status(&backend).await.stored.len(), 1);
    }

    #[tokio::test]
    async fn not_found_ends_tracking_and_reports_missing() {
        let mut q = OfflineQueue::default();
        let local = q.enqueue(req(1.0), None).unwrap();
        let backend = PendingBackend::new(vec![Err(404)]);
        q.flush(&backend).await;
        let r = q.sync_status(&backend).await;
        assert_eq!(r.missing, vec![local]);
        assert_eq!(q.awaiting_result(), 0);
    }

    #[tokio::test]
    async fn tracking_survives_json_round_trip_and_old_files_still_load() {
        let mut q = OfflineQueue::default();
        q.enqueue(req(1.0), None).unwrap();
        q.flush(&PendingBackend::new(vec![])).await;
        let restored = OfflineQueue::from_json(&q.to_json().unwrap()).unwrap();
        assert_eq!(restored.awaiting_result(), 1);
        assert_eq!(restored, q);

        // tracking 필드가 없던 이전 버전 파일
        let old = r#"{"next_seq":1,"pending":[],"resolved":{}}"#;
        let loaded = OfflineQueue::from_json(old).unwrap();
        assert_eq!(loaded.awaiting_result(), 0);
    }

    #[tokio::test]
    async fn open_saves_automatically_on_enqueue_flush_and_sync() {
        let path = temp_path("auto");
        let mut q = OfflineQueue::open(&path).unwrap();
        assert!(q.is_empty());

        // enqueue 직후: 앱이 죽어도 항목이 남아 있다
        q.enqueue(req(1.0), None).unwrap();
        assert_eq!(OfflineQueue::load_from(&path).unwrap().len(), 1);

        // flush 뒤: 보낸 항목은 대기열에서 빠지고 추적 목록으로 옮겨진다
        let backend = PendingBackend::new(vec![Ok(("stored", None))]);
        let report = q.flush(&backend).await;
        assert!(report.persist_error.is_none());
        let on_disk = OfflineQueue::load_from(&path).unwrap();
        assert_eq!((on_disk.len(), on_disk.awaiting_result()), (0, 1));

        // sync 뒤: 추적이 끝난 상태가 저장된다
        let sync = q.sync_status(&backend).await;
        assert!(sync.persist_error.is_none());
        assert_eq!(OfflineQueue::load_from(&path).unwrap().awaiting_result(), 0);

        // open으로 다시 열면 같은 상태다
        assert_eq!(OfflineQueue::open(&path).unwrap().awaiting_result(), 0);
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn enqueue_is_refused_when_the_disk_write_fails() {
        // 상위 폴더가 없어서 쓰기가 실패하는 경로
        let path = std::env::temp_dir()
            .join(format!("pd-missing-dir-{}", std::process::id()))
            .join("queue.json");
        let mut q = OfflineQueue::open(&path).unwrap();
        let err = q.enqueue(req(1.0), None).unwrap_err();
        assert!(matches!(err, AppError::Storage(_)), "{err:?}");
        assert!(q.is_empty(), "저장하지 못한 항목은 큐에 남기지 않는다");
        // 번호도 되돌려서 다음 항목이 local-1을 받는다
        let ok_path = temp_path("ok");
        let mut q2 = OfflineQueue::open(&ok_path).unwrap();
        assert_eq!(q2.enqueue(req(1.0), None).unwrap(), "local-1");
        std::fs::remove_file(&ok_path).unwrap();
    }

    #[tokio::test]
    async fn flush_without_changes_does_not_write_the_file() {
        let path = temp_path("nochange");
        let mut q = OfflineQueue::open(&path).unwrap();
        let backend = PendingBackend::new(vec![]);
        let r = q.flush(&backend).await;
        assert!(r.persist_error.is_none());
        assert!(!path.exists(), "바뀐 것이 없으면 쓰지 않는다");
    }
}

/// 시나리오 4 Part 4 (contracts/feedback_credit_v1.json FC-10 · FC-12): 401 · 429 가 항목을 버리지 않는다, 결과 대기 상한,
/// 같은 request_id 는 한 항목
#[cfg(test)]
mod lossless_tests {
    use super::test_support::{ScriptedBackend, Step};
    use super::*;

    fn req(seed: u32) -> SaveRequest {
        let text = format!("lossless {seed}");
        SaveRequest {
            bcc: None,
            request_id: format!("lossless-request-{seed:08}"),
            from: None,
            to: None,
            cc: "a.com".into(),
            text_hash: crate::api::text_hash(&text),
            text,
            vector: vec![seed as f32; 4],
            ref_: None,
        }
    }

    #[test]
    fn failures_split_five_ways() {
        let server = |status: u16, message: &str| AppError::Server { status, message: message.into() };
        assert_eq!(classify_failure(&AppError::Offline("x".into())), SyncFailure::Offline);
        assert_eq!(classify_failure(&server(401, "")), SyncFailure::AuthHold { status: 401 });
        assert_eq!(classify_failure(&server(403, "")), SyncFailure::AuthHold { status: 403 });
        assert_eq!(classify_failure(&server(429, "")), SyncFailure::RateLimited { retry_after_ms: DEFAULT_RETRY_AFTER_MS });
        assert_eq!(classify_failure(&server(503, "")), SyncFailure::Transient);
        assert_eq!(classify_failure(&server(408, "")), SyncFailure::Transient);
        assert_eq!(classify_failure(&server(404, "Not Found")), SyncFailure::Transient, "대상이 밝혀지지 않은 404 는 라우팅 문제일 수 있다");
        assert_eq!(classify_failure(&server(404, r#"{"error":"parent evaluation not found"}"#)), SyncFailure::Permanent);
        assert_eq!(classify_failure(&server(409, "")), SyncFailure::Permanent);
        assert_eq!(classify_failure(&server(422, "")), SyncFailure::Permanent);
        assert_eq!(classify_failure(&server(400, "")), SyncFailure::Permanent);
    }

    // FC-12: 401 · 429 상태로 10초 주기 6번(60초)이 지나도 항목은 그대로이고 시도 횟수는 0, 다시 로그인하면 바로 보낸다
    #[tokio::test]
    async fn fc12_auth_and_rate_limits_keep_every_item_and_never_count_attempts() {
        let mut q = OfflineQueue::default();
        let local = q.enqueue(req(1), None).unwrap();
        let mut steps = vec![Step::Status(401); 6];
        steps.extend(vec![Step::Status(429); 6]);
        let backend = ScriptedBackend::with_steps(steps);
        for round in 0..12 {
            let r = q.flush(&backend).await;
            assert!(r.sent.is_empty() && r.rejected.is_empty(), "round {round}: 버리지 않는다");
            assert_eq!(r.remaining, 1);
            if round < 6 {
                assert!(r.auth_required, "round {round}");
                assert!(r.stopped.as_deref().unwrap().starts_with("auth_required"));
            } else {
                assert!(!r.auth_required);
                assert_eq!(r.retry_after_ms, Some(DEFAULT_RETRY_AFTER_MS));
                assert!(r.stopped.as_deref().unwrap().starts_with("rate_limited"));
            }
            assert_eq!(q.pending_item(&local).unwrap().attempts, 0, "round {round}: 시도 횟수를 세지 않는다");
        }
        // 다시 로그인한 뒤: 시나리오가 비었으니 받아들인다
        let r = q.flush(&backend).await;
        assert_eq!(r.sent.len(), 1);
        assert!(q.is_empty() && r.stopped.is_none() && !r.auth_required);
    }

    // FC-10: 같은 request_id(5초 멱등 키)로 다시 넣으면 같은 항목이다 (두 번 보내지 않는다)
    #[tokio::test]
    async fn fc10_the_same_request_id_is_one_item() {
        let mut q = OfflineQueue::default();
        let a = q.enqueue(req(7), None).unwrap();
        let b = q.enqueue(req(7), None).unwrap();
        assert_eq!(a, b);
        assert_eq!(q.len(), 1);
        let c = q.enqueue(req(8), None).unwrap();
        assert_ne!(a, c, "다른 저장은 다른 항목");
        let backend = ScriptedBackend::default();
        let r = q.flush(&backend).await;
        assert_eq!(r.sent.len(), 2);
        assert_eq!(backend.saved.lock().unwrap().len(), 2);
    }

    // E-17: 결과를 7일 넘게 못 받은 항목과, 상한을 넘긴 가장 오래된 항목은 missing 으로 끝난다 (서버에 묻지 않는다)
    #[tokio::test]
    async fn tracking_is_bounded_by_age_and_count() {
        let now = now_ms();
        let tracked = |i: usize, since_ms: i64| {
            (format!("rq-{i:06}"), Tracked { local_id: format!("local-{i}"), id: format!("srv-{i}"), since_ms })
        };
        let mut entries: Vec<(String, Tracked)> = (0..MAX_TRACKING_ITEMS + 2).map(|i| tracked(i, now - 1_000 + i as i64)).collect();
        entries.push(tracked(999_999, now - RETAIN_MS - 1));
        entries.push(tracked(888_888, 0)); // 예전 파일: 시각이 없으면 기한을 따지지 않는다
        let mut q = OfflineQueue::restore(0, vec![], entries, vec![]);
        let mut missing = Vec::new();
        assert!(q.expire_tracking(now, &mut missing));
        // 7일 지난 1건 + 상한(2000)을 넘긴 3건(시각 0 인 항목이 가장 오래된 것으로 먼저 빠진다)
        assert_eq!(missing.len(), 4, "{missing:?}");
        assert!(missing.contains(&"local-999999".to_string()));
        assert!(missing.contains(&"local-888888".to_string()));
        assert!(missing.contains(&"local-0".to_string()) && missing.contains(&"local-1".to_string()));
        assert_eq!(q.awaiting_result(), MAX_TRACKING_ITEMS);
        // missing 은 부모로 쓸 수 없다
        assert!(matches!(q.enqueue(req(1), Some(ParentRef::Local("local-0".into()))), Err(AppError::Rejected(_))));
        let mut again = Vec::new();
        assert!(!q.expire_tracking(now, &mut again), "다시 불러도 더 끝낼 것이 없다");
    }
}
