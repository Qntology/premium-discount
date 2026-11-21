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
//!
//! 시나리오 5 에서 더한 것 (모두 생략할 수 있는 칸이라 예전 사건도 그대로 읽는다):
//! - `surface`: 주소는 그대로인데 SPA 모달 · 드로어가 열려 화면이 바뀐 것. 수집 스크립트는 그때 가상 주소
//!   `#/__modal/<slug>`([`modal_href`])로 `enter` 를 내고, 모달 위의 사건에 `surface: "modal_drawer"` 를 붙인다. 가상 주소는
//!   해시 라우팅 모양이라 [`clean_href`] · `url_template` 을 그대로 지나고, 퍼널에서는 그 slug 의 페이지 종류(장바구니 · 결제)가 된다.
//! - `tab_group`: 새 탭이 부모 탭의 sessionStorage 에서 물려받는 무작위 값 (쿠키 · 계정 없음, 탭을 모두 닫으면 사라진다).
//!   여러 탭으로 상품을 견준 방문은 이 값으로 묶는다(`funnel` 의 비교 쇼핑 면제).
//! - 재전송에도 바뀌지 않는 사건 id ([`deterministic_event_id`]): 서버가 받은 시각 대신 (세션, 순서, 경과 시간, 종류)로 만든다.

use premium_discount_nav::DomNode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const EVENT_SCHEMA: &str = "web-event/1";
/// 사건 하나의 형제 수 상한 (tracker-logis-center 의 topK)
pub const MAX_RELATED: usize = 100;
/// PUG 글 하나의 길이 상한 (글자)
pub const MAX_PUG_CHARS: usize = 4000;
/// 모달 · 드로어 가상 주소의 경로 조각 (`#/__modal/<slug>`, 해시 라우팅이 이미 있으면 `#/<경로>/__modal/<slug>`)
pub const MODAL_ROUTE: &str = "__modal";
/// 가상 주소 slug 의 길이 상한
const MODAL_SLUG_CHARS: usize = 32;
/// 탭 묶음 값의 길이 상한
const MAX_TAB_GROUP_CHARS: usize = 80;
/// 사건 id 의 길이 (SHA-256 hex 의 앞)
const EVENT_ID_CHARS: usize = 32;

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

impl EventKind {
    /// 선 위의 이름 (`type` 칸의 값)
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Enter => "enter",
            EventKind::Click => "click",
            EventKind::Hover => "hover",
            EventKind::Change => "change",
            EventKind::Scroll => "scroll",
            EventKind::Submit => "submit",
            EventKind::Back => "back",
            EventKind::Leave => "leave",
        }
    }
}

/// 사건이 난 화면 표면
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualSurface {
    /// 문서 페이지 (생략했을 때)
    #[default]
    DocumentPage,
    /// 주소는 그대로인 채 열린 SPA 모달 · 드로어 (aria-modal · role="dialog" · `<dialog open>`, 고정 층)
    ModalDrawer,
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
    /// SPA 모달 · 드로어 위의 사건이면 `modal_drawer` (그때 `href` 는 가상 주소 `#/__modal/<slug>`). 생략하면 문서 페이지
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<VirtualSurface>,
    /// 탭 묶음 (부모 탭에서 물려받은 무작위 값, 서버는 `sid` 처럼 hashId(cc + 값)으로 바꿔 저장한다). 생략할 수 있다
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_group: Option<String>,
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

/// 가상 주소의 slug: 소문자 영문 · 숫자 · `-` 만 남기고(그 밖은 `-`, 겹친 `-` 는 하나로) 32자까지. 비면 `dialog`.
/// 수집 스크립트의 `slugify` 와 같은 규칙이다
pub fn modal_slug(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().to_ascii_lowercase().chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
    }
    let out: String = out.trim_end_matches('-').chars().take(MODAL_SLUG_CHARS).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "dialog".to_string()
    } else {
        out
    }
}

/// 해시 라우팅 조각 (`#/…` · `#!/…` 의 `#` 뒤). 없으면 None
fn hash_route(href: &str) -> Option<&str> {
    href.split_once('#').map(|(_, f)| f).filter(|f| f.starts_with('/') || f.starts_with("!/"))
}

/// 모달 · 드로어의 가상 주소. 해시 라우팅이 없으면 `<주소>#/__modal/<slug>`, 있으면 `<주소>#/<경로>/__modal/<slug>`
/// (이미 모달 주소면 그 모달 조각을 바꾼다). [`clean_href`] 와 `url_template` 이 그대로 남기는 모양이다. http(s) 가 아니면 None
pub fn modal_href(href: &str, slug: &str) -> Option<String> {
    let clean = clean_href(href)?;
    let slug = modal_slug(slug);
    let marker = format!("/{MODAL_ROUTE}/");
    Some(match clean.split_once('#') {
        Some((base, route)) => {
            let route = match route.find(&marker) {
                Some(p) => &route[..p],
                None => route,
            };
            format!("{base}#{}/{MODAL_ROUTE}/{slug}", route.trim_end_matches('/'))
        }
        None => format!("{clean}#/{MODAL_ROUTE}/{slug}"),
    })
}

/// 주소가 모달 가상 주소면 그 slug
pub fn modal_slug_of(href: &str) -> Option<String> {
    let route = hash_route(href.trim())?;
    let route = &route[..route.find('?').unwrap_or(route.len())];
    let mut parts = route.split('/').filter(|s| !s.is_empty());
    parts.by_ref().find(|s| *s == MODAL_ROUTE)?;
    parts.next().map(modal_slug)
}

/// 주소로 본 표면: 모달 가상 주소면 [`VirtualSurface::ModalDrawer`]
pub fn surface_of(href: &str) -> VirtualSurface {
    if modal_slug_of(href).is_some() {
        VirtualSurface::ModalDrawer
    } else {
        VirtualSurface::DocumentPage
    }
}

/// SHA-256 의 소문자 hex
pub(crate) fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// 재전송에도 바뀌지 않는 사건 id: `sha256("{cc}|{session}|{seq}|{t_ms}|{type}")` hex 의 앞 32자. `cc` · `session` 은 서버 행의
/// 값(hashId(host) · hashId(cc + sid))이다. 서버가 받은 시각(`at_ms`)은 넣지 않는다: 같은 묶음을 다시 보내면 시각만 달라지는데,
/// 그 시각을 넣으면 다시 보낸 사건이 다른 id 를 받아 두 번 들어간다. 형제(`related`)도 넣지 않는다(떠날 때 줄여 보낼 수 있다).
/// 순서와 경과 시간이 모두 0 인 예전 사건(번호 없는 시험 입력)은 None 이다: 부르는 쪽이 예전처럼 무작위 id 를 쓴다
pub fn deterministic_event_id(cc_hash: &str, session_hash: &str, e: &WebEvent) -> Option<String> {
    if e.seq == 0 && e.t_ms == 0 {
        return None;
    }
    let text = format!(
        "{}|{}|{}|{}|{}",
        cc_hash.trim().to_ascii_lowercase(),
        session_hash.trim().to_ascii_lowercase(),
        e.seq,
        e.t_ms,
        e.kind.as_str()
    );
    Some(sha256_hex(&text)[..EVENT_ID_CHARS].to_string())
}

impl WebEvent {
    /// 빈 사건 하나 (종류와 주소만)
    pub fn new(kind: EventKind, href: &str) -> WebEvent {
        WebEvent {
            kind,
            href: href.to_string(),
            sid: String::new(),
            seq: 0,
            t_ms: 0,
            surface: None,
            tab_group: None,
            target: None,
            item: None,
            related: Vec::new(),
            rage: false,
            dwell_ms: None,
            flips: None,
            field: None,
            dirty: None,
            at_ms: 0,
        }
    }

    /// 표면: 붙은 값이 있으면 그것, 없으면 주소로 본다 (예전 사건 · 서버가 칸을 버린 사건도 모달 주소면 모달)
    pub fn effective_surface(&self) -> VirtualSurface {
        self.surface.unwrap_or_else(|| surface_of(&self.href))
    }

    /// 받은 사건을 다듬는다: 주소 정리, PUG 글 개인정보 거르기 · 길이 자르기, 형제 수 자르기, 탭 묶음 값 검사(영문 · 숫자 ·
    /// `-` · `_`, 80자까지. 아니면 버린다). 주소가 http(s) 가 아니면 None
    pub fn sanitized(mut self) -> Option<WebEvent> {
        self.href = clean_href(&self.href)?;
        self.tab_group = self.tab_group.map(|t| t.trim().to_string()).filter(|t| {
            !t.is_empty() && t.chars().count() <= MAX_TAB_GROUP_CHARS && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
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

    #[test]
    fn modal_hrefs_are_hash_routes_appended_to_the_page() {
        assert_eq!(modal_href("https://shop.example.com/product/1?ref=x#top", "cart").as_deref(), Some("https://shop.example.com/product/1#/__modal/cart"));
        assert_eq!(modal_href("https://shop.example.com/#/product/1", "checkout").as_deref(), Some("https://shop.example.com/#/product/1/__modal/checkout"));
        assert_eq!(modal_href("https://app.example.com/#!/cart/", "Cart Drawer").as_deref(), Some("https://app.example.com/#!/cart/__modal/cart-drawer"));
        // 모달 위에 다른 모달: 모달 조각만 바뀐다
        assert_eq!(modal_href("https://shop.example.com/product/1#/__modal/cart", "checkout").as_deref(), Some("https://shop.example.com/product/1#/__modal/checkout"));
        assert_eq!(modal_href("ftp://shop.example.com/a", "cart"), None);
        // 가상 주소는 주소 정리를 그대로 지난다
        let h = modal_href("https://shop.example.com/product/1", "cart").unwrap();
        assert_eq!(clean_href(&h).as_deref(), Some(h.as_str()));
        assert_eq!(modal_slug_of(&h).as_deref(), Some("cart"));
        assert_eq!(surface_of(&h), VirtualSurface::ModalDrawer);
        assert_eq!(surface_of("https://shop.example.com/#/orders/12"), VirtualSurface::DocumentPage);
    }

    #[test]
    fn modal_slugs_keep_only_lowercase_letters_digits_and_dashes() {
        assert_eq!(modal_slug("  Cart__Drawer!! "), "cart-drawer");
        assert_eq!(modal_slug("장바구니"), "dialog");
        assert_eq!(modal_slug(&"a".repeat(40)), "a".repeat(32));
        assert_eq!(modal_slug("checkout-"), "checkout");
    }

    #[test]
    fn surface_and_tab_group_are_optional_on_the_wire() {
        // 예전 사건: 두 칸이 없다
        let old: WebEvent = serde_json::from_value(serde_json::json!({"type": "enter", "href": "https://shop.example.com/product/1#/__modal/cart"})).unwrap();
        assert_eq!((old.surface, old.tab_group.as_deref()), (None, None));
        assert_eq!(old.effective_surface(), VirtualSurface::ModalDrawer, "the virtual href alone tells the surface");
        assert!(!serde_json::to_string(&WebEvent::new(EventKind::Click, "https://a.example.com/")).unwrap().contains("surface"));

        let raw = serde_json::json!({"type": "click", "href": "https://shop.example.com/p/1", "surface": "modal_drawer", "tab_group": " 0f3a9c "});
        let e: WebEvent = serde_json::from_value(raw).unwrap();
        let e = e.sanitized().unwrap();
        assert_eq!((e.surface, e.tab_group.as_deref()), (Some(VirtualSurface::ModalDrawer), Some("0f3a9c")));
        let back: WebEvent = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        // 탭 묶음 값이 이상하면 버린다 (개인정보 · 주입이 들어갈 자리를 남기지 않는다)
        let long = "x".repeat(81);
        for bad in ["hong@example.com", "a b", "", long.as_str()] {
            let mut e = WebEvent::new(EventKind::Enter, "https://shop.example.com/");
            e.tab_group = Some(bad.to_string());
            assert_eq!(e.sanitized().unwrap().tab_group, None, "{bad}");
        }
    }

    #[test]
    fn event_ids_ignore_the_receive_time_and_the_trimmed_siblings() {
        let mut e = WebEvent::new(EventKind::Leave, "https://shop.example.com/cart");
        e.seq = 7;
        e.t_ms = 12_345;
        e.at_ms = 1_000;
        let id = deterministic_event_id("0xCC", "0xSession", &e).unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        let mut again = e.clone();
        again.at_ms = 3_000;
        again.related = vec![Element::default()];
        assert_eq!(deterministic_event_id("0xcc", "0xsession", &again).as_deref(), Some(id.as_str()), "case and receive time do not matter");
        let mut other = e.clone();
        other.seq = 8;
        assert_ne!(deterministic_event_id("0xcc", "0xsession", &other).unwrap(), id);
        other = e.clone();
        other.kind = EventKind::Submit;
        assert_ne!(deterministic_event_id("0xcc", "0xsession", &other).unwrap(), id);
        assert_ne!(deterministic_event_id("0xcc", "0xother", &e).unwrap(), id);
        // 번호 없는 예전 사건은 무작위 id 를 그대로 쓴다
        assert_eq!(deterministic_event_id("0xcc", "0xsession", &WebEvent::new(EventKind::Click, "https://a.example.com/")), None);
        // 같은 규칙의 기준값 (Worker 가 같은 글을 SHA-256 해 같은 값을 내야 한다)
        assert_eq!(id, &sha256_hex("0xcc|0xsession|7|12345|leave")[..32]);
    }
}
