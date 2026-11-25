//! google/embeddinggemma-2 임베딩 모듈.
//! 텍스트, 이미지, 오디오, 비디오를 같은 768차원 공간에 넣는 모델이다. MRL 절삭 없이 원본 768을 쓴다.
//!
//! 입력은 `Part` 목록으로 받는다. 조각마다 512차원 임베딩 열을 만들고 이어 붙인 뒤 텍스트 백본에 넣는다.
//! 이미지·비디오는 `lv_core::gemma_vision`, 오디오는 오디오 타워(embeddinggemma_audio.rs)를 거친다.
//! 평균 풀링과 L2 정규화는 모든 입력이 같다.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use candle_core::safetensors::MmapedSafetensors;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use lv_core::gemma_vision::{RgbImage, VisionTower};
use tokenizers::Tokenizer;

use crate::embedding::Embedder;
use crate::embeddinggemma_audio::AudioTower;
use crate::embeddinggemma_text::TextModel;
use crate::embeddinggemma_tokens::TokenTable;
use crate::error::AppError;
use crate::qcache::{cache_path, Fingerprint, QCache};
use crate::quant::{Bits, Kernel, QuantRecipe, TowerQuant};
use crate::residency::{Modalities, ResidencyReport, SystemMemory, TowerError, Towers};

/// 모델 디렉터리 안의 가중치 파일 (Hugging Face 저장소 그대로, 텍스트·비전·오디오 전부)
pub const WEIGHTS_FILE: &str = "model.safetensors";
/// 텍스트 전용 가중치 파일: 원본에서 `language_model.*` 텐서만 옮긴 것 (앱의 모델 내려받기가 만든다, model_fetch.rs)
pub const TEXT_WEIGHTS_FILE: &str = "model-text.safetensors";
/// 모델 디렉터리 안의 토크나이저 파일
pub const TOKENIZER_FILE: &str = "tokenizer.json";

/// 타워가 파일에 있는지 볼 때 찾는 텐서 (타워마다 하나뿐인 투영)
const VISION_MARKER: &str = "embed_vision.embedding_projection.weight";
const AUDIO_MARKER: &str = "embed_audio.embedding_projection.weight";

/// 모델 폴더에서 쓸 가중치 파일. 전체 파일이 있으면 그것을, 없으면 텍스트 전용 파일을 쓴다.
pub fn weights_file(dir: &Path) -> Option<PathBuf> {
    [WEIGHTS_FILE, TEXT_WEIGHTS_FILE].iter().map(|f| dir.join(f)).find(|p| p.is_file())
}

/// 모델을 여는 방법
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// auto: 이미지·오디오 타워는 그 입력이 처음 올 때 올린다. text: 올리지 않는다
    pub modalities: Modalities,
    /// 텍스트 백본 층 투영의 양자화 (기본: 끔). quant.rs
    pub quant: QuantRecipe,
    /// 양자화한 가중치로 계산하는 방법 (기본: 계산 직전에 F32로 풀기)
    pub kernel: Kernel,
    /// 비전·오디오 타워 인코더 층의 양자화 (None이면 F32)
    pub tower_bits: Option<Bits>,
}

/// 모델 식별자 (Hugging Face 저장소)
pub const MODEL_ID: &str = "google/embeddinggemma-2";

/// 출력 차원 (원본 768)
pub const OUTPUT_DIM: usize = 768;

/// 입력 한 건이 쓸 수 있는 토큰 상한 (모든 모달리티가 공유)
pub const MAX_INPUT_TOKENS: usize = 8192;

/// 이미지 한 장의 토큰 상한 (processor_config의 image_processor: max_soft_tokens 280)
const IMAGE_TOKENS: usize = 280;
/// 비디오 프레임 한 장의 토큰 상한 (processor_config의 video_processor: max_soft_tokens 140)
const VIDEO_FRAME_TOKENS: usize = 140;
/// 오디오 1초당 토큰 수 (processor_config: audio_ms_per_token 40)
const AUDIO_TOKENS_PER_SECOND: f32 = 25.0;

/// 특수 토큰 id (config.json과 tokenizer.json 기준)
const BOS_ID: u32 = 2;
const EOS_ID: u32 = 1;
const BOI_ID: u32 = 255_999;
const EOI_ID: u32 = 258_882;
const BOA_ID: u32 = 256_000;
const EOA_ID: u32 = 258_883;

/// 모델 입력 한 건의 토큰 수 추정. 실제 수는 타워를 거친 뒤에 정해진다.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// 이미 토큰화된 텍스트 (토큰 수)
    Text { tokens: usize },
    /// 이미지 한 장 (상한)
    Image,
    /// 오디오 (길이, 초)
    Audio { seconds: f32 },
    /// 비디오 (프레임 수)
    Video { frames: usize },
}

impl Input {
    /// 이 입력이 쓰는 토큰 수 (상한)
    pub fn token_cost(&self) -> usize {
        match self {
            Input::Text { tokens } => *tokens,
            Input::Image => IMAGE_TOKENS,
            Input::Audio { seconds } => (seconds * AUDIO_TOKENS_PER_SECOND).ceil() as usize,
            Input::Video { frames } => frames * VIDEO_FRAME_TOKENS,
        }
    }
}

/// 입력들의 합 토큰 수를 돌려준다. 상한을 넘으면 거부한다.
pub fn check_budget(inputs: &[Input]) -> Result<usize, AppError> {
    check_total(inputs.iter().map(Input::token_cost).sum())
}

/// 실제 토큰 수를 상한과 비교한다.
pub fn check_total(total: usize) -> Result<usize, AppError> {
    if total > MAX_INPUT_TOKENS {
        return Err(AppError::InvalidInput(format!(
            "input uses {total} tokens, limit is {MAX_INPUT_TOKENS}"
        )));
    }
    Ok(total)
}

// --- 설정 기준 구조 (config.json, 가중치 없이 읽은 값) ---

/// 텍스트 백본 hidden size (text_config.hidden_size)
pub const TEXT_HIDDEN_SIZE: usize = 512;
/// 텍스트 백본 레이어 수 (text_config.num_hidden_layers)
pub const TEXT_NUM_LAYERS: usize = 24;
/// 일반 레이어의 attention head 수와 KV head 수, head 차원
pub const TEXT_NUM_HEADS: usize = 4;
pub const TEXT_NUM_KV_HEADS: usize = 2;
pub const TEXT_HEAD_DIM: usize = 256;
/// 전체 attention 레이어의 head 차원과 KV head 수 (per_layer_config 재정의)
pub const TEXT_GLOBAL_HEAD_DIM: usize = 512;
pub const TEXT_GLOBAL_KV_HEADS: usize = 1;
/// 어휘 크기 (text_config.vocab_size)
pub const TEXT_VOCAB_SIZE: usize = 262_144;
/// 슬라이딩 attention 창 크기 (text_config.sliding_window)
pub const SLIDING_WINDOW: usize = 512;
/// RoPE base: 전체 attention 1,000,000 / 슬라이딩 attention 10,000
pub const ROPE_THETA_FULL: f64 = 1_000_000.0;
pub const ROPE_THETA_SLIDING: f64 = 10_000.0;

/// 전체 attention 레이어 번호 (0부터 센다). config.json layer_types 기준
pub const FULL_ATTENTION_LAYERS: [usize; 4] = [5, 11, 17, 23];

/// 레이어 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    Sliding,
    Full,
}

/// 레이어 번호의 attention 종류. 범위를 벗어나면 None
pub fn attention_kind(layer: usize) -> Option<Attention> {
    if layer >= TEXT_NUM_LAYERS {
        return None;
    }
    Some(if FULL_ATTENTION_LAYERS.contains(&layer) {
        Attention::Full
    } else {
        Attention::Sliding
    })
}

// --- 역할별 접두어 (config_sentence_transformers.json의 prompts) ---

/// 입력의 역할. 모델은 역할마다 다른 접두어를 기대한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 검색 질의 (평가 검색)
    Query,
    /// 저장할 문서 (평가 원문 저장)
    Document,
    /// 문장 유사도 (두 평가 비교)
    Similarity,
}

pub const PROMPT_QUERY: &str = "task: search result | query: ";
pub const PROMPT_DOCUMENT: &str = "title: none | text: ";
pub const PROMPT_SIMILARITY: &str = "task: sentence similarity | query: ";

/// 역할 접두어를 붙인다. 임베더에 넣는 문자열만 바뀌고, 서버로 보내는 원문과 해시는 그대로다.
pub fn with_prompt(role: Role, text: &str) -> String {
    let prefix = match role {
        Role::Query => PROMPT_QUERY,
        Role::Document => PROMPT_DOCUMENT,
        Role::Similarity => PROMPT_SIMILARITY,
    };
    format!("{prefix}{text}")
}

/// 임베딩 입력의 한 조각. 텍스트는 원문 그대로, 미디어는 디코딩된 결과다.
pub enum Part<'a> {
    /// 텍스트 원문 (역할 접두어는 호출하는 쪽이 `with_prompt`로 붙인다)
    Text(&'a str),
    /// 이미지 한 장
    Image(&'a RgbImage),
    /// 비디오 프레임 목록 (`video_frame_indices`로 고른 프레임)
    Video(&'a [RgbImage]),
    /// 16kHz 모노 PCM (-1..1)
    Audio(&'a [f32]),
}

/// 타워나 텐서 오류를 임베딩 오류로 바꾼다
fn embed_error(what: &'static str) -> impl Fn(candle_core::Error) -> AppError {
    move |e| AppError::Embed(format!("{what}: {e}"))
}

/// embeddinggemma-2 모델 핸들. 토크나이저와 텍스트 백본은 늘 상주하고,
/// 비전·오디오 타워는 그 입력이 처음 올 때 올린다 (residency.rs: 실측 비용, 메모리 예산, 타워 간 전환).
pub struct EmbeddingGemma {
    tokenizer: Tokenizer,
    model: TextModel,
    weights: PathBuf,
    device: Device,
    towers: Towers<VisionTower, AudioTower>,
    quant_cache: QuantCache,
    tower_bits: Option<Bits>,
    kernel: Kernel,
}

/// 양자화 캐시 사용 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantCache {
    /// 양자화하지 않았다
    Off,
    /// 캐시를 그대로 읽었다
    Hit,
    /// 원본에서 양자화하고 캐시를 새로 썼다
    Built,
    /// 원본에서 양자화했지만 캐시를 쓰지 못했다 (읽기 전용 폴더 등)
    Unwritable,
}

impl EmbeddingGemma {
    /// 모델 디렉터리(model.safetensors 또는 model-text.safetensors, tokenizer.json)를 기본 설정으로 연다.
    pub fn load(dir: &Path) -> Result<Self, AppError> {
        Self::load_with(dir, LoadOptions::default())
    }

    /// 모델 디렉터리를 연다 (`PD_DEVICE` 환경 변수로 장치를 고른다; 비어 있으면 CPU).
    /// 텍스트 백본의 층 가중치는 f32로 올리고, 입력 임베딩 표(262,144 x 512)는
    /// 파일 매핑에 둔 채 필요한 행만 읽는다. 이미지·오디오 타워는 여기서 올리지 않는다.
    pub fn load_with(dir: &Path, options: LoadOptions) -> Result<Self, AppError> {
        let device = crate::utils::device_from_env()
            .map_err(|e| AppError::Embed(format!("장치 선택 실패: {e}")))?;
        Self::load_with_on(dir, options, device)
    }

    /// 지정한 `device` 에 EmbeddingGemma 모델을 연다.
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
        let tokens = TokenTable::open(&weights, TEXT_VOCAB_SIZE, TEXT_HIDDEN_SIZE, &device)
            .map_err(embed_error("입력 임베딩 표 열기 실패"))?;
        // 양자화하면: 같은 원본·레시피로 만든 캐시가 있으면 그대로 읽는다 (qcache.rs)
        let fingerprint = options.quant.key("text").and_then(|k| Fingerprint::of(&weights, "text", &k).ok());
        let mut cache = fingerprint.as_ref().and_then(|fp| QCache::load(&cache_path(&weights, fp), fp, &device));
        let cached = cache.as_ref().map_or(0, |c| c.len());
        let mut model = TextModel::from_vb(&vb, tokens, &device, true, &options.quant, options.kernel, cache.as_mut())
            .map_err(embed_error("텍스트 백본 로드 실패"))?;
        let quant_cache = match &fingerprint {
            None => QuantCache::Off,
            Some(fp) => {
                let tensors = model.quantized_tensors();
                if cached > 0 && cached == tensors.len() {
                    QuantCache::Hit
                } else {
                    // 처음이거나 지문이 바뀌었다: 방금 만든 결과를 쓴다. 쓰지 못해도 모델은 그대로 쓴다
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

        let (installed, disk_bytes) = tower_inventory(&weights).map_err(embed_error("가중치 목록 읽기 실패"))?;
        let towers = Towers::new(options.modalities, installed, disk_bytes, Arc::new(SystemMemory));
        Ok(Self { tokenizer, model, weights, device, towers, quant_cache, tower_bits: options.tower_bits, kernel: options.kernel })
    }

    /// 양자화 캐시를 어떻게 썼는지 (진단·실측용)
    pub fn quant_cache(&self) -> QuantCache {
        self.quant_cache
    }

    /// 텍스트 백본 층 투영이 메모리에서 차지하는 크기 (바이트)
    pub fn text_layer_bytes(&self) -> usize {
        self.model.layer_weight_bytes()
    }

    /// 비전·오디오 타워의 상주 상태 (진단·화면용)
    pub fn residency(&self) -> ResidencyReport {
        self.towers.report()
    }

    /// 올라와 있는 비전·오디오 타워를 내린다. 계산 중이면 그 계산이 끝난 뒤 메모리가 풀린다.
    pub fn release_towers(&self) {
        self.towers.release_all()
    }

    /// 타워를 올릴 때 쓰는 VarBuilder (같은 파일을 다시 매핑한다: 헤더만 읽어 가볍다)
    fn tower_vb(&self) -> Result<VarBuilder<'static>, String> {
        // SAFETY: load_with와 같은 가정
        unsafe { VarBuilder::from_mmaped_safetensors(&[&self.weights], DType::F32, &self.device) }.map_err(|e| e.to_string())
    }

    fn vision(&self) -> Result<Arc<VisionTower>, AppError> {
        self.towers
            .vision(|| self.load_vision_tower())
            .map_err(tower_error)
    }

    fn load_vision_tower(&self) -> Result<VisionTower, String> {
        let vb = self.tower_vb()?;
        let lv_bits = self.tower_bits.and_then(|b| lv_core::quant::Bits::parse(b.name()));
        let lv_kernel = match self.kernel {
            Kernel::Dequant => lv_core::quant::Kernel::Dequant,
            Kernel::Native => lv_core::quant::Kernel::Native,
        };
        let fp = lv_bits.and_then(|b| {
            lv_core::qcache::Fingerprint::of(&self.weights, "vision", &format!("eg2_vision_{}_v1", b.name())).ok()
        });
        let mut cache = fp
            .as_ref()
            .and_then(|fp| lv_core::qcache::QCache::load(&lv_core::qcache::cache_path(&self.weights, fp), fp, &self.device));
        let cached = cache.as_ref().map_or(0, |c| c.len());
        let quant = lv_core::quant::TowerQuant {
            bits: lv_bits,
            kernel: lv_kernel,
            cache: cache.as_mut(),
            keep_f32: lv_core::quant::TowerQuant::default_keep("vision"),
        };
        let tower = VisionTower::load_with(&vb, &self.device, quant).map_err(|e| e.to_string())?;
        if let Some(fp) = &fp {
            let t = tower.quantized_tensors();
            if cached == 0 || cached != t.len() {
                if let Err(e) = lv_core::qcache::QCache::save(&lv_core::qcache::cache_path(&self.weights, fp), fp, &t) {
                    eprintln!("vision 양자화 캐시를 쓰지 못했습니다 (계속 진행): {e}");
                }
            }
        }
        Ok(tower)
    }

    fn audio(&self) -> Result<Arc<AudioTower>, AppError> {
        self.towers
            .audio(|| self.load_tower("audio", |vb, q| AudioTower::load_with(vb, &self.device, q), AudioTower::quantized_tensors))
            .map_err(tower_error)
    }

    /// 타워를 올린다. 양자화하면 텍스트 백본과 같은 방식으로 캐시를 읽고, 없으면 원본에서 양자화해 캐시를 쓴다.
    fn load_tower<T>(
        &self,
        component: &str,
        build: impl FnOnce(&VarBuilder, TowerQuant) -> candle_core::Result<T>,
        tensors: impl Fn(&T) -> Vec<(String, &candle_core::quantized::QTensor)>,
    ) -> Result<T, String> {
        let vb = self.tower_vb()?;
        let fp = self
            .tower_bits
            .and_then(|b| Fingerprint::of(&self.weights, component, &format!("eg2_{component}_{}_v1", b.name())).ok());
        let mut cache = fp.as_ref().and_then(|fp| QCache::load(&cache_path(&self.weights, fp), fp, &self.device));
        let cached = cache.as_ref().map_or(0, |c| c.len());
        let quant = TowerQuant { bits: self.tower_bits, kernel: self.kernel, cache: cache.as_mut(), keep_f32: TowerQuant::default_keep(component) };
        let tower = build(&vb, quant).map_err(|e| e.to_string())?;
        if let Some(fp) = &fp {
            let t = tensors(&tower);
            if cached == 0 || cached != t.len() {
                if let Err(e) = QCache::save(&cache_path(&self.weights, fp), fp, &t) {
                    eprintln!("{component} 양자화 캐시를 쓰지 못했습니다 (계속 진행): {e}");
                }
            }
        }
        Ok(tower)
    }

    /// 조각 목록을 한 열로 이어 임베딩한다. 순서는 [BOS] 조각들 [EOS]이고,
    /// 미디어 조각은 열기·닫기 특수 토큰 사이에 놓인다 (processor와 같은 배치).
    pub fn embed_parts(&self, parts: &[Part]) -> Result<Vec<f32>, AppError> {
        if parts.is_empty() {
            return Err(AppError::InvalidInput("임베딩할 입력이 없습니다".into()));
        }
        let mut segments = Vec::with_capacity(parts.len() + 2);
        segments.push(self.special(BOS_ID)?);
        for part in parts {
            match part {
                Part::Text(text) => segments.push(self.text(text)?),
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
                    let features = self
                        .audio()?
                        .encode_pcm(samples)
                        .map_err(embed_error("오디오 임베딩 실패"))?;
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
        self.model.embed_hidden(&hidden).map_err(embed_error("풀링 실패"))
    }

    /// 텍스트 조각: 특수 토큰을 빼고 토큰화한다 (BOS와 EOS는 호출하는 쪽이 붙인다)
    fn text(&self, text: &str) -> Result<Tensor, AppError> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| AppError::Embed(format!("토큰화 실패: {e}")))?;
        self.model
            .token_embeddings(encoding.get_ids())
            .map_err(embed_error("텍스트 임베딩 실패"))
    }

    /// 특수 토큰 하나의 임베딩 (1 x 512)
    fn special(&self, id: u32) -> Result<Tensor, AppError> {
        self.model
            .token_embeddings(&[id])
            .map_err(embed_error("특수 토큰 임베딩 실패"))
    }

    /// 미디어 특징 앞뒤에 열기·닫기 특수 토큰을 붙인다
    fn wrap(&self, open: u32, close: u32, features: Tensor) -> Result<Tensor, AppError> {
        let open = self.special(open)?;
        let close = self.special(close)?;
        Tensor::cat(&[open, features, close], 0).map_err(embed_error("미디어 조립 실패"))
    }
}

/// 타워를 쓸 수 없는 이유를 앱 오류로. 설정·설치 때문이면 입력 오류(사용자가 고칠 수 있다), 그 밖은 임베딩 오류.
fn tower_error(e: TowerError) -> AppError {
    match e {
        TowerError::Disabled(_) | TowerError::NotInstalled(_) => AppError::InvalidInput(e.to_string()),
        TowerError::Memory { .. } | TowerError::Load(..) => AppError::Embed(e.to_string()),
    }
}

/// 처음부터 상주시킬 입력 임베딩 행: 특수 토큰과 역할 접두어 (요청마다 붙으므로)
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

/// 파일에 비전·오디오 타워가 있는지와 그 가중치 크기(바이트). 헤더만 읽는다.
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
    /// 받은 문자열을 텍스트 조각 하나로 임베딩한다. 역할 접두어는 호출하는 쪽이 `with_prompt`로 붙인다.
    fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        self.embed_parts(&[Part::Text(text)])
    }

    /// 텍스트 토큰 수에 BOS와 EOS를 더한 값. `embed_parts`가 만드는 열의 길이와 같다.
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
    use crate::embedding::EMBEDDING_DIM;

    #[test]
    fn output_dim_matches_server_contract() {
        assert_eq!(OUTPUT_DIM, EMBEDDING_DIM);
        assert_eq!(OUTPUT_DIM, 768);
    }

    #[test]
    fn special_token_ids_follow_config() {
        // config.json: boi_token_id, eoi_token_id, boa_token_id, eoa_token_index
        assert_eq!(BOI_ID, 255_999);
        assert_eq!(EOI_ID, 258_882);
        assert_eq!(BOA_ID, 256_000);
        assert_eq!(EOA_ID, 258_883);
        // tokenizer.json: 텍스트는 <bos> ... <eos>로 감싼다
        assert_eq!(BOS_ID, 2);
        assert_eq!(EOS_ID, 1);
    }

    #[test]
    fn token_costs_follow_processor_limits() {
        assert_eq!(Input::Text { tokens: 12 }.token_cost(), 12);
        assert_eq!(Input::Image.token_cost(), 280);
        assert_eq!(Input::Video { frames: 3 }.token_cost(), 420);
        assert_eq!(Input::Audio { seconds: 2.0 }.token_cost(), 50);
        // 오디오는 올림 처리한다 (0.01초 = 0.25토큰 -> 1토큰)
        assert_eq!(Input::Audio { seconds: 0.01 }.token_cost(), 1);
    }

    #[test]
    fn budget_accepts_total_within_limit() {
        let inputs = [Input::Text { tokens: 100 }, Input::Image];
        assert_eq!(check_budget(&inputs).unwrap(), 380);
    }

    #[test]
    fn budget_rejects_total_over_limit() {
        // 오디오 330초 = 8250토큰 > 8192
        assert!(check_budget(&[Input::Audio { seconds: 330.0 }]).is_err());
        assert!(check_total(MAX_INPUT_TOKENS + 1).is_err());
    }

    #[test]
    fn load_reports_missing_model_dir() {
        // 모델 파일 없이는 로드가 실패하고, 그 사실을 Embed 에러로 돌려준다
        let err = EmbeddingGemma::load(Path::new("no-such-embeddinggemma-dir"))
            .err()
            .expect("load must fail without model files");
        assert!(matches!(err, AppError::Embed(_)), "{err}");
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn layer_kinds_follow_config_layer_types() {
        // config.json layer_types: 0~4 sliding, 5 full, 6~10 sliding, 11 full, ...
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
