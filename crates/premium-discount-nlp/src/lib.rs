//! # premium-discount-nlp
//!
//! 다국어 텍스트 이해 (raon-zena 의 `zena-nlp` 를 옮김, 2026-11-17). Qntology/logis-center 에서 옮겨 온 코드다.
//!
//! * [`granite`] – (기능 `granite`) `ibm-granite/granite-embedding-97m-multilingual-r2` (ModernBERT 12층, 384차원, CLS 풀링)
//!   을 candle 로. 공간 이름 [`granite::SPACE_TAG`] 은 premium-discount-nav 의 라벨 공간과 같다.
//! * `stanza`    – (기능 `stanza`) stanza-onnx-export format 2 모델로 돌리는 Stanza 파이프라인(토큰 → MWT → UPOS/XPOS/feats →
//!   원형 → 의존 구문)과 질의 언어 판정. ONNX Runtime 은 실행할 때 불러온다(`onnxruntime-sys::dynload`).
//! * [`models`]  – 이 기기에서 모델을 찾는 곳.

#[cfg(feature = "granite")]
pub mod granite;
pub mod models;
#[cfg(feature = "stanza")]
pub mod stanza;
