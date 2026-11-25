//! 실모델 부분 검증(프로브): 로드 시간·상주 메모리·텍스트 참조 벡터와의 코사인을 한 번에 잰다.
//! 전체 참조 테스트(tests/embeddinggemma_*.rs)를 돌리기 전에 바뀐 경로만 빠르게 대조하는 용도다.
//!
//!   cargo run --release -j 1 --example model_probe -- <모델 폴더> [--modalities auto|text] [--quant off|q8_0] [--image]
//!
//! --image: 합성 이미지 한 장을 임베딩해 비전 타워의 지연 로드와 그 실측 비용을 본다 (코사인은 tests/embeddinggemma_vision.rs가 본다).
//! 출력 마지막 줄은 JSON 요약이다.

use std::path::PathBuf;
use std::time::Instant;

use sha2::Digest;

use lv_core::gemma_vision::RgbImage;
use premium_discount_app::embeddinggemma::{with_prompt, Role};
use premium_discount_app::{Embedder, EmbeddingGemma, LoadOptions, Modalities, Part, QuantSetting};
use serde::Deserialize;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/embeddinggemma_reference.json");

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    role: String,
    text: String,
    vector: Vec<f32>,
}

fn rss_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = s.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb / 1024)
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    dot / (na * nb)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(args.first().expect("모델 폴더를 주세요"));
    let mut options = LoadOptions::default();
    let mut image = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--modalities" => {
                i += 1;
                options.modalities = Modalities::parse(&args[i]).expect("modalities");
            }
            "--quant" => {
                i += 1;
                options.quant = QuantSetting::parse(&args[i]).expect("quant").recipe("desktop");
            }
            "--image" => image = true,
            other => panic!("알 수 없는 인자: {other}"),
        }
        i += 1;
    }

    let rss_start = rss_mb();
    let t = Instant::now();
    let model = EmbeddingGemma::load_with(&dir, options).expect("모델 로드");
    let load_ms = t.elapsed().as_millis();
    let rss_loaded = rss_mb();
    println!(
        "load {load_ms} ms, rss {rss_start:?} -> {rss_loaded:?} MB, text layers {:.1} MB, quant cache {:?}",
        model.text_layer_bytes() as f64 / 1e6,
        model.quant_cache()
    );

    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("fixture")).expect("fixture json");
    let mut min_cos = f64::MAX;
    // 출력 벡터 전체의 비트 해시 (캐시 유무로 결과가 같은지 비교)
    let mut bits = sha2::Sha256::new();
    let mut max_norm_err = 0f64;
    let t = Instant::now();
    for case in &fixture.cases {
        let role = match case.role.as_str() {
            "query" => Role::Query,
            "document" => Role::Document,
            _ => Role::Similarity,
        };
        let v = model.embed(&with_prompt(role, &case.text)).expect("embed");
        for x in &v {
            sha2::Digest::update(&mut bits, x.to_bits().to_le_bytes());
        }
        let c = cosine(&v, &case.vector);
        let norm = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        min_cos = min_cos.min(c);
        max_norm_err = max_norm_err.max((norm - 1.0).abs());
        println!("  {:<10} cos {c:.6} norm {norm:.6}  {}", case.role, case.text.chars().take(30).collect::<String>());
    }
    let text_ms = t.elapsed().as_millis();
    let rss_text = rss_mb();
    let vector_bits: String = sha2::Digest::finalize(bits).iter().take(8).map(|b| format!("{b:02x}")).collect();
    println!(
        "text {} cases {text_ms} ms, min cos {min_cos:.6}, max |norm-1| {max_norm_err:.2e}, rss {rss_text:?} MB, vector bits {vector_bits}",
        fixture.cases.len()
    );

    let mut image_result = serde_json::Value::Null;
    if image {
        let (w, h) = (320u32, 240u32);
        let pixels: Vec<u8> = (0..w * h).flat_map(|p| [(p % w) as u8, (p / w) as u8, ((p * 7) % 251) as u8]).collect();
        let img = RgbImage::new(w as usize, h as usize, pixels).expect("image");
        let t = Instant::now();
        let r = model.embed_parts(&[Part::Image(&img)]);
        image_result = serde_json::json!({
            "ok": r.is_ok(),
            "error": r.as_ref().err().map(|e| e.to_string()),
            "ms": t.elapsed().as_millis() as u64,
            "rss_mb": rss_mb(),
        });
        println!("image: {image_result}");
    }
    let report = model.residency();
    println!("residency: {}", serde_json::to_string(&report).unwrap());
    println!(
        "{}",
        serde_json::json!({
            "load_ms": load_ms as u64, "text_layer_mb": model.text_layer_bytes() as f64 / 1e6, "rss_start_mb": rss_start, "rss_loaded_mb": rss_loaded, "rss_text_mb": rss_text,
            "text_ms": text_ms as u64, "min_cos": min_cos, "max_norm_err": max_norm_err, "image": image_result,
            "residency": report,
        })
    );
}
