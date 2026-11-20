//! 받은 본문의 `JSON.parse(text)` — V8 와 같은 문법 · 같은 값.
//!
//! common 의 `jsv::parse_str`(serde_json)를 쓰지 않는 까닭: 수집 본문은 아무나 보낸다. serde_json 은 깊이 128 을 넘는 중첩과
//! `1e400` 같은 범위 밖의 수를 오류로 보지만, V8 의 JSON.parse 는 깊이 제한이 없고(1000만 단계도 받는다) 범위 밖의 수는 ±Infinity 다.
//! 그 차이는 원본의 400 "every event must be web-event/1"(또는 저장)과 Rust 의 400 "invalid JSON body" 로 갈린다.
//!
//! - 반복문으로 읽어(재귀 없음) 아무리 깊어도 스택이 넘치지 않는다. 값은 깊이 [`MAX_DEPTH`] 까지만 만들고 그보다 깊은 배열 · 객체는
//!   같은 종류의 빈 것으로 둔다 (V 의 해제 · `String(x)` 가 재귀라서). sanitizeEvent 가 들여다보는 깊이는 10 남짓이고, 더 깊이 보는 것은
//!   `String(e.href)` 가 중첩 배열을 join 하는 경우뿐이다 (알려진 차이: 512 단계보다 깊은 배열 주소).
//! - 수는 JSON 문법을 확인한 뒤 Rust 의 f64 읽기(올바른 반올림, 넘치면 ±inf, "-0" 은 −0)로 — V8 와 같은 비트다.
//! - 같은 열쇠가 두 번이면 처음 자리에 마지막 값 (V 의 put = IndexMap insert, JS 의 CreateDataProperty 와 같다).
//! - 짝 없는 서로게이트 이스케이프(`"\uD800"`)는 Rust 문자열이 담지 못해 U+FFFD 다 (docs/workers-rs.md 의 알려진 차이).

use premium_discount_worker_common::jsv::V;

/// 값을 만드는 깊이 (이보다 깊은 배열 · 객체는 빈 것)
pub const MAX_DEPTH: usize = 512;

enum Frame {
    Arr { v: Option<V> },
    Obj { v: Option<V>, key: String },
}

impl Frame {
    /// 다 읽은 배열 · 객체의 값: 만들지 않은 깊이면 같은 종류의 빈 것
    fn finish(self) -> V {
        match self {
            Frame::Arr { v } => v.unwrap_or_else(V::new_arr),
            Frame::Obj { v, .. } => v.unwrap_or_else(V::new_obj),
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    s: &'a str,
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while let Some(c) = self.b.get(self.i) {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.i += 1)
    }

    fn lit(&mut self, word: &str) -> Option<()> {
        self.b.get(self.i..self.i + word.len()).filter(|w| *w == word.as_bytes())?;
        self.i += word.len();
        Some(())
    }

    fn hex4(&mut self) -> Option<u16> {
        let h = self.s.get(self.i..self.i + 4)?;
        if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        self.i += 4;
        u16::from_str_radix(h, 16).ok()
    }

    /// 글 (여는 따옴표에서)
    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            // 원문이 &str 이고 멈춘 곳은 ASCII 라 글자 경계다
            out.push_str(&self.s[start..self.i]);
            match self.peek()? {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) && self.b.get(self.i..self.i + 2) == Some(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                                        let c = 0x10000 + (((u as u32) - 0xD800) << 10) + ((lo as u32) - 0xDC00);
                                        out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                        continue;
                                    }
                                    _ => self.i = save,
                                }
                            }
                            // 짝 없는 서로게이트는 담지 못한다 (알려진 차이)
                            out.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
                        }
                        _ => return None,
                    }
                }
                // 이스케이프하지 않은 제어 문자
                _ => return None,
            }
        }
    }

    /// 수: -?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?
    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            p.i - s
        };
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => {
                digits(self);
            }
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return None;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return None;
            }
        }
        self.s[start..self.i].parse::<f64>().ok()
    }
}

/// `JSON.parse(text)` — 문법 오류면 None (원본의 SyntaxError)
pub fn parse(text: &str) -> Option<V> {
    let mut p = P { b: text.as_bytes(), s: text, i: 0 };
    let mut stack: Vec<Frame> = Vec::new();
    'value: loop {
        p.ws();
        let depth = stack.len() + 1;
        let mut v = match p.peek()? {
            b'{' => {
                p.i += 1;
                p.ws();
                let obj = (depth <= MAX_DEPTH).then(V::new_obj);
                if p.eat(b'}').is_some() {
                    obj.unwrap_or_else(V::new_obj)
                } else {
                    let key = p.string()?;
                    p.ws();
                    p.eat(b':')?;
                    stack.push(Frame::Obj { v: obj, key });
                    continue 'value;
                }
            }
            b'[' => {
                p.i += 1;
                p.ws();
                let arr = (depth <= MAX_DEPTH).then(V::new_arr);
                if p.eat(b']').is_some() {
                    arr.unwrap_or_else(V::new_arr)
                } else {
                    stack.push(Frame::Arr { v: arr });
                    continue 'value;
                }
            }
            b'"' => V::Str(p.string()?),
            b't' => p.lit("true").map(|_| V::Bool(true))?,
            b'f' => p.lit("false").map(|_| V::Bool(false))?,
            b'n' => p.lit("null").map(|_| V::Null)?,
            _ => V::Num(p.number()?),
        };
        // 다 읽은 값 v 를 부모에 넣고, 부모가 끝나면 그 부모에 — 다음 값을 읽을 자리까지
        loop {
            match stack.last_mut() {
                None => {
                    p.ws();
                    return (p.i == p.b.len()).then_some(v);
                }
                Some(Frame::Arr { v: arr }) => {
                    if let Some(a) = arr {
                        let _ = a.push(v);
                    }
                    p.ws();
                    match p.peek()? {
                        b',' => {
                            p.i += 1;
                            continue 'value;
                        }
                        b']' => {
                            p.i += 1;
                            v = stack.pop()?.finish();
                        }
                        _ => return None,
                    }
                }
                Some(Frame::Obj { v: obj, key }) => {
                    if let Some(o) = obj {
                        o.put(key, v);
                    }
                    p.ws();
                    match p.peek()? {
                        b',' => {
                            p.i += 1;
                            p.ws();
                            *key = p.string()?;
                            p.ws();
                            p.eat(b':')?;
                            continue 'value;
                        }
                        b'}' => {
                            p.i += 1;
                            v = stack.pop()?.finish();
                        }
                        _ => return None,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::jsv::stringify;

    fn js(s: &str) -> Option<String> {
        parse(s).map(|v| stringify(&v).unwrap_or_default())
    }

    #[test]
    fn grammar_like_json_parse() {
        assert_eq!(js(r#" {"z":1,"a":[1,2.5,"x\n\u00e9\ud83d\ude00"],"z":3} "#).unwrap(), r#"{"z":3,"a":[1,2.5,"x\né😀"]}"#);
        assert_eq!(js(r#"{"2":1,"b":2,"1":3}"#).unwrap(), r#"{"1":3,"2":1,"b":2}"#);
        for bad in ["", " ", "{", "[1,]", "{\"a\":1,}", "01", "1.", ".5", "+1", "1e", "'a'", "\"\t\"", "[1] x", "tru", "{\"a\" 1}", "\"\\x\"", "NaN"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        assert_eq!(parse("1e400").unwrap().as_num(), Some(f64::INFINITY));
        assert!(parse("-0").unwrap().as_num().unwrap().is_sign_negative());
        assert_eq!(js("\"\\ud800x\"").unwrap(), "\"\u{FFFD}x\"");
        assert_eq!(js("\"\\ud800\\u0041\"").unwrap(), "\"\u{FFFD}A\"");
    }

    #[test]
    fn deep_nesting_is_fine_and_stubbed() {
        let n = 200_000;
        let v = parse(&format!("{}{}", "[".repeat(n), "]".repeat(n))).unwrap();
        assert_eq!(v.len(), 1);
        let deep = format!("{}1{}", "[".repeat(MAX_DEPTH + 3), "]".repeat(MAX_DEPTH + 3));
        let mut x = parse(&deep).unwrap();
        for _ in 0..MAX_DEPTH - 1 {
            x = x.at(0);
        }
        assert!(x.is_arr() && x.len() == 1);
        assert!(x.at(0).is_arr() && x.at(0).is_empty());
        assert!(parse(&format!("{}{}", "[".repeat(n), "]".repeat(n - 1))).is_none());
    }
}
