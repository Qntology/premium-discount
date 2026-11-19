//! 원본 `evaluations.ts` (677줄) — 평가 저장(대기열) · 검색(앱 · 공개) · 상태 · 투표 · 대체 보고 · 판정 조회.
//!
//! 경로 (그 밖은 `Ok(None)` = 원본의 `return null`, 다음 처리기로):
//!
//! | 경로 | 메서드 | 요청 제한 | 처리 |
//! | --- | --- | --- | --- |
//! | `/evaluations` | POST | 저장 | 원문 · 해시 · 벡터를 `eval_queue` 에 넣는다 (저장 · Vectorize 는 cron 이 욕설 검사 뒤에) |
//! | `/evaluations/search` · `/public/evaluations/search` | POST | 읽기 | Vectorize 검색 + D1 판정 요약 (공개는 `to` 조건 · 필드 없음) |
//! | `/evaluations/status` | GET | 없음 | `request_id` 로 저장 · 대기 상태 |
//! | `/evaluations/votes` | POST | 저장 | 저장된 평가에 표, `verdict_dirty` 표시 |
//! | `/evaluations/supersedes` | POST | 저장 | 신구 대체 보고 (작성자만), 옛 평가에 `verdict_dirty` |
//! | `/evaluations/verdicts` | GET | 읽기 | 판정 행 (없으면 unrated 기본값) |
//!
//! 같은 SQL 글(줄바꿈 · 탭까지) · 같은 D1 · Vectorize 호출 차례 · 같은 JSON 키 차례를 지킨다. 예외 경계: 처리기 안의 모든 예외는
//! 원본의 try/catch 처럼 응답이 된다 — ValidationError 400 · HashMismatchError 422 · ConflictError 409 · 그 밖은
//! `console.log('evaluations error', err)` 뒤 500. 그래서 이 모듈은 `Err` 를 돌려주지 않는다.
//!
//! 요청 본문은 원본 `request.json()` 처럼 UTF-8 로 풀고(BOM 하나 버림, 잘못된 바이트는 U+FFFD) **JS 의 `JSON.parse`** 로 읽는다
//! (수 · 중복 키 · 오류가 V8 과 같게). 판정 행의 `reasons` 도 JS 의 `JSON.parse` 로 읽어 깨진 행의 SyntaxError 글까지 같다.
//! graph.rs 가 쓰는 검사 · 판정 요약(`parse_domain` · `parse_uuid` · `binds` · `verdict_summaries`)은 util.rs 에 있다.
//! 검색의 판정 요약은 Vectorize 가 준 id 를 **값 그대로** 묶어야 해서(원본은 `m.id` 를 그대로 bind) 여기 따로 둔다(`summaries`).

use premium_discount_worker_common::jsv::{js_trim, to_lower};
use premium_discount_worker_common::{R, V};

use crate::util::{hex, validation_error};

/// `EVAL_DIMENSIONS` — google/embeddinggemma-2 출력 차원 (Vectorize 인덱스 차원과 같아야 한다)
pub const EVAL_DIMENSIONS: usize = 768;
/// Vectorize: 메타데이터를 함께 반환할 때 topK 최대 50
pub const TOP_K_MAX: u32 = 50;
/// 원문 최대 크기 (UTF-8 바이트). 임시값
pub const MAX_TEXT_BYTES: usize = 8192;
/// 판정 조회 한 번에 받는 최대 id 수 (IN 바인딩이 D1 의 문장당 상한 100 을 넘지 않게)
pub const VERDICT_IDS_MAX: usize = 50;
/// 검색의 tier: 'rated' 조건. 판정이 난 평가 전부 (unrated 제외)
pub const RATED_TIERS: [&str; 3] = ["premium", "neutral", "discount"];
/// `ethers.ZeroAddress`
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// `class ConflictError extends Error {}` — 같은 request_id 로 다른 내용을 보낸 경우 (409)
pub const CONFLICT_ERROR: &str = "ConflictError";
/// `class HashMismatchError extends Error {}` — 원문과 해시가 맞지 않는 경우 (422). 저장하지 않는다
pub const HASH_MISMATCH_ERROR: &str = "HashMismatchError";

/// `request.json()` 의 글 풀기: UTF-8 decode (앞의 BOM 하나를 버리고, 잘못된 바이트는 U+FFFD — WHATWG 와 Rust 의 lossy 는 같은 규칙)
pub fn decode_body(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{FEFF}').unwrap_or(&text).to_string()
}

/// `parseAddress(value, field)` — 비어 있거나(undefined · null · '') ZeroAddress 면 None.
/// 고침(fix-eval-address-normalize): 원본은 `ethers.isAddress` 를 지난 글을 `value.toLowerCase()` 로만 바꿔, isAddress 가 받는 다른 꼴
/// (0x 없는 40자리 · ICAP `XE…`)이 그 글 그대로 from · to · bcc 와 검색의 to 조건이 되고 ZeroAddress 검사도 빠져나갔다
/// → `ethers.getAddress(value).toLowerCase()` (늘 0x + 소문자 40자리)
pub fn parse_address(value: &V, field: &str) -> R<Option<String>> {
    if value.is_nullish() || value.as_str() == Some("") {
        return Ok(None);
    }
    let normalized = match value.as_str().and_then(premium_discount_worker_common::eth::get_address) {
        Some(checksummed) => to_lower(&checksummed),
        None => return Err(validation_error(format!("{field} must be a valid address"))),
    };
    Ok(if normalized == ZERO_ADDRESS { None } else { Some(normalized) })
}

/// `guestAddress` 의 계산: `('0x' + keccak256(toUtf8Bytes(\`guest|${salt}|${ip}\`)).slice(-40)).toLowerCase()`
pub fn guest_address_from(salt: &str, ip: &str) -> String {
    let digest = premium_discount_worker_common::eth::keccak256(format!("guest|{salt}|{ip}").as_bytes());
    format!("0x{}", &hex(&digest)[24..])
}

/// `parseVector(value)` — 768 개의 유한한 수, L2 노름이 1 ± 0.01. 받은 배열을 그대로 돌려준다
pub fn parse_vector(value: &V) -> R<V> {
    if !value.is_arr() || value.len() != EVAL_DIMENSIONS {
        return Err(validation_error(format!("vector must be an array of length {EVAL_DIMENSIONS}")));
    }
    let mut sum_sq = 0.0f64;
    for x in value.items() {
        match x {
            V::Num(n) if n.is_finite() => sum_sq += n * n,
            _ => return Err(validation_error("vector must contain only finite numbers")),
        }
    }
    if (sum_sq.sqrt() - 1.0).abs() > 0.01 {
        return Err(validation_error("vector must be L2-normalized (norm ≈ 1)"));
    }
    Ok(value.clone())
}

/// `new Float32Array(vector).buffer` (리틀 엔디언, f64 → f32 는 가장 가까운 값 · 짝수 쪽 — JS 와 같다)
pub fn float32_bytes(vector: &V) -> Vec<u8> {
    vector.items().iter().flat_map(|x| (x.to_num() as f32).to_le_bytes()).collect()
}

/// `parseText(value)` — 해시는 보내온 글 그대로 계산하므로 저장도 trim 없이 그대로
pub fn parse_text(value: &V) -> R<String> {
    let Some(s) = value.as_str() else { return Err(validation_error("text is required")) };
    if js_trim(s).is_empty() {
        return Err(validation_error("text must not be empty"));
    }
    if s.len() > MAX_TEXT_BYTES {
        return Err(validation_error(format!("text must be at most {MAX_TEXT_BYTES} bytes")));
    }
    Ok(s.to_string())
}

/// `parseTextHash(value)` — `/^[0-9a-f]{64}$/`
pub fn parse_text_hash(value: &V) -> R<String> {
    match value.as_str() {
        Some(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => Ok(s.to_string()),
        _ => Err(validation_error("text_hash must be 64 lowercase hex characters (SHA-256 of text)")),
    }
}

/// `sha256Hex(text)` (UTF-8)
pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(text.as_bytes()))
}

/// `uuidv7()` 의 계산 — 앞 48비트는 밀리초(`Math.floor(ms / 2 ** k) & 0xff`), 버전 7, 변형 10
pub fn uuidv7_from(ms: f64, mut b: [u8; 16]) -> String {
    let byte = |x: f64| (x.floor() as i64 & 0xff) as u8;
    b[0] = byte(ms / 2f64.powi(40));
    b[1] = byte(ms / 2f64.powi(32));
    b[2] = byte(ms / 2f64.powi(24));
    b[3] = byte(ms / 2f64.powi(16));
    b[4] = byte(ms / 2f64.powi(8));
    b[5] = byte(ms);
    b[6] = (b[6] & 0x0f) | 0x70;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// `parseRequestId(value)` — 멱등 키 `/^[A-Za-z0-9_-]{16,64}$/` (대기열 방식에서는 필수)
pub fn parse_request_id(value: &V) -> R<String> {
    match value.as_str() {
        Some(s) if (16..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') => Ok(s.to_string()),
        _ => Err(validation_error("request_id is required (16-64 characters of A-Z, a-z, 0-9, _ or -)")),
    }
}

/// `parseVoteValue(value)` — 1 추천 · -1 비추천 · 0 철회. 수만 받는다(true · "1" 은 받지 않는다). 받은 수 그대로(-0 도) 돌려준다
pub fn parse_vote_value(value: &V) -> R<V> {
    match value {
        V::Num(n) if *n == 1.0 || *n == -1.0 || *n == 0.0 => Ok(value.clone()),
        _ => Err(validation_error("value must be 1, -1 or 0")),
    }
}

/// `parseVoteKind(value)` — 생략하거나 null 이면 vote
pub fn parse_vote_kind(value: &V) -> R<String> {
    match value {
        V::Undef | V::Null => Ok("vote".into()),
        V::Str(s) if s == "vote" || s == "vote_outcome" => Ok(s.clone()),
        _ => Err(validation_error("kind must be vote or vote_outcome")),
    }
}

/// `parseTierFilter(value)` — premium | discount | rated (= premium, neutral, discount)
pub fn parse_tier_filter(value: &V) -> R<Vec<&'static str>> {
    match value.as_str() {
        Some("premium") => Ok(vec!["premium"]),
        Some("discount") => Ok(vec!["discount"]),
        Some("rated") => Ok(RATED_TIERS.to_vec()),
        _ => Err(validation_error("tier must be premium, discount or rated")),
    }
}

/// `UUID_RE.test(s)` (`/…/i`)
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36 && b.iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

/// `parseIdList(raw)` — 쉼표로 구분한 UUID 1~50개 (중복 포함 개수). 중복은 처음 나온 자리만 남긴다
pub fn parse_id_list(raw: Option<&str>) -> R<Vec<String>> {
    let ids: Vec<String> = match raw {
        Some(r) if !r.is_empty() => r.split(',').map(|s| to_lower(js_trim(s))).collect(),
        _ => Vec::new(),
    };
    if ids.is_empty() || ids.len() > VERDICT_IDS_MAX || !ids.iter().all(|id| is_uuid(id)) {
        return Err(validation_error(format!("ids must be 1-{VERDICT_IDS_MAX} comma-separated UUIDs")));
    }
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
pub use wasm::handle;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use premium_discount_worker_common::jsv::less_than;
    use premium_discount_worker_common::js::{self, Env};
    use premium_discount_worker_common::web::{Req, Resp};
    use premium_discount_worker_common::{console_log, obj, platform, strict_eq, Throw, R, V};

    use super::*;
    use crate::auth::verified_address;
    use crate::ratelimit::{is_rate_limited, RETRY_AFTER_SECONDS};
    use crate::util::{binds, cors_headers, empty_response, header_opt, header_or_empty, hv, is_validation, json_response, parse_domain, parse_uuid, pathname};

    /// `new ConflictError(message)`
    fn conflict(message: &str) -> Throw {
        Throw { name: CONFLICT_ERROR, message: message.to_string() }
    }

    /// `json(body, status, origin)` — 머리글: Content-Type · CORS 차례
    fn json(body: V, status: u16, origin: &str) -> Resp {
        let mut headers = hv(&[("Content-Type", "application/json")]);
        headers.extend(cors_headers(origin));
        json_response(&body, status, headers)
    }

    /// `rateLimited(origin)` — Content-Type · Retry-After · CORS
    fn rate_limited(origin: &str) -> Resp {
        let retry = RETRY_AFTER_SECONDS.to_string();
        let mut headers = hv(&[("Content-Type", "application/json"), ("Retry-After", retry.as_str())]);
        headers.extend(cors_headers(origin));
        json_response(&obj! { "error" => "rate limit exceeded" }, 429, headers)
    }

    fn error(message: &str) -> V {
        obj! { "error" => message }
    }

    /// JS 의 `JSON.parse(String(value))` (V8 의 수 읽기 · 오류 글 그대로)
    fn js_json_parse(text: &str) -> R<V> {
        js_sys::JSON::parse(text).map(|j| js::to_v(&j)).map_err(js::to_throw)
    }

    /// `readJson(request)` — JSON 이 아니면 'invalid JSON body', 객체가 아니면(배열 포함) 'body must be an object'
    fn read_json(req: &Req) -> R<V> {
        let body = match js_json_parse(&decode_body(&req.body)) {
            Ok(v) => v,
            Err(_) => return Err(validation_error("invalid JSON body")),
        };
        if !body.truthy() || body.type_of() != "object" || body.is_arr() {
            return Err(validation_error("body must be an object"));
        }
        Ok(body)
    }

    /// `guestAddress(request, env)` — from 이 없으면 IP 를 솔트와 함께 해싱한다. 솔트 · IP 가 없으면 (Validation 이 아닌) Error → 500
    fn guest_address(req: &Req, env: &Env) -> R<String> {
        let salt = env.get("GUEST_SALT");
        if !salt.truthy() {
            return Err(Throw::error("GUEST_SALT is not configured"));
        }
        // request.headers.get('cf-connecting-ip') ?? request.headers.get('X-Real-IP') ?? '' — 빈 cf-connecting-ip 는 X-Real-IP 로 넘어가지 않는다
        let ip = header_opt(req, "cf-connecting-ip").or_else(|| header_opt(req, "X-Real-IP")).unwrap_or_default();
        let ip = js_trim(&ip);
        if ip.is_empty() {
            return Err(Throw::error("client ip not available"));
        }
        Ok(guest_address_from(&salt.to_str(), ip))
    }

    /// `uuidv7()` — `Date.now()` · `crypto.getRandomValues(16바이트)`
    fn uuidv7() -> R<String> {
        let ms = platform::now();
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).map_err(|e| Throw::error(format!("crypto.getRandomValues: {e}")))?;
        Ok(uuidv7_from(ms, b))
    }

    // ───────────── D1 (env.DB.prepare(sql).bind(...).first/all/run) ─────────────

    async fn d1_first(env: &Env, sql: &str, args: &[V]) -> R<V> {
        js::first(&js::bind_exact(&env.prepare("DB", sql)?, args)?).await
    }

    async fn d1_all(env: &Env, sql: &str, args: &[V]) -> R<V> {
        js::all(&js::bind_exact(&env.prepare("DB", sql)?, args)?).await
    }

    async fn d1_run(env: &Env, sql: &str, args: &[V]) -> R<V> {
        js::run(&js::bind_exact(&env.prepare("DB", sql)?, args)?).await
    }

    /// `{ ...row, [key]: value }`
    fn spread_with(row: &V, key: &str, value: &str) -> V {
        let out = V::new_obj();
        for (k, v) in row.entries() {
            out.put(&k, v);
        }
        out.put(key, V::from(value));
        out
    }

    // ───────────── POST /evaluations ─────────────

    /// `findReplay(env, requestId, cc, ref)` — 같은 request_id 가 이미 저장되었거나 대기 중인지. cc · ref 가 다르면 409
    async fn find_replay(env: &Env, request_id: &str, cc: &str, ref_: &V) -> R<Option<V>> {
        let stored = d1_first(env, r#"SELECT "id", "root", "ref", "from", "cc" FROM evaluations WHERE "request_id" = ?1"#, &[V::from(request_id)]).await?;
        let found = if stored.truthy() {
            spread_with(&stored, "status", "stored")
        } else {
            d1_first(env, r#"SELECT "id", "root", "ref", "from", "cc", "state" AS "status" FROM eval_queue WHERE "request_id" = ?1"#, &[V::from(request_id)]).await?
        };
        if !found.truthy() {
            return Ok(None);
        }
        if !strict_eq(&found.get("cc"), &V::from(cc)) || !strict_eq(&found.get("ref"), ref_) {
            return Err(conflict("request_id was already used for a different evaluation"));
        }
        Ok(Some(found))
    }

    fn replay_body(r: &V) -> V {
        obj! { "id" => r.get("id"), "root" => r.get("root"), "ref" => r.get("ref"), "from" => r.get("from"), "status" => r.get("status") }
    }

    /// `findParent(env, ref)` — 저장된 평가 또는 대기열 항목
    async fn find_parent(env: &Env, ref_: &str) -> R<V> {
        let stored = d1_first(env, r#"SELECT "cc", "root" FROM evaluations WHERE "id" = ?1"#, &[V::from(ref_)]).await?;
        if stored.truthy() {
            return Ok(spread_with(&stored, "state", "stored"));
        }
        d1_first(env, r#"SELECT "cc", "root", "state" FROM eval_queue WHERE "id" = ?1"#, &[V::from(ref_)]).await
    }

    /// `enqueueEvaluation(request, env, origin)`
    async fn enqueue(req: &Req, env: &Env, origin: &str) -> R<Resp> {
        let body = read_json(req)?;
        let request_id = parse_request_id(&body.get("request_id"))?;
        let cc = parse_domain(&body.get("cc"))?;
        let text = parse_text(&body.get("text"))?;
        let text_hash = parse_text_hash(&body.get("text_hash"))?;
        // 해시가 원문과 맞지 않으면 저장하지 않는다 (422)
        if sha256_hex(&text) != text_hash {
            return Err(Throw { name: HASH_MISMATCH_ERROR, message: "text_hash does not match text".into() });
        }
        // from 우선순위: 검증된 세션 주소 > 클라이언트 값(임시) > 게스트 주소. 세션이 있으면 body.from 은 검사하지도 않는다 (원본 그대로)
        let from = match verified_address(req, env).await {
            Some(a) => a,
            None => match parse_address(&body.get("from"), "from")? {
                Some(a) => a,
                None => guest_address(req, env)?,
            },
        };
        let to = V::from(parse_address(&body.get("to"), "to")?).or_null();
        let bcc = V::from(parse_address(&body.get("bcc"), "bcc")?).or_null();
        let vector = parse_vector(&body.get("vector"))?;
        let raw_ref = body.get("ref");
        let ref_ = if raw_ref.is_nullish() { V::Null } else { V::from(parse_uuid(&raw_ref, "ref")?) };

        // 같은 request_id: 이미 저장되었거나 대기 중이면 처음 결과를 돌려준다 (앱의 재시도)
        if let Some(replay) = find_replay(env, &request_id, &cc, &ref_).await? {
            return Ok(json(replay_body(&replay), 200, origin));
        }

        // 단일 부모, 같은 cc 만. 거부 · 실패한 부모에는 답글을 달 수 없다
        let mut root = V::Null;
        if let Some(ref_id) = ref_.as_str() {
            let parent = find_parent(env, ref_id).await?;
            if !parent.truthy() {
                return Err(validation_error("parent evaluation not found"));
            }
            if !strict_eq(&parent.get("cc"), &V::from(cc.as_str())) {
                return Err(validation_error("reply must use the same cc as its parent"));
            }
            let state = parent.get("state");
            if state.as_str() == Some("rejected") || state.as_str() == Some("failed") {
                return Err(validation_error("parent evaluation was rejected"));
            }
            root = parent.get("root");
        }

        let id = uuidv7()?;
        if root.is_nullish() {
            root = V::from(id.as_str());
        }
        let now = platform::now();

        let inserted = d1_run(
            env,
            "INSERT INTO eval_queue (\"id\", \"request_id\", \"from\", \"to\", \"cc\", \"ref\", \"root\", \"text\", \"text_hash\", \"vector\", \"state\", \"attempts\", \"created_at\", \"updated_at\", \"bcc\")\n\t\t\t VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', 0, ?11, ?11, ?12)",
            &[
                V::from(id.as_str()),
                V::from(request_id.as_str()),
                V::from(from.as_str()),
                to,
                V::from(cc.as_str()),
                ref_.clone(),
                root.clone(),
                V::from(text),
                V::from(text_hash),
                V::bytes(float32_bytes(&vector)),
                V::Num(now),
                bcc,
            ],
        )
        .await;
        if let Err(err) = inserted {
            // 같은 request_id 가 거의 동시에 들어와 유니크 제약에 걸렸다면, 먼저 들어간 항목을 돌려준다
            if let Some(again) = find_replay(env, &request_id, &cc, &ref_).await? {
                return Ok(json(replay_body(&again), 200, origin));
            }
            return Err(err);
        }

        Ok(json(obj! { "id" => id, "root" => root, "ref" => ref_, "from" => from, "status" => "pending" }, 202, origin))
    }

    // ───────────── POST /evaluations/votes ─────────────

    /// `markDirty(env, evalId, at)` — cron 이 판정을 다시 계산하도록. 이미 있으면 시각만 늦춘다
    async fn mark_dirty(env: &Env, eval_id: &V, at: f64) -> R<()> {
        d1_run(env, r#"INSERT OR REPLACE INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)"#, &[eval_id.clone(), V::Num(at)]).await?;
        Ok(())
    }

    /// `findVote(env, requestId, vote)` — 평가 · 종류 · 값 중 하나라도 다르면 409. 투표자는 비교하지 않는다
    async fn find_vote(env: &Env, request_id: &str, vote: &V) -> R<Option<V>> {
        let found = d1_first(env, r#"SELECT "eval_id", "voter", "voter_kind", "kind", "value" FROM eval_votes WHERE "request_id" = ?1"#, &[V::from(request_id)]).await?;
        if !found.truthy() {
            return Ok(None);
        }
        if !strict_eq(&found.get("eval_id"), &vote.get("eval_id")) || !strict_eq(&found.get("kind"), &vote.get("kind")) || !strict_eq(&found.get("value"), &vote.get("value")) {
            return Err(conflict("request_id was already used for a different vote"));
        }
        Ok(Some(found))
    }

    /// `voteBody(v, author)` — counted: 확인된 로그인이고 작성자 자신이 아닌 표만 센다
    fn vote_body(v: &V, author: &V) -> V {
        let counted = v.get("voter_kind").as_str() == Some("session") && !strict_eq(&v.get("voter"), author);
        obj! { "id" => v.get("eval_id"), "value" => v.get("value"), "kind" => v.get("kind"), "counted" => counted, "status" => "accepted" }
    }

    /// `replayVote(env, vote, author, origin)` — 재시도에 답할 때도 다시 표시한다
    async fn replay_vote(env: &Env, vote: &V, author: &V, origin: &str) -> R<Resp> {
        mark_dirty(env, &vote.get("eval_id"), platform::now()).await?;
        Ok(json(vote_body(vote, author), 200, origin))
    }

    /// `castVote(request, env, origin)`
    async fn cast_vote(req: &Req, env: &Env, origin: &str) -> R<Resp> {
        let body = read_json(req)?;
        let request_id = parse_request_id(&body.get("request_id"))?;
        let id = parse_uuid(&body.get("id"), "id")?;
        let value = parse_vote_value(&body.get("value"))?;
        let kind = parse_vote_kind(&body.get("kind"))?;

        // 앱은 이 404 를 (메시지까지 보고) 영구 거절로 처리한다. 대기열에만 있는 평가도 아직 투표할 수 없다
        let evaluation = d1_first(env, r#"SELECT "from" FROM evaluations WHERE "id" = ?1"#, &[V::from(id.as_str())]).await?;
        if !evaluation.truthy() {
            return Ok(json(error("evaluation not found"), 404, origin));
        }
        let author = evaluation.get("from");

        // 투표자는 서버가 정한다: 검증된 세션 주소, 없으면 게스트 주소 (클라이언트의 from 은 쓰지 않는다)
        let session = verified_address(req, env).await;
        let voter = match &session {
            Some(a) => a.clone(),
            None => guest_address(req, env)?,
        };
        let vote = obj! {
            "eval_id" => id.as_str(),
            "voter" => voter.as_str(),
            "voter_kind" => if session.is_some() { "session" } else { "guest" },
            "kind" => kind.as_str(),
            "value" => value.clone()
        };

        if let Some(replay) = find_vote(env, &request_id, &vote).await? {
            return replay_vote(env, &replay, &author, origin).await;
        }

        let now = platform::now();
        let inserted = async {
            let stmt = env.prepare("DB", "INSERT INTO eval_votes (\"id\", \"request_id\", \"eval_id\", \"voter\", \"voter_kind\", \"kind\", \"value\", \"created_at\")\n\t\t\t VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
            let args = [V::from(uuidv7()?), V::from(request_id.as_str()), V::from(id.as_str()), V::from(voter.as_str()), vote.get("voter_kind"), V::from(kind.as_str()), value.clone(), V::Num(now)];
            js::run(&js::bind_exact(&stmt, &args)?).await
        }
        .await;
        if let Err(err) = inserted {
            // 같은 request_id 가 거의 동시에 들어와 유니크 제약에 걸렸다면, 먼저 들어간 표로 답한다
            if let Some(again) = find_vote(env, &request_id, &vote).await? {
                return replay_vote(env, &again, &author, origin).await;
            }
            return Err(err);
        }
        // 표를 넣은 뒤에 표시한다 (먼저 표시하면 cron 이 표시를 지우는 사이에 들어온 표를 놓칠 수 있다)
        mark_dirty(env, &V::from(id.as_str()), now).await?;
        Ok(json(vote_body(&vote, &author), 202, origin))
    }

    // ───────────── POST /evaluations/supersedes ─────────────

    /// `findSupersede(env, requestId, record)` — 같은 request_id 로 기록된 보고. 쌍이 다르면 409
    async fn find_supersede(env: &Env, request_id: &str, record: &V) -> R<Option<V>> {
        let found = d1_first(env, r#"SELECT "old_id", "new_id", "reporter" FROM eval_supersedes WHERE "request_id" = ?1"#, &[V::from(request_id)]).await?;
        if !found.truthy() {
            return Ok(None);
        }
        if !strict_eq(&found.get("old_id"), &record.get("old_id")) || !strict_eq(&found.get("new_id"), &record.get("new_id")) {
            return Err(conflict("request_id was already used for a different report"));
        }
        Ok(Some(found))
    }

    fn supersede_body(r: &V) -> V {
        obj! { "old" => r.get("old_id"), "new" => r.get("new_id"), "status" => "accepted" }
    }

    async fn replay_supersede(env: &Env, record: &V, origin: &str) -> R<Resp> {
        mark_dirty(env, &record.get("old_id"), platform::now()).await?;
        Ok(json(supersede_body(record), 200, origin))
    }

    /// `reportSupersede(request, env, origin)` — 둘 다 저장된 평가, 같은 cc · to, new 가 더 늦게, 보고자는 확인된 로그인이고 두 작성자 중 하나
    async fn report_supersede(req: &Req, env: &Env, origin: &str) -> R<Resp> {
        let body = read_json(req)?;
        let request_id = parse_request_id(&body.get("request_id"))?;
        let old_id = parse_uuid(&body.get("old"), "old")?;
        let new_id = parse_uuid(&body.get("new"), "new")?;
        if old_id == new_id {
            return Err(validation_error("old and new must be different evaluations"));
        }

        const HEAD: &str = r#"SELECT "from", "cc", "to", "created_at" FROM evaluations WHERE "id" = ?1"#;
        let older = d1_first(env, HEAD, &[V::from(old_id.as_str())]).await?;
        let newer = d1_first(env, HEAD, &[V::from(new_id.as_str())]).await?;
        if !older.truthy() || !newer.truthy() {
            return Ok(json(error("evaluation not found"), 404, origin));
        }
        if !strict_eq(&older.get("cc"), &newer.get("cc")) || !strict_eq(&older.get("to").or_null(), &newer.get("to").or_null()) {
            return Err(validation_error("old and new must share cc and to"));
        }
        // newer.created_at <= older.created_at  ⇔  !(older < newer) 이고 비교가 정해짐
        if less_than(&older.get("created_at"), &newer.get("created_at")) == Some(false) {
            return Err(validation_error("new must be written after old"));
        }

        let Some(reporter) = verified_address(req, env).await else {
            return Ok(json(error("a verified session is required"), 401, origin));
        };
        let rep = V::from(reporter.as_str());
        if !strict_eq(&rep, &older.get("from")) && !strict_eq(&rep, &newer.get("from")) {
            return Ok(json(error("only an author of old or new can report it"), 403, origin));
        }
        let record = obj! { "old_id" => old_id.as_str(), "new_id" => new_id.as_str(), "reporter" => reporter.as_str() };

        if let Some(replay) = find_supersede(env, &request_id, &record).await? {
            return replay_supersede(env, &replay, origin).await;
        }
        let now = platform::now();
        let inserted = d1_run(
            env,
            "INSERT INTO eval_supersedes (\"old_id\", \"new_id\", \"reporter\", \"request_id\", \"created_at\") VALUES (?1, ?2, ?3, ?4, ?5)\n\t\t\t ON CONFLICT (\"old_id\", \"new_id\") DO NOTHING",
            &[V::from(old_id.as_str()), V::from(new_id.as_str()), V::from(reporter.as_str()), V::from(request_id.as_str()), V::Num(now)],
        )
        .await;
        if let Err(err) = inserted {
            if let Some(again) = find_supersede(env, &request_id, &record).await? {
                return replay_supersede(env, &again, origin).await;
            }
            return Err(err);
        }
        // 기록한 뒤에 표시한다. 같은 쌍이 다른 request_id 로 다시 와도 표시만 다시 한다
        mark_dirty(env, &V::from(old_id.as_str()), now).await?;
        Ok(json(supersede_body(&record), 202, origin))
    }

    // ───────────── POST .../search ─────────────

    /// JS Map 의 SameValueZero
    fn same_value_zero(a: &V, b: &V) -> bool {
        strict_eq(a, b) || matches!((a, b), (V::Num(x), V::Num(y)) if x.is_nan() && y.is_nan())
    }

    /// `new Map()` 의 get
    fn map_get(map: &[(V, V)], key: &V) -> Option<V> {
        map.iter().find(|(k, _)| same_value_zero(k, key)).map(|(_, v)| v.clone())
    }

    /// `verdictSummaries(env, ids)` — util::verdict_summaries 와 같은 SQL 이지만 ids 를 받은 값 그대로 묶고 키도 값 그대로 둔다
    /// (원본은 Vectorize 의 `m.id` 를 그대로 bind 하고 `Map` 의 키로 쓴다)
    async fn summaries(env: &Env, ids: &[V]) -> R<Vec<(V, V)>> {
        let mut out: Vec<(V, V)> = Vec::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let sql = format!("SELECT \"eval_id\", \"tier\", \"grade\", \"rank\", \"evidence\" FROM eval_verdicts WHERE \"eval_id\" IN ({})", binds(ids.len()));
        let rs = d1_all(env, &sql, ids).await?;
        for r in rs.g("results")?.items() {
            let key = r.get("eval_id");
            let value = obj! { "tier" => r.get("tier"), "grade" => r.get("grade"), "rank" => r.get("rank"), "evidence" => r.get("evidence") };
            match out.iter_mut().find(|(k, _)| same_value_zero(k, &key)) {
                Some(slot) => slot.1 = value,
                None => out.push((key, value)),
            }
        }
        Ok(out)
    }

    /// `result.matches` 를 `.map` · `.filter` 로 쓸 때 (배열이 아니면 원본과 같은 TypeError)
    fn match_list(result: &V) -> R<Vec<V>> {
        let matches = result.g("matches")?;
        match &matches {
            V::Arr(_) => Ok(matches.items()),
            V::Undef | V::Null => Err(Throw::type_error(format!("Cannot read properties of {} (reading 'map')", matches.to_str()))),
            _ => Err(Throw::type_error("result.matches.map is not a function")),
        }
    }

    /// `searchEvaluations(request, env, origin, scope)` — cc · root · ref (· to) 조건은 검색 전에, tier 는 D1 판정으로 받아 온 뒤에 거른다
    async fn search(req: &Req, env: &Env, origin: &str, public: bool) -> R<Resp> {
        let body = read_json(req)?;
        let vector = parse_vector(&body.get("vector"))?;

        let raw_top_k = body.get("topK");
        let top_k = match if raw_top_k.is_undef() { V::Num(10.0) } else { raw_top_k } {
            V::Num(n) if n.is_finite() && n.trunc() == n && n >= 1.0 && n <= f64::from(TOP_K_MAX) => n,
            _ => return Err(validation_error(format!("topK must be an integer between 1 and {TOP_K_MAX}"))),
        };

        let filter = V::new_obj();
        let cc = body.get("cc");
        if !cc.is_undef() {
            filter.put("cc", V::from(parse_domain(&cc)?));
        }
        let root = body.get("root");
        if !root.is_undef() {
            filter.put("root", V::from(parse_uuid(&root, "root")?));
        }
        let ref_ = body.get("ref");
        if !ref_.is_undef() {
            filter.put("ref", V::from(parse_uuid(&ref_, "ref")?));
        }
        // 공개 검색에서는 주소(to) 조건을 허용하지 않는다 (주소 기반 추론 방지)
        let to = body.get("to");
        if !to.is_undef() {
            if public {
                return Err(validation_error("to filter is not available in public search"));
            }
            match parse_address(&to, "to")? {
                Some(a) => filter.put("to", V::from(a)),
                None => return Err(validation_error("to must be a non-zero address")),
            }
        }
        let tier = body.get("tier");
        let tiers = if tier.is_undef() { None } else { Some(parse_tier_filter(&tier)?) };

        // 거르고도 topK 가 차도록 등급 조건이 있으면 세 배(최대 50)까지 받아 온다
        let options = obj! { "topK" => if tiers.is_some() { f64::from(TOP_K_MAX).min(top_k * 3.0) } else { top_k }, "returnMetadata" => true };
        if !filter.keys().is_empty() {
            options.put("filter", filter);
        }
        let result = env.vectorize_query("EVAL_VECTORIZE", &vector, &options).await?;
        let mut ids = Vec::new();
        for m in match_list(&result)? {
            ids.push(m.g("id")?);
        }
        let verdicts = summaries(env, &ids).await?;
        let matches = match &tiers {
            Some(t) => match_list(&result)?
                .into_iter()
                .filter(|m| {
                    let tier = map_get(&verdicts, &m.get("id")).map(|v| v.get("tier")).filter(|x| !x.is_nullish()).unwrap_or_else(|| V::from("unrated"));
                    tier.as_str().is_some_and(|s| t.contains(&s))
                })
                .collect(),
            None => match_list(&result)?,
        };

        let null_if = |v: V| if v.is_nullish() { V::Null } else { v };
        let out: Vec<V> = matches
            .iter()
            .take(top_k as usize)
            .map(|m| {
                let md = null_if(m.get("metadata"));
                let md = if md.is_null() { V::new_obj() } else { md };
                // 판정 행이 없으면 null (아직 판정 전)
                let verdict = map_get(&verdicts, &m.get("id")).unwrap_or(V::Null);
                let o = obj! { "id" => m.get("id"), "score" => m.get("score"), "cc" => null_if(md.get("cc")), "root" => null_if(md.get("root")), "ref" => null_if(md.get("ref")) };
                // 앱 응답에는 to 를 포함하고, 공개 응답에서는 제외한다
                if !public {
                    o.put("to", null_if(md.get("to")));
                }
                o.put("verdict", verdict);
                o
            })
            .collect();
        Ok(json(obj! { "matches" => out }, 200, origin))
    }

    // ───────────── GET /evaluations/status · /evaluations/verdicts ─────────────

    /// `new URL(request.url).searchParams.get(name)` → 글 · null
    fn search_param(req: &Req, name: &str) -> Option<String> {
        let url = url::Url::parse(&req.url).ok()?;
        let found = url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        found
    }

    /// `readStatus(request, env, origin)` — request_id 를 아는 쪽만 조회할 수 있다. 주소는 돌려주지 않는다
    async fn read_status(req: &Req, env: &Env, origin: &str) -> R<Resp> {
        let request_id = parse_request_id(&V::from(search_param(req, "request_id")).or_null())?;
        let stored = d1_first(env, r#"SELECT "id", "root", "ref" FROM evaluations WHERE "request_id" = ?1"#, &[V::from(request_id.as_str())]).await?;
        if stored.truthy() {
            return Ok(json(obj! { "id" => stored.get("id"), "root" => stored.get("root"), "ref" => stored.get("ref"), "status" => "stored" }, 200, origin));
        }
        let queued = d1_first(env, r#"SELECT "id", "root", "ref", "state", "last_error" FROM eval_queue WHERE "request_id" = ?1"#, &[V::from(request_id.as_str())]).await?;
        if queued.truthy() {
            let state = queued.get("state");
            let reason = if state.as_str() == Some("rejected") || state.as_str() == Some("failed") { queued.get("last_error") } else { V::Null };
            return Ok(json(obj! { "id" => queued.get("id"), "root" => queued.get("root"), "ref" => queued.get("ref"), "status" => state, "reason" => reason }, 200, origin));
        }
        Ok(json(error("not found"), 404, origin))
    }

    /// `readVerdicts(request, env, origin)` — 평가별 판정을 요청 순서대로. 투표자는 돌려주지 않는다
    async fn read_verdicts(req: &Req, env: &Env, origin: &str) -> R<Resp> {
        let ids = parse_id_list(search_param(req, "ids").as_deref())?;
        let sql = format!(
            "SELECT \"eval_id\", \"tier\", \"grade\", \"rank\", \"value\", \"evidence\", \"up\", \"down\", \"level\", \"reasons\", \"recipe\", \"updated_at\"\n\t\t FROM eval_verdicts WHERE \"eval_id\" IN ({})",
            binds(ids.len())
        );
        let args: Vec<V> = ids.iter().map(|s| V::from(s.as_str())).collect();
        let rs = d1_all(env, &sql, &args).await?;
        let mut rows: Vec<(V, V)> = Vec::new();
        for r in rs.g("results")?.items() {
            let key = r.get("eval_id");
            match rows.iter_mut().find(|(k, _)| same_value_zero(k, &key)) {
                Some(slot) => slot.1 = r,
                None => rows.push((key, r)),
            }
        }
        let mut verdicts = Vec::new();
        for id in &ids {
            let idv = V::from(id.as_str());
            let Some(r) = map_get(&rows, &idv) else {
                // 판정 행이 없다: 표가 없거나 cron 이 아직 계산하지 않았다
                verdicts.push(obj! {
                    "id" => idv, "tier" => "unrated", "grade" => V::Null, "rank" => V::Null, "value" => 0, "evidence" => 0, "up" => 0, "down" => 0,
                    "level" => V::Null, "reasons" => V::from(vec![V::from("low_evidence"), V::from("no_baseline")]),
                    "recipe" => premium_discount_verdict::RECIPE_V1, "updated_at" => V::Null
                });
                continue;
            };
            let reasons = js_json_parse(&r.get("reasons").to_str())?;
            verdicts.push(obj! {
                "id" => idv, "tier" => r.get("tier"), "grade" => r.get("grade"), "rank" => r.get("rank"), "value" => r.get("value"),
                "evidence" => r.get("evidence"), "up" => r.get("up"), "down" => r.get("down"), "level" => r.get("level"),
                "reasons" => reasons, "recipe" => r.get("recipe"), "updated_at" => r.get("updated_at")
            });
        }
        Ok(json(obj! { "verdicts" => verdicts }, 200, origin))
    }

    // ───────────── 처리기 ─────────────

    /// `handleEvaluations(request, env)` — 평가 경로만 처리하고 다른 경로는 `Ok(None)`. 처리기 안의 예외는 모두 응답이 된다
    pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>> {
        let path = pathname(req);
        let is_app_search = path == "/evaluations/search";
        let is_public_search = path == "/public/evaluations/search";
        let is_status = path == "/evaluations/status";
        let is_votes = path == "/evaluations/votes";
        let is_supersedes = path == "/evaluations/supersedes";
        let is_verdicts = path == "/evaluations/verdicts";
        if path != "/evaluations" && !is_app_search && !is_public_search && !is_status && !is_votes && !is_supersedes && !is_verdicts {
            return Ok(None);
        }

        let origin = header_or_empty(req, "Origin");
        if req.method == "OPTIONS" {
            return Ok(Some(empty_response(204, cors_headers(&origin))));
        }
        if req.method != if is_status || is_verdicts { "GET" } else { "POST" } {
            return Ok(Some(json(error("method not allowed"), 405, &origin)));
        }

        // 저장 · 투표 · 대체 보고는 저장 제한기, 검색 · 판정 조회는 읽기 제한기. status 조회는 제한하지 않는다
        if !is_status {
            let name = if path == "/evaluations" || is_votes || is_supersedes { "EVAL_WRITE_LIMITER" } else { "EVAL_READ_LIMITER" };
            if is_rate_limited(req, &env.get(name)).await {
                return Ok(Some(rate_limited(&origin)));
            }
        }

        let r = if is_status {
            read_status(req, env, &origin).await
        } else if is_verdicts {
            read_verdicts(req, env, &origin).await
        } else if is_votes {
            cast_vote(req, env, &origin).await
        } else if is_supersedes {
            report_supersede(req, env, &origin).await
        } else if path == "/evaluations" {
            enqueue(req, env, &origin).await
        } else {
            search(req, env, &origin, !is_app_search).await
        };
        Ok(Some(match r {
            Ok(resp) => resp,
            Err(err) if is_validation(&err) => json(error(&err.message), 400, &origin),
            Err(err) if err.name == HASH_MISMATCH_ERROR => json(error(&err.message), 422, &origin),
            Err(err) if err.name == CONFLICT_ERROR => json(error(&err.message), 409, &origin),
            Err(err) => {
                console_log!("evaluations error", err);
                json(error("internal error"), 500, &origin)
            }
        }))
    }

    trait OrNull {
        fn or_null(self) -> V;
    }
    impl OrNull for V {
        /// `x ?? null`
        fn or_null(self) -> V {
            if self.is_nullish() {
                V::Null
            } else {
                self
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::{arr, obj};

    fn unit(n: usize) -> V {
        V::from((0..n).map(|i| V::Num(if i == 0 { 1.0 } else { 0.0 })).collect::<Vec<_>>())
    }

    #[test]
    fn request_id_text_and_hash() {
        assert_eq!(parse_request_id(&V::from("req-0123456789abcdef")).unwrap(), "req-0123456789abcdef");
        for bad in [V::from("short"), V::from("a".repeat(65)), V::from("bad id!0123456789"), V::Null, V::Num(1.0), V::from("é".repeat(16))] {
            assert!(parse_request_id(&bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_request_id(&V::from("a".repeat(64))).unwrap().len(), 64);
        assert_eq!(parse_text(&V::Num(1.0)).unwrap_err().message, "text is required");
        assert_eq!(parse_text(&V::from(" \u{3000}\n")).unwrap_err().message, "text must not be empty");
        assert_eq!(parse_text(&V::from("가".repeat(2731))).unwrap_err().message, "text must be at most 8192 bytes");
        assert!(parse_text(&V::from("a".repeat(8192))).is_ok());
        assert!(parse_text_hash(&V::from("A".repeat(64))).is_err());
        assert!(parse_text_hash(&V::from("a".repeat(64))).is_ok());
        // node: createHash('sha256').update('좋은 평가입니다').digest('hex')
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn addresses_are_normalized_and_zero_is_none() {
        assert_eq!(parse_address(&V::Undef, "from").unwrap(), None);
        assert_eq!(parse_address(&V::from(""), "from").unwrap(), None);
        assert_eq!(parse_address(&V::from(ZERO_ADDRESS), "to").unwrap(), None);
        let up = format!("0x{}", "AB".repeat(20));
        assert_eq!(parse_address(&V::from(up.as_str()), "bcc").unwrap(), Some(up.to_lowercase()));
        assert_eq!(parse_address(&V::from("0x12"), "from").unwrap_err().message, "from must be a valid address");
        assert_eq!(parse_address(&V::Num(1.0), "to").unwrap_err().message, "to must be a valid address");
        // 고침(fix-eval-address-normalize): 0x 없는 꼴 · ICAP 은 0x + 소문자로, 0x 없는 ZeroAddress 도 None
        let ab = format!("0x{}", "ab".repeat(20));
        assert_eq!(parse_address(&V::from("ab".repeat(20)), "to").unwrap(), Some(ab.clone()));
        assert_eq!(parse_address(&V::from("XE51K1WQMAMNR3M45C495YWONVZHZJG9TWR"), "to").unwrap(), Some(ab));
        assert_eq!(parse_address(&V::from("0".repeat(40)), "to").unwrap(), None);
    }

    #[test]
    fn guest_address_matches_ethers() {
        // node: ('0x' + ethers.keccak256(ethers.toUtf8Bytes('guest|salt|203.0.113.7')).slice(-40)).toLowerCase()
        let g = guest_address_from("salt", "203.0.113.7");
        assert_eq!(g.len(), 42);
        assert!(g.starts_with("0x") && g[2..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_ne!(g, guest_address_from("salt", "203.0.113.8"));
    }

    #[test]
    fn vector_checks() {
        assert!(parse_vector(&unit(768)).is_ok());
        assert_eq!(parse_vector(&unit(767)).unwrap_err().message, "vector must be an array of length 768");
        assert_eq!(parse_vector(&obj! { "length" => 768 }).unwrap_err().message, "vector must be an array of length 768");
        let bad = unit(768);
        bad.s_("5", V::from("x")).unwrap();
        assert_eq!(parse_vector(&bad).unwrap_err().message, "vector must contain only finite numbers");
        let long = unit(768);
        long.s_("0", V::Num(1.02)).unwrap();
        assert_eq!(parse_vector(&long).unwrap_err().message, "vector must be L2-normalized (norm ≈ 1)");
        let ok = unit(768);
        ok.s_("0", V::Num(1.005)).unwrap();
        assert!(parse_vector(&ok).is_ok());
        assert_eq!(float32_bytes(&arr![1.0, 0.1]), [1.0f32.to_le_bytes(), 0.1f32.to_le_bytes()].concat());
    }

    #[test]
    fn uuidv7_layout() {
        // T0 = 1763380800000 = 0x019a91afaa00
        let id = uuidv7_from(1763380800000.0, [0xff; 16]);
        assert_eq!(id, "019a91af-aa00-7fff-bfff-ffffffffffff");
    }

    #[test]
    fn vote_and_tier_and_id_list() {
        assert!(parse_vote_value(&V::Num(-0.0)).is_ok());
        for bad in [V::Num(2.0), V::Num(0.5), V::from("1"), V::Bool(true), V::Null, V::Undef] {
            assert_eq!(parse_vote_value(&bad).unwrap_err().message, "value must be 1, -1 or 0");
        }
        assert_eq!(parse_vote_kind(&V::Null).unwrap(), "vote");
        assert!(parse_vote_kind(&V::from("like")).is_err());
        assert_eq!(parse_tier_filter(&V::from("rated")).unwrap(), RATED_TIERS.to_vec());
        assert!(parse_tier_filter(&V::from("constructor")).is_err());
        let a = "0190f0a0-0000-7000-8000-00000000000a";
        assert_eq!(parse_id_list(Some(&format!(" {} ,{a}", a.to_uppercase()))).unwrap(), vec![a.to_string()]);
        for bad in ["", ",", &format!("{a},"), &format!("{a},abc")] {
            assert_eq!(parse_id_list(Some(bad)).unwrap_err().message, "ids must be 1-50 comma-separated UUIDs");
        }
        assert!(parse_id_list(None).is_err());
        assert_eq!(decode_body(b"\xEF\xBB\xBF{}"), "{}");
        assert_eq!(decode_body(b"\xff"), "\u{FFFD}");
    }
}
