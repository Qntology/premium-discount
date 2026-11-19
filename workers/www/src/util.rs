//! 여러 모듈이 같이 쓰는 작은 것들: CORS 머리글(원본 `evaluations.ts` 의 `corsHeaders`), JSON 응답, 요청에서 읽는 값.
//!
//! 원본에서 `corsHeaders` 는 evaluations.ts 에 있고 session.ts · navtrials.ts 가 가져다 쓴다. 2단계에서 evaluations.rs 를
//! 갈아 끼워도 session.rs 가 흔들리지 않게 여기 둔다 (evaluations.rs · navtrials.rs 도 이것을 쓰면 된다).

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

#[cfg(test)]
mod tests {
    use super::*;

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
