//! 로컬 보관함: 큐(`OfflineQueue`)의 보내기 로직 위에 로컬 원본 저장소(`LocalDb`, LanceDB)를 얹는다.
//!
//! 역할 나누기
//! - `OfflineQueue`: 무엇을 어떤 순서로 보내고, 실패를 어떻게 다룰지 (메모리 상태 기계, 기존 테스트 유지)
//! - `LocalDb`: 평가 한 건당 한 행. 원문과 벡터와 상태를 디스크에 남긴다 (원본)
//! - `Outbox`: 두 가지를 묶는다. 큐의 상태가 바뀔 때마다 해당 행만 갱신하고, 시작할 때 행에서 큐를 되살린다.
//!
//! 저장 방식은 JSON 큐 파일과 다르다. 파일 전체를 다시 쓰지 않고 바뀐 행의 바뀐 컬럼만 쓴다.
//!
//! 중간에 앱이 죽었을 때
//! - enqueue: 행을 먼저 쓰고, 쓰지 못하면 큐에서도 뺀다 (저장되지 않은 항목은 받지 않는다).
//! - flush와 sync_status: 서버가 받았는데 행을 갱신하기 전에 죽으면, 다시 열 때 그 행은 `queued`로 남아 있다.
//!   같은 request_id로 다시 보내면 서버가 중복 저장하지 않고 처음 결과를 돌려준다 (멱등).

use std::path::Path;

use serde_json::Value;

use crate::api::{EvalBackend, SaveRequest};
use crate::error::AppError;
use crate::local::{local_seq, LocalDb, LocalMatch, LocalRecord, LocalState, SqlValue};
use crate::queue::{
    now_ms, FlushReport, OfflineQueue, ParentRef, PendingSave, Resolution, SyncReport, Tracked,
};
use crate::votes::VoteStore;

/// 이전 JSON 큐를 옮긴 결과
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// 보내지 못한 채 남아 있던 항목 (`queued`로 들어간다)
    pub queued: usize,
    /// 서버가 접수만 했고 결과를 기다리던 항목 (`pending`으로 들어간다. 원문과 벡터는 이전 파일에 없다)
    pub tracked: usize,
    /// 이미 같은 request_id가 있어서 건너뛴 항목
    pub skipped: usize,
}

pub struct Outbox {
    db: LocalDb,
    queue: OfflineQueue,
    /// 추천/비추천 투표와 판정 캐시 (같은 폴더의 다른 표, votes.rs)
    votes: VoteStore,
}

impl Outbox {
    /// 폴더를 열고, `queued`와 `pending` 행에서 큐를 되살린다.
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let dir = dir.as_ref();
        let db = LocalDb::open(dir).await?;
        // 정리는 실패해도 데이터는 그대로이므로 열기를 막지 않는다 (다음에 다시 시도한다)
        let _ = db.optimize().await;
        let queue = Self::rebuild(&db).await?;
        let votes = VoteStore::open(dir).await?;
        Ok(Self { db, queue, votes })
    }

    /// 투표 보관함과 판정 캐시
    pub fn votes(&self) -> &VoteStore {
        &self.votes
    }

    /// 작은 파일을 합치고 오래된 버전을 정리한다. 앱을 오래 켜 두는 경우 가끔 불러 준다.
    pub async fn maintain(&self) -> Result<(), AppError> {
        self.db.optimize().await
    }

    pub fn db(&self) -> &LocalDb {
        &self.db
    }

    /// 보내기를 기다리는 항목 수
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// 서버가 접수만 하고 최종 결과를 아직 모르는 항목 수
    pub fn awaiting_result(&self) -> usize {
        self.queue.awaiting_result()
    }

    async fn rebuild(db: &LocalDb) -> Result<OfflineQueue, AppError> {
        // 1. 아직 못 보낸 항목 (벡터가 있어야 다시 보낼 수 있다). 만든 순서, 같으면 번호 순서
        let mut queued_rows = db.by_state(LocalState::Queued, true).await?;
        queued_rows.sort_by_key(|r| (r.created_at, local_seq(&r.local_id).unwrap_or(0)));
        let mut pending = Vec::with_capacity(queued_rows.len());
        for r in &queued_rows {
            pending.push(pending_from_row(r)?);
        }

        // 2. 접수만 된 항목: 최종 결과를 계속 물어봐야 한다
        let mut tracking = Vec::new();
        let mut resolved: Vec<(String, Resolution)> = Vec::new();
        for r in db.by_state(LocalState::Pending, false).await? {
            let Some(id) = r.id.clone() else {
                return Err(AppError::Storage(format!(
                    "local row {} is pending but has no server id",
                    r.local_id
                )));
            };
            resolved.push((r.local_id.clone(), Resolution::Sent { id: id.clone() }));
            tracking.push((r.request_id.clone(), Tracked { local_id: r.local_id.clone(), id }));
        }

        // 3. 못 보낸 항목이 가리키는 로컬 부모 중, 큐 밖에서 이미 처리된 것의 결과
        let in_queue: std::collections::BTreeSet<&str> = queued_rows.iter().map(|r| r.local_id.as_str()).collect();
        let mut wanted: Vec<&str> = queued_rows
            .iter()
            .filter_map(|r| r.parent_local.as_deref())
            .filter(|p| !in_queue.contains(p))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        for parent in wanted {
            if let Some(row) = db.get_by_local_id(parent, false).await? {
                if let Some(res) = resolution_of(&row) {
                    resolved.push((row.local_id.clone(), res));
                }
            }
        }

        let next_seq = db.max_local_seq().await?;
        Ok(OfflineQueue::restore(next_seq, pending, tracking, resolved))
    }

    /// 저장 요청을 로컬 저장소에 먼저 쓰고 큐에 넣는다. 로컬 id를 돌려준다.
    /// 로컬 저장소에 쓰지 못하면 큐에도 넣지 않고 오류를 돌려준다.
    pub async fn enqueue(&mut self, req: SaveRequest, parent: Option<ParentRef>) -> Result<String, AppError> {
        // 부모가 큐 메모리에 없는 로컬 id면(앱을 다시 켠 뒤 이미 처리된 항목에 답글을 다는 경우) 저장소에서 결과를 찾아 알려 준다
        if let Some(ParentRef::Local(local)) = &parent {
            if self.queue.pending_item(local).is_none() {
                if let Some(row) = self.db.get_by_local_id(local, false).await? {
                    if let Some(res) = resolution_of(&row) {
                        self.queue.learn_resolution(local, res);
                    }
                }
            }
        }

        let local_id = self.queue.enqueue(req, parent)?;
        let item = self
            .queue
            .pending_item(&local_id)
            .cloned()
            .ok_or_else(|| AppError::Storage("enqueued item vanished from the queue".into()))?;
        let record = record_from_pending(&item);
        if let Err(err) = self.db.upsert(&record).await {
            self.queue.cancel_last(&local_id);
            return Err(err);
        }
        Ok(local_id)
    }

    /// 큐 앞에서부터 서버로 보내고, 결과를 해당 행에 반영한다.
    pub async fn flush<B: EvalBackend + ?Sized>(&mut self, backend: &B) -> FlushReport {
        let mut report = self.queue.flush(backend).await;
        let now = now_ms();
        let mut first_error: Option<String> = None;
        let mut note = |r: Result<(), AppError>| {
            if let Err(e) = r {
                first_error.get_or_insert(e.to_string());
            }
        };

        for sent in &report.sent {
            let state = match sent.response.status.as_deref() {
                Some("pending") | Some("processing") => LocalState::Pending,
                _ => LocalState::Stored,
            };
            note(
                self.db
                    .update_by_local_id(
                        &sent.local_id,
                        &[
                            ("id", SqlValue::Str(sent.response.id.clone())),
                            ("root", SqlValue::Str(sent.response.root.clone())),
                            ("ref", SqlValue::opt_str(sent.response.ref_.as_deref())),
                            ("from", SqlValue::Str(sent.response.from.clone())),
                            ("state", SqlValue::Str(state.as_str().into())),
                            ("reason", SqlValue::Null),
                            ("updated_at", SqlValue::Int(now)),
                        ],
                    )
                    .await,
            );
        }
        for rejected in &report.rejected {
            note(
                self.db
                    .update_by_local_id(
                        &rejected.local_id,
                        &[
                            ("state", SqlValue::Str(LocalState::Rejected.as_str().into())),
                            ("reason", SqlValue::Str(rejected.reason.clone())),
                            ("updated_at", SqlValue::Int(now)),
                        ],
                    )
                    .await,
            );
        }
        // 재시도 중인 항목의 시도 횟수
        let retrying: Vec<(String, u32)> = self
            .queue
            .pending_items()
            .filter(|p| p.attempts > 0)
            .map(|p| (p.local_id.clone(), p.attempts))
            .collect();
        for (local_id, attempts) in retrying {
            note(
                self.db
                    .update_by_local_id(
                        &local_id,
                        &[("attempts", SqlValue::Int(attempts as i64)), ("updated_at", SqlValue::Int(now))],
                    )
                    .await,
            );
        }

        report.persist_error = first_error;
        report
    }

    /// 서버가 접수만 한 항목의 최종 결과를 물어보고 행에 반영한다.
    pub async fn sync_status<B: EvalBackend + ?Sized>(&mut self, backend: &B) -> SyncReport {
        let mut report = self.queue.sync_status(backend).await;
        let now = now_ms();
        let mut first_error: Option<String> = None;
        let mut note = |r: Result<(), AppError>| {
            if let Err(e) = r {
                first_error.get_or_insert(e.to_string());
            }
        };

        for item in &report.stored {
            note(
                self.db
                    .update_by_local_id(
                        &item.local_id,
                        &[
                            ("state", SqlValue::Str(LocalState::Stored.as_str().into())),
                            ("updated_at", SqlValue::Int(now)),
                        ],
                    )
                    .await,
            );
        }
        for item in &report.rejected {
            note(
                self.db
                    .update_by_local_id(
                        &item.local_id,
                        &[
                            ("state", SqlValue::Str(LocalState::Rejected.as_str().into())),
                            ("reason", SqlValue::Str(item.reason.clone())),
                            ("updated_at", SqlValue::Int(now)),
                        ],
                    )
                    .await,
            );
        }
        for local_id in &report.missing {
            note(
                self.db
                    .update_by_local_id(
                        local_id,
                        &[
                            ("state", SqlValue::Str(LocalState::Missing.as_str().into())),
                            ("reason", SqlValue::Str("server record not found".into())),
                            ("updated_at", SqlValue::Int(now)),
                        ],
                    )
                    .await,
            );
        }

        report.persist_error = first_error;
        report
    }

    /// 내 글 검색 (로컬 벡터 검색). 질의 벡터는 서버 검색과 같은 방식으로 만들어 넘긴다.
    pub async fn search(&self, vector: &[f32], top_k: usize, cc: Option<&str>) -> Result<Vec<LocalMatch>, AppError> {
        self.db.search(vector, top_k, cc).await
    }

    pub async fn get(&self, request_id: &str) -> Result<Option<LocalRecord>, AppError> {
        self.db.get(request_id, false).await
    }

    /// since_ms 이후에 바뀐 행을 화면 캐시(Dexie) 형태로 돌려준다. 화면은 이 행을 그대로 `bulkPut`하면 된다.
    pub async fn cache_rows_since(&self, since_ms: i64) -> Result<Vec<Value>, AppError> {
        let mut rows = self.db.changed_since(since_ms).await?;
        rows.sort_by_key(|r| (r.updated_at, local_seq(&r.local_id).unwrap_or(0)));
        Ok(rows.iter().map(LocalRecord::to_cache_row).collect())
    }

    /// 이전 버전의 JSON 큐 파일을 로컬 저장소로 옮긴다. 같은 request_id가 이미 있으면 건너뛰므로 다시 실행해도 안전하다.
    /// - 못 보낸 항목은 `queued`로, 접수만 된 항목은 `pending`으로 들어간다.
    /// - 이전 파일에는 접수 완료된 항목의 원문과 벡터가 없다. 그런 행은 원문이 비어 있고 벡터가 없어서 검색에는 나오지 않는다.
    /// - 이전 파일의 `local-N` 번호는 이 저장소의 번호와 겹칠 수 있어서, 현재 가장 큰 번호만큼 밀어서 옮긴다.
    pub async fn import_json_queue(&mut self, path: &Path) -> Result<ImportReport, AppError> {
        let old = OfflineQueue::load_from(path)?;
        let offset = self.db.max_local_seq().await?;
        let remap = |local: &str| match local_seq(local) {
            Some(n) => format!("local-{}", n + offset),
            None => local.to_string(),
        };
        let now = now_ms();
        let mut report = ImportReport::default();

        for p in old.pending_items() {
            if self.db.get(&p.request_id, false).await?.is_some() {
                report.skipped += 1;
                continue;
            }
            let mut item = p.clone();
            item.local_id = remap(&p.local_id);
            if let Some(ParentRef::Local(parent)) = &p.parent {
                item.parent = Some(ParentRef::Local(remap(parent)));
            }
            if item.created_at == 0 {
                item.created_at = now;
            }
            self.db.upsert(&record_from_pending(&item)).await?;
            report.queued += 1;
        }

        for (request_id, tracked) in old.tracked_items() {
            if self.db.get(request_id, false).await?.is_some() {
                report.skipped += 1;
                continue;
            }
            let rec = LocalRecord {
                request_id: request_id.clone(),
                local_id: remap(&tracked.local_id),
                id: Some(tracked.id.clone()),
                root: None,
                ref_: None,
                parent_local: None,
                cc: String::new(),
                from: None,
                to: None,
                bcc: None,
                text: String::new(),
                text_hash: String::new(),
                vector: None,
                state: LocalState::Pending,
                attempts: 0,
                reason: None,
                created_at: now,
                updated_at: now,
            };
            self.db.upsert(&rec).await?;
            report.tracked += 1;
        }

        self.queue = Self::rebuild(&self.db).await?;
        Ok(report)
    }
}

/// 행이 가리키는 처리 결과. 부모로서 의미가 있는 것만 돌려준다.
fn resolution_of(row: &LocalRecord) -> Option<Resolution> {
    match row.state {
        LocalState::Stored | LocalState::Pending => row.id.clone().map(|id| Resolution::Sent { id }),
        // 서버가 기록을 지운 항목(Missing)도 부모로는 쓸 수 없다 (서버가 "부모를 찾을 수 없다"고 거부한다)
        LocalState::Rejected | LocalState::Failed | LocalState::Missing => Some(Resolution::Rejected),
        LocalState::Queued => None,
    }
}

fn pending_from_row(r: &LocalRecord) -> Result<PendingSave, AppError> {
    let vector = r.vector.clone().ok_or_else(|| {
        AppError::Storage(format!("local row {} is queued but has no vector", r.local_id))
    })?;
    let parent = match (&r.ref_, &r.parent_local) {
        (Some(id), _) => Some(ParentRef::Server(id.clone())),
        (None, Some(local)) => Some(ParentRef::Local(local.clone())),
        (None, None) => None,
    };
    Ok(PendingSave {
        local_id: r.local_id.clone(),
        request_id: r.request_id.clone(),
        cc: r.cc.clone(),
        text: r.text.clone(),
        text_hash: r.text_hash.clone(),
        vector,
        from: r.from.clone(),
        to: r.to.clone(),
        bcc: r.bcc.clone(),
        parent,
        attempts: r.attempts,
        created_at: r.created_at,
    })
}

fn record_from_pending(p: &PendingSave) -> LocalRecord {
    let (ref_, parent_local) = match &p.parent {
        Some(ParentRef::Server(id)) => (Some(id.clone()), None),
        Some(ParentRef::Local(local)) => (None, Some(local.clone())),
        None => (None, None),
    };
    LocalRecord {
        request_id: p.request_id.clone(),
        local_id: p.local_id.clone(),
        id: None,
        root: None,
        ref_,
        parent_local,
        cc: p.cc.clone(),
        from: p.from.clone(),
        to: p.to.clone(),
        bcc: p.bcc.clone(),
        text: p.text.clone(),
        text_hash: p.text_hash.clone(),
        vector: Some(p.vector.clone()),
        state: LocalState::Queued,
        attempts: p.attempts,
        reason: None,
        created_at: p.created_at,
        updated_at: p.created_at,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::api::{SearchRequest, SearchResponse, SaveResponse, StatusResponse};
    use crate::local::test_support::{axis, temp_dir};
    use crate::queue::test_support::{ScriptedBackend, Step};

    fn req(seed: usize) -> SaveRequest {
        let text = format!("평가 본문 {seed}");
        SaveRequest {
            bcc: None,
            request_id: format!("req-{seed:04}-aaaaaaaaaaaa"),
            from: None,
            to: None,
            cc: "example.com".into(),
            text_hash: crate::api::text_hash(&text),
            text,
            vector: axis(seed),
            ref_: None,
        }
    }

    /// 저장은 항상 pending(202)으로 받고, 상태 조회는 정해 둔 답을 차례로 돌려주는 백엔드
    struct PendingBackend {
        answers: Mutex<VecDeque<Result<(&'static str, Option<&'static str>), u16>>>,
        saved: Mutex<Vec<SaveRequest>>,
    }

    impl PendingBackend {
        fn new(answers: Vec<Result<(&'static str, Option<&'static str>), u16>>) -> Self {
            Self { answers: Mutex::new(answers.into()), saved: Mutex::new(vec![]) }
        }
    }

    #[async_trait::async_trait]
    impl EvalBackend for PendingBackend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            self.saved.lock().unwrap().push(req.clone());
            Ok(SaveResponse {
                id: format!("0190f0a0-0000-7000-8000-{:012}", self.saved.lock().unwrap().len()),
                root: format!("0190f0a0-0000-7000-8000-{:012}", 1),
                ref_: req.ref_.clone(),
                from: "0x00000000000000000000000000000000000000ab".into(),
                status: Some("pending".into()),
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, request_id: &str) -> Result<StatusResponse, AppError> {
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

    async fn row(outbox: &Outbox, seed: usize) -> LocalRecord {
        outbox.db().get(&req(seed).request_id, true).await.unwrap().expect("row exists")
    }

    #[tokio::test]
    async fn enqueue_writes_the_row_before_any_network_call() {
        let dir = temp_dir("ob-enqueue");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let local = ob.enqueue(req(1), None).await.unwrap();
        assert_eq!(local, "local-1");

        let r = row(&ob, 1).await;
        assert_eq!(r.state, LocalState::Queued);
        assert_eq!(r.text, "평가 본문 1", "원문이 로컬에 남는다");
        assert_eq!(r.vector.as_deref(), Some(axis(1).as_slice()));
        assert!(r.created_at > 0 && r.created_at == r.updated_at, "밀리초 시각이 기록된다");
        assert!(r.id.is_none() && r.root.is_none() && r.from.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_reopened_outbox_restores_the_queue_in_order_and_resends() {
        let dir = temp_dir("ob-reopen");
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            ob.enqueue(req(1), None).await.unwrap();
            ob.enqueue(req(2), None).await.unwrap();
            ob.enqueue(req(3), None).await.unwrap();
            let offline = ScriptedBackend::with_steps(vec![Step::Offline]);
            let r = ob.flush(&offline).await;
            assert!(r.stopped.is_some() && r.persist_error.is_none());
            assert_eq!(ob.queued(), 3);
        } // 앱 종료

        let mut ob = Outbox::open(&dir).await.unwrap();
        assert_eq!(ob.queued(), 3, "시작할 때 행에서 큐를 되살린다");
        let backend = ScriptedBackend::default();
        let report = ob.flush(&backend).await;
        assert_eq!(report.sent.len(), 3);
        assert_eq!(ob.queued(), 0);

        let order: Vec<String> = backend.saved.lock().unwrap().iter().map(|s| s.text.clone()).collect();
        assert_eq!(order, ["평가 본문 1", "평가 본문 2", "평가 본문 3"], "만든 순서대로 보낸다");
        // 번호는 이어서 붙는다
        assert_eq!(ob.enqueue(req(4), None).await.unwrap(), "local-4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_records_the_server_fields_on_the_row() {
        let dir = temp_dir("ob-flush");
        let mut ob = Outbox::open(&dir).await.unwrap();
        ob.enqueue(req(1), None).await.unwrap();
        let backend = ScriptedBackend::default();
        let report = ob.flush(&backend).await;
        assert!(report.persist_error.is_none());

        let r = row(&ob, 1).await;
        assert_eq!(r.state, LocalState::Stored);
        assert_eq!(r.id.as_deref(), Some("srv-1"));
        assert_eq!(r.root.as_deref(), Some("srv-1"));
        assert_eq!(r.from.as_deref(), Some("0x00000000000000000000000000000000000000ab"), "서버가 정한 from을 받아 둔다");
        assert!(r.updated_at >= r.created_at);
        assert_eq!(r.text, "평가 본문 1", "저장된 뒤에도 원문은 로컬에 남는다");
        assert!(r.vector.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pending_rows_are_tracked_across_restart_until_stored() {
        let dir = temp_dir("ob-pending");
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            ob.enqueue(req(1), None).await.unwrap();
            ob.flush(&PendingBackend::new(vec![])).await;
            assert_eq!(row(&ob, 1).await.state, LocalState::Pending);
            assert_eq!((ob.queued(), ob.awaiting_result()), (0, 1));
        }

        let mut ob = Outbox::open(&dir).await.unwrap();
        assert_eq!((ob.queued(), ob.awaiting_result()), (0, 1), "재시작 뒤에도 결과를 계속 물어본다");
        let report = ob.sync_status(&PendingBackend::new(vec![Ok(("stored", None))])).await;
        assert_eq!(report.stored.len(), 1);
        assert!(report.persist_error.is_none());
        assert_eq!(row(&ob, 1).await.state, LocalState::Stored);
        assert_eq!(ob.awaiting_result(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_later_rejection_is_recorded_and_still_blocks_replies_after_restart() {
        let dir = temp_dir("ob-rejected");
        let root_local;
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            root_local = ob.enqueue(req(1), None).await.unwrap();
            ob.flush(&PendingBackend::new(vec![])).await;
            let report = ob.sync_status(&PendingBackend::new(vec![Ok(("rejected", Some("profanity")))])).await;
            assert_eq!(report.rejected.len(), 1);
            let r = row(&ob, 1).await;
            assert_eq!(r.state, LocalState::Rejected);
            assert!(r.reason.as_deref().is_some_and(|x| x.contains("profanity")));
            assert_eq!(r.text, "평가 본문 1", "거부된 글의 원문은 내가 고칠 수 있게 남는다");
        }

        let mut ob = Outbox::open(&dir).await.unwrap();
        let late = ob.enqueue(req(2), Some(ParentRef::Local(root_local))).await;
        assert!(matches!(late, Err(AppError::Rejected(_))), "{late:?}");
        assert!(ob.db().get(&req(2).request_id, false).await.unwrap().is_none(), "거부된 부모의 답글은 저장하지 않는다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_reply_to_a_queued_parent_is_sent_with_the_server_id_after_restart() {
        let dir = temp_dir("ob-reply-queued");
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            let root = ob.enqueue(req(1), None).await.unwrap();
            ob.enqueue(req(2), Some(ParentRef::Local(root))).await.unwrap();
            ob.flush(&ScriptedBackend::with_steps(vec![Step::Offline])).await;
            let reply = row(&ob, 2).await;
            assert_eq!(reply.parent_local.as_deref(), Some("local-1"));
            assert!(reply.ref_.is_none());
        }

        let mut ob = Outbox::open(&dir).await.unwrap();
        let backend = ScriptedBackend::default();
        let report = ob.flush(&backend).await;
        assert_eq!(report.sent.len(), 2);
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].ref_, None);
        assert_eq!(saved[1].ref_.as_deref(), Some("srv-1"), "부모의 서버 id로 바뀌어 전송된다");
        drop(saved);
        assert_eq!(row(&ob, 2).await.ref_.as_deref(), Some("srv-1"), "행에도 서버 부모 id가 기록된다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_reply_to_an_already_stored_parent_finds_it_in_the_local_db() {
        let dir = temp_dir("ob-reply-stored");
        let root_local;
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            root_local = ob.enqueue(req(1), None).await.unwrap();
            ob.flush(&ScriptedBackend::default()).await;
        }

        let mut ob = Outbox::open(&dir).await.unwrap();
        ob.enqueue(req(2), Some(ParentRef::Local(root_local))).await.unwrap();
        let reply = row(&ob, 2).await;
        assert_eq!(reply.ref_.as_deref(), Some("srv-1"), "저장된 부모는 서버 id로 바로 바뀐다");
        assert!(reply.parent_local.is_none());

        let backend = ScriptedBackend::default();
        ob.flush(&backend).await;
        assert_eq!(backend.saved.lock().unwrap()[0].ref_.as_deref(), Some("srv-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_unknown_local_parent_is_refused_without_writing_a_row() {
        let dir = temp_dir("ob-unknown-parent");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let err = ob.enqueue(req(1), Some(ParentRef::Local("local-99".into()))).await;
        assert!(matches!(err, Err(AppError::InvalidInput(_))), "{err:?}");
        assert_eq!(ob.db().count().await.unwrap(), 0);
        assert_eq!(ob.queued(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_crash_after_the_server_accepted_resends_with_the_same_request_id() {
        let dir = temp_dir("ob-crash");
        let backend = ScriptedBackend::default();
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            ob.enqueue(req(1), None).await.unwrap();
            // 서버는 받았지만(저장됨) 행을 갱신하기 전에 앱이 죽은 상황
            let rec = row(&ob, 1).await;
            let sent = SaveRequest {
                bcc: None,
                request_id: rec.request_id.clone(),
                from: rec.from.clone(),
                to: rec.to.clone(),
                cc: rec.cc.clone(),
                text: rec.text.clone(),
                text_hash: rec.text_hash.clone(),
                vector: rec.vector.clone().unwrap(),
                ref_: None,
            };
            backend.save(&sent).await.unwrap();
            assert_eq!(row(&ob, 1).await.state, LocalState::Queued, "행은 아직 queued다");
        }

        let mut ob = Outbox::open(&dir).await.unwrap();
        assert_eq!(ob.queued(), 1);
        let report = ob.flush(&backend).await;
        assert_eq!(report.sent.len(), 1);
        assert_eq!(backend.stored_count(), 1, "서버는 같은 request_id를 한 번만 저장했다");
        let r = row(&ob, 1).await;
        assert_eq!((r.state, r.id.as_deref()), (LocalState::Stored, Some("srv-1")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn retry_attempts_are_persisted_and_restored() {
        let dir = temp_dir("ob-attempts");
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            ob.enqueue(req(1), None).await.unwrap();
            let report = ob.flush(&ScriptedBackend::with_steps(vec![Step::Status(503)])).await;
            assert!(report.stopped.is_some());
            assert_eq!(row(&ob, 1).await.attempts, 1);
        }
        let ob = Outbox::open(&dir).await.unwrap();
        assert_eq!(row(&ob, 1).await.attempts, 1);
        assert_eq!(ob.queued(), 1, "아직 포기하지 않았다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_permanent_rejection_marks_the_row_and_its_reply() {
        let dir = temp_dir("ob-permanent");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let root = ob.enqueue(req(1), None).await.unwrap();
        ob.enqueue(req(2), Some(ParentRef::Local(root))).await.unwrap();
        let report = ob.flush(&ScriptedBackend::with_steps(vec![Step::Status(400)])).await;
        assert_eq!(report.rejected.len(), 2);
        for seed in [1, 2] {
            let r = row(&ob, seed).await;
            assert_eq!(r.state, LocalState::Rejected, "seed {seed}");
            assert!(r.reason.is_some());
        }
        assert_eq!(ob.queued(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn not_found_marks_the_row_missing() {
        let dir = temp_dir("ob-missing");
        let mut ob = Outbox::open(&dir).await.unwrap();
        ob.enqueue(req(1), None).await.unwrap();
        ob.flush(&PendingBackend::new(vec![])).await;
        let report = ob.sync_status(&PendingBackend::new(vec![Err(404)])).await;
        assert_eq!(report.missing.len(), 1);
        assert_eq!(row(&ob, 1).await.state, LocalState::Missing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_local_write_leaves_nothing_in_the_queue() {
        let dir = temp_dir("ob-writefail");
        let mut ob = Outbox::open(&dir).await.unwrap();
        // 벡터 차원이 틀리면 로컬 저장소가 쓰기를 거절한다
        let mut bad = req(1);
        bad.vector = vec![0.5; 4];
        let err = ob.enqueue(bad, None).await;
        assert!(matches!(err, Err(AppError::InvalidInput(_))), "{err:?}");
        assert_eq!(ob.queued(), 0, "저장되지 않은 항목은 큐에도 남기지 않는다");
        assert_eq!(ob.enqueue(req(2), None).await.unwrap(), "local-1", "번호도 되돌린다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn local_search_finds_my_own_text_even_after_it_was_stored() {
        let dir = temp_dir("ob-search");
        let mut ob = Outbox::open(&dir).await.unwrap();
        for seed in [1, 2, 3] {
            ob.enqueue(req(seed), None).await.unwrap();
        }
        ob.flush(&ScriptedBackend::default()).await; // 모두 stored

        let hits = ob.search(&axis(2), 2, None).await.unwrap();
        assert_eq!(hits[0].text, "평가 본문 2");
        assert!((hits[0].score - 1.0).abs() < 1e-4);
        assert_eq!(hits[0].state, LocalState::Stored);
        assert_eq!(hits[0].id.as_deref(), Some("srv-2"));
        assert_eq!(hits.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cache_rows_since_gives_dexie_shaped_rows_in_order() {
        let dir = temp_dir("ob-cache");
        let mut ob = Outbox::open(&dir).await.unwrap();
        ob.enqueue(req(1), None).await.unwrap();
        ob.enqueue(req(2), None).await.unwrap();

        let all = ob.cache_rows_since(0).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["request_id"], req(1).request_id.as_str());
        for row in &all {
            assert_eq!(row["id"], "", "접수 전에는 빈 문자열");
            assert_eq!(row["to"], "");
            assert_eq!(row["bcc"], "");
            assert_eq!(row["state"], "queued");
            assert!(row.get("vector").is_none());
        }

        let cut = all[1]["updated_at"].as_i64().unwrap();
        ob.flush(&ScriptedBackend::default()).await;
        let changed = ob.cache_rows_since(cut).await.unwrap();
        assert!(!changed.is_empty(), "flush로 바뀐 행이 나온다");
        assert!(changed.iter().all(|r| r["state"] == "stored" && r["id"] != ""));
        assert!(changed.iter().all(|r| r["updated_at"].as_i64().unwrap() > cut));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn importing_the_old_json_queue_moves_queued_and_tracked_items_once() {
        let dir = temp_dir("ob-import");
        let json = std::env::temp_dir().join(format!("pd-import-{}.json", std::process::id()));
        {
            // 이전 방식: JSON 큐. 하나는 못 보냈고(오프라인), 하나는 서버가 접수만 했다
            let mut old = OfflineQueue::default();
            let tracked_local = old.enqueue(req(10), None).unwrap();
            old.flush(&PendingBackend::new(vec![])).await;
            let root = old.enqueue(req(11), None).unwrap();
            old.enqueue(req(12), Some(ParentRef::Local(root))).unwrap();
            old.flush(&ScriptedBackend::with_steps(vec![Step::Offline])).await;
            assert_eq!((old.len(), old.awaiting_result()), (2, 1));
            let _ = tracked_local;
            old.save_to(&json).unwrap();
        }

        let mut ob = Outbox::open(&dir).await.unwrap();
        // 이미 새 저장소에 항목이 있어서 번호가 겹칠 수 있는 상황
        ob.enqueue(req(1), None).await.unwrap();
        ob.flush(&ScriptedBackend::default()).await;

        let report = ob.import_json_queue(&json).await.unwrap();
        assert_eq!(report, ImportReport { queued: 2, tracked: 1, skipped: 0 });
        assert_eq!((ob.queued(), ob.awaiting_result()), (2, 1));

        // 접수만 된 항목: 원문과 벡터는 이전 파일에 없다
        let tracked = ob.db().get(&req(10).request_id, true).await.unwrap().unwrap();
        assert_eq!(tracked.state, LocalState::Pending);
        assert!(tracked.vector.is_none() && tracked.text.is_empty() && tracked.id.is_some());

        // 못 보낸 항목: 벡터와 원문이 그대로 있고, 번호가 겹치지 않으며 답글의 부모 지정이 따라 바뀐다
        let root = ob.db().get(&req(11).request_id, true).await.unwrap().unwrap();
        let reply = ob.db().get(&req(12).request_id, true).await.unwrap().unwrap();
        assert_eq!(root.vector.as_deref(), Some(axis(11).as_slice()));
        assert_ne!(root.local_id, "local-1");
        assert_eq!(reply.parent_local.as_deref(), Some(root.local_id.as_str()));
        let mut ids = vec![tracked.local_id.clone(), root.local_id.clone(), reply.local_id.clone(), "local-1".to_string()];
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 4, "로컬 번호가 모두 다르다");

        // 다시 실행해도 중복되지 않는다
        let again = ob.import_json_queue(&json).await.unwrap();
        assert_eq!(again, ImportReport { queued: 0, tracked: 0, skipped: 3 });
        assert_eq!(ob.db().count().await.unwrap(), 4);

        // 옮긴 뒤 보내기가 동작한다. 답글은 부모의 서버 id를 달고 간다
        let backend = ScriptedBackend::default();
        let sent = ob.flush(&backend).await;
        assert_eq!(sent.sent.len(), 2);
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].text, "평가 본문 11");
        assert!(saved[1].ref_.is_some());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&json);
    }

    #[tokio::test]
    async fn a_missing_json_file_imports_nothing() {
        let dir = temp_dir("ob-import-none");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let none = std::env::temp_dir().join("pd-no-such-queue-file.json");
        assert_eq!(ob.import_json_queue(&none).await.unwrap(), ImportReport::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
