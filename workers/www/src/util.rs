//! 여러 모듈이 같이 쓰는 작은 것들: CORS 머리글(원본 `evaluations.ts` 의 `corsHeaders`), JSON 응답, 요청에서 읽는 값.
//!
//! 원본에서 `corsHeaders` 는 evaluations.ts 에 있고 session.ts · navtrials.ts 가 가져다 쓴다. graph.ts 는 evaluations.ts 의
//! ValidationError · parseDomain · parseUuid · verdictSummaries 를 가져다 쓴다. 2단계에서 evaluations.rs · graph.rs · navtrials.rs 를
//! 따로 갈아 끼워도 서로 흔들리지 않게 그것들을 여기 둔다 (evaluations.rs 도 자기 것을 새로 만들지 말고 이것을 쓴다).

use premium_discount_worker_common::web::{Body, Req, Resp};
use premium_discount_worker_common::{stringify_str, V};

/// 원본 `ALLOWED_ORIGINS` (evaluations.ts) — 앱 웹뷰의 출처
pub const ALLOWED_ORIGINS: [&str; 2] = ["tauri://localhost", "http://tauri.localhost"];

/// 원본 `corsHeaders(origin)` (evaluations.ts) — 객체 리터럴의 키 순서 그대로
pub fn cors_headers(origin: &str) -> Vec<(String, String)> {
    let mut h = vec![
        // status·verdicts 조회는 GET이다. 앱 웹뷰는 세션을 x-oauth-hash / x-oauth-token 헤더로 보낸다 (auth.ts)
        ("Access-Control-Allow-Methods".to_string(), "GET, POST, OPTIONS".to_string()),
        ("Access-Control-Allow-Headers".to_string(), "Content-Type, x-oauth-hash, x-oauth-token".to_string()),
        ("Vary".to_string(), "Origin".to_string()),
    ];
    if ALLOWED_ORIGINS.contains(&origin) {
        h.push(("Access-Control-Allow-Origin".to_string(), origin.to_string()));
    }
    h
}

/// `new Response(JSON.stringify(body), { status, headers })` — headers 는 원본 객체 리터럴의 차례
/// (`{ 'Content-Type': 'application/json', ...corsHeaders(origin), ...extra }` 처럼 펼친 것을 이어 붙인다. 같은 이름은 뒤의 것이 이긴다)
pub fn json_response(body: &V, status: u16, headers: Vec<(String, String)>) -> Resp {
    let mut r = Resp::new(status, Body::Text(stringify_str(body)));
    for (k, v) in headers {
        r.set(&k, v);
    }
    r
}

/// `new Response(null, { status, headers })`
pub fn empty_response(status: u16, headers: Vec<(String, String)>) -> Resp {
    let mut r = Resp::new(status, Body::Empty);
    for (k, v) in headers {
        r.set(&k, v);
    }
    r
}

/// `[(이름, 값), …]` 줄임
pub fn hv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// `request.headers.get(name) ?? ''` (없으면 빈 글)
pub fn header_or_empty(req: &Req, name: &str) -> String {
    req.header(name).as_str().map(str::to_string).unwrap_or_default()
}

/// `request.headers.get(name)` → Some(값) · None(null)
pub fn header_opt(req: &Req, name: &str) -> Option<String> {
    req.header(name).as_str().map(str::to_string)
}

/// `new URL(request.url).pathname` — 들어온 요청의 주소는 늘 올바르다 (런타임이 만든 것)
pub fn pathname(req: &Req) -> String {
    url::Url::parse(&req.url).map(|u| u.path().to_string()).unwrap_or_default()
}

/// `(request.headers.get('cf-connecting-ip') ?? '').trim()` — 요청 제한 · 세션 확인의 IP
pub fn client_ip(req: &Req) -> String {
    premium_discount_worker_common::jsv::js_trim(&header_or_empty(req, "cf-connecting-ip")).to_string()
}

/// 바이트 → 소문자 hex (`Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')`)
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ───────────── evaluations.ts 의 export 가운데 graph.ts 도 쓰는 것 (ValidationError · parseDomain · parseUuid · verdictSummaries) ─────────────
// 2단계에서 evaluations.rs 와 graph.rs 를 따로 옮겨도 서로의 파일을 건드리지 않게 여기 둔다.

/// `class ValidationError extends Error {}` 를 가르는 이름. 원본의 처리기는 `instanceof ValidationError` 면 400 `{ error: err.message }` 로 답한다.
/// 주의: 원본 클래스는 name 을 바꾸지 않아 `String(err)` 가 `Error: …` 다 — 이 Throw 를 console 에 찍거나 밖으로 내보낼 일이 생기면
/// [`plain_error`] 로 이름을 `Error` 로 바꿀 것 (원본에서 ValidationError 가 처리기 밖으로 새는 자리는 없다)
pub const VALIDATION_ERROR: &str = "ValidationError";

/// `new ValidationError(message)`
pub fn validation_error(message: impl Into<String>) -> premium_discount_worker_common::Throw {
    premium_discount_worker_common::Throw { name: VALIDATION_ERROR, message: message.into() }
}

/// `err instanceof ValidationError`
pub fn is_validation(err: &premium_discount_worker_common::Throw) -> bool {
    err.name == VALIDATION_ERROR
}

/// 원본의 Error 하위 클래스(ValidationError · ConflictError · HashMismatchError)를 `String(err)` 의 꼴(`Error: …`)로
pub fn plain_error(err: &premium_discount_worker_common::Throw) -> premium_discount_worker_common::Throw {
    premium_discount_worker_common::Throw::error(err.message.clone())
}

/// `DOMAIN_MAX_LEN`
pub const DOMAIN_MAX_LEN: usize = 64;

/// `parseDomain(value)` — 글이 아니면 'cc is required', trim · 소문자 뒤 64자(UTF-16) 이하이고
/// `/^([a-z0-9-]{1,63}\.)+[a-z]{2,63}$/` 에 맞아야 한다
pub fn parse_domain(value: &V) -> premium_discount_worker_common::R<String> {
    use premium_discount_worker_common::jsv::{js_trim, to_lower, u16_len};
    static DOMAIN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let Some(s) = value.as_str() else { return Err(validation_error("cc is required")) };
    let cc = to_lower(js_trim(s));
    let re = DOMAIN_RE.get_or_init(|| regex::Regex::new(r"^([a-z0-9-]{1,63}\.)+[a-z]{2,63}$").expect("정규식"));
    if u16_len(&cc) > DOMAIN_MAX_LEN || !re.is_match(&cc) {
        return Err(validation_error("cc must be a registrable domain (max 64 chars)"));
    }
    Ok(cc)
}

/// `parseUuid(value, field)` — `/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i`, 소문자로 돌려준다
pub fn parse_uuid(value: &V, field: &str) -> premium_discount_worker_common::R<String> {
    let ok = value.as_str().is_some_and(|s| {
        let b = s.as_bytes();
        b.len() == 36 && b.iter().enumerate().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() })
    });
    match value.as_str() {
        Some(s) if ok => Ok(s.to_ascii_lowercase()),
        _ => Err(validation_error(format!("{field} must be a UUID"))),
    }
}

/// `binds(n)` — IN 목록용 자리표시자 `?1, ?2, ...`
pub fn binds(n: usize) -> String {
    (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ")
}

/// `verdictSummaries(env, ids)` — 검색 결과와 그래프 노드에 붙이는 판정 요약 (`eval_id` → `{ tier, grade, rank, evidence }`, 그 키 차례).
/// ids 가 비었으면 D1 을 부르지 않는다. ids 는 50개 이하로만 부른다 (D1 은 문장당 바인딩이 100개까지)
#[cfg(target_arch = "wasm32")]
pub async fn verdict_summaries(env: &premium_discount_worker_common::js::Env, ids: &[String]) -> premium_discount_worker_common::R<indexmap::IndexMap<String, V>> {
    use premium_discount_worker_common::{js, obj};
    let mut out = indexmap::IndexMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let sql = format!("SELECT \"eval_id\", \"tier\", \"grade\", \"rank\", \"evidence\" FROM eval_verdicts WHERE \"eval_id\" IN ({})", binds(ids.len()));
    let args: Vec<V> = ids.iter().map(|s| V::from(s.as_str())).collect();
    let rs = js::all(&js::bind_exact(&env.prepare("DB", &sql)?, &args)?).await?;
    for r in rs.g("results")?.items() {
        out.insert(r.get("eval_id").to_key(), obj! { "tier" => r.get("tier"), "grade" => r.get("grade"), "rank" => r.get("rank"), "evidence" => r.get("evidence") });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_and_uuid_validation() {
        assert_eq!(parse_domain(&V::from("  Example.COM ")).unwrap(), "example.com");
        assert_eq!(parse_domain(&V::from("a.b.co.kr")).unwrap(), "a.b.co.kr");
        assert_eq!(parse_domain(&V::Null).unwrap_err().message, "cc is required");
        for bad in ["example", "exa_mple.com", "-.c", "a.c", &format!("{}.com", "a".repeat(64)), &format!("{}com", "a.".repeat(31))] {
            let e = parse_domain(&V::from(bad)).unwrap_err();
            assert!(is_validation(&e) && e.message == "cc must be a registrable domain (max 64 chars)", "{bad}");
        }
        let id = "0190F0A0-0000-7000-8000-0000000000A1";
        assert_eq!(parse_uuid(&V::from(id), "id").unwrap(), id.to_lowercase());
        assert_eq!(parse_uuid(&V::from("0190f0a0-0000-7000-8000-0000000000a"), "id").unwrap_err().message, "id must be a UUID");
        assert_eq!(parse_uuid(&V::Num(1.0), "ids").unwrap_err().message, "ids must be a UUID");
        assert_eq!(binds(3), "?1, ?2, ?3");
        assert_eq!(plain_error(&validation_error("x")).to_string(), "Error: x");
    }

    #[test]
    fn cors_only_echoes_the_app_origins() {
        let h = cors_headers("tauri://localhost");
        assert_eq!(h.len(), 4);
        assert_eq!(h[3], ("Access-Control-Allow-Origin".into(), "tauri://localhost".into()));
        assert_eq!(cors_headers("https://evil.example").len(), 3);
    }

    #[test]
    fn json_response_keeps_the_last_of_the_same_header() {
        let r = json_response(&V::Null, 200, hv(&[("Content-Type", "application/json"), ("content-type", "x")]));
        assert_eq!(r.headers, vec![("content-type".to_string(), "x".to_string())]);
        assert_eq!(r.body, Body::Text("null".into()));
    }
}
