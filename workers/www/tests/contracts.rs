//! 계약 · 벡터 시험 (호스트에서 돈다):
//! - contracts/wallet_v1.json — `wallet.test.ts` 의 단언 모두 (서명 글 · 계약 서명 받기 · 위조 · 5분 · 새 지갑). 지갑 확인은
//!   navtrials · analytic 의 경로에서만 쓰여 1단계 시험대로는 닿지 않아 여기서 봤다 (2단계부터는 시험대의 www_navtrials ·
//!   www_analytic · www_wallet 시나리오도 닿는다)
//!   그리고 fix-wallet-replay 의 한 번만 쓰기 키(서명한 글의 SHA-256 · 기한) — D1 에 기억하는 쪽은 시험대 scenarios/www_wallet.mjs
//! - contracts/analytic_blob_v1.json — blob.ts 의 계약 (crates/premium-discount-analytic 와 같은 파일)
//! - tests/render_vectors.json — 원본 renderHtml 로 뽑은 바이트 (scripts/workers-diff/www_render_vectors.mjs)

use premium_discount_worker_common::jsv::{from_json, stringify, V};
use premium_discount_worker_common::web::Req;
use premium_discount_worker_www::auth::{verify_message, verify_wallet_request, verify_wallet_signature, wallet_request_message, WALLET_MAX_SKEW_MS};
use premium_discount_worker_www::blob::decode_blob;
use premium_discount_worker_www::render::render_html;
use serde_json::Value;

const WALLET_V1: &str = include_str!("../../../contracts/wallet_v1.json");
const BLOB_V1: &str = include_str!("../../../contracts/analytic_blob_v1.json");
const RENDER: &str = include_str!("render_vectors.json");

/// 계약의 요청 r 을 Req 로. 바꿀 것(본문 · 경로 · 헤더)을 덮어 위조를 만든다
fn signed(r: &Value, body: Option<&str>, path: Option<&str>, headers: &[(&str, &str)]) -> (Req, Vec<u8>) {
    let mut h: Vec<(String, String)> = vec![
        ("x-pd-address".into(), r["address"].as_str().unwrap().into()),
        ("x-pd-time".into(), r["time_ms"].as_i64().unwrap().to_string()),
        ("x-pd-signature".into(), r["signature"].as_str().unwrap().into()),
    ];
    for (k, v) in headers {
        h.retain(|(n, _)| n != k);
        h.push((k.to_string(), v.to_string()));
    }
    let body = body.unwrap_or(r["body"].as_str().unwrap()).as_bytes().to_vec();
    let req = Req {
        method: r["method"].as_str().unwrap().to_uppercase(),
        url: format!("https://premium.discount{}", path.unwrap_or(r["path"].as_str().unwrap())),
        headers: h,
        cf: V::Undef,
        body: body.clone(),
    };
    (req, body)
}

#[test]
fn wallet_message_matches_the_contract() {
    let c: Value = serde_json::from_str(WALLET_V1).unwrap();
    assert_eq!(c["max_skew_ms"].as_f64().unwrap(), WALLET_MAX_SKEW_MS);
    for r in c["requests"].as_array().unwrap() {
        let m = wallet_request_message(
            r["address"].as_str().unwrap(),
            r["time_ms"].as_f64().unwrap(),
            r["method"].as_str().unwrap(),
            r["path"].as_str().unwrap(),
            r["body"].as_str().unwrap().as_bytes(),
        );
        assert_eq!(m, r["message"].as_str().unwrap());
    }
    // ethers.verifyMessage 의 계약 서명
    let signer = c["wallets"][0]["address"].as_str().unwrap();
    for m in c["messages"].as_array().unwrap() {
        assert_eq!(verify_message(m["text"].as_str().unwrap(), m["signature"].as_str().unwrap()).as_deref(), Some(signer));
    }
}

#[test]
fn wallet_contract_requests_verify() {
    let c: Value = serde_json::from_str(WALLET_V1).unwrap();
    for r in c["requests"].as_array().unwrap() {
        let (req, body) = signed(r, None, None, &[]);
        let at = r["time_ms"].as_f64().unwrap() + WALLET_MAX_SKEW_MS;
        assert_eq!(verify_wallet_request(&req, &body, at).as_deref(), r["address"].as_str(), "{}", r["path"]);
    }
}

#[test]
fn wallet_forgeries_and_skew_are_rejected() {
    let c: Value = serde_json::from_str(WALLET_V1).unwrap();
    let r = &c["requests"][0];
    let t = r["time_ms"].as_f64().unwrap();
    let other = c["wallets"][1]["address"].as_str().unwrap();
    let forged = [
        ("본문", signed(r, Some("{\"tampered\":true}"), None, &[])),
        ("경로", signed(r, None, Some("/nav/other"), &[])),
        ("다른 주소", signed(r, None, None, &[("x-pd-address", other)])),
        ("서명 형식", signed(r, None, None, &[("x-pd-signature", "0x1234")])),
        ("시각 형식", signed(r, None, None, &[("x-pd-time", "soon")])),
        ("주소 없음", signed(r, None, None, &[("x-pd-address", "")])),
    ];
    for (what, (req, body)) in forged {
        assert_eq!(verify_wallet_request(&req, &body, t), None, "{what}");
    }
    let (req, body) = signed(r, None, None, &[]);
    assert_eq!(verify_wallet_request(&req, &body, t + WALLET_MAX_SKEW_MS + 1.0), None, "5분이 넘으면");
    assert_eq!(verify_wallet_request(&req, &body, t - WALLET_MAX_SKEW_MS - 1.0), None, "5분 앞서도");
}

#[test]
fn wallet_replay_key_is_the_signed_message() {
    use sha2::{Digest, Sha256};
    let c: Value = serde_json::from_str(WALLET_V1).unwrap();
    for r in c["requests"].as_array().unwrap() {
        let t = r["time_ms"].as_f64().unwrap();
        let (req, body) = signed(r, None, None, &[]);
        let s = verify_wallet_signature(&req, &body, t).expect("계약 서명");
        let want: String = Sha256::digest(r["message"].as_str().unwrap().as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(s.nonce, want, "키 = 서명한 글의 SHA-256");
        assert_eq!(s.expires_at, t + WALLET_MAX_SKEW_MS, "기한 = 서명 시각 + 5분 (시각 검사가 받는 마지막 때)");
        assert_eq!(verify_wallet_signature(&req, &body, s.expires_at).map(|x| x.nonce), Some(want.clone()), "기한까지는 시각 검사를 지난다");
        assert_eq!(verify_wallet_signature(&req, &body, s.expires_at + 1.0), None, "기한 뒤로는 시각 검사가 막는다");
        // 쿼리만 바꾼 다시 보내기는 같은 글 → 같은 키 (fix-wallet-replay 가 막는 꼴)
        let path = format!("{}?app=0x{}", r["path"].as_str().unwrap(), "9".repeat(40));
        let (q, _) = signed(r, None, Some(&path), &[]);
        assert_eq!(verify_wallet_signature(&q, &body, t).map(|x| x.nonce), Some(want.clone()), "쿼리는 키에 들어가지 않는다");
        // 같은 글의 다른 서명 꼴(v 27/28 → 0/1, 16진 대문자)도 받고 같은 키다
        let sig = r["signature"].as_str().unwrap();
        let v = u8::from_str_radix(&sig[130..], 16).unwrap();
        let v_form = format!("{}{:02x}", &sig[..130], v - 27);
        let upper = format!("0x{}", sig[2..].to_uppercase());
        for form in [v_form, upper] {
            let (f, _) = signed(r, None, None, &[("x-pd-signature", form.as_str())]);
            assert_eq!(verify_wallet_signature(&f, &body, t).map(|x| x.nonce), Some(want.clone()), "{form}");
        }
    }
}

#[test]
fn a_fresh_wallet_with_mixed_case_header_and_query_verifies() {
    let wallet = premium_discount_identity::Wallet::create();
    let time = 1763380800000_i64;
    let body = "{\"runs\":[]}";
    // 대소문자 섞인 주소(체크섬) 헤더도 받는다 — 서명 글은 소문자 주소로 만든다
    let mixed = premium_discount_worker_common::eth::get_address(wallet.address()).unwrap();
    let message = wallet_request_message(&mixed, time as f64, "post", "/nav/trials", body.as_bytes());
    let signature = wallet.sign_message(&message);
    let req = Req {
        method: "POST".into(),
        url: "https://premium.discount/nav/trials?debug=1".into(),
        headers: vec![("x-pd-address".into(), mixed), ("x-pd-time".into(), time.to_string()), ("x-pd-signature".into(), signature)],
        cf: V::Undef,
        body: body.as_bytes().to_vec(),
    };
    assert_eq!(verify_wallet_request(&req, body.as_bytes(), time as f64).as_deref(), Some(wallet.address()), "쿼리는 서명 글에 들어가지 않는다");
}

#[test]
fn ethers_signature_edge_rules() {
    let c: Value = serde_json::from_str(WALLET_V1).unwrap();
    let m = &c["messages"][0];
    let text = m["text"].as_str().unwrap();
    let sig = m["signature"].as_str().unwrap();
    let signer = c["wallets"][0]["address"].as_str().unwrap();
    let with_v = |v: u8| format!("{}{:02x}", &sig[..130], v);
    let v = u8::from_str_radix(&sig[130..], 16).unwrap();
    // 27/28 · 0/1 · EIP-155(35 이상: 홀수 27, 짝수 28)는 같은 서명이다
    let parity = v - 27;
    assert_eq!(verify_message(text, &with_v(parity)).as_deref(), Some(signer));
    assert_eq!(verify_message(text, &with_v(if parity == 0 { 37 } else { 38 })).as_deref(), Some(signer));
    // 2 ~ 26 · 29 ~ 34 는 ethers 가 던진다
    assert_eq!(verify_message(text, &with_v(2)), None);
    assert_eq!(verify_message(text, &with_v(30)), None);
    // s 의 맨 위 비트가 켜지면 "non-canonical s"
    let high = format!("{}{}{}", &sig[..66], "f", &sig[67..]);
    assert_eq!(verify_message(text, &high), None);
}

#[test]
fn blob_contract() {
    let c: Value = serde_json::from_str(BLOB_V1).unwrap();
    for case in c["cases"].as_array().unwrap() {
        let got = decode_blob(&from_json(case["input"].clone()));
        let want = stringify(&from_json(case["expect"].clone())).unwrap();
        assert_eq!(stringify(&got).unwrap(), want, "{}", case["name"]);
    }
    // Uint8Array 로 온 gzip (V::Bytes)
    let gz: Vec<u8> = c["cases"].as_array().unwrap().iter().find(|x| x["name"] == "D1 byte array (gzip)").unwrap()["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as u8)
        .collect();
    assert_ne!(stringify(&decode_blob(&V::bytes(gz))).unwrap(), "{}");
}

#[test]
fn render_matches_the_original_bytes() {
    let v: Value = serde_json::from_str(RENDER).unwrap();
    for case in v["cases"].as_array().unwrap() {
        let rows = if case["rows"] == serde_json::json!({ "$": "undefined" }) { V::Undef } else { from_json(case["rows"].clone()) };
        let html = render_html(&from_json(case["cookies"].clone()), &rows).unwrap();
        assert_eq!(html, case["html"].as_str().unwrap(), "{}", case["name"]);
    }
}
