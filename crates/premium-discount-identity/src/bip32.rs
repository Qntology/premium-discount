//! BIP-32 경로 파생 (secp256k1)

use hmac::{Hmac, Mac};
use k256::elliptic_curve::ff::PrimeField;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::{Scalar, SecretKey};
use sha2::Sha512;
use zeroize::Zeroizing;

type HmacSha512 = Hmac<Sha512>;

pub(crate) const HARDENED: u32 = 0x8000_0000;

/// 0 이 아니고 곡선 차수보다 작은 32바이트 정수. 아니면 None
fn scalar(bytes: &[u8]) -> Option<Scalar> {
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    let s: Option<Scalar> = Scalar::from_repr(bytes.into()).into();
    s.filter(|s| !bool::from(s.is_zero()))
}

/// HMAC-SHA512 결과를 지울 수 있는 배열로
fn hmac512(key: &[u8], parts: &[&[u8]]) -> Option<Zeroizing<[u8; 64]>> {
    let mut mac = HmacSha512::new_from_slice(key).ok()?;
    for p in parts {
        mac.update(p);
    }
    let mut out = Zeroizing::new([0u8; 64]);
    out.copy_from_slice(&mac.finalize().into_bytes());
    Some(out)
}

/// 시드에서 경로(`path`: 단계마다 번호, 강화 단계는 `HARDENED` 를 더함)의 개인 키를 만든다.
/// 키가 범위를 벗어나는 드문 경우(확률 약 2^-127)에는 None
pub(crate) fn derive(seed: &[u8], path: &[u32]) -> Option<Zeroizing<[u8; 32]>> {
    let master = hmac512(b"Bitcoin seed", &[seed])?;
    let mut key = Zeroizing::new([0u8; 32]);
    let mut chain = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&master[..32]);
    chain.copy_from_slice(&master[32..]);
    let mut parent = scalar(key.as_ref())?;
    for &index in path {
        let number = index.to_be_bytes();
        let step = if index >= HARDENED {
            hmac512(chain.as_ref(), &[&[0u8], key.as_ref(), &number])?
        } else {
            let secret = SecretKey::from_slice(key.as_ref()).ok()?;
            hmac512(chain.as_ref(), &[secret.public_key().to_encoded_point(true).as_bytes(), &number])?
        };
        let tweak: Option<Scalar> = Scalar::from_repr(<[u8; 32]>::try_from(&step[..32]).ok()?.into()).into();
        let child = tweak? + parent;
        if bool::from(child.is_zero()) {
            return None;
        }
        key.copy_from_slice(&child.to_bytes());
        chain.copy_from_slice(&step[32..]);
        parent = child;
    }
    Some(key)
}
