//! 로컬 원본 저장소 (LanceDB).
//!
//! 저장소 구조 결정 (README 13절): **Rust(이 모듈)가 원본이고, 화면의 Dexie는 캐시**다.
//! - 평가 한 건이 한 행이다. 보내기 전(`queued`)부터 서버에 저장된 뒤(`stored`)까지 같은 행이 상태만 바뀐다.
//! - 서버에는 원문이 남지 않으므로(cron이 처리 후 지운다) 내가 쓴 글의 원문은 이 저장소가 유일한 원본이다.
//! - 벡터도 함께 두어 서버에 가지 않고도 내 글을 검색할 수 있다 (코사인 거리).
//!
//! 컬럼 규칙은 서버 `evaluations`와 같은 의미다 (README 13-2). 이 저장소에서 "없음"은 NULL이고,
//! 화면(Dexie)으로 내보낼 때만 빈 문자열로 바꾼다 (`to_cache_row`, `none_to_empty`).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use futures::TryStreamExt;
use lancedb::arrow::arrow_array::types::Float32Type;
use lancedb::arrow::arrow_array::{
    Array, FixedSizeListArray, Float32Array, Int32Array, Int64Array, RecordBatch, RecordBatchIterator,
    StringArray,
};
use lancedb::arrow::arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::table::OptimizeAction;
use lancedb::{connect, DistanceType, Table};
use serde_json::{json, Value};

use crate::embedding::EMBEDDING_DIM;
use crate::error::AppError;

/// 로컬 평가 테이블 이름
pub const TABLE: &str = "evaluations_local";

pub(crate) fn storage<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Storage(e.to_string())
}

/// 평가의 로컬 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalState {
    /// 아직 서버에 보내지 못했다 (오프라인이거나 재시도 대기)
    Queued,
    /// 서버가 접수했고 욕설 검사와 저장이 진행 중이다
    Pending,
    /// 서버 저장까지 확인했다
    Stored,
    /// 서버가 욕설 등으로 거부했다
    Rejected,
    /// 서버가 재시도 한도를 넘겨 실패로 끝냈다
    Failed,
    /// 서버가 기록을 지워서 결과를 알 수 없다
    Missing,
}

impl LocalState {
    pub fn as_str(self) -> &'static str {
        match self {
            LocalState::Queued => "queued",
            LocalState::Pending => "pending",
            LocalState::Stored => "stored",
            LocalState::Rejected => "rejected",
            LocalState::Failed => "failed",
            LocalState::Missing => "missing",
        }
    }

    pub fn parse(s: &str) -> Result<Self, AppError> {
        Ok(match s {
            "queued" => LocalState::Queued,
            "pending" => LocalState::Pending,
            "stored" => LocalState::Stored,
            "rejected" => LocalState::Rejected,
            "failed" => LocalState::Failed,
            "missing" => LocalState::Missing,
            other => return Err(AppError::Storage(format!("unknown local state: {other}"))),
        })
    }

    /// 로컬 검색에 포함되는 상태. 거부되거나 실패한 글은 검색하지 않는다.
    pub fn searchable(self) -> bool {
        matches!(self, LocalState::Queued | LocalState::Pending | LocalState::Stored)
    }
}

/// 로컬 평가 한 건 (테이블 한 행)
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRecord {
    /// 멱등 키. 기본 키
    pub request_id: String,
    /// 큐 안에서 쓰는 번호 (`local-N`). 답글의 부모 지정에 쓴다
    pub local_id: String,
    /// 서버가 정한 UUID. 접수되기 전에는 None
    pub id: Option<String>,
    pub root: Option<String>,
    /// 서버 id로 정해진 부모
    pub ref_: Option<String>,
    /// 부모가 아직 서버에 없을 때 부모의 로컬 id
    pub parent_local: Option<String>,
    pub cc: String,
    pub from: Option<String>,
    pub to: Option<String>,
    /// 보류 (항상 None)
    pub bcc: Option<String>,
    pub text: String,
    pub text_hash: String,
    /// 768차원. 이전 큐에서 옮겨 온 접수 완료 항목처럼 벡터가 없는 행은 None
    pub vector: Option<Vec<f32>>,
    pub state: LocalState,
    pub attempts: u32,
    /// 거부·실패 이유
    pub reason: Option<String>,
    /// 밀리초 (UTC epoch)
    pub created_at: i64,
    pub updated_at: i64,
}

/// 로컬 검색 결과 한 건. 내 글의 원문이 들어 있다.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalMatch {
    pub request_id: String,
    pub local_id: String,
    pub id: Option<String>,
    pub cc: String,
    pub state: LocalState,
    pub text: String,
    /// 코사인 유사도 (1에 가까울수록 비슷하다). 서버 검색의 score와 같은 방향이다
    pub score: f32,
}

/// 화면 캐시(Dexie)로 내보낼 때 "없음"을 빈 문자열로 바꾼다 (README 13-3의 변환 규칙, 이 함수 한 곳에서만)
pub fn none_to_empty(v: Option<&str>) -> String {
    v.unwrap_or("").to_string()
}

/// 화면에서 받은 빈 문자열을 "없음"으로 되돌린다 (공백만 있는 값도 없음으로 본다)
pub fn empty_to_none(v: &str) -> Option<String> {
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

impl LocalRecord {
    /// 화면 캐시(Dexie) 행. 벡터는 넣지 않는다 (검색은 Rust에서 하므로 IPC로 보낼 이유가 없다).
    /// 형태는 `web/desktop/localdb.ts`의 `LocalRow`와 같아야 한다.
    pub fn to_cache_row(&self) -> Value {
        json!({
            "request_id": self.request_id,
            "id": none_to_empty(self.id.as_deref()),
            "from": none_to_empty(self.from.as_deref()),
            "to": none_to_empty(self.to.as_deref()),
            "cc": self.cc,
            "bcc": none_to_empty(self.bcc.as_deref()),
            "ref": none_to_empty(self.ref_.as_deref()),
            "root": none_to_empty(self.root.as_deref()),
            "state": self.state.as_str(),
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "data": {
                "local_id": self.local_id,
                "parent_local": none_to_empty(self.parent_local.as_deref()),
                "text": self.text,
                "text_hash": self.text_hash,
                "attempts": self.attempts,
                "reason": none_to_empty(self.reason.as_deref()),
            },
        })
    }
}

/// 테이블 스키마 (서버 `evaluations`와 같은 컬럼 이름, 로컬 전용 컬럼 추가)
pub fn schema() -> SchemaRef {
    let text = |name: &str, nullable: bool| Field::new(name, DataType::Utf8, nullable);
    Arc::new(Schema::new(vec![
        text("request_id", false),
        text("local_id", false),
        text("id", true),
        text("root", true),
        text("ref", true),
        text("parent_local", true),
        text("cc", false),
        text("from", true),
        text("to", true),
        text("bcc", true),
        text("text", false),
        text("text_hash", false),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), EMBEDDING_DIM as i32),
            true,
        ),
        text("state", false),
        Field::new("attempts", DataType::Int32, false),
        text("reason", true),
        Field::new("created_at", DataType::Int64, false),
        Field::new("updated_at", DataType::Int64, false),
    ]))
}

/// 타입 비교용 표기. 벡터는 차원만 비교한다 (LanceDB가 내부 필드 이름이나 메타데이터를 바꿔 돌려줄 수 있다)
fn type_signature(dt: &DataType) -> String {
    match dt {
        DataType::FixedSizeList(_, n) => format!("fixed_size_list<{n}>"),
        other => format!("{other:?}"),
    }
}

pub(crate) fn signature(schema: &Schema) -> Vec<(String, String)> {
    schema
        .fields()
        .iter()
        .map(|f| (f.name().clone(), type_signature(f.data_type())))
        .collect()
}

/// 벡터를 뺀 컬럼 목록 (목록 조회에서 벡터를 읽지 않기 위해)
fn columns_without_vector() -> Vec<String> {
    schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .filter(|n| n != "vector")
        .collect()
}

/// SQL 문자열 리터럴. 작은따옴표를 이스케이프한다
pub(crate) fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// UPDATE에 넣을 값
#[derive(Debug, Clone)]
pub enum SqlValue {
    Str(String),
    Null,
    Int(i64),
}

impl SqlValue {
    pub fn opt_str(v: Option<&str>) -> Self {
        match v {
            Some(s) => SqlValue::Str(s.to_string()),
            None => SqlValue::Null,
        }
    }

    fn to_sql(&self) -> String {
        match self {
            SqlValue::Str(s) => sql_str(s),
            SqlValue::Null => "NULL".to_string(),
            SqlValue::Int(i) => i.to_string(),
        }
    }
}

fn to_batch(records: &[LocalRecord]) -> Result<RecordBatch, AppError> {
    for r in records {
        if let Some(v) = &r.vector {
            if v.len() != EMBEDDING_DIM {
                return Err(AppError::InvalidInput(format!(
                    "local vector must have {EMBEDDING_DIM} dims, got {}",
                    v.len()
                )));
            }
        }
    }
    let strs = |f: &dyn Fn(&LocalRecord) -> Option<&str>| -> Arc<dyn Array> {
        Arc::new(StringArray::from_iter(records.iter().map(|r| f(r))))
    };
    let vector = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        records
            .iter()
            .map(|r| r.vector.as_ref().map(|v| v.iter().copied().map(Some).collect::<Vec<_>>())),
        EMBEDDING_DIM as i32,
    );
    let columns: Vec<Arc<dyn Array>> = vec![
        strs(&|r| Some(r.request_id.as_str())),
        strs(&|r| Some(r.local_id.as_str())),
        strs(&|r| r.id.as_deref()),
        strs(&|r| r.root.as_deref()),
        strs(&|r| r.ref_.as_deref()),
        strs(&|r| r.parent_local.as_deref()),
        strs(&|r| Some(r.cc.as_str())),
        strs(&|r| r.from.as_deref()),
        strs(&|r| r.to.as_deref()),
        strs(&|r| r.bcc.as_deref()),
        strs(&|r| Some(r.text.as_str())),
        strs(&|r| Some(r.text_hash.as_str())),
        Arc::new(vector),
        strs(&|r| Some(r.state.as_str())),
        Arc::new(Int32Array::from_iter_values(records.iter().map(|r| r.attempts as i32))),
        strs(&|r| r.reason.as_deref()),
        Arc::new(Int64Array::from_iter_values(records.iter().map(|r| r.created_at))),
        Arc::new(Int64Array::from_iter_values(records.iter().map(|r| r.updated_at))),
    ];
    RecordBatch::try_new(schema(), columns).map_err(storage)
}

fn string_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray, AppError> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| AppError::Storage(format!("local table is missing string column {name}")))
}

fn opt(arr: &StringArray, i: usize) -> Option<String> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i).to_string())
    }
}

fn read_records(batches: &[RecordBatch]) -> Result<Vec<LocalRecord>, AppError> {
    let mut out = Vec::new();
    for b in batches {
        let request_id = string_col(b, "request_id")?;
        let local_id = string_col(b, "local_id")?;
        let id = string_col(b, "id")?;
        let root = string_col(b, "root")?;
        let ref_ = string_col(b, "ref")?;
        let parent_local = string_col(b, "parent_local")?;
        let cc = string_col(b, "cc")?;
        let from = string_col(b, "from")?;
        let to = string_col(b, "to")?;
        let bcc = string_col(b, "bcc")?;
        let text = string_col(b, "text")?;
        let text_hash = string_col(b, "text_hash")?;
        let state = string_col(b, "state")?;
        let reason = string_col(b, "reason")?;
        let attempts = b
            .column_by_name("attempts")
            .and_then(|c| c.as_any().downcast_ref::<Int32Array>())
            .ok_or_else(|| AppError::Storage("local table is missing attempts".into()))?;
        let created_at = b
            .column_by_name("created_at")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            .ok_or_else(|| AppError::Storage("local table is missing created_at".into()))?;
        let updated_at = b
            .column_by_name("updated_at")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            .ok_or_else(|| AppError::Storage("local table is missing updated_at".into()))?;
        // 벡터를 읽지 않은 조회(목록)에서는 컬럼 자체가 없다
        let vectors = b
            .column_by_name("vector")
            .and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());

        for i in 0..b.num_rows() {
            let vector = match vectors {
                Some(list) if !list.is_null(i) => {
                    let values = list.value(i);
                    let floats = values
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .ok_or_else(|| AppError::Storage("vector items are not Float32".into()))?;
                    Some(floats.values().to_vec())
                }
                _ => None,
            };
            out.push(LocalRecord {
                request_id: request_id.value(i).to_string(),
                local_id: local_id.value(i).to_string(),
                id: opt(id, i),
                root: opt(root, i),
                ref_: opt(ref_, i),
                parent_local: opt(parent_local, i),
                cc: cc.value(i).to_string(),
                from: opt(from, i),
                to: opt(to, i),
                bcc: opt(bcc, i),
                text: text.value(i).to_string(),
                text_hash: text_hash.value(i).to_string(),
                vector,
                state: LocalState::parse(state.value(i))?,
                attempts: attempts.value(i).max(0) as u32,
                reason: opt(reason, i),
                created_at: created_at.value(i),
                updated_at: updated_at.value(i),
            });
        }
    }
    Ok(out)
}

/// `local-N` 형태의 번호에서 N. 형식이 다르면 None
pub(crate) fn local_seq(local_id: &str) -> Option<u64> {
    local_id.strip_prefix("local-")?.parse().ok()
}

/// 로컬 원본 저장소 (LanceDB 테이블 하나)
pub struct LocalDb {
    table: Table,
}

impl LocalDb {
    /// 폴더를 열고(없으면 만들고) 테이블을 준비한다.
    /// 이미 있는 테이블의 컬럼이 이 버전의 스키마와 다르면 오류로 멈춘다 (조용히 덮어쓰거나 비우지 않는다).
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(storage)?;
        let uri = dir
            .to_str()
            .ok_or_else(|| AppError::Storage("local db path is not valid UTF-8".into()))?;
        let conn = connect(uri).execute().await.map_err(storage)?;
        let names = conn.table_names().execute().await.map_err(storage)?;
        let table = if names.iter().any(|n| n == TABLE) {
            let table = conn.open_table(TABLE).execute().await.map_err(storage)?;
            let existing = table.schema().await.map_err(storage)?;
            if signature(&existing) != signature(&schema()) {
                return Err(AppError::Storage(format!(
                    "local table {TABLE} has an unexpected schema (expected {} columns, found {})",
                    schema().fields().len(),
                    existing.fields().len()
                )));
            }
            table
        } else {
            conn.create_empty_table(TABLE, schema()).execute().await.map_err(storage)?
        };
        Ok(Self { table })
    }

    pub async fn count(&self) -> Result<usize, AppError> {
        self.table.count_rows(None).await.map_err(storage)
    }

    /// 작은 파일을 합치고 오래된 버전을 정리한다. 쓰기(upsert, update)마다 새 버전과 작은 파일이 생기므로
    /// 앱을 열 때 한 번씩 부르는 것으로 충분하다 (LanceDB 문서: 수정 작업이 20번쯤 쌓이면 실행을 권장).
    pub async fn optimize(&self) -> Result<(), AppError> {
        self.table.optimize(OptimizeAction::All).await.map_err(storage)?;
        Ok(())
    }

    /// request_id가 같은 행이 있으면 전체를 덮어쓰고, 없으면 넣는다 (멱등)
    pub async fn upsert(&self, rec: &LocalRecord) -> Result<(), AppError> {
        let batch = to_batch(std::slice::from_ref(rec))?;
        let reader = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], schema());
        let mut merge = self.table.merge_insert(&["request_id"]);
        merge.when_matched_update_all(None).when_not_matched_insert_all();
        merge.execute(Box::new(reader)).await.map_err(storage)?;
        Ok(())
    }

    /// 필터에 맞는 행의 일부 컬럼만 바꾼다. 벡터가 든 행 전체를 다시 쓰지 않는다.
    pub async fn update_where(&self, filter: &str, sets: &[(&str, SqlValue)]) -> Result<(), AppError> {
        let mut update = self.table.update().only_if(filter);
        for (column, value) in sets {
            update = update.column(*column, value.to_sql());
        }
        update.execute().await.map_err(storage)?;
        Ok(())
    }

    pub async fn update_by_local_id(&self, local_id: &str, sets: &[(&str, SqlValue)]) -> Result<(), AppError> {
        self.update_where(&format!("local_id = {}", sql_str(local_id)), sets).await
    }

    /// 필터에 맞는 행을 읽는다. with_vector가 false면 벡터 컬럼은 읽지 않는다.
    pub async fn fetch(
        &self,
        filter: Option<String>,
        with_vector: bool,
        limit: Option<usize>,
    ) -> Result<Vec<LocalRecord>, AppError> {
        let mut query = self.table.query();
        if let Some(f) = filter {
            query = query.only_if(f);
        }
        if !with_vector {
            query = query.select(Select::columns(&columns_without_vector().iter().map(String::as_str).collect::<Vec<_>>()));
        }
        if let Some(n) = limit {
            query = query.limit(n);
        }
        let batches: Vec<RecordBatch> = query
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        read_records(&batches)
    }

    pub async fn get(&self, request_id: &str, with_vector: bool) -> Result<Option<LocalRecord>, AppError> {
        let rows = self
            .fetch(Some(format!("request_id = {}", sql_str(request_id))), with_vector, Some(1))
            .await?;
        Ok(rows.into_iter().next())
    }

    pub async fn get_by_local_id(&self, local_id: &str, with_vector: bool) -> Result<Option<LocalRecord>, AppError> {
        let rows = self
            .fetch(Some(format!("local_id = {}", sql_str(local_id))), with_vector, Some(1))
            .await?;
        Ok(rows.into_iter().next())
    }

    pub async fn by_state(&self, state: LocalState, with_vector: bool) -> Result<Vec<LocalRecord>, AppError> {
        self.fetch(Some(format!("state = {}", sql_str(state.as_str()))), with_vector, None).await
    }

    /// 가장 큰 `local-N`의 N. 재시작 뒤에도 번호가 겹치지 않게 큐의 다음 번호를 정하는 데 쓴다.
    pub async fn max_local_seq(&self) -> Result<u64, AppError> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .select(Select::columns(&["local_id"]))
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        let mut max = 0;
        for b in &batches {
            let col = string_col(b, "local_id")?;
            for i in 0..b.num_rows() {
                max = max.max(local_seq(col.value(i)).unwrap_or(0));
            }
        }
        Ok(max)
    }

    /// updated_at이 since_ms보다 큰 행 (화면 캐시를 갱신할 때 쓴다). 벡터는 읽지 않는다.
    pub async fn changed_since(&self, since_ms: i64) -> Result<Vec<LocalRecord>, AppError> {
        self.fetch(Some(format!("updated_at > {since_ms}")), false, None).await
    }

    /// 로컬 벡터 검색 (코사인). 거부·실패한 글과 벡터가 없는 행은 제외한다.
    pub async fn search(&self, vector: &[f32], top_k: usize, cc: Option<&str>) -> Result<Vec<LocalMatch>, AppError> {
        if vector.len() != EMBEDDING_DIM {
            return Err(AppError::InvalidInput(format!(
                "query vector must have {EMBEDDING_DIM} dims, got {}",
                vector.len()
            )));
        }
        if top_k == 0 {
            return Ok(vec![]);
        }
        let mut filter = String::from("vector IS NOT NULL AND state IN ('queued', 'pending', 'stored')");
        if let Some(cc) = cc {
            filter.push_str(&format!(" AND cc = {}", sql_str(cc)));
        }
        let batches: Vec<RecordBatch> = self
            .table
            .vector_search(vector.to_vec())
            .map_err(storage)?
            .column("vector")
            .distance_type(DistanceType::Cosine)
            .only_if(filter)
            .limit(top_k)
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;

        let mut out = Vec::new();
        for b in &batches {
            let request_id = string_col(b, "request_id")?;
            let local_id = string_col(b, "local_id")?;
            let id = string_col(b, "id")?;
            let cc = string_col(b, "cc")?;
            let text = string_col(b, "text")?;
            let state = string_col(b, "state")?;
            let distance = b
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .ok_or_else(|| AppError::Storage("search result has no _distance column".into()))?;
            for i in 0..b.num_rows() {
                out.push(LocalMatch {
                    request_id: request_id.value(i).to_string(),
                    local_id: local_id.value(i).to_string(),
                    id: opt(id, i),
                    cc: cc.value(i).to_string(),
                    state: LocalState::parse(state.value(i))?,
                    text: text.value(i).to_string(),
                    // 코사인 거리 = 1 - 코사인 유사도
                    score: 1.0 - distance.value(i),
                });
            }
        }
        out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    /// 상태별 행 수 (화면 표시와 점검용)
    pub async fn counts_by_state(&self) -> Result<BTreeMap<String, usize>, AppError> {
        let mut out = BTreeMap::new();
        for s in [
            LocalState::Queued,
            LocalState::Pending,
            LocalState::Stored,
            LocalState::Rejected,
            LocalState::Failed,
            LocalState::Missing,
        ] {
            let n = self
                .table
                .count_rows(Some(format!("state = {}", sql_str(s.as_str()))))
                .await
                .map_err(storage)?;
            if n > 0 {
                out.insert(s.as_str().to_string(), n);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// 테스트용 임시 폴더 (프로세스 id와 시각으로 겹치지 않게 한다)
    pub fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("pd-local-{tag}-{}-{nanos}", std::process::id()))
    }

    /// i번째 축만 1인 단위 벡터. 서로 다른 i는 직교한다.
    pub fn axis(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; EMBEDDING_DIM];
        v[i % EMBEDDING_DIM] = 1.0;
        v
    }

    pub fn record(request_id: &str, local_seq: u64, vector: Option<Vec<f32>>) -> LocalRecord {
        LocalRecord {
            request_id: request_id.into(),
            local_id: format!("local-{local_seq}"),
            id: None,
            root: None,
            ref_: None,
            parent_local: None,
            cc: "example.com".into(),
            from: None,
            to: None,
            bcc: None,
            text: format!("text of {request_id}"),
            text_hash: crate::api::text_hash(&format!("text of {request_id}")),
            vector,
            state: LocalState::Queued,
            attempts: 0,
            reason: None,
            created_at: 1_000 + local_seq as i64,
            updated_at: 1_000 + local_seq as i64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[tokio::test]
    async fn open_creates_the_table_and_reopen_keeps_rows() {
        let dir = temp_dir("open");
        let db = LocalDb::open(&dir).await.unwrap();
        assert_eq!(db.count().await.unwrap(), 0);
        db.upsert(&record("req-aaaaaaaaaaaaaaaa", 1, Some(axis(0)))).await.unwrap();
        drop(db);

        let again = LocalDb::open(&dir).await.unwrap();
        assert_eq!(again.count().await.unwrap(), 1);
        let got = again.get("req-aaaaaaaaaaaaaaaa", true).await.unwrap().unwrap();
        assert_eq!(got.vector.as_deref(), Some(axis(0).as_slice()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn upsert_by_request_id_updates_instead_of_duplicating() {
        let dir = temp_dir("upsert");
        let db = LocalDb::open(&dir).await.unwrap();
        let mut rec = record("req-bbbbbbbbbbbbbbbb", 1, Some(axis(1)));
        db.upsert(&rec).await.unwrap();

        rec.state = LocalState::Stored;
        rec.id = Some("0190f0a0-0000-7000-8000-000000000001".into());
        rec.updated_at = 5_000;
        db.upsert(&rec).await.unwrap();

        assert_eq!(db.count().await.unwrap(), 1, "같은 request_id는 한 행이다");
        let got = db.get("req-bbbbbbbbbbbbbbbb", false).await.unwrap().unwrap();
        assert_eq!(got.state, LocalState::Stored);
        assert_eq!(got.id.as_deref(), Some("0190f0a0-0000-7000-8000-000000000001"));
        assert_eq!(got.updated_at, 5_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn every_column_round_trips_including_nulls() {
        let dir = temp_dir("roundtrip");
        let db = LocalDb::open(&dir).await.unwrap();
        let mut full = record("req-cccccccccccccccc", 7, Some(axis(5)));
        full.id = Some("0190f0a0-0000-7000-8000-000000000007".into());
        full.root = Some("0190f0a0-0000-7000-8000-000000000001".into());
        full.ref_ = Some("0190f0a0-0000-7000-8000-000000000001".into());
        full.from = Some("0x00000000000000000000000000000000000000ab".into());
        full.to = Some("0x0000000000000000000000000000000000000002".into());
        full.reason = Some("profanity".into());
        full.state = LocalState::Rejected;
        full.attempts = 3;
        db.upsert(&full).await.unwrap();

        let bare = record("req-dddddddddddddddd", 8, None);
        db.upsert(&bare).await.unwrap();

        assert_eq!(db.get("req-cccccccccccccccc", true).await.unwrap().unwrap(), full);
        let got_bare = db.get("req-dddddddddddddddd", true).await.unwrap().unwrap();
        assert_eq!(got_bare, bare);
        assert!(got_bare.vector.is_none() && got_bare.id.is_none() && got_bare.reason.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fetch_without_vector_does_not_read_the_vector_column() {
        let dir = temp_dir("novec");
        let db = LocalDb::open(&dir).await.unwrap();
        db.upsert(&record("req-eeeeeeeeeeeeeeee", 1, Some(axis(2)))).await.unwrap();
        let rows = db.fetch(None, false, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].vector.is_none());
        assert_eq!(rows[0].text, "text of req-eeeeeeeeeeeeeeee");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn update_where_changes_only_the_named_columns_and_can_set_null() {
        let dir = temp_dir("update");
        let db = LocalDb::open(&dir).await.unwrap();
        let mut rec = record("req-ffffffffffffffff", 1, Some(axis(3)));
        rec.reason = Some("old reason".into());
        db.upsert(&rec).await.unwrap();

        db.update_by_local_id(
            "local-1",
            &[
                ("state", SqlValue::Str("stored".into())),
                ("id", SqlValue::Str("0190f0a0-0000-7000-8000-000000000009".into())),
                ("from", SqlValue::Str("0x00000000000000000000000000000000000000ab".into())),
                ("ref", SqlValue::Null),
                ("reason", SqlValue::Null),
                ("attempts", SqlValue::Int(4)),
                ("updated_at", SqlValue::Int(9_999)),
            ],
        )
        .await
        .unwrap();

        let got = db.get("req-ffffffffffffffff", true).await.unwrap().unwrap();
        assert_eq!(got.state, LocalState::Stored);
        assert_eq!(got.id.as_deref(), Some("0190f0a0-0000-7000-8000-000000000009"));
        assert_eq!(got.from.as_deref(), Some("0x00000000000000000000000000000000000000ab"));
        assert_eq!((got.attempts, got.updated_at), (4, 9_999));
        assert!(got.reason.is_none() && got.ref_.is_none(), "NULL로 바꿀 수 있다");
        assert_eq!(got.vector.as_deref(), Some(axis(3).as_slice()), "벡터는 그대로");
        assert_eq!(got.text, rec.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn quotes_in_values_do_not_break_or_inject_into_filters() {
        let dir = temp_dir("quote");
        let db = LocalDb::open(&dir).await.unwrap();
        db.upsert(&record("req-gggggggggggggggg", 1, None)).await.unwrap();
        db.upsert(&record("req-hhhhhhhhhhhhhhhh", 2, None)).await.unwrap();

        assert!(db.get("x' OR '1'='1", false).await.unwrap().is_none(), "주입으로 다른 행이 잡히면 안 된다");
        assert!(db.get("it's", false).await.unwrap().is_none());

        // 따옴표가 든 값도 저장하고 다시 찾을 수 있다
        let mut odd = record("req-iiiiiiiiiiiiiiii", 3, None);
        odd.cc = "o'brien.com".into();
        db.upsert(&odd).await.unwrap();
        assert_eq!(db.count().await.unwrap(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_ranks_by_cosine_and_reports_similarity() {
        let dir = temp_dir("search");
        let db = LocalDb::open(&dir).await.unwrap();
        // 질의는 축 0. a는 축 0(동일), b는 축 0과 1의 중간, c는 축 2(직교)
        let mut b_vec = vec![0.0; EMBEDDING_DIM];
        b_vec[0] = std::f32::consts::FRAC_1_SQRT_2;
        b_vec[1] = std::f32::consts::FRAC_1_SQRT_2;
        db.upsert(&record("req-search-a000000", 1, Some(axis(0)))).await.unwrap();
        db.upsert(&record("req-search-b000000", 2, Some(b_vec))).await.unwrap();
        db.upsert(&record("req-search-c000000", 3, Some(axis(2)))).await.unwrap();

        let hits = db.search(&axis(0), 3, None).await.unwrap();
        let order: Vec<&str> = hits.iter().map(|h| h.request_id.as_str()).collect();
        assert_eq!(order, ["req-search-a000000", "req-search-b000000", "req-search-c000000"]);
        assert!((hits[0].score - 1.0).abs() < 1e-4, "같은 방향은 1: {}", hits[0].score);
        assert!((hits[1].score - 0.7071).abs() < 1e-3, "45도는 약 0.707: {}", hits[1].score);
        assert!(hits[2].score.abs() < 1e-4, "직교는 0: {}", hits[2].score);
        assert_eq!(hits[0].text, "text of req-search-a000000", "내 글의 원문이 같이 나온다");

        assert_eq!(db.search(&axis(0), 1, None).await.unwrap().len(), 1, "top_k를 지킨다");
        assert!(db.search(&axis(0), 0, None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_skips_rejected_failed_and_vectorless_rows_and_filters_by_cc() {
        let dir = temp_dir("searchfilter");
        let db = LocalDb::open(&dir).await.unwrap();
        db.upsert(&record("req-ok-0000000000000", 1, Some(axis(0)))).await.unwrap();

        let mut rejected = record("req-rejected-00000", 2, Some(axis(0)));
        rejected.state = LocalState::Rejected;
        db.upsert(&rejected).await.unwrap();
        let mut failed = record("req-failed-0000000", 3, Some(axis(0)));
        failed.state = LocalState::Failed;
        db.upsert(&failed).await.unwrap();
        db.upsert(&record("req-novector-00000", 4, None)).await.unwrap();

        let mut other = record("req-other-cc-000000", 5, Some(axis(0)));
        other.cc = "other.org".into();
        other.state = LocalState::Stored;
        db.upsert(&other).await.unwrap();

        let all: Vec<String> = db.search(&axis(0), 10, None).await.unwrap().into_iter().map(|h| h.request_id).collect();
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(all.contains(&"req-ok-0000000000000".to_string()) && all.contains(&"req-other-cc-000000".to_string()));

        let only = db.search(&axis(0), 10, Some("other.org")).await.unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].request_id, "req-other-cc-000000");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_on_an_empty_table_returns_nothing_instead_of_failing() {
        let dir = temp_dir("searchempty");
        let db = LocalDb::open(&dir).await.unwrap();
        assert!(db.search(&axis(0), 5, None).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wrong_dimension_is_refused_on_write_and_on_search() {
        let dir = temp_dir("dims");
        let db = LocalDb::open(&dir).await.unwrap();
        let bad = record("req-badvec-0000000", 1, Some(vec![0.5; 10]));
        assert!(matches!(db.upsert(&bad).await, Err(AppError::InvalidInput(_))));
        assert_eq!(db.count().await.unwrap(), 0);
        assert!(matches!(db.search(&[0.1; 4], 3, None).await, Err(AppError::InvalidInput(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_table_with_a_different_schema_is_refused_not_overwritten() {
        let dir = temp_dir("schemamismatch");
        std::fs::create_dir_all(&dir).unwrap();
        // 다른 컬럼 구성의 같은 이름 테이블을 먼저 만든다
        let other = Arc::new(Schema::new(vec![Field::new("request_id", DataType::Utf8, false)]));
        let conn = connect(dir.to_str().unwrap()).execute().await.unwrap();
        let batch = RecordBatch::try_new(other.clone(), vec![Arc::new(StringArray::from(vec!["x"]))]).unwrap();
        conn.create_table(TABLE, batch).execute().await.unwrap();
        drop(conn);

        let err = LocalDb::open(&dir).await.err().expect("must refuse");
        assert!(matches!(err, AppError::Storage(ref m) if m.contains("unexpected schema")), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn optimize_keeps_every_row_and_search_working_after_many_small_writes() {
        let dir = temp_dir("optimize");
        let db = LocalDb::open(&dir).await.unwrap();
        for i in 0..30u64 {
            let rec = record(&format!("req-opt-{i:012}"), i + 1, Some(axis(i as usize)));
            db.upsert(&rec).await.unwrap();
            db.update_by_local_id(&rec.local_id, &[("state", SqlValue::Str("stored".into()))]).await.unwrap();
        }
        // 파일 수가 바로 줄어드는지는 확인하지 않는다. 합친 새 파일이 생기지만 이전 버전의 파일은 오래된 버전을 정리할 때까지 남는다
        // (측정: 작은 쓰기 60번 뒤 데이터 파일 60개, optimize 뒤 61개). 데이터가 그대로인지만 확인한다.
        db.optimize().await.unwrap();

        assert_eq!(db.count().await.unwrap(), 30);
        let hits = db.search(&axis(7), 1, None).await.unwrap();
        assert_eq!(hits[0].request_id, "req-opt-000000000007");
        let again = LocalDb::open(&dir).await.unwrap();
        assert_eq!(again.count().await.unwrap(), 30);
        assert_eq!(again.by_state(LocalState::Stored, false).await.unwrap().len(), 30);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn max_local_seq_and_changed_since_and_counts() {
        let dir = temp_dir("misc");
        let db = LocalDb::open(&dir).await.unwrap();
        assert_eq!(db.max_local_seq().await.unwrap(), 0);
        db.upsert(&record("req-m1-000000000000", 3, None)).await.unwrap();
        db.upsert(&record("req-m2-000000000000", 12, None)).await.unwrap();
        assert_eq!(db.max_local_seq().await.unwrap(), 12, "문자열 정렬(local-3 > local-12)이 아니라 숫자로 본다");

        // record(): updated_at = 1000 + seq
        let changed = db.changed_since(1_005).await.unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].local_id, "local-12");

        let counts = db.counts_by_state().await.unwrap();
        assert_eq!(counts.get("queued"), Some(&2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn states_round_trip_and_unknown_is_an_error() {
        for s in [
            LocalState::Queued,
            LocalState::Pending,
            LocalState::Stored,
            LocalState::Rejected,
            LocalState::Failed,
            LocalState::Missing,
        ] {
            assert_eq!(LocalState::parse(s.as_str()).unwrap(), s);
        }
        assert!(LocalState::parse("weird").is_err());
        assert!(LocalState::Stored.searchable() && !LocalState::Rejected.searchable());
    }

    #[test]
    fn none_and_empty_convert_in_one_place() {
        assert_eq!(none_to_empty(None), "");
        assert_eq!(none_to_empty(Some("0xab")), "0xab");
        assert_eq!(empty_to_none(""), None);
        assert_eq!(empty_to_none("   "), None);
        assert_eq!(empty_to_none(" 0xab "), Some("0xab".to_string()));
    }

    /// Rust(`to_cache_row`)와 JS(`web/desktop/localdb.ts`)가 같은 행 모양을 쓰는지 확인하는 계약 테스트.
    /// 같은 픽스처를 JS 테스트(`web/desktop/localdb.test.ts`)도 읽는다. 한쪽 모양을 바꾸면 이 테스트나 JS 테스트가 깨진다.
    #[test]
    fn cache_rows_match_the_contract_fixture_shared_with_the_js_side() {
        let full = LocalRecord {
            request_id: "req-contract-0000000001".into(),
            local_id: "local-2".into(),
            id: Some("0190f0a0-0000-7000-8000-000000000002".into()),
            root: Some("0190f0a0-0000-7000-8000-000000000001".into()),
            ref_: Some("0190f0a0-0000-7000-8000-000000000001".into()),
            parent_local: None,
            cc: "example.com".into(),
            from: Some("0x00000000000000000000000000000000000000ab".into()),
            to: Some("0x0000000000000000000000000000000000000002".into()),
            bcc: None,
            text: "답글 본문".into(),
            text_hash: "h-a".into(),
            vector: Some(axis(0)),
            state: LocalState::Stored,
            attempts: 2,
            reason: None,
            created_at: 1_700_000_000_001,
            updated_at: 1_700_000_000_999,
        };
        let bare = LocalRecord {
            request_id: "req-contract-0000000002".into(),
            local_id: "local-3".into(),
            id: None,
            root: None,
            ref_: None,
            parent_local: Some("local-2".into()),
            cc: "example.com".into(),
            from: None,
            to: None,
            bcc: None,
            text: "대기 중인 글".into(),
            text_hash: "h-b".into(),
            vector: None,
            state: LocalState::Queued,
            attempts: 0,
            reason: None,
            created_at: 1_700_000_001_000,
            updated_at: 1_700_000_001_000,
        };
        let fixture: Value = serde_json::from_str(include_str!("../../../web/desktop/fixtures/cache_rows.json")).unwrap();
        let actual = Value::Array(vec![full.to_cache_row(), bare.to_cache_row()]);
        assert_eq!(actual, fixture, "Rust가 내보내는 행이 계약 픽스처와 다르다: {actual:#}");
    }

    #[test]
    fn cache_row_uses_empty_strings_and_leaves_the_vector_out() {
        let mut rec = record("req-cache-000000000", 4, Some(axis(0)));
        rec.to = None;
        rec.id = Some("0190f0a0-0000-7000-8000-000000000004".into());
        let row = rec.to_cache_row();
        assert_eq!(row["to"], "");
        assert_eq!(row["ref"], "");
        assert_eq!(row["bcc"], "");
        assert_eq!(row["from"], "");
        assert_eq!(row["id"], "0190f0a0-0000-7000-8000-000000000004");
        assert_eq!(row["cc"], "example.com");
        assert_eq!(row["state"], "queued");
        assert_eq!(row["created_at"], 1_004);
        assert_eq!(row["data"]["text"], "text of req-cache-000000000");
        assert_eq!(row["data"]["reason"], "");
        assert!(row.get("vector").is_none() && row["data"].get("vector").is_none());
    }
}
