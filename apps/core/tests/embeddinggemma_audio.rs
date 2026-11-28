//! 오디오 경로를 transformers 5.17 참조 구현과 비교한다 (tests/fixtures/audio_reference.json).
//! 참조는 Gemma4AudioFeatureExtractor, Gemma4AudioModel, Gemma4MultimodalEmbedder 출력이다.
//! 모델 파일(약 1.5GB)이 필요해서 기본 실행에서는 건너뛴다.
//!
//! 실행 (PowerShell):
//!   $env:EMBEDDINGGEMMA_DIR = "C:\Users\HP\models\embeddinggemma-2"
//!   cargo test --release --test embeddinggemma_audio -- --ignored --nocapture

use std::f64::consts::PI;
use std::path::PathBuf;
use std::time::Instant;

use candle_core::{DType, Device};
use candle_nn::VarBuilder;
use premium_discount_app::embeddinggemma_audio::{audio_token_count, log_mel_spectrogram, AudioTower, NUM_MEL_BINS, OUTPUT_DIM};
use serde::Deserialize;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/audio_reference.json");
/// 토큰 하나마다 참조와의 최소 코사인 유사도
const MIN_COSINE: f32 = 0.999;
/// 타워를 양자화했을 때(PD_QUANT=q8_0)의 토큰 단위 기준. 타워 출력은 중간값이라 F32 계약(0.999)보다 낮게 두고,
/// 최종 768차원 벡터의 계약(0.999)은 embeddinggemma_multimodal.rs가 그대로 검사한다 (README 17절)
const MIN_COSINE_QUANTIZED: f32 = 0.995;
const TOWER: &str = "audio";
/// log-mel 최대 절대 오차 (f64 계산 결과를 f32로 저장하므로 1e-3이면 충분히 여유가 있다)
const MAX_MEL_ERROR: f32 = 1e-3;

#[derive(Deserialize)]
struct Fixture {
    clips: Vec<Clip>,
    /// [샘플 수, 토큰 수, 유효 프레임 수]
    token_counts: Vec<[usize; 3]>,
}

#[derive(Deserialize)]
struct Clip {
    samples: usize,
    frames: usize,
    tokens: usize,
    /// [tokens x 512], 행 우선
    embedding: Vec<f32>,
    /// [frames x 128], 행 우선. 1초 클립에만 있다
    #[serde(default)]
    mel: Option<Vec<f32>>,
}

/// 참조와 같은 합성 파형: 0.3*sin(2*pi*440*t) + 0.2*sin(2*pi*1234*t), t = i / 16000
fn synth(n: usize) -> Vec<f32> {
    (0..n)
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
fn audio_path_matches_transformers_reference() {
    let dir = PathBuf::from(std::env::var("EMBEDDINGGEMMA_DIR").expect("EMBEDDINGGEMMA_DIR 환경 변수가 필요합니다"));
    let raw = std::fs::read_to_string(FIXTURE).expect("fixture 읽기");
    let fixture: Fixture = serde_json::from_str(&raw).expect("fixture 파싱");

    let weights = dir.join("model.safetensors");
    // SAFETY: 파일은 읽기 전용으로 매핑하고, 로드 중에 바꾸지 않는다고 가정한다
    let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &Device::Cpu) }.expect("가중치 열기");
    let started = Instant::now();
    // PD_QUANT=q8_0이면 콘포머 층을 양자화한 채로 같은 참조와 비교한다 (앱 설정 quant와 같은 값)
    let quant = premium_discount_app::QuantSetting::parse(&std::env::var("PD_QUANT").unwrap_or_else(|_| "off".into())).expect("PD_QUANT");
    let bits = quant.tower_bits("desktop");
    println!("tower quant: {bits:?}");
    // 실측용: PD_TOWER_KEEP=0,1,14 (그 층은 F32), PD_TOWER_ONLY=5 (그 층만 양자화)
    let layers = |k: &str| std::env::var(k).ok().map(|v| v.split(',').filter(|s| !s.is_empty()).map(|s| s.parse::<usize>().unwrap()).collect::<Vec<_>>());
    let keep_f32 = match (layers("PD_TOWER_ONLY"), layers("PD_TOWER_KEEP")) {
        (Some(only), _) => (0..32).filter(|i| !only.contains(i)).collect(),
        (None, Some(keep)) => keep,
        (None, None) => premium_discount_app::quant::TowerQuant::default_keep(TOWER),
    };
    let q = premium_discount_app::quant::TowerQuant { bits, keep_f32, ..premium_discount_app::quant::TowerQuant::off() };
    let tower = AudioTower::load_with(&vb, &Device::Cpu, q).expect("오디오 타워 로드");
    println!("linear MB: {:.1}", tower.linear_bytes() as f64 / 1e6);
    let threshold = if bits.is_some() { MIN_COSINE_QUANTIZED } else { MIN_COSINE };
    println!("load: {:?}", started.elapsed());

    // 토큰 수 규칙: 함수 값과 참조 값이 같아야 하고, 실제 인코더 출력 길이도 같아야 한다
    for &[n, tokens, _valid] in &fixture.token_counts {
        assert_eq!(audio_token_count(n), tokens, "샘플 {n}개의 토큰 수 규칙");
        let out = tower.encode_pcm(&synth(n)).expect("인코딩");
        assert_eq!(out.dims(), &[tokens, OUTPUT_DIM], "샘플 {n}개의 인코더 출력 길이");
    }

    for clip in &fixture.clips {
        let samples = synth(clip.samples);

        if let Some(mel_ref) = &clip.mel {
            let mel = log_mel_spectrogram(&samples).expect("log-mel");
            assert_eq!(mel.dims(), &[clip.frames, NUM_MEL_BINS], "mel 모양");
            let got = mel.flatten_all().unwrap().to_vec1::<f32>().unwrap();
            let max_err = got.iter().zip(mel_ref).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            println!("mel max abs err: {max_err:.3e}");
            assert!(max_err < MAX_MEL_ERROR, "mel 최대 오차 {max_err}");
        }

        let started = Instant::now();
        let out = tower.encode_pcm(&samples).expect("인코딩");
        let elapsed = started.elapsed();
        assert_eq!(out.dims(), &[clip.tokens, OUTPUT_DIM], "토큰 모양");

        let got = out.flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let min_cos = got
            .chunks(OUTPUT_DIM)
            .zip(clip.embedding.chunks(OUTPUT_DIM))
            .map(|(g, r)| cosine(g, r))
            .fold(f32::INFINITY, f32::min);
        println!("samples={} tokens={} min per-token cos={min_cos:.6} encode={elapsed:?}", clip.samples, clip.tokens);
        // PD_MEASURE=1이면 재기만 한다 (층별 민감도 실측용). 기본은 계약 검사
        if std::env::var("PD_MEASURE").is_err() {
            assert!(min_cos >= threshold, "토큰 코사인 최소 {min_cos} < {threshold}");
        }
    }
}
