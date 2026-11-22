//! 추천/비추천 투표 보관함과 전역 판정 캐시 (LanceDB 표 두 개, 평가 보관함과 같은 폴더).
//!
//! - `votes_local`: 투표 한 건이 한 행이다. 보내기 전(`queued`)부터 보낸 뒤(`sent`)까지 상태만 바뀐다.
//!   (평가, 종류)마다 아직 못 보낸 표는 하나만 둔다. 오프라인에서 눌렀다 바꿔도 서버는 마지막 값 하나만 받는다.
//!   바꿀 때는 행을 고치지 않고 새 request_id로 새 행을 만든다: 옛 요청이 사실은 서버에 닿았는데 응답만 잃었다면,
//!   같은 request_id에 다른 값을 보내는 순간 서버가 409로 거부하기 때문이다. 서버는 나중 표를 유효한 표로 본다.
//! - `verdicts_cache`: 서버가 계산한 판정(GET /evaluations/verdicts)을 그대로 둔다. 앱은 판정을 다시 계산하지 않는다.
//!   판정 규칙은 서버 cron과 raon-zena가 계산하고, 계약은 contracts/verdict_v1.json이다.
//!
//! 재시도 규칙은 평가 보관함(`queue.rs` 의 [`crate::queue::SyncFailure`] 다섯 갈래)과 같고, 다른 점은 두 가지다.
//! - 투표끼리는 순서가 없다. 일시 오류는 그 항목만 미루고 다음 항목을 보낸다. 연결이 없거나, 401 · 403(다시 로그인할
//!   때까지) · 429(Retry-After 뒤까지)일 때만 멈춘다. 이 셋은 시도 횟수를 세지 않는다(예전에는 50초 만에 실패로 끝냈다).
//! - 본문이 "evaluation not found"인 404는 평가가 서버에 없다는 뜻이라 바로 거부한다. 다른 404는 설정 문제일 수 있어 재시도한다.
//!
//! 못 보낸 표는 [`MAX_QUEUED_VOTES`] 까지 둔다. 넘치면 가장 오래된 결과 표(`vote_outcome`, 자동)부터 지우고,
//! 사용자가 직접 누른 표(`vote`)는 지우지 않는다(그것만 남았으면 새 표를 받지 않는다: queue_full).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use futures::TryStreamExt;
use lancedb::arrow::arrow_array::{
    Array, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use lancedb::arrow::arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Connection, Table};
use serde::Serialize;

use crate::api::{new_request_id, EvalBackend, VerdictDto, VoteRequest, VoteResponse};
use crate::error::AppError;
use crate::local::{signature, sql_str, storage};
use crate::queue::{classify_failure, now_ms, SyncFailure, MAX_SERVER_ATTEMPTS};

pub const VOTES_TABLE: &str = "votes_local";
/// 못 보낸 표의 상한 (서로 다른 평가 수. 같은 (평가, 종류)는 이미 한 행으로 합쳐진다)
pub const MAX_QUEUED_VOTES: usize = 2_000;
pub const VERDICTS_TABLE: &str = "verdicts_cache";
/// 캐시에 없던 평가를 대체로 표시할 때 적는 판정 규칙 (서버 cron 의 RECIPE 와 같음)
const SUPERSEDED_RECIPE: &str = "pd-verdict/1";
/// 판정 캐시를 서버에 다시 묻기까지의 시간 (밀리초)
pub const VERDICT_TTL_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteKind {
    /// 사용자가 직접 누른 추천/비추천
    Vote,
    /// 결과로 확인된 표 (raon-zena가 동의를 받고 보낸다)
    VoteOutcome,
}

impl VoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VoteKind::Vote => "vote",
            VoteKind::VoteOutcome => "vote_outcome",
        }
    }

    pub fn parse(s: &str) -> Result<Self, AppError> {
        match s {
            "vote" => Ok(VoteKind::Vote),
            "vote_outcome" => Ok(VoteKind::VoteOutcome),
            other => Err(AppError::InvalidInput(format!("kind must be vote or vote_outcome, got {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteState {
    /// 아직 보내지 못했다
    Queued,
    /// 서버가 받았다
    Sent,
    /// 서버가 영구적으로 거부했다 (평가 없음, 형식 오류, 충돌)
    Rejected,
    /// 재시도 한도를 넘겼다
    Failed,
}

impl VoteState {
    pub fn as_str(self) -> &'static str {
        match self {
            VoteState::Queued => "queued",
            VoteState::Sent => "sent",
            VoteState::Rejected => "rejected",
            VoteState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Result<Self, AppError> {
        Ok(match s {
            "queued" => VoteState::Queued,
            "sent" => VoteState::Sent,
            "rejected" => VoteState::Rejected,
            "failed" => VoteState::Failed,
            other => return Err(AppError::Storage(format!("unknown vote state: {other}"))),
        })
    }
}

/// `votes_local` 한 행
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoteRecord {
    pub request_id: String,
    /// 평가의 서버 id
    pub eval_id: String,
    pub kind: VoteKind,
    /// 1 추천 | -1 비추천 | 0 철회
    pub value: i32,
    pub state: VoteState,
    /// 서버가 판정에 넣는 표인지 (보낸 뒤에만 안다)
    pub counted: Option<bool>,
    pub attempts: u32,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 거부되거나 실패한 표
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoteRejected {
    pub request_id: String,
    pub eval_id: String,
    pub reason: String,
}

/// 투표 보내기 결과
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VoteFlushReport {
    pub sent: Vec<(String, VoteResponse)>,
    pub rejected: Vec<VoteRejected>,
    /// 아직 보내지 못한 표
    pub remaining: usize,
    /// 연결이 없어 멈춘 이유
    pub stopped: Option<String>,
    /// 401 · 403 으로 멈췄다: 다시 로그인할 때까지 보내지 않는다
    pub auth_required: bool,
    /// 429 로 멈췄다: 이만큼 뒤에 다시 보낸다 (밀리초)
    pub retry_after_ms: Option<u64>,
    /// 로컬 저장소에 상태를 쓰지 못한 이유
    pub persist_error: Option<String>,
}

/// 판정 캐시 한 건: 서버 값 + 받은 시각
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CachedVerdict {
    #[serde(flatten)]
    pub verdict: VerdictDto,
    pub fetched_at: i64,
}

fn votes_schema() -> SchemaRef {
    let text = |name: &str, nullable: bool| Field::new(name, DataType::Utf8, nullable);
    Arc::new(Schema::new(vec![
        text("request_id", false),
        text("eval_id", false),
        text("kind", false),
        Field::new("value", DataType::Int32, false),
        text("state", false),
        Field::new("counted", DataType::Boolean, true),
        Field::new("attempts", DataType::Int32, false),
        text("reason", true),
        Field::new("created_at", DataType::Int64, false),
        Field::new("updated_at", DataType::Int64, false),
    ]))
}

fn verdicts_schema() -> SchemaRef {
    let text = |name: &str, nullable: bool| Field::new(name, DataType::Utf8, nullable);
    Arc::new(Schema::new(vec![
        text("eval_id", false),
        text("tier", false),
        Field::new("grade", DataType::Int32, true),
        Field::new("rank", DataType::Float64, true),
        Field::new("value", DataType::Float64, false),
        Field::new("evidence", DataType::Float64, false),
        Field::new("up", DataType::Int32, false),
        Field::new("down", DataType::Int32, false),
        text("level", true),
        // 이유 코드 JSON 배열
        text("reasons", false),
        text("recipe", false),
        Field::new("server_updated_at", DataType::Int64, true),
        Field::new("fetched_at", DataType::Int64, false),
    ]))
}

async fn open_table(conn: &Connection, name: &str, schema: SchemaRef) -> Result<Table, AppError> {
    let names = conn.table_names().execute().await.map_err(storage)?;
    if names.iter().any(|n| n == name) {
        let table = conn.open_table(name).execute().await.map_err(storage)?;
        let existing = table.schema().await.map_err(storage)?;
        if signature(&existing) != signature(&schema) {
            return Err(AppError::Storage(format!(
                "local table {name} has an unexpected schema (expected {} columns, found {})",
                schema.fields().len(),
                existing.fields().len()
            )));
        }
        Ok(table)
    } else {
        conn.create_empty_table(name, schema).execute().await.map_err(storage)
    }
}

fn col<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T, AppError> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<T>())
        .ok_or_else(|| AppError::Storage(format!("local table is missing column {name}")))
}

fn opt_str(arr: &StringArray, i: usize) -> Option<String> {
    (!arr.is_null(i)).then(|| arr.value(i).to_string())
}

fn read_votes(batches: &[RecordBatch]) -> Result<Vec<VoteRecord>, AppError> {
    let mut out = Vec::new();
    for b in batches {
        let request_id = col::<StringArray>(b, "request_id")?;
        let eval_id = col::<StringArray>(b, "eval_id")?;
        let kind = col::<StringArray>(b, "kind")?;
        let value = col::<Int32Array>(b, "value")?;
        let state = col::<StringArray>(b, "state")?;
        let counted = col::<BooleanArray>(b, "counted")?;
        let attempts = col::<Int32Array>(b, "attempts")?;
        let reason = col::<StringArray>(b, "reason")?;
        let created_at = col::<Int64Array>(b, "created_at")?;
        let updated_at = col::<Int64Array>(b, "updated_at")?;
        for i in 0..b.num_rows() {
            out.push(VoteRecord {
                request_id: request_id.value(i).to_string(),
                eval_id: eval_id.value(i).to_string(),
                kind: VoteKind::parse(kind.value(i)).map_err(|e| AppError::Storage(e.to_string()))?,
                value: value.value(i),
                state: VoteState::parse(state.value(i))?,
                counted: (!counted.is_null(i)).then(|| counted.value(i)),
                attempts: attempts.value(i).max(0) as u32,
                reason: opt_str(reason, i),
                created_at: created_at.value(i),
                updated_at: updated_at.value(i),
            });
        }
    }
    Ok(out)
}

fn votes_batch(records: &[VoteRecord]) -> Result<RecordBatch, AppError> {
    let strs = |f: &dyn Fn(&VoteRecord) -> Option<&str>| -> Arc<dyn Array> {
        Arc::new(StringArray::from_iter(records.iter().map(|r| f(r))))
    };
    let columns: Vec<Arc<dyn Array>> = vec![
        strs(&|r| Some(r.request_id.as_str())),
        strs(&|r| Some(r.eval_id.as_str())),
        strs(&|r| Some(r.kind.as_str())),
        Arc::new(Int32Array::from_iter_values(records.iter().map(|r| r.value))),
        strs(&|r| Some(r.state.as_str())),
        Arc::new(BooleanArray::from(records.iter().map(|r| r.counted).collect::<Vec<_>>())),
        Arc::new(Int32Array::from_iter_values(records.iter().map(|r| r.attempts as i32))),
        strs(&|r| r.reason.as_deref()),
        Arc::new(Int64Array::from_iter_values(records.iter().map(|r| r.created_at))),
        Arc::new(Int64Array::from_iter_values(records.iter().map(|r| r.updated_at))),
    ];
    RecordBatch::try_new(votes_schema(), columns).map_err(storage)
}

fn read_verdicts(batches: &[RecordBatch]) -> Result<Vec<CachedVerdict>, AppError> {
    let mut out = Vec::new();
    for b in batches {
        let eval_id = col::<StringArray>(b, "eval_id")?;
        let tier = col::<StringArray>(b, "tier")?;
        let grade = col::<Int32Array>(b, "grade")?;
        let rank = col::<Float64Array>(b, "rank")?;
        let value = col::<Float64Array>(b, "value")?;
        let evidence = col::<Float64Array>(b, "evidence")?;
        let up = col::<Int32Array>(b, "up")?;
        let down = col::<Int32Array>(b, "down")?;
        let level = col::<StringArray>(b, "level")?;
        let reasons = col::<StringArray>(b, "reasons")?;
        let recipe = col::<StringArray>(b, "recipe")?;
        let server_updated_at = col::<Int64Array>(b, "server_updated_at")?;
        let fetched_at = col::<Int64Array>(b, "fetched_at")?;
        for i in 0..b.num_rows() {
            let reasons: Vec<String> = serde_json::from_str(reasons.value(i))
                .map_err(|e| AppError::Storage(format!("verdict reasons are not a JSON array: {e}")))?;
            out.push(CachedVerdict {
                verdict: VerdictDto {
                    id: eval_id.value(i).to_string(),
                    tier: tier.value(i).to_string(),
                    grade: (!grade.is_null(i)).then(|| grade.value(i)),
                    rank: (!rank.is_null(i)).then(|| rank.value(i)),
                    value: value.value(i),
                    evidence: evidence.value(i),
                    up: up.value(i).max(0) as u32,
                    down: down.value(i).max(0) as u32,
                    level: opt_str(level, i),
                    reasons,
                    recipe: recipe.value(i).to_string(),
                    updated_at: (!server_updated_at.is_null(i)).then(|| server_updated_at.value(i)),
                },
                fetched_at: fetched_at.value(i),
            });
        }
    }
    Ok(out)
}

fn verdicts_batch(verdicts: &[VerdictDto], fetched_at: i64) -> Result<RecordBatch, AppError> {
    let reasons = verdicts
        .iter()
        .map(|v| serde_json::to_string(&v.reasons).map_err(|e| AppError::Storage(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    let columns: Vec<Arc<dyn Array>> = vec![
        Arc::new(StringArray::from_iter_values(verdicts.iter().map(|v| v.id.as_str()))),
        Arc::new(StringArray::from_iter_values(verdicts.iter().map(|v| v.tier.as_str()))),
        Arc::new(Int32Array::from(verdicts.iter().map(|v| v.grade).collect::<Vec<_>>())),
        Arc::new(Float64Array::from(verdicts.iter().map(|v| v.rank).collect::<Vec<_>>())),
        Arc::new(Float64Array::from_iter_values(verdicts.iter().map(|v| v.value))),
        Arc::new(Float64Array::from_iter_values(verdicts.iter().map(|v| v.evidence))),
        Arc::new(Int32Array::from_iter_values(verdicts.iter().map(|v| v.up as i32))),
        Arc::new(Int32Array::from_iter_values(verdicts.iter().map(|v| v.down as i32))),
        Arc::new(StringArray::from_iter(verdicts.iter().map(|v| v.level.as_deref()))),
        Arc::new(StringArray::from_iter_values(reasons.iter().map(String::as_str))),
        Arc::new(StringArray::from_iter_values(verdicts.iter().map(|v| v.recipe.as_str()))),
        Arc::new(Int64Array::from(verdicts.iter().map(|v| v.updated_at).collect::<Vec<_>>())),
        Arc::new(Int64Array::from_iter_values(verdicts.iter().map(|_| fetched_at))),
    ];
    RecordBatch::try_new(verdicts_schema(), columns).map_err(storage)
}

fn in_list(column: &str, values: &[String]) -> String {
    let quoted: Vec<String> = values.iter().map(|v| sql_str(v)).collect();
    format!("{column} IN ({})", quoted.join(", "))
}

/// 404 본문이 "평가 없음"인지 (서버 workers/www/src/evaluations.ts의 메시지)
fn is_missing_evaluation(status: u16, message: &str) -> bool {
    crate::queue::names_missing_target(status, message)
}

pub struct VoteStore {
    votes: Table,
    verdicts: Table,
    /// 못 보낸 표의 상한 ([`MAX_QUEUED_VOTES`], 시험에서만 줄인다)
    max_queued: usize,
}

impl VoteStore {
    /// 폴더를 열고(없으면 만들고) 두 표를 준비한다. 컬럼이 이 버전과 다르면 오류로 멈춘다 (평가 표와 같은 규칙).
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(storage)?;
        let uri = dir
            .to_str()
            .ok_or_else(|| AppError::Storage("local db path is not valid UTF-8".into()))?;
        let conn = connect(uri).execute().await.map_err(storage)?;
        Ok(Self {
            votes: open_table(&conn, VOTES_TABLE, votes_schema()).await?,
            verdicts: open_table(&conn, VERDICTS_TABLE, verdicts_schema()).await?,
            max_queued: MAX_QUEUED_VOTES,
        })
    }

    #[cfg(test)]
    fn with_cap(mut self, max_queued: usize) -> Self {
        self.max_queued = max_queued;
        self
    }

    async fn fetch_votes(&self, filter: String) -> Result<Vec<VoteRecord>, AppError> {
        let batches: Vec<RecordBatch> = self
            .votes
            .query()
            .only_if(filter)
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        let mut rows = read_votes(&batches)?;
        rows.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.request_id.cmp(&b.request_id)));
        Ok(rows)
    }

    async fn upsert_vote(&self, rec: &VoteRecord) -> Result<(), AppError> {
        self.upsert_votes(std::slice::from_ref(rec)).await
    }

    async fn upsert_votes(&self, recs: &[VoteRecord]) -> Result<(), AppError> {
        if recs.is_empty() {
            return Ok(());
        }
        let batch = votes_batch(recs)?;
        let reader = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], votes_schema());
        let mut merge = self.votes.merge_insert(&["request_id"]);
        merge.when_matched_update_all(None).when_not_matched_insert_all();
        merge.execute(Box::new(reader)).await.map_err(storage)?;
        Ok(())
    }

    /// 표를 남긴다. 같은 (평가, 종류)에 아직 못 보낸 표가 있으면 그 행을 지우고 새 request_id로 새 행을 만든다.
    /// 못 보낸 표가 [`MAX_QUEUED_VOTES`] 에 닿았으면 가장 오래된 결과 표(vote_outcome)를 지워 자리를 만든다.
    /// 직접 누른 표만 남았으면 받지 않는다 (queue_full).
    pub async fn put_vote(&self, eval_id: &str, kind: VoteKind, value: i32) -> Result<VoteRecord, AppError> {
        if !matches!(value, -1..=1) {
            return Err(AppError::InvalidInput(format!("value must be 1, -1 or 0, got {value}")));
        }
        let now = now_ms();
        self.votes
            .delete(&format!(
                "eval_id = {} AND kind = {} AND state = {}",
                sql_str(eval_id),
                sql_str(kind.as_str()),
                sql_str(VoteState::Queued.as_str())
            ))
            .await
            .map_err(storage)?;
        let queued = self.queued_count().await?;
        if queued >= self.max_queued {
            let oldest_outcomes = self
                .fetch_votes(format!(
                    "state = {} AND kind = {}",
                    sql_str(VoteState::Queued.as_str()),
                    sql_str(VoteKind::VoteOutcome.as_str())
                ))
                .await?;
            let need = queued + 1 - self.max_queued;
            if oldest_outcomes.len() < need {
                return Err(AppError::QueueFull(queued));
            }
            // fetch_votes 는 created_at 순이다: 앞쪽이 가장 오래된 표
            let ids: Vec<String> = oldest_outcomes.into_iter().take(need).map(|r| r.request_id).collect();
            self.votes.delete(&in_list("request_id", &ids)).await.map_err(storage)?;
        }
        let rec = VoteRecord {
            request_id: new_request_id(),
            eval_id: eval_id.to_string(),
            kind,
            value,
            state: VoteState::Queued,
            counted: None,
            attempts: 0,
            reason: None,
            created_at: now,
            updated_at: now,
        };
        self.upsert_vote(&rec).await?;
        Ok(rec)
    }

    pub async fn queued(&self) -> Result<Vec<VoteRecord>, AppError> {
        self.fetch_votes(format!("state = {}", sql_str(VoteState::Queued.as_str()))).await
    }

    /// 못 보낸 표의 수 (행을 읽지 않고 센다). 보내기 없이도 SyncLoop 가 투표만 남은 상태를 알 수 있게 한다
    pub async fn queued_count(&self) -> Result<usize, AppError> {
        self.votes
            .count_rows(Some(format!("state = {}", sql_str(VoteState::Queued.as_str()))))
            .await
            .map_err(storage)
    }

    pub async fn get(&self, request_id: &str) -> Result<Option<VoteRecord>, AppError> {
        Ok(self.fetch_votes(format!("request_id = {}", sql_str(request_id))).await?.into_iter().next())
    }

    /// 평가마다 이 기기의 유효한 직접 투표 (1 | -1). 철회(0)했거나, 거부·실패한 표는 빠진다
    pub async fn my_votes(&self, eval_ids: &[String]) -> Result<HashMap<String, i32>, AppError> {
        if eval_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self
            .fetch_votes(format!(
                "{} AND kind = {} AND state IN ({}, {})",
                in_list("eval_id", eval_ids),
                sql_str(VoteKind::Vote.as_str()),
                sql_str(VoteState::Queued.as_str()),
                sql_str(VoteState::Sent.as_str())
            ))
            .await?;
        // created_at 순으로 정렬되어 있으므로 마지막 행이 유효한 표다
        let mut latest: HashMap<String, i32> = HashMap::new();
        for r in rows {
            latest.insert(r.eval_id, r.value);
        }
        latest.retain(|_, v| *v != 0);
        Ok(latest)
    }

    fn stage_persist(rec: &mut VoteRecord, state: VoteState, counted: Option<bool>, reason: Option<String>, dirty: &mut Vec<VoteRecord>) {
        rec.state = state;
        if counted.is_some() {
            rec.counted = counted;
        }
        rec.reason = reason;
        rec.updated_at = now_ms();
        dirty.push(rec.clone());
    }

    /// 못 보낸 표를 오래된 순으로 보낸다. 연결이 없으면 멈추고, 그 밖의 일시 오류는 그 표만 다음으로 미룬다.
    pub async fn flush<B: EvalBackend + ?Sized>(&self, backend: &B) -> VoteFlushReport {
        let mut report = VoteFlushReport::default();
        let queued = match self.queued().await {
            Ok(q) => q,
            Err(e) => {
                report.persist_error = Some(e.to_string());
                return report;
            }
        };
        let mut dirty: Vec<VoteRecord> = Vec::new();
        for mut rec in queued {
            if report.stopped.is_some() {
                report.remaining += 1;
                continue;
            }
            let req = VoteRequest {
                request_id: rec.request_id.clone(),
                id: rec.eval_id.clone(),
                value: rec.value,
                kind: rec.kind.as_str().to_string(),
            };
            match backend.vote(&req).await {
                Ok(resp) => {
                    Self::stage_persist(&mut rec, VoteState::Sent, Some(resp.counted), None, &mut dirty);
                    report.sent.push((rec.request_id.clone(), resp));
                }
                Err(AppError::Offline(m)) => {
                    report.stopped = Some(format!("offline: {m}"));
                    report.remaining += 1;
                }
                // 이 백엔드는 투표를 모른다 (판정 기능이 없는 서버). 버리지 않고 남겨 둔다
                Err(AppError::Rejected(m)) => {
                    report.stopped = Some(m);
                    report.remaining += 1;
                }
                Err(AppError::Server { status, message })
                    if is_missing_evaluation(status, &message) || matches!(status, 400 | 409 | 422) =>
                {
                    let reason = format!("server returned {status}: {message}");
                    Self::stage_persist(&mut rec, VoteState::Rejected, None, Some(reason.clone()), &mut dirty);
                    report.rejected.push(VoteRejected { request_id: rec.request_id.clone(), eval_id: rec.eval_id.clone(), reason });
                }
                // 401 · 403 · 429 는 표 탓이 아니다: 시도 횟수를 세지 않고 멈춘다 (남은 표는 그대로 다음에)
                Err(e) if matches!(classify_failure(&e), SyncFailure::AuthHold { .. } | SyncFailure::RateLimited { .. }) => {
                    match classify_failure(&e) {
                        SyncFailure::AuthHold { .. } => {
                            report.auth_required = true;
                            report.stopped = Some(format!("auth_required: {e}"));
                        }
                        SyncFailure::RateLimited { retry_after_ms } => {
                            report.retry_after_ms = Some(retry_after_ms);
                            report.stopped = Some(format!("rate_limited: {e}"));
                        }
                        _ => {}
                    }
                    report.remaining += 1;
                }
                Err(e) => {
                    rec.attempts += 1;
                    let reason = e.to_string();
                    if rec.attempts >= MAX_SERVER_ATTEMPTS {
                        Self::stage_persist(&mut rec, VoteState::Failed, None, Some(reason.clone()), &mut dirty);
                        report.rejected.push(VoteRejected { request_id: rec.request_id.clone(), eval_id: rec.eval_id.clone(), reason });
                    } else {
                        Self::stage_persist(&mut rec, VoteState::Queued, None, Some(reason), &mut dirty);
                        report.remaining += 1;
                    }
                }
            }
        }
        if !dirty.is_empty() {
            if let Err(e) = self.upsert_votes(&dirty).await {
                report.persist_error.get_or_insert(e.to_string());
            }
        }
        report
    }

    /// 캐시에 있는 판정 (없는 id는 빠진다)
    pub async fn cached_verdicts(&self, ids: &[String]) -> Result<HashMap<String, CachedVerdict>, AppError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let batches: Vec<RecordBatch> = self
            .verdicts
            .query()
            .only_if(in_list("eval_id", ids))
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        Ok(read_verdicts(&batches)?.into_iter().map(|c| (c.verdict.id.clone(), c)).collect())
    }

    /// 대체된 평가(서버가 신구 대체 보고를 받음)를 곧바로 discount(grade −2, 이유 superseded)로 둔다. cron 이 다음 판정에서
    /// 같은 등급을 매기므로(PD-Verdict: superseded 는 근거와 상관없이 discount), 서버 판정을 다시 받을 때까지의 표시다.
    /// 캐시에 있던 근거량 · 값 · 표 수는 그대로 두고 등급 · 이유만 바꾼다.
    pub async fn mark_superseded(&self, eval_id: &str, now: i64) -> Result<(), AppError> {
        let cached = self.cached_verdicts(&[eval_id.to_string()]).await?;
        let mut v = match cached.get(eval_id) {
            Some(c) => c.verdict.clone(),
            None => VerdictDto {
                id: eval_id.to_string(),
                tier: "unrated".into(),
                grade: None,
                rank: None,
                value: 0.0,
                evidence: 0.0,
                up: 0,
                down: 0,
                level: None,
                reasons: Vec::new(),
                recipe: SUPERSEDED_RECIPE.into(),
                updated_at: None,
            },
        };
        v.tier = "discount".into();
        v.grade = Some(-2);
        v.reasons = vec!["superseded".into()];
        self.store_verdicts(&[v], now).await
    }

    /// 서버에서 받은 판정을 캐시에 쓴다 (같은 평가는 덮어쓴다)
    pub async fn store_verdicts(&self, verdicts: &[VerdictDto], fetched_at: i64) -> Result<(), AppError> {
        if verdicts.is_empty() {
            return Ok(());
        }
        // 같은 id가 두 번 오면 merge_insert가 거부하므로 마지막 값만 남긴다
        let mut seen = HashSet::new();
        let mut unique: Vec<VerdictDto> = verdicts.iter().rev().filter(|v| seen.insert(v.id.clone())).cloned().collect();
        unique.reverse();
        let batch = verdicts_batch(&unique, fetched_at)?;
        let reader = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], verdicts_schema());
        let mut merge = self.verdicts.merge_insert(&["eval_id"]);
        merge.when_matched_update_all(None).when_not_matched_insert_all();
        merge.execute(Box::new(reader)).await.map_err(storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{SaveRequest, SaveResponse, SearchRequest, SearchResponse, StatusResponse, VerdictsResponse};
    use crate::local::test_support::temp_dir;
    use async_trait::async_trait;
    use std::sync::Mutex;

    const EVAL: &str = "0190f0a0-0000-7000-8000-0000000000aa";
    const OTHER: &str = "0190f0a0-0000-7000-8000-0000000000bb";

    /// 투표 요청을 기록하고, 정해 둔 답을 차례로 돌려주는 가짜 서버
    #[derive(Default)]
    struct VoteServer {
        seen: Mutex<Vec<VoteRequest>>,
        script: Mutex<Vec<Result<bool, AppError>>>,
    }

    #[async_trait]
    impl EvalBackend for VoteServer {
        async fn save(&self, _req: &SaveRequest) -> Result<SaveResponse, AppError> {
            unreachable!()
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            unreachable!()
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            unreachable!()
        }
        async fn vote(&self, req: &VoteRequest) -> Result<VoteResponse, AppError> {
            self.seen.lock().unwrap().push(req.clone());
            let next = {
                let mut script = self.script.lock().unwrap();
                if script.is_empty() { Ok(true) } else { script.remove(0) }
            };
            next.map(|counted| VoteResponse {
                id: req.id.clone(),
                value: req.value,
                kind: req.kind.clone(),
                counted,
                status: Some("accepted".into()),
            })
        }
        async fn verdicts(&self, _ids: &[String]) -> Result<VerdictsResponse, AppError> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn a_changed_offline_vote_sends_only_the_last_value_under_a_new_request_id() {
        let store = VoteStore::open(temp_dir("votes-change")).await.unwrap();
        let first = store.put_vote(EVAL, VoteKind::Vote, 1).await.unwrap();
        let second = store.put_vote(EVAL, VoteKind::Vote, -1).await.unwrap();
        assert_ne!(first.request_id, second.request_id);
        assert_eq!(store.queued().await.unwrap(), vec![second.clone()], "앞의 표는 지워졌다");
        // 다른 종류(결과로 확인된 표)는 따로 둔다
        store.put_vote(EVAL, VoteKind::VoteOutcome, 1).await.unwrap();
        assert_eq!(store.queued().await.unwrap().len(), 2);
        assert_eq!(store.my_votes(&[EVAL.into()]).await.unwrap()[EVAL], -1, "직접 누른 표만 보인다");

        let server = VoteServer::default();
        let report = store.flush(&server).await;
        assert_eq!((report.sent.len(), report.remaining, report.stopped.clone()), (2, 0, None));
        let seen = server.seen.lock().unwrap().clone();
        assert_eq!((seen[0].value, seen[0].kind.as_str()), (-1, "vote"));
        assert_eq!(seen[0].request_id, second.request_id);
        let sent = store.get(&second.request_id).await.unwrap().unwrap();
        assert_eq!((sent.state, sent.counted), (VoteState::Sent, Some(true)));

        // 철회하면 내 표가 사라진다
        store.put_vote(EVAL, VoteKind::Vote, 0).await.unwrap();
        assert!(store.my_votes(&[EVAL.into()]).await.unwrap().is_empty());
        assert!(store.put_vote(EVAL, VoteKind::Vote, 2).await.is_err());
    }

    #[tokio::test]
    async fn offline_stops_permanent_errors_reject_and_temporary_errors_retry_then_fail() {
        let store = VoteStore::open(temp_dir("votes-errors")).await.unwrap();
        let a = store.put_vote(EVAL, VoteKind::Vote, 1).await.unwrap();
        let b = store.put_vote(OTHER, VoteKind::Vote, 1).await.unwrap();

        let server = VoteServer::default();
        *server.script.lock().unwrap() = vec![Err(AppError::Offline("no network".into()))];
        let r = store.flush(&server).await;
        assert_eq!((r.sent.len(), r.remaining), (0, 2));
        assert!(r.stopped.unwrap().starts_with("offline"));
        assert_eq!(server.seen.lock().unwrap().len(), 1, "연결이 없으면 다음 표를 보내지 않는다");

        // 평가가 서버에 없다: 바로 거부. 다른 표는 일시 오류(503)로 남는다
        *server.script.lock().unwrap() = vec![
            Err(AppError::Server { status: 404, message: r#"{"error":"evaluation not found"}"#.into() }),
            Err(AppError::Server { status: 503, message: "busy".into() }),
        ];
        let r = store.flush(&server).await;
        assert_eq!(r.rejected.len(), 1);
        assert_eq!(r.rejected[0].request_id, a.request_id);
        assert_eq!(r.remaining, 1);
        assert_eq!(store.get(&a.request_id).await.unwrap().unwrap().state, VoteState::Rejected);
        assert_eq!(store.get(&b.request_id).await.unwrap().unwrap().attempts, 1);

        // 그냥 404(설정 문제일 수 있음)는 재시도. 한도에 닿으면 실패로 끝낸다
        *server.script.lock().unwrap() =
            (0..MAX_SERVER_ATTEMPTS).map(|_| Err(AppError::Server { status: 404, message: "Not Found".into() })).collect();
        for _ in 1..MAX_SERVER_ATTEMPTS - 1 {
            assert_eq!(store.flush(&server).await.remaining, 1);
        }
        let r = store.flush(&server).await;
        assert_eq!((r.remaining, r.rejected.len()), (0, 1));
        assert_eq!(store.get(&b.request_id).await.unwrap().unwrap().state, VoteState::Failed);
    }

    // FC-12 (투표 쪽): 401 · 429 는 시도 횟수를 세지 않고 멈춘다. 60초가 지나도 표는 그대로 남고, 풀리면 바로 보낸다
    #[tokio::test]
    async fn fc12_auth_and_rate_limits_never_fail_a_vote() {
        let store = VoteStore::open(temp_dir("votes-auth-hold")).await.unwrap();
        let a = store.put_vote(EVAL, VoteKind::Vote, 1).await.unwrap();
        store.put_vote(OTHER, VoteKind::Vote, -1).await.unwrap();
        assert_eq!(store.queued_count().await.unwrap(), 2);
        let server = VoteServer::default();
        for round in 0..6 {
            let status = if round < 3 { 401 } else { 429 };
            *server.script.lock().unwrap() = vec![Err(AppError::Server { status, message: "scripted".into() })];
            let r = store.flush(&server).await;
            assert_eq!((r.sent.len(), r.rejected.len(), r.remaining), (0, 0, 2), "round {round}");
            if status == 401 {
                assert!(r.auth_required && r.stopped.as_deref().unwrap().starts_with("auth_required"));
            } else {
                assert_eq!(r.retry_after_ms, Some(crate::queue::DEFAULT_RETRY_AFTER_MS));
            }
            let rec = store.get(&a.request_id).await.unwrap().unwrap();
            assert_eq!((rec.state, rec.attempts), (VoteState::Queued, 0), "round {round}");
        }
        let r = store.flush(&server).await;
        assert_eq!((r.sent.len(), r.remaining), (2, 0));
        assert_eq!(store.queued_count().await.unwrap(), 0);
    }

    // E-17 (투표 쪽): 상한에 닿으면 가장 오래된 결과 표(vote_outcome)부터 지우고, 직접 누른 표는 지우지 않는다
    #[tokio::test]
    async fn the_vote_queue_is_bounded_and_manual_votes_are_kept() {
        assert_eq!(MAX_QUEUED_VOTES, 2_000);
        // 2,000 행을 채우면 느리므로 상한을 3 으로 줄여 같은 규칙을 시험한다
        let store = VoteStore::open(temp_dir("votes-cap")).await.unwrap().with_cap(3);
        let id = |i: usize| format!("0190f0a0-0000-7000-8000-{i:012}");
        let old_outcome = store.put_vote(&id(1), VoteKind::VoteOutcome, 1).await.unwrap();
        let new_outcome = store.put_vote(&id(2), VoteKind::VoteOutcome, -1).await.unwrap();
        let manual = store.put_vote(&id(3), VoteKind::Vote, 1).await.unwrap();
        // 같은 (평가, 종류)는 한 행으로 합쳐지므로 상한에 닿아도 지울 것이 없다
        let changed = store.put_vote(&id(3), VoteKind::Vote, -1).await.unwrap();
        assert_eq!(store.queued_count().await.unwrap(), 3);
        assert!(store.get(&manual.request_id).await.unwrap().is_none(), "바뀐 표는 새 request_id");
        // 넷째 평가: 가장 오래된 결과 표가 자리를 비운다
        store.put_vote(&id(4), VoteKind::Vote, 1).await.unwrap();
        assert_eq!(store.queued_count().await.unwrap(), 3);
        assert!(store.get(&old_outcome.request_id).await.unwrap().is_none());
        assert!(store.get(&new_outcome.request_id).await.unwrap().is_some());
        store.put_vote(&id(5), VoteKind::Vote, 1).await.unwrap();
        assert!(store.get(&new_outcome.request_id).await.unwrap().is_none());
        // 직접 누른 표만 남았다: 새 표를 받지 않는다 (지우지 않는다)
        assert!(matches!(store.put_vote(&id(6), VoteKind::Vote, 1).await, Err(AppError::QueueFull(3))));
        assert!(store.get(&changed.request_id).await.unwrap().is_some());
        assert_eq!(store.queued_count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn verdicts_are_cached_by_id_and_survive_a_reopen() {
        let dir = temp_dir("verdicts-cache");
        let v = |id: &str, tier: &str, grade: Option<i32>, rank: Option<f64>| VerdictDto {
            id: id.into(),
            tier: tier.into(),
            grade,
            rank,
            value: 0.5,
            evidence: 2.9,
            up: 3,
            down: 0,
            level: rank.map(|_| "secondary".into()),
            reasons: vec!["rank_high".into(), "level:secondary".into()],
            recipe: "pd-verdict/1".into(),
            updated_at: Some(1_000),
        };
        {
            let store = VoteStore::open(&dir).await.unwrap();
            store.store_verdicts(&[v(EVAL, "neutral", Some(0), Some(0.1)), v(EVAL, "premium", Some(2), Some(0.97))], 5_000).await.unwrap();
            store.store_verdicts(&[v(OTHER, "unrated", None, None)], 6_000).await.unwrap();
        }
        let store = VoteStore::open(&dir).await.unwrap();
        let got = store.cached_verdicts(&[EVAL.into(), OTHER.into(), "0190f0a0-0000-7000-8000-0000000000cc".into()]).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[EVAL].verdict.tier, "premium", "같은 id는 마지막 값");
        assert_eq!(got[EVAL].verdict.reasons, vec!["rank_high", "level:secondary"]);
        assert_eq!(got[EVAL].fetched_at, 5_000);
        assert_eq!((got[OTHER].verdict.grade, got[OTHER].verdict.rank), (None, None));
    }
}
