//! 원본 `auth.ts` — 세션 자격 증명(OAuth3 hash/token: 브라우저는 쿠키, 앱은 헤더)과 지갑 요청 서명.
//!
//! 세션 검증: S3 hash/<hash> 객체를 읽기 전용으로 조회해 token 일치 · email 존재 · ip 일치를 확인한다.
//! 읽기 전용 AWS 자격 증명(SESSION_S3_*)이 모두 있을 때만 동작한다. 설정이 없거나 확인이 하나라도 실패하면 로그인이 아니다.
//! 세션은 두 종류다.
//! 1) 웹 세션: cdn(oauth.network)이 발급한다. S3 객체의 token 이 요청의 token 과 같아야 한다.
//! 2) 앱 세션: 이 Worker 가 발급한다 ([`issue_session`]). token = HMAC-SHA256(SESSION_SECRET, "session|" + hash) 라서 저장소가 필요 없다.
//!    사용자가 QR(mailto:<hash>.<호스트>@oauth.email)의 주소로 메일을 보내면 SES 가 S3 hash/<hash> 에 email 을 기록한다.
//!
//! 인증의 두 번째 갈래는 지갑(익명 계정)이다: 요청마다 EIP-191 서명(x-pd-address · x-pd-time · x-pd-signature)을 붙인다
//! ([`verify_wallet_request`]). 규칙과 기준값은 contracts/wallet_v1.json 이고 기기 쪽 crates/premium-discount-identity 가 같은 값을 낸다.
//!
//! 2단계 모듈이 쓰는 것: [`read_session_credentials`] · [`verified_address`] · [`verify_wallet_request`] · [`address_from_email`] ·
//! [`wallet_request_message`]. 원본의 async 함수 가운데 crypto.subtle 만 기다리는 것(HMAC · SHA-256)은 Rust 에서 바로 계산한다.

use hmac::{Hmac, Mac};
use premium_discount_worker_common::jsv::{js_trim, num_to_string, to_lower, to_upper};
use premium_discount_worker_common::web::Req;
use sha2::{Digest, Sha256};

use crate::util::hex;

/// `{ hash, token }`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCredentials {
    pub hash: String,
    pub token: String,
}

fn is_hex_lower(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `/^0+$/`
fn all_zero(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b == b'0')
}

/// `parseCookies(header).get(name)` — `;` 로 나누고 첫 `=` 앞뒤를 trim, 같은 이름은 뒤의 것 (Map.set)
fn cookie(header: &str, name: &str) -> Option<String> {
    let mut out = None;
    for part in header.split(';') {
        let Some(i) = part.find('=') else { continue };
        if js_trim(&part[..i]) == name {
            out = Some(js_trim(&part[i + 1..]).to_string());
        }
    }
    out
}

/// `normalizeHex(value)` — trim · 소문자 · 0x 접두사 제거 (앱은 0x 를 붙여 보낼 수 있고 브라우저 쿠키는 붙지 않는다)
pub fn normalize_hex(value: &str) -> String {
    let v = to_lower(js_trim(value));
    match v.strip_prefix("0x") {
        Some(rest) => rest.to_string(),
        None => v,
    }
}

/// `readSessionCredentials(request)` — 헤더(x-oauth-hash · x-oauth-token)가 하나라도 있으면 헤더만, 아니면 쿠키(hash · token).
/// 형식이 맞지 않거나 앱의 오프라인 자리값(0x000…)이면 None (익명 방문자)
pub fn read_session_credentials(req: &Req) -> Option<SessionCredentials> {
    let header_hash = crate::util::header_opt(req, "x-oauth-hash");
    let header_token = crate::util::header_opt(req, "x-oauth-token");
    let (hash, token) = if header_hash.is_some() || header_token.is_some() {
        (header_hash.unwrap_or_default(), header_token.unwrap_or_default())
    } else {
        let cookies = crate::util::header_or_empty(req, "Cookie");
        (cookie(&cookies, "hash").unwrap_or_default(), cookie(&cookies, "token").unwrap_or_default())
    };
    let hash = normalize_hex(&hash);
    let token = normalize_hex(&token);
    if !is_hex_lower(&hash, 40) || !is_hex_lower(&token, 64) {
        return None;
    }
    if all_zero(&hash) || all_zero(&token) {
        return None;
    }
    Some(SessionCredentials { hash, token })
}

/// 이메일 → 주소 (`addressFromEmail`): oauth.network(cdn/api)과 같은 식 `computeAddress(hashMessage(email)).toLowerCase()`
pub fn address_from_email(email: &str) -> String {
    premium_discount_worker_common::util::hash_str(email)
}

/// `timingSafeEqual(a, b)` — UTF-8 바이트를 끝까지 비교한다 (원본과 같은 뜻: 길이와 내용이 모두 같을 때만 참)
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn timing_safe_equal(a: &str, b: &str) -> bool {
    let (ea, eb) = (a.as_bytes(), b.as_bytes());
    let mut diff = ea.len() ^ eb.len();
    for i in 0..ea.len().max(eb.len()) {
        diff |= (*ea.get(i).unwrap_or(&0) ^ *eb.get(i).unwrap_or(&0)) as usize;
    }
    diff == 0
}

fn hmac_hex(secret: &str, message: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC 는 어떤 길이의 키도 받는다");
    mac.update(message.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// `tokenFor(secret, hash)` = HMAC-SHA256(secret, "session|" + hash)
pub fn token_for(secret: &str, hash: &str) -> String {
    hmac_hex(secret, &format!("session|{hash}"))
}

/// `encodeURIComponent(s)` — 남기는 것: A-Z a-z 0-9 - _ . ! ~ * ' ( )
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 앱 세션 발급 결과 (`IssuedSession`) — JSON 키 차례: hash, token, address, mailto
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedSession {
    pub hash: String,
    pub token: String,
    pub address: String,
    pub mailto: String,
}

/// `issueSession(env)` 의 계산 (무작위 20바이트 = `crypto.getRandomValues`, 메일 호스트는 `env.SESSION_MAIL_HOST || 'premium.discount'`)
pub fn issue_session_with(secret: &str, mail_host: &str, bytes: &[u8; 20]) -> IssuedSession {
    let hash = hex(bytes);
    let token = token_for(secret, &hash);
    let address = format!("{hash}.{mail_host}@oauth.email");
    let mailto = format!("mailto:{}", encode_uri_component(&address));
    IssuedSession { hash, token, address, mailto }
}

/// `SessionCheck`
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionCheck {
    Verified(String),
    /// 우리가 발급한 세션이지만 아직 메일이 도착하지 않았다
    Pending,
    /// 자격 증명이 맞지 않는다 (익명으로 취급)
    Invalid,
    /// 확인할 수 없다 (S3 설정이 없거나 S3 오류). 로그인으로 취급하지 않는다
    Unavailable,
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{check_session, issue_session, verified_address};

#[cfg(target_arch = "wasm32")]
mod wasm {
    use premium_discount_worker_common::aws::{self, AwsClient};
    use premium_discount_worker_common::js::{self, Env};
    use premium_discount_worker_common::web::Req;
    use premium_discount_worker_common::{Throw, R, V};

    use super::{address_from_email, issue_session_with, normalize_hex, read_session_credentials, timing_safe_equal, token_for};
    use super::{IssuedSession, SessionCheck, SessionCredentials};

    /// `issueSession(env)` — 저장하지 않는다: 같은 hash 로 token 을 다시 계산할 수 있으므로 검증 때 비밀만 있으면 된다
    pub fn issue_session(env: &Env) -> R<IssuedSession> {
        let secret = env.get("SESSION_SECRET");
        if !secret.truthy() {
            return Err(Throw::error("SESSION_SECRET is not configured"));
        }
        let mut bytes = [0u8; 20];
        getrandom::fill(&mut bytes).map_err(|e| Throw::error(format!("crypto.getRandomValues: {e}")))?;
        let host = env.get("SESSION_MAIL_HOST");
        let host = if host.truthy() { host.to_str() } else { "premium.discount".to_string() };
        Ok(issue_session_with(&secret.to_str(), &host, &bytes))
    }

    /// `checkSession(creds, env, ip)` — verified 가 아닌 모든 경우는 로그인이 아니다 (fail-closed).
    /// - 웹 세션: S3 객체의 token 일치 + email 존재 + 객체에 ip 가 있으면 CF-Connecting-IP 와 일치
    /// - 앱 세션: token 이 HMAC 과 일치 + S3 객체에 email 존재 (객체가 아직 없으면 pending)
    ///
    /// S3 는 GetObject 권한만 있으면 없는 키에도 403 을 돌려주므로 403 · 404 는 "아직 없음"으로 본다
    pub async fn check_session(creds: &SessionCredentials, env: &Env, ip: &str) -> SessionCheck {
        let bucket = env.get("SESSION_S3_BUCKET");
        let region = env.get("SESSION_S3_REGION");
        let access_key_id = env.get("SESSION_S3_ACCESS_KEY_ID");
        let secret_access_key = env.get("SESSION_S3_SECRET_ACCESS_KEY");
        if !bucket.truthy() || !region.truthy() || !access_key_id.truthy() || !secret_access_key.truthy() {
            return SessionCheck::Unavailable;
        }
        let secret = env.get("SESSION_SECRET");
        let issued = secret.truthy() && timing_safe_equal(&creds.token, &token_for(&secret.to_str(), &creds.hash));
        let r: R<SessionCheck> = async {
            let aws = AwsClient::try_new(access_key_id.clone(), secret_access_key.clone(), V::from("s3"), region.clone())?;
            // creds.hash 는 40자리 hex 로 검증된 값이라 경로에 그대로 써도 안전하다
            let url = format!("https://{}.s3.{}.amazonaws.com/hash/{}", bucket.to_str(), region.to_str(), creds.hash);
            let res = aws::fetch(&aws, &url, "", &[], &[]).await?;
            let status = js::response_status(&res);
            if status == 403.0 || status == 404.0 {
                return Ok(if issued { SessionCheck::Pending } else { SessionCheck::Invalid });
            }
            // res.ok = 200 ~ 299
            if !(200.0..300.0).contains(&status) {
                return Ok(SessionCheck::Unavailable);
            }
            let obj = js::response_json(&res).await?;
            if !obj.truthy() || obj.type_of() != "object" {
                return Ok(if issued { SessionCheck::Pending } else { SessionCheck::Invalid });
            }
            let email = match obj.get("email") {
                V::Str(s) => premium_discount_worker_common::jsv::js_trim(&s).to_string(),
                _ => String::new(),
            };
            if issued {
                return Ok(if email.is_empty() { SessionCheck::Pending } else { SessionCheck::Verified(email) });
            }
            let token_ok = match obj.get("token") {
                V::Str(t) => timing_safe_equal(&normalize_hex(&t), &creds.token),
                _ => false,
            };
            if !token_ok {
                return Ok(SessionCheck::Invalid);
            }
            if email.is_empty() {
                return Ok(SessionCheck::Invalid);
            }
            if let V::Str(obj_ip) = obj.get("ip") {
                if !obj_ip.is_empty() && obj_ip != ip {
                    return Ok(SessionCheck::Invalid);
                }
            }
            // 고침(fix-web-session-email-trim): 확인에 쓴 trim 한 email 을 돌려준다. 원본(auth.ts 162)은 obj.email 을 그대로 돌려줘,
            // 앞뒤 공백 · 줄바꿈이 붙은 같은 이메일이 앱 세션(trim 한 값)과 웹 세션에서 다른 주소가 되었다
            Ok(SessionCheck::Verified(email))
        }
        .await;
        r.unwrap_or(SessionCheck::Unavailable)
    }

    /// `verifiedAddress(request, env)` — 검증된 세션의 주소. 검증되지 않았으면 None
    pub async fn verified_address(req: &Req, env: &Env) -> Option<String> {
        let creds = read_session_credentials(req)?;
        let ip = crate::util::client_ip(req);
        match check_session(&creds, env, &ip).await {
            SessionCheck::Verified(email) => Some(address_from_email(&email)),
            _ => None,
        }
    }
}

// ───────────────────────── 지갑 (익명 계정) ─────────────────────────
// 서명하는 글(줄바꿈 \n):
//   premium.discount request v1 / address: <소문자 주소> / time: <유닉스 ms> / method: <대문자 메서드> / path: <경로> / body-sha256: <hex>

pub const WALLET_REQUEST_VERSION: &str = "premium.discount request v1";
pub const WALLET_MAX_SKEW_MS: f64 = 300_000.0;

/// `walletRequestMessage(address, timeMs, method, path, body)`
pub fn wallet_request_message(address: &str, time_ms: f64, method: &str, path: &str, body: &[u8]) -> String {
    [
        WALLET_REQUEST_VERSION.to_string(),
        format!("address: {}", to_lower(js_trim(address))),
        format!("time: {}", num_to_string(time_ms)),
        format!("method: {}", to_upper(js_trim(method))),
        format!("path: {path}"),
        format!("body-sha256: {}", hex(&Sha256::digest(body))),
    ]
    .join("\n")
}

/// `ethers.verifyMessage(message, signature)` 의 주소(소문자). ethers 가 던지는 자리는 None:
/// 65바이트가 아님 · v 가 0 · 1 · 27 · 28 · 35 이상이 아님 · s 의 맨 위 비트가 켜짐("non-canonical s") · r, s 가 [1, n) 밖 · 복원 실패.
/// ethers(noble)는 s 가 n/2 보다 커도(맨 위 비트는 꺼진 좁은 띠) 받는다. k256 의 복원은 낮은 s 만 받으므로 s 를 n − s 로 뒤집고
/// y 홀짝을 바꿔 같은 공개 키를 되살린다
pub fn verify_message(message: &str, signature: &str) -> Option<String> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
    let s = signature.strip_prefix("0x")?;
    if s.len() != 130 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..65).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
    if bytes[32] & 0x80 != 0 {
        return None;
    }
    let mut parity = match bytes[64] {
        0 | 27 => 0u8,
        1 | 28 => 1,
        v if v >= 35 => {
            if v & 1 == 1 {
                0
            } else {
                1
            }
        }
        _ => return None,
    };
    let mut sig = Signature::from_slice(&bytes[..64]).ok()?;
    if let Some(low) = sig.normalize_s() {
        sig = low;
        parity ^= 1;
    }
    let key = VerifyingKey::recover_from_prehash(&premium_discount_identity::hash_message(message), &sig, RecoveryId::new(parity == 1, false)).ok()?;
    Some(premium_discount_identity::address_of(&key))
}

fn is_wallet_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && is_hex_lower(&s[2..], 40)
}

fn is_wallet_signature(s: &str) -> bool {
    s.len() == 132 && s.starts_with("0x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_wallet_time(s: &str) -> bool {
    (1..=15).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
}

/// `verifyWalletRequest(request, body, nowMs)` — 지갑 서명이 맞으면 그 주소(소문자), 아니면 None.
/// 시각이 5분 안이고 서명에서 되살린 주소가 x-pd-address 와 같아야 한다. body 는 요청 본문 바이트 그대로(본문을 읽은 쪽이 넘긴다).
/// 서명 글의 path 는 `new URL(request.url).pathname` (쿼리는 들어가지 않는다)
pub fn verify_wallet_request(req: &Req, body: &[u8], now_ms: f64) -> Option<String> {
    let address = to_lower(js_trim(&crate::util::header_or_empty(req, "x-pd-address")));
    let time_text = js_trim(&crate::util::header_or_empty(req, "x-pd-time")).to_string();
    let signature = js_trim(&crate::util::header_or_empty(req, "x-pd-signature")).to_string();
    if !is_wallet_address(&address) || !is_wallet_time(&time_text) || !is_wallet_signature(&signature) {
        return None;
    }
    let time_ms: f64 = time_text.parse().ok()?;
    if (now_ms - time_ms).abs() > WALLET_MAX_SKEW_MS {
        return None;
    }
    let message = wallet_request_message(&address, time_ms, &req.method, &crate::util::pathname(req), body);
    (verify_message(&message, &signature)? == address).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(headers: &[(&str, &str)]) -> Req {
        Req {
            method: "POST".into(),
            url: "https://premium.discount/evaluations".into(),
            headers: headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.to_string())).collect(),
            ..Default::default()
        }
    }
    const HASH: &str = "abababababababababababababababababababab";
    const TOKEN: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    #[test]
    fn credentials_from_headers_or_cookies_normalised() {
        let want = Some(SessionCredentials { hash: HASH.into(), token: TOKEN.into() });
        assert_eq!(read_session_credentials(&req(&[("x-oauth-hash", HASH), ("x-oauth-token", TOKEN)])), want);
        let upper = format!("0x{}", HASH.to_uppercase());
        let tok = format!("0x{TOKEN}");
        assert_eq!(read_session_credentials(&req(&[("x-oauth-hash", &upper), ("x-oauth-token", &tok)])), want);
        let c = format!("a=1; hash={HASH}; token={TOKEN}");
        assert_eq!(read_session_credentials(&req(&[("cookie", &c)])), want);
        // 헤더가 하나라도 있으면 쿠키는 보지 않는다
        let c2 = format!("hash={HASH}; token={TOKEN}");
        assert_eq!(read_session_credentials(&req(&[("x-oauth-hash", HASH), ("cookie", &c2)])), None);
        let zh = format!("0x{}", "0".repeat(40));
        let zt = format!("0x{}", "0".repeat(64));
        assert_eq!(read_session_credentials(&req(&[("x-oauth-hash", &zh), ("x-oauth-token", &zt)])), None);
        assert_eq!(read_session_credentials(&req(&[("x-oauth-hash", "zz"), ("x-oauth-token", TOKEN)])), None);
        assert_eq!(read_session_credentials(&req(&[])), None);
    }

    #[test]
    fn issued_token_is_the_hmac_and_the_mailto_is_encoded() {
        let s = issue_session_with("test-session-secret", "premium.discount", &[0xab; 20]);
        assert_eq!(s.hash, HASH);
        // node: createHmac('sha256', 'test-session-secret').update('session|' + 'ab'.repeat(20)).digest('hex')
        assert_eq!(s.token, "add654532258a022de24ea8e3cf2b6edb8e5625e380bdbdf73dc9f70980fe820");
        assert_eq!(s.address, format!("{HASH}.premium.discount@oauth.email"));
        assert_eq!(s.mailto, format!("mailto:{HASH}.premium.discount%40oauth.email"));
        assert_eq!(encode_uri_component("a b/한!*'()~"), "a%20b%2F%ED%95%9C!*'()~");
    }

    #[test]
    fn timing_safe_equal_is_plain_equality() {
        assert!(timing_safe_equal("abc", "abc"));
        assert!(!timing_safe_equal("abc", "abd"));
        assert!(!timing_safe_equal("abc", "abcd"));
        assert!(!timing_safe_equal("", "a"));
    }
}
