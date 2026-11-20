//! 원본 `vector.ts`: D1 BLOB 칸에서 읽은 벡터를 Float32 768개(3072바이트)로 바꾼다.
//!
//! D1 은 BLOB 을 ArrayBuffer 가 아니라 바이트 숫자 배열로 돌려준다. 배열이면 원소가 모두 0..255 정수인지 먼저 보고,
//! 길이를 검사한다. 오류 글에는 길이와 형태만 넣는다(벡터 값은 넣지 않는다).
//! Rust 판은 검사한 바이트를 돌려주고, 진입점이 그 바이트를 그대로 `Float32Array` 로 본다(비트 그대로).

use premium_discount_worker_common::jsv::{Throw, R, V};

/// workers/www/src/evaluations.ts 의 EVAL_DIMENSIONS 와 같아야 한다 (google/embeddinggemma-2 출력 차원)
pub const VECTOR_DIMENSIONS: usize = 768;
const BYTES_PER_FLOAT: usize = 4;

/// `toVector(raw)` 의 바이트 (리틀 엔디언 Float32 × 768)
pub fn to_vector_bytes(raw: &V) -> R<Vec<u8>> {
    let bytes = match raw {
        // ArrayBuffer · Uint8Array (D1 에서는 오지 않는다)
        V::Bytes(b) => b.borrow().clone(),
        V::Arr(_) => {
            // 바이트 값이 아닌 것이 섞여 있으면 Uint8Array.from 이 조용히 값을 깎아 버리므로 먼저 검사한다
            let items = raw.items();
            let mut out = Vec::with_capacity(items.len());
            for b in &items {
                match b {
                    V::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= 255.0 => out.push(*n as u8),
                    _ => return Err(Throw::error("vector blob contains a non-byte value")),
                }
            }
            out
        }
        _ => return Err(Throw::error("vector blob has an unexpected type")),
    };
    let want = VECTOR_DIMENSIONS * BYTES_PER_FLOAT;
    if bytes.len() != want {
        return Err(Throw::error(format!("vector blob has {} bytes, expected {want}", bytes.len())));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<f32> {
        (0..VECTOR_DIMENSIONS).map(|i| (i as f32 - 384.0) / 768.0).collect()
    }

    fn le(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn floats(b: &[u8]) -> Vec<f32> {
        b.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    fn arr(b: &[u8]) -> V {
        V::from(b.iter().map(|x| V::Num(*x as f64)).collect::<Vec<_>>())
    }

    #[test]
    fn array_buffer_and_d1_byte_arrays_give_the_same_vector() {
        let v = sample();
        assert_eq!(floats(&to_vector_bytes(&V::bytes(le(&v))).unwrap()), v);
        let out = to_vector_bytes(&arr(&le(&v))).unwrap();
        assert_eq!(out.len(), 3072);
        assert_eq!(floats(&out), v);
    }

    #[test]
    fn wrong_lengths_are_rejected() {
        for n in [100usize, 3071, 6144] {
            let err = to_vector_bytes(&arr(&vec![0u8; n])).unwrap_err();
            assert_eq!(err.message, format!("vector blob has {n} bytes, expected 3072"));
        }
    }

    #[test]
    fn non_byte_values_are_rejected_without_the_value_in_the_message() {
        for x in [V::Num(256.0), V::Num(-1.0), V::Num(1.5), V::Num(f64::NAN), V::from("7")] {
            let a = arr(&vec![0u8; 3072]);
            a.put("10", x);
            assert_eq!(to_vector_bytes(&a).unwrap_err().message, "vector blob contains a non-byte value");
        }
        let mut big = vec![V::Num(7.0); 3072];
        big.push(V::Num(300.0));
        assert!(!to_vector_bytes(&V::from(big)).unwrap_err().message.contains("300"));
    }

    #[test]
    fn unsupported_types_are_rejected() {
        for x in [V::Null, V::Undef, V::from("abc"), V::Num(42.0), V::new_obj()] {
            assert_eq!(to_vector_bytes(&x).unwrap_err().message, "vector blob has an unexpected type");
        }
    }
}
