//! 임베딩 모델(candle)이 쓰는 디바이스 헬퍼.
//!
//! candle은 candle-rocm-vulkan 포크다 (Cargo.toml의 [patch.crates-io]). 빌드 기능(cuda · rocm · vulkan · metal)으로
//! 켠 백엔드만 들어가고, 실행할 때는 `PD_DEVICE`(auto · cpu · cuda · rocm · vulkan · metal)로 장치를 고른다.
//! auto는 켜 둔 GPU 백엔드 가운데 처음 열리는 것을 쓰고(CUDA → ROCm → Vulkan → Metal), 하나도 안 열리면 CPU다.
//! 장치 선택은 raon-zena의 raon-infer · logis-center와 같은 순서다.

use anyhow::{bail, Result};
use candle_core::Device;

pub trait GpuDeviceExt {
    fn is_cuda_or_rocm(&self) -> bool;
}

impl GpuDeviceExt for Device {
    fn is_cuda_or_rocm(&self) -> bool {
        self.is_cuda() || self.is_rocm()
    }
}

/// 장치를 고르는 환경 변수. 값이 없으면 CPU다
pub const DEVICE_ENV: &str = "PD_DEVICE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Cpu,
    Cuda,
    Rocm,
    Vulkan,
    Metal,
}

impl std::str::FromStr for Backend {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "gpu" => Backend::Auto,
            "cpu" => Backend::Cpu,
            "cuda" => Backend::Cuda,
            "rocm" | "hip" => Backend::Rocm,
            "vulkan" | "vk" => Backend::Vulkan,
            "metal" => Backend::Metal,
            _ => bail!("알 수 없는 장치 {s} (auto, cpu, cuda, rocm, vulkan, metal)"),
        })
    }
}

/// 이 바이너리에 들어간 백엔드
pub fn compiled_backends() -> Vec<&'static str> {
    let mut v = vec!["cpu"];
    if cfg!(feature = "cuda") {
        v.push("cuda");
    }
    if cfg!(feature = "rocm") {
        v.push("rocm");
    }
    if cfg!(feature = "vulkan") {
        v.push("vulkan");
    }
    if cfg!(feature = "metal") {
        v.push("metal");
    }
    v
}

/// 빌드에 없는 백엔드는 None
fn open(b: Backend, ordinal: usize) -> Result<Option<Device>> {
    let _ = ordinal;
    Ok(match b {
        #[cfg(feature = "cuda")]
        Backend::Cuda => Some(Device::new_cuda(ordinal)?),
        #[cfg(feature = "rocm")]
        Backend::Rocm => Some(Device::new_rocm(ordinal)?),
        #[cfg(feature = "vulkan")]
        Backend::Vulkan => Some(Device::new_vulkan(ordinal)?),
        #[cfg(feature = "metal")]
        Backend::Metal => Some(Device::new_metal(ordinal)?),
        Backend::Cpu => Some(Device::Cpu),
        _ => None,
    })
}

/// `b`로 장치를 연다. 백엔드를 지정했는데 빌드에 없거나 열리지 않으면 오류다
pub fn select_device(b: Backend, ordinal: usize) -> Result<Device> {
    if b != Backend::Auto {
        return match open(b, ordinal)? {
            Some(d) => Ok(d),
            None => bail!("{b:?} 백엔드가 빌드에 없습니다 (빌드: {})", compiled_backends().join(", ")),
        };
    }
    for cand in [Backend::Cuda, Backend::Rocm, Backend::Vulkan, Backend::Metal] {
        if let Ok(Some(d)) = open(cand, ordinal) {
            return Ok(d);
        }
    }
    Ok(Device::Cpu)
}

/// `PD_DEVICE`로 고른 장치. 값이 없으면 CPU다
pub fn device_from_env() -> Result<Device> {
    match std::env::var(DEVICE_ENV) {
        Ok(v) if !v.trim().is_empty() => select_device(v.parse()?, 0),
        _ => Ok(Device::Cpu),
    }
}

/// 기록용 장치 이름
pub fn describe(d: &Device) -> String {
    if d.is_cpu() {
        return "cpu".into();
    }
    format!("{:?}", d.location())
}

/// GPU가 쥐고 있는 캐시 메모리를 드라이버에 돌려준다. GPU를 다 쓰고 프로세스를 끝내기 전에 부른다.
/// Windows HIP는 포크의 비동기 메모리 풀이 남은 채 프로세스가 끝나면 종료 단계에서 멈추고, 그 프로세스는
/// 재부팅 전까지 코어 하나와 VRAM을 쥔다 (gravity-ocr · noto-sandbox에서 확인). 텐서를 모두 놓은 뒤에 부른다.
pub fn release_gpu(device: &Device) -> Result<()> {
    #[cfg(feature = "rocm")]
    if let Device::Rocm(d) = device {
        d.release_cached_resources()?;
    }
    let _ = device;
    Ok(())
}

/// (free, total) 바이트. 원본 GPU 메모리 조회 구현이 없어서 자리표시자로 None을 반환한다.
/// None이면 free 메모리를 0으로 보고, 관측 후에는 1스레드로 내려간다 (보수적).
pub fn gpu_mem_info(_device_index: usize) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::quantized::{GgmlDType, QMatMul, QTensor};
    use candle_core::{Module, Tensor};

    fn max_abs_diff(a: &Tensor, b: &Tensor) -> Result<f32> {
        Ok((a.to_device(&Device::Cpu)? - b.to_device(&Device::Cpu)?)?.abs()?.flatten_all()?.max(0)?.to_scalar::<f32>()?)
    }

    #[test]
    fn backend_names_parse() {
        assert_eq!("ROCm".parse::<Backend>().unwrap(), Backend::Rocm);
        assert_eq!(" hip ".parse::<Backend>().unwrap(), Backend::Rocm);
        assert_eq!("gpu".parse::<Backend>().unwrap(), Backend::Auto);
        assert_eq!("vk".parse::<Backend>().unwrap(), Backend::Vulkan);
        assert!("tpu".parse::<Backend>().is_err());
        assert!(select_device(Backend::Cpu, 0).unwrap().is_cpu());
        assert!(compiled_backends().contains(&"cpu"));
    }

    /// GPU 백엔드를 켜고 빌드했고(`--features rocm` …) 장치가 있으면, 밀집 · 양자화 계산이 그 장치에서 돌고
    /// CPU와 같은 값을 낸다. GPU가 없으면 CPU가 골라지는지만 본다.
    #[test]
    fn gpu_backend_runs_and_matches_cpu() -> Result<()> {
        let dev = select_device(Backend::Auto, 0)?;
        eprintln!("장치: {} (빌드: {})", describe(&dev), compiled_backends().join(", "));
        if dev.is_cpu() {
            return Ok(());
        }
        assert!(dev.is_cuda_or_rocm() || dev.is_vulkan() || dev.is_metal());

        // GPU 텐서는 이 블록 안에서만 살고, 끝나면 release_gpu로 풀을 비운다 (Windows HIP 종료 멈춤 방지)
        let (dense, quant, scale) = {
            let a = Tensor::randn(0f32, 1.0, (8, 256), &Device::Cpu)?;
            let w = Tensor::randn(0f32, 0.1, (512, 256), &Device::Cpu)?;
            let cpu = candle_nn::ops::softmax_last_dim(&a.matmul(&w.t()?)?)?;
            let gpu = candle_nn::ops::softmax_last_dim(&a.to_device(&dev)?.matmul(&w.to_device(&dev)?.t()?)?)?;
            let dense = max_abs_diff(&cpu, &gpu)?;

            // 앱의 양자화 경로(quant.rs)가 쓰는 Q8_0. 장치마다 활성값 양자화가 달라 출력 크기의 5 % 안에서 본다 (raon-infer와 같은 기준)
            let cpu_q = QMatMul::from_qtensor(QTensor::quantize(&w, GgmlDType::Q8_0)?)?.forward(&a)?;
            let gpu_q = QMatMul::from_qtensor(QTensor::quantize(&w.to_device(&dev)?, GgmlDType::Q8_0)?)?.forward(&a.to_device(&dev)?)?;
            let scale = cpu_q.abs()?.flatten_all()?.max(0)?.to_scalar::<f32>()?;
            (dense, max_abs_diff(&cpu_q, &gpu_q)?, scale)
        };
        release_gpu(&dev)?;
        assert!(dense < 1e-4, "softmax(matmul) 차이 {dense}");
        assert!(quant <= 0.05 * scale.max(1e-3), "Q8_0 행렬곱 차이 {quant} (크기 {scale})");
        Ok(())
    }
}
