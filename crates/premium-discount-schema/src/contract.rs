//! premium-discount-app 의 명령 계약: `serve_lines` · Tauri 셸 · 화면(`app/ui/commands.ts`)이 주고받는 인자와 결과,
//! 오류 종류. 앱(`app/src/commands.rs`)은 이 타입을 다시 내보내고 그대로 쓰며, premium-discount-mcp 는 같은 이름과
//! 오류 종류로 도구를 만든다. 명령 이름과 인자 모양의 기준은 `app/ui/fixtures/command_contract.json` 이고, 이 crate 의
//! 시험과 앱 · 화면의 시험이 같은 파일을 읽는다.
//!
//! 앱 안의 타입에 기대는 것(판정 캐시 행 `VerdictsResult`, 모델 상주 상태가 붙는 `AppStatus`, 모델 내려받기 · 초안
//! 인자)은 앱에 남는다.

use serde::{Deserialize, Serialize};

/// 검색 개수 기본값 (인자를 생략했을 때)
pub const DEFAULT_TOP_K: u32 = 10;

fn default_top_k() -> u32 {
    DEFAULT_TOP_K
}

/// UI는 "없음"을 빈 문자열로 들고 있다 (`app/ui/localdb.ts`). 빈 문자열과 공백은 "없음"으로 읽는다.
pub fn blank_to_none(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

/// 앱이 여는 명령 이름. 코어(`AppCore::dispatch`) 17개(웹 행동 분석 4개 포함), 셸이 모델 없이도 답하는 모델 명령 3개, 셸의 초안 엔진 1개.
pub const COMMANDS: [&str; 21] = [
    "cmd_submit_evaluation",
    "cmd_sync_cache",
    "cmd_search_local",
    "cmd_search_remote",
    "cmd_search_merged",
    "cmd_flush_outbox",
    "cmd_vote",
    "cmd_verdicts",
    "cmd_app_status",
    "cmd_session",
    "cmd_login_start",
    "cmd_login_status",
    "cmd_logout",
    "cmd_model_status",
    "cmd_model_download",
    "cmd_model_download_cancel",
    "cmd_item_draft",
    "cmd_analytic_site",
    "cmd_analytic_sync",
    "cmd_analytic_report",
    "cmd_analytic_forget",
];

/// [`CommandError::kind`] 의 값. 앞의 열 개는 앱 오류(`AppError`)에서, `not_ready` · `unknown_command` · `internal` 은
/// 명령 계층에서 온다. `unavailable` 은 앱 프로세스를 띄우거나 닿을 수 없을 때 premium-discount-mcp 가 쓰고,
/// `unknown` 은 알 수 없는 종류를 받았을 때의 자리다.
pub const ERROR_KINDS: [&str; 15] = [
    "invalid_input",
    "embed",
    "filter",
    "offline",
    "http",
    "bad_response",
    "server",
    "rejected",
    "queue_full",
    "storage",
    "not_ready",
    "unknown_command",
    "internal",
    "unavailable",
    "unknown",
];

// ---- 인자 · 결과 (app/src/commands.rs 에서 옮김) ----

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitArgs {
    pub text: String,
    pub cc: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// 콘텐츠 해시 주소 (0x + 40자리)
    #[serde(default)]
    pub bcc: Option<String>,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
    /// 부모가 아직 서버에 없을 때 보관함의 로컬 id
    #[serde(default)]
    pub local_parent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SyncArgs {
    /// 마지막으로 반영한 updated_at (밀리초). 처음에는 0
    #[serde(default)]
    pub since: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchLocalArgs {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    #[serde(default)]
    pub cc: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// 등급 필터: premium | discount | rated (빈 문자열은 없음)
    #[serde(default)]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoteArgs {
    /// 평가의 서버 id
    pub id: String,
    /// 1 추천 | -1 비추천 | 0 철회
    pub value: i32,
    /// vote(기본) | vote_outcome
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VerdictsArgs {
    pub ids: Vec<String>,
    /// true면 캐시가 새것이어도 서버에 묻는다
    #[serde(default)]
    pub refresh: bool,
}

/// 웹 행동 분석: 사이트 확인 (`cmd_analytic_site`)
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticSiteArgs {
    /// 사이트 호스트 (`shop.example.com`, 주소를 넣어도 호스트만 쓴다)
    pub host: String,
}

/// 웹 행동 분석: 원시 사건 받기 + 기기 구조화 (`cmd_analytic_sync`)
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticSyncArgs {
    pub host: String,
    /// 받을 쪽 수 상한 (한 쪽 최대 500행). 생략하면 앱의 기본값
    #[serde(default)]
    pub max_pages: Option<u32>,
}

/// 웹 행동 분석: 자연어 질의 리포트 (`cmd_analytic_report`)
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticReportArgs {
    pub host: String,
    /// "이번 달 결제 이탈 원인이 뭐야?" 같은 질의. 기간은 앱이 사용자 시계로 다시 계산한다
    pub query: String,
    /// 사용자 언어 (시간대 · 반구). 생략하면 ko
    #[serde(default)]
    pub lang: Option<String>,
    /// 벡터 검색 결과 수
    #[serde(default)]
    pub top_k: Option<u32>,
}

/// 웹 행동 분석: 사건 지우기 (`cmd_analytic_forget`). 지운 id 는 다시 받지 않는다(묘비)
#[derive(Debug, Clone, Deserialize)]
pub struct AnalyticForgetArgs {
    pub ids: Vec<String>,
}

/// 저장 결과. `status`로 구분한다.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SubmitResult {
    /// 로컬 필터에서 욕설로 판정되었다. 임베딩도 로컬 저장도 전송도 하지 않았다
    Blocked,
    /// 서버에 저장이 끝났다
    Saved {
        id: String,
        root: String,
        #[serde(rename = "ref")]
        ref_: Option<String>,
        from: String,
    },
    /// 서버가 접수했고 욕설 검사와 저장이 진행 중이다. 최종 결과는 `cmd_flush_outbox`가 확인한다
    Accepted {
        id: String,
        root: String,
        #[serde(rename = "ref")]
        ref_: Option<String>,
        from: String,
    },
    /// 연결이 없거나 앞선 항목을 기다린다. 로컬에는 저장되어 있고 나중에 전송된다
    Queued { local_id: String },
}

/// 내 글 검색 결과 한 건 (원문 포함)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalMatchDto {
    pub request_id: String,
    pub local_id: String,
    pub id: Option<String>,
    pub cc: String,
    pub state: String,
    pub text: String,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RejectedDto {
    pub local_id: String,
    pub reason: String,
}

/// 투표 결과. `status`로 구분한다 (서버가 거부하면 rejected 오류)
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VoteResult {
    /// 서버가 받았다. counted가 false면 판정에 들어가지 않는 표다 (로그인 확인이 안 됐거나 내 글)
    Sent { request_id: String, counted: bool },
    /// 연결이 없어 보관함에 남았다. 다음 `cmd_flush_outbox`가 보낸다
    Queued { request_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoteRejectedDto {
    pub request_id: String,
    pub eval_id: String,
    pub reason: String,
}

/// `cmd_flush_outbox` 결과: 전송 결과와 상태 확인 결과
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct FlushSummary {
    /// 이번에 서버로 보낸 항목 수
    pub sent: usize,
    /// 서버가 영구적으로 거부해서 버린 항목
    pub rejected: Vec<RejectedDto>,
    /// 아직 못 보낸 항목 수
    pub remaining: usize,
    /// 연결이나 서버 일시 오류로 전송이 멈춘 이유
    pub stopped: Option<String>,
    /// 이번에 저장 완료로 확인된 항목 수
    pub stored: usize,
    /// 확인 결과 서버가 욕설 등으로 거부했거나 실패로 끝낸 항목
    pub sync_rejected: Vec<RejectedDto>,
    /// 서버가 기록을 지워서 결과를 알 수 없는 항목 수
    pub missing: usize,
    /// 아직 결과를 기다리는 항목 수
    pub still_pending: usize,
    /// 상태 확인이 멈춘 이유
    pub sync_stopped: Option<String>,
    /// 로컬 저장소 기록에 실패한 이유
    pub persist_error: Option<String>,
    /// 이번에 보낸 투표 수
    pub votes_sent: usize,
    /// 아직 못 보낸 투표 수
    pub votes_remaining: usize,
    /// 서버가 거부했거나 재시도 한도를 넘긴 투표
    pub votes_rejected: Vec<VoteRejectedDto>,
    /// 투표 전송이 멈춘 이유
    pub votes_stopped: Option<String>,
}

/// 앱이 쓰는 경로와 서버 (화면의 안내문에 보여 준다)
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AppInfo {
    pub model_dir: String,
    pub data_dir: String,
    pub server_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionArgs {
    /// true면 서버에 물어 로그인이 아직 유효한지 확인한다. 서버가 아니라고 하면 로그아웃한다 (연결이 없으면 그대로 둔다)
    #[serde(default)]
    pub verify: bool,
}

/// 지금 로그인 상태
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionInfo {
    /// anonymous | signed_in
    pub state: &'static str,
    pub address: Option<String>,
    /// 알려 줄 일이 있을 때의 문구 (예: 저장된 로그인 정보를 읽지 못함, 로그인이 만료됨)
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoginStartResult {
    /// 메일을 보낼 주소 (QR을 못 쓰는 환경에서 직접 보낸다)
    pub address: String,
    /// QR에 넣을 mailto: 링크
    pub mailto: String,
    pub expires_in_sec: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoginStatus {
    /// none(시작한 로그인 없음) | pending(메일 대기) | signed_in | expired(시간이 지났거나 서버가 세션을 모른다)
    pub state: &'static str,
    pub address: Option<String>,
    /// pending일 때 남은 시간
    pub remaining_sec: Option<u64>,
    /// 로그인은 되었지만 이 기기에 저장하지 못한 경우 등
    pub warning: Option<String>,
}

/// UI로 돌려주는 오류. `kind`로 분기하고 `message`는 사람이 읽는 설명이다.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandError {
    pub kind: &'static str,
    pub message: String,
}

impl CommandError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const CONTRACT: &str = include_str!("../../../app/ui/fixtures/command_contract.json");

    fn contract() -> serde_json::Map<String, Value> {
        let v: Value = serde_json::from_str(CONTRACT).unwrap();
        v.as_object().unwrap().clone()
    }

    /// 계약 파일의 명령은 이 crate 가 아는 명령과 같다 (화면 · 앱 · 이 crate 가 한 목록을 쓴다)
    #[test]
    fn the_command_list_is_the_contract_files() {
        let mut in_file: Vec<String> = contract().keys().filter(|k| !k.starts_with('_')).cloned().collect();
        in_file.sort();
        let mut known: Vec<String> = COMMANDS.iter().map(|s| s.to_string()).collect();
        known.sort();
        assert_eq!(in_file, known);
    }

    /// 계약 파일의 인자가 이 crate 의 인자 타입으로 그대로 읽힌다
    #[test]
    fn contract_arguments_parse_into_the_argument_types() {
        let c = contract();
        let args = |cmd: &str| -> Value {
            let a = c[cmd]["args"].clone();
            if a.is_null() {
                Value::Object(Default::default())
            } else {
                a
            }
        };
        let s: SubmitArgs = serde_json::from_value(args("cmd_submit_evaluation")).unwrap();
        assert_eq!((s.cc.as_str(), blank_to_none(s.to.clone())), ("example.com", None));
        assert_eq!(s.bcc.as_deref(), Some("0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"));
        let _: SyncArgs = serde_json::from_value(args("cmd_sync_cache")).unwrap();
        let l: SearchLocalArgs = serde_json::from_value(args("cmd_search_local")).unwrap();
        assert_eq!(l.top_k, 5);
        for cmd in ["cmd_search_remote", "cmd_search_merged"] {
            let a: SearchArgs = serde_json::from_value(args(cmd)).unwrap();
            assert_eq!(blank_to_none(a.tier), None, "{cmd}");
        }
        let v: VoteArgs = serde_json::from_value(args("cmd_vote")).unwrap();
        assert_eq!((v.value, v.kind.as_deref()), (1, Some("vote")));
        let d: VerdictsArgs = serde_json::from_value(args("cmd_verdicts")).unwrap();
        assert_eq!(d.ids.len(), 1);
        let _: SessionArgs = serde_json::from_value(args("cmd_session")).unwrap();
        let s: AnalyticSiteArgs = serde_json::from_value(args("cmd_analytic_site")).unwrap();
        assert_eq!(s.host, "shop.example.com");
        let y: AnalyticSyncArgs = serde_json::from_value(args("cmd_analytic_sync")).unwrap();
        assert_eq!(y.max_pages, Some(5));
        let r: AnalyticReportArgs = serde_json::from_value(args("cmd_analytic_report")).unwrap();
        assert_eq!((r.lang.as_deref(), r.top_k), (Some("ko"), Some(5)));
        let f: AnalyticForgetArgs = serde_json::from_value(args("cmd_analytic_forget")).unwrap();
        assert_eq!(f.ids.len(), 1);
        // 생략한 개수는 기본값
        let a: SearchArgs = serde_json::from_value(serde_json::json!({"query": "q"})).unwrap();
        assert_eq!(a.top_k, DEFAULT_TOP_K);
    }

    #[test]
    fn results_keep_their_wire_names() {
        let r = serde_json::to_value(SubmitResult::Saved { id: "i".into(), root: "r".into(), ref_: None, from: "f".into() }).unwrap();
        assert_eq!(r["status"], "saved");
        assert!(r.get("ref").is_some());
        let q = serde_json::to_value(VoteResult::Queued { request_id: "x".into() }).unwrap();
        assert_eq!(q["status"], "queued");
        let e = serde_json::to_value(CommandError::new("offline", "no network")).unwrap();
        assert_eq!(e, serde_json::json!({"kind": "offline", "message": "no network"}));
        assert!(ERROR_KINDS.contains(&"unavailable"));
        assert_eq!(blank_to_none(Some("  ".into())), None);
    }
}
