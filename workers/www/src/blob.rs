//! 원본 `blob.ts` — 사건 BLOB 코덱: D1 의 data 칸(gzip 한 JSON)을 어떤 모양으로 받아도 같은 객체로 푼다.
//!
//! 쓰는 곳은 analytic(2단계)이다. 같은 규칙의 기기 쪽 Rust 판은 crates/premium-discount-analytic/src/blob.rs 이고(그 crate 는 wasm 에서
//! 쓰지 않는 의존성을 끌어와 여기서는 쓰지 않는다), 셋은 contracts/analytic_blob_v1.json 을 함께 통과한다 (`tests/contracts.rs`).
//!
//! 값의 모양 (원본의 instanceof 갈래 → V):
//! - `Uint8Array` · `ArrayBuffer` · 다른 TypedArray 의 바이트 → `V::Bytes` (js::to_v 가 Uint8Array · ArrayBuffer 를 그렇게 둔다)
//! - D1 이 돌려주는 BLOB(숫자 배열) · 배열 → `V::Arr`
//! - 객체 → `V::Obj`, 글 → `V::Str`, 그 밖 → `{}`

use premium_discount_worker_common::gzip;
use premium_discount_worker_common::jsv::{parse_str, V};

/// 푼 결과의 크기 상한 (압축 폭탄 막기)
pub const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;

fn is_record(v: &V) -> bool {
    matches!(v, V::Obj(_))
}

fn object_or_empty(v: V) -> V {
    if is_record(&v) {
        v
    } else {
        V::new_obj()
    }
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

/// `decodeBytes(bytes)` — 바이트(gzip 이거나 UTF-8 JSON)를 객체로. 풀 수 없거나 객체가 아니면 {}
pub fn decode_bytes(bytes: &[u8]) -> V {
    let text: Vec<u8> = if is_gzip(bytes) {
        match gzip::ungzip(&V::bytes(bytes.to_vec())) {
            Ok(t) if t.len() > MAX_DECODED_BYTES => return V::new_obj(),
            Ok(t) => t,
            Err(_) => return V::new_obj(),
        }
    } else {
        bytes.to_vec()
    };
    match parse_str(&gzip::decode_utf8(&text)) {
        Ok(v) => object_or_empty(v),
        Err(_) => V::new_obj(),
    }
}

/// `Number.isInteger(v) && 0 <= v <= 255`
fn byte_of(v: &V) -> Option<u8> {
    match v {
        V::Num(n) if n.is_finite() && n.fract() == 0.0 && *n >= 0.0 && *n <= 255.0 => Some(*n as u8),
        _ => None,
    }
}

/// `byteArray(items)` — 모두 0 ~ 255 의 정수일 때만
fn byte_array(items: &[V]) -> Option<Vec<u8>> {
    items.iter().map(byte_of).collect()
}

fn is_digits(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit())
}

/// `indexedBytes(m)` — {"0":31,"1":139,…}: 열쇠가 0 부터 이어진 숫자일 때만 바이트다
fn indexed_bytes(m: &V) -> Option<Vec<u8>> {
    let keys = m.keys();
    let mut out = vec![0u8; keys.len()];
    for k in &keys {
        if !is_digits(k) {
            return None;
        }
        // Number(k): 숫자만인 글이라 f64 로 읽는다 (아주 긴 글은 Infinity 가 되어 아래에서 걸린다)
        let i: f64 = k.parse().unwrap_or(f64::INFINITY);
        let b = byte_of(&m.get(k));
        if i >= keys.len() as f64 || b.is_none() {
            return None;
        }
        out[i as usize] = b?;
    }
    Some(out)
}

/// `atob` 의 forgiving-base64 (공백은 이미 뺐고 글자 · `=` 위치는 `base64Bytes` 의 정규식이 걸렀다)
fn atob(s: &str) -> Option<Vec<u8>> {
    let mut s = s.to_string();
    if s.len() % 4 == 0 {
        if s.ends_with("==") {
            s.truncate(s.len() - 2);
        } else if s.ends_with('=') {
            s.truncate(s.len() - 1);
        }
    }
    if s.len() % 4 == 1 || s.contains('=') {
        return None;
    }
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            _ => 63,
        }
    };
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        buf = (buf << 6) | val(c);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `base64Bytes(s)`
fn base64_bytes(s: &str) -> Option<Vec<u8>> {
    // s.replace(/\s+/g, '') — JS 의 \s (유니코드 공백 · 줄 끝)
    let compact: String = s.chars().filter(|c| !premium_discount_worker_common::jsv::is_js_space(*c)).collect();
    // /^[A-Za-z0-9+/]*={0,2}$/
    let body = compact.trim_end_matches('=');
    if compact.len() - body.len() > 2 || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
        return None;
    }
    if compact.len() % 4 == 1 {
        return None;
    }
    atob(&compact)
}

/// `decodeBlob(raw)` — D1 · Worker 가 준 data 값을 사건 객체로 푼다. 풀 수 없으면 {}
pub fn decode_blob(raw: &V) -> V {
    match raw {
        // 조각이면 그 범위만 (BUG-RT-3) — V::Bytes 는 이미 그 범위의 바이트다
        V::Bytes(b) => decode_bytes(&b.borrow()),
        V::Arr(_) => match byte_array(&raw.items()) {
            Some(bytes) => decode_bytes(&bytes),
            None => V::new_obj(),
        },
        V::Obj(_) => {
            // D1 행({…, data: BLOB}) · Node Buffer({type: 'Buffer', data: […]})면 data 를 푼다
            let inner = raw.get("data");
            if matches!(inner, V::Arr(_) | V::Str(_) | V::Obj(_) | V::Bytes(_)) {
                return decode_blob(&inner);
            }
            // 열쇠가 모두 숫자면 JSON.stringify(Uint8Array) 다: 이어지지 않으면 망가진 것
            if raw.keys().iter().all(|k| is_digits(k)) {
                return match indexed_bytes(raw) {
                    Some(bytes) => decode_bytes(&bytes),
                    None => V::new_obj(),
                };
            }
            raw.clone()
        }
        V::Str(s) => match parse_str(s) {
            // 원시값이면 {} (BUG-RT-4)
            Ok(v) => object_or_empty(v),
            // 길이와 상관없이 gzip 머리를 본다 (BUG-RT-2)
            Err(_) => match base64_bytes(s) {
                Some(bytes) => decode_bytes(&bytes),
                None => V::new_obj(),
            },
        },
        _ => V::new_obj(),
    }
}

/// `encodeBlob(obj)` — 객체를 gzip 한 JSON 바이트로 (D1 에 저장하는 모양). gzip 바이트는 pako 와 다르지만 풀면 같다(common/gzip.rs)
pub fn encode_blob(obj: &V) -> Vec<u8> {
    let text = premium_discount_worker_common::jsv::stringify(obj).unwrap_or_default();
    gzip::gzip(text.as_bytes())
}

/// `bytesToBase64(bytes)`
pub fn bytes_to_base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `blobToBase64(raw)` — D1 이 BLOB 을 바이트 숫자 배열 · ArrayBuffer 로 돌려줄 때, 응답에 싣는 base64 로
pub fn blob_to_base64(raw: &V) -> String {
    match raw {
        V::Bytes(b) => bytes_to_base64(&b.borrow()),
        V::Arr(_) => byte_array(&raw.items()).map(|b| bytes_to_base64(&b)).unwrap_or_default(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_worker_common::jsv::stringify;

    #[test]
    fn base64_round_trip_and_atob_padding_rules() {
        for n in 0..8 {
            let b: Vec<u8> = (0..n).map(|i| (i * 37 + 1) as u8).collect();
            assert_eq!(base64_bytes(&bytes_to_base64(&b)).unwrap(), b);
        }
        assert_eq!(base64_bytes("YQ").unwrap(), b"a");
        assert!(base64_bytes("YQ=").is_none(), "길이가 4의 배수가 아니면 = 를 받지 않는다 (atob)");
        assert!(base64_bytes("Y").is_none());
        assert!(base64_bytes("YQ===").is_none());
        assert_eq!(base64_bytes(" Y Q = = ").unwrap(), b"a");
    }

    #[test]
    fn shapes() {
        let gz = gzip::gzip(br#"{"a":1}"#);
        assert_eq!(stringify(&decode_blob(&V::bytes(gz.clone()))).unwrap(), r#"{"a":1}"#);
        let nums = V::from(gz.iter().map(|b| V::Num(*b as f64)).collect::<Vec<_>>());
        assert_eq!(stringify(&decode_blob(&nums)).unwrap(), r#"{"a":1}"#);
        let row = premium_discount_worker_common::obj! { "id" => "x", "data" => nums.clone() };
        assert_eq!(stringify(&decode_blob(&row)).unwrap(), r#"{"a":1}"#);
        assert_eq!(stringify(&decode_blob(&V::from("42"))).unwrap(), "{}");
        assert_eq!(stringify(&decode_blob(&V::Num(3.0))).unwrap(), "{}");
        assert_eq!(blob_to_base64(&nums), bytes_to_base64(&gz));
        assert_eq!(blob_to_base64(&V::from("x")), "");
    }
}
