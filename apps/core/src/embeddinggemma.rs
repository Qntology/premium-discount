//! google/embeddinggemma-2 Tier 2 어댑터 (PR 3 단일 원천화).
//! 텍스트 백본·비전 타워·오디오 타워·양자화·2-슬롯 GGUF 캐시 구현은 모두 Tier 1(`lv_core`)을 호출하며,
//! 이 모듈은 `residency.rs`의 지연 로드/VmRSS 예산 전환, `EmbedMode::{CanonicalFull, LocalPerforated}` 분기,
//! 그리고 서버 정준 색인 대칭을 보장하는 `SymmetricGemmaEmbedder`만 제공한다.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use candle_core::safetensors::MmapedSafetensors;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use lv_core::gemma_audio::AudioTower;
use lv_core::gemma_embed::{
    select_perforated_document_text, GemmaEmbedder, GemmaPart, GemmaTextConfig, GemmaTextModel,
    GemmaTokenTable, BOA_ID, BOI_ID, BOS_ID, EOA_ID, EOI_ID, EOS_ID, LONG_DOC_PERFORATE_THRESHOLD,
};
use lv_core::gemma_vision::{RgbImage, VisionTower};
use lv_core::qcache::{cache_path, Fingerprint, QCache};
use lv_core::quant::{Bits, Kernel, QuantEncoder, QuantRecipe, TowerQuant, TowerQuantPlan};
use tokenizers::Tokenizer;

use crate::embedding::Embedder;
use crate::error::AppError;
use crate::residency::{Modalities, ResidencyReport, SystemMemory, TowerError, Towers};

pub use lv_core::gemma_embed::{
    attention_kind, weights_file, with_prompt, EmbedMode, GemmaAttention as Attention,
    GemmaQuantCache as QuantCache, Role, FULL_ATTENTION_LAYERS, IMAGE_TOKENS, MAX_INPUT_TOKENS,
    MODEL_ID, OUTPUT_DIM, PROMPT_DOCUMENT, PROMPT_QUERY, PROMPT_SIMILARITY, ROPE_THETA_FULL,
    ROPE_THETA_SLIDING, SLIDING_WINDOW, TEXT_GLOBAL_HEAD_DIM, TEXT_GLOBAL_KV_HEADS, TEXT_HEAD_DIM,
    TEXT_HIDDEN_SIZE, TEXT_NUM_HEADS, TEXT_NUM_KV_HEADS, TEXT_NUM_LAYERS, TEXT_VOCAB_SIZE,
    TEXT_WEIGHTS_FILE, TOKENIZER_FILE, VIDEO_FRAME_TOKENS, WEIGHTS_FILE,
};

const VISION_MARKER: &str = "embed_vision.embedding_projection.weight";
const AUDIO_MARKER: &str = "embed_audio.embedding_projection.weight";
const AUDIO_TOKENS_PER_SECOND: f32 = 25.0;

/// 모델을 여는 방법 (`QuantSetting` / `TowerQuantPlan` / `QuantEncoder` 직결)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// auto: 이미지·오디오 타워는 그 입력이 처음 올 때 올린다. text: 올리지 않는다
    pub modalities: Modalities,
    /// 텍스트 백본 층 투영의 양자화 (기본: 끔)
    pub quant: QuantRecipe,
    /// 양자화한 가중치로 계산하는 방법 (기본: 계산 직전에 F32로 풀기)
    pub kernel: Kernel,
    /// 양자화 인코더 (`CandleFast` 또는 `KQuantWeightedLs`)
    pub encoder: QuantEncoder,
    /// 비전·오디오 타워 인코더 층의 양자화 (None이면 F32; 전용 플랜이 없을 때 폴백)
    pub tower_bits: Option<Bits>,
    /// 비전 타워 전용 양자화 계획 (예: `TowerQuantPlan::vision_tiered()`)
    pub vision_quant: Option<TowerQuantPlan>,
    /// 오디오 타워 전용 양자화 계획 (예: `TowerQuantPlan::audio_tiered()`)
    pub audio_quant: Option<TowerQuantPlan>,
}

/// 모델 입력 한 건의 토큰 수 추정. 실제 수는 타워를 거친 뒤에 정해진다.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Text { tokens: usize },
    Image,
    Audio { seconds: f32 },
    Video { frames: usize },
}

impl Input {
    pub fn token_cost(&self) -> usize {
        match self {
            Input::Text { tokens } => *tokens,
            Input::Image => IMAGE_TOKENS,
            Input::Audio { seconds } => (seconds * AUDIO_TOKENS_PER_SECOND).ceil() as usize,
            Input::Video { frames } => frames * VIDEO_FRAME_TOKENS,
        }
    }
}

pub fn check_budget(inputs: &[Input]) -> Result<usize, AppError> {
    check_total(inputs.iter().map(Input::token_cost).sum())
}

pub fn check_total(total: usize) -> Result<usize, AppError> {
    if total > MAX_INPUT_TOKENS {
        return Err(AppError::InvalidInput(format!(
            "input uses {total} tokens, limit is {MAX_INPUT_TOKENS}"
        )));
    }
    Ok(total)
}

/// 임베딩 입력의 한 조각. 텍스트는 원문 그대로, 미디어는 디코딩된 결과다.
pub enum Part<'a> {
    Text(&'a str),
    Image(&'a RgbImage),
    Video(&'a [RgbImage]),
    Audio(&'a [f32]),
}

fn embed_error(what: &'static str) -> impl Fn(candle_core::Error) -> AppError {
    move |e| AppError::Embed(format!("{what}: {e}"))
}

/// embeddinggemma-2 모델 핸들 (`lv_core` 단일 엔진 + `residency.rs` 지연 타워 전환).
pub struct EmbeddingGemma {
    tokenizer: Tokenizer,
    model: GemmaTextModel,
    weights: PathBuf,
    device: Device,
    towers: Towers<VisionTower, AudioTower>,
    quant_cache: QuantCache,
    vision_plan: TowerQuantPlan,
    audio_plan: TowerQuantPlan,
    kernel: Kernel,
}

impl EmbeddingGemma {
    pub fn load(dir: &Path) -> Result<Self, AppError> {
        Self::load_with(dir, LoadOptions::default())
    }

    pub fn load_with(dir: &Path, options: LoadOptions) -> Result<Self, AppError> {
        let device = crate::utils::device_from_env()
            .map_err(|e| AppError::Embed(format!("장치 선택 실패: {e}")))?;
        Self::load_with_on(dir, options, device)
    }

    pub fn load_with_on(dir: &Path, options: LoadOptions, device: Device) -> Result<Self, AppError> {
        let tokenizer = Tokenizer::from_file(dir.join(TOKENIZER_FILE))
            .map_err(|e| AppError::Embed(format!("토크나이저 로드 실패: {e}")))?;
        let weights = weights_file(dir).ok_or_else(|| {
            AppError::Embed(format!(
                "가중치 파일이 없습니다: {} 또는 {} ({})",
                WEIGHTS_FILE,
                TEXT_WEIGHTS_FILE,
                dir.display()
            ))
        })?;
        // SAFETY: 파일은 읽기 전용으로 매핑하고, 로드 중에 바꾸지 않는다고 가정한다
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights], DType::F32, &device) }
            .map_err(embed_error("가중치 로드 실패"))?;
        let tokens = GemmaTokenTable::open(&weights, TEXT_VOCAB_SIZE, TEXT_HIDDEN_SIZE, &device)
            .map_err(embed_error("입력 임베딩 표 열기 실패"))?;
        let fingerprint = options
            .quant
            .key("text")
            .and_then(|k| Fingerprint::of(&weights, "text", &k).ok());
        let mut cache = fingerprint
            .as_ref()
            .and_then(|fp| QCache::load(&cache_path(&weights, fp), fp, &device));
        let cached = cache.as_ref().map_or(0, |c| c.len());
        let mut model = GemmaTextModel::from_vb_with_config_and_encoder(
            GemmaTextConfig::default(),
            &vb,
            tokens,
            &device,
            true,
            &options.quant,
            options.kernel,
            options.encoder,
            cache.as_mut(),
        )
        .map_err(embed_error("텍스트 백본 로드 실패"))?;

        let quant_cache = match &fingerprint {
            None => QuantCache::Off,
            Some(fp) => {
                let tensors = model.quantized_tensors();
                if cached > 0 && cached == tensors.len() {
                    QuantCache::Hit
                } else {
                    match QCache::save(&cache_path(&weights, fp), fp, &tensors) {
                        Ok(()) => QuantCache::Built,
                        Err(e) => {
                            eprintln!("양자화 캐시를 쓰지 못했습니다 (계속 진행): {e}");
                            QuantCache::Unwritable
                        }
                    }
                }
            }
        };
        model
            .pin_tokens(&pinned_token_ids(&tokenizer)?)
            .map_err(embed_error("고정 토큰 행 읽기 실패"))?;

        let vision_plan = options.vision_quant.unwrap_or_else(|| TowerQuantPlan {
            default_bits: options.tower_bits,
            keep_f32_layers: TowerQuant::default_keep("vision"),
            group_overrides: Vec::new(),
            encoder: options.encoder,
        });
        let audio_plan = options.audio_quant.unwrap_or_else(|| TowerQuantPlan {
            default_bits: options.tower_bits,
            keep_f32_layers: TowerQuant::default_keep("audio"),
            group_overrides: Vec::new(),
            encoder: options.encoder,
        });

        let (installed, disk_bytes) =
            tower_inventory(&weights).map_err(embed_error("가중치 목록 읽기 실패"))?;
        let towers = Towers::new(options.modalities, installed, disk_bytes, Arc::new(SystemMemory));
        Ok(Self {
            tokenizer,
            model,
            weights,
            device,
            towers,
            quant_cache,
            vision_plan,
            audio_plan,
            kernel: options.kernel,
        })
    }

    pub fn quant_cache(&self) -> QuantCache {
        self.quant_cache
    }

    pub fn text_layer_bytes(&self) -> usize {
        self.model.layer_weight_bytes()
    }

    pub fn residency(&self) -> ResidencyReport {
        self.towers.report()
    }

    pub fn release_towers(&self) {
        self.towers.release_all()
    }

    fn tower_vb(&self) -> Result<VarBuilder<'static>, String> {
        // SAFETY: load_with와 같은 가정
        unsafe { VarBuilder::from_mmaped_safetensors(&[&self.weights], DType::F32, &self.device) }
            .map_err(|e| e.to_string())
    }

    fn vision(&self) -> Result<Arc<VisionTower>, AppError> {
        self.towers
            .vision(|| {
                self.load_tower(
                    "vision",
                    &self.vision_plan,
                    |vb, q| VisionTower::load_with(vb, &self.device, q),
                    VisionTower::quantized_tensors,
                )
            })
            .map_err(tower_error)
    }

    fn audio(&self) -> Result<Arc<AudioTower>, AppError> {
        self.towers
            .audio(|| {
                self.load_tower(
                    "audio",
                    &self.audio_plan,
                    |vb, q| AudioTower::load_with(vb, &self.device, q),
                    AudioTower::quantized_tensors,
                )
            })
            .map_err(tower_error)
    }

    fn load_tower<T>(
        &self,
        component: &str,
        plan: &TowerQuantPlan,
        build: impl FnOnce(&VarBuilder, TowerQuant<'_>) -> candle_core::Result<T>,
        tensors: impl Fn(&T) -> Vec<(String, &candle_core::quantized::QTensor)>,
    ) -> Result<T, String> {
        let vb = self.tower_vb()?;
        let fp = plan
            .recipe_key(component)
            .and_then(|k| Fingerprint::of(&self.weights, component, &k).ok());
        let mut cache = fp
            .as_ref()
            .and_then(|f| QCache::load(&cache_path(&self.weights, f), f, &self.device));
        let cached = cache.as_ref().map_or(0, |c| c.len());
        let quant = TowerQuant::from_plan(plan, self.kernel, cache.as_mut());
        let tower = build(&vb, quant).map_err(|e| e.to_string())?;
        if let Some(f) = &fp {
            let t = tensors(&tower);
            if cached == 0 || cached != t.len() {
                if let Err(e) = QCache::save(&cache_path(&self.weights, f), f, &t) {
                    eprintln!("{component} 양자화 캐시를 쓰지 못했습니다 (계속 진행): {e}");
                }
            }
        }
        Ok(tower)
    }

    /// 정준 전체 경로(`EmbedMode::CanonicalFull`) 임베딩 (서버 색인·검색 대칭 기본값).
    pub fn embed_parts(&self, parts: &[Part]) -> Result<Vec<f32>, AppError> {
        self.embed_parts_with_mode(parts, EmbedMode::CanonicalFull)
    }

    /// 지정한 [`EmbedMode`]로 텍스트·이미지·비디오·오디오 조각 목록을 768차원 정규 벡터로 임베딩한다.
    pub fn embed_parts_with_mode(&self, parts: &[Part], mode: EmbedMode) -> Result<Vec<f32>, AppError> {
        if parts.is_empty() {
            return Err(AppError::InvalidInput("임베딩할 입력이 없습니다".into()));
        }
        let mut segments = Vec::with_capacity(parts.len() + 2);
        segments.push(self.special(BOS_ID)?);
        for part in parts {
            match part {
                Part::Text(text) => segments.push(self.text_with_mode(text, mode)?),
                Part::Image(image) => {
                    let features = self
                        .vision()?
                        .encode_image(image, IMAGE_TOKENS)
                        .map_err(embed_error("이미지 임베딩 실패"))?;
                    segments.push(self.wrap(BOI_ID, EOI_ID, features)?);
                }
                Part::Video(frames) => {
                    let vision = self.vision()?;
                    for frame in frames.iter() {
                        let features = vision
                            .encode_image(frame, VIDEO_FRAME_TOKENS)
                            .map_err(embed_error("비디오 프레임 임베딩 실패"))?;
                        segments.push(self.wrap(BOI_ID, EOI_ID, features)?);
                    }
                }
                Part::Audio(samples) => {
                    let audio = self.audio()?;
                    let features = match mode {
                        EmbedMode::CanonicalFull => audio
                            .encode_pcm(samples)
                            .map_err(embed_error("오디오 임베딩 실패"))?,
                        EmbedMode::LocalPerforated => audio
                            .encode_pcm_voiced(samples, 0.015)
                            .map_err(embed_error("오디오 유성 청크 임베딩 실패"))?
                            .0,
                    };
                    segments.push(self.wrap(BOA_ID, EOA_ID, features)?);
                }
            }
        }
        segments.push(self.special(EOS_ID)?);

        let mut total = 0;
        for segment in &segments {
            total += segment.dim(0).map_err(embed_error("길이 계산 실패"))?;
        }
        check_total(total)?;

        let input = Tensor::cat(&segments, 0)
            .and_then(|t| t.unsqueeze(0))
            .map_err(embed_error("입력 조립 실패"))?;
        let hidden = self
            .model
            .forward_embeds(&input)
            .map_err(embed_error("텍스트 백본 실패"))?;
        self.model
            .embed_hidden(&hidden)
            .map_err(embed_error("풀링 실패"))
    }

    fn text_with_mode(&self, text: &str, mode: EmbedMode) -> Result<Tensor, AppError> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| AppError::Embed(format!("토큰화 실패: {e}")))?;
        if mode == EmbedMode::LocalPerforated
            && encoding.get_ids().len() > LONG_DOC_PERFORATE_THRESHOLD
        {
            let perf = select_perforated_document_text(text, 0.65);
            let enc_perf = self
                .tokenizer
                .encode(perf.as_str(), false)
                .map_err(|e| AppError::Embed(format!("토큰화 실패: {e}")))?;
            return self
                .model
                .token_embeddings(enc_perf.get_ids())
                .map_err(embed_error("텍스트 임베딩 실패"));
        }
        self.model
            .token_embeddings(encoding.get_ids())
            .map_err(embed_error("텍스트 임베딩 실패"))
    }

    fn special(&self, id: u32) -> Result<Tensor, AppError> {
        self.model
            .token_embeddings(&[id])
            .map_err(embed_error("특수 토큰 임베딩 실패"))
    }

    fn wrap(&self, open: u32, close: u32, features: Tensor) -> Result<Tensor, AppError> {
        let open = self.special(open)?;
        let close = self.special(close)?;
        Tensor::cat(&[open, features, close], 0).map_err(embed_error("미디어 조립 실패"))
    }
}

enum SymmetricBackend {
    Live(Arc<EmbeddingGemma>),
    Deterministic(GemmaEmbedder),
}

/// 서버 정준 색인 대칭 계약 래퍼 (`embedding_draft_dev_plan.md` PR 3).
/// - `Embedder::embed` 및 `embed_canonical`은 항상 `EmbedMode::CanonicalFull`로 실행해 서버 벡터와 대칭을 유지한다.
/// - 로컬 전용 저비용 탐색만 `embed_local_perforated`를 통해 `EmbedMode::LocalPerforated`를 명시적으로 호출한다.
pub struct SymmetricGemmaEmbedder {
    backend: SymmetricBackend,
}

impl SymmetricGemmaEmbedder {
    pub fn new(inner: Arc<EmbeddingGemma>) -> Self {
        Self {
            backend: SymmetricBackend::Live(inner),
        }
    }

    pub fn from_embedder(inner: EmbeddingGemma) -> Self {
        Self::new(Arc::new(inner))
    }

    /// 모델 가중치 없이 대칭/천공 계약을 검증하기 위한 결정론적 768차원 테스트 임베더.
    pub fn deterministic_for_tests(seed: u64) -> Self {
        Self {
            backend: SymmetricBackend::Deterministic(GemmaEmbedder::deterministic_for_tests(seed)),
        }
    }

    pub fn canonical_mode(&self) -> EmbedMode {
        EmbedMode::CanonicalFull
    }

    pub fn embed_canonical(&self, parts: &[Part]) -> Result<Vec<f32>, AppError> {
        self.embed_with_mode(parts, EmbedMode::CanonicalFull)
    }

    pub fn embed_local_perforated(&self, parts: &[Part]) -> Result<Vec<f32>, AppError> {
        self.embed_with_mode(parts, EmbedMode::LocalPerforated)
    }

    pub fn embed_with_mode(&self, parts: &[Part], mode: EmbedMode) -> Result<Vec<f32>, AppError> {
        match &self.backend {
            SymmetricBackend::Live(g) => g.embed_parts_with_mode(parts, mode),
            SymmetricBackend::Deterministic(g) => {
                let lv_parts: Vec<GemmaPart<'_>> = parts
                    .iter()
                    .map(|p| match p {
                        Part::Text(t) => GemmaPart::Text(t),
                        Part::Image(img) => GemmaPart::Image(img),
                        Part::Video(v) => GemmaPart::Video(v),
                        Part::Audio(a) => GemmaPart::Audio(a),
                    })
                    .collect();
                g.embed_parts_with_mode(&lv_parts, mode)
                    .map_err(|e| AppError::Embed(e.to_string()))
            }
        }
    }
}

impl Embedder for SymmetricGemmaEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        self.embed_canonical(&[Part::Text(text)])
    }

    fn token_count(&self, text: &str) -> Result<Option<usize>, AppError> {
        match &self.backend {
            SymmetricBackend::Live(g) => g.token_count(text),
            SymmetricBackend::Deterministic(g) => g
                .token_count(text)
                .map(Some)
                .map_err(|e| AppError::Embed(e.to_string())),
        }
    }

    fn residency(&self) -> Option<ResidencyReport> {
        match &self.backend {
            SymmetricBackend::Live(g) => g.residency(),
            SymmetricBackend::Deterministic(_) => None,
        }
    }
}

fn tower_error(e: TowerError) -> AppError {
    match e {
        TowerError::Disabled(_) | TowerError::NotInstalled(_) => AppError::InvalidInput(e.to_string()),
        TowerError::Memory { .. } | TowerError::Load(..) => AppError::Embed(e.to_string()),
    }
}

fn pinned_token_ids(tokenizer: &Tokenizer) -> Result<Vec<u32>, AppError> {
    let mut ids = vec![BOS_ID, EOS_ID, BOI_ID, EOI_ID, BOA_ID, EOA_ID];
    for prompt in [PROMPT_QUERY, PROMPT_DOCUMENT, PROMPT_SIMILARITY] {
        let encoding = tokenizer
            .encode(prompt, false)
            .map_err(|e| AppError::Embed(format!("토큰화 실패: {e}")))?;
        ids.extend_from_slice(encoding.get_ids());
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn tower_inventory(weights: &Path) -> candle_core::Result<([bool; 2], [u64; 2])> {
    // SAFETY: load_with와 같은 가정
    let file = unsafe { MmapedSafetensors::new(weights)? };
    let mut bytes = [0u64; 2];
    let mut installed = [false; 2];
    for (name, view) in file.tensors() {
        let i = if name.starts_with("vision_tower.") || name.starts_with("embed_vision.") {
            0
        } else if name.starts_with("audio_tower.") || name.starts_with("embed_audio.") {
            1
        } else {
            continue;
        };
        bytes[i] += view.data().len() as u64;
        if name == VISION_MARKER || name == AUDIO_MARKER {
            installed[i] = true;
        }
    }
    Ok((installed, bytes))
}

impl Embedder for EmbeddingGemma {
    fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        self.embed_parts(&[Part::Text(text)])
    }

    fn token_count(&self, text: &str) -> Result<Option<usize>, AppError> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| AppError::Embed(format!("토큰화 실패: {e}")))?;
        Ok(Some(encoding.get_ids().len() + 2))
    }

    fn residency(&self) -> Option<ResidencyReport> {
        Some(self.towers.report())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::{check_contract, EMBEDDING_DIM};

    #[test]
    fn output_dim_matches_server_contract() {
        assert_eq!(OUTPUT_DIM, EMBEDDING_DIM);
        assert_eq!(OUTPUT_DIM, 768);
    }

    #[test]
    fn special_token_ids_follow_config() {
        assert_eq!(BOI_ID, 255_999);
        assert_eq!(EOI_ID, 258_882);
        assert_eq!(BOA_ID, 256_000);
        assert_eq!(EOA_ID, 258_883);
        assert_eq!(BOS_ID, 2);
        assert_eq!(EOS_ID, 1);
    }

    #[test]
    fn token_costs_follow_processor_limits() {
        assert_eq!(Input::Text { tokens: 12 }.token_cost(), 12);
        assert_eq!(Input::Image.token_cost(), 280);
        assert_eq!(Input::Video { frames: 3 }.token_cost(), 420);
        assert_eq!(Input::Audio { seconds: 2.0 }.token_cost(), 50);
        assert_eq!(Input::Audio { seconds: 0.01 }.token_cost(), 1);
    }

    #[test]
    fn budget_accepts_total_within_limit() {
        let inputs = [Input::Text { tokens: 100 }, Input::Image];
        assert_eq!(check_budget(&inputs).unwrap(), 380);
    }

    #[test]
    fn budget_rejects_total_over_limit() {
        assert!(check_budget(&[Input::Audio { seconds: 330.0 }]).is_err());
        assert!(check_total(MAX_INPUT_TOKENS + 1).is_err());
    }

    #[test]
    fn load_reports_missing_model_dir() {
        let err = EmbeddingGemma::load(Path::new("no-such-embeddinggemma-dir"))
            .err()
            .expect("load must fail without model files");
        assert!(matches!(err, AppError::Embed(_)), "{err}");
    }

    #[test]
    fn symmetric_gemma_embedder_enforces_canonical_contract_and_local_perforated_mode() {
        let sym = SymmetricGemmaEmbedder::deterministic_for_tests(2026);
        assert_eq!(sym.canonical_mode(), EmbedMode::CanonicalFull);
        let q = with_prompt(Role::Query, "서울 물류센터 정산 검증");
        let v_trait = sym.embed(&q).unwrap();
        let v_canon = sym.embed_canonical(&[Part::Text(&q)]).unwrap();
        assert_eq!(v_trait, v_canon, "Embedder::embed must match CanonicalFull bit-for-bit");
        check_contract(&v_trait).unwrap();

        let pcm: Vec<f32> = (0..16_000).map(|i| 0.25 * ((i as f32) * 0.04).sin()).collect();
        let v_local = sym
            .embed_local_perforated(&[Part::Text(&q), Part::Audio(&pcm)])
            .unwrap();
        check_contract(&v_local).unwrap();
        assert!(sym.token_count(&q).unwrap().unwrap() >= 3);
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn layer_kinds_follow_config_layer_types() {
        assert_eq!(attention_kind(0), Some(Attention::Sliding));
        assert_eq!(attention_kind(4), Some(Attention::Sliding));
        assert_eq!(attention_kind(5), Some(Attention::Full));
        assert_eq!(attention_kind(11), Some(Attention::Full));
        assert_eq!(attention_kind(23), Some(Attention::Full));
        assert_eq!(attention_kind(24), None);
    }

    #[test]
    fn four_full_attention_layers_and_twenty_sliding() {
        let full = (0..TEXT_NUM_LAYERS)
            .filter(|&i| attention_kind(i) == Some(Attention::Full))
            .count();
        assert_eq!(full, 4);
        assert_eq!(TEXT_NUM_LAYERS - full, 20);
    }

    #[test]
    fn global_layers_use_wider_head_and_one_kv_head() {
        assert_eq!(TEXT_GLOBAL_HEAD_DIM, 512);
        assert_eq!(TEXT_GLOBAL_KV_HEADS, 1);
        assert_eq!(TEXT_HEAD_DIM * TEXT_NUM_HEADS, 1024);
    }

    #[test]
    fn prompts_match_model_card() {
        assert_eq!(with_prompt(Role::Query, "질문"), "task: search result | query: 질문");
        assert_eq!(with_prompt(Role::Document, "원문"), "title: none | text: 원문");
        assert_eq!(
            with_prompt(Role::Similarity, "비교"),
            "task: sentence similarity | query: 비교"
        );
    }
}
