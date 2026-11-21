//! 캡처 모니터 위 창들이 개인 화면 판정에 주는 신호. 픽셀 없이 창 목록만 본다.
//!
//! raon-zena 는 DXGI Desktop Duplication 으로 모니터 하나를 통째로 찍는다. 앞 창 하나만 보면 화면을 나눠 옆에 띄운
//! 은행 · 비밀번호 관리자 창이 그대로 찍힌다. 그래서 [`crate::visible_windows_intersecting`] 이 캡처 모니터와 겹치는
//! 보이는 창을 모두 Z 순서(위부터)로 주고, [`assess_surfaces`] 가 그 목록을 본다:
//!
//! * 창이 스스로 캡처를 막았다(`WDA_MONITOR` · `WDA_EXCLUDEFROMCAPTURE`) → 화면 전체 잠금
//! * 게임 결제 오버레이 · 자격 증명 · 금융 보안 · 비밀번호 관리 프로세스의 창 → 화면 전체 잠금
//! * 창 제목에 결제 · 인증 · 시드 구문 낱말 → 화면 전체 잠금
//! * 화면 구석의 알림 토스트 · 메신저 미리보기 → 그 자리만 가림 (캡처 사각형 기준 정규화 사각형)
//!
//! 위쪽 불투명 창들에 남김없이 가린 창은 캡처에 한 픽셀도 찍히지 않으므로 판정에 치지 않는다 (가렸다고 표시만 한다).
//! 다른 모니터에만 있는 창은 목록에 오지 않는다. 이 프로세스의 창(오버레이는 스스로 `WDA_EXCLUDEFROMCAPTURE` 를 걸 수
//! 있다)은 뺀다. 창 제목은 이 자리에서 낱말만 보고 결과에 담지 않는다.
//!
//! Win32 가 필요 없다: 시험은 손으로 만든 창 목록으로 한다.

use crate::identify::VisibleWindowEntry;
use crate::layer::{WS_EX_NOACTIVATE, WS_EX_TRANSPARENT};

/// `GetWindowDisplayAffinity`: 아무것도 막지 않음
pub const WDA_NONE: u32 = 0x00;
/// 캡처에 검게 찍힌다
pub const WDA_MONITOR: u32 = 0x01;
/// 캡처에서 빠진다 (Windows 10 2004 부터)
pub const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;

/// 물리 픽셀 사각형 `[left, right) × [top, bottom)` (Win32 `RECT` 와 같은 뜻). 왼쪽 · 위 모니터에서는 음수다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PxRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl PxRect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> PxRect {
        PxRect {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(self) -> i64 {
        (self.right as i64 - self.left as i64).max(0)
    }

    pub fn height(self) -> i64 {
        (self.bottom as i64 - self.top as i64).max(0)
    }

    pub fn area(self) -> i64 {
        self.width() * self.height()
    }

    pub fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    /// 겹친 부분. 1픽셀 이상 겹칠 때만 `Some`
    pub fn intersect(self, other: PxRect) -> Option<PxRect> {
        let r = PxRect::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        );
        (!r.is_empty()).then_some(r)
    }

    pub fn intersects(self, other: PxRect) -> bool {
        self.intersect(other).is_some()
    }

    /// `other` 를 남김없이 품는가 (빈 사각형은 늘 품는다)
    pub fn contains(self, other: PxRect) -> bool {
        other.is_empty()
            || (self.left <= other.left
                && self.top <= other.top
                && other.right <= self.right
                && other.bottom <= self.bottom)
    }

    /// `frame` 기준 0..1 정규화 `(x, y, w, h)`. `frame` 밖은 잘라 낸다. 겹치지 않거나 `frame` 이 비면 `None`
    pub fn normalized_in(self, frame: PxRect) -> Option<(f32, f32, f32, f32)> {
        let r = self.intersect(frame)?;
        let (fw, fh) = (frame.width() as f64, frame.height() as f64);
        Some((
            ((r.left as i64 - frame.left as i64) as f64 / fw) as f32,
            ((r.top as i64 - frame.top as i64) as f64 / fh) as f32,
            (r.width() as f64 / fw) as f32,
            (r.height() as f64 / fh) as f32,
        ))
    }
}

impl From<(i32, i32, i32, i32)> for PxRect {
    /// `(left, top, right, bottom)`
    fn from((left, top, right, bottom): (i32, i32, i32, i32)) -> PxRect {
        PxRect::new(left, top, right, bottom)
    }
}

/// `covers` 를 합치면 `target` 을 남김없이 덮는가. 덮개 사이에 1픽셀 틈이라도 있으면 아니다 (좌표 압축).
pub fn covered_by(target: PxRect, covers: &[PxRect]) -> bool {
    if target.is_empty() {
        return true;
    }
    let clipped: Vec<PxRect> = covers.iter().filter_map(|c| c.intersect(target)).collect();
    if clipped.is_empty() {
        return false;
    }
    let axis = |lo: fn(&PxRect) -> i32, hi: fn(&PxRect) -> i32| {
        let mut v: Vec<i32> = clipped
            .iter()
            .flat_map(|c| [lo(c), hi(c)])
            .chain([lo(&target), hi(&target)])
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let xs = axis(|r| r.left, |r| r.right);
    let ys = axis(|r| r.top, |r| r.bottom);
    xs.windows(2).all(|x| {
        ys.windows(2).all(|y| {
            let cell = PxRect::new(x[0], y[0], x[1], y[1]);
            clipped.iter().any(|c| c.contains(cell))
        })
    })
}

/// 창 하나가 개인 화면 판정에 주는 신호
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceSignal {
    /// 창이 스스로 화면 캡처를 막았다 (`SetWindowDisplayAffinity`). 은행 · 비밀번호 관리자 창이 쓴다
    CaptureProtected,
    /// 게임 결제 오버레이 · 자격 증명 · 금융 보안 · 비밀번호 관리 프로세스의 창 ([`PAYMENT_PROCESS_PREFIXES`])
    PaymentOverlay,
    /// 창 제목에 결제 · 인증 · 시드 구문 낱말 ([`SENSITIVE_TITLE_WORDS`])
    SensitiveTitle,
    /// 화면 구석의 알림 토스트 · 메신저 미리보기: 그 자리만 가린다
    NotificationToast,
}

impl SurfaceSignal {
    /// 화면 전체를 잠그는 신호인가 (알림 토스트만 그 자리를 가린다)
    pub fn locks_screen(self) -> bool {
        !matches!(self, SurfaceSignal::NotificationToast)
    }
}

/// 이 실행 파일(소문자 이름의 앞부분)의 창은 결제 · 인증 화면으로 본다
pub const PAYMENT_PROCESS_PREFIXES: &[&str] = &[
    "gameoverlayui",      // Steam 오버레이 (게임 안 소액 결제 승인 창)
    "eosoverlayrenderer", // Epic Online Services 오버레이
    "credentialuibroker", // Windows 자격 증명 창
    "consent.exe",        // 사용자 계정 컨트롤
    "touchen",            // TouchEn nxKey (키보드 보안)
    "astx",               // AhnLab Safe Transaction
    "nprotect",           // nProtect Online Security
    "1password",          // 비밀번호 관리자
    "bitwarden",
    "keepass",
];

/// 창 제목(소문자)에 이 낱말이 있으면 결제 · 인증 · 시드 구문 화면으로 본다. 영숫자로만 된 낱말(`otp` · `2fa`)은
/// 낱말 단위로 맞춘다 (`hotpot` 에 걸리지 않게)
pub const SENSITIVE_TITLE_WORDS: &[&str] = &[
    "/checkout/pay",
    "/auth/",
    "2fa",
    "otp",
    "본인인증",
    "안심클릭",
    "간편비밀번호",
    "인증번호",
    "보안카드",
    "공동인증서",
    "공인인증서",
    "시드 구문",
    "seed phrase",
    "recovery phrase",
];

/// 메신저: 이 프로그램의 작은 맨 위 창이 화면 구석에 뜨면 메시지 미리보기로 보고 그 자리만 가린다
pub const MESSENGER_PROCESSES: &[&str] = &[
    "kakaotalk.exe",
    "discord.exe",
    "telegram.exe",
    "line.exe",
    "slack.exe",
    "ms-teams.exe",
    "teams.exe",
    "whatsapp.exe",
    "signal.exe",
];

/// 창 제목에 [`SENSITIVE_TITLE_WORDS`] 의 낱말이 있는가
pub fn sensitive_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    SENSITIVE_TITLE_WORDS.iter().any(|word| {
        if word.bytes().all(|b| b.is_ascii_alphanumeric()) {
            lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|t| t == *word)
        } else {
            lower.contains(word)
        }
    })
}

/// 게임 오버레이 프로세스: 게임 내내 클릭 통과 · 비활성 · 초소형(≤ 16px) 호스트 창을 띄워 둘 수 있어,
/// 실제로 클릭 · 포커스를 받거나 결제 · 인증 제목이 있는 활성 창만 결제 오버레이로 본다.
const GAME_OVERLAY_PREFIXES: &[&str] = &["gameoverlayui", "eosoverlayrenderer"];

/// 게임 오버레이가 실제 결제 · Shift+Tab 창이 아니라 게임 중 늘 떠 있는 유휴 호스트 창인가.
fn is_dormant_game_overlay(w: &VisibleWindowEntry, exe: &str, capture: PxRect) -> bool {
    if !GAME_OVERLAY_PREFIXES.iter().any(|p| exe.starts_with(p)) {
        return false;
    }
    if sensitive_title(&w.info.title) {
        return false;
    }
    let tiny = w
        .rect
        .intersect(capture)
        .is_none_or(|v| v.width() <= 16 || v.height() <= 16);
    tiny || (w.ex_style & WS_EX_TRANSPARENT != 0) || (w.ex_style & WS_EX_NOACTIVATE != 0)
}

/// 창 하나의 신호 (없으면 `None`). 캡처 보호 > 결제 프로세스 > 민감 제목 > 알림 토스트 순으로 하나만 준다
pub fn classify_window(w: &VisibleWindowEntry, capture: PxRect) -> Option<SurfaceSignal> {
    if w.capture_protected() {
        return Some(SurfaceSignal::CaptureProtected);
    }
    let exe = w
        .info
        .exe_name
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    if !exe.is_empty() && PAYMENT_PROCESS_PREFIXES.iter().any(|p| exe.starts_with(p)) {
        if !is_dormant_game_overlay(w, &exe, capture) {
            return Some(SurfaceSignal::PaymentOverlay);
        }
    }
    if sensitive_title(&w.info.title) {
        return Some(SurfaceSignal::SensitiveTitle);
    }
    is_notification_toast(w, &exe, capture).then_some(SurfaceSignal::NotificationToast)
}

/// Windows 알림 토스트(ShellExperienceHost 의 `Windows.UI.Core.CoreWindow`, 제목 "새 알림" · "New notification"),
/// 또는 메신저의 작은(캡처 넓이의 1/4 이하) 맨 위 창이 화면 구석(가로 · 세로 모두 바깥 1/3)에 있다
fn is_notification_toast(w: &VisibleWindowEntry, exe: &str, capture: PxRect) -> bool {
    let Some(visible) = w.rect.intersect(capture) else {
        return false;
    };
    let title = w.info.title.to_lowercase();
    if w.class_name == "Windows.UI.Core.CoreWindow"
        && (title.contains("알림") || title.contains("notification"))
    {
        return true;
    }
    let (cx, cy) = (
        visible.left as i64 + visible.width() / 2,
        visible.top as i64 + visible.height() / 2,
    );
    let (third_w, third_h) = (capture.width() / 3, capture.height() / 3);
    let outer_x = cx < capture.left as i64 + third_w || cx >= capture.right as i64 - third_w;
    let outer_y = cy < capture.top as i64 + third_h || cy >= capture.bottom as i64 - third_h;
    MESSENGER_PROCESSES.contains(&exe)
        && w.is_topmost()
        && visible.area() * 4 <= capture.area()
        && outer_x
        && outer_y
}

/// 신호를 준 창 하나. 창 제목은 담지 않는다
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceHit {
    pub signal: SurfaceSignal,
    pub hwnd: isize,
    pub pid: u32,
    pub exe_name: Option<String>,
    /// 캡처 사각형 안에서 그 창이 차지한 자리 (물리 픽셀)
    pub visible: PxRect,
    /// 위쪽 불투명 창들에 남김없이 가려 캡처에 한 픽셀도 찍히지 않는다. 판정에 치지 않는다
    pub occluded: bool,
}

/// 캡처 모니터 전체를 본 결과
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OsSurfaceVerdict {
    /// 신호를 준 창들, Z 순서 위부터 (가린 창도 담는다)
    pub hits: Vec<SurfaceHit>,
    /// 가릴 자리: 가리지 않은 알림 토스트, 캡처 사각형 기준 0..1 `(x, y, w, h)`
    pub redact_zones: Vec<(f32, f32, f32, f32)>,
}

impl OsSurfaceVerdict {
    /// 화면 전체를 잠글 창: 가리지 않은 잠금 신호 가운데 Z 순서 맨 위
    pub fn lockdown(&self) -> Option<&SurfaceHit> {
        self.hits
            .iter()
            .find(|h| !h.occluded && h.signal.locks_screen())
    }

    /// 화면 전체를 잠가야 하는가
    pub fn full_lockdown(&self) -> bool {
        self.lockdown().is_some()
    }

    /// 가리지 않은 캡처 보호 창이 있다 (내비게이션 개인 화면 판정의 OS 캡처 보호 입력)
    pub fn capture_protected(&self) -> bool {
        self.visible_signal(|s| s == SurfaceSignal::CaptureProtected)
    }

    /// 가리지 않은 결제 오버레이 · 민감 제목 창이 있다 (내비게이션 개인 화면 판정의 결제 오버레이 입력)
    pub fn overlay_payment(&self) -> bool {
        self.visible_signal(|s| {
            matches!(
                s,
                SurfaceSignal::PaymentOverlay | SurfaceSignal::SensitiveTitle
            )
        })
    }

    fn visible_signal(&self, want: impl Fn(SurfaceSignal) -> bool) -> bool {
        self.hits.iter().any(|h| !h.occluded && want(h.signal))
    }
}

/// 캡처 사각형과 그 위의 창들(Z 순서 위부터, [`crate::visible_windows_intersecting`] 이 주는 그대로)을 본다.
/// 이 프로세스의 창은 건너뛴다. 불투명 창([`VisibleWindowEntry::is_opaque`])은 그 밑의 창을 가리는 덮개가 된다.
pub fn assess_surfaces(windows: &[VisibleWindowEntry], capture: PxRect) -> OsSurfaceVerdict {
    let mut verdict = OsSurfaceVerdict::default();
    let mut covers: Vec<PxRect> = Vec::new();
    for w in windows {
        if w.info.is_current_process() {
            continue;
        }
        let Some(visible) = w.rect.intersect(capture) else {
            continue;
        };
        if let Some(signal) = classify_window(w, capture) {
            let occluded = covered_by(visible, &covers);
            if signal == SurfaceSignal::NotificationToast && !occluded {
                verdict.redact_zones.extend(visible.normalized_in(capture));
            }
            verdict.hits.push(SurfaceHit {
                signal,
                hwnd: w.info.hwnd,
                pid: w.info.pid,
                exe_name: w.info.exe_name.clone(),
                visible,
                occluded,
            });
        }
        if w.is_opaque() {
            covers.push(visible);
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identify::WindowInfo;
    use crate::layer::{WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT};
    use std::path::PathBuf;

    const MONITOR: PxRect = PxRect::new(0, 0, 1920, 1080);

    fn win(pid: u32, exe: &str, title: &str, rect: PxRect) -> VisibleWindowEntry {
        VisibleWindowEntry {
            info: WindowInfo {
                hwnd: pid as isize * 0x10,
                pid,
                exe_path: Some(PathBuf::from(format!("C:/Program Files/X/{exe}"))),
                exe_name: Some(exe.to_string()),
                title: title.to_string(),
            },
            class_name: "AppWindow".into(),
            rect,
            ex_style: 0,
            display_affinity: WDA_NONE,
        }
    }

    fn game() -> VisibleWindowEntry {
        win(100, "BossRush-Win64-Shipping.exe", "Boss Rush", MONITOR)
    }

    #[test]
    fn ps03_overlay_and_split_screen_protected_window_force_lockdown() {
        // A: 전체 화면 게임 위에 Steam 소액 결제 오버레이 (게임 UIA 는 민감 속성이 없다)
        let overlay = VisibleWindowEntry {
            ex_style: WS_EX_TOPMOST,
            ..win(
                200,
                "GameOverlayUI.exe",
                "Steam",
                PxRect::new(560, 240, 1360, 840),
            )
        };
        let v = assess_surfaces(&[overlay.clone(), game()], MONITOR);
        assert!(v.full_lockdown());
        let hit = v.lockdown().unwrap();
        assert_eq!(
            (hit.signal, hit.exe_name.as_deref()),
            (SurfaceSignal::PaymentOverlay, Some("GameOverlayUI.exe"))
        );
        assert!(v.overlay_payment() && !v.capture_protected());
        assert!(
            !assess_surfaces(&[game()], MONITOR).full_lockdown(),
            "게임만이면 깨끗하다"
        );
        // B: 게임 내내 떠 있는 Steam 유휴 호스트 창(클릭 통과 / 비활성 / 초소형)은 잠그지 않는다
        let dormant_transparent = VisibleWindowEntry {
            ex_style: WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            ..overlay.clone()
        };
        assert!(!assess_surfaces(&[dormant_transparent, game()], MONITOR).full_lockdown());
        let dormant_noactivate = VisibleWindowEntry {
            ex_style: WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            ..overlay.clone()
        };
        assert!(!assess_surfaces(&[dormant_noactivate, game()], MONITOR).full_lockdown());
        let dormant_tiny = VisibleWindowEntry {
            rect: PxRect::new(0, 0, 1, 1),
            ..overlay.clone()
        };
        assert!(!assess_surfaces(&[dormant_tiny, game()], MONITOR).full_lockdown());
        // 비활성이더라도 결제 · 인증 제목이 있으면 잠근다
        let titled_checkout = VisibleWindowEntry {
            ex_style: WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            info: WindowInfo {
                title: "Steam /checkout/pay".into(),
                ..overlay.info.clone()
            },
            ..overlay.clone()
        };
        assert!(assess_surfaces(&[titled_checkout, game()], MONITOR).full_lockdown());

        // C: 화면을 나눠 왼쪽은 창 모드 게임(앞 창), 오른쪽 절반은 WDA_EXCLUDEFROMCAPTURE 은행 창
        let left_game = win(
            100,
            "BossRush-Win64-Shipping.exe",
            "Boss Rush",
            PxRect::new(0, 0, 960, 1080),
        );
        let bank = VisibleWindowEntry {
            display_affinity: WDA_EXCLUDEFROMCAPTURE,
            ..win(
                300,
                "BankClient.exe",
                "인터넷뱅킹",
                PxRect::new(960, 0, 1920, 1080),
            )
        };
        let v = assess_surfaces(&[left_game, bank], MONITOR);
        assert!(v.full_lockdown() && v.capture_protected());
        let hit = v.lockdown().unwrap();
        assert_eq!(
            (hit.signal, hit.pid, hit.occluded),
            (SurfaceSignal::CaptureProtected, 300, false)
        );
        assert_eq!(hit.visible, PxRect::new(960, 0, 1920, 1080));
        // WDA_MONITOR(캡처에 검게 찍힘)도 같다. 전체 화면 게임 밑에 가리면 치지 않는다
        let vault = VisibleWindowEntry {
            display_affinity: WDA_MONITOR,
            ..win(301, "Vault.exe", "", PxRect::new(1500, 100, 1900, 600))
        };
        assert!(assess_surfaces(&[vault.clone(), game()], MONITOR).capture_protected());
        assert!(!assess_surfaces(&[game(), vault], MONITOR).capture_protected());
    }

    #[test]
    fn own_windows_and_windows_on_other_monitors_do_not_lock() {
        // 이 프로세스의 오버레이는 스스로 캡처에서 빠질 수 있다: 신호로 치지 않는다
        let me = VisibleWindowEntry {
            display_affinity: WDA_EXCLUDEFROMCAPTURE,
            ex_style: WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            ..win(
                std::process::id(),
                "raon-zena.exe",
                "Raon Zena",
                PxRect::new(1500, 600, 1850, 1050),
            )
        };
        assert_eq!(
            assess_surfaces(&[me, game()], MONITOR),
            OsSurfaceVerdict::default()
        );
        // 보조 모니터(오른쪽)에만 있는 은행 창
        let elsewhere = VisibleWindowEntry {
            display_affinity: WDA_EXCLUDEFROMCAPTURE,
            ..win(300, "BankClient.exe", "", PxRect::new(1920, 0, 3840, 1080))
        };
        assert!(assess_surfaces(&[elsewhere.clone(), game()], MONITOR)
            .hits
            .is_empty());
        // 1픽셀만 걸쳐도 친다
        let edge = VisibleWindowEntry {
            rect: PxRect::new(1919, 0, 3840, 1080),
            ..elsewhere
        };
        assert!(assess_surfaces(&[edge, game()], MONITOR).full_lockdown());
    }

    #[test]
    fn a_window_fully_behind_opaque_windows_is_occluded_but_one_pixel_is_enough() {
        let bank = VisibleWindowEntry {
            display_affinity: WDA_EXCLUDEFROMCAPTURE,
            ..win(
                300,
                "BankClient.exe",
                "인터넷뱅킹",
                PxRect::new(400, 200, 1400, 900),
            )
        };
        // 전체 화면 불투명 게임 뒤: 캡처에 한 픽셀도 없다 (Alt-Tab 복귀)
        let v = assess_surfaces(&[game(), bank.clone()], MONITOR);
        assert_eq!(v.hits.len(), 1);
        assert!(v.hits[0].occluded && !v.full_lockdown());
        // 두 창이 이어서 덮어도 가린 것이다
        let left = win(101, "a.exe", "", PxRect::new(0, 0, 900, 1080));
        let right = win(102, "b.exe", "", PxRect::new(900, 0, 1920, 1080));
        assert!(!assess_surfaces(&[left.clone(), right, bank.clone()], MONITOR).full_lockdown());
        // 창 모드 게임 옆으로 1픽셀이라도 나오면 잠근다
        let windowed = win(
            100,
            "BossRush-Win64-Shipping.exe",
            "Boss Rush",
            PxRect::new(0, 0, 1399, 1080),
        );
        assert!(assess_surfaces(&[windowed, bank.clone()], MONITOR).full_lockdown());
        // 반투명 층 창 · 클릭 통과 창은 밑을 가리지 않는다
        let hud = VisibleWindowEntry {
            ex_style: WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST,
            ..win(103, "overlay-hud.exe", "", MONITOR)
        };
        assert!(assess_surfaces(&[hud, bank], MONITOR).full_lockdown());
        // 덮개 틈 계산
        let target = PxRect::new(0, 0, 10, 10);
        assert!(covered_by(
            target,
            &[PxRect::new(0, 0, 10, 5), PxRect::new(-5, 5, 20, 20)]
        ));
        assert!(!covered_by(
            target,
            &[PxRect::new(0, 0, 10, 5), PxRect::new(0, 6, 10, 10)]
        ));
        assert!(!covered_by(target, &[]));
        assert!(covered_by(PxRect::default(), &[]));
    }

    #[test]
    fn toasts_are_redacted_in_place_and_titles_match_whole_words() {
        // Windows 알림 토스트: 오른쪽 아래 그 자리만 가린다
        let toast = VisibleWindowEntry {
            class_name: "Windows.UI.Core.CoreWindow".into(),
            ex_style: WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            ..win(
                400,
                "ShellExperienceHost.exe",
                "새 알림",
                PxRect::new(1536, 864, 1920, 1080),
            )
        };
        let v = assess_surfaces(&[toast, game()], MONITOR);
        assert!(!v.full_lockdown());
        assert_eq!(v.hits[0].signal, SurfaceSignal::NotificationToast);
        assert_eq!(v.redact_zones, vec![(0.8, 0.8, 0.2, 0.2)]);
        // 메신저 미리보기 (작고 맨 위, 구석)
        let kakao = VisibleWindowEntry {
            ex_style: WS_EX_TOPMOST,
            ..win(401, "KakaoTalk.exe", "", PxRect::new(1600, 900, 1900, 1060))
        };
        assert_eq!(
            classify_window(&kakao, MONITOR),
            Some(SurfaceSignal::NotificationToast)
        );
        // 같은 메신저라도 가운데의 큰 대화창은 토스트가 아니다
        let chat = win(401, "KakaoTalk.exe", "", PxRect::new(400, 100, 1500, 1000));
        assert_eq!(classify_window(&chat, MONITOR), None);
        // 민감 제목: 낱말 단위
        for t in [
            "OTP 인증 - 은행",
            "Google 2FA 설정",
            "본인인증 | 쇼핑몰",
            "https://shop.example/checkout/pay?step=2",
            "Enter your seed phrase",
        ] {
            assert!(sensitive_title(t), "{t}");
        }
        for t in ["Hotpot recipe", "보스 공략", "Stop the timer", ""] {
            assert!(!sensitive_title(t), "{t}");
        }
        let auth = win(
            500,
            "chrome.exe",
            "본인인증 - Chrome",
            PxRect::new(0, 0, 960, 1080),
        );
        assert_eq!(
            assess_surfaces(&[auth], MONITOR)
                .lockdown()
                .map(|h| h.signal),
            Some(SurfaceSignal::SensitiveTitle)
        );
    }

    #[test]
    fn rects_intersect_contain_and_normalize() {
        let a = PxRect::new(0, 0, 100, 50);
        assert_eq!(
            a.intersect(PxRect::new(50, 25, 150, 75)),
            Some(PxRect::new(50, 25, 100, 50))
        );
        assert_eq!(
            a.intersect(PxRect::new(100, 0, 200, 50)),
            None,
            "맞닿기만 하면 겹치지 않는다"
        );
        assert!(
            a.contains(PxRect::new(10, 10, 20, 20)) && !a.contains(PxRect::new(90, 0, 110, 10))
        );
        assert_eq!(PxRect::from((1, 2, 3, 4)), PxRect::new(1, 2, 3, 4));
        assert_eq!((a.width(), a.height(), a.area()), (100, 50, 5000));
        assert_eq!(
            PxRect::new(i32::MIN, 0, i32::MAX, 1).width(),
            u32::MAX as i64
        );
        // 왼쪽 모니터(음수 좌표)도 같다
        let left_monitor = PxRect::new(-1920, 0, 0, 1080);
        assert_eq!(
            PxRect::new(-960, 540, 0, 1080).normalized_in(left_monitor),
            Some((0.5, 0.5, 0.5, 0.5))
        );
        assert_eq!(a.normalized_in(PxRect::new(500, 500, 600, 600)), None);
    }
}
