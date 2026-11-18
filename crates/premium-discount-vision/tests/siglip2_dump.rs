//! (--ignored) logis-center 와 같은 입력으로 SigLIP2 타워 출력을 씁니다 (CPU F32). logis-center 의
//! `tests/it/synthetic_eval.rs::siglip2_dump` 가 먼저 출력 폴더에 디코딩한 RGB8(`<이름>.rgb` · `.dims`)을 남기고, 이 시험은 그
//! 픽셀로 시작합니다. 이미지마다 `.pd.pix`(패치 행, 칸 순서 · 패치 안 채널 우선) · `.pd.grid` · `.pd.pooled` · `.pd.dense`,
//! 문장은 `texts.pd.ids.json` · `texts.pd.f32` (벡터는 f32 little-endian, 모두 단위 길이).
//!   PD_SIGLIP_DIR=<siglip2-so400m-patch16-naflex> PD_DUMP_TEXTS=<문장 JSON> PD_DUMP_OUT=<폴더> cargo test --test siglip2_dump -- --ignored

use candle_core::Device;
use premium_discount_sensor::naflex::{patchify_frame_with, DEFAULT_MAX_PATCHES};
use premium_discount_vision::text::{SiglipTokenizer, TextTower};
use premium_discount_vision::{load_vision, preprocess_for, WeightFormat};
use std::path::PathBuf;

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[test]
#[ignore = "PD_SIGLIP_DIR · PD_DUMP_TEXTS · PD_DUMP_OUT 필요"]
fn siglip2_dump() {
    let dir = PathBuf::from(std::env::var("PD_SIGLIP_DIR").expect("PD_SIGLIP_DIR"));
    let out = PathBuf::from(std::env::var("PD_DUMP_OUT").expect("PD_DUMP_OUT"));
    let texts: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(std::env::var("PD_DUMP_TEXTS").expect("PD_DUMP_TEXTS")).unwrap()).unwrap();
    let device = Device::Cpu;
    let ckpt = dir.join("model.safetensors");

    let tok = SiglipTokenizer::load(&dir.join("tokenizer.json")).expect("tokenizer");
    let ids: Vec<Vec<u32>> = texts.iter().map(|t| tok.encode(t).expect("encode")).collect();
    std::fs::write(out.join("texts.pd.ids.json"), serde_json::to_string(&ids).unwrap()).unwrap();
    let text = TextTower::load(&ckpt, &device, WeightFormat::F32).expect("text tower");
    let rows = text.embed(&tok, &texts, 16).expect("embed");
    std::fs::write(out.join("texts.pd.f32"), f32_bytes(&rows.concat())).unwrap();
    drop(text);

    let (tower, _, _) = load_vision(&ckpt, &device, WeightFormat::F32).expect("vision tower");
    let pre = preprocess_for(&dir);
    let mut names: Vec<PathBuf> = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rgb"))
        .collect();
    names.sort();
    for path in names {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let dims = std::fs::read_to_string(out.join(format!("{stem}.dims"))).unwrap();
        let wh: Vec<usize> = dims.split_whitespace().map(|x| x.parse().unwrap()).collect();
        let rgb = std::fs::read(&path).unwrap();
        let grid = patchify_frame_with(&rgb, wh[0], wh[1], DEFAULT_MAX_PATCHES, &pre);
        std::fs::write(out.join(format!("{stem}.pd.pix")), f32_bytes(&grid.patches)).unwrap();
        std::fs::write(out.join(format!("{stem}.pd.grid")), format!("{} {}", grid.grid_h, grid.grid_w)).unwrap();
        let pass = tower.forward_full(&grid).expect("forward");
        std::fs::write(out.join(format!("{stem}.pd.pooled")), f32_bytes(&tower.pooled(&pass).unwrap())).unwrap();
        std::fs::write(out.join(format!("{stem}.pd.dense")), f32_bytes(&tower.dense(&pass).unwrap().concat())).unwrap();
        println!("@@SIGLIP2 {stem}: grid {}x{}", grid.grid_h, grid.grid_w);
    }
}
