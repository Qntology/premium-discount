//! 멀티모달 조립 (텍스트 + 이미지 + 오디오 + 비디오 프레임)을 transformers 조립 참조와 비교한다.
//! 참조는 tests/fixtures/multimodal_reference.json (생성 스크립트 ref_multimodal.py는 리포지터리 밖).
//! 입력은 두 언어에서 같은 수식으로 만든다: 이미지 (x*7 + y*3 + c*50) % 256, 오디오 0.3*sin(2pi*440t) + 0.2*sin(2pi*1234t).
//! 모델 파일이 필요해서 기본 실행에서는 건너뛴다.
//!
//! 실행 (PowerShell):
//!   $env:EMBEDDINGGEMMA_DIR = "C:\Users\HP\models\embeddinggemma-2"
//!   cargo test --release --test embeddinggemma_multimodal -- --ignored --nocapture

use std::f64::consts::PI;
use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::embeddinggemma::{with_prompt, Role};
use premium_discount_app::gemma_vision::RgbImage;
use premium_discount_app::{EmbeddingGemma, LoadOptions, Part, QuantSetting};
use serde::Deserialize;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/multimodal_reference.json");
/// 참조 벡터와의 최소 코사인 유사도
const MIN_COSINE: f32 = 0.999;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    tokens: usize,
    vector: Vec<f32>,
}

/// 조각을 소유하는 형태. `Part`는 참조만 들고 있으므로 입력을 먼저 여기에 둔다.
enum Owned {
    Text(String),
    Image(RgbImage),
    Video(Vec<RgbImage>),
    Audio(Vec<f32>),
}

impl Owned {
    fn as_part(&self) -> Part<'_> {
        match self {
            Owned::Text(text) => Part::Text(text),
            Owned::Image(image) => Part::Image(image),
            Owned::Video(frames) => Part::Video(frames),
            Owned::Audio(samples) => Part::Audio(samples),
        }
    }
}

/// ref_vision.formula_image와 같은 수식. 픽셀은 행 우선, RGB 인터리브
fn formula_image(width: usize, height: usize) -> RgbImage {
    let mut pixels = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            for c in 0..3 {
                pixels.push(((x * 7 + y * 3 + c * 50) % 256) as u8);
            }
        }
    }
    RgbImage::new(width, height, pixels).expect("formula image size")
}

/// ref_audio.synth와 같은 신호 (f64로 계산하고 f32로 저장)
fn synth(num_samples: usize) -> Vec<f32> {
    (0..num_samples)
        .map(|i| {
            let t = i as f64 / 16000.0;
            (0.3 * (2.0 * PI * 440.0 * t).sin() + 0.2 * (2.0 * PI * 1234.0 * t).sin()) as f32
        })
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

#[test]
#[ignore = "모델 파일이 필요하다. EMBEDDINGGEMMA_DIR을 지정하고 --ignored로 실행한다"]
fn multimodal_sequences_match_transformers_reference() {
    let dir = PathBuf::from(
        std::env::var("EMBEDDINGGEMMA_DIR").expect("EMBEDDINGGEMMA_DIR 환경 변수가 필요합니다"),
    );
    let raw = std::fs::read_to_string(FIXTURE).expect("fixture 읽기");
    let fixture: Fixture = serde_json::from_str(&raw).expect("fixture 파싱");
    // PD_QUANT=q8_0이면 텍스트 백본을 양자화한 채로 같은 참조와 비교한다 (계약: 코사인 0.999 이상, 앱 설정 quant와 같은 값)
    let quant = QuantSetting::parse(&std::env::var("PD_QUANT").unwrap_or_else(|_| "off".into())).expect("PD_QUANT");
    println!("quant: {quant:?}");
    let model = EmbeddingGemma::load_with(&dir, LoadOptions { quant: quant.recipe("desktop"), tower_bits: quant.tower_bits("desktop"), ..Default::default() }).expect("모델 로드");

    let document = |text: &str| with_prompt(Role::Document, text);
    let cases: Vec<(&str, Vec<Owned>)> = vec![
        (
            "text_only",
            vec![Owned::Text(document("좋은 평가입니다. 응답이 빨랐습니다."))],
        ),
        (
            "text_image_text",
            vec![
                Owned::Text("title: none | text: 사진 ".to_string()),
                Owned::Image(formula_image(300, 200)),
                Owned::Text(" 리뷰".to_string()),
            ],
        ),
        ("audio_only", vec![Owned::Audio(synth(16000))]),
        (
            "image_audio",
            vec![
                Owned::Image(formula_image(384, 384)),
                Owned::Audio(synth(8000)),
            ],
        ),
        (
            "video_two_frames",
            vec![Owned::Video(vec![formula_image(300, 200), formula_image(384, 384)])],
        ),
    ];

    let mut failures = Vec::new();
    for (name, owned) in &cases {
        let case = fixture
            .cases
            .iter()
            .find(|c| c.name == *name)
            .unwrap_or_else(|| panic!("fixture에 {name} 없음"));
        let parts: Vec<Part> = owned.iter().map(Owned::as_part).collect();

        let started = Instant::now();
        let vector = model.embed_parts(&parts).expect("멀티모달 임베딩");
        let elapsed = started.elapsed();

        assert_eq!(vector.len(), case.vector.len(), "차원이 참조와 다릅니다");
        let cos = cosine(&vector, &case.vector);
        println!(
            "{name:18} reference tokens={:>4} cos={cos:.6} time={elapsed:?}",
            case.tokens
        );
        if cos < MIN_COSINE {
            failures.push(format!("{name}: cos {cos} < {MIN_COSINE}"));
        }
    }
    assert!(failures.is_empty(), "참조와 다른 케이스: {failures:?}");
}
