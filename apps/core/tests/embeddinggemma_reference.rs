//! embeddinggemma-2 텍스트 경로를 Python 참조 구현과 비교한다.
//! 참조 벡터는 tests/fixtures/embeddinggemma_reference.json (양방향 모드, 생성 스크립트는 리포지터리 밖).
//! 모델 파일(약 1.5GB)이 필요해서 기본 실행에서는 건너뛴다.
//!
//! 실행 (PowerShell):
//!   $env:EMBEDDINGGEMMA_DIR = "C:\Users\HP\models\embeddinggemma-2"
//!   cargo test --test embeddinggemma_reference -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::embeddinggemma::{with_prompt, Role};
use premium_discount_app::{Embedder, EmbeddingGemma, LoadOptions, QuantSetting};
use serde::Deserialize;
use tokenizers::Tokenizer;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/embeddinggemma_reference.json");
/// 참조 벡터와의 최소 코사인 유사도 (f32 연산 순서 차이만 허용)
const MIN_COSINE: f32 = 0.999;

#[derive(Deserialize)]
struct Fixture {
    model: String,
    mode: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    role: String,
    text: String,
    ids: Vec<u32>,
    vector: Vec<f32>,
}

fn role_of(name: &str) -> Role {
    match name {
        "query" => Role::Query,
        "document" => Role::Document,
        "similarity" => Role::Similarity,
        other => panic!("fixture에 알 수 없는 역할: {other}"),
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

#[test]
#[ignore = "모델 파일이 필요하다. EMBEDDINGGEMMA_DIR을 지정하고 --ignored로 실행한다"]
fn text_path_matches_python_reference() {
    let dir = PathBuf::from(
        std::env::var("EMBEDDINGGEMMA_DIR").expect("EMBEDDINGGEMMA_DIR 환경 변수가 필요합니다"),
    );
    let raw = std::fs::read_to_string(FIXTURE).expect("fixture 읽기");
    let fixture: Fixture = serde_json::from_str(&raw).expect("fixture 파싱");
    assert_eq!(fixture.model, "google/embeddinggemma-2");
    assert_eq!(fixture.mode, "bidirectional");

    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).expect("tokenizer.json 로드");
    let started = Instant::now();
    // PD_QUANT=q8_0이면 텍스트 백본을 양자화한 채로 같은 참조와 비교한다 (계약: 코사인 0.999 이상, 앱 설정 quant와 같은 값)
    let quant = QuantSetting::parse(&std::env::var("PD_QUANT").unwrap_or_else(|_| "off".into())).expect("PD_QUANT");
    println!("quant: {quant:?}");
    let model = EmbeddingGemma::load_with(&dir, LoadOptions { quant: quant.recipe("desktop"), ..Default::default() }).expect("모델 로드");
    println!("load: {:?}", started.elapsed());

    for case in &fixture.cases {
        let prompted = with_prompt(role_of(&case.role), &case.text);
        let encoding = tokenizer.encode(prompted.as_str(), true).expect("토큰화");
        assert_eq!(encoding.get_ids(), case.ids.as_slice(), "토큰 id가 참조와 다릅니다: {}", case.text);

        let started = Instant::now();
        let vector = model.embed(&prompted).expect("임베딩");
        let elapsed = started.elapsed();

        assert_eq!(vector.len(), case.vector.len(), "차원이 참조와 다릅니다");
        let cos = cosine(&vector, &case.vector);
        println!(
            "{:>10} tokens={:>3} cos={cos:.6} time={elapsed:?}  {}",
            case.role,
            case.ids.len(),
            case.text.chars().take(16).collect::<String>()
        );
        assert!(cos >= MIN_COSINE, "코사인 {cos} < {MIN_COSINE}: {}", case.text);
    }
}

/// 양자화 캐시: 원본에서 양자화하고 캐시를 쓴 결과와, 캐시를 읽은 결과가 비트 단위로 같다 (재양자화가 없다)
#[test]
#[ignore = "모델 파일이 필요하다. EMBEDDINGGEMMA_DIR을 지정하고 --ignored로 실행한다"]
fn the_quantization_cache_gives_bit_identical_vectors() {
    use premium_discount_app::embeddinggemma::QuantCache;
    let dir = PathBuf::from(std::env::var("EMBEDDINGGEMMA_DIR").expect("EMBEDDINGGEMMA_DIR 환경 변수가 필요합니다"));
    let _ = std::fs::remove_dir_all(dir.join(".pd-cache"));
    let options = || LoadOptions { quant: QuantSetting::Q8_0.recipe("desktop"), ..Default::default() };
    let texts = ["매너가 좋고 응답이 빨라서 만족합니다.", "The northern lights are visible tonight."];
    let run = || {
        let model = EmbeddingGemma::load_with(&dir, options()).expect("모델 로드");
        let vectors: Vec<Vec<u32>> =
            texts.iter().map(|t| model.embed(&with_prompt(Role::Document, t)).unwrap().iter().map(|x| x.to_bits()).collect()).collect();
        (model.quant_cache(), vectors)
    };
    let (first, built) = run();
    assert_eq!(first, QuantCache::Built);
    let (second, hit) = run();
    assert_eq!(second, QuantCache::Hit);
    assert_eq!(built, hit, "캐시를 읽어도 벡터가 비트 단위로 같다");
}
