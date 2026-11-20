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
        if s.contains("position") && (s.contains("absolute") || s.contains("fixed")) {
            return true;
        }
    }
    if tag == "img" && attr(attrs, "src").is_some_and(|s| s.contains("base64")) {
        return true;
    }
    tag == "option" && attr(attrs, "selected").is_none()
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
}
