//! 원본 `analytic.ts` 89~200 · 257~278줄: 주소 · 글 다듬기(cleanHref · hostOf · scrubText), 받은 사건 검사(sanitizeEvent),
//! 첫 화면의 meta(siteMetaOwners), 사이트 호스트(normalizeHost). 그리고 저장할 사건의 `JSON.stringify` (원본은 객체를 만들어 encodeBlob).
//!
//! JS 문자열은 UTF-16 이다. `s.slice(0, n)` 이 서로게이트 쌍의 가운데를 자르면 짝 없는 high surrogate 가 끝에 남고,
//! `JSON.stringify` 는 그것을 `\udXXX` 로 쓴다. Rust 문자열은 그것을 담지 못해 [`JStr`] 가 따로 들고, [`event_json`] 이 같은 글로 쓴다.
//! 받은 JSON 의 짝 없는 `\uD800` 같은 이스케이프는 담지 못해 U+FFFD 가 된다 (docs/workers-rs.md 의 알려진 차이와 같다).

use std::sync::OnceLock;

use premium_discount_worker_common::jsv::{js_trim, num_to_string, quote, to_lower, u16_len, V};
use regex::Regex;

/// `EVENT_SCHEMA`
pub const EVENT_SCHEMA: &str = "web-event/1";
/// `SITE_META_NAME`
pub const SITE_META_NAME: &str = "premium-discount-site";
/// `EVENT_TYPES`
pub const EVENT_TYPES: [&str; 8] = ["enter", "click", "hover", "change", "scroll", "submit", "back", "leave"];
const MAX_PUG_CHARS: usize = 4000;
const MAX_RELATED: usize = 100;
const MAX_CHAIN: usize = 5;
const MAX_SID: usize = 64;

/// JS 문자열 조각: `s` 뒤에 짝 없는 high surrogate(`lone`)가 하나 붙을 수 있다 (`slice` 가 쌍을 가른 것)
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct JStr {
    pub s: String,
    pub lone: Option<u16>,
}

impl JStr {
    /// `x.trim() === ''` (짝 없는 서로게이트는 공백이 아니다)
    pub fn trims_empty(&self) -> bool {
        self.lone.is_none() && js_trim(&self.s).is_empty()
    }

    fn write(&self, out: &mut String) {
        let q = quote(&self.s);
        match self.lone {
            None => out.push_str(&q),
            Some(u) => {
                out.push_str(&q[..q.len() - 1]);
                out.push_str(&format!("\\u{u:04x}\""));
            }
        }
    }
}

/// `s.slice(0, max)` (UTF-16 단위)
pub fn slice_prefix(s: &str, max: usize) -> JStr {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        let n = c.len_utf16();
        if units + n > max {
            let lone = if n == 2 && units + 1 == max {
                let mut buf = [0u16; 2];
                Some(c.encode_utf16(&mut buf)[0])
            } else {
                None
            };
            return JStr { s: s[..i].to_string(), lone };
        }
        units += n;
    }
    JStr { s: s.to_string(), lone: None }
}

/// `cleanString(v, max)` — 비지 않은 글이면 앞 max 글자 (UTF-16)
fn clean_string(v: &V, max: usize) -> Option<JStr> {
    match v {
        V::Str(s) if !s.is_empty() => Some(slice_prefix(s, max)),
        _ => None,
    }
}

/// `cleanHref(href)` — 주소에서 쿼리와 문서 안 위치를 뺀다. 해시 라우팅(#/cart, #!/cart)은 남긴다. http(s) 가 아니면 None
pub fn clean_href(href: &str) -> Option<String> {
    let h = js_trim(href);
    let sep = h.find("://")?;
    if sep == 0 {
        return None;
    }
    let scheme = to_lower(&h[..sep]);
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let rest = &h[sep + 3..];
    let fragment = rest.find('#').map(|i| &rest[i + 1..]);
    let route = fragment.filter(|f| f.starts_with('/') || f.starts_with("!/"));
    let base = match rest.find(['?', '#']) {
        Some(cut) => &rest[..cut],
        None => rest,
    };
    if base.split('/').next().unwrap_or("").is_empty() {
        return None;
    }
    if let Some(route) = route {
        let r = match route.find('?') {
            Some(q) => &route[..q],
            None => route,
        };
        return Some(format!("{scheme}://{base}#{r}"));
    }
    Some(format!("{scheme}://{base}"))
}

/// `hostOf(href)` — 주소의 호스트 (소문자, 사용자 정보 · 포트 없이)
pub fn host_of(href: &str) -> Option<String> {
    let sep = href.find("://")?;
    if sep == 0 {
        return None;
    }
    let authority = href[sep + 3..].split(['/', '?', '#']).next().unwrap_or("");
    let last = authority.split('@').next_back().unwrap_or("");
    let host = to_lower(last.split(':').next().unwrap_or(""));
    (!host.is_empty()).then_some(host)
}

/// `looksPrivate(word)` — 이메일(@ 뒤에 .) · 숫자(ASCII) 네 개 이상
fn looks_private(word: &str) -> bool {
    let email = matches!(word.find('@'), Some(at) if at > 0 && word[at + 1..].contains('.'));
    email || word.bytes().filter(u8::is_ascii_digit).count() >= 4
}

/// `scrubText(text)` — 개인정보로 보이는 낱말을 '#' 로. 줄 구조 · 들여쓰기(앞 공백)는 그대로
pub fn scrub_text(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let n = line.len() - line.trim_start_matches(' ').len();
            let words: Vec<&str> = line[n..].split(' ').map(|w| if looks_private(w) { "#" } else { w }).collect();
            format!("{}{}", &line[..n], words.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("정규식"))
}

/// `DomNode` (키 차례: tag, id, classes, attrs, nth_of_type)
#[derive(Clone, Debug, PartialEq)]
pub struct DomNode {
    pub tag: String,
    pub id: Option<JStr>,
    pub classes: Option<Vec<String>>,
    pub attrs: Option<Vec<(JStr, JStr)>>,
    pub nth_of_type: Option<f64>,
}

/// `WebElement` (chain, pug)
#[derive(Clone, Debug, PartialEq)]
pub struct WebElement {
    pub chain: Vec<DomNode>,
    pub pug: JStr,
}

/// `WebEvent` — 키 차례는 sanitizeEvent 가 넣는 차례 그대로
#[derive(Clone, Debug, PartialEq)]
pub struct WebEvent {
    pub typ: String,
    pub href: String,
    pub sid: String,
    pub seq: f64,
    pub t_ms: f64,
    pub target: Option<WebElement>,
    pub item: Option<WebElement>,
    pub related: Option<Vec<WebElement>>,
    pub rage: bool,
    pub dwell_ms: Option<f64>,
    pub flips: Option<f64>,
    pub field: Option<String>,
    pub dirty: Option<f64>,
}

/// `!v || typeof v !== 'object'` 의 반대 (배열도 객체다)
fn is_object(v: &V) -> bool {
    v.truthy() && v.type_of() == "object"
}

/// `cleanNode(v)`
fn clean_node(v: &V) -> Option<DomNode> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    if !is_object(v) {
        return None;
    }
    let tag = clean_string(&v.get("tag"), 40)?;
    // /^[a-zA-Z0-9-]+$/ — 잘린 서로게이트가 남았으면 맞지 않는다
    if tag.lone.is_some() || !re(&TAG, r"^[a-zA-Z0-9-]+$").is_match(&tag.s) {
        return None;
    }
    let mut out = DomNode { tag: tag.s.to_ascii_lowercase(), id: None, classes: None, attrs: None, nth_of_type: None };
    out.id = clean_string(&v.get("id"), 80);
    if let classes @ V::Arr(_) = v.get("classes") {
        let list: Vec<String> = classes
            .items()
            .into_iter()
            .filter_map(|c| match c {
                V::Str(s) if u16_len(&s) <= 80 => Some(s),
                _ => None,
            })
            .take(8)
            .collect();
        out.classes = Some(list);
    }
    if let attrs @ V::Arr(_) = v.get("attrs") {
        let list: Vec<(JStr, JStr)> = attrs
            .items()
            .into_iter()
            .filter_map(|a| match (&a, a.at(0), a.at(1)) {
                (V::Arr(_), V::Str(k), V::Str(val)) => Some((slice_prefix(&k, 40), slice_prefix(&val, 80))),
                _ => None,
            })
            .take(6)
            .collect();
        out.attrs = Some(list);
    }
    if let V::Num(n) = v.get("nth_of_type") {
        if n.is_finite() && n.trunc() == n && n > 0.0 {
            out.nth_of_type = Some(n.min(10_000.0));
        }
    }
    Some(out)
}

/// `cleanElement(v)`
fn clean_element(v: &V) -> Option<WebElement> {
    if !is_object(v) {
        return None;
    }
    let chain = match v.get("chain") {
        c @ V::Arr(_) => c.items().iter().take(MAX_CHAIN).filter_map(clean_node).collect(),
        _ => Vec::new(),
    };
    let pug = match v.get("pug") {
        V::Str(s) => slice_prefix(&scrub_text(&s), MAX_PUG_CHARS),
        _ => JStr::default(),
    };
    if chain.is_empty() && pug.trims_empty() {
        return None;
    }
    Some(WebElement { chain, pug })
}

/// `count(v, max)` — 0 이상의 유한한 수면 `Math.min(Math.floor(v), max)` (−0 은 −0 으로 남는다)
fn count(v: &V, max: f64) -> Option<f64> {
    match v {
        V::Num(n) if n.is_finite() && *n >= 0.0 => Some(n.floor().min(max)),
        _ => None,
    }
}

/// `sanitizeEvent(raw)` — 받은 사건 하나를 검사하고 다듬는다. 모양이 틀리면 None
pub fn sanitize_event(raw: &V) -> Option<WebEvent> {
    static FIELD: OnceLock<Regex> = OnceLock::new();
    if !is_object(raw) || raw.is_arr() {
        return None;
    }
    let typ = match raw.get("type") {
        V::Str(t) if EVENT_TYPES.contains(&t.as_str()) => t,
        _ => return None,
    };
    // String(e.href ?? '')
    let href_raw = raw.get("href");
    let href = clean_href(&if href_raw.is_nullish() { String::new() } else { href_raw.to_str() });
    let sid = clean_string(&raw.get("sid"), MAX_SID);
    let (Some(href), Some(sid)) = (href, sid) else { return None };
    // 고침(fix-analytic-sid-surrogate): sid 를 64 글자로 자른 곳이 서로게이트 쌍의 가운데면 원본은 짝 없는 서로게이트를 hashId(cc + sid)
    // (ethers toUtf8Bytes)에 넘겨 TypeError("invalid surrogate pair") → 500 이었다. 그런 sid 는 사건 모양이 틀린 것으로 본다
    if sid.lone.is_some() {
        return None;
    }
    // 고침(fix-analytic-empty-host): 주소의 호스트가 비면("https://a@/" · "https://:80/") 원본은 Origin 이 없을 때 hashId(null) 이
    // TypeError → 500 이었다(Origin 이 있으면 403). 호스트 없는 주소는 사건 모양이 틀린 것으로 본다
    host_of(&href)?;
    let mut out = WebEvent {
        typ,
        href,
        sid: sid.s,
        seq: count(&raw.get("seq"), 1e9).unwrap_or(0.0),
        t_ms: count(&raw.get("t_ms"), 864e5).unwrap_or(0.0),
        target: clean_element(&raw.get("target")),
        // 누른 요소가 속한 목록 항목 (형제와 같은 단위)
        item: clean_element(&raw.get("item")),
        related: None,
        rage: false,
        dwell_ms: None,
        flips: None,
        field: None,
        dirty: None,
    };
    if let related @ V::Arr(_) = raw.get("related") {
        let list: Vec<WebElement> = related.items().iter().take(MAX_RELATED).filter_map(clean_element).collect();
        if !list.is_empty() {
            out.related = Some(list);
        }
    }
    out.rage = matches!(raw.get("rage"), V::Bool(true));
    out.dwell_ms = count(&raw.get("dwell_ms"), 3_600_000.0);
    out.flips = count(&raw.get("flips"), 1000.0);
    if let Some(field) = clean_string(&raw.get("field"), 20) {
        if field.lone.is_none() && re(&FIELD, r"^[a-z-]+$").is_match(&field.s) {
            out.field = Some(field.s);
        }
    }
    out.dirty = count(&raw.get("dirty"), 1000.0);
    Some(out)
}

fn write_num(out: &mut String, n: f64) {
    out.push_str(&num_to_string(n));
}

fn write_node(out: &mut String, n: &DomNode) {
    out.push_str("{\"tag\":");
    out.push_str(&quote(&n.tag));
    if let Some(id) = &n.id {
        out.push_str(",\"id\":");
        id.write(out);
    }
    if let Some(classes) = &n.classes {
        out.push_str(",\"classes\":[");
        out.push_str(&classes.iter().map(|c| quote(c)).collect::<Vec<_>>().join(","));
        out.push(']');
    }
    if let Some(attrs) = &n.attrs {
        out.push_str(",\"attrs\":[");
        for (i, (k, v)) in attrs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('[');
            k.write(out);
            out.push(',');
            v.write(out);
            out.push(']');
        }
        out.push(']');
    }
    if let Some(nth) = n.nth_of_type {
        out.push_str(",\"nth_of_type\":");
        write_num(out, nth);
    }
    out.push('}');
}

fn write_element(out: &mut String, e: &WebElement) {
    out.push_str("{\"chain\":[");
    for (i, n) in e.chain.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_node(out, n);
    }
    out.push_str("],\"pug\":");
    e.pug.write(out);
    out.push('}');
}

/// `JSON.stringify({ schema: EVENT_SCHEMA, ...data })` — data 는 사건에서 sid 를 뺀 것 (`const { sid, ...data } = e`)
pub fn event_json(e: &WebEvent) -> String {
    let mut out = String::from("{\"schema\":");
    out.push_str(&quote(EVENT_SCHEMA));
    out.push_str(",\"type\":");
    out.push_str(&quote(&e.typ));
    out.push_str(",\"href\":");
    out.push_str(&quote(&e.href));
    out.push_str(",\"seq\":");
    write_num(&mut out, e.seq);
    out.push_str(",\"t_ms\":");
    write_num(&mut out, e.t_ms);
    if let Some(t) = &e.target {
        out.push_str(",\"target\":");
        write_element(&mut out, t);
    }
    if let Some(t) = &e.item {
        out.push_str(",\"item\":");
        write_element(&mut out, t);
    }
    if let Some(list) = &e.related {
        out.push_str(",\"related\":[");
        for (i, t) in list.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_element(&mut out, t);
        }
        out.push(']');
    }
    if e.rage {
        out.push_str(",\"rage\":true");
    }
    for (name, v) in [("dwell_ms", e.dwell_ms), ("flips", e.flips)] {
        if let Some(n) = v {
            out.push_str(&format!(",\"{name}\":"));
            write_num(&mut out, n);
        }
    }
    if let Some(f) = &e.field {
        out.push_str(",\"field\":");
        out.push_str(&quote(f));
    }
    if let Some(n) = e.dirty {
        out.push_str(",\"dirty\":");
        write_num(&mut out, n);
    }
    out.push('}');
    out
}

/// JS 정규식의 `\s` (WhiteSpace + LineTerminator — Rust 의 `\s` 와 달리 U+FEFF 를 넣고 U+0085 는 뺀다)
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// 유니코드 플래그 없는 JS 정규식의 `\b` (낱말 글자는 [A-Za-z0-9_] 만)
fn ascii_word(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 패턴 `pat` 이 `\b` 로 시작할 때, 맞은 곳 앞뒤가 JS 의 `\b` 를 만족하는 첫 맞음 (Rust 의 `\b` 는 유니코드 낱말을 본다)
fn first_at_word_boundary<'h>(re: &Regex, hay: &'h str) -> Option<regex::Captures<'h>> {
    let mut from = 0;
    while from <= hay.len() {
        let caps = re.captures_at(hay, from)?;
        let start = caps.get(0).map_or(0, |m| m.start());
        let before = hay[..start].chars().next_back();
        let first = hay[start..].chars().next();
        if ascii_word(before) != ascii_word(first) {
            return Some(caps);
        }
        from = start + first.map_or(1, char::len_utf8);
    }
    None
}

/// `/^0x[0-9a-f]{40}$/`
fn is_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `siteMetaOwners(html)` — 사이트 첫 화면의 `<meta name="premium-discount-site" content="…">` 값들 (쉼표로 여럿)
pub fn site_meta_owners(html: &str) -> Vec<String> {
    static META: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    static CONTENT: OnceLock<Regex> = OnceLock::new();
    // /<meta\b[^>]*>/gi — 글자 대소문자는 ASCII 만 (유니코드 플래그 없는 JS 의 /i 는 ASCII 와 비ASCII 를 섞어 맞추지 않는다)
    let meta = re(&META, r"<[mM][eE][tT][aA][^>]*>");
    let name = re(&NAME, &format!(r#"[nN][aA][mM][eE][{JS_SPACE}]*=[{JS_SPACE}]*["']?([^"'{JS_SPACE}>]+)"#));
    let content = re(&CONTENT, &format!(r#"[cC][oO][nN][tT][eE][nN][tT][{JS_SPACE}]*=[{JS_SPACE}]*(?:"([^"]*)"|'([^']*)'|([^{JS_SPACE}>]+))"#));
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(m) = meta.find_at(html, from) {
        from = m.end().max(m.start() + 1);
        // <meta 뒤의 \b: 다음 글자가 낱말 글자가 아니어야 한다 (a 는 낱말 글자)
        if ascii_word(html[m.start() + 5..].chars().next()) {
            // 이 자리에서는 맞지 않는다: [^>]* 를 줄여도 \b 가 그대로이므로 다음 '<' 부터 찾는다
            from = m.start() + 1;
            continue;
        }
        let tag = m.as_str();
        let Some(n) = first_at_word_boundary(name, tag) else { continue };
        if to_lower(n.get(1).map_or("", |x| x.as_str())) != SITE_META_NAME {
            continue;
        }
        let value = first_at_word_boundary(content, tag)
            .map(|c| c.get(1).or(c.get(2)).or(c.get(3)).map_or(String::new(), |x| x.as_str().to_string()))
            .unwrap_or_default();
        for a in value.split(',') {
            let addr = to_lower(js_trim(a));
            if is_address(&addr) {
                out.push(addr);
            }
        }
    }
    out
}

/// `normalizeHost(raw)` — 글이 아니면 None. 주소면 호스트, 아니면 `https://` 를 붙여 호스트. `[a-z0-9.-]` 이고 '.' 가 있어야 한다
pub fn normalize_host(raw: &V) -> Option<String> {
    let V::Str(s) = raw else { return None };
    let t = js_trim(s);
    let h = if s.contains("://") { host_of(t) } else { host_of(&format!("https://{t}")) }?;
    (h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-') && h.contains('.')).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::jsv::parse_str;

    #[test]
    fn href_host_and_scrub_follow_the_test_vectors() {
        // analytic.test.ts '주소와 글을 다듬는다'
        assert_eq!(clean_href("HTTPS://Shop.example.com/cart?coupon=A1&email=x@y.z#top").unwrap(), "https://Shop.example.com/cart");
        assert_eq!(clean_href("https://app.example.com/#/orders/12?tab=2").unwrap(), "https://app.example.com/#/orders/12");
        assert_eq!(clean_href("https://app.example.com/#!/cart").unwrap(), "https://app.example.com/#!/cart");
        assert_eq!(clean_href("javascript:alert(1)"), None);
        assert_eq!(clean_href("https:///nohost"), None);
        assert_eq!(clean_href("://x"), None);
        assert_eq!(host_of("https://user:pw@Shop.Example.com:8443/a").unwrap(), "shop.example.com");
        assert_eq!(host_of("https://a@/x"), None);
        assert_eq!(
            scrub_text("div\n  span | 홍길동 010-1234-5678 hong@example.com\n  button | 결제하기 2026"),
            "div\n  span | 홍길동 # #\n  button | 결제하기 #"
        );
        assert_eq!(scrub_text("@a.b  x"), "@a.b  x");
    }

    #[test]
    fn slices_can_split_a_surrogate_pair() {
        let s = format!("{}😀", "a".repeat(79));
        let j = slice_prefix(&s, 80);
        assert_eq!(j.s, "a".repeat(79));
        assert_eq!(j.lone, Some(0xd83d));
        let mut out = String::new();
        j.write(&mut out);
        assert!(out.ends_with("a\\ud83d\""));
        assert_eq!(slice_prefix("가😀", 3), JStr { s: "가😀".into(), lone: None });
    }

    #[test]
    fn sanitize_keeps_the_order_and_drops_unknown_fields() {
        let raw = parse_str(
            r#"{"type":"click","href":"https://shop.example.com/checkout?x=1","sid":"s-1","seq":3.7,"t_ms":-1,"value":"비밀","rage":"yes",
            "target":{"chain":[{"tag":"BUTTON","id":"pay","classes":["btn",1],"attrs":[["data-x","y"],"bad"],"nth_of_type":2}],"pug":"button | 010-1234-5678"},
            "related":[{"pug":"  "},{"pug":"li | a"}],"field":"Text","dirty":2000}"#,
        )
        .unwrap();
        let e = sanitize_event(&raw).unwrap();
        assert_eq!(
            event_json(&e),
            r#"{"schema":"web-event/1","type":"click","href":"https://shop.example.com/checkout","seq":3,"t_ms":0,"target":{"chain":[{"tag":"button","id":"pay","classes":["btn"],"attrs":[["data-x","y"]],"nth_of_type":2}],"pug":"button | #"},"related":[{"chain":[],"pug":"li | a"}],"dirty":1000}"#
        );
        assert!(sanitize_event(&parse_str(r#"{"type":"keylog","href":"https://a.b","sid":"s"}"#).unwrap()).is_none());
        assert!(sanitize_event(&parse_str(r#"[{"type":"click","href":"https://a.b","sid":"s"}]"#).unwrap()).is_none());
        assert!(sanitize_event(&parse_str(r#"{"type":"click","href":"https://:80/","sid":"s"}"#).unwrap()).is_none());
        let long_sid = format!(r#"{{"type":"click","href":"https://a.b","sid":"{}😀"}}"#, "s".repeat(63));
        assert!(sanitize_event(&parse_str(&long_sid).unwrap()).is_none());
    }

    #[test]
    fn meta_owners() {
        let a = format!("0x{}", "1".repeat(40));
        let b = format!("0x{}", "a".repeat(40));
        let html = format!(r#"<head><META content='{a}, {b}' Name="premium-discount-site"><meta name="x" content="{a}"></head>"#);
        assert_eq!(site_meta_owners(&html), vec![a.clone(), b.clone()]);
        // data-name 의 name 앞은 '-' 라 \b 가 맞는다 (원본 그대로)
        assert_eq!(site_meta_owners(&format!(r#"<meta data-name=premium-discount-site content={a}>"#)), vec![a.clone()]);
        // <metax 는 meta 가 아니다 · qname 은 \b 가 아니다
        assert!(site_meta_owners(&format!(r#"<metax name="premium-discount-site" content="{a}">"#)).is_empty());
        assert!(site_meta_owners(&format!(r#"<meta qname="premium-discount-site" content="{a}">"#)).is_empty());
        assert_eq!(normalize_host(&V::from(" https://Shop.Example.com/any ")).unwrap(), "shop.example.com");
        assert_eq!(normalize_host(&V::from("not a host")), None);
        assert_eq!(normalize_host(&V::Num(1.0)), None);
    }
}
