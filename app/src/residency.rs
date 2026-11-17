//! 비전·오디오 타워의 상주 관리 (지연 로드, 실측 비용, 메모리 예산, 타워 간 전환).
//!
//! logis-center(`commerce/src-tauri/src/model/residency.rs`)의 생성 모델 ↔ 임베딩 모델 VRAM 교차 상주를
//! 이 앱의 CPU 메모리(모바일 포함)에 맞게 옮겼다. 그대로 가져온 원칙:
//! - **상태는 슬롯에서만 유도한다.** 따로 적어 두는 원장이 없어 사실과 어긋날 일이 없다.
//! - **비용은 실측을 쓴다.** 로드 전후의 상주 메모리(VmRSS) 차이. 실측이 없으면 디스크 크기로 추정하는데,
//!   가중치를 F32로 올리므로 BF16 크기의 2배로 잡는다 (과대평가 쪽이라 안전).
//! - **예산이 모자라면 다른 타워를 내리고(전환), 그래도 모자라면 거절한다.** 메모리 부족으로 앱이 죽는 것보다 오류가 낫다.
//! - **계산 중인 타워는 내려가지 않는다.** 슬롯에서 꺼낸 `Arc`를 계산이 끝날 때까지 쥐므로, 슬롯을 비워도
//!   메모리는 계산이 끝난 뒤에 풀린다 (logis-center의 `SlotLease`와 같은 목적: 빈 슬롯을 보고 틀린 값을 내지 않는다).
//!
//! 예산·측정 규칙은 메서드라 실제 모델 없이 시험한다 (아래 tests의 가짜 메모리).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// 어떤 입력을 처리할지. 설정(`modalities`)으로 고른다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modalities {
    /// 텍스트는 늘 쓰고, 이미지·오디오 타워는 그 입력이 처음 올 때 올린다 (파일에 있을 때만)
    #[default]
    Auto,
    /// 텍스트만. 이미지·오디오 타워는 올리지 않고, 그런 입력은 오류로 거절한다
    Text,
}

impl Modalities {
    /// 설정 문자열을 읽는다. 빈 값은 기본값(auto)이다.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "all" => Ok(Self::Auto),
            "text" | "text-only" | "text_only" => Ok(Self::Text),
            other => Err(format!("modalities 값 '{other}'를 알 수 없습니다 (auto 또는 text)")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tower {
    Vision,
    Audio,
}

impl Tower {
    fn index(self) -> usize {
        match self {
            Self::Vision => 0,
            Self::Audio => 1,
        }
    }

    fn other(self) -> Self {
        match self {
            Self::Vision => Self::Audio,
            Self::Audio => Self::Vision,
        }
    }
}

/// 타워 하나의 지금 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TowerState {
    /// 설정으로 껐다 (modalities = text)
    Disabled,
    /// 설치된 모델 파일에 이 타워가 없다 (텍스트 전용으로 내려받음)
    NotInstalled,
    /// 쓸 수 있지만 아직 올리지 않았다 (또는 내렸다)
    Standby,
    /// 메모리에 올라와 있다
    Resident,
}

/// 타워를 쓸 수 없는 이유
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TowerError {
    Disabled(Tower),
    NotInstalled(Tower),
    /// 내려야 할 것을 내려도 메모리가 모자라다
    Memory { tower: Tower, need_mb: u64, available_mb: u64 },
    Load(Tower, String),
}

impl std::fmt::Display for TowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = |t: &Tower| match t {
            Tower::Vision => "이미지",
            Tower::Audio => "오디오",
        };
        match self {
            Self::Disabled(t) => write!(f, "텍스트 전용으로 설정되어 있어 {} 입력을 처리하지 않습니다 (설정 modalities=text)", name(t)),
            Self::NotInstalled(t) => write!(
                f,
                "설치된 모델에 {} 인코더가 없습니다 (텍스트 전용으로 내려받음). 전체 모델을 내려받으면 쓸 수 있습니다",
                name(t)
            ),
            Self::Memory { tower, need_mb, available_mb } => write!(
                f,
                "메모리가 부족해 {} 인코더를 올리지 못했습니다 (필요 약 {need_mb}MB, 사용 가능 {available_mb}MB)",
                name(tower)
            ),
            Self::Load(t, e) => write!(f, "{} 인코더를 올리지 못했습니다: {e}", name(t)),
        }
    }
}

/// 프로세스 메모리를 읽는다. 시험에서는 가짜를 쓴다.
pub trait MemoryProbe: Send + Sync {
    /// 이 프로세스가 지금 차지한 물리 메모리 (바이트)
    fn resident_bytes(&self) -> Option<u64>;
    /// 시스템이 더 내줄 수 있는 메모리 (바이트)
    fn available_bytes(&self) -> Option<u64>;
}

/// Linux·Android의 /proc를 읽는다. 그 밖의 운영체제에서는 모르므로(None) 예산 판정을 건너뛰고 실측도 하지 않는다.
/// 페이지 크기(Android는 16KB 기기도 있다)에 기대지 않도록 kB 단위 항목을 읽는다.
pub struct SystemMemory;

impl SystemMemory {
    fn kb_field(path: &str, key: &str) -> Option<u64> {
        let text = std::fs::read_to_string(path).ok()?;
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .map(|kb| kb * 1024)
    }
}

impl MemoryProbe for SystemMemory {
    fn resident_bytes(&self) -> Option<u64> {
        Self::kb_field("/proc/self/status", "VmRSS:")
    }

    fn available_bytes(&self) -> Option<u64> {
        Self::kb_field("/proc/meminfo", "MemAvailable:")
    }
}

const MB: u64 = 1024 * 1024;

/// 실측 비용과 전환 통계. 값은 원자값이라 잠금 없이 읽는다.
#[derive(Default)]
pub struct CostBook {
    measured: [AtomicU64; 2],
    loads: AtomicU64,
    swaps: AtomicU64,
    refusals: AtomicU64,
}

impl CostBook {
    pub fn measured_bytes(&self, tower: Tower) -> Option<u64> {
        match self.measured[tower.index()].load(Ordering::SeqCst) {
            0 => None,
            v => Some(v),
        }
    }

    /// 로드 전후 상주 메모리로 비용을 기록한다. 줄었거나 모르면 기록하지 않는다 (다른 정리와 겹친 측정은 믿지 않는다).
    fn record_load(&self, tower: Tower, before: Option<u64>, after: Option<u64>) -> Option<u64> {
        let (Some(before), Some(after)) = (before, after) else { return None };
        let cost = after.checked_sub(before).filter(|&c| c > 0)?;
        self.measured[tower.index()].store(cost, Ordering::SeqCst);
        Some(cost)
    }
}

/// 상주 상태 보고 (진단·화면용)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResidencyReport {
    pub modalities: Modalities,
    pub vision: TowerReport,
    pub audio: TowerReport,
    /// 타워를 올린 횟수, 예산 때문에 다른 타워를 내린 횟수, 메모리 부족으로 거절한 횟수
    pub loads: u64,
    pub swaps: u64,
    pub refusals: u64,
    /// 지금 프로세스 상주 메모리 (MB). 모르는 운영체제는 None
    pub resident_mb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TowerReport {
    pub state: TowerState,
    /// 실측 비용 (MB). 아직 올려 본 적이 없으면 None
    pub measured_mb: Option<u64>,
    /// 예산 판정에 쓰는 비용 (MB): 실측, 없으면 디스크 추정
    pub budget_mb: u64,
}

/// 두 타워의 슬롯과 예산 규칙. `V`, `A`는 비전·오디오 타워 타입 (시험에서는 가짜).
pub struct Towers<V, A> {
    modalities: Modalities,
    installed: [bool; 2],
    /// 파일에 있는 타워 가중치 크기 (바이트, 파일의 자료형 그대로)
    disk_bytes: [u64; 2],
    /// 파일 자료형에서 메모리 자료형으로 늘어나는 배수 (BF16 → F32 = 2)
    expand: u64,
    /// 계산에 쓸 여유 (activation)
    reserve_bytes: u64,
    vision: Mutex<Option<Arc<V>>>,
    audio: Mutex<Option<Arc<A>>>,
    /// 올리기·내리기 결정을 한 번에 하나씩 (슬롯 잠금은 짧게만 쥔다: 서로 겹쳐 잡지 않아 교착이 없다)
    deciding: Mutex<()>,
    book: CostBook,
    probe: Arc<dyn MemoryProbe>,
}

/// 계산에 쓸 여유. 타워 계산 중간값(280 토큰 x 768, 오디오 프레임 등)은 수십 MB라 넉넉히 잡는다.
pub const DEFAULT_RESERVE_BYTES: u64 = 256 * MB;

impl<V, A> Towers<V, A> {
    pub fn new(modalities: Modalities, installed: [bool; 2], disk_bytes: [u64; 2], probe: Arc<dyn MemoryProbe>) -> Self {
        Self {
            modalities,
            installed,
            disk_bytes,
            expand: 2,
            reserve_bytes: DEFAULT_RESERVE_BYTES,
            vision: Mutex::new(None),
            audio: Mutex::new(None),
            deciding: Mutex::new(()),
            book: CostBook::default(),
            probe,
        }
    }

    pub fn with_reserve(mut self, bytes: u64) -> Self {
        self.reserve_bytes = bytes;
        self
    }

    pub fn state(&self, tower: Tower) -> TowerState {
        if self.modalities == Modalities::Text {
            TowerState::Disabled
        } else if !self.installed[tower.index()] {
            TowerState::NotInstalled
        } else if self.is_resident(tower) {
            TowerState::Resident
        } else {
            TowerState::Standby
        }
    }

    fn is_resident(&self, tower: Tower) -> bool {
        match tower {
            Tower::Vision => self.vision.lock().expect("vision slot").is_some(),
            Tower::Audio => self.audio.lock().expect("audio slot").is_some(),
        }
    }

    /// 예산 판정에 쓰는 비용: 실측, 없으면 디스크 크기 x 확장 배수
    pub fn budget_bytes(&self, tower: Tower) -> u64 {
        self.book.measured_bytes(tower).unwrap_or(self.disk_bytes[tower.index()] * self.expand)
    }

    /// 비전 타워를 돌려준다. 없으면 `load`로 올린다.
    pub fn vision(&self, load: impl FnOnce() -> Result<V, String>) -> Result<Arc<V>, TowerError> {
        if let Some(t) = self.vision.lock().expect("vision slot").clone() {
            return Ok(t);
        }
        self.acquire(Tower::Vision, load, |s| &s.vision)
    }

    /// 오디오 타워를 돌려준다. 없으면 `load`로 올린다.
    pub fn audio(&self, load: impl FnOnce() -> Result<A, String>) -> Result<Arc<A>, TowerError> {
        if let Some(t) = self.audio.lock().expect("audio slot").clone() {
            return Ok(t);
        }
        self.acquire(Tower::Audio, load, |s| &s.audio)
    }

    fn acquire<T>(
        &self,
        tower: Tower,
        load: impl FnOnce() -> Result<T, String>,
        slot: impl Fn(&Self) -> &Mutex<Option<Arc<T>>>,
    ) -> Result<Arc<T>, TowerError> {
        match self.state(tower) {
            TowerState::Disabled => return Err(TowerError::Disabled(tower)),
            TowerState::NotInstalled => return Err(TowerError::NotInstalled(tower)),
            _ => {}
        }
        let _deciding = self.deciding.lock().expect("deciding");
        // 기다리는 동안 다른 요청이 올렸을 수 있다
        if let Some(t) = slot(self).lock().expect("slot").clone() {
            return Ok(t);
        }

        let need = self.budget_bytes(tower) + self.reserve_bytes;
        if let Some(mut available) = self.probe.available_bytes() {
            if available < need && self.is_resident(tower.other()) {
                // 전환: 다른 타워를 내린다. 계산 중이면 그 계산이 끝날 때 메모리가 풀린다
                self.release(tower.other());
                self.book.swaps.fetch_add(1, Ordering::SeqCst);
                available = self.probe.available_bytes().unwrap_or(available);
            }
            if available < need {
                self.book.refusals.fetch_add(1, Ordering::SeqCst);
                return Err(TowerError::Memory { tower, need_mb: need.div_ceil(MB), available_mb: available / MB });
            }
        }

        let before = self.probe.resident_bytes();
        let value = load().map_err(|e| TowerError::Load(tower, e))?;
        let after = self.probe.resident_bytes();
        self.book.record_load(tower, before, after);
        self.book.loads.fetch_add(1, Ordering::SeqCst);
        let value = Arc::new(value);
        *slot(self).lock().expect("slot") = Some(value.clone());
        Ok(value)
    }

    /// 타워 하나를 내린다. 계산 중이면 그 계산이 끝날 때 메모리가 풀린다.
    pub fn release(&self, tower: Tower) {
        match tower {
            Tower::Vision => drop(self.vision.lock().expect("vision slot").take()),
            Tower::Audio => drop(self.audio.lock().expect("audio slot").take()),
        }
    }

    pub fn release_all(&self) {
        self.release(Tower::Vision);
        self.release(Tower::Audio);
    }

    pub fn report(&self) -> ResidencyReport {
        let tower = |t: Tower| TowerReport {
            state: self.state(t),
            measured_mb: self.book.measured_bytes(t).map(|b| b.div_ceil(MB)),
            budget_mb: self.budget_bytes(t).div_ceil(MB),
        };
        ResidencyReport {
            modalities: self.modalities,
            vision: tower(Tower::Vision),
            audio: tower(Tower::Audio),
            loads: self.book.loads.load(Ordering::SeqCst),
            swaps: self.book.swaps.load(Ordering::SeqCst),
            refusals: self.book.refusals.load(Ordering::SeqCst),
            resident_mb: self.probe.resident_bytes().map(|b| b / MB),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    /// 가짜 메모리: 타워를 올리면 그만큼 상주가 늘고 가용이 준다. 타워(가짜)가 버려지면 되돌린다.
    struct FakeMemory {
        resident: AtomicI64,
        available: AtomicI64,
    }

    impl FakeMemory {
        fn new(available_mb: i64) -> Arc<Self> {
            Arc::new(Self { resident: AtomicI64::new(500 * MB as i64), available: AtomicI64::new(available_mb * MB as i64) })
        }
        fn take(&self, bytes: i64) {
            self.resident.fetch_add(bytes, Ordering::SeqCst);
            self.available.fetch_sub(bytes, Ordering::SeqCst);
        }
    }

    impl MemoryProbe for FakeMemory {
        fn resident_bytes(&self) -> Option<u64> {
            Some(self.resident.load(Ordering::SeqCst) as u64)
        }
        fn available_bytes(&self) -> Option<u64> {
            Some(self.available.load(Ordering::SeqCst).max(0) as u64)
        }
    }

    /// 버려질 때 메모리를 돌려주는 가짜 타워
    struct FakeTower {
        bytes: i64,
        mem: Arc<FakeMemory>,
    }

    impl Drop for FakeTower {
        fn drop(&mut self) {
            self.mem.take(-self.bytes);
        }
    }

    fn tower(mem: &Arc<FakeMemory>, mb: i64) -> impl FnOnce() -> Result<FakeTower, String> {
        let mem = mem.clone();
        move || {
            mem.take(mb * MB as i64);
            Ok(FakeTower { bytes: mb * MB as i64, mem })
        }
    }

    fn towers(mem: &Arc<FakeMemory>, modalities: Modalities, installed: [bool; 2]) -> Towers<FakeTower, FakeTower> {
        // 디스크(BF16) 비전 335MB, 오디오 610MB → 추정은 2배
        Towers::new(modalities, installed, [335 * MB, 610 * MB], mem.clone()).with_reserve(100 * MB)
    }

    #[test]
    fn towers_load_lazily_once_and_the_cost_is_measured() {
        let mem = FakeMemory::new(4000);
        let t = towers(&mem, Modalities::Auto, [true, true]);
        assert_eq!(t.state(Tower::Vision), TowerState::Standby);
        assert_eq!(t.report().vision.budget_mb, 670, "실측 전에는 디스크 x 2");

        let a = t.vision(tower(&mem, 640)).unwrap();
        let b = t.vision(|| panic!("두 번째는 올리지 않는다")).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(t.state(Tower::Vision), TowerState::Resident);
        let report = t.report();
        assert_eq!(report.vision.measured_mb, Some(640));
        assert_eq!(report.vision.budget_mb, 640, "실측이 생기면 실측으로 판정");
        assert_eq!(report.loads, 1);
        assert_eq!(report.audio.state, TowerState::Standby);
    }

    #[test]
    fn text_only_and_missing_towers_are_refused_without_loading() {
        let mem = FakeMemory::new(4000);
        let text = towers(&mem, Modalities::Text, [true, true]);
        assert_eq!(text.vision(|| panic!("올리면 안 된다")).err(), Some(TowerError::Disabled(Tower::Vision)));
        assert_eq!(text.state(Tower::Audio), TowerState::Disabled);

        let text_file = towers(&mem, Modalities::Auto, [false, false]);
        let err = text_file.audio(|| panic!("올리면 안 된다")).err().unwrap();
        assert_eq!(err, TowerError::NotInstalled(Tower::Audio));
        assert!(err.to_string().contains("전체 모델을 내려받으면"));
        assert_eq!(text_file.report().loads, 0);
    }

    #[test]
    fn when_memory_is_short_the_other_tower_is_swapped_out() {
        // 가용 1800MB: 비전(670 + 여유 100)을 올린 뒤 남는 1130MB로는 오디오(1220 + 100)가 안 들어간다
        let mem = FakeMemory::new(1800);
        let t = towers(&mem, Modalities::Auto, [true, true]);
        t.vision(tower(&mem, 670)).unwrap();
        let audio = t.audio(tower(&mem, 1220)).unwrap();
        assert_eq!(t.state(Tower::Vision), TowerState::Standby, "비전을 내리고 오디오를 올렸다");
        assert_eq!(t.state(Tower::Audio), TowerState::Resident);
        assert_eq!(t.report().swaps, 1);
        drop(audio);

        // 다시 비전이 필요하면 이번에는 오디오를 내린다 (교차 전환)
        t.vision(tower(&mem, 670)).unwrap();
        assert_eq!(t.state(Tower::Audio), TowerState::Standby);
        assert_eq!(t.report().swaps, 2);
    }

    #[test]
    fn when_even_swapping_is_not_enough_the_request_is_refused_not_loaded() {
        let mem = FakeMemory::new(600);
        let t = towers(&mem, Modalities::Auto, [true, true]);
        let err = t.vision(|| panic!("메모리가 모자라면 올리지 않는다")).err().unwrap();
        assert!(matches!(err, TowerError::Memory { tower: Tower::Vision, need_mb: 770, available_mb: 600 }), "{err:?}");
        assert!(err.to_string().contains("메모리가 부족해"));
        assert_eq!(t.report().refusals, 1);
        assert_eq!(t.state(Tower::Vision), TowerState::Standby);
    }

    #[test]
    fn a_tower_in_use_keeps_its_memory_until_the_computation_ends() {
        let mem = FakeMemory::new(4000);
        let t = towers(&mem, Modalities::Auto, [true, true]);
        let in_use = t.vision(tower(&mem, 670)).unwrap();
        let resident = mem.resident_bytes().unwrap();
        t.release_all();
        assert_eq!(t.state(Tower::Vision), TowerState::Standby, "슬롯은 비었다");
        assert_eq!(mem.resident_bytes().unwrap(), resident, "계산 중이라 메모리는 아직 그대로");
        drop(in_use);
        assert_eq!(mem.resident_bytes().unwrap(), resident - 670 * MB, "계산이 끝나면 풀린다");
    }

    #[test]
    fn a_failed_load_leaves_the_slot_empty_and_is_reported() {
        let mem = FakeMemory::new(4000);
        let t = towers(&mem, Modalities::Auto, [true, true]);
        let err = t.audio(|| Err::<FakeTower, _>("파일이 잘렸습니다".into())).err().unwrap();
        assert!(err.to_string().contains("파일이 잘렸습니다"));
        assert_eq!(t.state(Tower::Audio), TowerState::Standby);
        assert!(t.audio(tower(&mem, 1220)).is_ok(), "다음 요청은 다시 시도한다");
    }

    #[test]
    fn modalities_setting_parses_and_rejects_unknown_values() {
        assert_eq!(Modalities::parse("").unwrap(), Modalities::Auto);
        assert_eq!(Modalities::parse(" TEXT ").unwrap(), Modalities::Text);
        assert_eq!(Modalities::parse("text-only").unwrap(), Modalities::Text);
        assert!(Modalities::parse("vision").is_err());
    }

    #[test]
    fn the_system_probe_reads_proc_on_linux() {
        if cfg!(target_os = "linux") {
            let p = SystemMemory;
            assert!(p.resident_bytes().unwrap() > 0);
            assert!(p.available_bytes().unwrap() > 0);
        }
    }
}
