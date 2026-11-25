//! 웹 행동 분석: 사이트 주인의 앱 쪽 (logis-center `modes/analytic.ts` · `analytic.rs` 의 동기화 · 구조화를 옮김, 2026-11-18)
//!
//! 1. **받기**: 서버(`GET /analytic/events`, QR 로그인 세션)에서 (시각, id) 커서로 원시 사건을 받는다. `data` 는 base64(gzip)이고
//!    `premium_discount_analytic::blob::decode`(logis-center BUG-RT-2/3/4 를 고친 코덱)로 푼다.
//! 2. **넣기**: LanceDB `web_events_local`. 두 가드를 지킨다.
//!    - DELTA GUARD: 기기에서 구조화를 마친 행(`updated_at > 0`)은 서버의 원시 사건(`updated_at = 0`)으로 덮어쓰지 않는다.
//!    - TOMBSTONE GUARD: 주인이 지운 사건 id 는 묘비(`web_event_tombstones`)에 남기고, 다음 받기에서 다시 들이지 않는다.
//! 3. **구조화 (Track 2)**: 모델 없는 요약글([`summary_of`]: 사건 · 페이지 종류 · 고른 요소의 글 · 고르지 않은 형제의 글)을
//!    embeddinggemma(768차원, 문서 프롬프트)로 임베딩하고 `updated_at` 을 찍는다. logis-center 는 이 자리에 LLM(Qwen3.5 2B)
//!    구조화를 썼다. 요약 문장은 아니지만 기간 · 페이지 · 선택 대조는 Track 1 이 결정론적으로 낸다.
//! 4. **리포트**: 질의 해석(기간 · 인사이트 · 페이지 · 사건 종류) → 그 기간의 사건으로 퍼널 · 병목(마찰 discount) · 선택 대조
//!    (premium-discount-analytic `funnel`) + 요약글 벡터 검색.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use futures::TryStreamExt;
use lancedb::arrow::arrow_array::types::Float32Type;
use lancedb::arrow::arrow_array::{Array, FixedSizeListArray, Float32Array, Int64Array, RecordBatch, RecordBatchIterator, StringArray};
use lancedb::arrow::arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{connect, DistanceType, Table};
use premium_discount_analytic::event::WebEvent;
use premium_discount_analytic::funnel::{ChoiceParams, FunnelStep, SiteAnalysis};
use premium_discount_analytic::pug::text_of;
use premium_discount_analytic::query::{AnalyticDictionary, AnalyticQuery, QueryParser};
use premium_discount_analytic::{blob, time};
use premium_discount_nav::{page_hash, url_template, MarkType};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::embedding::{l2_normalize, Embedder, EMBEDDING_DIM};
use crate::embeddinggemma::{with_prompt, Role};
use crate::error::AppError;
use crate::local::sql_str;

pub const TABLE: &str = "web_events_local";
pub const TOMBSTONES: &str = "web_event_tombstones";
/// 리포트의 퍼널: 페이지 종류 순서 (사전 `analytic_page_filters` 의 키)
pub const DEFAULT_FUNNEL: [&str; 4] = ["goods_view", "cart", "checkout", "order_complete"];
/// 한 번의 동기화에서 받는 쪽 수 상한 (한 쪽은 서버가 최대 500행)
pub const MAX_PAGES: usize = 20;
const SUMMARY_RELATED: usize = 5;

fn storage(e: impl std::fmt::Display) -> AppError {
    AppError::Storage(e.to_string())
}

pub fn schema() -> SchemaRef {
    let text = |name: &str| Field::new(name, DataType::Utf8, false);
    Arc::new(Schema::new(vec![
        text("id"),
        text("host"),
        text("session"),
        text("type"),
        Field::new("seq", DataType::Int64, false),
        Field::new("created_at", DataType::Int64, false),
        Field::new("updated_at", DataType::Int64, false),
        text("event"),
        text("summary"),
        Field::new(
            "vector",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), EMBEDDING_DIM as i32),
            true,
        ),
    ]))
}

fn tombstone_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![Field::new("id", DataType::Utf8, false), Field::new("deleted_at", DataType::Int64, false)]))
}

/// 서버가 돌려준 원시 행 (`GET /analytic/events` 의 rows)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEventRow {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub session: String,
    #[serde(default)]
    pub seq: i64,
    /// base64(gzip(JSON))
    pub data: String,
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    pub since: i64,
    pub after_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EventsPage {
    pub rows: Vec<RawEventRow>,
    #[serde(default)]
    pub next: Option<Cursor>,
    #[serde(default)]
    pub more: bool,
}

/// 기기의 사건 한 행
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyticRow {
    pub id: String,
    pub host: String,
    pub session: String,
    pub kind: String,
    pub seq: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// 다듬은 사건 (`web-event/1` JSON)
    pub event: String,
    pub summary: String,
    pub vector: Option<Vec<f32>>,
}

impl AnalyticRow {
    pub fn web_event(&self) -> Option<WebEvent> {
        let mut e: WebEvent = serde_json::from_str(&self.event).ok()?;
        e.sid = self.session.clone();
        e.at_ms = self.created_at;
        Some(e)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IngestReport {
    pub received: usize,
    pub inserted: usize,
    /// 이미 구조화된 행이라 덮어쓰지 않음 (DELTA GUARD)
    pub kept_structured: usize,
    /// 지운 사건이라 다시 들이지 않음 (TOMBSTONE GUARD)
    pub blocked_tombstoned: usize,
    /// 풀 수 없거나 모양이 틀린 사건
    pub broken: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticMatch {
    pub id: String,
    pub score: f32,
    #[serde(rename = "type")]
    pub kind: String,
    pub created_at: i64,
    pub summary: String,
}

/// 사건 하나의 요약글 (임베딩하는 글). 모델 없이 만든다: `<사건> | <페이지 종류 · 경로> | <고른 요소의 글> | 고르지 않음: <형제의 글들>`
pub fn summary_of(e: &WebEvent, dict: &AnalyticDictionary) -> String {
    let mut parts: Vec<String> = Vec::new();
    let kind = serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let mut head = kind;
    if e.rage {
        head.push_str(" (rage)");
    }
    if let Some(f) = &e.field {
        head.push_str(&format!(" {f}"));
    }
    if let Some(ms) = e.dwell_ms {
        head.push_str(&format!(" {ms}ms"));
    }
    if let Some(n) = e.flips {
        head.push_str(&format!(" flips {n}"));
    }
    parts.push(head);
    let page = match (dict.page_kind_of(&e.href), url_template(&e.href)) {
        (Some(k), Some(t)) => format!("{k} {t}"),
        (None, Some(t)) => t,
        (Some(k), None) => k,
        (None, None) => String::new(),
    };
    if !page.is_empty() {
        parts.push(page);
    }
    let target_text = e.target.as_ref().map(|t| text_of(&t.pug)).unwrap_or_default();
    if !target_text.is_empty() {
        parts.push(target_text.clone());
    }
    // 목록 항목 안의 요소면 그 항목(어느 상품의 '장바구니 담기'였나)
    if let Some(item) = &e.item {
        let text = text_of(&item.pug);
        if !text.is_empty() && text != target_text {
            parts.push(format!("in: {text}"));
        }
    }
    let related: Vec<String> = e.related.iter().map(|r| text_of(&r.pug)).filter(|t| !t.is_empty()).take(SUMMARY_RELATED).collect();
    if !related.is_empty() {
        parts.push(format!("not chosen: {}", related.join(", ")));
    }
    parts.join(" | ")
}

fn string_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a StringArray, AppError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| AppError::Storage(format!("column {name} is missing or not text")))
}

fn int_col<'a>(b: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, AppError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| AppError::Storage(format!("column {name} is missing or not int64")))
}

fn to_batch(rows: &[AnalyticRow]) -> Result<RecordBatch, AppError> {
    let s = |f: fn(&AnalyticRow) -> &str| Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>()));
    let i = |f: fn(&AnalyticRow) -> i64| Arc::new(Int64Array::from(rows.iter().map(f).collect::<Vec<_>>()));
    for r in rows {
        if r.vector.as_ref().is_some_and(|v| v.len() != EMBEDDING_DIM) {
            return Err(AppError::InvalidInput(format!("analytic vector must have {EMBEDDING_DIM} dims")));
        }
    }
    let vectors = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        rows.iter().map(|r| r.vector.as_ref().map(|v| v.iter().map(|x| Some(*x)).collect::<Vec<_>>())),
        EMBEDDING_DIM as i32,
    );
    RecordBatch::try_new(
        schema(),
        vec![
            s(|r| &r.id),
            s(|r| &r.host),
            s(|r| &r.session),
            s(|r| &r.kind),
            i(|r| r.seq),
            i(|r| r.created_at),
            i(|r| r.updated_at),
            s(|r| &r.event),
            s(|r| &r.summary),
            Arc::new(vectors),
        ],
    )
    .map_err(storage)
}

fn read_rows(batches: &[RecordBatch]) -> Result<Vec<AnalyticRow>, AppError> {
    let mut out = Vec::new();
    for b in batches {
        let (id, host, session, kind, event, summary) =
            (string_col(b, "id")?, string_col(b, "host")?, string_col(b, "session")?, string_col(b, "type")?, string_col(b, "event")?, string_col(b, "summary")?);
        let (seq, created, updated) = (int_col(b, "seq")?, int_col(b, "created_at")?, int_col(b, "updated_at")?);
        let vectors = b.column_by_name("vector").and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
        for i in 0..b.num_rows() {
            let vector = vectors.filter(|v| !v.is_null(i)).and_then(|v| {
                v.value(i).as_any().downcast_ref::<Float32Array>().map(|a| a.values().to_vec())
            });
            out.push(AnalyticRow {
                id: id.value(i).to_string(),
                host: host.value(i).to_string(),
                session: session.value(i).to_string(),
                kind: kind.value(i).to_string(),
                seq: seq.value(i),
                created_at: created.value(i),
                updated_at: updated.value(i),
                event: event.value(i).to_string(),
                summary: summary.value(i).to_string(),
                vector,
            });
        }
    }
    Ok(out)
}

async fn open_table(conn: &lancedb::Connection, name: &str, schema: SchemaRef) -> Result<Table, AppError> {
    let names = conn.table_names().execute().await.map_err(storage)?;
    if names.iter().any(|n| n == name) {
        let table = conn.open_table(name).execute().await.map_err(storage)?;
        let existing = table.schema().await.map_err(storage)?;
        if crate::local::signature(&existing) != crate::local::signature(&schema) {
            return Err(AppError::Storage(format!("table {name} has an unexpected schema")));
        }
        Ok(table)
    } else {
        conn.create_empty_table(name, schema).execute().await.map_err(storage)
    }
}

fn host_filter(host: &str) -> String {
    format!("host = {}", sql_str(&host.trim().to_ascii_lowercase()))
}

/// 시각 컷오프 묘비의 id (사건 id 묘비와 같은 표에 둔다). 사건 id 는 UUID 라 겹치지 않는다
pub fn cutoff_key(host: &str) -> String {
    format!("cutoff:{}", host.trim().to_ascii_lowercase())
}

fn range_filter(range: Option<(i64, i64)>) -> String {
    match range {
        Some((s, e)) => format!(" AND created_at >= {s} AND created_at <= {e}"),
        None => String::new(),
    }
}

/// 기기의 사건 저장소 (LanceDB 테이블 둘: 사건 · 묘비)
pub struct AnalyticStore {
    table: Table,
    tombstones: Table,
}

impl AnalyticStore {
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, AppError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(storage)?;
        let uri = dir.to_str().ok_or_else(|| AppError::Storage("analytic db path is not valid UTF-8".into()))?;
        let conn = connect(uri).execute().await.map_err(storage)?;
        Ok(Self { table: open_table(&conn, TABLE, schema()).await?, tombstones: open_table(&conn, TOMBSTONES, tombstone_schema()).await? })
    }

    async fn query(&self, filter: String, with_vector: bool, limit: Option<usize>) -> Result<Vec<AnalyticRow>, AppError> {
        let mut q = self.table.query().only_if(filter);
        if !with_vector {
            let cols: Vec<String> = schema().fields().iter().map(|f| f.name().clone()).filter(|n| n != "vector").collect();
            q = q.select(Select::columns(&cols.iter().map(String::as_str).collect::<Vec<_>>()));
        }
        if let Some(n) = limit {
            q = q.limit(n);
        }
        let batches: Vec<RecordBatch> = q.execute().await.map_err(storage)?.try_collect().await.map_err(storage)?;
        read_rows(&batches)
    }

    pub async fn count(&self) -> Result<usize, AppError> {
        self.table.count_rows(None).await.map_err(storage)
    }

    pub async fn get(&self, id: &str) -> Result<Option<AnalyticRow>, AppError> {
        Ok(self.query(format!("id = {}", sql_str(id)), true, Some(1)).await?.into_iter().next())
    }

    async fn tombstoned(&self, ids: &[&str]) -> Result<HashSet<String>, AppError> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let list = ids.iter().map(|i| sql_str(i)).collect::<Vec<_>>().join(", ");
        let batches: Vec<RecordBatch> = self
            .tombstones
            .query()
            .only_if(format!("id IN ({list})"))
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        let mut out = HashSet::new();
        for b in &batches {
            let col = string_col(b, "id")?;
            for i in 0..b.num_rows() {
                out.insert(col.value(i).to_string());
            }
        }
        Ok(out)
    }

    async fn existing_updated(&self, ids: &[&str]) -> Result<BTreeMap<String, i64>, AppError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let list = ids.iter().map(|i| sql_str(i)).collect::<Vec<_>>().join(", ");
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(format!("id IN ({list})"))
            .select(Select::columns(&["id", "updated_at"]))
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        let mut out = BTreeMap::new();
        for b in &batches {
            let id_col = string_col(b, "id")?;
            let upd_col = b
                .column_by_name("updated_at")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| AppError::Storage("events table is missing updated_at".into()))?;
            for i in 0..b.num_rows() {
                out.insert(id_col.value(i).to_string(), upd_col.value(i));
            }
        }
        Ok(out)
    }

    /// 사이트의 시각 컷오프 묘비 (`cutoff:<host>` 행의 deleted_at): 이 시각 이하에 생긴 사건은 다시 들이지 않는다
    pub async fn cutoff(&self, host: &str) -> Result<Option<i64>, AppError> {
        let key = cutoff_key(host);
        let batches: Vec<RecordBatch> = self
            .tombstones
            .query()
            .only_if(format!("id = {}", sql_str(&key)))
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        let mut out: Option<i64> = None;
        for b in &batches {
            let col = b
                .column_by_name("deleted_at")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| AppError::Storage("tombstone table is missing deleted_at".into()))?;
            for i in 0..b.num_rows() {
                out = Some(out.map_or(col.value(i), |o| o.max(col.value(i))));
            }
        }
        Ok(out)
    }

    /// 사이트의 사건 중 `before_ms` 이하에 생긴 것을 모두 지우고 시각 컷오프 묘비를 남긴다 (시나리오 5 WA-12).
    /// 묘비는 사건 id 묘비와 같은 표에 `id = "cutoff:<host>"`, `deleted_at = max(전 값, before_ms)` 로 둔다: 표의 모양을 바꾸지
    /// 않으므로 예전 폴더를 그대로 연다. 다음 받기([`AnalyticStore::ingest`])가 이 시각 이하의 사건을 막는다. 지운 수를 돌려준다
    pub async fn forget_before(&self, host: &str, before_ms: i64) -> Result<usize, AppError> {
        let host = host.trim().to_ascii_lowercase();
        let keep = self.cutoff(&host).await?.map_or(before_ms, |c| c.max(before_ms));
        let batch = RecordBatch::try_new(
            tombstone_schema(),
            vec![Arc::new(StringArray::from(vec![cutoff_key(&host)])), Arc::new(Int64Array::from(vec![keep]))],
        )
        .map_err(storage)?;
        let reader = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], tombstone_schema());
        let mut merge = self.tombstones.merge_insert(&["id"]);
        merge.when_matched_update_all(None).when_not_matched_insert_all();
        merge.execute(Box::new(reader)).await.map_err(storage)?;
        let filter = format!("{} AND created_at <= {keep}", host_filter(&host));
        let before = self.table.count_rows(Some(filter.clone())).await.map_err(storage)?;
        self.table.delete(&filter).await.map_err(storage)?;
        Ok(before)
    }

    /// 서버의 원시 행을 넣는다. 지운 사건(id 묘비)과 시각 컷오프 이하의 사건은 다시 들이지 않고, 구조화된 행은 덮어쓰지 않는다
    pub async fn ingest(&self, host: &str, rows: &[RawEventRow]) -> Result<IngestReport, AppError> {
        let host = host.trim().to_ascii_lowercase();
        let mut report = IngestReport { received: rows.len(), ..Default::default() };
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        let tombs = self.tombstoned(&ids).await?;
        let cutoff = self.cutoff(&host).await?;
        let existing = self.existing_updated(&ids).await?;
        let mut fresh: Vec<AnalyticRow> = Vec::new();
        for r in rows {
            if tombs.contains(&r.id) || cutoff.is_some_and(|c| r.created_at <= c) {
                report.blocked_tombstoned += 1;
                continue;
            }
            if existing.get(&r.id).is_some_and(|u| *u > 0) {
                report.kept_structured += 1;
                continue;
            }
            let decoded = blob::decode(&Value::String(r.data.clone()));
            let mut obj = Value::Object(decoded);
            if let Some(o) = obj.as_object_mut() {
                o.insert("sid".into(), Value::String(r.session.clone()));
            }
            let event = serde_json::from_value::<WebEvent>(obj).ok().and_then(WebEvent::sanitized);
            let Some(mut event) = event else {
                report.broken += 1;
                continue;
            };
            event.sid = String::new();
            event.at_ms = r.created_at;
            fresh.push(AnalyticRow {
                id: r.id.clone(),
                host: host.clone(),
                session: r.session.clone(),
                kind: r.kind.clone(),
                seq: r.seq,
                created_at: r.created_at,
                updated_at: 0,
                event: serde_json::to_string(&event).map_err(storage)?,
                summary: String::new(),
                vector: None,
            });
        }
        if !fresh.is_empty() {
            report.inserted = fresh.len();
            let batch = to_batch(&fresh)?;
            let reader = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], schema());
            let mut merge = self.table.merge_insert(&["id"]);
            // DELTA GUARD 를 저장소에서도: 원시 행끼리만 바꾼다
            merge.when_matched_update_all(Some("target.updated_at = 0".into())).when_not_matched_insert_all();
            merge.execute(Box::new(reader)).await.map_err(storage)?;
        }
        Ok(report)
    }

    /// 다음 받기의 커서: 이 사이트에서 받은 가장 늦은 (시각, id). 묘비만 남은 사건도 커서를 넘긴다.
    /// 시각 컷오프가 그보다 늦으면 컷오프부터 받는다 (지운 구간을 다시 내려받지 않는다)
    pub async fn cursor(&self, host: &str) -> Result<Cursor, AppError> {
        let batches: Vec<RecordBatch> = self
            .table
            .query()
            .only_if(host_filter(host))
            .select(Select::columns(&["id", "created_at"]))
            .execute()
            .await
            .map_err(storage)?
            .try_collect()
            .await
            .map_err(storage)?;
        let mut best: Option<(i64, String)> = None;
        for b in &batches {
            let id_col = string_col(b, "id")?;
            let ts_col = b
                .column_by_name("created_at")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
                .ok_or_else(|| AppError::Storage("events table is missing created_at".into()))?;
            for i in 0..b.num_rows() {
                let cand = (ts_col.value(i), id_col.value(i).to_string());
                if best.as_ref().is_none_or(|cur| cand > *cur) {
                    best = Some(cand);
                }
            }
        }
        let seen = best
            .map(|(since, after_id)| Cursor { since, after_id })
            .unwrap_or_default();
        Ok(match self.cutoff(host).await? {
            Some(c) if c > seen.since => Cursor { since: c, after_id: String::new() },
            _ => seen,
        })
    }

    /// 아직 구조화하지 않은 행 (updated_at = 0)
    pub async fn pending(&self, host: Option<&str>, limit: usize) -> Result<Vec<AnalyticRow>, AppError> {
        let mut filter = "updated_at = 0".to_string();
        if let Some(h) = host {
            filter = format!("{} AND {filter}", host_filter(h));
        }
        self.query(filter, false, Some(limit)).await
    }

    /// 구조화: 요약글을 만들고 임베딩해 `updated_at = now_ms` 를 찍는다. 처리한 수를 돌려준다
    pub async fn structure<E: Embedder + ?Sized>(&self, embedder: &E, host: Option<&str>, limit: usize, now_ms: i64) -> Result<usize, AppError> {
        let dict = AnalyticDictionary::builtin();
        let mut done: Vec<AnalyticRow> = Vec::new();
        for mut row in self.pending(host, limit).await? {
            let Some(event) = row.web_event() else { continue };
            row.summary = summary_of(&event, dict);
            row.vector = Some(l2_normalize(embedder.embed(&with_prompt(Role::Document, &row.summary))?)?);
            row.updated_at = now_ms.max(1);
            done.push(row);
        }
        if !done.is_empty() {
            let batch = to_batch(&done)?;
            let reader = RecordBatchIterator::new(vec![Ok::<_, ArrowError>(batch)], schema());
            let mut merge = self.table.merge_insert(&["id"]);
            merge.when_matched_update_all(None).when_not_matched_insert_all();
            merge.execute(Box::new(reader)).await.map_err(storage)?;
        }
        Ok(done.len())
    }

    /// 사건을 지운다: 묘비를 남기고 행을 지운다 (다음 받기에서 다시 들어오지 않는다)
    pub async fn forget(&self, ids: &[String], now_ms: i64) -> Result<usize, AppError> {
        let ids: Vec<&str> = ids.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let already = self.tombstoned(&ids).await?;
        let new: Vec<&str> = ids.iter().copied().filter(|i| !already.contains(*i)).collect();
        if !new.is_empty() {
            let batch = RecordBatch::try_new(
                tombstone_schema(),
                vec![Arc::new(StringArray::from(new.clone())), Arc::new(Int64Array::from(vec![now_ms; new.len()]))],
            )
            .map_err(storage)?;
            self.tombstones.add(batch).execute().await.map_err(storage)?;
        }
        let list = ids.iter().map(|i| sql_str(i)).collect::<Vec<_>>().join(", ");
        let before = self.table.count_rows(Some(format!("id IN ({list})"))).await.map_err(storage)?;
        self.table.delete(&format!("id IN ({list})")).await.map_err(storage)?;
        Ok(before)
    }

    /// 사이트의 사건 (기간 안, 시각 · 순서대로)
    pub async fn events(&self, host: &str, range: Option<(i64, i64)>) -> Result<Vec<WebEvent>, AppError> {
        let mut rows = self.query(format!("{}{}", host_filter(host), range_filter(range)), false, None).await?;
        rows.sort_by(|a, b| (a.created_at, a.seq, &a.id).cmp(&(b.created_at, b.seq, &b.id)));
        Ok(rows.iter().filter_map(AnalyticRow::web_event).collect())
    }

    /// 요약글 벡터 검색 (코사인). 구조화된 행만
    pub async fn search(&self, vector: &[f32], top_k: usize, host: &str, range: Option<(i64, i64)>) -> Result<Vec<AnalyticMatch>, AppError> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let filter = format!("vector IS NOT NULL AND {}{}", host_filter(host), range_filter(range));
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
            let (id, kind, summary) = (string_col(b, "id")?, string_col(b, "type")?, string_col(b, "summary")?);
            let created = int_col(b, "created_at")?;
            let distance = b
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .ok_or_else(|| AppError::Storage("search result has no _distance column".into()))?;
            for i in 0..b.num_rows() {
                out.push(AnalyticMatch {
                    id: id.value(i).to_string(),
                    score: 1.0 - distance.value(i),
                    kind: kind.value(i).to_string(),
                    created_at: created.value(i),
                    summary: summary.value(i).to_string(),
                });
            }
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementRow {
    pub element: String,
    pub label: String,
    pub episodes: u32,
    pub premium: u32,
    pub discount: u32,
    pub mean_friction: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceRow {
    pub label: String,
    /// premium | discount
    pub mark: String,
    pub picks: u32,
    pub exposures: u32,
    pub z: f64,
}

/// `cmd_analytic_report` 의 결과
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticReport {
    pub host: String,
    pub query: AnalyticQuery,
    pub events: usize,
    pub sessions: u32,
    pub funnel: Vec<FunnelStep>,
    /// 마찰이 큰 요소 (discount 가 난 것, 많은 순)
    pub bottlenecks: Vec<ElementRow>,
    /// 띠를 넘은 선택 대조 (고른 것 vs 지나친 것)
    pub choices: Vec<ChoiceRow>,
    /// 요약글 벡터 검색
    pub matches: Vec<AnalyticMatch>,
    /// 사람이 읽는 몇 줄 (raon-zena 의 [참고] 줄로도 쓴다)
    pub lines: Vec<String>,
    /// 표본 신뢰도 (시나리오 5 WA-11): 세션이 premium-discount-analytic 의 `MIN_FUNNEL_SESSIONS` 보다 적으면
    /// InsufficientSample 이고, 줄에는 오르내림을 단정하는 말 대신 표본이 모자라다고 적는다
    pub confidence: premium_discount_analytic::report::ReportConfidence,
}

/// 해석한 질의와 그 기간의 사건으로 리포트를 만든다 (모델 없음). `matches` 는 부르는 쪽이 벡터 검색한 결과
pub fn build_report(host: &str, query: AnalyticQuery, events: &[WebEvent], matches: Vec<AnalyticMatch>) -> AnalyticReport {
    let dict = AnalyticDictionary::builtin();
    let mut site = SiteAnalysis::new(host);
    let mut sessions: BTreeMap<&str, Vec<WebEvent>> = BTreeMap::new();
    for e in events {
        sessions.entry(e.sid.as_str()).or_default().push(e.clone());
    }
    for (_, evs) in sessions.iter_mut() {
        evs.sort_by_key(|e| (e.seq, e.t_ms));
        site.add_session(evs);
    }
    // 퍼널: 페이지 종류마다 가장 많은 세션이 지난 페이지
    let mut by_kind: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for e in events.iter().filter(|e| e.kind == premium_discount_analytic::event::EventKind::Enter) {
        if let (Some(kind), Some(page)) = (dict.page_kind_of(&e.href), page_hash(&e.href)) {
            *by_kind.entry(kind).or_default().entry(page).or_default() += 1;
        }
    }
    let steps: Vec<String> = DEFAULT_FUNNEL
        .iter()
        .filter_map(|k| by_kind.get(*k).and_then(|pages| pages.iter().max_by_key(|(_, n)| **n).map(|(p, _)| p.clone())))
        .collect();
    let funnel = if steps.len() >= 2 { site.funnel(&steps) } else { Vec::new() };
    let bottlenecks: Vec<ElementRow> = site
        .bottlenecks()
        .into_iter()
        .take(5)
        .map(|(h, t)| ElementRow { element: h.to_string(), label: t.label.clone(), episodes: t.episodes, premium: t.premium, discount: t.discount, mean_friction: t.mean_friction() })
        .collect();
    let p = ChoiceParams::default();
    let mut choices: Vec<ChoiceRow> = site
        .choices
        .items
        .iter()
        .filter_map(|(k, t)| {
            let mark = t.decide(&p)?;
            Some(ChoiceRow {
                label: site.choices.labels.get(k).cloned().unwrap_or_default(),
                mark: if mark == MarkType::Premium { "premium" } else { "discount" }.into(),
                picks: t.picks,
                exposures: t.exposures,
                z: t.z().unwrap_or(0.0),
            })
        })
        .collect();
    choices.sort_by(|a, b| b.z.abs().total_cmp(&a.z.abs()));
    let mut lines = Vec::new();
    if let Some(r) = &query.range {
        lines.push(format!("기간 {} ~ {} ({})", r.start, r.end, r.label));
    }
    for w in funnel.windows(2) {
        if w[0].reached > 0 && w[0].dropped > 0 {
            lines.push(format!("{} → {}: {}/{} 세션이 다음 단계로 가지 못함", w[0].label, w[1].label, w[0].dropped, w[0].reached));
        }
    }
    for b in bottlenecks.iter().take(3) {
        lines.push(format!("병목 '{}': discount {} · premium {} (평균 마찰 {:.1})", b.label, b.discount, b.premium, b.mean_friction));
    }
    for c in choices.iter().take(3) {
        lines.push(format!("선택 '{}': {} ({}/{}번 골라짐)", c.label, c.mark, c.picks, c.exposures));
    }
    let confidence = premium_discount_analytic::report::ReportConfidence::of(site.sessions, None);
    if let premium_discount_analytic::report::ReportConfidence::InsufficientSample { sessions, min_required } = &confidence {
        lines.push(format!("표본 부족: 세션 {sessions}건 (최소 {min_required}건)이라 변화는 판단하지 않습니다"));
    }
    AnalyticReport { host: host.to_string(), query, events: events.len(), sessions: site.sessions, funnel, bottlenecks, choices, matches, lines, confidence }
}

/// 사전 일치 해석기 (한 번 만든다)
pub fn query_parser() -> &'static QueryParser {
    static PARSER: OnceLock<QueryParser> = OnceLock::new();
    PARSER.get_or_init(|| QueryParser::lexical(AnalyticDictionary::builtin()))
}

/// 질의 → 기간 (리포트의 사건 범위). 기간 말이 없으면 None = 전 구간
pub fn parse_query(text: &str, lang: &str, now_ms: i64) -> AnalyticQuery {
    query_parser().parse(text, lang, now_ms)
}

/// 리포트에 쓰는 현재 시각 (평가 시각 고정 `PD_EVAL_NOW_MS` 를 따른다)
pub fn now_ms() -> i64 {
    time::now_ms()
}

/// Damped-Holt 시계열 게이트 (`premium-discount-sds::temporal`): 단순 트래픽 요동과 실질 UI 마찰 급변(`Held`)을 구분한다.
pub fn evaluate_conversion_temporal_gate(
    history: &[f64],
    observed: f64,
    sds_sd: Option<f64>,
) -> premium_discount_sds::temporal::TemporalVerdict {
    premium_discount_sds::temporal::temporal_gate(
        history,
        observed,
        sds_sd,
        &[],
        &[],
        &premium_discount_sds::temporal::GateConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_analytic::blob::encode_base64;
    use serde_json::json;

    struct HashEmbedder;
    impl Embedder for HashEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
            let mut v = vec![0f32; EMBEDDING_DIM];
            for (i, b) in text.bytes().enumerate() {
                v[(b as usize * 31 + i) % EMBEDDING_DIM] += 1.0;
            }
            Ok(v)
        }
    }

    fn raw(id: &str, created_at: i64, event: Value) -> RawEventRow {
        let kind = event["type"].as_str().unwrap().to_string();
        RawEventRow {
            id: id.into(),
            kind,
            session: "0xsession".into(),
            seq: event["seq"].as_i64().unwrap_or(0),
            data: encode_base64(event.as_object().unwrap()),
            created_at,
            updated_at: 0,
        }
    }

    fn click(seq: i64, text: &str) -> Value {
        json!({"schema": "web-event/1", "type": "click", "href": "https://shop.example.com/order/cart", "seq": seq, "t_ms": seq * 1000,
               "target": {"chain": [{"tag": "button", "id": format!("b{seq}")}], "pug": format!("button | {text}")},
               "related": [{"chain": [{"tag": "button", "id": "x"}], "pug": "button | 쿠폰 적용"}]})
    }

    #[tokio::test]
    async fn ingest_guards_and_structure() {
        let dir = crate::local::test_support::temp_dir("analytic");
        let store = AnalyticStore::open(&dir).await.unwrap();
        let rows = vec![raw("e1", 1000, click(1, "간편결제")), raw("e2", 2000, click(2, "주문하기")), RawEventRow { data: "!!".into(), ..raw("e3", 3000, click(3, "x")) }];
        let r = store.ingest("Shop.Example.com", &rows).await.unwrap();
        assert_eq!((r.received, r.inserted, r.broken), (3, 2, 1));
        assert_eq!(store.cursor("shop.example.com").await.unwrap(), Cursor { since: 2000, after_id: "e2".into() });

        // 구조화: 요약글 + 768 벡터 + updated_at
        assert_eq!(store.structure(&HashEmbedder, Some("shop.example.com"), 10, 5_000).await.unwrap(), 2);
        let e1 = store.get("e1").await.unwrap().unwrap();
        assert_eq!(e1.updated_at, 5_000);
        assert_eq!(e1.summary, "click | cart shop.example.com/order/cart | 간편결제 | not chosen: 쿠폰 적용");
        assert_eq!(e1.vector.as_ref().map(Vec::len), Some(EMBEDDING_DIM));

        // DELTA GUARD: 같은 사건이 서버에서 원시로 다시 와도 구조화된 행을 덮지 않는다
        let again = store.ingest("shop.example.com", &rows[..1]).await.unwrap();
        assert_eq!((again.inserted, again.kept_structured), (0, 1));
        assert_eq!(store.get("e1").await.unwrap().unwrap().updated_at, 5_000);

        // TOMBSTONE GUARD: 지운 사건은 다시 들이지 않는다
        assert_eq!(store.forget(&["e2".to_string()], 6_000).await.unwrap(), 1);
        let back = store.ingest("shop.example.com", &rows[1..2]).await.unwrap();
        assert_eq!((back.inserted, back.blocked_tombstoned), (0, 1));
        assert!(store.get("e2").await.unwrap().is_none());
        assert_eq!(store.count().await.unwrap(), 1);

        // 검색: 구조화된 행만
        let q = l2_normalize(HashEmbedder.embed(&with_prompt(Role::Document, &e1.summary)).unwrap()).unwrap();
        let hits = store.search(&q, 5, "shop.example.com", None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.99, "{:?}", hits[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // WA-12: 시각 컷오프 묘비(cutoff:<host>)와 사건 id 묘비가 한 표에서 함께 막는다. 다시 받아도 지운 구간 · 지운 사건은 0건,
    // 구조화된 행은 덮지 않고, 깨진 행은 broken 으로 세고 나머지는 넣는다
    #[tokio::test]
    async fn wa12_cutoff_and_id_tombstones_block_reinsertion_together() {
        let dir = crate::local::test_support::temp_dir("analytic-cutoff");
        let store = AnalyticStore::open(&dir).await.unwrap();
        let rows = vec![
            raw("e1", 1_000, click(1, "간편결제")),
            raw("e2", 2_000, click(2, "주문하기")),
            raw("e3", 3_000, click(3, "쿠폰")),
            raw("e4", 4_000, click(4, "결제")),
            RawEventRow { data: "!!".into(), ..raw("e5", 5_000, click(5, "x")) },
        ];
        let r = store.ingest("shop.example.com", &rows).await.unwrap();
        assert_eq!((r.inserted, r.broken), (4, 1));
        assert_eq!(store.structure(&HashEmbedder, Some("shop.example.com"), 10, 9_000).await.unwrap(), 4);

        // forget(host, before_ms): 2,000 이하 사건 둘을 지우고 컷오프를 남긴다. forget(ids): e4 를 지운다
        assert_eq!(store.forget_before("Shop.Example.com", 2_000).await.unwrap(), 2);
        assert_eq!(store.cutoff("shop.example.com").await.unwrap(), Some(2_000));
        assert_eq!(store.forget(&["e4".to_string()], 9_500).await.unwrap(), 1);
        // 더 이른 컷오프로 다시 불러도 컷오프는 줄지 않는다
        store.forget_before("shop.example.com", 1_500).await.unwrap();
        assert_eq!(store.cutoff("shop.example.com").await.unwrap(), Some(2_000));
        assert_eq!(store.cutoff("other.example.com").await.unwrap(), None, "사이트마다 따로");

        // 서버가 같은 다섯 행을 다시 보낸다
        let again = store.ingest("shop.example.com", &rows).await.unwrap();
        assert_eq!(again.inserted, 0, "{again:?}");
        assert_eq!(again.blocked_tombstoned, 3, "컷오프 이하 둘 + id 묘비 하나");
        assert_eq!(again.kept_structured, 1, "e3 는 구조화되어 있어 덮지 않는다");
        assert_eq!(again.broken, 1);
        assert_eq!(store.count().await.unwrap(), 1);
        assert_eq!(store.get("e3").await.unwrap().unwrap().updated_at, 9_000);
        // 커서는 컷오프보다 앞으로 돌아가지 않는다
        assert!(store.cursor("shop.example.com").await.unwrap().since >= 2_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_report_finds_the_bottleneck_in_the_question_range() {
        let dir = crate::local::test_support::temp_dir("analytic-report");
        let store = AnalyticStore::open(&dir).await.unwrap();
        // 2026-11-17 (어제, KST) 의 세션 40개: 30개는 장바구니 → 결제 → 완료, 10개는 결제에서 쿠폰 칸을 헤매다 떠난다
        let day = 1_794_841_200_000i64 + 3_600_000;
        let mut rows = Vec::new();
        for s in 0..40 {
            let struggle = s % 4 == 3;
            let session = format!("0xs{s}");
            let mut seq = 0;
            let mut push = |v: Value, rows: &mut Vec<RawEventRow>| {
                seq += 1;
                let mut v = v;
                v["seq"] = json!(seq);
                v["t_ms"] = json!(seq * 2000);
                let mut r = raw(&format!("s{s}-{seq}"), day + s * 60_000 + seq, v);
                r.session = session.clone();
                rows.push(r);
            };
            let page = |href: &str| json!({"type": "enter", "href": href});
            let button = |href: &str, id: &str, text: &str, related: Option<(&str, &str)>| {
                let mut v = json!({"type": "click", "href": href, "target": {"chain": [{"tag": "button", "id": id}], "pug": format!("button | {text}")}});
                if let Some((rid, rt)) = related {
                    v["related"] = json!([{"chain": [{"tag": "button", "id": rid}], "pug": format!("button | {rt}")}]);
                }
                v
            };
            push(page("https://shop.example.com/goods/view/1234"), &mut rows);
            push(button("https://shop.example.com/goods/view/1234", "add", "장바구니 담기", None), &mut rows);
            push(page("https://shop.example.com/order/cart"), &mut rows);
            push(button("https://shop.example.com/order/cart", "order", "주문하기", None), &mut rows);
            push(page("https://shop.example.com/order/checkout"), &mut rows);
            if struggle {
                push(json!({"type": "scroll", "href": "https://shop.example.com/order/checkout", "flips": 4}), &mut rows);
                push(button("https://shop.example.com/order/checkout", "coupon", "쿠폰 적용", Some(("easy-pay", "간편결제"))), &mut rows);
                let mut rage = button("https://shop.example.com/order/checkout", "coupon", "쿠폰 적용", None);
                rage["rage"] = json!(true);
                push(rage, &mut rows);
                for _ in 0..3 {
                    push(json!({"type": "change", "href": "https://shop.example.com/order/checkout", "field": "text", "target": {"chain": [{"tag": "input", "id": "coupon-code"}], "pug": "input"}}), &mut rows);
                }
                push(json!({"type": "leave", "href": "https://shop.example.com/order/checkout", "dirty": 1}), &mut rows);
            } else {
                push(button("https://shop.example.com/order/checkout", "easy-pay", "간편결제", Some(("coupon", "쿠폰 적용"))), &mut rows);
                push(json!({"type": "submit", "href": "https://shop.example.com/order/checkout"}), &mut rows);
                push(page("https://shop.example.com/order/complete"), &mut rows);
            }
        }
        // 그저께 사건 하나 (기간 밖)
        rows.push(raw("old", day - 86_400_000, click(1, "옛날")));
        assert_eq!(store.ingest("shop.example.com", &rows).await.unwrap().broken, 0);

        let now = 1_794_970_800_000; // 2026-11-18 12:00 KST
        let query = parse_query("어제 결제 이탈 원인이 뭐야?", "ko", now);
        assert_eq!(query.time_key.as_deref(), Some("yesterday"));
        assert_eq!(query.insight.as_deref(), Some("conversion_insight"));
        let range = query.range.as_ref().map(|r| (r.start_ms, r.end_ms));
        let events = store.events("shop.example.com", range).await.unwrap();
        assert_eq!(events.len(), rows.len() - 1, "그저께 사건은 기간 밖");
        let report = build_report("shop.example.com", query, &events, Vec::new());
        assert_eq!(report.sessions, 40);
        assert_eq!(report.funnel.iter().map(|s| s.reached).collect::<Vec<_>>(), [40, 40, 40, 30], "{:?}", report.funnel);
        assert_eq!(report.funnel[2].dropped, 10);
        assert_eq!(report.bottlenecks.first().map(|b| b.label.as_str()), Some("쿠폰 적용"), "{:?}", report.bottlenecks);
        assert!(report.choices.iter().any(|c| c.label == "간편결제" && c.mark == "premium"), "{:?}", report.choices);
        assert!(report.lines.iter().any(|l| l.contains("10/40")), "{:?}", report.lines);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn temporal_gate_holds_sudden_conversion_drop_spike() {
        let hist: Vec<f64> = (0..18).map(|i| 0.20 + 0.01 * ((i % 3) as f64 - 1.0)).collect();
        let spike = evaluate_conversion_temporal_gate(&hist, 0.72, Some(0.02));
        assert_eq!(spike.outcome, premium_discount_sds::temporal::TemporalOutcome::Held);
        assert!(!spike.passes());
    }
}
