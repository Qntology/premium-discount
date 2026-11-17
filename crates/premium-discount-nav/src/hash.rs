//! 해시: premium.discount 의 `hashId(text)` 와 화면 키
//!
//! [`hash_id`] 는 서버 · 웹 클라이언트 · raon-zena 와 같은 규칙이다: `ethers.computeAddress(ethers.hashMessage(text))`.
//! EIP-191 메시지 해시를 secp256k1 개인 키로 보고, 그 공개 키의 주소를 낸다(구현은 `premium-discount-identity`). 같은 화면은 누가
//! 보든 같은 해시가 되므로 서버는 계정 없이도 같은 화면의 기록을 모을 수 있다. 원문(URL · 창 구조)은 기기를 떠나지 않는다.

pub use premium_discount_identity::{hash_id, hash_message};

use crate::privacy::scrub_anchor;

/// 화면 · 영역 해시를 평가 주소와 섞이지 않게 하는 머리말
const NAV_PREFIX: &str = "nav/1|";

/// 앱 구조 지문에 넣는 UIA 트리의 깊이 · 노드 수 한도 (창마다 바뀌는 깊은 내용은 넣지 않는다)
const STRUCTURE_DEPTH: u8 = 2;
const STRUCTURE_NODES: usize = 64;

/// 웹 화면 키의 틀(링크값): 호스트(소문자, 사용자 정보 · 포트 없이) + 경로. 쿼리는 버리고, 숫자 · id 처럼 보이는
/// 경로 조각은 `:id` 로 바꾼다. 조각(#)은 해시 라우팅(`#/cart`, `#!/cart`: SPA 의 화면 이동)일 때만 `#/경로` 로 남기고,
/// 문서 안 위치(`#top`)는 버린다. http(s) 가 아니면 None
pub fn url_template(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = match url.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return None,
        None => url,
    };
    let fragment = rest.split_once('#').map(|(_, f)| f);
    let rest = &rest[..rest.find(|c| c == '?' || c == '#').unwrap_or(rest.len())];
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    let segments: Vec<String> = path.split('/').filter(|s| !s.is_empty()).map(template_segment).collect();
    let base = if segments.is_empty() { host } else { format!("{host}/{}", segments.join("/")) };
    let route = fragment.and_then(|f| f.strip_prefix('!').unwrap_or(f).strip_prefix('/')).map(|r| {
        let r = &r[..r.find('?').unwrap_or(r.len())];
        r.split('/').filter(|s| !s.is_empty()).map(template_segment).collect::<Vec<_>>().join("/")
    });
    Some(match route {
        Some(route) => format!("{base}#/{route}"),
        None => base,
    })
}

/// 웹 페이지(링크값)의 해시: URL 틀([`url_template`])과 그 호스트로 만든 화면 해시. 같은 틀의 페이지는 누가 열어도 같다.
/// http(s) 가 아니면 None
pub fn page_hash(url: &str) -> Option<String> {
    let template = url_template(url)?;
    let host = template.split(|c| c == '/' || c == '#').next().unwrap_or("").to_string();
    screen_hash(&host, &template)
}

/// 웹 요소의 해시: 페이지 해시와 DOM 셀렉터 틀([`crate::selector_template`])로. 셀렉터의 긴 숫자는 `#` 로 바뀐다
pub fn element_hash(page_hash: &str, selector: &str) -> Option<String> {
    region_hash(page_hash, &format!("css:{}", selector.trim()))
}

fn template_segment(segment: &str) -> String {
    let digits = segment.chars().filter(|c| c.is_ascii_digit()).count();
    let all_digits = segment.chars().all(|c| c.is_ascii_digit());
    let hex_like = segment.len() >= 8 && segment.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let token_like = segment.len() >= 24 && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if all_digits || digits >= 4 || hex_like || token_like || segment.contains('@') {
        ":id".to_string()
    } else {
        segment.to_lowercase()
    }
}

/// 앱 화면 키의 틀: 실행 파일 이름 + UIA 트리 위쪽(깊이 0 ~ 2, 앞의 64개)의 `깊이:컨트롤 종류:AutomationId`.
/// 창 제목은 쓰지 않는다(문서 · 사람 이름이 섞인다). AutomationId 의 긴 숫자는 `#` 로 바꾼다
pub fn structure_template(exe: &str, nodes: &[(u8, &str, &str)]) -> String {
    let mut out = exe.trim().to_ascii_lowercase();
    for (depth, control, id) in nodes.iter().filter(|(d, _, _)| *d <= STRUCTURE_DEPTH).take(STRUCTURE_NODES) {
        out.push_str(&format!("|{depth}:{}:{}", control.trim().to_ascii_lowercase(), scrub_anchor(id.trim())));
    }
    out
}

/// 앱 · 웹 해시: 타임트라이얼 콜렉션을 앱 · 웹마다 나누는 키. 서버는 `cc` 원문(도메인 · 실행 파일 이름)을 받지 않는다.
/// 머리말이 화면 해시(`nav/1|`)와 달라 `cc` 가 "app" 인 화면과 섞이지 않는다
pub fn app_hash(cc: &str) -> Option<String> {
    hash_id(&format!("nav-app/1|{}", cc.trim().to_ascii_lowercase()))
}

/// 화면 해시: `cc`(앱 · 사이트)와 화면 키의 틀로
pub fn screen_hash(cc: &str, template: &str) -> Option<String> {
    hash_id(&format!("{NAV_PREFIX}{}|{}", cc.trim().to_ascii_lowercase(), template.trim()))
}

/// 영역 해시: 화면 해시와 위치 단서로 (위치 단서의 긴 숫자는 `#`)
pub fn region_hash(screen_hash: &str, anchor: &str) -> Option<String> {
    hash_id(&format!("{NAV_PREFIX}{}|{}", screen_hash.trim().to_ascii_lowercase(), scrub_anchor(anchor.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 서버 프로젝트가 싣는 ethers 6.9.0 묶음(www/public/static/js/ethers.umd.min.js)으로 뽑은 값.
    // raon-zena premium-discount-mcp 의 address.rs 시험과 같은 기준값이다
    const REFERENCE: [(&str, &str, &str); 4] = [
        ("", "5f35dce98ba4fba25530a026ed80b2cecdaa31091ba4958b99b52ea1d068adad", "0x7404358246d491ed2f9dac694f7dea789037424c"),
        ("hello", "50b2c43fd39106bafbba0da34fc430e1f91e3c96ea2acee2bc34119f92b37750", "0x56d67386939607c11bd60bb009eb02f4dd29c318"),
        (
            "패치 1.4: 보스 체력 15% 너프",
            "98f220dae2d6ec4549c64dcfa6a12f256d6994b01d2a865ad550bb38ab6264f5",
            "0x75252908349507fec4859cf5429f5e19a3710381",
        ),
        ("store.steampowered.com", "926cadd235c4ef546f5e41b355dfb0b7f79cee0a35d9b8ae9eb7824ffc5c4b60", "0x0fee18a53a5dd368d3ce799e31cdef5763533d15"),
    ];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn hash_id_matches_ethers() {
        for (text, msg, addr) in REFERENCE {
            assert_eq!(hex(&hash_message(text)), msg, "hashMessage({text:?})");
            assert_eq!(hash_id(text).unwrap(), addr, "hashId({text:?})");
        }
    }

    #[test]
    fn url_templates_drop_ids_queries_and_fragments() {
        assert_eq!(
            url_template("https://user:pw@WWW.Example.com:8443/Users/12345/orders?id=9&q=홍길동#top").as_deref(),
            Some("www.example.com/users/:id/orders")
        );
        assert_eq!(url_template("http://shop.example/p/a1b2c3d4-e5f6-4711-8a9b-0c1d2e3f4a5b/reviews").as_deref(), Some("shop.example/p/:id/reviews"));
        assert_eq!(url_template("https://mail.example.com/u/hong@example.com/inbox").as_deref(), Some("mail.example.com/u/:id/inbox"));
        assert_eq!(url_template("https://example.com/").as_deref(), Some("example.com"));
        assert_eq!(url_template("example.com/page/2").as_deref(), Some("example.com/page/:id"));
        assert_eq!(url_template("file:///C:/secret.txt"), None);
        // 해시 라우팅은 화면 이동이라 남기고, 문서 안 위치는 버린다
        assert_eq!(url_template("https://app.example.com/?tab=1#/orders/48213?sort=new").as_deref(), Some("app.example.com#/orders/:id"));
        assert_eq!(url_template("https://app.example.com/shop#!/cart").as_deref(), Some("app.example.com/shop#/cart"));
        assert_eq!(url_template("https://app.example.com/docs#install").as_deref(), Some("app.example.com/docs"));
        assert_eq!(url_template("https:///nohost"), None);
    }

    #[test]
    fn app_structure_ignores_deep_nodes_and_long_numbers() {
        let nodes = [
            (0, "Window", "MainWindow"),
            (1, "Pane", "Sidebar"),
            (2, "Edit", "Search_20261116"),
            (3, "Text", "홍길동님의 문서"),
        ];
        let t = structure_template("Code.exe", &nodes);
        assert_eq!(t, "code.exe|0:window:MainWindow|1:pane:Sidebar|2:edit:Search_#");
        assert!(!t.contains("홍길동"), "깊은 노드(문서 내용)는 들어가지 않는다");
    }

    #[test]
    fn web_pages_come_from_link_values_and_elements_from_selectors() {
        // 같은 틀의 링크는 같은 페이지
        let page = page_hash("https://Shop.Example.com/orders/48213?ref=mail#top").unwrap();
        assert_eq!(page, page_hash("https://shop.example.com/orders/99120").unwrap());
        assert_eq!(page, screen_hash("shop.example.com", "shop.example.com/orders/:id").unwrap());
        // 해시 라우팅은 다른 페이지
        assert_ne!(page_hash("https://shop.example.com/#/cart").unwrap(), page_hash("https://shop.example.com/#/orders").unwrap());
        assert_eq!(page_hash("file:///C:/a.html"), None);
        // 요소: 셀렉터의 긴 숫자는 지워져 같은 요소가 된다
        let a = element_hash(&page, "form#checkout > button.pay-48213").unwrap();
        assert_eq!(a, element_hash(&page, "form#checkout > button.pay-77001").unwrap());
        assert_ne!(a, element_hash(&page, "form#checkout > button.cancel").unwrap());
        assert_ne!(a, region_hash(&page, "form#checkout > button.pay-48213").unwrap(), "앱 영역 단서와 섞이지 않는다");
    }

    #[test]
    fn screen_and_region_hashes_are_stable_addresses() {
        let tpl = url_template("https://example.com/cart/991234").unwrap();
        let screen = screen_hash("Example.com", &tpl).unwrap();
        assert_eq!(screen, screen_hash("example.com", "example.com/cart/:id").unwrap());
        assert!(screen.starts_with("0x") && screen.len() == 42);
        assert_ne!(screen, hash_id("example.com").unwrap(), "평가 주소와 섞이지 않는다");
        let a = region_hash(&screen, "main>button#pay-12345678").unwrap();
        let b = region_hash(&screen, "main>button#pay-87654321").unwrap();
        assert_eq!(a, b, "위치 단서의 긴 숫자는 지운 뒤 해시한다");
    }
}
