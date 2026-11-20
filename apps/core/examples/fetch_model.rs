//! 모델을 명령줄에서 내려받는다 (앱의 "모델 내려받기"와 같은 코드).
//!
//!   cargo run --release --example fetch_model -- <모델 폴더> text|full [받는 주소]
//!
//! 받는 주소를 생략하면 https://huggingface.co (리비전·해시 고정은 model_fetch.rs).

use std::time::{Duration, Instant};

use premium_discount_app::model_fetch::{FetchState, ModelFetcher, Variant, DEFAULT_BASE_URL};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.first().expect("모델 폴더를 주세요").clone();
    let variant = match args.get(1).map(String::as_str) {
        Some("full") => Variant::Full,
        Some("text") | None => Variant::Text,
        Some(other) => panic!("text 또는 full: {other}"),
    };
    let base = args.get(2).cloned().unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let fetcher = ModelFetcher::new(&dir, base).expect("fetcher");
    let started = Instant::now();
    fetcher.start(variant);
    loop {
        let p = fetcher.progress();
        eprintln!(
            "{:?} {:>7.1} / {:.1} MB  {}",
            p.state,
            p.done_bytes as f64 / 1e6,
            p.total_bytes as f64 / 1e6,
            p.file.as_deref().unwrap_or("")
        );
        if p.state != FetchState::Running {
            println!("{}", serde_json::to_string(&fetcher.status()).unwrap());
            println!("elapsed {:.1}s", started.elapsed().as_secs_f64());
            if p.state != FetchState::Done {
                std::process::exit(1);
            }
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
