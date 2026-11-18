use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

/// POST /evaluations 요청. 원문과 원문 해시와 벡터를 보낸다.
/// 서버는 해시가 원문과 맞는지 확인한 뒤 대기열에 넣고, 욕설 검사를 거쳐 저장한다.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SaveRequest {
    /// 멱등 키. 같은 저장을 재시도할 때 같은 값을 보낸다 (서버가 중복 저장을 막는다)
    pub request_id: String,
    /// 없으면 서버가 게스트 주소를 발급하고 응답에 담아 준다
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub cc: String,
    /// 콘텐츠 해시 주소 (0x + 40자리). 서버가 evaluations.bcc에 저장한다. 없으면 생략한다
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bcc: Option<String>,
    /// 원문 (UTF-8)
    pub text: String,
    /// 원문의 SHA-256 hex (소문자 64자). 서버가 받은 원문으로 다시 계산해 비교한다
    pub text_hash: String,
    pub vector: Vec<f32>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
}

/// POST /evaluations 응답. 202는 대기열 접수(pending), 200은 같은 request_id의 현재 상태다.
#[derive(Debug, Clone, Deserialize)]
pub struct SaveResponse {
    pub id: String,
    pub root: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub from: String,
    /// stored | pending | processing | rejected | failed. 없으면 저장 완료로 본다 (이전 서버 호환)
    #[serde(default)]
    pub status: Option<String>,
}

/// POST /evaluations/search 요청
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchRequest {
    pub vector: Vec<f32>,
    #[serde(rename = "topK")]
    pub top_k: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// 등급 필터: premium | discount | rated(판정이 난 것 전부). 없으면 거르지 않는다
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub matches: Vec<Match>,
}

/// 검색 결과 한 건. 서버는 metadata로 감싸지 않고 필드를 바로 준다.
/// 앱 검색 응답에는 to가 들어 있고, 공개 검색 응답에는 to가 없다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Match {
    pub id: String,
    pub score: f32,
    pub cc: Option<String>,
    pub root: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub to: Option<String>,
    /// 서버의 전역 판정 (판정 행이 없으면 null). 판정을 모르는 이전 서버는 이 필드를 보내지 않는다
    #[serde(default)]
    pub verdict: Option<MatchVerdict>,
    /// 이 기기에서 이 평가에 한 표 (1 추천 | -1 비추천). 서버 응답에는 없고 앱이 채운다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_vote: Option<i32>,
}

/// 검색 결과에 붙는 판정 요약
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchVerdict {
    /// premium | neutral | discount | unrated
    pub tier: String,
    pub grade: Option<i32>,
    pub rank: Option<f64>,
    pub evidence: f64,
}

/// POST /evaluations/votes 요청. 같은 request_id는 재시도이고, 서버는 처음 결과를 돌려준다
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VoteRequest {
    pub request_id: String,
    /// 평가의 서버 id
    pub id: String,
    /// 1 추천 | -1 비추천 | 0 철회
    pub value: i32,
    /// vote | vote_outcome
    pub kind: String,
}

/// POST /evaluations/votes 응답 (202 접수, 200 같은 request_id의 처음 결과)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoteResponse {
    pub id: String,
    pub value: i32,
    pub kind: String,
    /// 판정에 들어가는 표인지. 게스트(로그인 확인 안 됨)와 자기 글 표는 저장만 되고 세지 않는다
    pub counted: bool,
    #[serde(default)]
    pub status: Option<String>,
}

/// POST /evaluations/supersedes 요청: 새 평가(`new`)가 옛 평가(`old`)를 대체한다 (www/migrations/0009).
/// 같은 request_id 는 재시도이고, 서버는 처음 결과를 돌려준다
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SupersedeRequest {
    pub request_id: String,
    pub old: String,
    pub new: String,
}

/// POST /evaluations/supersedes 응답 (202 접수, 200 같은 request_id 의 처음 결과)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SupersedeResponse {
    pub old: String,
    pub new: String,
    #[serde(default)]
    pub status: Option<String>,
}

/// 평가 하나의 전역 판정 (GET /evaluations/verdicts). 규칙은 contracts/verdict_v1.json (PD-Verdict)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerdictDto {
    pub id: String,
    /// premium | neutral | discount | unrated
    pub tier: String,
    /// -2..2 (unrated면 null)
    pub grade: Option<i32>,
    /// 스코프 기준선 대비 상대 순위 (기준선이 없으면 null)
    pub rank: Option<f64>,
    /// 축소 평균 v (-1..1)
    pub value: f64,
    /// 근거량 E
    pub evidence: f64,
    pub up: u32,
    pub down: u32,
    /// secondary | primary | global (기준선을 찾은 단계)
    pub level: Option<String>,
    /// 이유 코드 (rank_high, held, low_evidence, level:secondary …)
    pub reasons: Vec<String>,
    pub recipe: String,
    /// 서버가 값을 계산한 시각 (판정 행이 없으면 null)
    pub updated_at: Option<i64>,
}

/// GET /evaluations/verdicts 응답 (요청한 id 순서)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerdictsResponse {
    pub verdicts: Vec<VerdictDto>,
}

/// 한 번에 물을 수 있는 판정 수 (서버 상한)
pub const VERDICT_IDS_MAX: usize = 50;

/// GET /evaluations/status 응답
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct StatusResponse {
    pub id: String,
    pub root: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    /// stored | pending | processing | rejected | failed
    pub status: String,
    /// rejected·failed일 때 이유 (예: profanity)
    pub reason: Option<String>,
}

/// POST /session/start 응답: 앱 로그인 세션. 사용자는 mailto(QR)의 주소로 메일을 보낸다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStart {
    pub hash: String,
    pub token: String,
    /// 메일을 보낼 주소 (<hash>.<호스트>@oauth.email)
    pub address: String,
    /// QR에 넣을 mailto: 링크 (웹의 QR과 같은 모양)
    pub mailto: String,
}

/// GET /session 응답. state: anonymous | pending(메일 대기) | signed_in(address)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStatus {
    pub state: String,
    #[serde(default)]
    pub address: Option<String>,
}

/// 서버 통신 추상화. 테스트에서는 가짜 구현을 쓴다.
#[async_trait]
pub trait EvalBackend: Send + Sync {
    async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError>;
    async fn search(&self, req: &SearchRequest) -> Result<SearchResponse, AppError>;
    /// 같은 request_id의 처리 상태
    async fn status(&self, request_id: &str) -> Result<StatusResponse, AppError>;

    // ---- 투표와 판정 (지원하지 않는 백엔드는 기본 구현을 쓴다) ----

    /// 추천/비추천 투표 (POST /evaluations/votes)
    async fn vote(&self, _req: &VoteRequest) -> Result<VoteResponse, AppError> {
        Err(AppError::Rejected("votes are not supported by this backend".into()))
    }
    /// 전역 판정 (GET /evaluations/verdicts, 한 번에 VERDICT_IDS_MAX개까지)
    async fn verdicts(&self, _ids: &[String]) -> Result<VerdictsResponse, AppError> {
        Err(AppError::Rejected("verdicts are not supported by this backend".into()))
    }
    /// 신구 대체 보고 (POST /evaluations/supersedes)
    async fn supersede(&self, _req: &SupersedeRequest) -> Result<SupersedeResponse, AppError> {
        Err(AppError::Rejected("supersede reports are not supported by this backend".into()))
    }

    // ---- 로그인 (지원하지 않는 백엔드는 기본 구현을 쓴다) ----

    /// 앱 세션 발급 (POST /session/start)
    async fn session_start(&self) -> Result<SessionStart, AppError> {
        Err(AppError::Rejected("login is not supported by this backend".into()))
    }
    /// 주어진 세션의 상태 (GET /session). 아직 쓰기 시작하지 않은 세션도 물어볼 수 있다
    async fn session_status(&self, _session: &Session) -> Result<SessionStatus, AppError> {
        Err(AppError::Rejected("login is not supported by this backend".into()))
    }
    /// 이후 요청에 쓸 세션으로 바꾼다
    fn set_session(&self, _session: Session) {}
    /// 지금 요청에 쓰는 세션
    fn session(&self) -> Session {
        Session::anonymous()
    }

    // ---- 웹 행동 분석 (사이트 주인, 로그인 세션) ----

    /// 사이트 확인 (POST /analytic/sites): 서버가 사이트 첫 화면의 `<meta name="premium-discount-site">` 를 본다
    async fn analytic_site(&self, _host: &str) -> Result<Value, AppError> {
        Err(AppError::Rejected("web analytics is not supported by this backend".into()))
    }
    /// 원시 사건 한 쪽 (GET /analytic/events, (시각, id) 커서)
    async fn analytic_events(&self, _host: &str, _cursor: &crate::analytic::Cursor, _limit: u32) -> Result<crate::analytic::EventsPage, AppError> {
        Err(AppError::Rejected("web analytics is not supported by this backend".into()))
    }
}

#[async_trait]
impl<T: EvalBackend + ?Sized> EvalBackend for Arc<T> {
    async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
        (**self).save(req).await
    }
    async fn search(&self, req: &SearchRequest) -> Result<SearchResponse, AppError> {
        (**self).search(req).await
    }
    async fn status(&self, request_id: &str) -> Result<StatusResponse, AppError> {
        (**self).status(request_id).await
    }
    async fn vote(&self, req: &VoteRequest) -> Result<VoteResponse, AppError> {
        (**self).vote(req).await
    }
    async fn verdicts(&self, ids: &[String]) -> Result<VerdictsResponse, AppError> {
        (**self).verdicts(ids).await
    }
    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeResponse, AppError> {
        (**self).supersede(req).await
    }
    async fn session_start(&self) -> Result<SessionStart, AppError> {
        (**self).session_start().await
    }
    async fn session_status(&self, session: &Session) -> Result<SessionStatus, AppError> {
        (**self).session_status(session).await
    }
    fn set_session(&self, session: Session) {
        (**self).set_session(session)
    }
    fn session(&self) -> Session {
        (**self).session()
    }
    async fn analytic_site(&self, host: &str) -> Result<Value, AppError> {
        (**self).analytic_site(host).await
    }
    async fn analytic_events(&self, host: &str, cursor: &crate::analytic::Cursor, limit: u32) -> Result<crate::analytic::EventsPage, AppError> {
        (**self).analytic_events(host, cursor, limit).await
    }
}

/// 앱의 익명 자리값. 인터넷이 없거나 아직 세션을 받지 못했을 때 헤더에 이 값을 보낸다.
/// 서버는 0으로만 이뤄진 hash/token을 세션으로 보지 않고 게스트로 처리한다.
pub const ANONYMOUS_HASH: &str = "0x0000000000000000000000000000000000000000";
pub const ANONYMOUS_TOKEN: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000000";

/// 로그인 세션 자격 증명 (hash: 주소 40자리, token: 키 64자리, 0x 접두사 포함 가능)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub hash: String,
    pub token: String,
}

impl Session {
    /// 익명 자리값 세션. 앱의 기본값이다.
    pub fn anonymous() -> Self {
        Self {
            hash: ANONYMOUS_HASH.to_string(),
            token: ANONYMOUS_TOKEN.to_string(),
        }
    }

    /// hash 또는 token 중 하나라도 0이면 익명이다 (서버도 같은 기준으로 게스트 처리한다).
    pub fn is_anonymous(&self) -> bool {
        is_zero_hex(&self.hash) || is_zero_hex(&self.token)
    }
}

fn is_zero_hex(value: &str) -> bool {
    let digits = value.strip_prefix("0x").unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|b| b == b'0')
}

/// 멱등 키 한 개: 난수 16바이트를 hex 32자리로 만든다. 저장 한 건마다 새로 만들고, 재시도에서는 그대로 쓴다.
/// OS 난수 소스가 없으면 패닉한다 (정상 환경에서는 일어나지 않는다).
pub fn new_request_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random source unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 원문의 SHA-256 (hex 소문자 64자). 서버는 받은 원문으로 같은 값을 계산해 비교한다.
pub fn text_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Cloudflare Worker HTTP 구현
pub struct HttpBackend {
    base_url: String,
    client: reqwest::Client,
    /// 로그인 세션. 기본값은 익명 자리값이고, 요청마다 x-oauth-hash / x-oauth-token 헤더로 보낸다.
    /// 로그인하면 실행 중에 바뀌므로 RwLock으로 둔다.
    session: std::sync::RwLock<Session>,
}

impl HttpBackend {
    pub fn new(base_url: impl Into<String>) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
            session: std::sync::RwLock::new(Session::anonymous()),
        })
    }

    /// 로그인 세션을 붙인다. 서버 검증은 아직 연결되지 않아 지금은 게스트로 처리된다.
    pub fn with_session(self, hash: impl Into<String>, token: impl Into<String>) -> Self {
        self.set_session(Session {
            hash: hash.into(),
            token: token.into(),
        });
        self
    }

    pub fn set_session(&self, session: Session) {
        *self.session.write().unwrap_or_else(|e| e.into_inner()) = session;
    }

    pub fn session(&self) -> Session {
        self.session.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    async fn post<B, R>(&self, path: &str, body: &B) -> Result<R, AppError>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let session = self.session();
        let resp = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("x-oauth-hash", &session.hash)
            .header("x-oauth-token", &session.token)
            .json(body)
            .send()
            .await
            .map_err(send_error)?;
        read_response(resp).await
    }

    async fn get<R>(&self, path: &str, query: &[(&str, &str)]) -> Result<R, AppError>
    where
        R: DeserializeOwned,
    {
        self.get_as(path, query, &self.session()).await
    }

    /// 지정한 세션으로 조회한다 (아직 쓰기 시작하지 않은 로그인 대기 세션을 확인할 때)
    async fn get_as<R>(&self, path: &str, query: &[(&str, &str)], session: &Session) -> Result<R, AppError>
    where
        R: DeserializeOwned,
    {
        let resp = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .query(query)
            .header("x-oauth-hash", &session.hash)
            .header("x-oauth-token", &session.token)
            .send()
            .await
            .map_err(send_error)?;
        read_response(resp).await
    }
}

/// 연결 실패와 시간 초과는 Offline (큐에 두고 나중에 다시 보낸다)
fn send_error(e: reqwest::Error) -> AppError {
    if e.is_connect() || e.is_timeout() {
        AppError::Offline(e.to_string())
    } else {
        AppError::Http(e)
    }
}

async fn read_response<R: DeserializeOwned>(resp: reqwest::Response) -> Result<R, AppError> {
    let status = resp.status();
    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(AppError::Server {
            status: status.as_u16(),
            message,
        });
    }
    // 2xx: 서버는 요청을 받았을 수 있다. 응답 본문만 못 읽었다고 영구 실패로 보면, 서버에는 저장되었는데 앱은 거부로 기록한다.
    // - 본문이 중간에 끊겼거나(읽기 오류) JSON이 잘렸으면: 연결이 끊긴 것이다. Offline으로 보고 같은 request_id로 다시 보낸다.
    //   서버는 같은 request_id에 처음 결과를 돌려주므로(www findReplay) 응답을 회수할 수 있다.
    // - 끝까지 닿았는데 계약과 맞지 않으면: BadResponse (재시도 한도까지 다시 보낸다)
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Offline(format!("response body lost (status {status}): {e}")))?;
    serde_json::from_slice::<R>(&bytes).map_err(|e| {
        if e.is_eof() {
            AppError::Offline(format!("response body truncated (status {status}): {e}"))
        } else {
            AppError::BadResponse(format!("status {status}: {e}"))
        }
    })
}

#[async_trait]
impl EvalBackend for HttpBackend {
    async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
        self.post("/evaluations", req).await
    }

    async fn search(&self, req: &SearchRequest) -> Result<SearchResponse, AppError> {
        self.post("/evaluations/search", req).await
    }

    async fn status(&self, request_id: &str) -> Result<StatusResponse, AppError> {
        self.get("/evaluations/status", &[("request_id", request_id)]).await
    }

    async fn vote(&self, req: &VoteRequest) -> Result<VoteResponse, AppError> {
        self.post("/evaluations/votes", req).await
    }

    async fn verdicts(&self, ids: &[String]) -> Result<VerdictsResponse, AppError> {
        self.get("/evaluations/verdicts", &[("ids", &ids.join(","))]).await
    }

    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeResponse, AppError> {
        self.post("/evaluations/supersedes", req).await
    }

    async fn session_start(&self) -> Result<SessionStart, AppError> {
        // 발급에는 세션이 필요 없다. 익명 자리값으로 보낸다
        let resp = self
            .client
            .post(format!("{}/session/start", self.base_url))
            .header("x-oauth-hash", ANONYMOUS_HASH)
            .header("x-oauth-token", ANONYMOUS_TOKEN)
            .send()
            .await
            .map_err(send_error)?;
        read_response(resp).await
    }

    async fn session_status(&self, session: &Session) -> Result<SessionStatus, AppError> {
        self.get_as("/session", &[], session).await
    }

    fn set_session(&self, session: Session) {
        HttpBackend::set_session(self, session)
    }

    fn session(&self) -> Session {
        HttpBackend::session(self)
    }

    async fn analytic_site(&self, host: &str) -> Result<Value, AppError> {
        self.post("/analytic/sites", &serde_json::json!({ "host": host })).await
    }

    async fn analytic_events(&self, host: &str, cursor: &crate::analytic::Cursor, limit: u32) -> Result<crate::analytic::EventsPage, AppError> {
        let (since, limit) = (cursor.since.to_string(), limit.to_string());
        self.get("/analytic/events", &[("host", host), ("since", &since), ("after_id", &cursor.after_id), ("limit", &limit)]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_request_uses_ref_key_and_omits_none() {
        let req = SaveRequest {
            bcc: None,
            request_id: "test-request-id-000001".into(),
            from: None,
            to: None,
            cc: "example.com".into(),
            text: "원문".into(),
            text_hash: text_hash("원문"),
            vector: vec![0.0; 2],
            ref_: Some("0190f0a0-0000-7000-8000-000000000001".into()),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("ref").is_some());
        assert!(v.get("from").is_none());
        assert!(v.get("to").is_none());
        assert!(v.get("ref_").is_none());
    }

    #[test]
    fn save_request_sends_request_id() {
        let req = SaveRequest {
            bcc: None,
            request_id: "test-request-id-000002".into(),
            from: None,
            to: None,
            cc: "example.com".into(),
            text: "원문".into(),
            text_hash: text_hash("원문"),
            vector: vec![0.0; 2],
            ref_: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["request_id"], "test-request-id-000002");
    }

    #[test]
    fn new_request_id_is_32_hex_and_unique() {
        let a = new_request_id();
        let b = new_request_id();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn search_request_uses_top_k_camel_case() {
        let req = SearchRequest {
            vector: vec![0.0],
            top_k: 5,
            cc: None,
            root: None,
            ref_: None,
            to: None,
            tier: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["topK"], 5);
        assert!(v.get("top_k").is_none());
    }

    #[test]
    fn parses_save_response_with_null_ref() {
        let json = r#"{"id":"0190f0a0-0000-7000-8000-000000000001","root":"0190f0a0-0000-7000-8000-000000000001","ref":null,"from":"0x00000000000000000000000000000000000000ab"}"#;
        let r: SaveResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.ref_, None);
        assert_eq!(r.from.len(), 42);
    }

    #[test]
    fn parses_app_search_response_with_flat_fields() {
        let json = r#"{"matches":[{"id":"a","score":0.91,"cc":"example.com","root":"r","ref":null,"to":"0x0000000000000000000000000000000000000001"}]}"#;
        let r: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.matches[0].cc.as_deref(), Some("example.com"));
        assert_eq!(r.matches[0].to.as_deref(), Some("0x0000000000000000000000000000000000000001"));
        assert_eq!(r.matches[0].ref_, None);
    }

    #[test]
    fn parses_public_search_response_without_to() {
        let json = r#"{"matches":[{"id":"a","score":0.5,"cc":"example.com","root":"r","ref":null}]}"#;
        let r: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.matches[0].to, None);
    }

    #[test]
    fn anonymous_session_is_default() {
        let plain = HttpBackend::new("https://example.test").unwrap();
        assert_eq!(plain.session(), Session::anonymous());
        assert!(plain.session().is_anonymous());
    }

    #[test]
    fn anonymous_placeholders_have_web_hex_shape() {
        assert_eq!(ANONYMOUS_HASH.len(), 42);
        assert_eq!(ANONYMOUS_TOKEN.len(), 66);
        assert!(ANONYMOUS_HASH.starts_with("0x") && ANONYMOUS_TOKEN.starts_with("0x"));
        assert!(Session::anonymous().is_anonymous());
    }

    #[test]
    fn one_zero_side_still_counts_as_anonymous() {
        let half = Session {
            hash: ANONYMOUS_HASH.into(),
            token: format!("0x{}", "ab".repeat(32)),
        };
        assert!(half.is_anonymous());
        let real = Session {
            hash: "ab".repeat(20),
            token: "cd".repeat(32),
        };
        assert!(!real.is_anonymous());
    }

    #[test]
    fn with_session_replaces_anonymous() {
        let with = HttpBackend::new("https://example.test")
            .unwrap()
            .with_session("ab", "cd");
        assert_eq!(
            with.session(),
            Session {
                hash: "ab".into(),
                token: "cd".into()
            }
        );
        assert!(!with.session().is_anonymous());
    }

    /// 요청 한 건을 받아 헤더를 돌려주는 로컬 서버. 실제 HTTP 전송 결과를 확인하려고 쓴다.
    fn capture_one_request() -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            let header_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
                let n = sock.read(&mut chunk).unwrap();
                assert!(n > 0, "connection closed before headers");
                buf.extend_from_slice(&chunk[..n]);
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
            let content_len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map(|v| v.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            while buf.len() < header_end + content_len {
                let n = sock.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = r#"{"matches":[]}"#;
            write!(
                sock,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            head
        });
        (format!("http://{addr}"), handle)
    }

    fn empty_search() -> SearchRequest {
        SearchRequest {
            vector: vec![0.0],
            top_k: 1,
            cc: None,
            root: None,
            ref_: None,
            to: None,
            tier: None,
        }
    }

    #[tokio::test]
    async fn sends_anonymous_headers_by_default() {
        let (url, server) = capture_one_request();
        let backend = HttpBackend::new(url).unwrap();
        backend.search(&empty_search()).await.unwrap();
        let head = server.join().unwrap();
        assert!(head.contains(&format!("x-oauth-hash: {ANONYMOUS_HASH}")), "{head}");
        assert!(head.contains(&format!("x-oauth-token: {ANONYMOUS_TOKEN}")), "{head}");
    }

    #[tokio::test]
    async fn connection_refused_is_reported_as_offline() {
        // 포트 1은 보통 닫혀 있어서 연결이 즉시 거부된다
        let backend = HttpBackend::new("http://127.0.0.1:1").unwrap();
        let err = backend.search(&empty_search()).await.unwrap_err();
        assert!(matches!(err, AppError::Offline(_)), "{err:?}");
    }

    #[tokio::test]
    async fn sends_session_headers_when_set() {
        let (url, server) = capture_one_request();
        let backend = HttpBackend::new(url).unwrap().with_session("ab", "cd");
        backend.search(&empty_search()).await.unwrap();
        let head = server.join().unwrap();
        assert!(head.contains("x-oauth-hash: ab"), "{head}");
        assert!(head.contains("x-oauth-token: cd"), "{head}");
        assert!(!head.contains(ANONYMOUS_HASH), "{head}");
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn text_hash_matches_known_sha256() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            text_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn save_request_sends_text_and_hash() {
        let req = SaveRequest {
            bcc: None,
            request_id: "test-request-id-000003".into(),
            from: None,
            to: None,
            cc: "example.com".into(),
            text: "abc".into(),
            text_hash: text_hash("abc"),
            vector: vec![0.0; 2],
            ref_: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["text"], "abc");
        assert_eq!(v["text_hash"], text_hash("abc"));
    }

    #[test]
    fn pending_202_response_parses_with_status() {
        let json = r#"{"id":"a","root":"a","ref":null,"from":"0x00000000000000000000000000000000000000ab","status":"pending"}"#;
        let r: SaveResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.status.as_deref(), Some("pending"));
    }

    #[test]
    fn response_without_status_still_parses() {
        let json = r#"{"id":"a","root":"a","ref":null,"from":"0x00000000000000000000000000000000000000ab"}"#;
        let r: SaveResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.status, None);
    }

    #[test]
    fn status_response_parses_reason() {
        let json = r#"{"id":"a","root":"a","ref":null,"status":"rejected","reason":"profanity"}"#;
        let r: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.status, "rejected");
        assert_eq!(r.reason.as_deref(), Some("profanity"));
    }
}

#[cfg(test)]
mod response_recovery_tests {
    //! 2xx인데 응답 본문을 읽지 못한 경우: 서버는 요청을 받았을 수 있으므로 영구 실패로 버리지 않는다.
    use super::*;
    use crate::queue::OfflineQueue;

    /// 연결마다 정해 둔 응답을 차례로 돌려주는 로컬 서버. 요청 본문(JSON 문자열)을 모아 돌려준다.
    fn serve_script(script: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let (url, handle) = serve_script_full(script);
        (url, std::thread::spawn(move || handle.join().unwrap().into_iter().map(|(_, body)| body).collect()))
    }

    /// serve_script와 같지만 요청 머리글(소문자)도 함께 돌려준다: (머리글, 본문)
    fn serve_script_full(script: Vec<Vec<u8>>) -> (String, std::thread::JoinHandle<Vec<(String, String)>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for response in script {
                let started = std::time::Instant::now();
                let mut sock = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(_) if started.elapsed() < std::time::Duration::from_secs(10) => {
                            std::thread::sleep(std::time::Duration::from_millis(5))
                        }
                        Err(e) => panic!("no connection: {e}"),
                    }
                };
                sock.set_nonblocking(false).unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                    let n = sock.read(&mut chunk).unwrap();
                    assert!(n > 0, "closed before headers");
                    buf.extend_from_slice(&chunk[..n]);
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map(|v| v.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                while buf.len() < header_end + len {
                    let n = sock.read(&mut chunk).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                bodies.push((head, String::from_utf8_lossy(&buf[header_end..]).to_string()));
                sock.write_all(&response).unwrap();
                // 소켓을 닫는다. 본문이 약속한 길이보다 짧으면 클라이언트는 연결이 끊긴 것으로 본다
            }
            bodies
        });
        (format!("http://{addr}"), handle)
    }

    /// Content-Length를 따로 줄 수 있는 응답. 본문보다 큰 값을 주면 중간에 끊긴 응답이 된다.
    fn http(status: &str, body: &str, content_length: Option<usize>) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            content_length.unwrap_or(body.len())
        )
        .into_bytes()
    }

    const FULL: &str = r#"{"id":"0190f0a0-0000-7000-8000-000000000001","root":"0190f0a0-0000-7000-8000-000000000001","ref":null,"from":"0x00000000000000000000000000000000000000ab","status":"pending"}"#;

    fn save_req() -> SaveRequest {
        SaveRequest {
            bcc: None,
            request_id: "recovery-request-id-0001".into(),
            from: None,
            to: None,
            cc: "example.com".into(),
            text: "원문".into(),
            text_hash: text_hash("원문"),
            vector: vec![0.0; 2],
            ref_: None,
        }
    }

    #[tokio::test]
    async fn body_cut_off_mid_transfer_is_offline_not_a_permanent_failure() {
        let (url, server) = serve_script(vec![http("202 Accepted", &FULL[..40], Some(FULL.len()))]);
        let err = HttpBackend::new(url).unwrap().save(&save_req()).await.unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, AppError::Offline(_)), "{err:?}");
    }

    #[tokio::test]
    async fn complete_but_truncated_json_is_offline() {
        let (url, server) = serve_script(vec![http("200 OK", &FULL[..FULL.len() - 20], None)]);
        let err = HttpBackend::new(url).unwrap().save(&save_req()).await.unwrap_err();
        server.join().unwrap();
        assert!(matches!(err, AppError::Offline(_)), "{err:?}");
    }

    #[tokio::test]
    async fn complete_body_that_breaks_the_contract_is_a_retryable_bad_response() {
        for body in [r#"{"id":1}"#, "<html>captive portal</html>", "[]"] {
            let (url, server) = serve_script(vec![http("200 OK", body, None)]);
            let err = HttpBackend::new(url).unwrap().save(&save_req()).await.unwrap_err();
            server.join().unwrap();
            assert!(matches!(err, AppError::BadResponse(_)), "{body}: {err:?}");
        }
    }

    #[tokio::test]
    async fn error_statuses_and_good_bodies_behave_as_before() {
        let (url, server) = serve_script(vec![http("400 Bad Request", r#"{"error":"nope"}"#, None)]);
        let err = HttpBackend::new(url).unwrap().save(&save_req()).await.unwrap_err();
        server.join().unwrap();
        assert!(matches!(&err, AppError::Server { status: 400, message } if message.contains("nope")), "{err:?}");

        let (url, server) = serve_script(vec![http("202 Accepted", FULL, None)]);
        let ok = HttpBackend::new(url).unwrap().save(&save_req()).await.unwrap();
        server.join().unwrap();
        assert_eq!(ok.status.as_deref(), Some("pending"));
    }

    #[tokio::test]
    async fn queue_keeps_resending_with_the_same_request_id_and_recovers_the_servers_replay() {
        use crate::queue::MAX_SERVER_ATTEMPTS;
        // 서버는 매번 요청을 받았지만 응답이 중간에 끊긴다. 재시도 한도(MAX_SERVER_ATTEMPTS)를 넘겨도 버리지 않아야 한다.
        // 마지막에 서버가 같은 request_id에 처음 결과(200)를 돌려준다
        let cut = MAX_SERVER_ATTEMPTS as usize + 1;
        let mut script: Vec<Vec<u8>> = (0..cut).map(|_| http("202 Accepted", &FULL[..40], Some(FULL.len()))).collect();
        script.push(http("200 OK", FULL, None));
        let (url, server) = serve_script(script);
        let backend = HttpBackend::new(url).unwrap();
        let mut queue = OfflineQueue::default();
        queue.enqueue(save_req(), None).unwrap();

        for round in 1..=cut {
            let report = queue.flush(&backend).await;
            assert_eq!(report.sent.len(), 0, "round {round}");
            assert_eq!(report.remaining, 1, "round {round}: 버리지 않고 남겨 둔다");
            assert!(report.rejected.is_empty(), "round {round}");
            assert!(report.stopped.is_some(), "round {round}");
        }

        let last = queue.flush(&backend).await;
        assert_eq!(last.sent.len(), 1, "다시 보내서 응답을 회수했다");
        assert_eq!(last.remaining, 0);
        assert_eq!(last.sent[0].response.status.as_deref(), Some("pending"));

        let bodies = server.join().unwrap();
        assert_eq!(bodies.len(), cut + 1);
        for b in &bodies {
            let v: serde_json::Value = serde_json::from_str(b).unwrap();
            assert_eq!(v["request_id"], "recovery-request-id-0001", "같은 request_id로 다시 보냈다");
        }
    }

    const ISSUED: &str = r#"{"hash":"abababababababababababababababababababab","token":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd","address":"abababababababababababababababababababab.premium.discount@oauth.email","mailto":"mailto:abab.premium.discount%40oauth.email"}"#;

    #[tokio::test]
    async fn session_start_is_a_post_with_the_anonymous_placeholder_and_parses_the_issued_session() {
        let (url, server) = serve_script_full(vec![http("201 Created", ISSUED, None)]);
        let issued = HttpBackend::new(url).unwrap().session_start().await.unwrap();
        let (head, _) = server.join().unwrap().remove(0);
        assert!(head.starts_with("post /session/start "), "{head}");
        assert!(head.contains(&format!("x-oauth-hash: {ANONYMOUS_HASH}")), "{head}");
        assert_eq!(issued.hash.len(), 40);
        assert_eq!(issued.token.len(), 64);
        assert!(issued.mailto.starts_with("mailto:"));
    }

    #[tokio::test]
    async fn session_status_asks_with_the_given_session_not_the_active_one() {
        let (url, server) = serve_script_full(vec![http("200 OK", r#"{"state":"pending","address":null}"#, None)]);
        let backend = HttpBackend::new(url).unwrap();
        let waiting = Session { hash: "ab".repeat(20), token: "cd".repeat(32) };
        let st = backend.session_status(&waiting).await.unwrap();
        let (head, _) = server.join().unwrap().remove(0);
        assert_eq!((st.state.as_str(), st.address), ("pending", None));
        assert!(head.starts_with("get /session "), "{head}");
        assert!(head.contains(&format!("x-oauth-hash: {}", waiting.hash)), "{head}");
        assert!(backend.session().is_anonymous(), "묻는다고 해서 그 세션을 쓰기 시작하지 않는다");
    }

    #[tokio::test]
    async fn a_signed_in_status_carries_the_address_and_a_503_is_a_server_error() {
        let (url, _server) = serve_script_full(vec![http("200 OK", r#"{"state":"signed_in","address":"0x00000000000000000000000000000000000000cc"}"#, None)]);
        let st = HttpBackend::new(url).unwrap().session_status(&Session::anonymous()).await.unwrap();
        assert_eq!(st.address.as_deref(), Some("0x00000000000000000000000000000000000000cc"));

        let (url, _server) = serve_script_full(vec![http("503 Service Unavailable", r#"{"error":"login is not configured"}"#, None)]);
        let err = HttpBackend::new(url).unwrap().session_start().await.unwrap_err();
        assert!(matches!(&err, AppError::Server { status: 503, message } if message.contains("not configured")), "{err:?}");
    }

    #[tokio::test]
    async fn changing_the_session_at_runtime_changes_the_headers_of_later_requests() {
        let (url, server) = serve_script_full(vec![http("200 OK", r#"{"matches":[]}"#, None), http("200 OK", r#"{"matches":[]}"#, None)]);
        let backend = std::sync::Arc::new(HttpBackend::new(url).unwrap());
        let req = SearchRequest { vector: vec![0.0], top_k: 1, cc: None, root: None, ref_: None, to: None, tier: None };
        backend.search(&req).await.unwrap();
        // Arc<HttpBackend>도 같은 trait으로 세션을 바꾼다 (AppCore가 이렇게 쓴다)
        EvalBackend::set_session(&backend, Session { hash: "ab".repeat(20), token: "cd".repeat(32) });
        backend.search(&req).await.unwrap();
        let heads: Vec<String> = server.join().unwrap().into_iter().map(|(h, _)| h).collect();
        assert!(heads[0].contains(&format!("x-oauth-hash: {ANONYMOUS_HASH}")), "{}", heads[0]);
        assert!(heads[1].contains(&format!("x-oauth-hash: {}", "ab".repeat(20))), "{}", heads[1]);
        assert!(heads[1].contains(&format!("x-oauth-token: {}", "cd".repeat(32))), "{}", heads[1]);
    }
}
