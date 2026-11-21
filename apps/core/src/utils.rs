//! 임베딩 모델(candle)이 쓰는 디바이스 헬퍼.
//!
//! candle은 candle-rocm-vulkan 포크다 (Cargo.toml의 [patch.crates-io]). 빌드 기능(cuda · rocm · vulkan · metal)으로
//! 켠 백엔드만 들어가고, 실행할 때는 `PD_DEVICE`(auto · cpu · cuda · rocm · vulkan · metal)로 장치를 고른다.
//! auto는 켜 둔 GPU 백엔드 가운데 처음 열리는 것을 쓰고(CUDA → ROCm → Vulkan → Metal), 하나도 안 열리면 CPU다.
//! 장치 선택은 raon-zena의 raon-infer · logis-center와 같은 순서다.

use std::collections::HashMap;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Result};
use candle_core::{DType, Device, Tensor};

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
/// ROCm 빠른 종료를 끌 때만 `0` 지정
pub const FAST_EXIT_ENV: &str = "PD_ROCM_FAST_EXIT";

static ROCM_USED: AtomicBool = AtomicBool::new(false);
static OPENED_GPU: OnceLock<Mutex<Option<Device>>> = OnceLock::new();
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn opened_gpu() -> &'static Mutex<Option<Device>> {
    OPENED_GPU.get_or_init(|| Mutex::new(None))
}

fn record_opened_device(d: &Device) {
    if d.is_rocm() {
        ROCM_USED.store(true, Ordering::Relaxed);
    }
    if !d.is_cpu() {
        *opened_gpu().lock().unwrap_or_else(|p| p.into_inner()) = Some(d.clone());
    }
}

/// Windows 등에서 기존 파일을 크래시 없이 원자적으로 교체한다.
pub fn atomic_replace(tmp: &Path, target: &Path) -> std::io::Result<()> {
    match std::fs::rename(tmp, target) {
        Ok(()) => Ok(()),
        Err(_) if target.exists() => {
            let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let mut bak_os = target.as_os_str().to_os_string();
            bak_os.push(format!(".bak.{}.{seq}", std::process::id()));
            let bak = std::path::PathBuf::from(bak_os);
            let _ = std::fs::remove_file(&bak);
            std::fs::rename(target, &bak)?;
            match std::fs::rename(tmp, target) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&bak);
                    Ok(())
                }
                Err(e) => {
                    let _ = std::fs::rename(&bak, target);
                    Err(e)
                }
            }
        }
        Err(e) => Err(e),
    }
}

/// 고유 임시 파일에 쓰고 `sync_all()` 뒤 대상 경로로 원자적 교체한다.
/// (`std::fs::write` 와 동일하게 존재하지 않는 상위 폴더는 자동으로 만들지 않는다.)
pub fn write_file_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp_os = target.as_os_str().to_os_string();
    tmp_os.push(format!(".tmp.{}.{seq}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp_os);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = atomic_replace(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[cfg(feature = "vulkan")]
fn open_best_vulkan(ordinal: usize) -> Result<Device> {
    if ordinal != 0 {
        return Ok(Device::new_vulkan(ordinal)?);
    }
    let count = candle_core::vulkan_backend::device_count().unwrap_or(0).min(8);
    if count <= 1 {
        return Ok(Device::new_vulkan(0)?);
    }
    let mut best_dev: Option<(u64, Device)> = None;
    for id in 0..count {
        if let Ok(d) = Device::new_vulkan(id) {
            let free = d.mem_info().ok().flatten().map_or(0, |(f, _)| f as u64);
            if best_dev.as_ref().is_none_or(|(b, _)| free > *b) {
                best_dev = Some((free, d));
            }
        }
    }
    match best_dev {
        Some((_, d)) => Ok(d),
        None => Ok(Device::new_vulkan(0)?),
    }
}

/// 빌드에 없는 백엔드는 None
fn open(b: Backend, ordinal: usize) -> Result<Option<Device>> {
    let _ = ordinal;
    let opened = match b {
        #[cfg(feature = "cuda")]
        Backend::Cuda => Some(Device::new_cuda(ordinal)?),
        #[cfg(feature = "rocm")]
        Backend::Rocm => Some(Device::new_rocm(ordinal)?),
        #[cfg(feature = "vulkan")]
        Backend::Vulkan => Some(open_best_vulkan(ordinal)?),
        #[cfg(feature = "metal")]
        Backend::Metal => Some(Device::new_metal(ordinal)?),
        Backend::Cpu => Some(Device::Cpu),
        _ => None,
    };
    if let Some(ref d) = opened {
        record_opened_device(d);
    }
    Ok(opened)
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
        match open(cand, ordinal) {
            Ok(Some(d)) => return Ok(d),
            Ok(None) => {}
            Err(e) => eprintln!("[PD-ACCEL] {cand:?} 장치 열기 실패 → 다음 백엔드 시도: {e}"),
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

/// 백엔드와 ordinal 을 포함한 표시 이름 (예: `rocm:0`, `vulkan:0`, `cpu`)
pub fn device_label(d: &Device) -> String {
    match d.location() {
        candle_core::DeviceLocation::Cpu => "cpu".into(),
        candle_core::DeviceLocation::Cuda { gpu_id } => format!("cuda:{gpu_id}"),
        candle_core::DeviceLocation::Rocm { gpu_id } => format!("rocm:{gpu_id}"),
        candle_core::DeviceLocation::Metal { gpu_id } => format!("metal:{gpu_id}"),
        candle_core::DeviceLocation::Vulkan { gpu_id } => format!("vulkan:{gpu_id}"),
        candle_core::DeviceLocation::OpenCl { gpu_id } => format!("opencl:{gpu_id}"),
    }
}

/// 기록용 장치 이름
pub fn describe(d: &Device) -> String {
    if d.is_cpu() {
        return "cpu".into();
    }
    format!("{:?}", d.location())
}

/// GPU가 쥐고 있는 캐시 메모리를 드라이버에 돌려준다. 텐서를 모두 놓은 뒤에 부른다.
pub fn release_gpu(device: &Device) -> Result<()> {
    if !device.is_cpu() {
        let _ = device.synchronize_all();
        let _ = device.release_workspaces();
        let _ = device.trim_memory_pool(0);
    }
    #[cfg(feature = "rocm")]
    if let Device::Rocm(d) = device {
        d.release_cached_resources()?;
    }
    Ok(())
}

/// 이 프로세스가 실제로 열었던 GPU가 있을 때만 풀을 반환한다 (종료 시 새 GPU 컨텍스트를 열지 않음).
pub fn release_opened_gpu() -> bool {
    let dev = opened_gpu().lock().unwrap_or_else(|p| p.into_inner()).take();
    match dev {
        Some(d) => release_gpu(&d).is_ok(),
        None => false,
    }
}

#[cfg(windows)]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn TerminateProcess(process: isize, code: u32) -> i32;
}

/// ROCm 을 한 번이라도 사용한 Windows 프로세스에서 `rocblas.dll` 종료 멈춤을 방지한다.
pub fn rocm_exit_now_if_used(code: u32) {
    if !ROCM_USED.load(Ordering::Relaxed)
        || std::env::var(FAST_EXIT_ENV).is_ok_and(|v| v.trim() == "0")
    {
        return;
    }
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(windows)]
    unsafe {
        TerminateProcess(GetCurrentProcess(), code);
    }
    #[cfg(not(windows))]
    let _ = code;
}

/// (free, total) 바이트. 현재 열려 있는 GPU(없으면 None)의 실제 여유/전체 VRAM 을 돌려준다.
pub fn gpu_mem_info(_device_index: usize) -> Option<(u64, u64)> {
    let dev = opened_gpu().lock().unwrap_or_else(|p| p.into_inner()).clone()?;
    dev.mem_info().ok().flatten().map(|(f, t)| (f as u64, t as u64))
}

// ── Per-(Backend, ordinal, AccelOp) catch_unwind 자가 진단 ────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccelOp {
    Matmul,
    Softmax,
    LayerNorm,
    FlashAttn,
    DecoderOps,
    LmHeadArgmax,
    QuantQ8,
}

static ACCEL_VERDICTS: OnceLock<Mutex<HashMap<(String, AccelOp), bool>>> = OnceLock::new();
static ACCEL_FAULTS: OnceLock<Mutex<HashMap<AccelOp, bool>>> = OnceLock::new();

fn accel_verdicts() -> &'static Mutex<HashMap<(String, AccelOp), bool>> {
    ACCEL_VERDICTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn accel_faults() -> &'static Mutex<HashMap<AccelOp, bool>> {
    ACCEL_FAULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 테스트용 결함 주입 (지정 연산을 강제 실패시켜 CPU 폴백 경로를 검증한다).
pub fn set_accel_fault(op: AccelOp, fail: bool) {
    accel_faults().lock().unwrap_or_else(|p| p.into_inner()).insert(op, fail);
    accel_verdicts().lock().unwrap_or_else(|p| p.into_inner()).retain(|(_, o), _| *o != op);
}

pub fn reset_accel_cache() {
    accel_verdicts().lock().unwrap_or_else(|p| p.into_inner()).clear();
    accel_faults().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

fn det_vec(n: usize, seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
        })
        .collect()
}

fn rel_err_f32(got: &Tensor, want: &Tensor) -> Result<f32> {
    let g = got.to_device(&Device::Cpu)?.to_dtype(DType::F32)?;
    let w = want.to_device(&Device::Cpu)?.to_dtype(DType::F32)?;
    let d = (&g - &w)?.sqr()?.sum_all()?.to_scalar::<f32>()?;
    let n = w.sqr()?.sum_all()?.to_scalar::<f32>()?;
    let r = (d / n.max(1e-30)).sqrt();
    Ok(if r.is_finite() { r } else { f32::INFINITY })
}

/// 장치별 연산 자가 검증: 최초 1회 CPU 기준값과 비교하고 `catch_unwind` 로 패닉을 격리한다.
pub fn accel_op_ok(device: &Device, op: AccelOp) -> bool {
    if device.is_cpu() {
        return true;
    }
    if accel_faults().lock().unwrap_or_else(|p| p.into_inner()).get(&op).copied().unwrap_or(false) {
        return false;
    }
    let key = (device_label(device), op);
    if let Some(&ok) = accel_verdicts().lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return ok;
    }
    let ok = match catch_unwind(AssertUnwindSafe(|| probe_accel_op(device, op))) {
        Ok(Ok(err)) if err <= 5e-3 => true,
        Ok(Ok(err)) => {
            eprintln!("[PD-ACCEL] ❌ {} {:?}: 상대오차 {err:.2e} > 5e-3 → 폴백", key.0, op);
            false
        }
        Ok(Err(e)) => {
            eprintln!("[PD-ACCEL] ❌ {} {:?}: 검증 실패 ({e}) → 폴백", key.0, op);
            false
        }
        Err(_) => {
            eprintln!("[PD-ACCEL] ❌ {} {:?}: 검증 패닉 → 폴백", key.0, op);
            false
        }
    };
    accel_verdicts().lock().unwrap_or_else(|p| p.into_inner()).insert(key, ok);
    ok
}

fn probe_accel_op(device: &Device, op: AccelOp) -> Result<f32> {
    match op {
        AccelOp::Matmul => {
            let a = Tensor::from_vec(det_vec(16 * 32, 1), (16, 32), &Device::Cpu)?;
            let b = Tensor::from_vec(det_vec(32 * 16, 2), (32, 16), &Device::Cpu)?;
            let want = a.matmul(&b)?;
            let got = a.to_device(device)?.matmul(&b.to_device(device)?)?;
            rel_err_f32(&got, &want)
        }
        AccelOp::Softmax => {
            let a = Tensor::from_vec(det_vec(8 * 64, 3), (8, 64), &Device::Cpu)?;
            let want = candle_nn::ops::softmax_last_dim(&a)?;
            let got = candle_nn::ops::softmax_last_dim(&a.to_device(device)?)?;
            rel_err_f32(&got, &want)
        }
        AccelOp::LayerNorm => {
            let x = Tensor::from_vec(det_vec(4 * 64, 4), (4, 64), &Device::Cpu)?;
            let w = Tensor::from_vec(det_vec(64, 5), 64, &Device::Cpu)?;
            let b = Tensor::from_vec(det_vec(64, 6), 64, &Device::Cpu)?;
            let want = candle_nn::ops::layer_norm(&x, &w, &b, 1e-5)?;
            let got = candle_nn::ops::layer_norm(&x.to_device(device)?, &w.to_device(device)?, &b.to_device(device)?, 1e-5)?;
            rel_err_f32(&got, &want)
        }
        AccelOp::FlashAttn => {
            let (bh, n, d) = (2usize, 16usize, 64usize);
            let q = Tensor::from_vec(det_vec(bh * n * d, 7), (1, bh, n, d), &Device::Cpu)?;
            let k = Tensor::from_vec(det_vec(bh * n * d, 8), (1, bh, n, d), &Device::Cpu)?;
            let v = Tensor::from_vec(det_vec(bh * n * d, 9), (1, bh, n, d), &Device::Cpu)?;
            let scale = 1.0f32 / (d as f32).sqrt();
            let scores = (q.matmul(&k.transpose(2, 3)?.contiguous()?)? * scale as f64)?;
            let want = candle_nn::ops::softmax_last_dim(&scores)?.matmul(&v)?;
            let cfg = candle_nn::flash_attn::FlashAttnConfig::new(scale);
            let got = candle_nn::flash_attn::flash_attn_bhsd(&q.to_device(device)?, &k.to_device(device)?, &v.to_device(device)?, &cfg)?;
            rel_err_f32(&got, &want)
        }
        AccelOp::DecoderOps => {
            let x = Tensor::from_vec(det_vec(4 * 128, 10), (4, 128), &Device::Cpu)?;
            let d = Tensor::from_vec(det_vec(4 * 128, 11), (4, 128), &Device::Cpu)?;
            let w = Tensor::from_vec(det_vec(128, 12), 128, &Device::Cpu)?;
            let (wr, wy) = candle_core::decoder_ops::residual_rmsnorm(&x, Some(&d), &w, 1e-5, false)?;
            let (gr, gy) = candle_core::decoder_ops::residual_rmsnorm(
                &x.to_device(device)?,
                Some(&d.to_device(device)?),
                &w.to_device(device)?,
                1e-5,
                false,
            )?;
            let e1 = rel_err_f32(&gr, &wr)?;
            let e2 = rel_err_f32(&gy, &wy)?;
            let ws = candle_core::decoder_ops::silu_mul(&x, &d)?;
            let gs = candle_core::decoder_ops::silu_mul(&x.to_device(device)?, &d.to_device(device)?)?;
            Ok(e1.max(e2).max(rel_err_f32(&gs, &ws)?))
        }
        AccelOp::LmHeadArgmax => {
            let l = Tensor::from_vec(det_vec(2 * 64, 13), (2, 64), &Device::Cpu)?;
            let want = candle_core::lm_head::logits_argmax_cs(&l, None, None, None)?;
            let got = candle_core::lm_head::logits_argmax_cs(&l.to_device(device)?, None, None, None)?;
            if want.ids.to_vec1::<u32>()? != got.ids.to_device(&Device::Cpu)?.to_vec1::<u32>()? {
                return Ok(f32::INFINITY);
            }
            rel_err_f32(got.max_vals.as_ref().unwrap(), want.max_vals.as_ref().unwrap())
        }
        AccelOp::QuantQ8 => {
            use candle_core::quantized::{GgmlDType, QMatMul, QTensor};
            use candle_core::Module;
            let a = Tensor::from_vec(det_vec(4 * 64, 14), (4, 64), &Device::Cpu)?;
            let w = Tensor::from_vec(det_vec(64 * 64, 15), (64, 64), &Device::Cpu)?;
            let qt_cpu = QTensor::quantize(&w, GgmlDType::Q8_0)?;
            let want = a.matmul(&qt_cpu.dequantize(&Device::Cpu)?.t()?)?;
            let qd = QMatMul::from_qtensor(QTensor::quantize(&w.to_device(device)?, GgmlDType::Q8_0)?)?;
            let got = if device.is_cuda() || device.is_metal() || device.is_rocm() {
                qd.forward_exact(&a.to_device(device)?)?
            } else {
                qd.forward(&a.to_device(device)?)?
            };
            let err = rel_err_f32(&got, &want)?;
            // Exact paths match dequantized F32 reference within ~1e-3; Vulkan Q8 activation quant within 5e-2.
            let tol = if device.is_cuda() || device.is_metal() || device.is_rocm() { 1e-2 } else { 5e-2 };
            Ok(if err <= tol { err * 0.1 } else { err })
        }
    }
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

    #[test]
    fn accel_check_verifies_ops_and_fault_injection() -> Result<()> {
        let dev = select_device(Backend::Auto, 0)?;
        reset_accel_cache();
        for op in [
            AccelOp::Matmul,
            AccelOp::Softmax,
            AccelOp::LayerNorm,
            AccelOp::FlashAttn,
            AccelOp::DecoderOps,
            AccelOp::LmHeadArgmax,
            AccelOp::QuantQ8,
        ] {
            assert!(accel_op_ok(&dev, op), "accel_op_ok failed for {op:?} on {}", describe(&dev));
        }
        if !dev.is_cpu() {
            // Fault injection must trip catch_unwind and cache false without crashing.
            reset_accel_cache();
            set_accel_fault(AccelOp::DecoderOps, true);
            assert!(!accel_op_ok(&dev, AccelOp::DecoderOps));
            set_accel_fault(AccelOp::DecoderOps, false);
            reset_accel_cache();
            assert!(accel_op_ok(&dev, AccelOp::DecoderOps));
            release_gpu(&dev)?;
        }
        Ok(())
    }
}
