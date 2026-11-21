//! 점 아래 창과 앞 창 알아내기: 창 핸들 → 프로세스 번호 → 실행 파일 경로 · 이름, 그리고 창 제목.
//!
//! * [`window_at`] – `WindowFromPoint` 로 그 점의 창을 찾고 `GetAncestor(GA_ROOT)` 로 최상위 창까지 올라간다.
//!   드롭한 곳이 어느 프로그램인지 알 때 쓴다. 좌표는 화면의 물리 픽셀이다.
//! * [`foreground_window`] – `GetForegroundWindow`. 지금 사용자가 쓰고 있는 프로그램.
//! * [`exclude_self`] – 찾은 창이 이 프로세스 것이면 `None`. 오버레이가 자기를 알아보는 일을 막는다.
//! * [`visible_windows_intersecting`] – `EnumWindows` 로 캡처 모니터와 겹치는 보이는 최상위 창을 모두 Z 순서(위부터)로.
//!   창 자리(DWM 확장 프레임) · 확장 스타일 · 캡처 보호(`GetWindowDisplayAffinity`)를 함께 준다. 개인 화면 판정은
//!   [`crate::surface::assess_surfaces`] 가 이 목록으로 한다. 픽셀은 하나도 읽지 않는다.
//!
//! 실행 파일은 `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW` 로 읽는다.
//! 보호 프로세스처럼 열 수 없으면 경로 · 이름만 비고 나머지는 채운다. 어떤 실패에도 패닉하지 않는다.
//!
//! 제목(`GetWindowTextW`)은 다른 프로세스의 창이면 메시지 없이 읽는다. 이 프로세스의 창이면 그 창의 UI 스레드로
//! `WM_GETTEXT` 가 가므로, 그 스레드가 멈춰 있는 동안에는 기다린다 ([`visible_windows_intersecting`] 은 이 프로세스의
//! 창을 제목을 읽기 전에 뺀다).
//!
//! Windows 가 아니거나 `win32` 기능을 끄면 찾는 함수는 늘 `None` (목록은 빈 목록)이다.
//!
//! 찾은 창을 평가 대상(`cc`)으로 쓰려면 [`WindowInfo::app_uri`] 를 premium-discount-schema 의
//! `envelope::normalize_cc` 에 넘긴다 (`app://code - insiders.exe` → `code-insiders.exe`).

use crate::layer::{WS_EX_LAYERED, WS_EX_TOPMOST, WS_EX_TRANSPARENT};
use crate::surface::{PxRect, WDA_NONE};
use std::path::PathBuf;

/// 찾은 최상위 창.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// 창 핸들 (`HWND` 를 정수로). 창이 닫히면 다른 창이 같은 값을 받을 수 있으니 오래 들고 있지 않는다.
    pub hwnd: isize,
    /// 창을 만든 프로세스 번호.
    pub pid: u32,
    /// 실행 파일의 전체 경로 (`C:\Program Files\...\Code.exe`). 프로세스를 열 수 없으면 `None`.
    pub exe_path: Option<PathBuf>,
    /// 실행 파일 이름, 대소문자 그대로 (`Code.exe`).
    pub exe_name: Option<String>,
    /// 창 제목. 없으면 빈 문자열.
    pub title: String,
}

impl WindowInfo {
    /// `app://<실행 파일 이름, 소문자>` (`app://code.exe`). 이름을 모르면 `None`.
    /// premium-discount-schema 의 `envelope::normalize_cc` 에 넣으면 서버의 `cc` 가 된다.
    pub fn app_uri(&self) -> Option<String> {
        let name = self.exe_name.as_deref()?.trim();
        if name.is_empty() {
            return None;
        }
        Some(format!("app://{}", name.to_lowercase()))
    }

    /// 이 프로세스(부른 쪽)의 창인가.
    pub fn is_current_process(&self) -> bool {
        self.pid == std::process::id()
    }
}

/// 찾은 창이 이 프로세스 것이면 `None`, 아니면 그대로. `exclude_self(window_at(x, y))` 처럼 쓴다.
pub fn exclude_self(info: Option<WindowInfo>) -> Option<WindowInfo> {
    info.filter(|w| !w.is_current_process())
}

/// 화면의 점(물리 픽셀) 아래에 있는 최상위 창. 클릭 통과(`WS_EX_TRANSPARENT`) 창은 `WindowFromPoint` 가 건너뛴다.
pub fn window_at(x: i32, y: i32) -> Option<WindowInfo> {
    imp::window_at(x, y)
}

/// 지금 앞에 있는(키보드 입력을 받는) 창. 잠긴 화면이나 창이 바뀌는 사이에는 `None` 일 수 있다.
pub fn foreground_window() -> Option<WindowInfo> {
    imp::foreground_window()
}

/// 캡처 모니터와 겹치는 보이는 최상위 창 하나 ([`visible_windows_intersecting`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleWindowEntry {
    pub info: WindowInfo,
    /// 창 클래스 이름 (`Chrome_WidgetWin_1`, `Windows.UI.Core.CoreWindow` …).
    pub class_name: String,
    /// 화면에 그려진 자리, 물리 픽셀. DWM 확장 프레임(보이지 않는 크기 조절 테두리 · 그림자를 뺀 것), 안 되면
    /// `GetWindowRect`.
    pub rect: PxRect,
    /// 확장 창 스타일 (`GWL_EXSTYLE`).
    pub ex_style: u32,
    /// `GetWindowDisplayAffinity` 의 값: [`crate::WDA_NONE`] · [`crate::WDA_MONITOR`] ·
    /// [`crate::WDA_EXCLUDEFROMCAPTURE`].
    pub display_affinity: u32,
}

impl VisibleWindowEntry {
    /// 늘 맨 위 창 (`WS_EX_TOPMOST`).
    pub fn is_topmost(&self) -> bool {
        self.ex_style & WS_EX_TOPMOST != 0
    }

    /// 창이 스스로 화면 캡처를 막았다 (`WDA_MONITOR` · `WDA_EXCLUDEFROMCAPTURE`).
    pub fn capture_protected(&self) -> bool {
        self.display_affinity != WDA_NONE
    }

    /// 밑의 창을 가리는 보통 창인가. 층 창(`WS_EX_LAYERED`: 반투명일 수 있다) · 클릭 통과 창은 덮개로 치지 않는다.
    pub fn is_opaque(&self) -> bool {
        self.ex_style & (WS_EX_LAYERED | WS_EX_TRANSPARENT) == 0
    }
}

/// 캡처 사각형(물리 픽셀, 보통 DXGI 출력의 `DesktopCoordinates`)과 1픽셀 이상 겹치는 최상위 창들. 보이고
/// (`IsWindowVisible`) 최소화되지 않았고(`IsIconic`) DWM 이 가려 두지 않은(다른 가상 데스크톱 · 멈춘 UWP 앱) 창만,
/// Z 순서 위부터(`EnumWindows` 가 주는 차례) 준다. 이 프로세스의 창(오버레이)은 뺀다. 픽셀은 하나도 읽지 않는다.
///
/// 결과는 [`crate::surface::assess_surfaces`] 에 넘긴다. Windows 가 아니거나 `win32` 기능을 끄면 늘 빈 목록이다.
pub fn visible_windows_intersecting(capture: PxRect) -> Vec<VisibleWindowEntry> {
    imp::visible_windows(capture)
}

#[cfg(all(windows, feature = "win32"))]
mod imp {
    use super::{VisibleWindowEntry, WindowInfo};
    use crate::surface::{PxRect, WDA_NONE};
    use std::collections::HashMap;
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, HANDLE, HWND, LPARAM, POINT, RECT,
    };
    use windows_sys::Win32::Graphics::Dwm::{
        DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetLayeredWindowAttributes,
        GetWindowDisplayAffinity, GetWindowLongW, GetWindowRect, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, WindowFromPoint,
        GA_ROOT, GWL_EXSTYLE, LWA_ALPHA, WS_EX_LAYERED,
    };

    pub fn window_at(x: i32, y: i32) -> Option<WindowInfo> {
        // SAFETY: 값으로 넘기는 POINT 하나뿐이다. 돌려받은 HWND 는 다른 user32 함수에 넘기기만 한다
        // (닫힌 창의 핸들이어도 user32 는 실패를 돌려줄 뿐이다).
        let hit = unsafe { WindowFromPoint(POINT { x, y }) };
        if hit.is_null() {
            return None;
        }
        // SAFETY: 위와 같다.
        let root = unsafe { GetAncestor(hit, GA_ROOT) };
        info(if root.is_null() { hit } else { root })
    }

    pub fn foreground_window() -> Option<WindowInfo> {
        // SAFETY: 인자가 없다.
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_null() {
            None
        } else {
            info(hwnd)
        }
    }

    fn info(hwnd: HWND) -> Option<WindowInfo> {
        let mut pid = 0u32;
        // SAFETY: pid 는 이 함수의 지역 변수이고 호출 동안 살아 있다.
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if thread == 0 {
            return None; // 그 사이에 닫힌 창
        }
        let exe_path = if pid == 0 { None } else { image_path(pid) };
        let exe_name = exe_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        Some(WindowInfo {
            hwnd: hwnd as isize,
            pid,
            exe_path,
            exe_name,
            title: title(hwnd),
        })
    }

    /// 연 프로세스 핸들. 어디서 돌아가든 닫는다.
    struct Process(HANDLE);

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: OpenProcess 가 준, 아직 닫지 않은 핸들이다.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn image_path(pid: u32) -> Option<PathBuf> {
        // SAFETY: 인자는 값뿐이다. 실패하면 null 이다.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None; // 보호 프로세스 · 이미 끝난 프로세스
        }
        let process = Process(handle);
        // 보통 MAX_PATH 안이다. 긴 경로(\\?\ 32 767자)는 버퍼가 모자랄 때만 키운다.
        for cap in [260u32, 1024, 32_768] {
            let mut buf = vec![0u16; cap as usize];
            let mut len = cap;
            // SAFETY: buf 는 cap 칸이고 len 이 그 크기를 알린다. 성공하면 len 이 쓴 글자 수(널 제외)가 된다.
            let ok = unsafe {
                QueryFullProcessImageNameW(
                    process.0,
                    PROCESS_NAME_WIN32,
                    buf.as_mut_ptr(),
                    &mut len,
                )
            };
            if ok != 0 {
                buf.truncate((len as usize).min(cap as usize));
                return Some(PathBuf::from(OsString::from_wide(&buf)));
            }
            // SAFETY: 인자가 없다.
            if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
                return None;
            }
        }
        None
    }

    fn title(hwnd: HWND) -> String {
        // SAFETY: hwnd 를 넘기기만 한다.
        let len = unsafe { GetWindowTextLengthW(hwnd) };
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        // SAFETY: buf 는 len + 1 칸이고 그 크기를 넘긴다. 함수는 널을 포함해 그 안에만 쓴다.
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        buf.truncate(n.clamp(0, len) as usize);
        String::from_utf16_lossy(&buf)
    }

    pub fn visible_windows(capture: PxRect) -> Vec<VisibleWindowEntry> {
        let mut hwnds: Vec<HWND> = Vec::new();
        // SAFETY: 콜백은 아래 collect 이고 lparam 은 hwnds 의 주소다. EnumWindows 는 돌아오기 전에만 콜백을
        // 부르며, 그동안 hwnds 는 이 함수 안에 살아 있다.
        unsafe { EnumWindows(Some(collect), &mut hwnds as *mut Vec<HWND> as LPARAM) };
        let me = std::process::id();
        let mut images: HashMap<u32, Option<PathBuf>> = HashMap::new();
        let mut out = Vec::new();
        for hwnd in hwnds {
            // SAFETY: hwnd 를 넘기기만 한다 (그 사이 닫힌 창이면 FALSE 를 돌려줄 뿐이다).
            let shown = unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 };
            if !shown || cloaked(hwnd) {
                continue;
            }
            let Some(rect) = frame_rect(hwnd) else {
                continue;
            };
            if !rect.intersects(capture) {
                continue;
            }
            let mut pid = 0u32;
            // SAFETY: pid 는 이 함수의 지역 변수이고 호출 동안 살아 있다.
            if unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } == 0 || pid == me {
                continue; // 그 사이 닫힌 창, 또는 이 프로세스의 창 (제목을 읽으면 이 UI 스레드로 WM_GETTEXT 가 간다)
            }
            let exe_path = images
                .entry(pid)
                .or_insert_with(|| if pid == 0 { None } else { image_path(pid) })
                .clone();
            let exe_name = exe_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned());
            let mut affinity = WDA_NONE;
            // SAFETY: affinity 는 지역 변수다. 실패하면(닫힌 창) 아무것도 막지 않은 것으로 둔다.
            if unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) } == 0 {
                affinity = WDA_NONE;
            }
            // SAFETY: hwnd 를 넘기기만 한다. 확장 스타일은 32비트 값이라 GetWindowLongW 로 충분하다.
            let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
            if fully_transparent_layered(hwnd, ex_style) {
                continue;
            }
            out.push(VisibleWindowEntry {
                info: WindowInfo {
                    hwnd: hwnd as isize,
                    pid,
                    exe_path,
                    exe_name,
                    title: title(hwnd),
                },
                class_name: class_name(hwnd),
                rect,
                ex_style,
                display_affinity: affinity,
            });
        }
        out
    }

    /// EnumWindows 콜백: 창 핸들을 모은다.
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 은 visible_windows 의 hwnds 를 가리키고, 이 콜백은 EnumWindows 가 도는 동안만 불린다.
        let list = unsafe { &mut *(lparam as *mut Vec<HWND>) };
        list.push(hwnd);
        1 // 계속
    }

    /// DWM 이 가려 둔 창: 보인다고 하지만 그려지지 않는다 (다른 가상 데스크톱, 멈춘 UWP 앱).
    fn cloaked(hwnd: HWND) -> bool {
        let mut v = 0u32;
        // SAFETY: v 는 지역 u32 이고 그 크기를 넘긴다.
        let hr = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                &mut v as *mut u32 as *mut c_void,
                size_of::<u32>() as u32,
            )
        };
        hr == 0 && v != 0
    }

    /// 화면에 그려진 자리: DWM 확장 프레임(늘 물리 픽셀). 안 되면 GetWindowRect. 빈 사각형이면 `None`.
    fn frame_rect(hwnd: HWND) -> Option<PxRect> {
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: r 은 지역 RECT 이고 그 크기를 넘긴다.
        let hr = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut r as *mut RECT as *mut c_void,
                size_of::<RECT>() as u32,
            )
        };
        // SAFETY: 위와 같다.
        if hr != 0 && unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
            return None;
        }
        let rect = PxRect::new(r.left, r.top, r.right, r.bottom);
        (!rect.is_empty()).then_some(rect)
    }

    /// `WS_EX_LAYERED` 창이 `LWA_ALPHA` 로 완전 투명(alpha == 0)이면 화면에 보이지 않는다 (게임 오버레이 · 백그라운드 호스트 창).
    fn fully_transparent_layered(hwnd: HWND, ex_style: u32) -> bool {
        if ex_style & WS_EX_LAYERED == 0 {
            return false;
        }
        let (mut alpha, mut flags) = (255u8, 0u32);
        // SAFETY: 지역 변수 주소만 넘긴다. 실패하면 불투명한 것으로 둔다.
        let ok = unsafe {
            GetLayeredWindowAttributes(hwnd, std::ptr::null_mut(), &mut alpha, &mut flags)
        };
        ok != 0 && (flags & LWA_ALPHA != 0) && alpha == 0
    }

    fn class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: buf 는 256 칸이고 그 크기를 넘긴다. 함수는 널을 포함해 그 안에만 쓴다.
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.clamp(0, 255) as usize])
    }
}

#[cfg(not(all(windows, feature = "win32")))]
mod imp {
    use super::{VisibleWindowEntry, WindowInfo};
    use crate::surface::PxRect;

    pub fn window_at(_x: i32, _y: i32) -> Option<WindowInfo> {
        None
    }

    pub fn foreground_window() -> Option<WindowInfo> {
        None
    }

    pub fn visible_windows(_capture: PxRect) -> Vec<VisibleWindowEntry> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: u32, exe_name: Option<&str>) -> WindowInfo {
        WindowInfo {
            hwnd: 0x1234,
            pid,
            exe_path: exe_name.map(|n| PathBuf::from(format!("C:/Program Files/X/{n}"))),
            exe_name: exe_name.map(str::to_string),
            title: "제목".into(),
        }
    }

    #[test]
    fn app_uri_is_lowercased_file_name() {
        assert_eq!(
            sample(1, Some("Code.exe")).app_uri().as_deref(),
            Some("app://code.exe")
        );
        assert_eq!(
            sample(1, Some("Code - Insiders.EXE")).app_uri().as_deref(),
            Some("app://code - insiders.exe"),
            "접기는 normalize_cc 가 한다"
        );
        assert_eq!(
            sample(1, Some("카카오톡.exe")).app_uri().as_deref(),
            Some("app://카카오톡.exe")
        );
        assert_eq!(sample(1, None).app_uri(), None);
        assert_eq!(sample(1, Some("  ")).app_uri(), None);
    }

    #[test]
    fn excludes_own_process_only() {
        let me = std::process::id();
        assert!(sample(me, Some("app.exe")).is_current_process());
        assert_eq!(exclude_self(Some(sample(me, Some("app.exe")))), None);
        let other = sample(me.wrapping_add(1), Some("other.exe"));
        assert_eq!(exclude_self(Some(other.clone())), Some(other));
        assert_eq!(exclude_self(None), None);
    }

    #[test]
    fn far_away_point_does_not_panic() {
        // 어느 모니터에도 없는 점: Windows 는 None 이거나 무언가를 주고, 다른 곳은 늘 None 이다
        let _ = window_at(i32::MIN, i32::MIN);
        let _ = window_at(i32::MAX, i32::MAX);
        if !cfg!(all(windows, feature = "win32")) {
            assert_eq!(window_at(0, 0), None);
            assert_eq!(foreground_window(), None);
        }
    }
}

#[cfg(all(test, windows, feature = "win32"))]
mod win32_tests {
    use super::*;
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

    fn check(label: &str, w: &WindowInfo) {
        println!(
            "{label}: hwnd={:#x} pid={} exe={:?} title={:?} app_uri={:?}",
            w.hwnd,
            w.pid,
            w.exe_name,
            w.title,
            w.app_uri()
        );
        assert_ne!(w.hwnd, 0);
        if let Some(name) = &w.exe_name {
            assert!(name.to_lowercase().ends_with(".exe"), "{name}");
            assert!(w.app_uri().unwrap().starts_with("app://"));
            let path = w.exe_path.as_ref().expect("이름이 있으면 경로도 있다");
            assert!(path.is_absolute(), "{path:?}");
            assert_eq!(path.file_name().unwrap().to_string_lossy(), name.as_str());
        }
    }

    #[test]
    fn foreground_and_window_under_its_center() {
        let Some(fg) = foreground_window() else {
            println!("앞 창이 없다 (잠긴 화면 · 서비스 세션). 패닉하지 않은 것만 본다");
            return;
        };
        check("foreground_window", &fg);
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: r 은 지역 변수이고, 핸들은 방금 받은 것이다 (닫혔으면 0 을 돌려줄 뿐이다).
        if unsafe { GetWindowRect(fg.hwnd as HWND, &mut r) } == 0 {
            println!("앞 창이 그 사이에 닫혔다");
            return;
        }
        let (cx, cy) = (
            r.left + (r.right - r.left) / 2,
            r.top + (r.bottom - r.top) / 2,
        );
        match window_at(cx, cy) {
            // 다른 맨 위 창이 가운데를 덮고 있을 수 있으니 같은 창이라고는 보지 않는다
            Some(w) => check(&format!("window_at({cx}, {cy})"), &w),
            None => println!("window_at({cx}, {cy}): 창이 없다"),
        }
    }

    #[test]
    fn visible_windows_on_the_whole_desktop() {
        // 모든 모니터를 덮는 사각형: 보이는 창을 모두 받는다 (서비스 세션이면 빈 목록일 수 있다)
        let desktop = PxRect::new(-32_768, -32_768, 32_767, 32_767);
        let list = visible_windows_intersecting(desktop);
        for w in &list {
            assert!(!w.info.is_current_process(), "이 프로세스의 창은 뺀다");
            assert!(!w.rect.is_empty() && w.rect.intersects(desktop));
            assert_ne!(w.info.hwnd, 0);
        }
        let verdict = crate::assess_surfaces(&list, desktop);
        println!(
            "{} visible windows; signals {:?}",
            list.len(),
            verdict
                .hits
                .iter()
                .map(|h| (h.signal, h.exe_name.as_deref(), h.occluded))
                .collect::<Vec<_>>()
        );
        // 어느 모니터에도 없는 사각형에는 창이 없다
        let nowhere = PxRect::new(i32::MIN, i32::MIN, i32::MIN + 1, i32::MIN + 1);
        assert!(visible_windows_intersecting(nowhere).is_empty());
    }
}
