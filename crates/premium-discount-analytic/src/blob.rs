//! 사건 BLOB 코덱: D1 의 `data` 칸(gzip 한 JSON)을 어떤 모양으로 받아도 같은 객체로 푼다
//!
//! logis-center `commerce/src/modes/runtime.ts` 의 `decodeAnalyticBlob` 을 옮기면서, 그 시험(`runtime.test.ts`)이 `it.fails`
//! 로 남겨 둔 세 결함을 고쳤다. 같은 규칙의 TypeScript 판은 `www/src/blob.ts` 이고, 두 쪽은 `contracts/analytic_blob_v1.json`
//! 의 사례를 함께 통과한다.
//!
//! - **BUG-RT-2**: base64 로 온 gzip 이 50 바이트 이하면 풀지 않고 `{}` 를 냈다(`rawBytes.length > 50 &&`). 이제 길이와
//!   상관없이 gzip 머리(`1f 8b`)를 본다. 작은 사건(`{"a":1}` 은 gzip 27 바이트)이 조용히 사라지던 원인이다.
//! - **BUG-RT-3**: `Uint8Array` 조각(subarray)을 그 밑의 버퍼 전체로 읽었다. 바이트를 받는 이 판에는 해당 없고,
//!   TypeScript 판이 `byteOffset` · `byteLength` 를 지킨다.
//! - **BUG-RT-4**: JSON 원시값(`"42"`, `"null"`)을 그대로 돌려줬다. 사건은 객체여야 하므로 객체가 아니면 `{}` 다.
//!
//! 받는 모양: 바이트 숫자 배열(D1 이 BLOB 을 돌려주는 모양), `{"type":"Buffer","data":[…]}`, `{"0":31,"1":139,…}`
//! (`JSON.stringify(Uint8Array)`), base64 글(gzip 또는 UTF-8 JSON), JSON 글, 이미 풀린 객체.

use std::io::Read;

use base64::Engine;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{Map, Value};

/// 푼 결과의 크기 상한 (압축 폭탄 막기)
pub const MAX_DECODED_BYTES: u64 = 4 * 1024 * 1024;

fn object_or_empty(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

fn gunzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(bytes).take(MAX_DECODED_BYTES + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= MAX_DECODED_BYTES).then_some(out)
}

/// 바이트(gzip 이거나 UTF-8 JSON)를 객체로. 풀 수 없거나 객체가 아니면 `{}`
pub fn decode_bytes(bytes: &[u8]) -> Map<String, Value> {
    let text = if is_gzip(bytes) {
        match gunzip(bytes) {
            Some(b) => b,
            None => return Map::new(),
        }
    } else {
        bytes.to_vec()
    };
    serde_json::from_slice::<Value>(&text).map(object_or_empty).unwrap_or_default()
}

fn byte_array(items: &[Value]) -> Option<Vec<u8>> {
    items.iter().map(|v| v.as_u64().filter(|b| *b <= 255).map(|b| b as u8)).collect()
}

/// `{"0":31,"1":139,…}`: 열쇠가 0 부터 이어진 숫자일 때만 바이트다
fn indexed_bytes(m: &Map<String, Value>) -> Option<Vec<u8>> {
    let mut out = vec![0u8; m.len()];
    for (k, v) in m {
        let i: usize = k.parse().ok()?;
        let b = v.as_u64().filter(|b| *b <= 255)?;
        *out.get_mut(i)? = b as u8;
    }
    Some(out)
}

/// D1 · Worker 가 준 `data` 값을 사건 객체로 푼다 (모듈 설명의 모양들). 풀 수 없으면 `{}`
pub fn decode(raw: &Value) -> Map<String, Value> {
    match raw {
        Value::Array(items) => byte_array(items).map(|b| decode_bytes(&b)).unwrap_or_default(),
        Value::Object(m) => {
            // D1 행(`{…, data: BLOB}`) · Node Buffer(`{type: "Buffer", data: […]}`) 이면 `data` 를 푼다
            if let Some(inner @ (Value::Array(_) | Value::String(_) | Value::Object(_))) = m.get("data") {
                return decode(inner);
            }
            // 열쇠가 모두 숫자면 `JSON.stringify(Uint8Array)` 다: 이어지지 않으면 망가진 것
            if m.keys().all(|k| k.parse::<usize>().is_ok()) {
                return indexed_bytes(m).map(|b| decode_bytes(&b)).unwrap_or_default();
            }
            m.clone()
        }
        Value::String(s) => {
            // JSON 글이면 그대로, 아니면 base64(gzip 또는 UTF-8 JSON)
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                return object_or_empty(v);
            }
            let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
            match base64::engine::general_purpose::STANDARD.decode(compact.as_bytes()) {
                Ok(bytes) => decode_bytes(&bytes),
                Err(_) => Map::new(),
            }
        }
        _ => Map::new(),
    }
}

/// 객체를 gzip 한 JSON 바이트로 (Worker 가 저장하는 모양)
pub fn encode(obj: &Map<String, Value>) -> Vec<u8> {
    let json = serde_json::to_vec(obj).expect("JSON 객체는 직렬화된다");
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(&mut enc, &json).expect("메모리에 쓰기");
    enc.finish().expect("메모리에 쓰기")
}

/// [`encode`] 의 base64 (Worker 가 JSON 응답에 싣는 모양)
pub fn encode_base64(obj: &Map<String, Value>) -> String {
    base64::engine::general_purpose::STANDARD.encode(encode(obj))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        object_or_empty(v)
    }

    #[test]
    fn small_base64_gzip_is_decoded() {
        // BUG-RT-2: `{"a":1}` 의 gzip 은 50 바이트보다 작다
        let small = obj(json!({"a": 1}));
        assert!(encode(&small).len() <= 50);
        assert_eq!(decode(&Value::String(encode_base64(&small))), small);
    }

    #[test]
    fn primitives_are_not_events() {
        // BUG-RT-4
        for raw in ["42", "null", "true", "\"x\"", "[1,2]"] {
            assert_eq!(decode(&Value::String(raw.into())), Map::new(), "{raw}");
        }
    }

    #[test]
    fn every_transport_shape_decodes_to_the_same_object() {
        let event = obj(json!({"type": "click", "target": {"pug": "button | 결제하기"}}));
        let gz = encode(&event);
        let numbers: Vec<Value> = gz.iter().map(|b| json!(b)).collect();
        let indexed: Map<String, Value> = gz.iter().enumerate().map(|(i, b)| (i.to_string(), json!(b))).collect();
        let plain: Vec<Value> = serde_json::to_vec(&event).unwrap().iter().map(|b| json!(b)).collect();
        for raw in [
            Value::Array(numbers.clone()),
            json!({"type": "Buffer", "data": numbers}),
            Value::Object(indexed),
            Value::String(encode_base64(&event)),
            Value::String(serde_json::to_string(&event).unwrap()),
            Value::Array(plain),
            Value::Object(event.clone()),
        ] {
            assert_eq!(decode(&raw), event, "{raw}");
        }
    }

    #[test]
    fn broken_input_is_empty() {
        for raw in [json!("not base64 !!"), json!([300, 1]), json!(7), Value::Null, json!({"0": 31, "2": 139})] {
            assert_eq!(decode(&raw), Map::new(), "{raw}");
        }
    }
}
