//! Cross-ecosystem vision evaluation harness (`pd-vision-eval`):
//! links `louis-vision` (`lv-core`, `lv-screen`, `lv-doc`, `lv-spatial`) at `<LV_LIB_PIN_REV>`
//! and grades precision, calibration, gate A/B savings, and source-tree hygiene via `premium-discount-eval`.

pub mod cross_hygiene;
pub mod doc_eval;
pub mod screen_eval;
pub mod spatial_doc_eval;
pub mod spatial_eval;
