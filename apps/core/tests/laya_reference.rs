//! laya-multilingual Rust 이식이 진짜 laya(0.4.31, torch 2.15 CPU)와 같은 입력·로짓·확률을 내는지 확인한다.
//! 픽스처: tests/reference/laya_reference.py가 만든 tests/fixtures/laya_reference.json (11문장 x 질문 2개, 892토큰 긴 입력 포함)
//!
//!   LAYA_DIR=<laya-multilingual 폴더> cargo test --release -j 1 --test laya_reference -- --ignored --nocapture
//!   PD_QUANT=q8_0 이면 인코더 선형층을 Q8_0으로 양자화한 채로 비교한다 (판정이 같은지와 로짓 차이)

use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::laya::{profanity_question, LayaModel, QKind, Question};
use premium_discount_app::Bits;
use serde::Deserialize;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/laya_reference.json");

#[derive(Deserialize)]
struct Fixture {
    question: serde_json::Value,
    noul: serde_json::Value,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    text: String,
    question: String,
    ids: Vec<u32>,
    markers: Vec<usize>,
    qtype: usize,
    logits: Vec<f32>,
    enc0: Option<Vec<f32>>,
    enc_markers: Option<Vec<Vec<f32>>>,
}

#[test]
#[ignore = "모델 파일이 필요하다. LAYA_DIR을 지정하고 --ignored로 실행한다"]
fn laya_matches_the_python_reference() {
    let dir = PathBuf::from(std::env::var("LAYA_DIR").expect("LAYA_DIR 환경 변수가 필요합니다"));
    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    // 앱의 질문이 픽스처를 만든 질문과 같은지 (문구가 바뀌면 픽스처를 다시 만들어야 한다)
    let q = profanity_question();
    assert_eq!(fixture.question["instructions"].as_str().unwrap(), q.instructions);
    let crit = fixture.question["criteria"].as_object().unwrap();
    let expected_options: Vec<String> = crit.iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap())).collect();
    assert_eq!(expected_options, q.options);
    let noul = Question::noul(fixture.noul["instructions"].as_str().unwrap());

    let quantized = std::env::var("PD_QUANT").map(|v| v == "q8_0").unwrap_or(false);
    let t = Instant::now();
    let model = LayaModel::load(&dir, quantized.then_some(Bits::Q8_0)).expect("laya 로드");
    println!("load {:?} (quantized: {quantized})", t.elapsed());

    let (mut max_logit, mut max_enc, mut max_prob) = (0f32, 0f32, 0f32);
    let mut same_answer = 0;
    for case in &fixture.cases {
        let question = if case.question == "profanity" { &q } else { &noul };
        assert_eq!(question.kind, if case.qtype == 0 { QKind::Choice } else { QKind::Noul });
        let state = format!("{{\"body\": {}}}", serde_json::to_string(&case.text).unwrap());
        let (ids, markers) = model.build_sequence(&state, question).unwrap();
        assert_eq!(ids, case.ids, "입력 토큰이 같아야 한다: {}", case.text);
        assert_eq!(markers, case.markers);

        let t = Instant::now();
        if let (Some(enc0), Some(encm)) = (&case.enc0, &case.enc_markers) {
            let h = model.encoder_hidden(&ids).unwrap();
            let row = |i: usize| -> Vec<f32> { h.get(i).unwrap().narrow(0, 0, 16).unwrap().to_vec1().unwrap() };
            for (got, want) in std::iter::once((row(0), enc0.clone())).chain(markers.iter().zip(encm).map(|(&m, w)| (row(m), w.clone()))) {
                for (a, b) in got.iter().zip(&want) {
                    max_enc = max_enc.max((a - b).abs());
                }
            }
        }
        let logits = model.logits(&ids, &markers, question.kind).unwrap();
        let probs = model.predict(&case.text, question).unwrap();
        let ref_probs = softmax(&case.logits);
        for (a, b) in logits.iter().zip(&case.logits) {
            max_logit = max_logit.max((a - b).abs());
        }
        for (a, b) in probs.iter().zip(&ref_probs) {
            max_prob = max_prob.max((a - b).abs());
        }
        let argmax = |v: &[f32]| v.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        if argmax(&logits) == argmax(&case.logits) {
            same_answer += 1;
        }
        println!(
            "{:<18} {:4} tokens  logits {:?} ref {:?}  {:?}  {}",
            case.question,
            ids.len(),
            logits.iter().map(|v| (v * 1e4).round() / 1e4).collect::<Vec<_>>(),
            case.logits.iter().map(|v| (v * 1e4).round() / 1e4).collect::<Vec<_>>(),
            t.elapsed(),
            case.text.chars().take(24).collect::<String>()
        );
    }
    println!("max |logit diff| {max_logit:.2e}, max |enc diff| {max_enc:.2e}, max |prob diff| {max_prob:.2e}, same answer {same_answer}/{}", fixture.cases.len());
    assert_eq!(same_answer, fixture.cases.len(), "모든 질문의 답이 같아야 한다");
    if quantized {
        assert!(max_prob < 0.05, "Q8_0: 확률 차이 {max_prob}");
    } else {
        // F32 이식: 원본 F16 가중치를 F32로 바꿔 같은 연산 (연산 순서 차이만)
        assert!(max_logit < 2e-3, "로짓 차이 {max_logit}");
        assert!(max_prob < 5e-4, "확률 차이 {max_prob}");
    }
}

fn softmax(v: &[f32]) -> Vec<f32> {
    let m = v.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let e: Vec<f32> = v.iter().map(|x| (x - m).exp()).collect();
    let s: f32 = e.iter().sum();
    e.iter().map(|x| x / s).collect()
}
