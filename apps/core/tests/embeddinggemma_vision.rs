//! embeddinggemma-2 비전 경로를 transformers 5.17 참조와 비교한다.
//! - tests/fixtures/vision_reference.json: 토큰마다 코사인 유사도를 잰다 (ref_vision.py가 만든 값).
//! - VISION_REF_DIR의 덤프: 리사이즈 결과와 패치 픽셀, 위치를 원소 단위로 비교한다 (ref_vision.py 두 번째 인자).
//! 모델 파일(약 1.5GB)이 필요해서 기본 실행에서는 건너뛴다.
//!
//! 실행 (PowerShell):
//!   $env:EMBEDDINGGEMMA_DIR = "C:\Users\HP\models\embeddinggemma-2"
//!   cargo test --test embeddinggemma_vision -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::time::Instant;

use candle_core::{DType, Device};
use candle_nn::VarBuilder;
use lv_core::gemma_vision::{
    preprocess, resize_bicubic, target_size, RgbImage, VisionTower, SOFT_TOKEN_DIM,
};
use lv_core::quant::{Bits as LvBits, TowerQuant};
use serde::Deserialize;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vision_reference.json");
/// 토큰마다 잰 최소 코사인 유사도
const MIN_COSINE: f32 = 0.999;
/// 타워를 양자화했을 때(PD_QUANT=q8_0)의 토큰 단위 기준. 타워 출력은 중간값이라 F32 계약(0.999)보다 낮게 두고,
/// 최종 768차원 벡터의 계약(0.999)은 embeddinggemma_multimodal.rs가 그대로 검사한다 (README 17절)
const MIN_COSINE_QUANTIZED: f32 = 0.995;
const TOWER: &str = "vision";

#[derive(Deserialize)]
struct Fixture {
    model: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    height: usize,
    width: usize,
    max_soft_tokens: usize,
    tokens: usize,
    vector: Vec<Vec<f32>>,
}

fn read_fixture() -> Fixture {
    let raw = std::fs::read_to_string(FIXTURE).expect("fixture 읽기");
    serde_json::from_str(&raw).expect("fixture 파싱")
}

/// ref_vision.py의 formula_image와 같은 수식: (x*7 + y*3 + c*50) % 256
fn formula(height: usize, width: usize) -> RgbImage {
    let mut pixels = Vec::with_capacity(height * width * 3);
    for y in 0..height {
        for x in 0..width {
            for c in 0..3 {
                pixels.push(((x * 7 + y * 3 + c * 50) % 256) as u8);
            }
        }
    }
    RgbImage::new(width, height, pixels).expect("테스트 이미지")
}

fn load_tower(dir: &Path) -> VisionTower {
    let file = dir.join("model.safetensors");
    // SAFETY: 모델 파일을 읽기 전용으로 매핑하고 실행 중에 바꾸지 않는다고 가정한다 (텍스트 테스트와 같다)
    let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[file], DType::F32, &Device::Cpu).expect("가중치 매핑") };
    // PD_QUANT=q8_0이면 인코더 층을 양자화한 채로 같은 참조와 비교한다 (앱 설정 quant와 같은 값)
    let quant = premium_discount_app::QuantSetting::parse(&std::env::var("PD_QUANT").unwrap_or_else(|_| "off".into())).expect("PD_QUANT");
    let bits = quant.tower_bits("desktop").and_then(|b| LvBits::parse(b.name()));
    println!("tower quant: {bits:?}");
    // 실측용: PD_TOWER_KEEP=0,1,14 (그 층은 F32), PD_TOWER_ONLY=5 (그 층만 양자화)
    let layers = |k: &str| std::env::var(k).ok().map(|v| v.split(',').filter(|s| !s.is_empty()).map(|s| s.parse::<usize>().unwrap()).collect::<Vec<_>>());
    let keep_f32 = match (layers("PD_TOWER_ONLY"), layers("PD_TOWER_KEEP")) {
        (Some(only), _) => (0..32).filter(|i| !only.contains(i)).collect(),
        (None, Some(keep)) => keep,
        (None, None) => TowerQuant::default_keep(TOWER),
    };
    let q = TowerQuant { bits, kernel: Default::default(), cache: None, keep_f32 };
    let tower = VisionTower::load_with(&vb, &Device::Cpu, q).expect("비전 타워 로드");
    println!("linear MB: {:.1}", tower.linear_bytes() as f64 / 1e6);
    tower
}

fn model_dir() -> PathBuf {
    PathBuf::from(std::env::var("EMBEDDINGGEMMA_DIR").expect("EMBEDDINGGEMMA_DIR 환경 변수가 필요합니다"))
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

fn read_f32_le(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().expect("4바이트")))
        .collect()
}

fn read_i32_le(path: &Path) -> Vec<i32> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes
        .chunks_exact(4)
        .map(|c| i32::from_le_bytes(c.try_into().expect("4바이트")))
        .collect()
}

#[test]
#[ignore = "모델 파일이 필요하다. EMBEDDINGGEMMA_DIR을 지정하고 --ignored로 실행한다"]
fn image_and_video_tokens_match_python_reference() {
    let fixture = read_fixture();
    assert_eq!(fixture.model, "google/embeddinggemma-2");
    let tower = load_tower(&model_dir());
    let threshold = if std::env::var("PD_QUANT").map(|v| v != "off").unwrap_or(false) { MIN_COSINE_QUANTIZED } else { MIN_COSINE };

    for case in &fixture.cases {
        let image = formula(case.height, case.width);
        let started = Instant::now();
        let tokens = tower.encode_image(&image, case.max_soft_tokens).expect("인코딩");
        let elapsed = started.elapsed();

        assert_eq!(tokens.dims().to_vec(), vec![case.tokens, SOFT_TOKEN_DIM], "{}", case.name);
        let rows = tokens.to_vec2::<f32>().expect("행 단위로 읽기");
        let mut worst = 1.0f32;
        for (got, want) in rows.iter().zip(&case.vector) {
            worst = worst.min(cosine(got, want));
        }
        println!("{:24} tokens={:4} min_cos={worst:.6} time={elapsed:?}", case.name, case.tokens);
        // PD_MEASURE=1이면 재기만 한다 (층별 민감도 실측용). 기본은 계약 검사
        if std::env::var("PD_MEASURE").is_err() {
            assert!(worst >= threshold, "{}: 토큰 최소 코사인 {worst} < {threshold}", case.name);
        }
    }
}

#[test]
#[ignore = "VISION_REF_DIR에 ref_vision.py 덤프가 필요하다"]
fn preprocessing_matches_python_dumps() {
    let dir = PathBuf::from(std::env::var("VISION_REF_DIR").expect("VISION_REF_DIR 환경 변수가 필요합니다"));
    let fixture = read_fixture();

    for case in &fixture.cases {
        let image = formula(case.height, case.width);
        let (th, tw) = target_size(case.height, case.width, case.max_soft_tokens).expect("목표 크기");

        // 리사이즈 결과 (8비트). 덤프는 HWC 순서다.
        let resized = resize_bicubic(&image, th, tw);
        let want_resized = std::fs::read(dir.join(format!("resized_{}.u8", case.name))).expect("리사이즈 덤프");
        assert_eq!(want_resized.len(), resized.pixels.len(), "{}", case.name);
        let mut max_u8 = 0i32;
        let mut differing = 0usize;
        for (got, want) in resized.pixels.iter().zip(&want_resized) {
            let d = (*got as i32 - *want as i32).abs();
            max_u8 = max_u8.max(d);
            differing += usize::from(d != 0);
        }

        // 패치 픽셀 (2 * (v - 0.5))과 위치 (x, y)
        let prep = preprocess(&image, case.max_soft_tokens).expect("전처리");
        let want_px = read_f32_le(&dir.join(format!("pixels_{}.f32", case.name)));
        assert_eq!(prep.pixels.len(), want_px.len(), "{}", case.name);
        let max_px = prep
            .pixels
            .iter()
            .zip(&want_px)
            .map(|(g, w)| (g - w).abs())
            .fold(0f32, f32::max);
        let want_pos = read_i32_le(&dir.join(format!("positions_{}.i32", case.name)));
        let pos_mismatch = prep
            .positions
            .iter()
            .zip(want_pos.chunks_exact(2))
            .filter(|(g, w)| g[0] as i32 != w[0] || g[1] as i32 != w[1])
            .count();

        println!(
            "{:24} resize: max_abs={max_u8} differ={differing}/{} | pixels: max_abs={max_px:.2e} | positions mismatch={pos_mismatch}",
            case.name,
            want_resized.len()
        );
        // 리사이즈 1단계 차이는 픽셀 값 2/255만큼 입력을 바꾼다
        assert!(max_u8 <= 1, "{}: 리사이즈 최대 차이 {max_u8}", case.name);
        assert!(max_px <= 2.0 / 255.0 + 1e-6, "{}: 패치 픽셀 최대 차이 {max_px}", case.name);
        assert_eq!(pos_mismatch, 0, "{}: 위치가 다르다", case.name);
    }
}
