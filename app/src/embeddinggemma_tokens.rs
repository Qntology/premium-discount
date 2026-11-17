//! 텍스트 입력 임베딩 표(`language_model.embed_tokens.weight`, 262,144 x 512)를 메모리에 펼치지 않고
//! 파일 매핑(mmap)에 그대로 둔 채, 입력 토큰의 행만 읽는다.
//!
//! - 예전에는 표 전체를 F32로 올렸다 (BF16 268MB → F32 537MB 상주). 이제 상주하는 것은 읽은 페이지뿐이고,
//!   그 페이지도 운영체제가 언제든 내릴 수 있는 파일 캐시다.
//! - 어휘를 자르지 않는다. 인코더라 자유 텍스트의 어떤 토큰이 올지 모르기 때문이다 (UNK 0%).
//! - 특수 토큰과 역할 접두어의 행은 처음에 F32로 읽어 상주시킨다 (`pin`). 값은 매핑에서 읽은 것과 같다.
//! - BF16 → F32 변환은 정확하다 (상위 16비트를 옮길 뿐). 그래서 예전 경로(F32 표에서 `index_select`)와 비트 단위로 같다.

use std::collections::HashMap;
use std::path::Path;

use candle_core::safetensors::MmapedSafetensors;
use candle_core::{Device, Error, Result, Tensor};
use safetensors::Dtype;

pub const EMBED_TOKENS: &str = "language_model.embed_tokens.weight";

/// 행 하나를 F32로 바꾸는 방법 (파일의 원래 자료형)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowType {
    Bf16,
    F16,
    F32,
}

impl RowType {
    fn width(self) -> usize {
        match self {
            Self::Bf16 | Self::F16 => 2,
            Self::F32 => 4,
        }
    }
}

pub struct TokenTable {
    file: MmapedSafetensors,
    /// 표 텐서 이름
    name: String,
    rows: usize,
    cols: usize,
    kind: RowType,
    /// 상주시킨 행 (특수 토큰, 역할 접두어)
    pinned: HashMap<u32, Box<[f32]>>,
    device: Device,
}

impl TokenTable {
    /// 파일에서 표를 찾아 모양과 자료형을 확인한다. 데이터는 읽지 않는다.
    pub fn open(model_file: &Path, rows: usize, cols: usize, device: &Device) -> Result<Self> {
        Self::open_named(model_file, EMBED_TOKENS, rows, cols, device)
    }

    /// 표 이름을 지정해 연다 (다른 모델의 입력 임베딩 표: 예 laya의 `encoder.embeddings.tok_embeddings.weight`)
    pub fn open_named(model_file: &Path, name: &str, rows: usize, cols: usize, device: &Device) -> Result<Self> {
        // SAFETY: 파일은 읽기 전용으로 매핑하고, 앱이 쓰는 동안 바꾸지 않는다고 가정한다 (VarBuilder와 같은 가정)
        let file = unsafe { MmapedSafetensors::new(model_file)? };
        let (kind, shape) = {
            let view = file.get(name)?;
            let kind = match view.dtype() {
                Dtype::BF16 => RowType::Bf16,
                Dtype::F16 => RowType::F16,
                Dtype::F32 => RowType::F32,
                other => return Err(Error::Msg(format!("{name}: 지원하지 않는 자료형 {other:?}"))),
            };
            (kind, view.shape().to_vec())
        };
        if shape != [rows, cols] {
            return Err(Error::Msg(format!("{name}: 모양이 {shape:?}입니다 ({rows} x {cols}이어야 합니다)")));
        }
        Ok(Self { file, name: name.to_string(), rows, cols, kind, pinned: HashMap::new(), device: device.clone() })
    }

    /// 이 행들을 F32로 읽어 상주시킨다. 범위를 벗어난 id는 오류다.
    pub fn pin(&mut self, ids: &[u32]) -> Result<()> {
        let view = self.file.get(&self.name)?;
        let data = view.data();
        for &id in ids {
            if self.pinned.contains_key(&id) {
                continue;
            }
            let mut row = vec![0f32; self.cols];
            self.read_row(data, id, &mut row)?;
            self.pinned.insert(id, row.into_boxed_slice());
        }
        Ok(())
    }

    pub fn pinned_len(&self) -> usize {
        self.pinned.len()
    }

    /// 토큰 id 목록 → (t x cols) F32. 스케일은 곱하지 않는다 (호출하는 쪽의 규칙).
    pub fn rows(&self, ids: &[u32]) -> Result<Tensor> {
        let mut out = vec![0f32; ids.len() * self.cols];
        // 매핑 조회는 요청마다 한 번 (상주 행만 있으면 매핑을 건드리지 않는다)
        let mut data: Option<&[u8]> = None;
        let view;
        let needs_file = ids.iter().any(|id| !self.pinned.contains_key(id));
        if needs_file {
            view = self.file.get(&self.name)?;
            data = Some(view.data());
        }
        for (i, &id) in ids.iter().enumerate() {
            let dst = &mut out[i * self.cols..(i + 1) * self.cols];
            match self.pinned.get(&id) {
                Some(row) => dst.copy_from_slice(row),
                None => self.read_row(data.expect("매핑을 읽었다"), id, dst)?,
            }
        }
        Tensor::from_vec(out, (ids.len(), self.cols), &self.device)
    }

    fn read_row(&self, data: &[u8], id: u32, dst: &mut [f32]) -> Result<()> {
        let id = id as usize;
        if id >= self.rows {
            return Err(Error::Msg(format!("토큰 id {id}가 어휘 크기 {}를 벗어났습니다", self.rows)));
        }
        let w = self.kind.width();
        let start = id * self.cols * w;
        let bytes = data
            .get(start..start + self.cols * w)
            .ok_or_else(|| Error::Msg(format!("{}: 파일이 잘렸습니다 (행 {id})", self.name)))?;
        match self.kind {
            RowType::Bf16 => {
                for (d, b) in dst.iter_mut().zip(bytes.chunks_exact(2)) {
                    *d = f32::from_bits((u16::from_le_bytes([b[0], b[1]]) as u32) << 16);
                }
            }
            RowType::F16 => {
                for (d, b) in dst.iter_mut().zip(bytes.chunks_exact(2)) {
                    *d = half::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32();
                }
            }
            RowType::F32 => {
                for (d, b) in dst.iter_mut().zip(bytes.chunks_exact(4)) {
                    *d = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::test_support::temp_dir;
    use candle_core::DType;

    /// 작은 표(rows x cols)를 BF16 safetensors 파일로 쓴다. 다른 텐서도 하나 섞는다 (이름으로 찾는지 확인).
    fn write_table(path: &Path, rows: usize, cols: usize, dtype: DType) -> Tensor {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let values: Vec<f32> = (0..rows * cols).map(|i| ((i as f32) * 0.37).sin() * 3.0 + (i % 7) as f32 * 1e-3).collect();
        let table = Tensor::from_vec(values, (rows, cols), &Device::Cpu).unwrap().to_dtype(dtype).unwrap();
        let other = Tensor::ones((3,), DType::F32, &Device::Cpu).unwrap();
        candle_core::safetensors::save(
            &HashMap::from([(EMBED_TOKENS.to_string(), table.clone()), ("language_model.norm.weight".to_string(), other)]),
            path,
        )
        .unwrap();
        table
    }

    #[test]
    fn rows_match_index_select_on_the_fully_loaded_table_bit_for_bit() {
        for dtype in [DType::BF16, DType::F16, DType::F32] {
            let dir = temp_dir("token-table");
            let path = dir.join("m.safetensors");
            let table = write_table(&path, 50, 8, dtype);
            // 예전 경로: 표 전체를 F32로 올린 뒤 index_select
            let full = table.to_dtype(DType::F32).unwrap();
            let ids = [0u32, 49, 7, 7, 13];
            let ids_t = Tensor::from_slice(&ids, (ids.len(),), &Device::Cpu).unwrap();
            let expected: Vec<Vec<f32>> = full.index_select(&ids_t, 0).unwrap().to_vec2().unwrap();

            let mut t = TokenTable::open(&path, 50, 8, &Device::Cpu).unwrap();
            let got: Vec<Vec<f32>> = t.rows(&ids).unwrap().to_vec2().unwrap();
            assert_eq!(bits(&got), bits(&expected), "{dtype:?}: 매핑에서 읽은 행");

            // 상주 행을 섞어도 같다
            t.pin(&[7, 0]).unwrap();
            assert_eq!(t.pinned_len(), 2);
            let got: Vec<Vec<f32>> = t.rows(&ids).unwrap().to_vec2().unwrap();
            assert_eq!(bits(&got), bits(&expected), "{dtype:?}: 상주 행 섞음");
            // 상주 행만으로도 같다
            let only: Vec<Vec<f32>> = t.rows(&[7, 0]).unwrap().to_vec2().unwrap();
            assert_eq!(bits(&only), bits(&[expected[2].clone(), expected[0].clone()]));
        }
    }

    #[test]
    fn out_of_range_ids_wrong_shapes_and_missing_tables_are_errors() {
        let dir = temp_dir("token-table-err");
        let path = dir.join("m.safetensors");
        write_table(&path, 10, 4, DType::BF16);
        let t = TokenTable::open(&path, 10, 4, &Device::Cpu).unwrap();
        assert!(t.rows(&[10]).unwrap_err().to_string().contains("벗어났습니다"));
        assert!(TokenTable::open(&path, 11, 4, &Device::Cpu).err().expect("모양 오류").to_string().contains("모양"));

        let other = dir.join("no-table.safetensors");
        candle_core::safetensors::save(
            &HashMap::from([("x".to_string(), Tensor::ones((2,), DType::F32, &Device::Cpu).unwrap())]),
            &other,
        )
        .unwrap();
        assert!(TokenTable::open(&other, 10, 4, &Device::Cpu).is_err());
    }

    fn bits(rows: &[Vec<f32>]) -> Vec<Vec<u32>> {
        rows.iter().map(|r| r.iter().map(|v| v.to_bits()).collect()).collect()
    }
}
