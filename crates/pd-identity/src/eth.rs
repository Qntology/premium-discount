//! keccak · EIP-191 · 주소 · 서명 (ethers 와 같은 값)

use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use tiny_keccak::{Hasher, Keccak};

pub(crate) fn keccak256(parts: &[&[u8]]) -> [u8; 32] {
    let mut k = Keccak::v256();
    for p in parts {
        k.update(p);
    }
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `0x` 를 붙였든 안 붙였든 hex 를 바이트로. 짝수 길이 hex 가 아니면 None
pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    let t = text.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    if t.len() % 2 != 0 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..t.len()).step_by(2).map(|i| u8::from_str_radix(&t[i..i + 2], 16).ok()).collect()
}

/// `ethers.hashMessage(text)`: `"\x19Ethereum Signed Message:\n" + 바이트 길이 + text` 의 keccak256 (EIP-191)
pub fn hash_message(text: &str) -> [u8; 32] {
    let len = text.len().to_string();
    keccak256(&[b"\x19Ethereum Signed Message:\n", len.as_bytes(), text.as_bytes()])
}

/// 공개 키의 주소: 압축하지 않은 공개 키의 keccak256 뒤 20바이트 (`0x` + 소문자 hex 40자)
pub fn address_of(key: &VerifyingKey) -> String {
    let point = key.to_encoded_point(false);
    let digest = keccak256(&[&point.as_bytes()[1..]]);
    format!("0x{}", hex(&digest[12..]))
}

/// premium.discount 의 `hashId(text)` = `ethers.computeAddress(ethers.hashMessage(text))`.
/// 메시지 해시가 개인 키 범위 밖이면(확률 약 2^-128) None
pub fn hash_id(text: &str) -> Option<String> {
    let key = SigningKey::from_slice(&hash_message(text)).ok()?;
    Some(address_of(key.verifying_key()))
}

/// 32바이트 해시에 서명한다: r ‖ s ‖ v(27 + 복원 번호), `0x` hex 65바이트 (ethers 와 같은 형식, RFC 6979 결정적 서명)
pub(crate) fn sign_hash(key: &SigningKey, digest: &[u8; 32]) -> String {
    let (signature, recovery) = key.sign_prehash_recoverable(digest).expect("32바이트 해시에는 서명할 수 있다");
    let mut out = signature.to_bytes().to_vec();
    out.push(27 + recovery.to_byte());
    format!("0x{}", hex(&out))
}

/// EIP-191 서명에서 서명한 주소를 되살린다 (`ethers.verifyMessage`). 서명 형식이 틀리면 None
pub fn recover_address(text: &str, signature: &str) -> Option<String> {
    let bytes = unhex(signature)?;
    if bytes.len() != 65 {
        return None;
    }
    let v = match bytes[64] {
        27 | 28 => bytes[64] - 27,
        0 | 1 => bytes[64],
        _ => return None,
    };
    let signature = Signature::from_slice(&bytes[..64]).ok()?;
    let recovery = RecoveryId::from_byte(v)?;
    let key = VerifyingKey::recover_from_prehash(&hash_message(text), &signature, recovery).ok()?;
    Some(address_of(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_id_matches_the_server_and_raon_zena() {
        // raon-zena premium-discount-mcp · pd-nav 시험과 같은 ethers 기준값
        assert_eq!(hash_id("hello").as_deref(), Some("0x56d67386939607c11bd60bb009eb02f4dd29c318"));
        assert_eq!(hash_id("store.steampowered.com").as_deref(), Some("0x0fee18a53a5dd368d3ce799e31cdef5763533d15"));
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        assert_eq!(unhex("0x00ff10").unwrap(), vec![0, 255, 16]);
        assert_eq!(hex(&[0, 255, 16]), "00ff10");
        assert!(unhex("0x0").is_none());
        assert!(unhex("zz").is_none());
        assert_eq!(recover_address("hello", "0x1234"), None);
    }
}
