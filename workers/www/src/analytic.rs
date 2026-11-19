//! 원본 `analytic.ts` (409줄) — 웹 행동 분석 수집 (tracker-logis-center 의 Worker 를 옮긴 것).
//!
//! 사이트 주인이 자기 사이트에 수집 스크립트(web/www/static/js/tracker.js)를 심으면, 방문자의 사건(web-event/1)이
//! POST /analytic/events 로 들어와 사이트(cc = hashId(host))마다 쌓인다. 구조화 · 퍼널 판정은 사이트 주인의 앱이 기기에서 한다.
//! 서버는 받은 그대로를 gzip 해 두고(updated_at = 0: 원시), 사이트 주인에게만 돌려준다.
//!
//! 경로: /analytic/events (POST: 수집, GET · DELETE: 주인 읽기 · 지우기), /analytic/sites (GET · POST · DELETE: 주인)
//! 표: web_events · analytic_sites. 수집은 `DB.batch` 한 번(사건마다 `crypto.randomUUID()`), 사이트 확인은 `https://<호스트>/` 를 가져와 meta 를 본다.
//!
//! 원본과 같게 지킨 것: SQL 글 · D1 호출 차례 · batch 의 문장 차례 · randomUUID 를 부르는 차례와 수(403 으로 끝나는 사건 앞까지) ·
//! JSON 키 차례 · 수 표기 · 상태 · 머리글 · 예외 경계(try 안의 예외는 console 에 `analytic error` 와 함께 찍고 500).
//! 원본과 다른 바이트: 저장하는 gzip 은 pako 가 아니라 common/gzip.rs(flate2)라 바이트가 다르다 — 풀면 같은 JSON 이다 (blob.rs 와 같은 방침).
//!
//! 하위 모듈: [`event`](sanitizeEvent · cleanHref · scrubText · siteMetaOwners · 저장할 JSON), `json`(받은 본문의 JSON.parse),
//! `inflate`(pako ungzip 과 같은 받아들임 + 푼 크기 상한).
//!
//! 고친 버그 (docs 표의 id, `scripts/workers-diff/fixes/www_analytic.mjs` 가 원본에 같은 수정을 덧댄다):
//! - fix-analytic-gzip-cap: gzip 본문을 상한 없이 풀었다 (256 KB 가 약 264 MB 로) → 푼 크기 1 MiB 를 넘으면 413
//! - fix-analytic-sites-preflight: POST /analytic/sites 의 사전 요청이 수집용 CORS(지갑 · 세션 머리글 없음)를 돌려줬다
//! - fix-analytic-empty-host: 호스트가 빈 주소("https://a@/")의 사건이 Origin 없이 오면 hashId(null) 로 500
//! - fix-analytic-sid-surrogate: sid 를 64 글자로 자른 곳이 서로게이트 쌍의 가운데면 hashId 가 던져 500
//! - fix-analytic-list-limit: 읽기의 limit 이 정수가 아니면(`limit=2.5`) SQLite 의 LIMIT 가 거절해 500

mod event;
mod inflate;
mod json;

pub use event::{clean_href, event_json, host_of, normalize_host, sanitize_event, scrub_text, site_meta_owners, WebEvent, EVENT_SCHEMA, EVENT_TYPES, SITE_META_NAME};

/// `ANALYTIC_EVENTS_PATH`
pub const ANALYTIC_EVENTS_PATH: &str = "/analytic/events";
/// `ANALYTIC_SITES_PATH`
pub const ANALYTIC_SITES_PATH: &str = "/analytic/sites";
/// `MAX_BODY_BYTES` — 수집 본문 (gzip 이면 압축된 크기)
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// 고침(fix-analytic-gzip-cap): gzip 본문을 푼 크기의 상한
pub const MAX_INFLATED_BYTES: usize = 1024 * 1024;
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const MAX_EVENTS_PER_POST: usize = 20;
const MAX_LIST: f64 = 500.0;
const DEFAULT_LIST: f64 = 200.0;
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const MAX_SITE_PAGE_BYTES: usize = 512 * 1024;
/// 주인 쪽 본문 상한
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const MAX_OWNER_BODY: usize = 4096;
/// `WALLET_HEADERS`
pub const WALLET_HEADERS: [&str; 3] = ["x-pd-address", "x-pd-time", "x-pd-signature"];

/// 본문 읽기의 실패 (원본 readBody 가 던지는 것 · 고친 상한)
#[derive(Debug, PartialEq, Eq)]
pub enum BodyError {
    /// 원본의 catch → 400 'invalid JSON body'
    Invalid,
    /// 고침(fix-analytic-gzip-cap) → 413 'body too large'
    TooLarge,
}

/// `readBody(bytes, encoding)` — Content-Encoding 에 gzip 이 있으면 풀고, UTF-8 로 읽어 JSON.parse
pub fn read_body(bytes: &[u8], encoding: Option<&str>) -> Result<premium_discount_worker_common::V, BodyError> {
    use premium_discount_worker_common::gzip::decode_utf8;
    let gz = encoding.is_some_and(|e| !e.is_empty() && premium_discount_worker_common::jsv::to_lower(e).contains("gzip"));
    let text = if gz {
        // 고침(fix-analytic-gzip-cap): 원본은 pako ungzip 으로 끝까지 풀었다
        match inflate::ungzip_limited(bytes, MAX_INFLATED_BYTES) {
            Ok(b) => decode_utf8(&b),
            Err(inflate::Inflate::TooLarge) => return Err(BodyError::TooLarge),
            Err(inflate::Inflate::Bad) => return Err(BodyError::Invalid),
        }
    } else {
        decode_utf8(bytes)
    };
    json::parse(&text).ok_or(BodyError::Invalid)
}

/// `cors(origin, owner)` — 수집은 어느 사이트에서나 온다(자격 증명 없음). 주인 읽기 · 등록은 지갑 · 세션 머리글을 받는다
pub fn cors(origin: &str, owner: bool) -> Vec<(String, String)> {
    let allow_origin = if owner && !origin.is_empty() { origin } else { "*" };
    let headers = if owner { format!("Content-Type, x-oauth-hash, x-oauth-token, {}", WALLET_HEADERS.join(", ")) } else { "Content-Type, Content-Encoding".to_string() };
    vec![
        ("Access-Control-Allow-Origin".to_string(), allow_origin.to_string()),
        ("Access-Control-Allow-Methods".to_string(), if owner { "GET, POST, DELETE, OPTIONS" } else { "POST, OPTIONS" }.to_string()),
        ("Access-Control-Allow-Headers".to_string(), headers),
        ("Access-Control-Max-Age".to_string(), "86400".to_string()),
        ("Vary".to_string(), "Origin".to_string()),
    ]
}

/// `Math.min(MAX_LIST, Math.max(1, Math.floor(Number(limit ?? DEFAULT_LIST) || DEFAULT_LIST)))`
/// 고침(fix-analytic-list-limit): Math.floor 를 더했다 (원본은 2.5 를 그대로 LIMIT 에 묶어 SQLite 가 거절 → 500)
pub fn list_limit(raw: Option<&str>) -> f64 {
    let n = raw.map_or(DEFAULT_LIST, premium_discount_worker_common::jsv::string_to_number);
    let n = if n != 0.0 && !n.is_nan() { n } else { DEFAULT_LIST };
    MAX_LIST.min(n.floor().max(1.0))
}

/// `Math.max(0, Number(since ?? 0) || 0)`
pub fn list_since(raw: Option<&str>) -> f64 {
    let n = raw.map_or(0.0, premium_discount_worker_common::jsv::string_to_number);
    let n = if n != 0.0 && !n.is_nan() { n } else { 0.0 };
    if n > 0.0 {
        n
    } else {
        0.0
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::handle;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use js_sys::Reflect;
    use premium_discount_worker_common::js::{self, Env, Js};
    use premium_discount_worker_common::jsv::string_to_number;
    use premium_discount_worker_common::web::{Req, Resp};
    use premium_discount_worker_common::{console_log, obj, Throw, R, V};
    use wasm_bindgen::JsValue;

    use super::*;
    use crate::auth::{read_session_credentials, verified_address, verify_wallet_request_once};
    use crate::blob::blob_to_base64;
    use crate::ratelimit::{is_rate_limited, RETRY_AFTER_SECONDS};
    use crate::util::{empty_response, header_opt, header_or_empty, json_response, pathname};

    /// `reply(body, status, origin, owner, extra)` — 머리글: Content-Type · CORS · extra 차례
    fn reply(body: V, status: u16, origin: &str, owner: bool, extra: &[(&str, &str)]) -> Resp {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        headers.extend(cors(origin, owner));
        headers.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        json_response(&body, status, headers)
    }

    fn error(message: impl Into<String>, status: u16, origin: &str, owner: bool) -> Resp {
        reply(obj! { "error" => message.into() }, status, origin, owner, &[])
    }

    fn rate_limited(origin: &str, owner: bool) -> Resp {
        let retry = RETRY_AFTER_SECONDS.to_string();
        reply(obj! { "error" => "rate limit exceeded" }, 429, origin, owner, &[("Retry-After", retry.as_str())])
    }

    /// `hashId(text)` (원본은 Map 으로 기억하지만 값은 같다)
    fn hash_id(text: &str) -> String {
        crate::auth::address_from_email(text)
    }

    /// `new URL(request.url).searchParams.get(name)`
    fn query(req: &Req, name: &str) -> Option<String> {
        let u = url::Url::parse(&req.url).ok()?;
        let found = u.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        found
    }

    /// `crypto.randomUUID()`
    fn random_uuid() -> R<String> {
        let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto")).map_err(js::to_throw)?;
        let id = js::call(&V::ext(Js(crypto)), "randomUUID", &[])?;
        Ok(id.as_string().unwrap_or_default())
    }

    /// `collect(request, env, nowMs, origin)` — POST /analytic/events
    async fn collect(req: &Req, env: &Env, now_ms: f64, origin: &str) -> R<Resp> {
        // env.ANALYTIC_WRITE_LIMITER ?? env.EVAL_WRITE_LIMITER
        let mut limiter = env.get("ANALYTIC_WRITE_LIMITER");
        if limiter.is_nullish() {
            limiter = env.get("EVAL_WRITE_LIMITER");
        }
        if is_rate_limited(req, &limiter).await {
            return Ok(rate_limited(origin, false));
        }
        // Number(request.headers.get('Content-Length') ?? 0) > MAX_BODY_BYTES
        let declared = header_opt(req, "Content-Length").map_or(0.0, |v| string_to_number(&v));
        if declared > MAX_BODY_BYTES as f64 {
            return Ok(error("body too large", 413, origin, false));
        }
        if req.body.len() > MAX_BODY_BYTES {
            return Ok(error("body too large", 413, origin, false));
        }
        let parsed = match read_body(&req.body, header_opt(req, "Content-Encoding").as_deref()) {
            Ok(v) => v,
            Err(BodyError::Invalid) => return Ok(error("invalid JSON body", 400, origin, false)),
            // 고침(fix-analytic-gzip-cap)
            Err(BodyError::TooLarge) => return Ok(error("body too large", 413, origin, false)),
        };
        // 배열이면 그대로, { events: [...] } 면 그 배열, 아니면 [parsed]
        let list: Vec<V> = match &parsed {
            V::Arr(_) => parsed.items(),
            V::Obj(_) if parsed.get("events").is_arr() => parsed.get("events").items(),
            _ => vec![parsed.clone()],
        };
        if list.is_empty() || list.len() > MAX_EVENTS_PER_POST {
            return Ok(error(format!("send 1 to {MAX_EVENTS_PER_POST} events"), 400, origin, false));
        }
        let events: Vec<Option<WebEvent>> = list.iter().map(sanitize_event).collect();
        if events.iter().any(Option::is_none) {
            return Ok(error(format!("every event must be {EVENT_SCHEMA}"), 400, origin, false));
        }
        // 브라우저가 붙인 Origin 의 호스트와 사건 주소의 호스트가 다르면 남의 사이트 사건이다
        let origin_host = if origin.is_empty() { None } else { host_of(origin) };
        let mut statements: Vec<V> = Vec::new();
        for e in events.into_iter().flatten() {
            // sanitize_event 가 호스트 있는 주소만 남긴다 (고침 fix-analytic-empty-host)
            let host = host_of(&e.href).unwrap_or_default();
            if origin_host.as_ref().is_some_and(|o| *o != host) {
                return Ok(error("event host does not match the page origin", 403, origin, false));
            }
            let cc = hash_id(&host);
            let stmt = env.prepare(
                "DB",
                r#"INSERT INTO web_events ("id","cc","session","type","seq","data","created_at","updated_at") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)"#,
            )?;
            // 인자 차례: randomUUID() → hashId(cc + sid) → encodeBlob(...)
            let id = random_uuid()?;
            let session = hash_id(&format!("{cc}{}", e.sid));
            let blob = premium_discount_worker_common::gzip::gzip(event_json(&e).as_bytes());
            let args = [V::from(id), V::from(cc), V::from(session), V::from(e.typ.as_str()), V::Num(e.seq), V::bytes(blob), V::Num(now_ms)];
            statements.push(js::bind_exact(&stmt, &args)?);
        }
        let n = statements.len();
        env.batch("DB", &V::from(statements)).await?;
        Ok(reply(obj! { "stored" => n }, 200, origin, false, &[]))
    }

    /// `readLimited(res, max)` — 본문을 max 바이트까지만 읽고(넘으면 읽기를 멈추고 취소) UTF-8 로
    async fn read_limited(res: &JsValue, max: usize) -> R<String> {
        let body = Reflect::get(res, &JsValue::from_str("body")).map_err(js::to_throw)?;
        if body.is_null() || body.is_undefined() {
            return Ok(String::new());
        }
        let reader = V::ext(Js(js::call(&V::ext(Js(body)), "getReader", &[])?));
        let mut all: Vec<u8> = Vec::new();
        while all.len() < max {
            let chunk = js::call_async(&reader, "read", &[]).await?;
            let value = chunk.get("value");
            if chunk.get("done").truthy() || !value.truthy() {
                break;
            }
            all.extend(value.to_bytes().unwrap_or_default());
        }
        // await reader.cancel().catch(() => undefined)
        if let Ok(p) = js::call(&reader, "cancel", &[]) {
            let _ = js::wait(p).await;
        }
        all.truncate(max);
        Ok(premium_discount_worker_common::gzip::decode_utf8(&all))
    }

    /// `sites(...)` — GET (내 사이트) · DELETE (?host=) · POST ({ host }: 첫 화면의 meta 확인 후 등록)
    async fn sites(req: &Req, env: &Env, now_ms: f64, origin: &str, body: &[u8], owner: &str) -> R<Resp> {
        if req.method == "GET" {
            let stmt = js::bind_exact(&env.prepare("DB", r#"SELECT "host", "cc", "verified_at" FROM analytic_sites WHERE "owner" = ?1 ORDER BY "host""#)?, &[V::from(owner)])?;
            let results = js::all(&stmt).await?.g("results")?;
            return Ok(reply(obj! { "owner" => owner, "sites" => results }, 200, origin, true, &[]));
        }
        if req.method == "DELETE" {
            let Some(host) = normalize_host(&query(req, "host").map_or(V::Null, V::from)) else {
                return Ok(error("host is required", 400, origin, true));
            };
            let stmt = js::bind_exact(&env.prepare("DB", r#"DELETE FROM analytic_sites WHERE "cc" = ?1 AND "owner" = ?2"#)?, &[V::from(hash_id(&host)), V::from(owner)])?;
            let res = js::run(&stmt).await?;
            return Ok(reply(obj! { "host" => host, "deleted" => changes(&res) }, 200, origin, true, &[]));
        }
        // (JSON.parse(new TextDecoder().decode(body)) as { host?: unknown }).host — null 이면 .host 가 던져 400
        let host = match json::parse(&premium_discount_worker_common::gzip::decode_utf8(body)) {
            Some(v) if !v.is_nullish() => normalize_host(&v.get("host")),
            _ => return Ok(error("invalid JSON body", 400, origin, true)),
        };
        let Some(host) = host else {
            return Ok(error("host must be a domain name", 400, origin, true));
        };
        let expected = format!("<meta name=\"{SITE_META_NAME}\" content=\"{owner}\">");
        let page = format!("https://{host}/");
        let fetched: Result<Result<String, f64>, Throw> = async {
            let res = js::fetch(&page, "GET", &[("Accept", "text/html")], js::Body::None).await?;
            let status = js::response_status(&res);
            if !(200.0..300.0).contains(&status) {
                return Ok(Err(status));
            }
            Ok(Ok(read_limited(&res, MAX_SITE_PAGE_BYTES).await?))
        }
        .await;
        let html = match fetched {
            Ok(Ok(html)) => html,
            Ok(Err(status)) => {
                let msg = format!("https://{host}/ answered {}", premium_discount_worker_common::jsv::num_to_string(status));
                return Ok(reply(obj! { "error" => msg, "expected" => expected }, 403, origin, true, &[]));
            }
            Err(_) => return Ok(reply(obj! { "error" => format!("could not fetch https://{host}/"), "expected" => expected }, 403, origin, true, &[])),
        };
        if !site_meta_owners(&html).iter().any(|a| a == owner) {
            return Ok(reply(obj! { "error" => "the site does not name this wallet", "expected" => expected }, 403, origin, true, &[]));
        }
        let cc = hash_id(&host);
        let sql = "INSERT INTO analytic_sites (\"cc\", \"host\", \"owner\", \"verified_at\") VALUES (?1, ?2, ?3, ?4)\n\t\t ON CONFLICT (\"cc\", \"owner\") DO UPDATE SET \"verified_at\" = excluded.\"verified_at\"";
        let stmt = js::bind_exact(&env.prepare("DB", sql)?, &[V::from(cc.as_str()), V::from(host.as_str()), V::from(owner), V::Num(now_ms)])?;
        js::run(&stmt).await?;
        Ok(reply(obj! { "host" => host, "cc" => cc, "owner" => owner, "verified_at" => now_ms }, 200, origin, true, &[]))
    }

    /// `Number(res.meta?.changes ?? 0)`
    fn changes(res: &V) -> f64 {
        let c = res.get("meta").get("changes");
        if c.is_nullish() {
            0.0
        } else {
            c.to_num()
        }
    }

    /// `owns(db, cc, owner)`
    async fn owns(env: &Env, cc: &str, owner: &str) -> R<bool> {
        let stmt = js::bind_exact(&env.prepare("DB", r#"SELECT 1 AS ok FROM analytic_sites WHERE "cc" = ?1 AND "owner" = ?2"#)?, &[V::from(cc), V::from(owner)])?;
        Ok(!js::first(&stmt).await?.is_null())
    }

    /// `events(request, env, origin, owner)` — GET (커서로 읽기) · DELETE (?before= 보다 오래된 사건 지우기)
    async fn events(req: &Req, env: &Env, origin: &str, owner: &str) -> R<Resp> {
        let Some(host) = normalize_host(&query(req, "host").map_or(V::Null, V::from)) else {
            return Ok(error("host is required", 400, origin, true));
        };
        let cc = hash_id(&host);
        if !owns(env, &cc, owner).await? {
            return Ok(error("verify the site first (POST /analytic/sites)", 403, origin, true));
        }
        if req.method == "DELETE" {
            // Number(url.searchParams.get('before') ?? NaN)
            let before = query(req, "before").map_or(f64::NAN, |b| string_to_number(&b));
            if !before.is_finite() || before <= 0.0 {
                return Ok(error("before (epoch ms) is required", 400, origin, true));
            }
            let stmt = js::bind_exact(&env.prepare("DB", r#"DELETE FROM web_events WHERE "cc" = ?1 AND "created_at" < ?2"#)?, &[V::from(cc), V::Num(before)])?;
            let res = js::run(&stmt).await?;
            return Ok(reply(obj! { "host" => host, "deleted" => changes(&res) }, 200, origin, true, &[]));
        }
        let since = list_since(query(req, "since").as_deref());
        let after = query(req, "after_id").unwrap_or_default();
        let limit = list_limit(query(req, "limit").as_deref());
        // (created_at, id) 커서: 같은 시각에 여러 행이 있어도 잘린 곳부터 이어 읽는다
        let sql = "SELECT \"id\", \"type\", \"session\", \"seq\", \"data\", \"created_at\", \"updated_at\" FROM web_events\n\t\t WHERE \"cc\" = ?1 AND (\"created_at\" > ?2 OR (\"created_at\" = ?2 AND \"id\" > ?3))\n\t\t ORDER BY \"created_at\", \"id\" LIMIT ?4";
        let stmt = js::bind_exact(&env.prepare("DB", sql)?, &[V::from(cc.as_str()), V::Num(since), V::from(after), V::Num(limit)])?;
        let results = js::all(&stmt).await?.g("results")?;
        // results.map((r) => ({ ...r, data: blobToBase64(r.data) })) — data 는 제자리에서 바뀐다
        let rows: Vec<V> = results
            .items()
            .into_iter()
            .map(|r| {
                let copy = V::new_obj();
                for (k, v) in r.entries() {
                    copy.put(&k, v);
                }
                copy.put("data", V::from(blob_to_base64(&r.get("data"))));
                copy
            })
            .collect();
        let next = match rows.last() {
            Some(last) => obj! { "since" => last.get("created_at"), "after_id" => last.get("id") },
            None => V::Null,
        };
        let more = rows.len() as f64 == limit;
        Ok(reply(obj! { "host" => host, "cc" => cc, "rows" => V::from(rows), "next" => next, "more" => more }, 200, origin, true, &[]))
    }

    /// `handleAnalytic(request, env)` — 원본의 `nowMs = Date.now()` · `fetcher = fetch` 기본 인자 그대로
    pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>> {
        let path = pathname(req);
        if path != ANALYTIC_EVENTS_PATH && path != ANALYTIC_SITES_PATH {
            return Ok(None);
        }
        let now_ms = premium_discount_worker_common::platform::now();
        let origin = header_or_empty(req, "Origin");
        let method = req.method.as_str();
        let owner_call = path == ANALYTIC_SITES_PATH || method != "POST";
        if method == "OPTIONS" {
            // 고침(fix-analytic-sites-preflight): 원본은 `ownerCall && ACRM !== 'POST'` 라 POST /analytic/sites 의 사전 요청에도
            // 수집용 CORS(Allow-Headers 에 지갑 · 세션 머리글 없음, Allow-Origin *)를 돌려줘 브라우저의 주인 등록이 막혔다
            let acrm = header_opt(req, "Access-Control-Request-Method");
            let owner = path == ANALYTIC_SITES_PATH || acrm.as_deref() != Some("POST");
            return Ok(Some(empty_response(204, cors(&origin, owner))));
        }
        let r: R<Resp> = async {
            if path == ANALYTIC_EVENTS_PATH && method == "POST" {
                return collect(req, env, now_ms, &origin).await;
            }
            if !["GET", "POST", "DELETE"].contains(&method) {
                return Ok(error("method not allowed", 405, &origin, true));
            }
            let limiter = env.get(if method == "GET" { "EVAL_READ_LIMITER" } else { "EVAL_WRITE_LIMITER" });
            if is_rate_limited(req, &limiter).await {
                return Ok(rate_limited(&origin, true));
            }
            // 주인 쪽은 인증이 있어야 한다: 지갑 머리글이 있으면 지갑 서명(본문 바이트 그대로)만, 없으면 QR 로그인 세션
            let body = req.body.as_slice();
            if body.len() > MAX_OWNER_BODY {
                return Ok(error("body too large", 413, &origin, true));
            }
            let owner = if WALLET_HEADERS.iter().any(|h| !req.header(h).is_null()) {
                // 고침(fix-wallet-replay): 서명 확인 + 한 번만 쓰기 (auth.rs). 이미 쓴 서명은 틀린 서명과 같이 401
                verify_wallet_request_once(req, env, body, now_ms).await?
            } else if read_session_credentials(req).is_some() {
                verified_address(req, env).await
            } else {
                None
            };
            let Some(owner) = owner else {
                return Ok(error("sign in, or sign the request with a wallet (x-pd-address, x-pd-time, x-pd-signature)", 401, &origin, true));
            };
            if path == ANALYTIC_SITES_PATH {
                return sites(req, env, now_ms, &origin, body, &owner).await;
            }
            events(req, env, &origin, &owner).await
        }
        .await;
        Ok(Some(match r {
            Ok(resp) => resp,
            Err(err) => {
                console_log!("analytic error", err);
                error("internal error", 500, &origin, owner_call)
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_numbers() {
        assert_eq!(list_limit(None), 200.0);
        assert_eq!(list_limit(Some("2")), 2.0);
        assert_eq!(list_limit(Some("2.5")), 2.0);
        assert_eq!(list_limit(Some("0")), 200.0);
        assert_eq!(list_limit(Some("0.5")), 1.0);
        assert_eq!(list_limit(Some("abc")), 200.0);
        assert_eq!(list_limit(Some("-5")), 1.0);
        assert_eq!(list_limit(Some("1e9")), 500.0);
        assert_eq!(list_limit(Some("")), 200.0);
        assert_eq!(list_since(None), 0.0);
        assert_eq!(list_since(Some("-3")), 0.0);
        assert_eq!(list_since(Some("12.5")), 12.5);
        assert_eq!(list_since(Some("x")), 0.0);
    }

    #[test]
    fn body_reading() {
        let gz = premium_discount_worker_common::gzip::gzip(br#"[{"a":1}]"#);
        assert!(read_body(&gz, Some("GZIP")).unwrap().is_arr());
        assert!(read_body(&gz, Some("x-gzip, br")).unwrap().is_arr());
        assert_eq!(read_body(&gz, None).unwrap_err(), BodyError::Invalid);
        assert_eq!(read_body(b"\xEF\xBB\xBF[1]", Some("")).unwrap().len(), 1);
        let bomb = premium_discount_worker_common::gzip::gzip(&vec![b' '; MAX_INFLATED_BYTES + 1]);
        assert_eq!(read_body(&bomb, Some("gzip")).unwrap_err(), BodyError::TooLarge);
        let ok = premium_discount_worker_common::gzip::gzip(format!("{}1", " ".repeat(MAX_INFLATED_BYTES - 1)).as_bytes());
        assert_eq!(read_body(&ok, Some("gzip")).unwrap().as_num(), Some(1.0));
    }

    #[test]
    fn cors_headers_for_collector_and_owner() {
        assert_eq!(cors("https://shop.example.com", false)[0].1, "*");
        assert_eq!(cors("tauri://localhost", true)[0].1, "tauri://localhost");
        assert_eq!(cors("", true)[0].1, "*");
        assert_eq!(cors("", true)[2].1, "Content-Type, x-oauth-hash, x-oauth-token, x-pd-address, x-pd-time, x-pd-signature");
    }
}
