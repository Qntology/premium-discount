//! Qwen3.5-0.8B Rust 이식이 transformers 5.23(F32)과 같은 프롬프트 토큰·그리디 생성·로짓을 내는지 확인한다.
//! 픽스처: tests/reference/qwen35_reference.py가 만든 tests/fixtures/qwen35_reference.json
//!
//!   QWEN_DIR=<Qwen3.5-0.8B 폴더> cargo test --release -j 1 --test qwen35_reference -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::qwen35::{chat_prompt, Qwen35};
use serde::Deserialize;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/qwen35_reference.json");

#[derive(Deserialize)]
struct Fixture {
    system: String,
    cases: Vec<Case>,
    plain: Plain,
}

#[derive(Deserialize)]
struct Case {
    page: String,
    prompt: String,
    ids: Vec<u32>,
    generated: Vec<u32>,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    ids: Vec<u32>,
    values: Vec<f32>,
}

#[derive(Deserialize)]
struct Plain {
    ids: Vec<u32>,
    top_ids: Vec<u32>,
    top_values: Vec<f32>,
}

#[test]
#[ignore = "모델 파일이 필요하다. QWEN_DIR을 지정하고 --ignored로 실행한다"]
fn qwen35_matches_transformers() {
    let dir = PathBuf::from(std::env::var("QWEN_DIR").expect("QWEN_DIR 환경 변수가 필요합니다"));
    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    let t = Instant::now();
    let model = Qwen35::load(&dir, None).expect("모델 로드");
    let head = model.full_head().expect("전체 헤드");
    println!("load {:?}", t.elapsed());

    // 순수 텍스트: 다음 토큰 로짓 상위 20
    let mut st = model.new_state();
    let h = model.forward(&fixture.plain.ids, &mut st).unwrap();
    let logits = model.logits(&h, &head).unwrap();
    let mut max_diff = 0f32;
    for (id, v) in fixture.plain.top_ids.iter().zip(&fixture.plain.top_values) {
        max_diff = max_diff.max((logits[*id as usize] - v).abs());
    }
    let argmax = logits.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0 as u32;
    println!("plain: argmax {argmax} (ref {}), top-20 max |diff| {max_diff:.2e}", fixture.plain.top_ids[0]);
    assert_eq!(argmax, fixture.plain.top_ids[0]);
    assert!(max_diff < 2e-2, "로짓 차이 {max_diff}");

    for case in &fixture.cases {
        let prompt = chat_prompt(&fixture.system, &case.page);
        assert_eq!(prompt, case.prompt, "채팅 템플릿 결과가 같아야 한다");
        let ids = model.encode(&prompt);
        assert_eq!(ids, case.ids, "프롬프트 토큰이 같아야 한다");

        // 그리디 생성하면서 각 단계의 로짓을 참조의 상위 10과 비교한다
        let t = Instant::now();
        let mut st = model.new_state();
        let mut h = model.forward(&ids, &mut st).unwrap();
        let prefill = t.elapsed();
        let mut out = Vec::new();
        let mut step_diff = 0f32;
        for (s, step) in case.steps.iter().enumerate() {
            let logits = model.logits(&h, &head).unwrap();
            for (id, v) in step.ids.iter().zip(&step.values) {
                step_diff = step_diff.max((logits[*id as usize] - v).abs());
            }
            let next = logits.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0 as u32;
            out.push(next);
            if next != case.generated[s] {
                break;
            }
            h = model.forward(&[next], &mut st).unwrap();
        }
        let per_token = t.elapsed().saturating_sub(prefill) / out.len().max(1) as u32;
        println!(
            "{} prompt tokens: prefill {prefill:?}, {per_token:?}/token, same tokens {}/{}, max |logit diff| {step_diff:.2e}  {}",
            ids.len(),
            out.iter().zip(&case.generated).take_while(|(a, b)| a == b).count(),
            case.generated.len(),
            model.decode(&out).replace('\n', "⏎")
        );
        assert_eq!(out, case.generated, "그리디 생성이 같아야 한다");
        assert!(step_diff < 5e-2, "로짓 차이 {step_diff}");
    }
}
