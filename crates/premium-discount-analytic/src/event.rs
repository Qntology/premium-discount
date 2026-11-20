//! 수집 사건 (`web-event/1`): 사이트에 심은 수집 스크립트(`web/www/static/js/tracker.js`)가 보내고, Worker 가
//! `/analytic/events` 로 받아 사이트(`cc`)마다 쌓는 모양이다. tracker-logis-center 의 `pushEvent` 를 옮기면서 값을 줄였다.
//!
//! | tracker-logis-center | 여기 |
//! | --- | --- |
//! | `action`: 누른 요소의 `outerHTML` | `target.pug`: 속성을 지운 PUG(`{들여쓰기}{태그} \| {글}`) + `target.chain`: 셀렉터용 조상 모양([`DomNode`]) |
//! | `relate`: 같은 목록의 형제 `outerHTML`(그림 주소만 지움) | `related[]`: 같은 모양 |
//! | 주소 전체(`href`, 쿼리 포함) | 쿼리 · 문서 안 위치를 뺀 주소 (해시 라우팅 `#/…` 만 남김) |
//! | 쿠키의 `hash` · `token`(방문자 계정) | 탭마다 새로 만드는 무작위 `sid` (계정 없음) |
//! | 사건 `click` · `hover` · `change` | 더해서 `enter` · `scroll` · `submit` · `back` · `leave` (마찰 신호) |
//!
//! 입력값 · 속성(`href` · `src` · `value` · `class` …)은 보내지 않는다. 글에서는 이메일과 숫자 네 개 이상이 든 낱말을
//! 지운다([`scrub_text`], 전화 · 카드 · 주문 번호). 비밀번호 칸의 사건은 만들지 않는다.

use premium_discount_nav::DomNode;
use serde::{Deserialize, Serialize};

pub const EVENT_SCHEMA: &str = "web-event/1";
/// 사건 하나의 형제 수 상한 (tracker-logis-center 의 topK)
pub const MAX_RELATED: usize = 100;
/// PUG 글 하나의 길이 상한 (글자)
pub const MAX_PUG_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// 페이지에 들어왔다
    Enter,
    /// 눌렀다 (`rage`: 700 ms 안에 세 번 이상)
    Click,
    /// 500 ms 이상 머물다 떠났다 (`dwell_ms`)
    Hover,
    /// 입력 칸 · 선택을 바꿨다 (600 ms 디바운스, 값 없이 `field` 종류만)
    Change,
    /// 스크롤 묶음 하나 (`flips`: 방향을 뒤집은 수)
    Scroll,
    /// form 을 제출했다
    Submit,
    /// 뒤로 갔다 (popstate)
    Back,
    /// 페이지를 떠났다 (`dirty`: 바꾸고 제출하지 않은 칸 수)
    Leave,
}

/// 요소 하나: 속성 없는 PUG 와 셀렉터용 조상 모양
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Element {
    /// 누른 요소부터 위로 (`chain[0]` 이 요소). 글자 · 입력값 없음
    #[serde(default)]
    pub chain: Vec<DomNode>,
    /// 속성을 지운 PUG
    #[serde(default)]
    pub pug: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebEvent {
    #[serde(rename = "type")]
    pub kind: EventKind,
    /// 페이지 주소 (쿼리 없음)
    pub href: String,
    /// 탭 세션 (무작위, 탭을 닫으면 사라진다). 서버에 저장된 사건에는 없고, 행의 `session`(hashId(cc + sid))이 대신한다
    #[serde(default)]
    pub sid: String,
    /// 세션 안의 순서
    #[serde(default)]
    pub seq: u32,
    /// 세션 시작부터의 경과 시간 (ms). 벽시계 대신 쓴다
    #[serde(default)]
    pub t_ms: i64,
    /// 누른 · 바꾼 요소 (체크포인트의 단위)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Element>,
    /// 그 요소가 목록 안에 있으면 그 목록 항목 (형제와 같은 단위: 선택 대조의 단위). 카드 안의 링크를 누르면 target 은 링크,
    /// item 은 카드다. tracker-logis-center 는 이때 링크와 카드의 모양이 달라 형제를 하나도 보내지 못했다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Element>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Element>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rage: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dwell_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flips: Option<u32>,
    /// change: 칸의 종류 (`select` · `checkbox` · `radio` · `text` …)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// leave: 바꾸고 제출하지 않은 칸 수
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<u32>,
    /// 서버가 받은 시각 (epoch ms). Worker 가 채운다
    #[serde(default)]
    pub at_ms: i64,
}

fn looks_private(word: &str) -> bool {
    let email = word.split_once('@').is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.'));
    email || word.chars().filter(|c| c.is_ascii_digit()).count() >= 4
}

/// 글에서 개인정보로 보이는 낱말(이메일, 숫자 네 개 이상)을 `#` 로 바꾼다. 줄 구조는 그대로 둔다
/// (`premium-discount-nav` 의 `scrub_name` 과 같은 기준이지만, 지우지 않고 자리를 남겨 PUG 의 모양을 지킨다)
pub fn scrub_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            let indent: String = line.chars().take_while(|c| *c == ' ').collect();
            let words: Vec<String> = line[indent.len()..].split(' ').map(|w| if looks_private(w) { "#".to_string() } else { w.to_string() }).collect();
            format!("{indent}{}", words.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 주소에서 쿼리와 문서 안 위치를 뺀다. 해시 라우팅(`#/cart`, `#!/cart`)은 남긴다. http(s) 가 아니면 None
pub fn clean_href(href: &str) -> Option<String> {
    let href = href.trim();
    let (scheme, rest) = href.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let route = rest.split_once('#').map(|(_, f)| f).filter(|f| f.starts_with('/') || f.starts_with("!/"));
    let base = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    if base.split('/').next().unwrap_or("").is_empty() {
        return None;
    }
    let route = route.map(|r| &r[..r.find('?').unwrap_or(r.len())]);
    Some(match route {
        Some(r) => format!("{}://{base}#{r}", scheme.to_ascii_lowercase()),
        None => format!("{}://{base}", scheme.to_ascii_lowercase()),
    })
}

/// 사이트 키: 주소의 호스트 (소문자, 포트 · 사용자 정보 없이). Worker 의 `cc = hashId(host)` 와 앱의 `app_hash` 가 이 값에서 나온다
pub fn host_of(href: &str) -> Option<String> {
    let rest = href.trim().split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

impl WebEvent {
    /// 받은 사건을 다듬는다: 주소 정리, PUG 글 개인정보 거르기 · 길이 자르기, 형제 수 자르기. 주소가 http(s) 가 아니면 None
    pub fn sanitized(mut self) -> Option<WebEvent> {
        self.href = clean_href(&self.href)?;
        let clean = |e: &mut Element| {
            e.pug = scrub_text(&e.pug).chars().take(MAX_PUG_CHARS).collect();
            e.chain.truncate(premium_discount_nav::SELECTOR_DEPTH);
        };
        if let Some(t) = self.target.as_mut() {
            clean(t);
        }
        if let Some(t) = self.item.as_mut() {
            clean(t);
        }
        self.related.truncate(MAX_RELATED);
        self.related.iter_mut().for_each(clean);
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_words_become_hashes_and_lines_keep_their_indent() {
        let pug = "div\n  span | 홍길동 010-1234-5678 hong@example.com\n  button | 결제하기 2026";
        assert_eq!(scrub_text(pug), "div\n  span | 홍길동 # #\n  button | 결제하기 #");
    }

    #[test]
    fn href_keeps_only_the_route() {
        assert_eq!(clean_href("HTTPS://Shop.example.com/cart?coupon=A1&email=x@y.z#top").as_deref(), Some("https://Shop.example.com/cart"));
        assert_eq!(clean_href("https://app.example.com/#/orders/12?tab=2").as_deref(), Some("https://app.example.com/#/orders/12"));
        assert_eq!(clean_href("https://app.example.com/#!/cart").as_deref(), Some("https://app.example.com/#!/cart"));
        assert_eq!(clean_href("javascript:alert(1)"), None);
        assert_eq!(clean_href("https:///nohost"), None);
        assert_eq!(host_of("https://user:pw@Shop.Example.com:8443/a").as_deref(), Some("shop.example.com"));
    }

    #[test]
    fn events_round_trip_and_are_trimmed() {
        let raw = serde_json::json!({
            "type": "click", "href": "https://shop.example.com/p/1?ref=mail", "sid": "s1", "seq": 3, "t_ms": 1200,
            "target": {"chain": [{"tag": "button", "classes": ["buy"]}], "pug": "button | 구매 4111-1111"},
            "related": (0..120).map(|i| serde_json::json!({"pug": format!("li | item {i}")})).collect::<Vec<_>>(),
            "rage": true
        });
        let e: WebEvent = serde_json::from_value(raw).unwrap();
        let e = e.sanitized().unwrap();
        assert_eq!(e.href, "https://shop.example.com/p/1");
        assert_eq!(e.target.as_ref().unwrap().pug, "button | 구매 #");
        assert_eq!(e.related.len(), MAX_RELATED);
        assert!(e.rage);
        let back: WebEvent = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
