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
    // 외장 GPU > 내장 GPU > 가상 > 기타 > 소프트웨어(CPU) 순으로 우선하고, 같은 계층 안에서 여유 VRAM 이 큰 장치를 고른다.
    let mut best: Option<((u8, u64), Device)> = None;
    for id in 0..count {
        if let Ok(d) = Device::new_vulkan(id) {
            let tier: u8 = match d.as_vulkan_device().map(|v| v.device_type()) {
                Ok("discrete") => 4,
                Ok("integrated") => 3,
                Ok("virtual") => 2,
                Ok("other") => 1,
                _ => 0,
            };
            let free = d.mem_info().ok().flatten().map_or(0, |(f, _)| f as u64);
            let rank = (tier, free);
            if best.as_ref().is_none_or(|(b, _)| rank > *b) {
                best = Some((rank, d));
            }
        }
    }
    match best {
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

/// 장치의 GPU 순번(ordinal). CPU 이면 0.
pub fn device_ordinal(d: &Device) -> usize {
    match d.location() {
        candle_core::DeviceLocation::Cpu => 0,
        candle_core::DeviceLocation::Cuda { gpu_id }
        | candle_core::DeviceLocation::Rocm { gpu_id }
        | candle_core::DeviceLocation::Metal { gpu_id }
        | candle_core::DeviceLocation::Vulkan { gpu_id }
        | candle_core::DeviceLocation::OpenCl { gpu_id } => gpu_id,
    }
}

/// 기록용 장치 이름
pub fn describe(d: &Device) -> String {
    if d.is_cpu() {
        return "cpu".into();
    }
    format!("{:?}", d.location())
}

/// 추론 구간 사이 유휴 GPU 메모리 풀을 보수적으로 정리한다.
/// CUDA · ROCm 은 유휴 풀이 320 MiB 이상일 때만 256 MiB 핫 풀을 남기고 돌려주어 할당기 트림 스톨을 방지한다.
pub fn trim_idle_gpu_pool(device: &Device) {
    const HIGH_WATER: usize = 320 << 20;
    const KEEP_HOT: usize = 256 << 20;
    match device {
        #[cfg(feature = "rocm")]
        Device::Rocm(rd) => {
            if rd.pool_idle_bytes().unwrap_or(0) >= HIGH_WATER {
                let _ = rd.trim_memory_pool_to(KEEP_HOT);
            }
        }
        #[cfg(feature = "cuda")]
        Device::Cuda(cd) => {
            if cd.pool_idle_bytes().unwrap_or(0) >= HIGH_WATER {
                let _ = cd.trim_memory_pool(KEEP_HOT);
            }
        }
        #[cfg(feature = "vulkan")]
        Device::Vulkan(vd) => {
            const VK_HIGH_WATER: usize = 64 << 20;
            if vd.pooled_bytes() >= VK_HIGH_WATER {
                let _ = vd.trim_memory_pool();
            }
        }
        _ => {}
    }
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

/// (free, total) 바이트. 지정한 `device_index` GPU의 실제 여유/전체 VRAM 을 돌려준다.
/// 이미 열려 있는 GPU가 같은 순번이면 그 핸들을 재사용하고, 아니면 전역 `OPENED_GPU` 를
/// 덮어쓰지 않고 해당 순번의 장치를 직접 질의한다.
pub fn gpu_mem_info(device_index: usize) -> Option<(u64, u64)> {
    if let Some(dev) = opened_gpu().lock().unwrap_or_else(|p| p.into_inner()).clone() {
        if device_ordinal(&dev) == device_index {
            return dev.mem_info().ok().flatten().map(|(f, t)| (f as u64, t as u64));
        }
    }
    #[cfg(feature = "cuda")]
    if let Ok(d) = Device::new_cuda(device_index) {
        if let Ok(Some((f, t))) = d.mem_info() {
            return Some((f as u64, t as u64));
        }
    }
    #[cfg(feature = "rocm")]
    if let Ok(d) = Device::new_rocm(device_index) {
        ROCM_USED.store(true, Ordering::Relaxed);
        if let Ok(Some((f, t))) = d.mem_info() {
            return Some((f as u64, t as u64));
        }
    }
    #[cfg(feature = "vulkan")]
    if device_index < candle_core::vulkan_backend::device_count().unwrap_or(0) {
        if let Ok(d) = Device::new_vulkan(device_index) {
            if let Ok(Some((f, t))) = d.mem_info() {
                return Some((f as u64, t as u64));
            }
        }
    }
    #[cfg(feature = "metal")]
    if let Ok(d) = Device::new_metal(device_index) {
        if let Ok(Some((f, t))) = d.mem_info() {
            return Some((f as u64, t as u64));
        }
    }
    None
}

// ── Per-(Backend, ordinal, AccelOp) catch_unwind 자가 진단 ────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccelOp {
    Matmul,
    Softmax,
    LayerNorm,
    Rope,
    FlashAttn,
    DecoderOps,
    LmHeadArgmax,
    GatedDeltaRule,
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
/// `fail = true` 일 때만 기존 캐시를 지워 다음 호출이 `catch_unwind` 경로에서 실패·음성 캐시되도록 한다.
/// `fail = false` 로 되돌려도 음성 캐시는 유지되며 `reset_accel_cache()` 로만 재검증한다.
pub fn set_accel_fault(op: AccelOp, fail: bool) {
    accel_faults().lock().unwrap_or_else(|p| p.into_inner()).insert(op, fail);
    if fail {
        accel_verdicts().lock().unwrap_or_else(|p| p.into_inner()).retain(|(_, o), _| *o != op);
    }
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
    let key = (device_label(device), op);
    if let Some(&ok) = accel_verdicts().lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        return ok;
    }
    let ok = match catch_unwind(AssertUnwindSafe(|| {
        if accel_faults()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&op)
            .copied()
            .unwrap_or(false)
        {
            panic!("injected accel fault for {op:?}");
        }
        probe_accel_op(device, op)
    })) {
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
        AccelOp::Rope => {
            let (b, h, t, d) = (1usize, 4usize, 32usize, 64usize);
            let half = d / 2;
            let x = Tensor::from_vec(det_vec(b * h * t * d, 31), (b, h, t, d), &Device::Cpu)?;
            let angles = Tensor::from_vec(det_vec(t * half, 32), (t, half), &Device::Cpu)?;
            let cos = angles.cos()?;
            let sin = angles.sin()?;
            let want = candle_nn::rotary_emb::rope_slow(&x, &cos, &sin)?;
            let got = candle_nn::rotary_emb::rope(
                &x.to_device(device)?,
                &cos.to_device(device)?,
                &sin.to_device(device)?,
            )?;
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
            let mut worst = rel_err_f32(&got, &want)?;
            if device.is_cuda_or_rocm() {
                // Verify causal prefill (32x32, d=128) + causal decode / split-KV (1x300, 1x4096, d=256)
                // + paged flash_attn_blocks (used by Qwen3.5 full-attention layers).
                let check_causal = |hq: usize, hkv: usize, sq: usize, sk: usize, hd: usize, seed: u64| -> Result<f32> {
                    let q_c = Tensor::from_vec(det_vec(hq * sq * hd, seed), (1, hq, sq, hd), &Device::Cpu)?;
                    let k_c = Tensor::from_vec(det_vec(hkv * sk * hd, seed + 1), (1, hkv, sk, hd), &Device::Cpu)?;
                    let v_c = Tensor::from_vec(det_vec(hkv * sk * hd, seed + 2), (1, hkv, sk, hd), &Device::Cpu)?;
                    let sc = 1.0f32 / (hd as f32).sqrt();
                    let reps = hq / hkv;
                    let kf = if reps > 1 {
                        k_c.unsqueeze(2)?.expand((1, hkv, reps, sk, hd))?.contiguous()?.reshape((1, hq, sk, hd))?
                    } else {
                        k_c.clone()
                    };
                    let vf = if reps > 1 {
                        v_c.unsqueeze(2)?.expand((1, hkv, reps, sk, hd))?.contiguous()?.reshape((1, hq, sk, hd))?
                    } else {
                        v_c.clone()
                    };
                    let mut scores = (q_c.matmul(&kf.transpose(2, 3)?.contiguous()?)? * sc as f64)?;
                    if sq > 1 {
                        let offset = sk - sq;
                        let mask: Vec<f32> = (0..sq)
                            .flat_map(|i| (0..sk).map(move |j| if j > i + offset { f32::NEG_INFINITY } else { 0.0 }))
                            .collect();
                        scores = scores.broadcast_add(&Tensor::from_vec(mask, (1, 1, sq, sk), &Device::Cpu)?)?;
                    }
                    let want_c = candle_nn::ops::softmax_last_dim(&scores)?.matmul(&vf)?;
                    let ccfg = candle_nn::flash_attn::FlashAttnConfig::new(sc).causal();
                    let q_d = q_c.to_device(device)?;
                    let k_d = k_c.to_device(device)?;
                    let v_d = v_c.to_device(device)?;
                    let got_c = candle_nn::flash_attn::flash_attn_bhsd(&q_d, &k_d, &v_d, &ccfg)?;
                    let e1 = rel_err_f32(&got_c, &want_c)?;
                    let blk = [candle_nn::flash_attn::KvBlockRef { k: &k_d, v: &v_d, rows: sk }];
                    let got_b = candle_nn::flash_attn::flash_attn_blocks(&q_d, &blk, &ccfg)?;
                    Ok(e1.max(rel_err_f32(&got_b, &want_c)?))
                };
                worst = worst.max(check_causal(4, 2, 32, 32, 128, 40)?);
                worst = worst.max(check_causal(8, 2, 1, 300, 256, 50)?);
                worst = worst.max(check_causal(8, 2, 1, 4096, 256, 60)?);
            }
            Ok(worst)
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
            let (want_ids, _) = want.download_ids_and_mask()?;
            let (got_ids, _) = got.download_ids_and_mask()?;
            if want_ids != got_ids {
                return Ok(f32::INFINITY);
            }
            let e_mode_b = rel_err_f32(got.max_vals.as_ref().unwrap(), want.max_vals.as_ref().unwrap())?;
            // Mode A (`fused_linear_argmax_cs`, Qwen35::next_token 경로): CUDA/ROCm/Vulkan 네이티브 커널 검증
            let h = Tensor::from_vec(det_vec(2 * 64, 16), (2, 64), &Device::Cpu)?;
            let w = Tensor::from_vec(det_vec(128 * 64, 17), (128, 64), &Device::Cpu)?;
            let want_a = candle_core::lm_head::fused_linear_argmax_cs(&h, &w, None, None, None)?;
            let got_a = candle_core::lm_head::fused_linear_argmax_cs(
                &h.to_device(device)?,
                &w.to_device(device)?,
                None,
                None,
                None,
            )?;
            let (want_a_ids, _) = want_a.download_ids_and_mask()?;
            let (got_a_ids, _) = got_a.download_ids_and_mask()?;
            if want_a_ids != got_a_ids {
                return Ok(f32::INFINITY);
            }
            let e_mode_a = rel_err_f32(got_a.max_vals.as_ref().unwrap(), want_a.max_vals.as_ref().unwrap())?;
            let mut worst = e_mode_b.max(e_mode_a);
            if device.is_cuda_or_rocm() {
                use candle_core::quantized::{GgmlDType, QTensor};
                // M=12 (> 8) exercises chunked CUDA/ROCm Quantized Mode A across Q8_0 / Q4_0 / Q4K / Q5K / Q6K.
                let hq = Tensor::from_vec(det_vec(12 * 256, 18), (12, 256), &Device::Cpu)?;
                let wq = Tensor::from_vec(det_vec(64 * 256, 19), (64, 256), &Device::Cpu)?;
                let hq_dev = hq.to_device(device)?;
                let wq_dev = wq.to_device(device)?;
                for dt in [GgmlDType::Q8_0, GgmlDType::Q4_0, GgmlDType::Q4K, GgmlDType::Q5K, GgmlDType::Q6K] {
                    let qt_cpu = QTensor::quantize(&wq, dt)?;
                    let want_q = candle_core::lm_head::logits_argmax_cs(&qt_cpu.forward_exact(&hq)?, None, None, None)?;
                    let qt_dev = QTensor::quantize(&wq_dev, dt)?;
                    let got_q = candle_core::lm_head::fused_quant_argmax_cs(&hq_dev, &qt_dev, None, None, None)?;
                    let (got_ids, _) = got_q.download_ids_and_mask()?;
                    if want_q.ids.to_vec1::<u32>()? != got_ids {
                        return Ok(f32::INFINITY);
                    }
                    if let (Some(gv), Some(wv)) = (got_q.max_vals.as_ref(), want_q.max_vals.as_ref()) {
                        worst = worst.max(rel_err_f32(gv, wv)?);
                    }
                }
            }
            Ok(worst)
        }
        AccelOp::GatedDeltaRule => {
            use candle_core::delta_rule::{fused_gated_delta_rule, fused_gated_rms_norm, GdnConfig};
            let cfg = GdnConfig::new(2, 2, 16, 16);
            let (b, t, c, hv) = (1usize, 6usize, cfg.conv_dim(), cfg.num_v_heads);
            let mixed = Tensor::from_vec(det_vec(b * t * c, 21), (b, t, c), &Device::Cpu)?;
            let conv_w = Tensor::from_vec(det_vec(c * cfg.conv_kernel, 22), (c, cfg.conv_kernel), &Device::Cpu)?;
            let a = Tensor::from_vec(det_vec(b * t * hv, 23), (b, t, hv), &Device::Cpu)?;
            let beta_in = Tensor::from_vec(det_vec(b * t * hv, 24), (b, t, hv), &Device::Cpu)?;
            let neg_a = Tensor::from_vec(vec![-0.5f32; hv], hv, &Device::Cpu)?;
            let dt_bias = Tensor::from_vec(det_vec(hv, 25), hv, &Device::Cpu)?;
            let want = fused_gated_delta_rule(&mixed, &conv_w, None, None, &a, &beta_in, &neg_a, &dt_bias, None, &cfg)?;
            let got = fused_gated_delta_rule(
                &mixed.to_device(device)?,
                &conv_w.to_device(device)?,
                None,
                None,
                &a.to_device(device)?,
                &beta_in.to_device(device)?,
                &neg_a.to_device(device)?,
                &dt_bias.to_device(device)?,
                None,
                &cfg,
            )?;
            let e_out = rel_err_f32(&got.out, &want.out)?;
            let e_conv = rel_err_f32(&got.conv_state, &want.conv_state)?;
            let e_rec = rel_err_f32(&got.recurrent_state, &want.recurrent_state)?;
            // Also verify fused_gated_rms_norm (single-pass GDN output norm + SiLU gate)
            let z = Tensor::from_vec(det_vec(b * t * hv * 16, 26), (b * t * hv, 16), &Device::Cpu)?;
            let nw = Tensor::from_vec(det_vec(16, 27), 16, &Device::Cpu)?;
            let want_gn = fused_gated_rms_norm(&want.out.reshape((b * t * hv, 16))?, &z, &nw, 1e-6)?;
            let got_gn = fused_gated_rms_norm(
                &got.out.reshape((b * t * hv, 16))?,
                &z.to_device(device)?,
                &nw.to_device(device)?,
                1e-6,
            )?;
            let e_gn = rel_err_f32(&got_gn, &want_gn)?;
            Ok(e_out.max(e_conv).max(e_rec).max(e_gn))
        }
        AccelOp::QuantQ8 => {
            use candle_core::quantized::{GgmlDType, QMatMul, QTensor};
            use candle_core::Module;
            let a = Tensor::from_vec(det_vec(4 * 256, 14), (4, 256), &Device::Cpu)?;
            let w = Tensor::from_vec(det_vec(64 * 256, 15), (64, 256), &Device::Cpu)?;
            let a_dev = a.to_device(device)?;
            let w_dev = w.to_device(device)?;
            let mut worst = 0f32;
            for dt in [GgmlDType::Q8_0, GgmlDType::Q4_0, GgmlDType::Q4K, GgmlDType::Q5K, GgmlDType::Q6K] {
                let qt_cpu = QTensor::quantize(&w, dt)?;
                let want = a.matmul(&qt_cpu.dequantize(&Device::Cpu)?.t()?)?;
                let qd = QMatMul::from_qtensor(QTensor::quantize(&w_dev, dt)?)?;
                let got = if device.is_cuda() || device.is_metal() || device.is_rocm() {
                    qd.forward_exact(&a_dev)?
                } else {
                    qd.forward(&a_dev)?
                };
                let err = rel_err_f32(&got, &want)?;
                let tol = if device.is_cuda_or_rocm() || device.is_metal() {
                    1e-3
                } else if device.is_vulkan() {
                    1e-2
                } else {
                    5e-2
                };
                worst = worst.max(if err <= tol { err * 0.1 } else { err });
            }
            Ok(worst)
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
    /// CPU와 같은 값을 낸다. `PD_REQUIRE_GPU=1` 이면 GPU 부재 시 실패한다.
    #[test]
    fn gpu_backend_runs_and_matches_cpu() -> Result<()> {
        let require_gpu = std::env::var("PD_REQUIRE_GPU")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let dev = select_device(Backend::Auto, 0)?;
        eprintln!("장치: {} (빌드: {})", describe(&dev), compiled_backends().join(", "));
        if dev.is_cpu() {
            assert!(!require_gpu, "PD_REQUIRE_GPU=1 이지만 GPU 장치를 열지 못했습니다");
            return Ok(());
        }
        assert!(dev.is_cuda_or_rocm() || dev.is_vulkan() || dev.is_metal());

        #[cfg(feature = "vulkan")]
        let vk_native_before = dev
            .as_vulkan_device()
            .is_ok_and(|v| !v.is_cpu_device())
            .then(candle_core::vulkan_backend::shaders::native_exec_count);

        // GPU 텐서는 이 블록 안에서만 살고, 끝나면 release_gpu로 풀을 비운다 (Windows HIP 종료 멈춤 방지)
        let (dense, quant, scale) = {
            let a = Tensor::randn(0f32, 1.0, (8, 256), &Device::Cpu)?;
            let w = Tensor::randn(0f32, 0.1, (512, 256), &Device::Cpu)?;
            let cpu = candle_nn::ops::softmax_last_dim(&a.matmul(&w.t()?)?)?;
            let gpu = candle_nn::ops::softmax_last_dim(&a.to_device(&dev)?.matmul(&w.to_device(&dev)?.t()?)?)?;
            let dense = max_abs_diff(&cpu, &gpu)?;

            // 앱의 양자화 경로(quant.rs)가 쓰는 Q8_0 · Q4_0 · K-quant(Q4K/Q5K/Q6K).
            // CUDA / ROCm / Metal 은 forward_exact (F32 활성값, ~1e-7 오차), Vulkan 은 네이티브 GpuQuant 를 사용한다.
            let mut max_ratio = 0f32;
            for dt in [GgmlDType::Q8_0, GgmlDType::Q4_0, GgmlDType::Q4K, GgmlDType::Q5K, GgmlDType::Q6K] {
                let q_cpu = QTensor::quantize(&w, dt)?;
                let cpu_q = if dev.is_cuda_or_rocm() || dev.is_metal() {
                    a.matmul(&q_cpu.dequantize(&Device::Cpu)?.t()?)?
                } else {
                    QMatMul::from_qtensor(q_cpu)?.forward(&a)?
                };
                let q_gpu = QMatMul::from_qtensor(QTensor::quantize(&w.to_device(&dev)?, dt)?)?;
                let gpu_q = if dev.is_cuda_or_rocm() || dev.is_metal() {
                    q_gpu.forward_exact(&a.to_device(&dev)?)?
                } else {
                    q_gpu.forward(&a.to_device(&dev)?)?
                };
                let scale = cpu_q.abs()?.flatten_all()?.max(0)?.to_scalar::<f32>()?;
                let diff = max_abs_diff(&cpu_q, &gpu_q)?;
                max_ratio = max_ratio.max(diff / scale.max(1e-3));
            }
            (dense, max_ratio, 1.0f32)
        };
        #[cfg(feature = "vulkan")]
        if let Some(before) = vk_native_before {
            let after = candle_core::vulkan_backend::shaders::native_exec_count();
            assert!(
                after > before,
                "Vulkan hardware GPU must execute native GpuQuant shaders (before={before}, after={after})"
            );
            eprintln!("Vulkan native GpuQuant dispatches: {}", after - before);
        }
        release_gpu(&dev)?;
        assert!(dense < 1e-4, "softmax(matmul) 차이 {dense}");
        let quant_tol = if dev.is_cuda_or_rocm() || dev.is_metal() { 1e-4 } else { 0.05 };
        assert!(quant <= quant_tol * scale.max(1e-3), "양자화 행렬곱 차이 {quant} (크기 {scale}, 한도 {quant_tol})");
        Ok(())
    }

    #[test]
    fn accel_check_verifies_ops_and_fault_injection() -> Result<()> {
        let require_gpu = std::env::var("PD_REQUIRE_GPU")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let dev = select_device(Backend::Auto, 0)?;
        if dev.is_cpu() {
            assert!(!require_gpu, "PD_REQUIRE_GPU=1 이지만 GPU 장치를 열지 못했습니다");
        }
        reset_accel_cache();
        for op in [
            AccelOp::Matmul,
            AccelOp::Softmax,
            AccelOp::LayerNorm,
            AccelOp::Rope,
            AccelOp::FlashAttn,
            AccelOp::DecoderOps,
            AccelOp::LmHeadArgmax,
            AccelOp::GatedDeltaRule,
            AccelOp::QuantQ8,
        ] {
            assert!(accel_op_ok(&dev, op), "accel_op_ok failed for {op:?} on {}", describe(&dev));
        }
        if !dev.is_cpu() {
            // Fault injection must trip catch_unwind and cache false until explicit reset_accel_cache().
            reset_accel_cache();
            set_accel_fault(AccelOp::DecoderOps, true);
            assert!(!accel_op_ok(&dev, AccelOp::DecoderOps));
            set_accel_fault(AccelOp::DecoderOps, false);
            assert!(
                !accel_op_ok(&dev, AccelOp::DecoderOps),
                "negative verdict must stay cached after clearing fault until reset_accel_cache()"
            );
            reset_accel_cache();
            assert!(accel_op_ok(&dev, AccelOp::DecoderOps));
            release_gpu(&dev)?;
        }
        Ok(())
    }
}
