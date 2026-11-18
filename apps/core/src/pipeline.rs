use std::collections::HashMap;

use serde::Serialize;

use crate::api::{
    new_request_id, text_hash, EvalBackend, Match, SaveRequest, SaveResponse, SearchRequest, SearchResponse,
    StatusResponse,
};
use crate::embedding::{check_contract, l2_normalize, Embedder};
use crate::embeddinggemma::{check_budget, with_prompt, Input, Role};
use crate::error::AppError;
use crate::filter::{ProfanityFilter, Verdict};
use crate::local::LocalMatch;
use crate::outbox::Outbox;
use crate::queue::{FlushReport, OfflineQueue, ParentRef, SyncReport};

pub const TOP_K_MIN: u32 = 1;
pub const TOP_K_MAX: u32 = 50;
/// 원문 최대 크기 (UTF-8 바이트). 서버의 MAX_TEXT_BYTES와 같아야 한다
pub const MAX_TEXT_BYTES: usize = 8192;
/// ZeroAddress는 주소 컬럼에서 "없음"을 뜻한다. 서버는 저장할 때 to를 NULL로 바꾼다
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// 저장 입력. 원문은 서버로 함께 보낸다 (서버가 욕설 검사 후 저장한다). 로컬 필터를 먼저 거친다.
#[derive(Debug, Clone, Default)]
pub struct SubmitInput {
    pub text: String,
    pub cc: String,
    /// 없으면 게스트 주소로 저장되고, 응답의 from을 앱이 보관해야 한다
    pub from: Option<String>,
    pub to: Option<String>,
    /// 콘텐츠 해시 주소 (0x + 40자리). 서버(evaluations.bcc)와 로컬 저장소(bcc)에 같은 값으로 남는다
    pub bcc: Option<String>,
    pub ref_: Option<String>,
    /// 부모가 아직 서버에 없을 때 큐의 로컬 id (submit_queued 전용). ref_와 함께 쓸 수 없다.
    pub local_parent: Option<String>,
}

#[derive(Debug)]
pub enum SubmitOutcome {
    /// 로컬 필터에서 욕설로 판정되어 임베딩도 전송도 하지 않음
    Blocked,
    /// 서버에 저장이 끝났다 (status: stored)
    Saved(SaveResponse),
    /// 서버가 대기열에 받았다 (status: pending). 욕설 검사와 저장은 서버에서 아직 진행 중이다.
    /// 최종 결과는 status_of(request_id)로 확인한다.
    Accepted(SaveResponse),
    /// 연결이 없거나 앞선 항목을 기다리는 중. flush_queue로 나중에 보낸다.
    Queued { local_id: String },
}

/// 서버 응답의 status를 결과로 바꾼다. 거부되었거나 실패했으면 Rejected 에러다.
fn outcome_from_response(resp: SaveResponse) -> Result<SubmitOutcome, AppError> {
    match resp.status.as_deref() {
        None | Some("stored") => Ok(SubmitOutcome::Saved(resp)),
        Some("pending") | Some("processing") => Ok(SubmitOutcome::Accepted(resp)),
        Some(other) => Err(AppError::Rejected(format!("server status {other}"))),
    }
}

/// 검사와 임베딩이 끝난 저장 요청. parent는 답글일 때만 있다.
// 값은 저장 한 건마다 하나 만들어 바로 쓰고 버린다. Box로 감싸도 얻는 것이 없다
#[allow(clippy::large_enum_variant)]
enum Prepared {
    Blocked,
    Ready {
        req: SaveRequest,
        parent: Option<ParentRef>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub top_k: u32,
    pub cc: Option<String>,
    pub root: Option<String>,
    pub ref_: Option<String>,
    pub to: Option<String>,
    /// 등급 필터: premium | discount | rated. 판정은 서버에만 있으므로 이 필터가 있으면 로컬 결과를 섞지 않는다
    pub tier: Option<String>,
}

/// 검색의 등급 필터 값
pub const TIER_FILTERS: [&str; 3] = ["premium", "discount", "rated"];

pub struct Evaluator<B, E, F> {
    backend: B,
    embedder: E,
    filter: F,
}

impl<B: EvalBackend, E: Embedder, F: ProfanityFilter> Evaluator<B, E, F> {
    pub fn new(backend: B, embedder: E, filter: F) -> Self {
        Self { backend, embedder, filter }
    }

    /// 서버 백엔드 (로그인 세션을 바꾸고 확인하는 데 쓴다)
    pub fn embedder(&self) -> &E {
        &self.embedder
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// 평가 저장 (온라인 전용): 입력 검사 -> 욕설 필터 -> 임베딩 -> 서버 저장.
    /// 연결이 없으면 에러를 그대로 돌려준다. 오프라인에서도 저장하려면 submit_queued를 쓴다.
    pub async fn submit(&self, input: SubmitInput) -> Result<SubmitOutcome, AppError> {
        match self.prepare(&input)? {
            Prepared::Blocked => Ok(SubmitOutcome::Blocked),
            Prepared::Ready { mut req, parent } => {
                req.ref_ = match parent {
                    None => None,
                    Some(ParentRef::Server(id)) => Some(id),
                    Some(ParentRef::Local(_)) => {
                        return Err(AppError::InvalidInput(
                            "local_parent needs submit_queued".into(),
                        ));
                    }
                };
                let resp = self.backend.save(&req).await?;
                outcome_from_response(resp)
            }
        }
    }

    /// 평가 저장 (오프라인 허용): 요청을 큐 끝에 넣고, 큐를 앞에서부터 보낸다.
    /// 이번 항목이 서버에 저장됐으면 Saved, 연결이 없거나 앞선 항목을 기다리면 Queued.
    /// 욕설로 판정된 입력은 큐에 들어가지 않는다.
    pub async fn submit_queued(
        &self,
        input: SubmitInput,
        queue: &mut OfflineQueue,
    ) -> Result<SubmitOutcome, AppError> {
        let (req, parent) = match self.prepare(&input)? {
            Prepared::Blocked => return Ok(SubmitOutcome::Blocked),
            Prepared::Ready { req, parent } => (req, parent),
        };
        let local_id = queue.enqueue(req, parent)?;
        let report = queue.flush(&self.backend).await;
        if let Some(sent) = report.sent.into_iter().find(|s| s.local_id == local_id) {
            return outcome_from_response(sent.response);
        }
        if let Some(rejected) = report.rejected.into_iter().find(|r| r.local_id == local_id) {
            return Err(AppError::Rejected(rejected.reason));
        }
        Ok(SubmitOutcome::Queued { local_id })
    }

    /// 평가 저장 (로컬 원본 저장소 포함): 입력 검사 -> 욕설 필터 -> 임베딩 -> 로컬 저장소에 기록 -> 서버 전송.
    /// 원문과 벡터가 로컬 디스크에 먼저 남는다. 연결이 없으면 Queued로 돌려주고, 이후 flush_outbox로 보낸다.
    /// 욕설로 막힌 입력은 로컬에도 남기지 않는다.
    pub async fn submit_outbox(&self, input: SubmitInput, outbox: &mut Outbox) -> Result<SubmitOutcome, AppError> {
        let (req, parent) = match self.prepare(&input)? {
            Prepared::Blocked => return Ok(SubmitOutcome::Blocked),
            Prepared::Ready { req, parent } => (req, parent),
        };
        let local_id = outbox.enqueue(req, parent).await?;
        let report = outbox.flush(&self.backend).await;
        if let Some(sent) = report.sent.into_iter().find(|s| s.local_id == local_id) {
            return outcome_from_response(sent.response);
        }
        if let Some(rejected) = report.rejected.into_iter().find(|r| r.local_id == local_id) {
            return Err(AppError::Rejected(rejected.reason));
        }
        Ok(SubmitOutcome::Queued { local_id })
    }

    /// 보관함에 쌓인 저장을 보낸다. 앱을 시작할 때와 연결이 돌아왔을 때 호출한다.
    pub async fn flush_outbox(&self, outbox: &mut Outbox) -> FlushReport {
        outbox.flush(&self.backend).await
    }

    /// 서버가 접수(pending)만 한 항목의 최종 결과를 확인해 보관함의 행에 반영한다.
    pub async fn sync_outbox_status(&self, outbox: &mut Outbox) -> SyncReport {
        outbox.sync_status(&self.backend).await
    }

    /// 내 글 검색 (로컬 벡터 검색). 서버에 가지 않고, 내가 쓴 글의 원문과 함께 돌려준다.
    /// 질의는 서버 검색과 같은 질의 접두어로 임베딩한다 (저장할 때는 문서 접두어를 붙였다).
    pub async fn search_local(
        &self,
        text: &str,
        top_k: u32,
        cc: Option<&str>,
        outbox: &Outbox,
    ) -> Result<Vec<LocalMatch>, AppError> {
        let text = non_empty("query", text)?;
        if !(TOP_K_MIN..=TOP_K_MAX).contains(&top_k) {
            return Err(AppError::InvalidInput(format!(
                "top_k must be between {TOP_K_MIN} and {TOP_K_MAX}"
            )));
        }
        let cc = cc.map(normalize_cc).transpose()?;
        let vector = self.embed_checked(&with_prompt(Role::Query, text))?;
        outbox.search(&vector, top_k as usize, cc.as_deref()).await
    }

    /// 큐에 쌓인 저장을 보낸다. 앱을 시작할 때와 연결이 돌아왔을 때 호출한다.
    pub async fn flush_queue(&self, queue: &mut OfflineQueue) -> FlushReport {
        queue.flush(&self.backend).await
    }

    /// 서버가 접수(pending)만 한 항목의 최종 결과를 확인해 큐에 반영한다.
    /// 앱을 시작할 때, flush_queue 뒤, 그리고 주기적으로 호출한다.
    pub async fn sync_queue_status(&self, queue: &mut OfflineQueue) -> SyncReport {
        queue.sync_status(&self.backend).await
    }

    /// 입력 검사 -> 욕설 필터 -> 임베딩. 저장 요청은 ref_를 비우고 부모는 따로 돌려준다.
    fn prepare(&self, input: &SubmitInput) -> Result<Prepared, AppError> {
        let text = non_empty("text", &input.text)?;
        if text.len() > MAX_TEXT_BYTES {
            return Err(AppError::InvalidInput(format!(
                "text must be at most {MAX_TEXT_BYTES} bytes"
            )));
        }
        let cc = normalize_cc(&input.cc)?;
        let from = input
            .from
            .as_deref()
            .map(|a| normalize_address("from", a))
            .transpose()?;
        let to = input
            .to
            .as_deref()
            .map(|a| normalize_address("to", a))
            .transpose()?;
        let bcc = input
            .bcc
            .as_deref()
            .map(|a| normalize_address("bcc", a))
            .transpose()?;
        let parent = match (input.ref_.as_deref(), input.local_parent.as_deref()) {
            (Some(_), Some(_)) => {
                return Err(AppError::InvalidInput(
                    "ref and local_parent cannot both be set".into(),
                ));
            }
            (Some(r), None) => Some(ParentRef::Server(validate_uuid("ref", r)?)),
            (None, Some(local)) => Some(ParentRef::Local(non_empty("local_parent", local)?.to_string())),
            (None, None) => None,
        };

        // 저장하는 원문에는 문서 접두어를 붙여서 임베딩한다. 서버로 보내는 원문과 해시는 접두어 없이 그대로다
        let prompted = with_prompt(Role::Document, text);
        // 토큰 예산을 넘는 입력은 필터와 모델을 돌리기 전에 막는다
        self.check_token_budget(&prompted)?;

        if self.filter.check(text)? == Verdict::Profane {
            return Ok(Prepared::Blocked);
        }

        let vector = self.embed_normalized(&prompted)?;
        Ok(Prepared::Ready {
            req: SaveRequest {
                request_id: new_request_id(),
                from,
                to,
                bcc,
                cc,
                text: text.to_string(),
                text_hash: text_hash(text),
                vector,
                ref_: None,
            },
            parent,
        })
    }

    /// 처리 상태 조회 (request_id 기준). stored | pending | processing | rejected | failed
    pub async fn status_of(&self, request_id: &str) -> Result<StatusResponse, AppError> {
        self.backend.status(request_id).await
    }

    /// 검색 요청 만들기: 질의와 필터를 검사하고 질의 접두어를 붙여 임베딩한다 (욕설 필터는 저장 경로에만 적용)
    fn build_search_request(&self, text: &str, opts: &SearchOptions) -> Result<SearchRequest, AppError> {
        let text = non_empty("query", text)?;
        if !(TOP_K_MIN..=TOP_K_MAX).contains(&opts.top_k) {
            return Err(AppError::InvalidInput(format!(
                "top_k must be between {TOP_K_MIN} and {TOP_K_MAX}"
            )));
        }
        let cc = opts.cc.as_deref().map(normalize_cc).transpose()?;
        let root = opts.root.as_deref().map(|r| validate_uuid("root", r)).transpose()?;
        let ref_ = opts.ref_.as_deref().map(|r| validate_uuid("ref", r)).transpose()?;
        let to = opts.to.as_deref().map(|a| normalize_address("to", a)).transpose()?;
        // 서버는 검색의 to 필터가 ZeroAddress이면 400으로 거부한다 (ZeroAddress는 "없음"을 뜻하므로 필터가 될 수 없다)
        if to.as_deref() == Some(ZERO_ADDRESS) {
            return Err(AppError::InvalidInput("to must be a non-zero address".into()));
        }
        let tier = match opts.tier.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(t) if TIER_FILTERS.contains(&t) => Some(t.to_string()),
            Some(t) => return Err(AppError::InvalidInput(format!("tier must be premium, discount or rated, got {t}"))),
        };

        // 검색 질의에는 질의 접두어를 붙여서 임베딩한다
        let vector = self.embed_checked(&with_prompt(Role::Query, text))?;
        Ok(SearchRequest {
            vector,
            top_k: opts.top_k,
            cc,
            root,
            ref_,
            to,
            tier,
        })
    }

    /// 평가 확인: 질문 임베딩 -> 서버 벡터 검색 (욕설 필터는 저장 경로에만 적용)
    pub async fn search(&self, text: &str, opts: SearchOptions) -> Result<SearchResponse, AppError> {
        let req = self.build_search_request(text, &opts)?;
        self.backend.search(&req).await
    }

    /// 내 글(로컬)과 서버 검색을 한 번의 임베딩으로 함께 찾아 합친다.
    /// 방금 저장한 글은 서버 쪽 처리(최대 1분)가 끝나기 전에는 서버 검색에 나오지 않는다. 로컬 검색은 바로 찾으므로,
    /// 합치면 그 지연이 보이지 않는다. 같은 글은 서버 id로 하나로 합친다.
    /// - 로컬 검색은 cc 필터만 지원한다. root·ref·to·tier 필터가 있으면 로컬 결과를 섞지 않는다 (필터를 어기게 된다).
    /// - 서버 검색이 실패하면(오프라인 등) 오류 대신 로컬 결과를 돌려주고 이유를 remote_error에 담는다.
    pub async fn search_merged(&self, text: &str, opts: SearchOptions, outbox: &Outbox) -> Result<MergedSearch, AppError> {
        let req = self.build_search_request(text, &opts)?;
        let top_k = req.top_k as usize;

        let local = if req.root.is_none() && req.ref_.is_none() && req.to.is_none() && req.tier.is_none() {
            outbox.search(&req.vector, top_k, req.cc.as_deref()).await?
        } else {
            Vec::new()
        };
        let (remote, remote_error) = match self.backend.search(&req).await {
            Ok(resp) => (resp.matches, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
        Ok(MergedSearch {
            matches: merge_matches(local, remote, top_k),
            remote_error,
        })
    }

    /// 토큰 예산 사전 검사 (embeddinggemma::check_budget). 입력이 모델 한도(MAX_INPUT_TOKENS)를 넘으면 모델을 돌리기 전에
    /// InvalidInput으로 거부한다. 토큰을 셀 수 없는 임베더는 건너뛴다.
    fn check_token_budget(&self, prompted: &str) -> Result<(), AppError> {
        if let Some(tokens) = self.embedder.token_count(prompted)? {
            check_budget(&[Input::Text { tokens }])?;
        }
        Ok(())
    }

    /// 예산 검사 + 임베딩 (검색 질의용)
    fn embed_checked(&self, prompted: &str) -> Result<Vec<f32>, AppError> {
        self.check_token_budget(prompted)?;
        self.embed_normalized(prompted)
    }

    fn embed_normalized(&self, prompted: &str) -> Result<Vec<f32>, AppError> {
        // 임베더 출력은 정규화되어 있다고 가정하지 않는다. 저장과 검색 모두 같은 정규화를 거친다
        let v = l2_normalize(self.embedder.embed(prompted)?)?;
        check_contract(&v)?;
        Ok(v)
    }
}

/// 병합 검색 결과의 출처
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchSource {
    /// 내 글 (로컬에만 있다. 서버가 아직 처리 중이거나 서버에 없다)
    Local,
    /// 서버에만 있다 (남의 글이거나, 이 기기에 로컬 기록이 없는 내 글)
    Remote,
    /// 로컬과 서버 양쪽에 있다 (같은 서버 id)
    Both,
}

/// 병합 검색 결과 한 건. 서버 응답에는 원문이 없으므로 text는 내 글(로컬)일 때만 있다.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergedMatch {
    /// 서버 id. 서버가 아직 접수하지 않은 글은 None
    pub id: Option<String>,
    /// 내 글의 멱등 키 (로컬에 있을 때만)
    pub request_id: Option<String>,
    pub score: f32,
    pub cc: Option<String>,
    pub root: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub to: Option<String>,
    pub text: Option<String>,
    /// 로컬 상태 (queued | pending | stored | rejected | failed | missing). 로컬에 있을 때만
    pub state: Option<String>,
    pub source: MatchSource,
    /// 서버의 전역 판정 (서버 결과에 있을 때만)
    pub verdict: Option<crate::api::MatchVerdict>,
    /// 이 기기에서 한 표 (1 | -1). 앱 코어가 채운다
    pub my_vote: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergedSearch {
    pub matches: Vec<MergedMatch>,
    /// 서버 검색이 실패했을 때의 이유. 이때 matches는 로컬 결과뿐이다
    pub remote_error: Option<String>,
}

/// 로컬 결과와 서버 결과를 서버 id 기준으로 합친다. 점수 내림차순, 같으면 로컬 → 양쪽 → 서버 순으로 정렬하고 top_k개만 남긴다.
/// 같은 글이 양쪽에 있으면 하나로 합치고 점수는 큰 쪽을 쓴다 (원문은 로컬, 스레드 정보는 서버에서 온다).
pub fn merge_matches(local: Vec<LocalMatch>, remote: Vec<Match>, top_k: usize) -> Vec<MergedMatch> {
    let mut remote_by_id: HashMap<String, Match> = remote.into_iter().map(|m| (m.id.clone(), m)).collect();
    let mut merged: Vec<MergedMatch> = Vec::new();

    for l in local {
        let twin = l.id.as_ref().and_then(|id| remote_by_id.remove(id));
        merged.push(match twin {
            Some(r) => MergedMatch {
                id: Some(r.id),
                request_id: Some(l.request_id),
                score: l.score.max(r.score),
                cc: r.cc.or(Some(l.cc)),
                root: r.root,
                ref_: r.ref_,
                to: r.to,
                text: Some(l.text),
                state: Some(l.state.as_str().to_string()),
                source: MatchSource::Both,
                verdict: r.verdict,
                my_vote: None,
            },
            None => MergedMatch {
                id: l.id,
                request_id: Some(l.request_id),
                score: l.score,
                cc: Some(l.cc),
                root: None,
                ref_: None,
                to: None,
                text: Some(l.text),
                state: Some(l.state.as_str().to_string()),
                source: MatchSource::Local,
                verdict: None,
                my_vote: None,
            },
        });
    }
    for (_, r) in remote_by_id {
        merged.push(MergedMatch {
            id: Some(r.id),
            request_id: None,
            score: r.score,
            cc: r.cc,
            root: r.root,
            ref_: r.ref_,
            to: r.to,
            text: None,
            state: None,
            source: MatchSource::Remote,
            verdict: r.verdict,
            my_vote: None,
        });
    }

    let rank = |s: MatchSource| match s {
        MatchSource::Local => 0,
        MatchSource::Both => 1,
        MatchSource::Remote => 2,
    };
    merged.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| rank(a.source).cmp(&rank(b.source)))
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.request_id.cmp(&b.request_id))
    });
    merged.truncate(top_k);
    merged
}

fn non_empty<'a>(field: &str, s: &'a str) -> Result<&'a str, AppError> {
    let t = s.trim();
    if t.is_empty() {
        Err(AppError::InvalidInput(format!("{field} must not be empty")))
    } else {
        Ok(t)
    }
}

fn normalize_cc(raw: &str) -> Result<String, AppError> {
    let cc = raw.trim().to_ascii_lowercase();
    let valid_chars = cc
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-');
        // 서버(workers/www/src/evaluations.ts의 DOMAIN_RE)보다 느슨하면 서버가 400으로 거부해 큐에서 영구 거부로 버려진다.
    // 라벨은 1~63자, 최상위 도메인(TLD)은 영문 소문자 2~63자만 허용한다. 이 쪽이 더 엄격해도(라벨의 앞뒤 '-' 금지) 괄찮다.
    let labels_ok = cc
        .split('.')
        .all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-'));
    let tld_ok = cc
        .split('.')
        .last()
        .is_some_and(|tld| (2..=63).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_lowercase()));
    if cc.len() > 64 || !cc.contains('.') || !valid_chars || !labels_ok || !tld_ok {
        return Err(AppError::InvalidInput(format!(
            "cc must be a registrable domain: {raw}"
        )));
    }
    Ok(cc)
}

fn normalize_address(field: &str, raw: &str) -> Result<String, AppError> {
    let a = raw.trim().to_ascii_lowercase();
    let ok = a.len() == 42 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        Ok(a)
    } else {
        Err(AppError::InvalidInput(format!("{field} must be an address")))
    }
}

pub(crate) fn validate_uuid(field: &str, raw: &str) -> Result<String, AppError> {
    let ok = raw.len() == 36
        && raw.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    if ok {
        Ok(raw.to_ascii_lowercase())
    } else {
        Err(AppError::InvalidInput(format!("{field} must be a UUID")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Match;
    use crate::embedding::EMBEDDING_DIM;
    use crate::queue::test_support::{ScriptedBackend, Step};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const UUID: &str = "0190f0a0-0000-7000-8000-000000000001";
    const GUEST: &str = "0x00000000000000000000000000000000000000ab";

    struct FakeEmbedder {
        vector: Vec<f32>,
        calls: AtomicUsize,
    }
    impl Embedder for FakeEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, AppError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.vector.clone())
        }
    }

    struct FakeFilter {
        profane: bool,
    }
    impl ProfanityFilter for FakeFilter {
        fn check(&self, _text: &str) -> Result<Verdict, AppError> {
            Ok(if self.profane { Verdict::Profane } else { Verdict::Clean })
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        saved: Mutex<Vec<SaveRequest>>,
        searched: Mutex<Vec<SearchRequest>>,
    }
    #[async_trait::async_trait]
    impl EvalBackend for FakeBackend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            self.saved.lock().unwrap().push(req.clone());
            Ok(SaveResponse {
                id: UUID.into(),
                root: UUID.into(),
                ref_: req.ref_.clone(),
                from: req.from.clone().unwrap_or_else(|| GUEST.into()),
                status: None,
            })
        }
        async fn search(&self, req: &SearchRequest) -> Result<SearchResponse, AppError> {
            self.searched.lock().unwrap().push(req.clone());
            Ok(SearchResponse {
                matches: vec![Match {
                    id: UUID.into(),
                    score: 0.9,
                    cc: None,
                    root: None,
                    ref_: None,
                    to: None,
                    verdict: None,
                    my_vote: None,
                }],
            })
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            Err(AppError::Server {
                status: 404,
                message: "not used in this test".into(),
            })
        }
    }

    fn unit_vector() -> Vec<f32> {
        vec![1.0 / (EMBEDDING_DIM as f32).sqrt(); EMBEDDING_DIM]
    }

    type TestEval = Evaluator<Arc<FakeBackend>, Arc<FakeEmbedder>, Arc<FakeFilter>>;

    fn setup(profane: bool) -> (TestEval, Arc<FakeBackend>, Arc<FakeEmbedder>) {
        let backend = Arc::new(FakeBackend::default());
        let embedder = Arc::new(FakeEmbedder {
            vector: unit_vector(),
            calls: AtomicUsize::new(0),
        });
        let filter = Arc::new(FakeFilter { profane });
        let ev = Evaluator::new(backend.clone(), embedder.clone(), filter);
        (ev, backend, embedder)
    }

    fn input(text: &str) -> SubmitInput {
        SubmitInput {
            text: text.into(),
            cc: "Example.com".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn profane_text_is_blocked_before_embedding_and_saving() {
        let (ev, backend, embedder) = setup(true);
        let out = ev.submit(input("bad")).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Blocked));
        assert_eq!(embedder.calls.load(Ordering::SeqCst), 0);
        assert!(backend.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn clean_text_is_saved_with_vector_text_and_hash() {
        let (ev, backend, _) = setup(false);
        let out = ev.submit(input("좋은 평가")).await.unwrap();
        let SubmitOutcome::Saved(resp) = out else {
            panic!("expected Saved");
        };
        assert_eq!(resp.from, GUEST);

        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].cc, "example.com");
        assert_eq!(saved[0].vector.len(), EMBEDDING_DIM);
        assert_eq!(saved[0].from, None, "from 없으면 서버가 게스트 주소 발급");
        assert_eq!(saved[0].text, "좋은 평가", "원문을 함께 보낸다");
        assert_eq!(saved[0].text_hash, text_hash("좋은 평가"), "해시는 보낸 원문 기준");

        let json = serde_json::to_value(&saved[0]).unwrap();
        assert_eq!(json["text"], "좋은 평가");
        assert_eq!(json["text_hash"], text_hash("좋은 평가"));
    }

    #[tokio::test]
    async fn ref_is_passed_through_for_replies() {
        let (ev, backend, _) = setup(false);
        let mut i = input("답글");
        i.ref_ = Some(UUID.to_uppercase());
        ev.submit(i).await.unwrap();
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].ref_.as_deref(), Some(UUID));
    }

    #[tokio::test]
    async fn empty_text_is_rejected() {
        let (ev, backend, _) = setup(false);
        assert!(ev.submit(input("   ")).await.is_err());
        assert!(backend.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_cc_is_rejected() {
        let (ev, _, _) = setup(false);
        for cc in ["localhost", "example..com", "-bad.com", "exa mple.com", ""] {
            let mut i = input("ok");
            i.cc = cc.into();
            assert!(ev.submit(i).await.is_err(), "should reject cc={cc:?}");
        }
    }

    #[tokio::test]
    async fn invalid_ref_is_rejected() {
        let (ev, _, _) = setup(false);
        let mut i = input("ok");
        i.ref_ = Some("not-a-uuid".into());
        assert!(ev.submit(i).await.is_err());
    }

    #[tokio::test]
    async fn wrong_dimension_from_embedder_is_rejected_without_saving() {
        let backend = Arc::new(FakeBackend::default());
        let embedder = Arc::new(FakeEmbedder {
            vector: vec![1.0; 10],
            calls: AtomicUsize::new(0),
        });
        let ev = Evaluator::new(backend.clone(), embedder, Arc::new(FakeFilter { profane: false }));
        assert!(ev.submit(input("ok")).await.is_err());
        assert!(backend.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_normalizes_filters_and_passes_top_k() {
        let (ev, backend, _) = setup(false);
        let opts = SearchOptions {
            top_k: 5,
            cc: Some("Example.COM".into()),
            root: Some(UUID.into()),
            ..Default::default()
        };
        ev.search("질문", opts).await.unwrap();
        let searched = backend.searched.lock().unwrap();
        assert_eq!(searched[0].top_k, 5);
        assert_eq!(searched[0].cc.as_deref(), Some("example.com"));
        assert_eq!(searched[0].root.as_deref(), Some(UUID));
        assert_eq!(searched[0].vector.len(), EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn search_rejects_top_k_out_of_range() {
        let (ev, backend, _) = setup(false);
        for k in [0u32, TOP_K_MAX + 1] {
            let opts = SearchOptions { top_k: k, ..Default::default() };
            assert!(ev.search("q", opts).await.is_err(), "top_k={k}");
        }
        let opts = SearchOptions { top_k: TOP_K_MAX, ..Default::default() };
        assert!(ev.search("q", opts).await.is_ok());
        assert_eq!(backend.searched.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_rejects_zero_address_to_filter_like_the_server() {
        let (ev, backend, _) = setup(false);
        let zero = "0x0000000000000000000000000000000000000000";
        let opts = SearchOptions { top_k: 3, to: Some(zero.into()), ..Default::default() };
        assert!(matches!(ev.search("q", opts).await, Err(AppError::InvalidInput(_))));
        assert!(backend.searched.lock().unwrap().is_empty(), "서버로 보내지 않는다");

        // 영주소가 아닌 to는 그대로 통과한다
        let ok = SearchOptions { top_k: 3, to: Some(GUEST.into()), ..Default::default() };
        assert!(ev.search("q", ok).await.is_ok());
    }

    #[tokio::test]
    async fn submit_allows_zero_address_to_because_the_server_stores_it_as_null() {
        let (ev, backend, _) = setup(false);
        let mut i = input("수신 주소 없음");
        i.to = Some("0x0000000000000000000000000000000000000000".into());
        assert!(ev.submit(i).await.is_ok());
        assert_eq!(backend.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_does_not_run_profanity_filter() {
        // 스펙: 욕설 필터는 저장 경로에만 적용
        let (ev, backend, _) = setup(true);
        let opts = SearchOptions { top_k: 3, ..Default::default() };
        assert!(ev.search("bad", opts).await.is_ok());
        assert_eq!(backend.searched.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unnormalized_embedder_output_is_saved_as_unit_vector() {
        // 임베더가 정규화되지 않은 값을 주어도 저장되는 벡터는 단위 길이여야 한다
        let backend = Arc::new(FakeBackend::default());
        let embedder = Arc::new(FakeEmbedder {
            vector: vec![2.0; EMBEDDING_DIM],
            calls: AtomicUsize::new(0),
        });
        let ev = Evaluator::new(backend.clone(), embedder, Arc::new(FakeFilter { profane: false }));
        ev.submit(input("정규화 저장")).await.unwrap();
        let saved = backend.saved.lock().unwrap();
        let norm = saved[0].vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm={norm}");
    }

    #[tokio::test]
    async fn unnormalized_embedder_output_is_searched_as_unit_vector() {
        let backend = Arc::new(FakeBackend::default());
        let embedder = Arc::new(FakeEmbedder {
            vector: vec![2.0; EMBEDDING_DIM],
            calls: AtomicUsize::new(0),
        });
        let ev = Evaluator::new(backend.clone(), embedder, Arc::new(FakeFilter { profane: false }));
        let opts = SearchOptions { top_k: 3, ..Default::default() };
        ev.search("질문", opts).await.unwrap();
        let searched = backend.searched.lock().unwrap();
        let norm = searched[0].vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm={norm}");
    }

    #[tokio::test]
    async fn zero_vector_from_embedder_is_rejected_without_saving() {
        let backend = Arc::new(FakeBackend::default());
        let embedder = Arc::new(FakeEmbedder {
            vector: vec![0.0; EMBEDDING_DIM],
            calls: AtomicUsize::new(0),
        });
        let ev = Evaluator::new(backend.clone(), embedder, Arc::new(FakeFilter { profane: false }));
        assert!(ev.submit(input("0벡터")).await.is_err());
        assert!(backend.saved.lock().unwrap().is_empty());
    }

    type QueuedEval = Evaluator<Arc<ScriptedBackend>, Arc<FakeEmbedder>, Arc<FakeFilter>>;

    fn queued_setup(steps: Vec<Step>, profane: bool) -> (QueuedEval, Arc<ScriptedBackend>) {
        let backend = Arc::new(ScriptedBackend::with_steps(steps));
        let embedder = Arc::new(FakeEmbedder {
            vector: unit_vector(),
            calls: AtomicUsize::new(0),
        });
        let ev = Evaluator::new(backend.clone(), embedder, Arc::new(FakeFilter { profane }));
        (ev, backend)
    }

    #[tokio::test]
    async fn submit_queued_saves_immediately_when_online() {
        let (ev, backend) = queued_setup(vec![], false);
        let mut queue = OfflineQueue::default();
        let out = ev.submit_queued(input("좋은 평가"), &mut queue).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Saved(_)));
        assert!(queue.is_empty());
        assert_eq!(backend.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn submit_queued_keeps_item_while_offline_and_flushes_later() {
        let (ev, backend) = queued_setup(vec![Step::Offline], false);
        let mut queue = OfflineQueue::default();
        let out = ev.submit_queued(input("오프라인 평가"), &mut queue).await.unwrap();
        let SubmitOutcome::Queued { local_id } = out else {
            panic!("expected Queued");
        };
        assert_eq!(queue.len(), 1);

        let report = ev.flush_queue(&mut queue).await;
        assert_eq!(report.sent.len(), 1);
        assert_eq!(report.sent[0].local_id, local_id);
        assert!(queue.is_empty());
        assert_eq!(backend.saved.lock().unwrap().len(), 2, "1 offline attempt + 1 flush");
    }

    #[tokio::test]
    async fn reply_to_queued_parent_is_sent_after_it_with_server_id() {
        let (ev, backend) = queued_setup(vec![Step::Offline], false);
        let mut queue = OfflineQueue::default();
        let SubmitOutcome::Queued { local_id: root } =
            ev.submit_queued(input("루트"), &mut queue).await.unwrap()
        else {
            panic!("expected Queued");
        };

        let mut reply = input("답글");
        reply.local_parent = Some(root);
        let out = ev.submit_queued(reply, &mut queue).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Saved(_)));
        assert!(queue.is_empty());

        // 기록: 루트 오프라인 시도, 루트 전송, 답글 전송 (ScriptedBackend의 첫 서버 id는 srv-1)
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved.len(), 3);
        assert_eq!(saved[1].ref_, None);
        assert_eq!(saved[2].ref_.as_deref(), Some("srv-1"));
    }

    #[tokio::test]
    async fn profane_text_is_blocked_and_never_queued() {
        let (ev, backend) = queued_setup(vec![Step::Offline], true);
        let mut queue = OfflineQueue::default();
        let out = ev.submit_queued(input("bad"), &mut queue).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Blocked));
        assert!(queue.is_empty());
        assert!(backend.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn queued_save_is_stored_once_when_the_response_is_lost() {
        // 서버는 저장했지만 응답이 유실된 경우: 재시도해도 평가는 한 번만 저장된다
        let (ev, backend) = queued_setup(vec![Step::AcceptButLose], false);
        let mut queue = OfflineQueue::default();
        let out = ev.submit_queued(input("응답 유실"), &mut queue).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Queued { .. }));
        let report = ev.flush_queue(&mut queue).await;
        assert_eq!(report.sent.len(), 1);
        assert!(queue.is_empty());
        assert_eq!(backend.stored_count(), 1);
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].request_id, saved[1].request_id);
        assert_eq!(saved[0].request_id.len(), 32);
    }

    #[tokio::test]
    async fn plain_submit_rejects_local_parent_and_ref_together() {
        let (ev, backend) = queued_setup(vec![], false);

        let mut local = input("답글");
        local.local_parent = Some("local-1".into());
        assert!(matches!(ev.submit(local).await, Err(AppError::InvalidInput(_))));

        let mut both = input("답글");
        both.ref_ = Some(UUID.into());
        both.local_parent = Some("local-1".into());
        assert!(matches!(ev.submit_queued(both, &mut OfflineQueue::default()).await, Err(AppError::InvalidInput(_))));

        assert!(backend.saved.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;
    use crate::api::{SearchRequest, SearchResponse, StatusResponse};
    use crate::embedding::EMBEDDING_DIM;

    /// 서버 응답의 status를 정해진 값으로 돌려주는 가짜 백엔드
    struct StatusBackend {
        status: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl EvalBackend for StatusBackend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            Ok(SaveResponse {
                id: "srv-1".into(),
                root: "srv-1".into(),
                ref_: req.ref_.clone(),
                from: "0x00000000000000000000000000000000000000ab".into(),
                status: self.status.map(String::from),
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            Ok(StatusResponse {
                id: "srv-1".into(),
                root: "srv-1".into(),
                ref_: None,
                status: "stored".into(),
                reason: None,
            })
        }
    }

    struct UnitEmbedder;
    impl Embedder for UnitEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, AppError> {
            Ok(vec![1.0 / (EMBEDDING_DIM as f32).sqrt(); EMBEDDING_DIM])
        }
    }

    struct PassFilter;
    impl ProfanityFilter for PassFilter {
        fn check(&self, _text: &str) -> Result<Verdict, AppError> {
            Ok(Verdict::Clean)
        }
    }

    fn input(text: &str) -> SubmitInput {
        SubmitInput {
            text: text.into(),
            cc: "example.com".into(),
            ..Default::default()
        }
    }

    fn evaluator(status: Option<&'static str>) -> Evaluator<StatusBackend, UnitEmbedder, PassFilter> {
        Evaluator::new(StatusBackend { status }, UnitEmbedder, PassFilter)
    }

    #[tokio::test]
    async fn pending_202_is_accepted_not_saved() {
        let out = evaluator(Some("pending")).submit(input("대기")).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Accepted(ref r) if r.id == "srv-1"), "{out:?}");
    }

    #[tokio::test]
    async fn stored_or_missing_status_is_saved() {
        let out = evaluator(Some("stored")).submit(input("저장")).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Saved(_)));
        let out = evaluator(None).submit(input("이전 서버")).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Saved(_)));
    }

    #[tokio::test]
    async fn rejected_status_is_an_error() {
        let err = evaluator(Some("rejected")).submit(input("거부")).await.unwrap_err();
        assert!(matches!(err, AppError::Rejected(_)), "{err:?}");
    }

    #[tokio::test]
    async fn text_over_the_limit_is_rejected_before_embedding() {
        // 가 = UTF-8 3바이트. 3000자면 9000바이트로 상한(8192)을 넘는다
        let long = "가".repeat(3000);
        let err = evaluator(Some("stored")).submit(input(&long)).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)), "{err:?}");
    }

    #[tokio::test]
    async fn status_of_passes_through_to_the_backend() {
        let st = evaluator(None).status_of("req-x").await.unwrap();
        assert_eq!(st.status, "stored");
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::*;
    use std::sync::Arc;
    use crate::api::{SearchRequest, SearchResponse, StatusResponse};
    use crate::embedding::EMBEDDING_DIM;
    use std::sync::Mutex;

    /// 임베더가 받은 문자열을 모두 기록하는 가짜 임베더
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<String>>,
    }
    impl Embedder for Recording {
        fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
            self.seen.lock().unwrap().push(text.to_string());
            Ok(vec![1.0 / (EMBEDDING_DIM as f32).sqrt(); EMBEDDING_DIM])
        }
    }

    #[derive(Default)]
    struct Backend {
        saved: Mutex<Vec<SaveRequest>>,
    }
    #[async_trait::async_trait]
    impl EvalBackend for Backend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            self.saved.lock().unwrap().push(req.clone());
            Ok(SaveResponse {
                id: "x".into(),
                root: "x".into(),
                ref_: None,
                from: "0x00000000000000000000000000000000000000ab".into(),
                status: None,
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            Err(AppError::Server { status: 404, message: "unused".into() })
        }
    }

    struct Pass;
    impl ProfanityFilter for Pass {
        fn check(&self, _text: &str) -> Result<Verdict, AppError> {
            Ok(Verdict::Clean)
        }
    }

    #[tokio::test]
    async fn saved_text_is_raw_but_embedder_sees_document_prompt() {
        let backend = Arc::new(Backend::default());
        let rec = Arc::new(Recording::default());
        let ev = Evaluator::new(backend.clone(), rec.clone(), Pass);
        ev.submit(SubmitInput {
            text: "좋은 평가".into(),
            cc: "example.com".into(),
            ..Default::default()
        })
        .await
        .unwrap();

        assert_eq!(rec.seen.lock().unwrap()[0], "title: none | text: 좋은 평가");
        let saved = backend.saved.lock().unwrap();
        assert_eq!(saved[0].text, "좋은 평가", "서버로 보내는 원문에는 접두어가 없다");
        assert_eq!(saved[0].text_hash, text_hash("좋은 평가"));
    }

    #[tokio::test]
    async fn search_query_gets_query_prompt() {
        let backend = Arc::new(Backend::default());
        let rec = Arc::new(Recording::default());
        let ev = Evaluator::new(backend.clone(), rec.clone(), Pass);
        ev.search("질문", SearchOptions { top_k: 3, ..Default::default() })
            .await
            .unwrap();
        assert_eq!(rec.seen.lock().unwrap()[0], "task: search result | query: 질문");
    }
}

#[cfg(test)]
mod column_rule_tests {
    //! 서버(workers/www/src/evaluations.ts)의 검증 규칙과 앱이 어긋나지 않는지 확인한다.
    //! 앱이 더 느슨하면 서버가 400으로 거부하고, 큐는 그 항목을 영구 거부로 버린다.
    use super::*;

    #[test]
    fn cc_accepts_what_the_server_accepts() {
        for ok in ["example.com", "Example.COM", "a-b.example.co.kr", "x.io", "sub.domain.example.org"] {
            assert!(normalize_cc(ok).is_ok(), "should accept {ok}");
        }
        assert_eq!(normalize_cc("  Example.COM ").unwrap(), "example.com");
    }

    #[test]
    fn cc_rejects_what_the_server_rejects() {
        // 서버 DOMAIN_RE: /^([a-z0-9-]{1,63}\.)+[a-z]{2,63}$/ 와 최대 64자
        let long_label = format!("{}.com", "a".repeat(64));
        let too_long = format!("{}.com", "a.".repeat(32));
        for bad in [
            "example.123",   // TLD에 숫자
            "example.c0m",   // TLD에 숫자
            "example.x",     // TLD 1자
            "example.co-m",  // TLD에 하이픈
            "localhost",
            "example..com",
            "-bad.com",
            "bad-.com",
            "exa mple.com",
            "",
            long_label.as_str(),
            too_long.as_str(),
        ] {
            assert!(normalize_cc(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn cc_with_a_63_char_label_is_accepted_and_64_is_not() {
        assert!(normalize_cc(&format!("{}.com", "a".repeat(63))).is_err(), "전체가 64자를 넘는다");
        assert!(normalize_cc(&format!("{}.io", "a".repeat(58))).is_ok());
    }

    #[test]
    fn address_is_lowercased_and_must_be_42_hex_chars() {
        let mixed = "0xAbCdEf0123456789aBcDeF0123456789abcdef01";
        assert_eq!(normalize_address("from", mixed).unwrap(), mixed.to_ascii_lowercase());
        for bad in ["0x123", "abcdef0123456789abcdef0123456789abcdef01", "0xzz0000000000000000000000000000000000000000", ""] {
            assert!(normalize_address("to", bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn uuid_is_lowercased() {
        let up = "0190F0A0-0000-7000-8000-000000000001";
        assert_eq!(validate_uuid("ref", up).unwrap(), up.to_ascii_lowercase());
        assert!(validate_uuid("ref", "not-a-uuid").is_err());
    }
}

#[cfg(test)]
mod outbox_integration_tests {
    //! Evaluator와 Outbox(LanceDB)를 함께 쓰는 흐름
    use std::sync::Arc;

    use super::*;
    use crate::embedding::EMBEDDING_DIM;
    use crate::local::test_support::temp_dir;
    use crate::local::LocalState;
    use crate::queue::test_support::{ScriptedBackend, Step};

    struct UnitEmbedder;
    impl Embedder for UnitEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, AppError> {
            Ok(vec![1.0 / (EMBEDDING_DIM as f32).sqrt(); EMBEDDING_DIM])
        }
    }

    struct Gate {
        profane: bool,
    }
    impl ProfanityFilter for Gate {
        fn check(&self, _text: &str) -> Result<Verdict, AppError> {
            Ok(if self.profane { Verdict::Profane } else { Verdict::Clean })
        }
    }

    fn evaluator(steps: Vec<Step>, profane: bool) -> (Evaluator<Arc<ScriptedBackend>, UnitEmbedder, Gate>, Arc<ScriptedBackend>) {
        let backend = Arc::new(ScriptedBackend::with_steps(steps));
        (Evaluator::new(backend.clone(), UnitEmbedder, Gate { profane }), backend)
    }

    fn input(text: &str) -> SubmitInput {
        SubmitInput { text: text.into(), cc: "Example.com".into(), ..Default::default() }
    }

    #[tokio::test]
    async fn submit_outbox_keeps_text_and_vector_locally_and_marks_the_row_stored() {
        let dir = temp_dir("ev-store");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let (ev, backend) = evaluator(vec![], false);

        let out = ev.submit_outbox(input("좋은 평가"), &mut ob).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Saved(_)));
        assert_eq!(backend.saved.lock().unwrap().len(), 1);

        let rows = ob.db().fetch(None, true, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!((r.state, r.text.as_str(), r.cc.as_str()), (LocalState::Stored, "좋은 평가", "example.com"));
        assert_eq!(r.vector.as_ref().map(|v| v.len()), Some(EMBEDDING_DIM));
        assert_eq!(r.id.as_deref(), Some("srv-1"));
        assert_eq!(r.text_hash, text_hash("좋은 평가"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn profane_input_leaves_no_trace_locally() {
        let dir = temp_dir("ev-profane");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let (ev, backend) = evaluator(vec![], true);
        let out = ev.submit_outbox(input("bad"), &mut ob).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Blocked));
        assert_eq!(ob.db().count().await.unwrap(), 0);
        assert!(backend.saved.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn offline_submit_survives_a_restart_and_is_sent_later() {
        let dir = temp_dir("ev-offline");
        {
            let mut ob = Outbox::open(&dir).await.unwrap();
            let (ev, _) = evaluator(vec![Step::Offline], false);
            let out = ev.submit_outbox(input("오프라인에서 쓴 평가"), &mut ob).await.unwrap();
            assert!(matches!(out, SubmitOutcome::Queued { .. }));
        } // 앱 종료

        let mut ob = Outbox::open(&dir).await.unwrap();
        assert_eq!(ob.queued(), 1);
        let (ev, backend) = evaluator(vec![], false);
        let report = ev.flush_outbox(&mut ob).await;
        assert_eq!(report.sent.len(), 1);
        assert_eq!(backend.saved.lock().unwrap()[0].text, "오프라인에서 쓴 평가");
        let row = ob.db().fetch(None, false, None).await.unwrap().remove(0);
        assert_eq!(row.state, LocalState::Stored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_local_returns_my_text_with_a_score() {
        let dir = temp_dir("ev-search");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let (ev, _) = evaluator(vec![], false);
        ev.submit_outbox(input("검색될 내 글"), &mut ob).await.unwrap();

        let hits = ev.search_local("내 글 찾기", 5, None, &ob).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "검색될 내 글");
        assert!(hits[0].score > 0.99, "같은 방향의 벡터: {}", hits[0].score);

        assert!(ev.search_local("q", 5, Some("example.com"), &ob).await.unwrap().len() == 1);
        assert!(ev.search_local("q", 5, Some("other.org"), &ob).await.unwrap().is_empty(), "cc 필터");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn search_local_validates_like_the_server_search() {
        let dir = temp_dir("ev-search-validate");
        let ob = Outbox::open(&dir).await.unwrap();
        let (ev, _) = evaluator(vec![], false);
        for k in [0u32, TOP_K_MAX + 1] {
            assert!(matches!(ev.search_local("q", k, None, &ob).await, Err(AppError::InvalidInput(_))), "top_k={k}");
        }
        assert!(ev.search_local("   ", 5, None, &ob).await.is_err());
        assert!(ev.search_local("q", 5, Some("localhost"), &ob).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn sync_outbox_status_updates_pending_rows() {
        use crate::api::{SaveResponse, SearchRequest, SearchResponse, StatusResponse};

        struct AcceptThenStored;
        #[async_trait::async_trait]
        impl EvalBackend for AcceptThenStored {
            async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
                Ok(SaveResponse {
                    id: "0190f0a0-0000-7000-8000-000000000001".into(),
                    root: "0190f0a0-0000-7000-8000-000000000001".into(),
                    ref_: req.ref_.clone(),
                    from: "0x00000000000000000000000000000000000000ab".into(),
                    status: Some("pending".into()),
                })
            }
            async fn search(&self, _r: &SearchRequest) -> Result<SearchResponse, AppError> {
                Ok(SearchResponse { matches: vec![] })
            }
            async fn status(&self, _id: &str) -> Result<StatusResponse, AppError> {
                Ok(StatusResponse {
                    id: "0190f0a0-0000-7000-8000-000000000001".into(),
                    root: "0190f0a0-0000-7000-8000-000000000001".into(),
                    ref_: None,
                    status: "stored".into(),
                    reason: None,
                })
            }
        }

        let dir = temp_dir("ev-sync");
        let mut ob = Outbox::open(&dir).await.unwrap();
        let ev = Evaluator::new(AcceptThenStored, UnitEmbedder, Gate { profane: false });
        let out = ev.submit_outbox(input("접수 후 저장"), &mut ob).await.unwrap();
        assert!(matches!(out, SubmitOutcome::Accepted(_)));
        assert_eq!(ob.db().fetch(None, false, None).await.unwrap()[0].state, LocalState::Pending);

        let report = ev.sync_outbox_status(&mut ob).await;
        assert_eq!(report.stored.len(), 1);
        assert_eq!(ob.db().fetch(None, false, None).await.unwrap()[0].state, LocalState::Stored);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod budget_bcc_and_merge_tests {
    //! 토큰 예산 사전 검사, bcc 연결, 로컬·서버 검색 결과 병합
    use super::*;
    use crate::embedding::EMBEDDING_DIM;
    use crate::embeddinggemma::MAX_INPUT_TOKENS;
    use crate::local::LocalState;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// 토큰 수를 정해 둔 값으로 알려 주는 임베더. embed가 불렸는지 센다.
    struct Counted {
        tokens: Option<usize>,
        embeds: AtomicUsize,
    }
    impl Embedder for Counted {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, AppError> {
            self.embeds.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1.0 / (EMBEDDING_DIM as f32).sqrt(); EMBEDDING_DIM])
        }
        fn token_count(&self, _text: &str) -> Result<Option<usize>, AppError> {
            Ok(self.tokens)
        }
    }

    struct CountingFilter(AtomicUsize);
    impl ProfanityFilter for CountingFilter {
        fn check(&self, _text: &str) -> Result<Verdict, AppError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Verdict::Clean)
        }
    }

    #[derive(Default)]
    struct Backend {
        saved: Mutex<Vec<SaveRequest>>,
        searched: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl EvalBackend for Backend {
        async fn save(&self, req: &SaveRequest) -> Result<SaveResponse, AppError> {
            self.saved.lock().unwrap().push(req.clone());
            Ok(SaveResponse {
                id: "0190f0a0-0000-7000-8000-000000000001".into(),
                root: "0190f0a0-0000-7000-8000-000000000001".into(),
                ref_: None,
                from: "0x00000000000000000000000000000000000000ab".into(),
                status: None,
            })
        }
        async fn search(&self, _req: &SearchRequest) -> Result<SearchResponse, AppError> {
            self.searched.fetch_add(1, Ordering::SeqCst);
            Ok(SearchResponse { matches: vec![] })
        }
        async fn status(&self, _request_id: &str) -> Result<StatusResponse, AppError> {
            Err(AppError::Offline("unused".into()))
        }
    }

    type Eval = Evaluator<Arc<Backend>, Arc<Counted>, Arc<CountingFilter>>;
    fn setup(tokens: Option<usize>) -> (Eval, Arc<Backend>, Arc<Counted>, Arc<CountingFilter>) {
        let backend = Arc::new(Backend::default());
        let embedder = Arc::new(Counted { tokens, embeds: AtomicUsize::new(0) });
        let filter = Arc::new(CountingFilter(AtomicUsize::new(0)));
        (Evaluator::new(backend.clone(), embedder.clone(), filter.clone()), backend, embedder, filter)
    }

    fn input(text: &str) -> SubmitInput {
        SubmitInput { text: text.into(), cc: "example.com".into(), ..Default::default() }
    }

    // ---- 토큰 예산 ----

    #[tokio::test]
    async fn over_budget_text_is_rejected_before_the_filter_the_model_and_the_server() {
        let (ev, backend, embedder, filter) = setup(Some(MAX_INPUT_TOKENS + 1));
        let err = ev.submit(input("너무 긴 입력")).await.unwrap_err();
        assert!(matches!(&err, AppError::InvalidInput(m) if m.contains("limit is 8192")), "{err:?}");
        assert_eq!(embedder.embeds.load(Ordering::SeqCst), 0, "모델 추론 전에 막는다");
        assert_eq!(filter.0.load(Ordering::SeqCst), 0, "필터 전에 막는다");
        assert!(backend.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn text_exactly_at_the_budget_is_accepted() {
        let (ev, backend, embedder, _) = setup(Some(MAX_INPUT_TOKENS));
        assert!(matches!(ev.submit(input("딱 한도")).await.unwrap(), SubmitOutcome::Saved(_)));
        assert_eq!(embedder.embeds.load(Ordering::SeqCst), 1);
        assert_eq!(backend.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_embedder_that_cannot_count_tokens_skips_the_precheck() {
        let (ev, _, embedder, _) = setup(None);
        assert!(ev.submit(input("세지 못함")).await.is_ok());
        assert_eq!(embedder.embeds.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn over_budget_search_query_is_rejected_before_the_model_and_the_server() {
        let (ev, backend, embedder, _) = setup(Some(MAX_INPUT_TOKENS + 1));
        let err = ev.search("긴 질의", SearchOptions { top_k: 5, ..Default::default() }).await.unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)), "{err:?}");
        assert_eq!(embedder.embeds.load(Ordering::SeqCst), 0);
        assert_eq!(backend.searched.load(Ordering::SeqCst), 0);
    }

    // ---- bcc ----

    #[tokio::test]
    async fn bcc_is_normalized_and_sent_with_the_request() {
        let (ev, backend, _, _) = setup(None);
        let mut i = input("bcc 전달");
        i.bcc = Some("0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD".into());
        ev.submit(i).await.unwrap();
        let sent = backend.saved.lock().unwrap()[0].clone();
        assert_eq!(sent.bcc.as_deref(), Some("0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"));
        assert_eq!(serde_json::to_value(&sent).unwrap()["bcc"], "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd");
    }

    #[tokio::test]
    async fn bcc_is_omitted_from_the_json_when_absent_and_rejected_when_malformed() {
        let (ev, backend, _, _) = setup(None);
        ev.submit(input("bcc 없음")).await.unwrap();
        let sent = backend.saved.lock().unwrap()[0].clone();
        assert!(serde_json::to_value(&sent).unwrap().get("bcc").is_none());

        for bad in ["0x12", "abcdefabcdefabcdefabcdefabcdefabcdefabcd", "0xZZcdefabcdefabcdefabcdefabcdefabcdefabcd", "x"] {
            let mut i = input("나쁜 bcc");
            i.bcc = Some(bad.into());
            let err = ev.submit(i).await.unwrap_err();
            assert!(matches!(&err, AppError::InvalidInput(m) if m.contains("bcc")), "{bad}: {err:?}");
        }
        assert_eq!(backend.saved.lock().unwrap().len(), 1, "나쁜 bcc는 서버로 가지 않았다");
    }

    // ---- 병합 ----

    fn local(request_id: &str, id: Option<&str>, score: f32) -> LocalMatch {
        LocalMatch {
            request_id: request_id.into(),
            local_id: format!("local-{request_id}"),
            id: id.map(String::from),
            cc: "example.com".into(),
            state: if id.is_some() { LocalState::Stored } else { LocalState::Queued },
            text: format!("원문 {request_id}"),
            score,
        }
    }
    fn remote(id: &str, score: f32) -> Match {
        Match { id: id.into(), score, cc: Some("example.com".into()), root: Some("root".into()), ref_: None, to: None, verdict: None, my_vote: None }
    }

    #[test]
    fn merge_joins_the_same_post_by_server_id_and_keeps_text_from_local() {
        let merged = merge_matches(vec![local("r1", Some("a"), 0.90)], vec![remote("a", 0.95), remote("b", 0.5)], 10);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].source, MatchSource::Both);
        assert_eq!(merged[0].id.as_deref(), Some("a"));
        assert_eq!(merged[0].score, 0.95, "점수는 큰 쪽");
        assert_eq!(merged[0].text.as_deref(), Some("원문 r1"));
        assert_eq!(merged[0].root.as_deref(), Some("root"));
        assert_eq!(merged[0].state.as_deref(), Some("stored"));
        assert_eq!(merged[1].source, MatchSource::Remote);
        assert_eq!(merged[1].text, None);
    }

    #[test]
    fn posts_without_a_server_id_stay_separate_and_are_labelled_local() {
        let merged = merge_matches(vec![local("r1", None, 0.7), local("r2", None, 0.8)], vec![remote("a", 0.75)], 10);
        let order: Vec<_> = merged.iter().map(|m| (m.source, m.request_id.clone().or(m.id.clone()).unwrap())).collect();
        assert_eq!(
            order,
            vec![(MatchSource::Local, "r2".into()), (MatchSource::Remote, "a".into()), (MatchSource::Local, "r1".into())]
        );
        assert_eq!(merged[0].state.as_deref(), Some("queued"));
    }

    #[test]
    fn merge_truncates_to_top_k_and_orders_ties_locals_first() {
        let merged = merge_matches(vec![local("r1", Some("z"), 0.5)], vec![remote("a", 0.5), remote("b", 0.9), remote("c", 0.1)], 2);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id.as_deref(), Some("b"));
        assert_eq!(merged[1].id.as_deref(), Some("z"), "점수가 같으면 로컬이 먼저");
    }

    #[test]
    fn merge_handles_empty_sides_and_nan_scores_without_panicking() {
        assert!(merge_matches(vec![], vec![], 5).is_empty());
        assert_eq!(merge_matches(vec![local("r1", None, 0.1)], vec![], 5).len(), 1);
        assert_eq!(merge_matches(vec![], vec![remote("a", 0.1)], 5).len(), 1);
        let merged = merge_matches(vec![local("r1", None, f32::NAN)], vec![remote("a", 0.4)], 5);
        assert_eq!(merged.len(), 2);
    }
}
