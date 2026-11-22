//! items 초안 만들기 (지시서 Stage 3 로컬 스케줄러): 쇼핑 페이지 글 → {head, body, link, type, digest}.
//!
//! Qwen3.5-0.8B(qwen35.rs)로 필드를 하나씩 채운다. 모델이 JSON 전체를 자유롭게 쓰게 두지 않고 틀을 앱이 쓴다:
//! `{"type": ` 뒤에서는 "1"(premium)과 "2"(discount) 두 토큰 중 로짓이 큰 쪽만 고르고, `"head": "`와 `"body": "` 뒤에서는
//! 닫는 따옴표가 나올 때까지만 생성한다. 그래서 결과는 언제나 올바른 JSON이고 type은 1 또는 2다.
//! - link: 입력 그대로. digest: 제목(head) UTF-8의 SHA-256 16진수 (www의 items 주석 "Digest(title)". 계산 규칙이 저장소에
//!   정의되어 있지 않아 이 저장소의 text_hash와 같은 SHA-256으로 정했다. 서버와 맞춰야 한다)
//! - items 테이블(type 0 초안, 1 premium, 2 discount, data {head, body, link})의 스키마는 바꾸지 않는다.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::qwen35::{chat_prompt, Head, Qwen35, State, VocabPlan};

/// 페이지 글은 이만큼(토큰)까지만 본다 (프롬프트가 길면 앞부분을 쓴다)
pub const MAX_PAGE_TOKENS: usize = 1500;
const MAX_HEAD_TOKENS: usize = 40;
const MAX_BODY_TOKENS: usize = 120;

pub const SYSTEM: &str = "You read a shopping web page and write a JSON draft. \
type is 1 if the page sells premium goods (luxury, high quality, limited, handmade), \
2 if it is mainly a discount (sale, coupon, low price). \
head is a short title in the page's language. body is a one or two sentence summary in the page's language.";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemDraft {
    /// 1: premium, 2: discount
    #[serde(rename = "type")]
    pub kind: u8,
    pub head: String,
    pub body: String,
    pub link: String,
    pub digest: String,
}

/// 제목의 다이제스트 (SHA-256 16진수)
pub fn digest(head: &str) -> String {
    Sha256::digest(head.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn err(e: candle_core::Error) -> AppError {
    AppError::Embed(format!("초안 생성 실패: {e}"))
}

/// JSON 문자열 안의 글을 따옴표가 나올 때까지 생성한다. 이스케이프(\")는 글로 본다.
/// (글, 따옴표 뒤에 같은 토큰으로 이미 나온 글자)를 돌려준다.
/// `advance_on_end == false`이면 마지막 토큰(닫는 따옴표 또는 `max` 도달) 뒤에 불필요한 `model.forward`를 생략한다.
fn generate_string(
    model: &Qwen35,
    head: &Head,
    st: &mut State,
    h: &mut candle_core::Tensor,
    max: usize,
    advance_on_end: bool,
) -> Result<(String, String), AppError> {
    let mut ids: Vec<u32> = Vec::with_capacity(max);
    for step in 0..max {
        let id = model.next_token_id(h, head, None).map_err(err)?;
        ids.push(id);
        // ASCII '"' (0x22) never appears inside multi-byte UTF-8 continuation bytes, so we only run
        // full sequence decode when the newest token piece contains '"' (avoiding O(N^2) decoding).
        let piece = model.decode(&[id]);
        if piece.contains('"') {
            let text = model.decode(&ids);
            if let Some(end) = closing_quote(&text) {
                if advance_on_end {
                    *h = model.forward(&[id], st).map_err(err)?;
                }
                return Ok((unescape(&text[..end]), text[end + 1..].to_string()));
            }
        }
        if advance_on_end || step + 1 < max {
            *h = model.forward(&[id], st).map_err(err)?;
        }
    }
    let text = model.decode(&ids);
    Ok((unescape(&text), String::new()))
}

/// 다음 필드의 틀 중 모델이 이미 쓴 앞부분을 뺀 나머지 (예: 모델이 `",`까지 썼으면 ` "body": "`만 넣는다)
fn remaining<'a>(template: &'a str, already: &str) -> &'a str {
    if !already.is_empty() && template.starts_with(already) {
        &template[already.len()..]
    } else {
        template
    }
}

/// 이스케이프되지 않은 첫 따옴표의 위치
fn closing_quote(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i),
            _ => escaped = false,
        }
    }
    None
}

fn unescape(s: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{s}\"")).unwrap_or_else(|_| s.replace("\\\"", "\"").replace("\\n", "\n"))
}

/// 잘라 낸 출력 헤드로 초안을 만든다: 고정 어휘에 이 페이지의 토큰을 더한 16,384개 (요청마다 행 1~2만 개를 매핑에서 읽는다)
pub fn draft_trimmed(model: &Qwen35, plan: &VocabPlan, page: &str, link: &str) -> Result<ItemDraft, AppError> {
    let raw_page_ids = model.encode(page);
    let truncated_page = if raw_page_ids.len() > MAX_PAGE_TOKENS {
        model.decode(&raw_page_ids[..MAX_PAGE_TOKENS])
    } else {
        page.to_string()
    };
    let mut vocab_ids = raw_page_ids;
    vocab_ids.truncate(MAX_PAGE_TOKENS);
    vocab_ids.extend(model.encode(link));
    let head = model.trimmed_head(plan.ids_for(&vocab_ids)).map_err(err)?;
    draft_preprocessed(model, &head, &truncated_page, link)
}

/// 페이지 글과 링크로 초안을 만든다
pub fn draft(model: &Qwen35, head: &Head, page: &str, link: &str) -> Result<ItemDraft, AppError> {
    let page_ids = model.encode(page);
    let page = if page_ids.len() > MAX_PAGE_TOKENS {
        model.decode(&page_ids[..MAX_PAGE_TOKENS])
    } else {
        page.to_string()
    };
    draft_preprocessed(model, head, &page, link)
}

fn draft_preprocessed(model: &Qwen35, head: &Head, page: &str, link: &str) -> Result<ItemDraft, AppError> {
    let prompt = chat_prompt(SYSTEM, &format!("{page}\n\nlink: {link}"));
    let mut ids = model.encode(&prompt);
    ids.extend(model.encode("{\"type\": "));
    let mut st = model.new_state_with_capacity(ids.len() + MAX_HEAD_TOKENS + MAX_BODY_TOKENS + 32);
    let mut h = model.forward(&ids, &mut st).map_err(err)?;

    // type: "1"과 "2" 중에서만 고른다
    let one = *model.encode("1").first().ok_or_else(|| AppError::Embed("토큰 1".into()))?;
    let two = *model.encode("2").first().ok_or_else(|| AppError::Embed("토큰 2".into()))?;
    let choice = model.next_token_id(&h, head, Some(&[one, two])).map_err(err)?;
    let kind = if choice == one { 1 } else { 2 };

    h = model.forward(&[&[choice][..], &model.encode(", \"head\": \"")].concat(), &mut st).map_err(err)?;
    let (title, rest) = generate_string(model, head, &mut st, &mut h, MAX_HEAD_TOKENS, true)?;
    h = model.forward(&model.encode(remaining(", \"body\": \"", &rest)), &mut st).map_err(err)?;
    let (body, _) = generate_string(model, head, &mut st, &mut h, MAX_BODY_TOKENS, false)?;
    crate::utils::trim_idle_gpu_pool(model.device());
    Ok(ItemDraft { kind, digest: digest(title.trim()), head: title.trim().to_string(), body: body.trim().to_string(), link: link.to_string() })
}

/// 출력 어휘를 고를 때 넣는 JSON 틀과 도메인 문장
pub fn corpus() -> Vec<&'static str> {
    vec![
        "{\"type\": 1, \"head\": \"\", \"body\": \"\", \"link\": \"https://\", \"digest\": \"\"}",
        "{\"type\": 2, \"head\": \"\\\"\", \"body\": \"\\n\"}",
        "할인 쿠폰 무료 배송 특가 최저가 세일 이벤트 프리미엄 명품 한정판 수제 정품 리뷰 가격 구매 주문 배송비 적립금 회원 혜택 오늘 마감 신상품 인기 추천",
        "premium luxury limited edition handmade genuine leather free shipping discount coupon sale deal price off clearance warranty",
        "겨울 패딩 최대 70% 할인! 오늘 자정까지 쿠폰 적용 시 추가 10% 할인됩니다. 무료 배송.",
        "Hand-stitched Italian leather briefcase, full-grain calfskin, lifetime warranty. Limited edition of 200.",
    ]
}

/// 셸이 가진 초안 엔진: 첫 요청 때 모델을 올리고(약 2초) 그 뒤로는 그대로 쓴다. 모델이 없으면 not_ready
pub struct DraftEngine {
    dir: std::path::PathBuf,
    bits: Option<crate::quant::Bits>,
    slot: tokio::sync::Mutex<Option<std::sync::Arc<(Qwen35, VocabPlan)>>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct DraftArgs {
    pub text: String,
    #[serde(default)]
    pub link: String,
}

impl DraftEngine {
    pub fn new(dir: impl Into<std::path::PathBuf>, bits: Option<crate::quant::Bits>) -> Self {
        Self { dir: dir.into(), bits, slot: tokio::sync::Mutex::new(None) }
    }

    pub fn loaded(&self) -> bool {
        self.slot.try_lock().map(|s| s.is_some()).unwrap_or(true)
    }

    /// Drops the cached Qwen3.5 draft model so its memory/GPU buffers are freed before shutdown.
    pub async fn release(&self) -> bool {
        self.slot.lock().await.take().is_some()
    }

    /// Best-effort non-async release for synchronous shutdown hooks (`tauri::RunEvent::Exit`).
    pub fn try_release(&self) -> bool {
        self.slot.try_lock().map(|mut slot| slot.take().is_some()).unwrap_or(false)
    }

    pub async fn run(&self, args: DraftArgs) -> Result<ItemDraft, crate::commands::CommandError> {
        use crate::commands::CommandError;
        if args.text.trim().is_empty() {
            return Err(CommandError::new("invalid_input", "페이지 글이 비었습니다"));
        }
        let engine = {
            let mut slot = self.slot.lock().await;
            match &*slot {
                Some(e) => e.clone(),
                None => {
                    let (dir, bits) = (self.dir.clone(), self.bits);
                    let loaded = tokio::task::spawn_blocking(move || -> Result<(Qwen35, VocabPlan), String> {
                        let model = Qwen35::load(&dir, bits).map_err(|e| e.to_string())?;
                        let plan = crate::qwen35::plan_vocab(&model.tokenizer, &corpus(), 16_384)?;
                        Ok((model, plan))
                    })
                    .await
                    .map_err(|e| CommandError::new("internal", e.to_string()))?
                    .map_err(|e| CommandError::new("not_ready", format!("초안 모델을 열지 못했습니다: {e}")))?;
                    let e = std::sync::Arc::new(loaded);
                    *slot = Some(e.clone());
                    e
                }
            }
        };
        tokio::task::spawn_blocking(move || draft_trimmed(&engine.0, &engine.1, &args.text, &args.link))
            .await
            .map_err(|e| CommandError::new("internal", e.to_string()))?
            .map_err(CommandError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_quotes_skip_escaped_ones_and_strings_unescape() {
        assert_eq!(closing_quote(r#"겨울 패딩 할인", "body"#), Some("겨울 패딩 할인".len()));
        assert_eq!(closing_quote(r#"say \"hi\" now" x"#), Some(r#"say \"hi\" now"#.len()));
        assert_eq!(closing_quote("no quote yet"), None);
        assert_eq!(unescape(r#"say \"hi\"\nok"#), "say \"hi\"\nok");
        assert_eq!(digest("겨울 패딩 할인").len(), 64);
        assert_eq!(digest("a"), "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb");
        assert_eq!(remaining(", \"body\": \"", ","), " \"body\": \"");
        assert_eq!(remaining(", \"body\": \"", ""), ", \"body\": \"");
        assert_eq!(remaining(", \"body\": \"", "}"), ", \"body\": \"", "틀과 다르면 틀 전체를 넣는다");
    }
}
