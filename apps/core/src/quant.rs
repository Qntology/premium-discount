//! 텍스트 백본 선형층의 양자화 (원본 BF16 → F32(정확) → 한 번만 양자화. 이미 양자화된 가중치를 다시 깎지 않는다).
//!
//! 양자화하지 않는 것 (F32 그대로, 정밀 누산): 모든 RMSNorm, `layer_scalar`, 출력 투영 `language_model.embedding_projection`
//! (512 → 768, 768차원 계약의 관문), PLE 전역 투영 `ple.per_layer_model_projection`, 입력 임베딩 표(파일 매핑).
//! 양자화 대상은 24개 층의 투영 9종(q, k, v, o, gate, up, down, ple_gate, ple_proj)뿐이다.
//!
//! 어느 층·투영을 몇 비트로 할지는 `QuantRecipe`가 정한다. 기본은 끔(Off). 배치는 실측(examples/quant_probe.rs)으로 정했다:
//! README 16절의 민감도 표와 결정 근거를 볼 것.

use candle_core::quantized::{GgmlDType, QMatMul, QTensor};
use candle_core::{Module, Result, Tensor};
use candle_nn::{Linear, VarBuilder};

use crate::qcache::QCache;

/// 선형층 하나: 캐시에 그 레시피로 이미 양자화한 텐서가 있으면 그것을, 없으면 원본(BF16 → F32)에서 한 번 양자화한다.
/// `vb`의 `weight`를 `shape`(출력, 입력)로 읽는다. 캐시를 쓰면 원본 가중치는 읽지 않는다 (최대 메모리가 낮다).
pub fn load_lin(
    vb: &VarBuilder,
    shape: (usize, usize),
    bits: Option<Bits>,
    kernel: Kernel,
    cache: &mut Option<&mut QCache>,
    key: String,
) -> Result<Lin> {
    if let (Some(b), Some(c)) = (bits, cache.as_deref_mut()) {
        if let Some(q) = c.take(&key, b.ggml(), &[shape.0, shape.1]) {
            return Lin::from_qtensor(q, kernel);
        }
    }
    Lin::new(vb.get(shape, "weight")?, bits, kernel)
}

/// 비전·오디오 타워의 양자화: 인코더 층의 선형층만 한 비트로 (관문 투영·패치·위치·합성곱·정규화·클립 경계는 F32)
pub struct TowerQuant<'a> {
    pub bits: Option<Bits>,
    pub kernel: Kernel,
    pub cache: Option<&'a mut QCache>,
    /// 이 층들은 F32로 둔다 (실측으로 민감한 층)
    pub keep_f32: Vec<usize>,
}

impl TowerQuant<'_> {
    pub fn off() -> Self {
        Self { bits: None, kernel: Kernel::default(), cache: None, keep_f32: Vec::new() }
    }

    /// 실측으로 정한, F32로 두는 층 ("vision" | "audio"). README 17절의 층별 민감도 표를 볼 것.
    /// 그 층 하나만 Q8_0으로 내렸을 때 토큰 최소 코사인: 비전 층 0이 0.999584로 가장 낮고(나머지 0.99973~0.99996),
    /// 오디오 층 8이 0.998776으로 다른 층(0.99982 이상)보다 한 자릿수 크다. 이 둘만 F32로 둔다.
    pub fn default_keep(tower: &str) -> Vec<usize> {
        match tower {
            "vision" => vec![0],
            "audio" => vec![8],
            _ => Vec::new(),
        }
    }

    /// `layer`번 층의 선형층 하나
    pub fn lin(&mut self, vb: &VarBuilder, layer: usize, shape: (usize, usize), key: String) -> Result<Lin> {
        let bits = if self.keep_f32.contains(&layer) { None } else { self.bits };
        load_lin(vb, shape, bits, self.kernel, &mut self.cache, key)
    }
}

/// 층 하나 안의 투영
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Proj {
    Q,
    K,
    V,
    O,
    Gate,
    Up,
    Down,
    PleGate,
    PleProj,
}

impl Proj {
    pub const ALL: [Proj; 9] = [Proj::Q, Proj::K, Proj::V, Proj::O, Proj::Gate, Proj::Up, Proj::Down, Proj::PleGate, Proj::PleProj];

    /// 실측에서 묶어 보는 단위
    pub fn group(self) -> ProjGroup {
        match self {
            Proj::Q | Proj::K | Proj::V | Proj::O => ProjGroup::Attn,
            Proj::Gate | Proj::Up => ProjGroup::MlpIn,
            Proj::Down => ProjGroup::MlpOut,
            Proj::PleGate | Proj::PleProj => ProjGroup::Ple,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjGroup {
    Attn,
    MlpIn,
    MlpOut,
    Ple,
}

impl ProjGroup {
    pub const ALL: [ProjGroup; 4] = [ProjGroup::Attn, ProjGroup::MlpIn, ProjGroup::MlpOut, ProjGroup::Ple];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "attn" => Some(Self::Attn),
            "mlp_in" => Some(Self::MlpIn),
            "mlp_out" | "down" => Some(Self::MlpOut),
            "ple" => Some(Self::Ple),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Attn => "attn",
            Self::MlpIn => "mlp_in",
            Self::MlpOut => "mlp_out",
            Self::Ple => "ple",
        }
    }
}

/// 쓰는 자료형 (가중치 하나당 비트: Q8_0 8.5, Q6_K 6.5625, Q5_K 5.5, Q4_K 4.5)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Bits {
    Q4K,
    Q5K,
    Q6K,
    Q8_0,
}

impl Bits {
    pub fn ggml(self) -> GgmlDType {
        match self {
            Self::Q8_0 => GgmlDType::Q8_0,
            Self::Q6K => GgmlDType::Q6K,
            Self::Q5K => GgmlDType::Q5K,
            Self::Q4K => GgmlDType::Q4K,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "q8_0" | "q8" => Some(Self::Q8_0),
            "q6k" | "q6_k" => Some(Self::Q6K),
            "q5k" | "q5_k" => Some(Self::Q5K),
            "q4k" | "q4_k" => Some(Self::Q4K),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Q8_0 => "q8_0",
            Self::Q6K => "q6k",
            Self::Q5K => "q5k",
            Self::Q4K => "q4k",
        }
    }
}

/// 층·투영별 자료형을 정하는 규칙
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum QuantRecipe {
    /// 양자화하지 않는다 (F32). 기본값
    #[default]
    Off,
    /// 모든 층의 이 묶음들을 한 자료형으로 (실측용)
    Uniform { groups: Vec<ProjGroup>, bits: Bits },
    /// 이 층들의 이 묶음들만 (층별 민감도 실측용)
    Only { layers: Vec<usize>, groups: Vec<ProjGroup>, bits: Bits },
    /// 층·묶음별 표 (실측으로 정한 배치). 표에 없으면 F32
    Table(Vec<(usize, ProjGroup, Bits)>),
}

impl QuantRecipe {
    pub fn bits(&self, layer: usize, proj: Proj) -> Option<Bits> {
        let g = proj.group();
        match self {
            Self::Off => None,
            Self::Uniform { groups, bits } => groups.contains(&g).then_some(*bits),
            Self::Only { layers, groups, bits } => (layers.contains(&layer) && groups.contains(&g)).then_some(*bits),
            Self::Table(rows) => rows.iter().find(|(l, gr, _)| *l == layer && *gr == g).map(|(_, _, b)| *b),
        }
    }

    /// 캐시 지문에 넣는 레시피 이름 (`recipe_key`). 끔이면 None.
    /// 같은 이름이면 같은 텐서를 같은 비트로 양자화한다는 뜻이다. 양자화 코드가 바뀌면 `qcache::FORMAT`을 올린다.
    pub fn key(&self, component: &str) -> Option<String> {
        if self.is_off() {
            return None;
        }
        if let Self::Uniform { groups, bits } = self {
            if ProjGroup::ALL.iter().all(|g| groups.contains(g)) {
                return Some(format!("eg2_{component}_{}_v1", bits.name()));
            }
        }
        use sha2::Digest;
        let digest = sha2::Sha256::digest(format!("{self:?}").as_bytes());
        Some(format!("eg2_{component}_custom-{}_v1", digest.iter().take(4).map(|b| format!("{b:02x}")).collect::<String>()))
    }

    pub fn is_off(&self) -> bool {
        match self {
            Self::Off => true,
            Self::Table(rows) => rows.is_empty(),
            _ => false,
        }
    }
}

/// 앱 설정 `quant`: 텍스트 백본을 양자화할지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuantSetting {
    /// 모바일(android, ios)은 Q8_0, 데스크톱은 F32 (기본값)
    #[default]
    Auto,
    Off,
    /// 층 투영 9종 전부 Q8_0 (실측: 참조 대비 최소 코사인 0.9999, 층 가중치 520MB → 138MB)
    Q8_0,
}

impl QuantSetting {
    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "off" | "f32" | "none" => Ok(Self::Off),
            "q8_0" | "q8" => Ok(Self::Q8_0),
            other => Err(format!("quant 값 '{other}'를 알 수 없습니다 (auto, off, q8_0)")),
        }
    }

    /// 비전·오디오 타워 인코더 층의 비트 (텍스트와 같은 설정을 따른다)
    pub fn tower_bits(self, platform: &str) -> Option<Bits> {
        (!self.recipe(platform).is_off()).then_some(Bits::Q8_0)
    }

    /// 이 플랫폼(`commands::platform()`의 값)에서 쓸 레시피
    pub fn recipe(self, platform: &str) -> QuantRecipe {
        let q8 = || QuantRecipe::Uniform { groups: ProjGroup::ALL.to_vec(), bits: Bits::Q8_0 };
        match self {
            Self::Off => QuantRecipe::Off,
            Self::Q8_0 => q8(),
            Self::Auto if platform == "android" || platform == "ios" => q8(),
            Self::Auto => QuantRecipe::Off,
        }
    }
}

/// 지시서의 4계층 피라미드를 텍스트 백본에 옮긴 배치 (실측 전 초안: 실측 결과는 README 16절).
/// Tier 2(Q8_0): 첫 층(0)과 전역 어텐션 층(5, 11, 17, 23)의 모든 투영, 모든 층의 PLE 블록.
/// Tier 3(Q6_K): 나머지 층의 어텐션(q, k, v, o)과 MLP down. Tier 4(Q4_K): 나머지 층의 MLP gate·up.
/// Tier 1(F32): 정규화, layer_scalar, 출력 투영, PLE 전역 투영, 입력 임베딩 표 (애초에 양자화 대상이 아님).
pub fn pyramid_v1() -> QuantRecipe {
    let mut rows = Vec::new();
    for layer in 0..crate::embeddinggemma::TEXT_NUM_LAYERS {
        let guarded = layer == 0 || crate::embeddinggemma::attention_kind(layer) == Some(crate::embeddinggemma::Attention::Full);
        for group in ProjGroup::ALL {
            let bits = match (guarded, group) {
                (_, ProjGroup::Ple) | (true, _) => Bits::Q8_0,
                (false, ProjGroup::Attn) | (false, ProjGroup::MlpOut) => Bits::Q6K,
                (false, ProjGroup::MlpIn) => Bits::Q4K,
            };
            rows.push((layer, group, bits));
        }
    }
    QuantRecipe::Table(rows)
}

/// 양자화한 가중치로 계산하는 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kernel {
    /// 계산 직전에 층 하나씩 F32로 풀어(dequantize) F32 GEMM으로 누산한다. 보관은 양자화된 크기,
    /// 오차는 가중치 양자화뿐이다 (활성값은 F32 그대로). 기본값
    #[default]
    Dequant,
    /// candle의 양자화 행렬곱. 활성값도 8비트로 바꿔 내적한다 (오차가 더 크고, 이 모델의 CPU 실측에서는 6~9배 느렸다)
    Native,
}

impl Kernel {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dequant" | "" => Some(Self::Dequant),
            "native" => Some(Self::Native),
            _ => None,
        }
    }
}

/// 선형층: F32 그대로이거나 양자화
pub enum Lin {
    Dense(Linear),
    /// candle 양자화 행렬곱 (Kernel::Native)
    Quant(QMatMul),
    /// 양자화해 보관하고 계산할 때 F32로 푼다 (Kernel::Dequant)
    Packed(QTensor),
}

impl Lin {
    /// `weight`(F32, 원본 BF16에서 정확히 바꾼 값)를 그대로 쓰거나 한 번 양자화한다
    pub fn new(weight: Tensor, bits: Option<Bits>, kernel: Kernel) -> Result<Self> {
        match (bits, kernel) {
            (None, _) => Ok(Self::Dense(Linear::new(weight, None))),
            (Some(b), Kernel::Native) => Ok(Self::Quant(QMatMul::from_qtensor(QTensor::quantize(&weight, b.ggml())?)?)),
            (Some(b), Kernel::Dequant) => Ok(Self::Packed(QTensor::quantize(&weight, b.ggml())?)),
        }
    }

    /// 캐시에서 읽은(이미 한 번 양자화된) 텐서로 만든다. 다시 양자화하지 않는다.
    pub fn from_qtensor(q: QTensor, kernel: Kernel) -> Result<Self> {
        match kernel {
            Kernel::Native => Ok(Self::Quant(QMatMul::from_qtensor(q)?)),
            Kernel::Dequant => Ok(Self::Packed(q)),
        }
    }

    /// 양자화된 가중치 (캐시에 쓸 때). F32 층이면 None
    pub fn qtensor(&self) -> Option<&QTensor> {
        match self {
            Self::Packed(q) => Some(q),
            Self::Quant(QMatMul::QTensor(q)) => Some(q),
            _ => None,
        }
    }

    /// 메모리에 있는 가중치 크기 (바이트)
    pub fn weight_bytes(&self) -> usize {
        match self {
            Self::Dense(l) => l.weight().elem_count() * 4,
            Self::Quant(QMatMul::QTensor(q)) => q.storage_size_in_bytes(),
            Self::Packed(q) => q.storage_size_in_bytes(),
            Self::Quant(QMatMul::Tensor(t)) => t.elem_count() * t.dtype().size_in_bytes(),
            Self::Quant(QMatMul::TensorF16(t)) => t.elem_count() * 2,
        }
    }
}

impl Module for Lin {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        match self {
            Self::Dense(l) => l.forward(x),
            Self::Quant(QMatMul::QTensor(q))
                if x.device().is_cuda() || x.device().is_metal() || x.device().is_rocm() =>
            {
                q.forward_exact(x)
            }
            Self::Quant(q) => q.forward(x),
            // On CUDA/Metal/ROCm, `forward_exact` runs exact weight-only matmul without materializing
            // a full F32 weight buffer every call (including Metal's native F32-activation Q8 kernel).
            Self::Packed(q)
                if x.device().is_cuda() || x.device().is_metal() || x.device().is_rocm() =>
            {
                q.forward_exact(x)
            }
            // 층 하나 분량(최대 2048 x 512 x 4 = 4MB)만 잠깐 F32로 풀었다가 버린다
            Self::Packed(q) => Linear::new(q.dequantize(x.device())?, None).forward(x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{Device, D};

    fn weight(out: usize, inp: usize) -> Tensor {
        let v: Vec<f32> = (0..out * inp).map(|i| ((i as f32) * 0.618).sin() * 0.05).collect();
        Tensor::from_vec(v, (out, inp), &Device::Cpu).unwrap()
    }

    fn cosine(a: &Tensor, b: &Tensor) -> f32 {
        let dot = (a * b).unwrap().sum_all().unwrap().to_scalar::<f32>().unwrap();
        let na = a.sqr().unwrap().sum_all().unwrap().sqrt().unwrap().to_scalar::<f32>().unwrap();
        let nb = b.sqr().unwrap().sum_all().unwrap().sqrt().unwrap().to_scalar::<f32>().unwrap();
        dot / (na * nb)
    }

    #[test]
    fn quantized_layers_keep_shape_shrink_memory_and_track_the_dense_result() {
        let w = weight(512, 1024);
        let x = Tensor::from_vec((0..3 * 1024).map(|i| ((i as f32) * 0.37).cos()).collect::<Vec<f32>>(), (1, 3, 1024), &Device::Cpu).unwrap();
        let dense = Lin::new(w.clone(), None, Kernel::Native).unwrap();
        let reference = dense.forward(&x).unwrap();
        assert_eq!(dense.weight_bytes(), 512 * 1024 * 4);
        let mut last = 0usize;
        // 비트가 적을수록 작고, 결과는 F32와 가깝다 (가까운 정도는 비트 순)
        let mut prev_cos = 1.0f32;
        for bits in [Bits::Q8_0, Bits::Q6K, Bits::Q5K, Bits::Q4K] {
            let q = Lin::new(w.clone(), Some(bits), Kernel::Native).unwrap();
            let y = q.forward(&x).unwrap();
            assert_eq!(y.dims(), reference.dims(), "{bits:?}");
            let c = cosine(&y.flatten_all().unwrap(), &reference.flatten_all().unwrap());
            assert!(c > 0.97, "{bits:?}: cos {c}");
            assert!(c <= prev_cos + 1e-4, "{bits:?}: 비트를 줄였는데 더 정확해졌다 ({c} > {prev_cos})");
            prev_cos = c;
            let bytes = q.weight_bytes();
            assert!(bytes < 512 * 1024 * 4 / 3, "{bits:?}: {bytes}");
            if last > 0 {
                assert!(bytes < last, "{bits:?}: 비트가 적으면 작아야 한다");
            }
            last = bytes;
        }
        let _ = reference.max(D::Minus1);
    }

    #[test]
    fn the_dequant_kernel_keeps_quantized_storage_and_is_at_least_as_accurate_as_native() {
        let w = weight(512, 1024);
        let x = Tensor::from_vec((0..5 * 1024).map(|i| ((i as f32) * 0.37).cos()).collect::<Vec<f32>>(), (1, 5, 1024), &Device::Cpu).unwrap();
        let reference = Lin::new(w.clone(), None, Kernel::Dequant).unwrap().forward(&x).unwrap().flatten_all().unwrap();
        for bits in [Bits::Q8_0, Bits::Q6K, Bits::Q5K, Bits::Q4K] {
            let native = Lin::new(w.clone(), Some(bits), Kernel::Native).unwrap();
            let packed = Lin::new(w.clone(), Some(bits), Kernel::Dequant).unwrap();
            assert_eq!(native.weight_bytes(), packed.weight_bytes(), "{bits:?}: 보관 크기는 같다");
            let cn = cosine(&native.forward(&x).unwrap().flatten_all().unwrap(), &reference);
            let cp = cosine(&packed.forward(&x).unwrap().flatten_all().unwrap(), &reference);
            eprintln!("{bits:?}: dequant {cp:.7} native {cn:.7} (차이 {:+.1e})", cp - cn);
            if bits == Bits::Q8_0 {
                // 앱이 쓰는 비트(QuantSetting::Q8_0)에서는 가중치만 양자화한 쪽이 활성값까지 양자화한 쪽보다 나빠지지 않는다
                assert!(cp + 1e-6 >= cn, "{bits:?}: 가중치만 양자화한 쪽({cp})이 활성값까지 양자화한 쪽({cn})보다 나빠지지 않는다");
            } else {
                // K-quant는 candle-rocm-vulkan 포크(rev ebaf93d)에서 native가 비슷하거나 조금 낫다 (Q5K +5.2e-5, Q4K +2.0e-4).
                // 양자화 · 역양자화 · 내적 코드는 candle 0.9.2와 같고, CPU 양자화 행렬곱 경로가 바뀌었다. 같은 수준인지만 본다
                assert!((cp - cn).abs() <= 5e-4, "{bits:?}: 두 커널의 정확도가 크게 다르다 (dequant {cp}, native {cn})");
            }
        }
    }

    #[test]
    fn the_quant_setting_is_q8_on_phones_and_f32_on_desktops_by_default() {
        assert_eq!(QuantSetting::parse("").unwrap(), QuantSetting::Auto);
        assert_eq!(QuantSetting::parse("Q8").unwrap(), QuantSetting::Q8_0);
        assert!(QuantSetting::parse("q4k").is_err(), "실측에서 0.999를 못 지킨 비트는 설정으로 고를 수 없다");
        assert_eq!(QuantSetting::Auto.recipe("desktop"), QuantRecipe::Off);
        assert_eq!(QuantSetting::Auto.recipe("android").bits(7, Proj::Gate), Some(Bits::Q8_0));
        assert_eq!(QuantSetting::Off.recipe("ios"), QuantRecipe::Off);
        assert_eq!(QuantSetting::Q8_0.recipe("desktop").bits(23, Proj::PleProj), Some(Bits::Q8_0));
    }

    #[test]
    fn recipes_pick_bits_per_layer_and_group() {
        assert_eq!(QuantRecipe::Off.bits(3, Proj::Q), None);
        let u = QuantRecipe::Uniform { groups: vec![ProjGroup::MlpIn], bits: Bits::Q4K };
        assert_eq!(u.bits(0, Proj::Gate), Some(Bits::Q4K));
        assert_eq!(u.bits(0, Proj::Up), Some(Bits::Q4K));
        assert_eq!(u.bits(0, Proj::Down), None);
        let only = QuantRecipe::Only { layers: vec![5], groups: vec![ProjGroup::Attn], bits: Bits::Q8_0 };
        assert_eq!(only.bits(5, Proj::V), Some(Bits::Q8_0));
        assert_eq!(only.bits(6, Proj::V), None);
        let t = QuantRecipe::Table(vec![(0, ProjGroup::Ple, Bits::Q6K)]);
        assert_eq!(t.bits(0, Proj::PleGate), Some(Bits::Q6K));
        assert_eq!(t.bits(1, Proj::PleGate), None);
        assert!(QuantRecipe::Table(vec![]).is_off() && !t.is_off());
        // recipe_key: 끔이면 없음, 전부 Q8_0이면 읽기 쉬운 이름, 그 밖은 레시피 내용에서 나온 이름
        assert_eq!(QuantRecipe::Off.key("text"), None);
        assert_eq!(QuantSetting::Q8_0.recipe("desktop").key("text").as_deref(), Some("eg2_text_q8_0_v1"));
        let k1 = t.key("text").unwrap();
        assert!(k1.starts_with("eg2_text_custom-") && k1.ends_with("_v1"));
        assert_eq!(k1, QuantRecipe::Table(vec![(0, ProjGroup::Ple, Bits::Q6K)]).key("text").unwrap(), "같은 레시피는 같은 이름");
        assert_ne!(k1, QuantRecipe::Table(vec![(0, ProjGroup::Ple, Bits::Q8_0)]).key("text").unwrap());
        assert_eq!(Bits::parse("Q5_K"), Some(Bits::Q5K));
        assert_eq!(ProjGroup::parse("down"), Some(ProjGroup::MlpOut));
    }
}
