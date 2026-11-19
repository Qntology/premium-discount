//! 원본 `index.ts` 의 평가 대기열: www 가 eval_queue 에 넣은 항목을 욕설 검사 뒤 evaluations 와 Vectorize 로 옮긴다.
//!
//! 상태 흐름: pending → processing → (삭제: 저장 완료) | rejected (욕설) | failed (재시도 한도 초과).
//! 저장 순서는 D1(INSERT OR IGNORE) → Vectorize(upsert) → 대기열 삭제. 예외 경계(무엇을 잡고 무엇이 새는지)는 원본 그대로다.

use js_sys::{Float32Array, Uint8Array};
use premium_discount_worker_common::js::{self, Env, Js};
use premium_discount_worker_common::jsv::{strict_eq, Throw, R, V};
use premium_discount_worker_common::arr;

use crate::moderation::moderate;
use crate::vector::to_vector_bytes;

const BATCH_SIZE: f64 = 20.0; // 한 번의 실행에서 처리할 최대 건수 (임시값)
const LEASE_MS: f64 = 5.0 * 60.0 * 1000.0; // processing 상태 만료
const MAX_ATTEMPTS: f64 = 10.0; // 이 횟수를 넘으면 failed (임시값)
const RETAIN_MS: f64 = 7.0 * 24.0 * 60.0 * 60.0 * 1000.0; // rejected·failed 행을 남겨 두는 기간
/// text_hash 는 NOT NULL 이라 지울 때 이 값으로 덮어쓴다
pub const MASKED_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const SQL_DUE: &str = "SELECT \"id\" FROM eval_queue\n\t\t WHERE \"state\" = 'pending' OR (\"state\" = 'processing' AND \"lease_until\" < ?1)\n\t\t ORDER BY \"created_at\" LIMIT ?2";
const SQL_PURGE: &str = "DELETE FROM eval_queue WHERE \"state\" IN ('rejected', 'failed') AND \"updated_at\" < ?1";
const SQL_CLAIM: &str = "UPDATE eval_queue\n\t\t SET \"state\" = 'processing', \"lease_until\" = ?1, \"attempts\" = \"attempts\" + 1, \"updated_at\" = ?2\n\t\t WHERE \"id\" = ?3 AND (\"state\" = 'pending' OR (\"state\" = 'processing' AND \"lease_until\" < ?2))";
const SQL_ROW: &str = "SELECT \"id\", \"request_id\", \"from\", \"to\", \"cc\", \"bcc\", \"ref\", \"root\", \"text\", \"vector\", \"attempts\", \"created_at\"\n\t\t FROM eval_queue WHERE \"id\" = ?1";
const SQL_PARENT_STATE: &str = "SELECT \"state\" FROM eval_queue WHERE \"id\" = ?1";
const SQL_STORED: &str = "SELECT 1 AS ok FROM evaluations WHERE \"id\" = ?1";
const SQL_INSERT_EVAL: &str = "INSERT OR IGNORE INTO evaluations (\"id\", \"from\", \"to\", \"cc\", \"bcc\", \"ref\", \"root\", \"created_at\", \"updated_at\", \"request_id\")\n\t\t VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)";
const SQL_DELETE_QUEUED: &str = "DELETE FROM eval_queue WHERE \"id\" = ?1";
const SQL_REJECT: &str = "UPDATE eval_queue SET \"state\" = 'rejected', \"text\" = NULL, \"vector\" = NULL, \"text_hash\" = ?1, \"bcc\" = NULL,\n\t\t \"lease_until\" = NULL, \"last_error\" = ?2, \"updated_at\" = ?3 WHERE \"id\" = ?4";
const SQL_RELEASE: &str = "UPDATE eval_queue SET \"state\" = 'pending', \"lease_until\" = NULL,\n\t\t \"attempts\" = CASE WHEN \"attempts\" > 0 THEN \"attempts\" - 1 ELSE 0 END, \"updated_at\" = ?1 WHERE \"id\" = ?2";
const SQL_PENDING: &str = "UPDATE eval_queue SET \"state\" = 'pending', \"lease_until\" = NULL, \"last_error\" = ?1, \"updated_at\" = ?2 WHERE \"id\" = ?3";
const SQL_DELETE_EVAL: &str = "DELETE FROM evaluations WHERE \"id\" = ?1";
const SQL_FAIL: &str = "UPDATE eval_queue SET \"state\" = 'failed', \"text\" = NULL, \"vector\" = NULL, \"text_hash\" = ?1, \"bcc\" = NULL,\n\t\t\t \"lease_until\" = NULL, \"last_error\" = ?2, \"updated_at\" = ?3 WHERE \"id\" = ?4";

/// `env.DB.prepare(sql).bind(...params)`
pub fn stmt(env: &Env, sql: &str, params: &[V]) -> R<V> {
    js::bind_exact(&env.prepare("DB", sql)?, params)
}

/// `err instanceof Error ? err.message : 'unknown error'`
pub fn error_message(err: &Throw) -> String {
    if err.name.is_empty() {
        "unknown error".into()
    } else {
        err.message.clone()
    }
}

/// `processQueue(env, now)` → processed
pub async fn process_queue(env: &Env, now: f64) -> R<f64> {
    let due = js::all(&stmt(env, SQL_DUE, &[V::Num(now), V::Num(BATCH_SIZE)])?).await?;

    let mut processed = 0.0;
    for item in due.g("results")?.items() {
        let id = item.g("id")?;
        if !claim(env, &id, now).await? {
            continue;
        }
        process_one(env, &id, now).await?;
        processed += 1.0;
    }
    // rejected·failed 행은 원문이 없지만 기록으로 남아 있다. 보관 기간이 지나면 지운다
    js::run(&stmt(env, SQL_PURGE, &[V::Num(now - RETAIN_MS)])?).await?;
    Ok(processed)
}

/// `claim(env, id, now)` — 조건부 UPDATE, 바뀐 행이 1건이면 이 실행이 집은 것
async fn claim(env: &Env, id: &V, now: f64) -> R<bool> {
    let res = js::run(&stmt(env, SQL_CLAIM, &[V::Num(now + LEASE_MS), V::Num(now), id.clone()])?).await?;
    Ok(strict_eq(&res.g("meta")?.g("changes")?, &V::Num(1.0)))
}

/// try 블록이 `return reject(…)` · `return release(…)` 로 끝났을 때. 원본 그대로: async 함수의 try 안에서
/// 기다리지 않고 돌려준 promise 는 catch 가 잡지 않는다 — 그 UPDATE 가 실패하면 예외가 processQueue 밖으로 샌다
enum Tail {
    Done,
    Reject(String),
    Release,
}

async fn process_one(env: &Env, id: &V, now: f64) -> R<()> {
    let row = js::first(&stmt(env, SQL_ROW, &[id.clone()])?).await?;
    if !row.truthy() {
        return Ok(());
    }

    let attempt: R<Tail> = async {
        // 답글은 부모가 저장된 뒤에만 저장한다. 부모가 거부되었으면 답글도 거부한다
        let parent_ref = row.g("ref")?;
        if parent_ref.truthy() {
            let parent = js::first(&stmt(env, SQL_PARENT_STATE, &[parent_ref.clone()])?).await?;
            if parent.truthy() {
                let state = parent.g("state")?;
                if strict_eq(&state, &V::from("rejected")) || strict_eq(&state, &V::from("failed")) {
                    return Ok(Tail::Reject("parent rejected".into()));
                }
                // 부모가 아직 처리 중이다. 이번 시도는 세지 않고 다음 실행에 다시 본다
                return Ok(Tail::Release);
            }
            let stored = js::first(&stmt(env, SQL_STORED, &[parent_ref])?).await?;
            if !stored.truthy() {
                return Ok(Tail::Reject("parent missing".into()));
            }
        }

        if strict_eq(&row.g("text")?, &V::Null) || strict_eq(&row.g("vector")?, &V::Null) {
            return Err(Throw::error("queue row has no text or vector"));
        }

        let result = moderate(env, &row.g("text")?).await?;
        if result.profane() {
            // 사유는 걸린 질문 이름이다 (profanity, nsfw, illegal)
            return Ok(Tail::Reject(result.flagged.join(",")));
        }

        save(env, &row).await?;
        Ok(Tail::Done)
    }
    .await;

    match attempt {
        Ok(Tail::Done) => Ok(()),
        // 원본 그대로: 기다리지 않은 return — 실패하면 catch 를 지나 밖으로 샌다
        Ok(Tail::Reject(reason)) => reject(env, id, &reason, now).await,
        Ok(Tail::Release) => release(env, id, now).await,
        // 원문은 오류 메시지에 넣지 않는다. 오류 종류만 남긴다
        Err(err) => retry_or_fail(env, &row, now, &error_message(&err)).await,
    }
}

/// `save(env, row)`: D1 → Vectorize → 대기열 삭제. 같은 id 를 여러 번 써도 결과가 같다
async fn save(env: &Env, row: &V) -> R<()> {
    // D1 행을 쓰기 전에 변환한다. 벡터가 잘못된 행은 D1 에 흔적을 남기지 않고 실패한다
    let bytes = to_vector_bytes(&row.g("vector")?)?;

    let bcc = row.g("bcc")?;
    let bcc = if bcc.is_nullish() { V::Null } else { bcc };
    js::run(&stmt(
        env,
        SQL_INSERT_EVAL,
        &[
            row.g("id")?,
            row.g("from")?,
            row.g("to")?,
            row.g("cc")?,
            bcc,
            row.g("ref")?,
            row.g("root")?,
            row.g("created_at")?,
            row.g("request_id")?,
        ],
    )?)
    .await?;

    // 메타데이터 규칙은 workers/www/src/evaluations.ts 의 vectorMetadata 와 같아야 한다 (cc·root 항상, to·ref 는 값이 있을 때만)
    let metadata = V::new_obj();
    metadata.put("cc", row.g("cc")?);
    metadata.put("root", row.g("root")?);
    let to = row.g("to")?;
    if to.truthy() {
        metadata.put("to", to);
    }
    let parent_ref = row.g("ref")?;
    if parent_ref.truthy() {
        metadata.put("ref", parent_ref);
    }
    // new Float32Array(bytes.slice().buffer) — 바이트 그대로
    let values = Float32Array::new(&Uint8Array::from(&bytes[..]).buffer());
    let item = V::new_obj();
    item.put("id", row.g("id")?);
    item.put("values", V::ext(Js(values.into())));
    item.put("metadata", metadata);
    env.vectorize_upsert("EVAL_VECTORIZE", &arr![item]).await?;

    js::run(&stmt(env, SQL_DELETE_QUEUED, &[row.g("id")?])?).await?;
    Ok(())
}

async fn reject(env: &Env, id: &V, reason: &str, now: f64) -> R<()> {
    js::run(&stmt(env, SQL_REJECT, &[V::from(MASKED_HASH), V::from(reason), V::Num(now), id.clone()])?).await?;
    Ok(())
}

async fn release(env: &Env, id: &V, now: f64) -> R<()> {
    js::run(&stmt(env, SQL_RELEASE, &[V::Num(now), id.clone()])?).await?;
    Ok(())
}

/// `inspectStored` 의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoredState {
    Stored,
    Partial,
    None,
}

/// `inspectStored(env, id)` — 재시도 한도에서 이미 저장이 끝난 행인지 본다
async fn inspect_stored(env: &Env, id: &V) -> R<StoredState> {
    let in_d1 = js::first(&stmt(env, SQL_STORED, &[id.clone()])?).await?;
    let index = env.get("EVAL_VECTORIZE");
    let found = js::call_async(&index, "getByIds", &[js::to_js(&arr![id.clone()])]).await?;
    let in_vectorize = found.length()?.to_num() > 0.0;
    if in_d1.truthy() && in_vectorize {
        return Ok(StoredState::Stored);
    }
    if in_d1.truthy() {
        return Ok(StoredState::Partial);
    }
    // D1 에 없는데 벡터만 있는 경우는 받아들일 수 없는 상태다. 정리는 아래(finalize)가 한다
    Ok(if in_vectorize { StoredState::Partial } else { StoredState::None })
}

async fn retry_or_fail(env: &Env, row: &V, now: f64, error: &str) -> R<()> {
    let row_id = row.g("id")?;
    if row.g("attempts")?.to_num() >= MAX_ATTEMPTS {
        let state = match inspect_stored(env, &row_id).await {
            Ok(s) => s,
            Err(err) => {
                // 저장 여부를 확인할 수 없다. 판단을 미루고 대기열에 남긴다 (원문과 벡터도 그대로)
                let msg = format!("inspect failed: {}", error_message(&err));
                js::run(&stmt(env, SQL_PENDING, &[V::from(msg), V::Num(now), row_id])?).await?;
                return Ok(());
            }
        };
        if state == StoredState::Stored {
            js::run(&stmt(env, SQL_DELETE_QUEUED, &[row_id])?).await?;
            return Ok(());
        }
        if state == StoredState::Partial {
            // 부분 저장을 되돌린다. 실패하면 위와 같이 다음 실행에 다시 본다
            let rollback: R<()> = async {
                let index = env.get("EVAL_VECTORIZE");
                js::call_async(&index, "deleteByIds", &[js::to_js(&arr![row_id.clone()])]).await?;
                js::run(&stmt(env, SQL_DELETE_EVAL, &[row_id.clone()])?).await?;
                Ok(())
            }
            .await;
            if let Err(err) = rollback {
                let msg = format!("rollback failed: {}", error_message(&err));
                js::run(&stmt(env, SQL_PENDING, &[V::from(msg), V::Num(now), row_id])?).await?;
                return Ok(());
            }
        }
        js::run(&stmt(env, SQL_FAIL, &[V::from(MASKED_HASH), V::from(error), V::Num(now), row_id])?).await?;
        return Ok(());
    }
    js::run(&stmt(env, SQL_PENDING, &[V::from(error), V::Num(now), row_id])?).await?;
    Ok(())
}
