//! contracts/ 의 공용 사례: 서버(www, TypeScript) · 수집 스크립트(tracker.js)와 같은 결과를 내는가

use premium_discount_analytic::{blob, pug};
use serde_json::Value;

const BLOB_V1: &str = include_str!("../../../contracts/analytic_blob_v1.json");
const PUG_V1: &str = include_str!("../../../contracts/analytic_pug_v1.json");

#[test]
fn blob_cases_decode_like_the_worker() {
    let v: Value = serde_json::from_str(BLOB_V1).unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert!(cases.len() >= 19);
    for c in cases {
        let want = c["expect"].as_object().unwrap().clone();
        assert_eq!(blob::decode(&c["input"]), want, "{}", c["name"]);
    }
}

#[test]
fn pug_cases_match_the_tracker() {
    let v: Value = serde_json::from_str(PUG_V1).unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert!(cases.len() >= 12);
    for c in cases {
        let html = c["html"].as_str().unwrap();
        assert_eq!(pug::html_to_pug(html), c["pug"].as_str().unwrap(), "{}", c["name"]);
    }
}
