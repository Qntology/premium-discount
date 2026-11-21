//! 속성 없는 PUG: 사이트마다 다른 `class` · `id` · 인라인 스타일을 지우고 `{들여쓰기}{태그} | {글}` 만 남긴 요소 모양
//!
//! logis-center `analytic.rs` 의 `strip_pug_attributes` · `ai_utils.rs` 의 `pug_line_parts` · `parsing.rs` 의
//! `sanitize_llm_input` 을 그대로 옮겼다. 모델이 사이트의 이름표(class 이름)를 보고 지어내지 못하게 하고, 사용자가 고른
//! 요소(TARGET)와 고르지 않은 형제(RELATED)를 글로만 견주게 하려는 것이다.
//!
//! PD 에서는 수집 스크립트(`web/www/static/js/tracker.js`)가 브라우저의 DOM 에서 바로 이 모양을 만든다. 서버는 HTML 을
//! 받지 않는다. [`html_to_pug`] 는 HTML 배열로 쌓인 예전 사건(tracker-logis-center 의 `action` · `relate`)을 읽을 때만 쓴다.
//! 두 쪽 규칙은 `contracts/analytic_pug_v1.json` 의 사례로 맞춘다. logis-center 의 `ListMode` 규칙을 따르되 다음이 다르다:
//! - 입력 칸 · 글 상자의 값(`input[value]`, `textarea`)은 내지 않는다. 결제 화면에 미리 채운 이름 · 주소가 값으로 들어 있다.
//! - 조각의 뿌리부터 낸다. logis-center 는 조각을 문서로 파싱해 `html` 부터 냈다(B23).
//! - 표 머리 라벨 · 링크 주소 풀기는 분석 경로에서 닿지 않던 코드라 옮기지 않았다.
//!
//! 시나리오 5 에서 더한 규칙 (계약의 13 사례는 그대로이고, 새 규칙은 수집 스크립트의 시험이 같은 글로 확인한다):
//! - 글이 하나도 없는 대화형 요소(`button` · `a` · role="button" · "link" · "tab" · "menuitem")는 이름표를 `[ ]` 에 넣어 낸다:
//!   aria-label → title → 바로 아래 `img[alt]` 중 처음 것, 개인정보 거르기([`crate::event::scrub_text`]) 뒤 40자까지
//!   (`<button aria-label="장바구니"><svg/></button>` → `button | [장바구니]`). 입력 칸 · 글 상자 · 선택 상자는 대상이 아니다.
//! - 열린 모달의 뿌리(aria-modal="true", role="dialog" · "alertdialog", `<dialog open>`)는 고정 · 절대 배치여도 숨은 층으로
//!   버리지 않는다. 커머스의 장바구니 드로어는 대개 고정 층이다.
//! - 선택 대조 키 [`canonical_choice_of`]: 실시간 재고 · 남은 시간 · 구경하는 사람 수를 빼고 상품 번호 · 규격은 남긴 글로 만든다.

use serde::{Deserialize, Serialize};

/// PUG 한 줄의 들여쓰기 수 · 태그 · 속성부 · 글. 속성값 안의 파이프(`a[href="x|y"]`)는 구분자로 보지 않는다
pub fn pug_line_parts(line: &str) -> (usize, String, String, String) {
    let indent = line.chars().take_while(|c| c.is_whitespace()).count();
    let trimmed = line.trim();
    let chars: Vec<char> = trimmed.chars().collect();

    let mut depth = 0i32;
    let mut in_quote: Option<char> = None;
    let mut pipe_pos: Option<usize> = None;

    for (i, ch) in chars.iter().enumerate() {
        match in_quote {
            Some(q) => {
                if *ch == q {
                    in_quote = None;
                }
            }
            None => {
                if *ch == '"' || *ch == '\'' {
                    in_quote = Some(*ch);
                } else if *ch == '[' {
                    depth += 1;
                } else if *ch == ']' {
                    depth -= 1;
                } else if *ch == '|' && depth <= 0 {
                    pipe_pos = Some(i);
                    break;
                }
            }
        }
    }

    let (head, value) = match pipe_pos {
        Some(p) => (chars[..p].iter().collect::<String>().trim().to_string(), chars[p + 1..].iter().collect::<String>().trim().to_string()),
        None => (trimmed.to_string(), String::new()),
    };

    let tag = head.split(|c: char| c == '[' || c == ' ' || c == '(').next().unwrap_or("").to_lowercase();
    let attrs = match head.find('[') {
        Some(p) => head[p..].to_string(),
        None => String::new(),
    };
    (indent, tag, attrs, value)
}

/// PUG 의 속성부(`[...]`)를 모두 지우고 `{들여쓰기}{태그} | {글}` 만 남긴다
pub fn strip_pug_attributes(pug: &str) -> String {
    let mut out = String::new();
    for line in pug.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (indent, tag, _attrs, value) = pug_line_parts(line);
        let v = value.trim();
        if tag.is_empty() && v.is_empty() {
            continue;
        }
        let pad = " ".repeat(indent);
        if tag.is_empty() {
            out.push_str(&format!("{pad}| {v}\n"));
        } else if v.is_empty() {
            out.push_str(&format!("{pad}{tag}\n"));
        } else {
            out.push_str(&format!("{pad}{tag} | {v}\n"));
        }
    }
    out
}

/// 모델에 넣기 전의 글 다듬기: 제어 문자 · 폭 없는 문자 · 방향 바꾸기 문자(프롬프트 주입) · 사용자 영역 문자를 지우고,
/// 특수 토큰 모양(`<|`, `|>`)을 끊는다. 줄바꿈 · 탭은 PUG 구조라서 남긴다
pub fn sanitize_llm_input(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| {
            let u = *c as u32;
            if u == 9 || u == 10 || u == 13 {
                return true;
            }
            if u < 0x20 || (0x7F..=0x9F).contains(&u) {
                return false;
            }
            if matches!(u, 0xFEFF | 0x200B | 0x200C | 0x200D | 0x2060) {
                return false;
            }
            if (0x202A..=0x202E).contains(&u) || (0x2066..=0x2069).contains(&u) {
                return false;
            }
            if (0xE000..=0xF8FF).contains(&u) {
                return false;
            }
            true
        })
        .collect();
    cleaned.replace("<|", "< |").replace("|>", "| >")
}

/// PUG 의 글만 (`{태그} | {글}` · `| {글}` 줄의 글을 공백 하나로 잇는다)
pub fn text_of(pug: &str) -> String {
    pug.lines()
        .map(|l| pug_line_parts(l).3)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- HTML → PUG (예전 사건용) ----

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Element { tag: String, attrs: Vec<(String, String)>, children: Vec<Node> },
    Text(String),
}

const VOID_TAGS: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const SKIP_TAGS: [&str; 7] = ["script", "style", "link", "noscript", "iframe", "svg", "template"];
const PRESERVE_EMPTY: [&str; 5] = ["td", "th", "textarea", "select", "button"];
/// 껍데기: 자식이 하나면 자신을 숨기고 깊이를 그대로 넘긴다
const WRAPPERS: [&str; 16] = ["div", "span", "section", "article", "main", "aside", "header", "footer", "nav", "p", "strong", "b", "em", "i", "center", "font"];
/// 있으면 껍데기로 보지 않는 속성 (ListMode)
const MEANINGFUL_ATTRS: [&str; 15] = ["src", "href", "type", "name", "value", "placeholder", "checked", "selected", "disabled", "readonly", "rows", "cols", "rowspan", "colspan", "scope"];
/// 한 줄로 접는 글의 최대 바이트
const INLINE_MAX_BYTES: usize = 150;
const MAX_DEPTH: usize = 50;
const INDENT: &str = "    ";
/// 글이 없을 때 이름표로 대신하는 대화형 요소의 태그 · role
const INTERACTIVE_TAGS: [&str; 2] = ["button", "a"];
const INTERACTIVE_ROLES: [&str; 4] = ["button", "link", "tab", "menuitem"];
/// 대신 쓰는 이름표 · 선택 대조 라벨의 글자 수
const LABEL_CHARS: usize = 40;

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let end = after.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let name = &after[..e];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ if name.starts_with("#x") || name.starts_with("#X") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
                _ if name.starts_with('#') => name[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            ch.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == '/') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' && chars[i] != '/' {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let name: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < chars.len() && chars[i] == '=' {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i < chars.len() && (chars[i] == '"' || chars[i] == '\'') {
                let q = chars[i];
                i += 1;
                let vs = i;
                while i < chars.len() && chars[i] != q {
                    i += 1;
                }
                value = chars[vs..i].iter().collect();
                i += 1;
            } else {
                let vs = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                value = chars[vs..i].iter().collect();
            }
        }
        out.push((name, decode_entities(&value)));
    }
    out
}

/// 브라우저가 직렬화한 조각(`outerHTML`)을 읽는 작은 파서: 닫히지 않은 태그는 끝에서 닫고, 맞지 않는 닫는 태그는 버린다
fn parse_fragment(html: &str) -> Vec<Node> {
    // (태그, 속성, 자식) 스택. 바닥은 조각의 뿌리
    let mut stack: Vec<(String, Vec<(String, String)>, Vec<Node>)> = vec![(String::new(), Vec::new(), Vec::new())];
    let mut rest = html;
    fn close(stack: &mut Vec<(String, Vec<(String, String)>, Vec<Node>)>) {
        if stack.len() > 1 {
            let (tag, attrs, children) = stack.pop().expect("len > 1");
            stack.last_mut().expect("bottom").2.push(Node::Element { tag, attrs, children });
        }
    }
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            stack.last_mut().expect("bottom").2.push(Node::Text(decode_entities(rest)));
            break;
        };
        if lt > 0 {
            stack.last_mut().expect("bottom").2.push(Node::Text(decode_entities(&rest[..lt])));
        }
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |e| &r[e + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
            continue;
        }
        let Some(gt) = rest.find('>') else {
            stack.last_mut().expect("bottom").2.push(Node::Text(decode_entities(rest)));
            break;
        };
        let inner = &rest[1..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = inner.strip_prefix('/') {
            let name = name.trim().to_ascii_lowercase();
            if let Some(pos) = stack.iter().rposition(|(t, _, _)| *t == name) {
                if pos > 0 {
                    while stack.len() > pos {
                        close(&mut stack);
                    }
                }
            }
            continue;
        }
        let name_end = inner.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(inner.len());
        let tag = inner[..name_end].to_ascii_lowercase();
        if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            stack.last_mut().expect("bottom").2.push(Node::Text(decode_entities(&format!("<{inner}>"))));
            continue;
        }
        let attrs = parse_attrs(&inner[name_end..]);
        if tag == "script" || tag == "style" {
            // 원문 내용(raw text)은 닫는 태그까지 건너뛴다
            let closing = format!("</{tag}");
            let lower = rest.to_ascii_lowercase();
            rest = match lower.find(&closing) {
                Some(p) => rest[p..].find('>').map_or("", |e| &rest[p + e + 1..]),
                None => "",
            };
            continue;
        }
        let self_closing = inner.trim_end().ends_with('/');
        if VOID_TAGS.contains(&tag.as_str()) || self_closing {
            stack.last_mut().expect("bottom").2.push(Node::Element { tag, attrs, children: Vec::new() });
        } else {
            stack.push((tag, attrs, Vec::new()));
        }
    }
    while stack.len() > 1 {
        close(&mut stack);
    }
    stack.pop().map(|(_, _, c)| c).unwrap_or_default()
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// 천 단위 쉼표를 지운다 (`1,200` → `1200`, `12,345.6` → `12345.6`). 숫자 사이의 쉼표가 정확히 세 자리씩일 때만
fn strip_thousands(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == ',' {
            let before = i >= 1 && chars[i - 1].is_ascii_digit();
            let after3 = chars.get(i + 1..i + 4).is_some_and(|w| w.iter().all(char::is_ascii_digit));
            let not_four = !chars.get(i + 4).is_some_and(char::is_ascii_digit);
            if before && after3 && not_four {
                continue;
            }
        }
        out.push(*c);
    }
    out
}

fn clean_text(t: &str) -> String {
    strip_thousands(&t.replace('"', "'").split_whitespace().collect::<Vec<_>>().join(" "))
}

fn has_text_or_kept(node: &Node) -> bool {
    match node {
        Node::Text(t) => !t.trim().is_empty(),
        Node::Element { tag, children, .. } => {
            VOID_TAGS.contains(&tag.as_str()) || PRESERVE_EMPTY.contains(&tag.as_str()) || children.iter().any(has_text_or_kept)
        }
    }
}

/// 글로 접으면 사라지는 데이터(칸 · 입력 · 그림 · 링크)를 자손으로 가졌는가. 내보내지 않는 요소(base64 그림 · 숨은 층 등)는
/// 보지 않는다 (logis-center 는 봤다: 버려질 base64 아이콘 하나가 링크 글의 접기를 막았다)
fn has_data_bearing_descendant(children: &[Node]) -> bool {
    children.iter().any(|c| match c {
        Node::Text(_) => false,
        Node::Element { tag, attrs, .. } if skipped(tag, attrs) => false,
        Node::Element { tag, attrs, children } => {
            ["tr", "td", "th", "input", "select", "option", "textarea", "button", "img"].contains(&tag.as_str())
                || ["href", "src", "value"].iter().any(|a| attr(attrs, a).is_some_and(|v| !v.trim().is_empty()))
                || has_data_bearing_descendant(children)
        }
    })
}

/// 한 줄로 접을 글. 내보내지 않는 요소(숨은 층 · 고르지 않은 option 등)의 글은 넣지 않는다
/// (logis-center 는 접을 때 `text()` 로 숨은 글까지 모았다)
fn all_text(children: &[Node], out: &mut Vec<String>) {
    for c in children {
        match c {
            Node::Text(t) => out.push(t.clone()),
            Node::Element { tag, attrs, children } => {
                if !skipped(tag, attrs) {
                    all_text(children, out)
                }
            }
        }
    }
}

fn skipped(tag: &str, attrs: &[(String, String)]) -> bool {
    if SKIP_TAGS.contains(&tag) {
        return true;
    }
    // 사이트가 수집에서 뺀 영역 (수집 스크립트의 data-pd-ignore)
    if attr(attrs, "data-pd-ignore").is_some() {
        return true;
    }
    if let Some(style) = attr(attrs, "style") {
        let s = style.to_lowercase();
        if s.contains("position") && (s.contains("absolute") || s.contains("fixed")) && !modal_root(tag, attrs) {
            return true;
        }
    }
    if tag == "img" && attr(attrs, "src").is_some_and(|s| s.contains("base64")) {
        return true;
    }
    tag == "option" && attr(attrs, "selected").is_none()
}

/// 열린 모달 · 드로어의 뿌리: 고정 층이어도 숨은 광고 층이 아니다
fn modal_root(tag: &str, attrs: &[(String, String)]) -> bool {
    let role = attr(attrs, "role").map(|r| r.trim().to_ascii_lowercase());
    attr(attrs, "aria-modal").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
        || matches!(role.as_deref(), Some("dialog" | "alertdialog"))
        || (tag == "dialog" && attr(attrs, "open").is_some())
}

/// 누르는 요소 (글이 없으면 이름표로 대신한다). 입력 칸 · 글 상자 · 선택 상자는 아니다(값을 내지 않는다)
fn interactive(tag: &str, attrs: &[(String, String)]) -> bool {
    INTERACTIVE_TAGS.contains(&tag) || attr(attrs, "role").is_some_and(|r| INTERACTIVE_ROLES.contains(&r.trim().to_ascii_lowercase().as_str()))
}

/// 내보낼 글이나 남기는 자식(빈 칸 · 입력 · 그림 등)이 하나라도 있는가. 내보내지 않는 요소(아이콘 svg · base64 그림)는 보지 않는다
fn has_visible_content(children: &[Node]) -> bool {
    children.iter().any(|c| match c {
        Node::Text(t) => !t.trim().is_empty(),
        Node::Element { tag, attrs, .. } if skipped(tag, attrs) => false,
        Node::Element { tag, children, .. } => {
            VOID_TAGS.contains(&tag.as_str()) || PRESERVE_EMPTY.contains(&tag.as_str()) || has_visible_content(children)
        }
    })
}

/// 글 없는 대화형 요소의 이름표: aria-label → title → 바로 아래 img[alt] 중 처음 것. 개인정보를 거른 뒤 40자까지
fn fallback_label(attrs: &[(String, String)], children: &[Node]) -> Option<String> {
    let alt = children.iter().find_map(|c| match c {
        Node::Element { tag, attrs, .. } if tag == "img" => attr(attrs, "alt").filter(|a| !a.trim().is_empty()),
        _ => None,
    });
    let raw = [attr(attrs, "aria-label"), attr(attrs, "title"), alt].into_iter().flatten().map(clean_text).find(|s| !s.is_empty())?;
    // 거른 뒤에 자른다: 자르고 거르면 긴 번호의 앞 세 자리가 남는다
    let label: String = crate::event::scrub_text(&raw).chars().take(LABEL_CHARS).collect();
    let label = label.trim().to_string();
    (!label.is_empty()).then_some(label)
}

fn emit(node: &Node, level: usize, out: &mut String) {
    if level > MAX_DEPTH {
        return;
    }
    let indent = INDENT.repeat(level);
    match node {
        Node::Text(t) => {
            let t = t.trim();
            if !t.is_empty() {
                out.push_str(&format!("{indent}| {}\n", clean_text(t)));
            }
        }
        Node::Element { tag, attrs, children } => {
            if skipped(tag, attrs) {
                return;
            }
            // 글 없는 아이콘 단추 · 링크: 이름표를 `[ ]` 에 (모듈 설명)
            if interactive(tag, attrs) && !has_visible_content(children) {
                if let Some(label) = fallback_label(attrs, children) {
                    out.push_str(&format!("{indent}{tag} | [{label}]\n"));
                    return;
                }
            }
            let valid: Vec<&Node> = children.iter().filter(|c| has_text_or_kept(c)).collect();
            if valid.is_empty() && !VOID_TAGS.contains(&tag.as_str()) && !PRESERVE_EMPTY.contains(&tag.as_str()) {
                return;
            }
            let meaningful = attrs.iter().any(|(k, _)| MEANINGFUL_ATTRS.contains(&k.as_str()) || k.starts_with("data-"));
            if WRAPPERS.contains(&tag.as_str()) && !meaningful && valid.len() == 1 {
                for c in children {
                    emit(c, level, out);
                }
                return;
            }
            // 입력 칸 · 글 상자의 값은 내지 않는다 (모듈 설명)
            if tag == "input" || tag == "textarea" {
                out.push_str(&format!("{indent}{tag}\n"));
                return;
            }
            if !has_data_bearing_descendant(children) {
                let mut parts = Vec::new();
                all_text(children, &mut parts);
                let joined = parts.join(" ");
                let clean = joined.trim();
                if !clean.is_empty() && !clean.contains('\n') && clean.len() < INLINE_MAX_BYTES {
                    out.push_str(&format!("{indent}{tag} | {}\n", clean_text(clean)));
                    return;
                }
            }
            out.push_str(&format!("{indent}{tag}\n"));
            for c in children {
                emit(c, level + 1, out);
            }
        }
    }
}

/// 요소의 `outerHTML` 을 속성 없는 PUG 로 (logis-center 의 `html_to_semantic_pug`, 모듈 설명의 차이 포함)
pub fn html_to_pug(html: &str) -> String {
    let html = html.trim();
    if html.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for node in parse_fragment(html) {
        emit(&node, 0, &mut out);
    }
    strip_pug_attributes(&sanitize_llm_input(&out))
}

/// HTML 배열(예전 사건의 `action` · `relate`)을 블록마다 PUG 로 바꿔 `---` 줄로 잇는다. 글이 없는 블록은 건너뛴다
pub fn html_array_to_pug(blocks: &[&str], max_blocks: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for b in blocks {
        if used >= max_blocks {
            break;
        }
        let pug = html_to_pug(b);
        if pug.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("---\n");
        }
        out.push_str(&pug);
        used += 1;
    }
    out
}

// ---- 선택 대조 키: 가변 문구 빼기 (시나리오 5) ----

/// 실시간 재고의 머리 낱말 ("남은 수량 3개", "재고 2개 남음", "잔여 3")
const STOCK_LEADS: [&str; 6] = ["남은", "잔여", "재고", "남은수량", "잔여수량", "재고수량"];
/// 재고 수 뒤의 낱말
const STOCK_TAILS: [&str; 7] = ["남음", "남았어요", "남았습니다", "남았음", "남아요", "left", "remaining"];
/// 수 앞의 "단 · 딱 · only"
const ONLY_WORDS: [&str; 4] = ["단", "딱", "only", "just"];
/// 남은 시간 앞뒤의 낱말 ("14:22 마감", "05:12 남음", "남은 시간 05:12", "ends in 05:12"). 시각만 홀로 있으면 빼지 않는다
/// (예약 · 픽업 시간대 "14:00" 은 서로 다른 옵션이다). `in` · `시간` 은 앞말 사이에만 끼는 낱말이다
const TIMER_LEADS: [&str; 9] = ["ends", "ending", "until", "남은", "남은시간", "마감까지", "종료까지", "in", "시간"];
const TIMER_TAILS: [&str; 9] = ["마감", "남음", "남았어요", "후", "후마감", "종료", "까지", "left", "remaining"];
/// 시각 · 수에 붙여 쓴 꼬리 ("14:22마감", "3개남음")
const GLUED_TAILS: [&str; 5] = ["마감", "남음", "까지", "종료", "남았어요"];
/// 구경하는 사람 수 ("12명 구경 중", "지금 12명이 보고 있어요", "12 people viewing")
const VIEW_WORDS: [&str; 12] = ["구경", "구경중", "보는", "보고", "보는중", "관심", "조회", "조회중", "시청", "시청중", "viewing", "watching"];
const VIEW_TAILS: [&str; 7] = ["중", "있어요", "있습니다", "있음", "now", "this", "it"];
/// 할인 수식어: 바로 앞 · 뒤의 금액만 가린다 ("할인가 980원", "타임특가 15%", "15% off"). 수식어 없는 가격 옵션은 그대로다
const PRICE_MODIFIERS: [&str; 12] = ["할인가", "쿠폰가", "특가", "타임특가", "세일가", "할인", "쿠폰할인", "즉시할인", "최대", "sale", "off", "save"];
/// 가변 · 판촉 낱말: 빼고 나서 이런 낱말만 남으면 빼지 않고 숫자만 가린다
const FILLER_WORDS: [&str; 30] = [
    "오늘", "지금", "현재", "마감", "특가", "타임특가", "한정", "한정수량", "임박", "마감임박", "품절임박", "단", "딱", "남음", "종료", "까지", "후", "중",
    "할인", "세일", "only", "just", "left", "today", "now", "deal", "sale", "ends", "in", "hot",
];

/// 낱말 끝의 느낌표 · 마침표 · 쉼표 · 물결 · 쌍점을 뗀다
fn bare(w: &str) -> &str {
    w.trim_end_matches(['!', '.', ',', '~', ':'])
}

/// 붙여 쓴 꼬리를 뗀 몸통과 꼬리가 있었는지
fn untail(w: &str) -> (&str, bool) {
    let w = bare(w);
    match GLUED_TAILS.iter().find_map(|t| w.strip_suffix(t).filter(|b| !b.is_empty())) {
        Some(body) => (body, true),
        None => (w, false),
    }
}

/// 앞의 숫자와 나머지 (숫자로 시작하지 않으면 None)
fn digits_then(w: &str) -> Option<(&str, &str)> {
    let end = w.find(|c: char| !c.is_ascii_digit()).unwrap_or(w.len());
    (end > 0).then(|| w.split_at(end))
}

/// 수량: `3` · `3개` · `3점` · `3ea` · `3pcs` (`2개입` 같은 규격은 아니다)
fn is_count(w: &str) -> bool {
    matches!(digits_then(bare(w)), Some((_, rest)) if ["", "개", "점", "건", "ea", "pcs", "pc"].contains(&rest))
}

/// 시각: `14:22` · `05:12` · `02:13:45` (`1:1 문의` 는 아니다)
fn is_clock(w: &str) -> bool {
    let parts: Vec<&str> = bare(w).split(':').collect();
    (parts.len() == 2 || parts.len() == 3)
        && (1..=2).contains(&parts[0].len())
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && parts[1..].iter().all(|p| p.len() == 2)
}

/// 날짜 셈: `D-3`
fn is_dday(w: &str) -> bool {
    bare(w).strip_prefix("d-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// 남은 기간: `3시간` · `12분` · `2일` · `30초` · `5h` · `10min`
fn is_duration(w: &str) -> bool {
    matches!(digits_then(bare(w)), Some((_, rest)) if ["일", "시간", "분", "초", "h", "m", "s", "d", "hr", "hrs", "min", "mins", "sec", "secs"].contains(&rest))
}

/// 사람 수: `12명` · `12명이`
fn is_people(w: &str) -> bool {
    matches!(digits_then(bare(w)), Some((_, rest)) if ["명", "명이", "명의", "명이서"].contains(&rest))
}

/// 금액: `980원` · `15%` · `₩980` · `$9` · `1,200원` (단위가 있어야 한다. `500ml` · `256gb` 는 규격이다)
fn is_amount(w: &str) -> bool {
    let w = bare(w);
    let (body, prefixed) = match w.strip_prefix('₩').or_else(|| w.strip_prefix('$')) {
        Some(rest) => (rest, true),
        None => (w, false),
    };
    let end = body.find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.')).unwrap_or(body.len());
    let (num, unit) = body.split_at(end);
    num.chars().any(|c| c.is_ascii_digit()) && ((prefixed && unit.is_empty()) || ["원", "%", "₩"].contains(&unit))
}

/// `i` 번째 낱말에서 시작하는 가변 구절의 낱말 수 (없으면 0). `w` 는 소문자 낱말이다
fn volatile_len(w: &[String], i: usize) -> usize {
    let at = |k: usize| w.get(k).map(|s| bare(s));
    let is = |k: usize, list: &[&str]| at(k).is_some_and(|s| list.contains(&s));
    // 실시간 재고: 머리 (+ 수량) + 수 (+ 꼬리)
    if is(i, &STOCK_LEADS) {
        let mut j = i + 1;
        if is(j, &["수량", "수량:"]) {
            j += 1;
        }
        if at(j).is_some_and(is_count) {
            j += 1;
            if is(j, &STOCK_TAILS) {
                j += 1;
            }
            return j - i;
        }
    }
    // 붙여 쓴 재고 ("재고3개", "남은수량3개")
    if let Some(rest) = at(i).and_then(|s| STOCK_LEADS.iter().find_map(|l| s.strip_prefix(l))) {
        if !rest.is_empty() && is_count(rest) {
            return 1 + usize::from(is(i + 1, &STOCK_TAILS));
        }
    }
    // 수 + 꼬리 ("3개 남음", "3개남음", "단 3개 남음", "only 4 left", "4 left in stock", "4 in stock")
    let j0 = if is(i, &ONLY_WORDS) { i + 1 } else { i };
    if let Some((body, glued)) = at(j0).map(untail) {
        if is_count(body) {
            let mut j = j0 + 1;
            let tailed = glued || is(j, &STOCK_TAILS);
            if !glued && tailed {
                j += 1;
            }
            if is(j, &["in"]) && is(j + 1, &["stock"]) {
                return j + 2 - i;
            }
            if tailed {
                return j - i;
            }
        }
    }
    // 남은 시간 ("14:22 마감", "14:22마감", "ends in 05:12", "3시간 12분 남음", "D-3", "02:13:45"). 시각 · 기간만 홀로 있으면
    // (앞말 · 꼬리 없이) 빼지 않는다. 초까지 가는 시각과 D-day 는 늘 남은 시간이다
    let mut j = i;
    let mut led = false;
    while j < i + 3 && is(j, &TIMER_LEADS) {
        led |= !is(j, &["in", "시간"]);
        j += 1;
    }
    let start = j;
    let (body, mut glued) = at(j).map(untail).unwrap_or(("", false));
    let dday = is_dday(body);
    let seconds = is_clock(body) && body.matches(':').count() == 2;
    if dday || is_clock(body) {
        j += 1;
    } else {
        glued = false;
        while let Some((b, g)) = at(j).map(untail) {
            if !is_duration(b) {
                break;
            }
            j += 1;
            if g {
                glued = true;
                break;
            }
        }
    }
    if j > start {
        let body_end = j;
        while is(j, &TIMER_TAILS) {
            j += 1;
        }
        if led || dday || seconds || glued || j > body_end {
            return j - i;
        }
    }
    // 구경하는 사람 수 ("12명 구경 중", "지금 12명이 보고 있어요", "12 people viewing")
    let mut j = if is(i, &["지금", "현재", "now"]) { i + 1 } else { i };
    if at(j).is_some_and(|s| is_people(s) || (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))) {
        j += 1;
        if is(j, &["people", "others", "users", "shoppers", "persons"]) {
            j += 1;
        }
        if is(j, &["are", "is"]) {
            j += 1;
        }
        if is(j, &VIEW_WORDS) {
            j += 1;
            while is(j, &VIEW_TAILS) {
                j += 1;
            }
            return j - i;
        }
    }
    0
}

/// 숫자 묶음마다 `#` 하나로 (`23:59` → `#:#`). `amount` 면 쉼표 · 점이 든 금액 몸통을 통째로 (`1,200원` → `#원`)
fn mask_digits(w: &str, amount: bool) -> String {
    let mut out = String::new();
    let mut in_number = false;
    for c in w.chars() {
        let part = c.is_ascii_digit() || (amount && in_number && (c == ',' || c == '.'));
        if part {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
}

/// 가변 문구를 뺀 선택 대조용 글: 실시간 재고("남은 수량 3개" · "Only 4 left") · 남은 시간("14:22 마감") · 구경하는 사람 수
/// ("12명 구경 중")는 빼고, 할인 수식어 바로 옆의 금액("할인가 980원" · "타임특가 15%")은 `#` 로 가린다. 상품 번호("상품 1") ·
/// 규격("256GB" · "500ml" · "2개입")과 수식어 없는 가격은 그대로 둔다. 빼고 나서 가변 · 판촉 낱말만 남으면(버튼 글 전체가 가변
/// 문구) 빼지 않고 숫자만 가린다: "오늘 23:59 마감 특가" → "오늘 #:# 마감 특가"
pub fn canonical_text(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let mut keep = vec![true; words.len()];
    let mut i = 0;
    while i < words.len() {
        match volatile_len(&lower, i) {
            0 => i += 1,
            n => {
                keep[i..(i + n).min(words.len())].iter_mut().for_each(|k| *k = false);
                i += n;
            }
        }
    }
    let modifier = |k: Option<usize>| k.and_then(|k| lower.get(k)).is_some_and(|s| PRICE_MODIFIERS.contains(&bare(s)));
    let mut out: Vec<String> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let glued = PRICE_MODIFIERS.iter().any(|m| lower[i].strip_prefix(m).is_some_and(|rest| !rest.is_empty() && is_amount(rest)));
        if glued || (is_amount(&lower[i]) && (modifier(i.checked_sub(1)) || modifier(Some(i + 1)))) {
            out.push(mask_digits(w, true));
        } else {
            out.push(w.to_string());
        }
    }
    let content = out.iter().any(|w| w.chars().any(char::is_alphabetic) && !FILLER_WORDS.contains(&bare(&w.to_lowercase())));
    if content {
        out.join(" ")
    } else {
        words.iter().map(|w| mask_digits(w, false)).collect::<Vec<_>>().join(" ")
    }
}

/// 선택 대조의 키 (시나리오 5, [`crate::funnel::DebiasedChoiceBook`] 의 단위)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalChoice {
    /// 요소 해시 (페이지 + 셀렉터 틀. 셀렉터가 없으면 페이지 해시)
    pub element_hash: String,
    /// 요소 해시 + 정규 글([`canonical_text`], 소문자)의 SHA-256 앞 32자. 재고가 줄거나 남은 시간이 흘러도 같고, 같은 틀의
    /// 상품 카드("상품 1" · "상품 2")는 다르다
    pub stable_key: String,
    /// 말풍선 · 리포트에 보일 라벨 (정규 글, 개인정보 거름, 40자까지)
    pub display_label: String,
}

/// PUG 와 요소 해시로 선택 대조 키를 만든다. 예전 키([`crate::funnel::choice_key`]: 요소 해시 + 원래 글)는 그대로 둔다
pub fn canonical_choice_of(element_hash: &str, pug: &str) -> CanonicalChoice {
    let canonical = canonical_text(&text_of(pug));
    let element_hash = element_hash.trim().to_string();
    let stable_key = crate::event::sha256_hex(&format!("choice/2|{}|{}", element_hash.to_ascii_lowercase(), canonical.to_lowercase()))[..32].to_string();
    let display_label: String = crate::event::scrub_text(&canonical).chars().take(LABEL_CHARS).collect();
    CanonicalChoice { element_hash, stable_key, display_label: display_label.trim().to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_parts_match_logis_center() {
        assert_eq!(
            pug_line_parts("    td[alt=\"Qty\" field=\"quantity\"] | 10"),
            (4, "td".to_string(), "[alt=\"Qty\" field=\"quantity\"]".to_string(), "10".to_string())
        );
        // 따옴표 안의 파이프는 구분자가 아니다
        assert_eq!(pug_line_parts("a[href=\"x|y\"] | text").3, "text");
        assert_eq!(pug_line_parts("tr"), (0, "tr".to_string(), String::new(), String::new()));
    }

    #[test]
    fn strip_keeps_tag_and_text_only() {
        let pug = "div[class=\"a\"]\n    option[value=\"우체국|https://x\"] | 우체국\n    | 그냥 글\n\n    img[src=\"a.png\"]\n";
        assert_eq!(strip_pug_attributes(pug), "div\n    option | 우체국\n    | 그냥 글\n    img\n");
        assert_eq!(text_of(pug), "우체국 그냥 글");
    }

    #[test]
    fn sanitize_drops_invisible_and_injection_characters() {
        assert_eq!(sanitize_llm_input("a\u{200B}b\u{202E}c\u{E000}d\te\n<|im_end|>"), "abcd\te\n< |im_end| >");
    }

    #[test]
    fn html_fragments_fold_like_list_mode() {
        assert_eq!(html_to_pug("<button class=\"btn css-1x2y3z\" id=\"pay\">간편결제</button>"), "button | 간편결제\n");
        // 껍데기(div · span)는 자식이 하나면 숨는다, 천 단위 쉼표는 지운다
        assert_eq!(html_to_pug("<div class=\"w\"><span>합계 <b>12,345</b>원</span></div>"), "span | 합계 12345 원\n");
        // 숨은 층 · 스크립트 · 고르지 않은 option 은 빠진다, 입력값은 내지 않는다
        let form = "<div><select name=\"ship\"><option value=\"1\">일반</option><option value=\"2\" selected>빠른배송</option></select>\
                    <input type=\"text\" value=\"홍길동\"><div style=\"position:absolute\">팝업</div><script>x()</script></div>";
        assert_eq!(html_to_pug(form), "div\n    select\n        option | 빠른배송\n    input\n");
        // 글이 없는 요소는 빠진다
        assert_eq!(html_to_pug("<div><span> </span></div>"), "");
        // 글 하나뿐인 p 는 껍데기라 글 줄만 남는다 (logis-center 와 같음)
        assert_eq!(html_to_pug("<p>A &amp; B&nbsp;&#39;C&#x27;</p>"), "| A & B 'C'\n");
    }

    #[test]
    fn arrays_join_blocks_and_skip_empty_ones() {
        let blocks = ["<li>상품 A</li>", "<li> </li>", "<li>상품 B</li>", "<li>상품 C</li>"];
        assert_eq!(html_array_to_pug(&blocks, 2), "li | 상품 A\n---\nli | 상품 B\n");
    }

    #[test]
    fn icon_only_controls_fall_back_to_their_label() {
        assert_eq!(html_to_pug("<button aria-label=\"장바구니\"><svg><path d=\"M0\"/></svg></button>"), "button | [장바구니]\n");
        // 링크도, 숫자 네 개 이상 · 이메일은 거른다 (거른 뒤에 자르므로 긴 번호의 앞자리가 남지 않는다)
        assert_eq!(html_to_pug("<a href=\"/orders\" aria-label=\"주문 20261121 조회\"><svg/></a>"), "a | [주문 # 조회]\n");
        assert_eq!(html_to_pug(&format!("<a aria-label=\"{} 1234567\"><svg/></a>", "가".repeat(38))), format!("a | [{} #]\n", "가".repeat(38)));
        // role 이 단추 · 탭이면 껍데기(div · span)여도, title · 바로 아래 base64 그림의 alt 도 이름표다
        assert_eq!(html_to_pug("<div role=\"button\" aria-label=\"닫기\"><span><svg/></span></div>"), "div | [닫기]\n");
        assert_eq!(html_to_pug("<span role=\"tab\" title=\"리뷰\"></span>"), "span | [리뷰]\n");
        assert_eq!(html_to_pug("<button><img src=\"data:image/png;base64,AAAA\" alt=\"찜하기\"></button>"), "button | [찜하기]\n");
        // 글이 있으면 · 이름표가 없으면 · 보이는 그림이 있으면 예전과 같다
        assert_eq!(html_to_pug("<button aria-label=\"무시\">담기</button>"), "button | 담기\n");
        assert_eq!(html_to_pug("<button><svg/></button>"), "button\n");
        assert_eq!(html_to_pug("<button aria-label=\"장바구니\"><img src=\"cart.png\" alt=\"장바구니\"></button>"), "button\n    img\n");
        // 입력 칸은 이름표 · 값 모두 내지 않는다
        assert_eq!(html_to_pug("<input value=\"홍길동\" aria-label=\"이름\">"), "input\n");
        assert_eq!(html_to_pug("<select aria-label=\"수량\"><option>1</option></select>"), "select\n");
    }

    #[test]
    fn an_open_modal_root_is_not_a_hidden_layer() {
        let drawer = "<div role=\"dialog\" aria-modal=\"true\" style=\"position: fixed; inset: 0\"><h2>장바구니</h2><button>주문하기</button></div>";
        assert_eq!(html_to_pug(drawer), "div\n    h2 | 장바구니\n    button | 주문하기\n");
        assert_eq!(html_to_pug("<dialog open style=\"position:fixed\"><button>결제하기</button></dialog>"), "dialog\n    button | 결제하기\n");
        // 모달이 아닌 고정 층(광고 배너)과 닫힌 dialog 는 예전처럼 빠진다
        assert_eq!(html_to_pug("<div><button>담기</button><div style=\"position:fixed\">광고</div></div>"), "div\n    button | 담기\n");
        assert_eq!(html_to_pug("<dialog style=\"position:fixed\"><button>결제하기</button></dialog>"), "");
    }

    #[test]
    fn volatile_phrases_go_and_product_numbers_stay() {
        for (text, want) in [
            ("키보드 남은 수량 3개", "키보드"),
            ("재고 2개 남음 무선 마우스", "무선 마우스"),
            ("USB 허브 Only 4 left", "USB 허브"),
            ("머그컵 3개남음", "머그컵"),
            ("14:22 마감 머그컵", "머그컵"),
            ("머그컵 14:22마감", "머그컵"),
            ("한정판 남은 시간 02:13:45", "한정판"),
            ("우산 3시간 12분 남음", "우산"),
            ("D-3 우산", "우산"),
            ("니트 12명 구경 중", "니트"),
            ("니트 지금 12명이 보고 있어요", "니트"),
            ("Sweater 12 people viewing", "Sweater"),
            ("양말 할인가 980원", "양말 할인가 #원"),
            ("셔츠 타임특가 15%", "셔츠 타임특가 #%"),
            ("셔츠 15% off", "셔츠 #% off"),
            ("셔츠 쿠폰가500원", "셔츠 쿠폰가#원"),
            // 그대로 남는 것: 상품 번호 · 규격 · 수식어 없는 가격 · 홀로 있는 시각(예약 시간대) · 배송 기간
            ("상품 1 무료배송", "상품 1 무료배송"),
            ("256GB", "256GB"),
            ("생수 500ml 2개입", "생수 500ml 2개입"),
            ("500원", "500원"),
            ("픽업 14:00", "픽업 14:00"),
            ("3일 배송", "3일 배송"),
            ("1:1 문의", "1:1 문의"),
            // 글 전체가 가변 문구면 빼지 않고 숫자만 가린다 (빈 키가 되지 않게)
            ("오늘 23:59 마감 특가", "오늘 #:# 마감 특가"),
            ("남은 수량 3개", "남은 수량 #개"),
            ("12명 구경 중", "#명 구경 중"),
        ] {
            assert_eq!(canonical_text(text), want, "{text}");
        }
    }

    #[test]
    fn canonical_choices_survive_stock_updates_and_keep_cards_apart() {
        let el = "0xelement";
        let a3 = canonical_choice_of(el, "li\n    span | 키보드\n    span | 남은 수량 3개\n");
        let a1 = canonical_choice_of(el, "li\n    span | 키보드\n    span | 남은 수량 1개\n");
        assert_eq!(a3.stable_key, a1.stable_key);
        assert_eq!(a3.display_label, "키보드");
        assert_eq!(a3.stable_key.len(), 32);
        let p1 = canonical_choice_of(el, "li | 상품 1 무료배송\n");
        let p2 = canonical_choice_of(el, "li | 상품 2 무료배송\n");
        assert_ne!(p1.stable_key, p2.stable_key);
        // 글이 같아도 요소(셀렉터 틀)가 다르면 다른 후보
        assert_ne!(canonical_choice_of("0xother", "li | 상품 1 무료배송\n").stable_key, p1.stable_key);
        // 라벨: 개인정보 거름, 40자
        let long = canonical_choice_of(el, &format!("button | 주문 20261121 {}\n", "가".repeat(60)));
        assert!(long.display_label.starts_with("주문 # "), "{}", long.display_label);
        assert_eq!(long.display_label.chars().count(), 40);
    }
}
