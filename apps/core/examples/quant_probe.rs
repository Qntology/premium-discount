//! 양자화 실측 프로브: 레시피마다 모델을 열어 텍스트 참조 벡터(Python F32)와의 코사인, 층 가중치 크기, 로드·임베딩 시간을 잰다.
//! 지시서의 "고정 계층 할당이 아니라 실측 민감도로 배분"을 위한 도구다. 결과는 한 줄에 JSON 하나.
//!
//!   cargo run --release -j 1 --example quant_probe -- <모델 폴더> <레시피>...
//! 레시피:
//!   off                                   F32 (기준)
//!   uniform:<묶음>:<비트>                  모든 층의 그 묶음 (묶음: all | attn | mlp_in | mlp_out | ple, 쉼표로 여럿)
//!   only:<층>:<묶음>:<비트>                그 층들만 (층: 5 | 0,5,11 | 0-23)
//!   sweep-layers:<비트>                    층마다 only:i:all:<비트> (층별 민감도)
//!   sweep-groups                          묶음 x 비트마다 uniform (묶음별 민감도)
//!   pyramid                               지시서의 4계층 초안 (quant::pyramid_v1)
//!   measured-mix                          실측 민감도로 고른 혼합안 (gate·up 층 3~21만 Q6_K, 나머지 Q8_0)
//! 비트: q8_0 | q6k | q5k | q4k
//! 계산 방법: 환경 변수 PD_QUANT_KERNEL = dequant(기본, 계산 직전에 F32로 풀기) | native(candle 양자화 행렬곱)

use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::embeddinggemma::{with_prompt, Role};
use premium_discount_app::quant::{pyramid_v1, Bits, Kernel, ProjGroup, QuantRecipe};
use premium_discount_app::{Embedder, EmbeddingGemma, LoadOptions, Modalities};
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

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64) * (*y as f64)).sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    dot / (na * nb)
}

fn groups(spec: &str) -> Vec<ProjGroup> {
    if spec == "all" {
        return ProjGroup::ALL.to_vec();
    }
    spec.split(',').map(|g| ProjGroup::parse(g).unwrap_or_else(|| panic!("묶음: {g}"))).collect()
}

fn layers(spec: &str) -> Vec<usize> {
    spec.split(',')
        .flat_map(|part| match part.split_once('-') {
            Some((a, b)) => (a.parse::<usize>().unwrap()..=b.parse::<usize>().unwrap()).collect::<Vec<_>>(),
            None => vec![part.parse().unwrap()],
        })
        .collect()
}

fn bits(s: &str) -> Bits {
    Bits::parse(s).unwrap_or_else(|| panic!("비트: {s}"))
}

fn expand(spec: &str) -> Vec<(String, QuantRecipe)> {
    let parts: Vec<&str> = spec.split(':').collect();
    match parts.as_slice() {
        ["off"] => vec![(spec.into(), QuantRecipe::Off)],
        ["pyramid"] => vec![(spec.into(), pyramid_v1())],
        // 실측 민감도로 고른 혼합안: 가장 큰 텐서(gate·up) 중 둔감한 층(3~21)만 Q6_K, 나머지 Q8_0
        ["measured-mix"] => {
            let mut rows = Vec::new();
            for layer in 0..24 {
                for g in ProjGroup::ALL {
                    let b = if g == ProjGroup::MlpIn && (3..=21).contains(&layer) { Bits::Q6K } else { Bits::Q8_0 };
                    rows.push((layer, g, b));
                }
            }
            vec![(spec.into(), QuantRecipe::Table(rows))]
        }
        ["uniform", g, b] => vec![(spec.into(), QuantRecipe::Uniform { groups: groups(g), bits: bits(b) })],
        ["only", l, g, b] => vec![(spec.into(), QuantRecipe::Only { layers: layers(l), groups: groups(g), bits: bits(b) })],
        ["sweep-layers", b] => (0..24)
            .map(|i| (format!("only:{i}:all:{b}"), QuantRecipe::Only { layers: vec![i], groups: ProjGroup::ALL.to_vec(), bits: bits(b) }))
            .collect(),
        ["sweep-groups"] => ProjGroup::ALL
            .iter()
            .flat_map(|g| {
                [Bits::Q8_0, Bits::Q6K, Bits::Q5K, Bits::Q4K]
                    .map(|b| (format!("uniform:{}:{}", g.name(), b.name()), QuantRecipe::Uniform { groups: vec![*g], bits: b }))
            })
            .collect(),
        _ => panic!("레시피를 알 수 없습니다: {spec}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&args[0]);
    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("fixture")).expect("fixture json");
    for spec in &args[1..] {
        for (name, recipe) in expand(spec) {
            let t = Instant::now();
            let kernel = Kernel::parse(&std::env::var("PD_QUANT_KERNEL").unwrap_or_default()).expect("PD_QUANT_KERNEL: dequant | native");
            let model = EmbeddingGemma::load_with(&dir, LoadOptions { modalities: Modalities::Text, quant: recipe, kernel, tower_bits: None, ..Default::default() }).expect("모델 로드");
            let load_ms = t.elapsed().as_millis() as u64;
            let t = Instant::now();
            let mut cos = Vec::new();
            for case in &fixture.cases {
                let role = match case.role.as_str() {
                    "query" => Role::Query,
                    "document" => Role::Document,
                    _ => Role::Similarity,
                };
                let v = model.embed(&with_prompt(role, &case.text)).expect("embed");
                cos.push(cosine(&v, &case.vector));
            }
            let embed_ms = t.elapsed().as_millis() as u64;
            let min = cos.iter().cloned().fold(f64::MAX, f64::min);
            let mean = cos.iter().sum::<f64>() / cos.len() as f64;
            println!(
                "{}",
                serde_json::json!({
                    "recipe": name, "kernel": format!("{kernel:?}"), "min_cos": min, "mean_cos": mean, "cos": cos,
                    "layer_mb": model.text_layer_bytes() as f64 / 1e6, "load_ms": load_ms, "embed_ms": embed_ms,
                })
            );
        }
    }
}
