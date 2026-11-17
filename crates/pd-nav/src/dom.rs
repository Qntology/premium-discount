//! 웹 요소의 DOM 셀렉터 틀: 웹 체크포인트(`element`)의 키
//!
//! 브라우저 확장은 사용자가 누른 요소에서 위로 올라가며 조상 노드의 모양만 보낸다([`DomNode`]: 태그 · id · class ·
//! 허용한 속성 · 같은 태그 형제 중 순서). 글자(textContent) · 입력값 · aria-label · href 는 보내지 않는다. 사람 이름 ·
//! 주문 번호가 섞일 수 있어서다. [`selector_template`] 는 그 사슬을 페이지가 바뀌어도 같은 요소를 가리키는 셀렉터로 바꾼다.
//!
//! - 안정된 id(`#checkout`)나 시험용 속성(`data-testid` 등)을 만나면 거기서 멈춘다(그 요소를 하나로 가리키므로).
//! - 빌드마다 바뀌는 class(`css-1x2y3z`, `Button_root__3xK9z`)와 상태 class(`active`, `is-open`)는 버린다.
//! - id · class · 속성이 없는 노드만 `:nth-of-type(n)` 으로 자리를 남긴다(목록의 n 번째 상품처럼 데이터에 따라 바뀌는 자리는
//!   class 가 있으면 남기지 않는다).
//! - 깊이는 [`SELECTOR_DEPTH`] 까지. 긴 숫자는 해시를 만들 때 `#` 로 바뀐다([`crate::element_hash`]).

use serde::{Deserialize, Serialize};

/// 셀렉터에 넣는 조상 노드 수 (누른 요소 포함)
pub const SELECTOR_DEPTH: usize = 5;
/// 셀렉터에 남기는 class 수 (노드마다)
const MAX_CLASSES: usize = 2;
const MAX_TOKEN: usize = 40;
/// 요소를 하나로 가리키는 시험용 속성 (만나면 거기서 멈춘다)
const TEST_ATTRS: [&str; 4] = ["data-testid", "data-test", "data-qa", "data-cy"];
/// 셀렉터에 넣어도 되는 속성 (값은 사람이 입력한 것이 아니다)
const KEPT_ATTRS: [&str; 3] = ["role", "name", "type"];
/// 누르거나 열 때마다 바뀌는 상태 class
const STATE_CLASSES: [&str; 12] = ["active", "selected", "open", "opened", "show", "shown", "hover", "focus", "focused", "disabled", "checked", "expanded"];

/// 누른 요소와 그 조상 하나 (확장이 보내는 모양, 글자 없음)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomNode {
    pub tag: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub classes: Vec<String>,
    /// 속성 (이름, 값). [`TEST_ATTRS`] · [`KEPT_ATTRS`] 밖의 속성은 쓰지 않는다
    #[serde(default)]
    pub attrs: Vec<(String, String)>,
    /// 같은 태그 형제 중 몇 번째인가 (1부터)
    #[serde(default = "first")]
    pub nth_of_type: u32,
}

fn first() -> u32 {
    1
}

fn safe_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= MAX_TOKEN && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 빌드 · 화면마다 바뀌어 보이는 이름: 숫자 네 개 이상, 또는 `-` · `_` 로 나눈 조각 중 다섯 자 이상이면서 글자와 숫자가 섞인 것
/// (`css-1x2y3z`, `Button_root__3xK9z`, `ember1234`, `sc-a1b2c`)
fn volatile(token: &str) -> bool {
    let digits = token.chars().filter(|c| c.is_ascii_digit()).count();
    let hashy = token.split(['-', '_']).any(|part| {
        part.len() >= 5 && part.chars().any(|c| c.is_ascii_digit()) && part.chars().any(|c| c.is_ascii_alphabetic())
    });
    digits >= 4 || hashy || token.starts_with("css-") || token.starts_with("sc-") || token.starts_with("jsx-")
}

fn stable(token: &str) -> bool {
    safe_token(token) && !volatile(token)
}

fn state_class(class: &str) -> bool {
    let lower = class.to_ascii_lowercase();
    STATE_CLASSES.contains(&lower.as_str()) || lower.starts_with("is-") || lower.starts_with("has-")
}

fn attr<'a>(node: &'a DomNode, name: &str) -> Option<&'a str> {
    node.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim())
}

/// 누른 요소부터 위로 올라간 사슬(`chain[0]` 이 누른 요소)을 셀렉터 틀로 바꾼다. 뿌리에서 요소 쪽으로 ` > ` 로 잇는다.
/// 누른 요소의 태그가 이상하면 None
pub fn selector_template(chain: &[DomNode]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for node in chain.iter().take(SELECTOR_DEPTH) {
        let tag = node.tag.trim().to_ascii_lowercase();
        if !safe_token(&tag) {
            if parts.is_empty() {
                return None;
            }
            break;
        }
        if tag == "html" || tag == "body" {
            break;
        }
        if let Some(id) = node.id.as_deref().map(str::trim).filter(|id| stable(id)) {
            parts.push(format!("{tag}#{id}"));
            break;
        }
        if let Some((name, value)) = TEST_ATTRS.iter().find_map(|a| attr(node, a).filter(|v| safe_token(v)).map(|v| (*a, v))) {
            parts.push(format!("{tag}[{name}=\"{value}\"]"));
            break;
        }
        let mut part = tag.clone();
        for name in KEPT_ATTRS {
            if let Some(value) = attr(node, name).filter(|v| safe_token(v)) {
                part.push_str(&format!("[{name}=\"{value}\"]"));
            }
        }
        let mut classes: Vec<&str> = node.classes.iter().map(|c| c.trim()).filter(|c| stable(c) && !state_class(c)).collect();
        classes.sort_unstable();
        classes.dedup();
        for class in classes.iter().take(MAX_CLASSES) {
            part.push('.');
            part.push_str(class);
        }
        if part == tag && node.nth_of_type > 1 {
            part.push_str(&format!(":nth-of-type({})", node.nth_of_type));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    Some(parts.join(" > "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tag: &str, id: Option<&str>, classes: &[&str], attrs: &[(&str, &str)], nth: u32) -> DomNode {
        DomNode {
            tag: tag.into(),
            id: id.map(Into::into),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            nth_of_type: nth,
        }
    }

    #[test]
    fn stops_at_a_stable_id_or_test_attribute() {
        let chain = [
            node("BUTTON", None, &["btn", "btn-primary", "active"], &[("type", "submit")], 1),
            node("div", None, &["css-1x2y3z", "actions"], &[], 2),
            node("form", Some("checkout"), &[], &[], 1),
            node("main", None, &[], &[], 1),
        ];
        assert_eq!(selector_template(&chain).as_deref(), Some("form#checkout > div.actions > button[type=\"submit\"].btn.btn-primary"));
        let tested = [node("a", None, &["link"], &[("data-testid", "add-to-cart")], 3), node("li", None, &[], &[], 4)];
        assert_eq!(selector_template(&tested).as_deref(), Some("a[data-testid=\"add-to-cart\"]"));
    }

    #[test]
    fn volatile_ids_and_classes_are_dropped() {
        let chain = [
            node("span", Some(":r1:"), &["Button_root__3xK9z", "is-open", "label"], &[], 1),
            node("div", Some("ember1234"), &["sc-a1b2c", "card"], &[], 7),
            node("section", Some("user-48213"), &[], &[], 1),
            node("body", None, &[], &[], 1),
        ];
        // 숫자 · 해시 모양 id 는 안정된 id 가 아니다. 목록의 7 번째 카드는 class 가 있어 자리(nth)를 남기지 않는다
        assert_eq!(selector_template(&chain).as_deref(), Some("section > div.card > span.label"));
    }

    #[test]
    fn bare_nodes_keep_their_place_and_depth_is_limited() {
        let chain: Vec<DomNode> = (0..8).map(|i| node("div", None, &[], &[], i + 1)).collect();
        let t = selector_template(&chain).unwrap();
        assert_eq!(t.matches(" > ").count(), SELECTOR_DEPTH - 1);
        assert_eq!(t, "div:nth-of-type(5) > div:nth-of-type(4) > div:nth-of-type(3) > div:nth-of-type(2) > div");
    }

    #[test]
    fn no_text_or_unsafe_values_leak_into_the_selector() {
        let chain = [node(
            "input",
            None,
            &[],
            &[("name", "email"), ("aria-label", "홍길동님의 이메일"), ("value", "hong@example.com"), ("role", "text box")],
            1,
        )];
        let t = selector_template(&chain).unwrap();
        assert_eq!(t, "input[name=\"email\"]");
        assert!(!t.contains("홍길동") && !t.contains("hong@"));
        assert_eq!(selector_template(&[node("x y", None, &[], &[], 1)]), None);
        assert_eq!(selector_template(&[]), None);
    }
}
