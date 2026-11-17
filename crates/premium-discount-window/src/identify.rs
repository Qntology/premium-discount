//! 점 아래 창과 앞 창 알아내기: 창 핸들 → 프로세스 번호 → 실행 파일 경로 · 이름, 그리고 창 제목.
//!
//! * [`window_at`] – `WindowFromPoint` 로 그 점의 창을 찾고 `GetAncestor(GA_ROOT)` 로 최상위 창까지 올라간다.
//!   드롭한 곳이 어느 프로그램인지 알 때 쓴다. 좌표는 화면의 물리 픽셀이다.
//! * [`foreground_window`] – `GetForegroundWindow`. 지금 사용자가 쓰고 있는 프로그램.
//! * [`exclude_self`] – 찾은 창이 이 프로세스 것이면 `None`. 오버레이가 자기를 알아보는 일을 막는다.
//!
//! 실행 파일은 `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW` 로 읽는다.
//! 보호 프로세스처럼 열 수 없으면 경로 · 이름만 비고 나머지는 채운다. 어떤 실패에도 패닉하지 않는다.
//!
//! 제목(`GetWindowTextW`)은 다른 프로세스의 창이면 메시지 없이 읽는다. 이 프로세스의 창이면 그 창의 UI 스레드로
//! `WM_GETTEXT` 가 가므로, 그 스레드가 멈춰 있는 동안에는 기다린다.
//!
//! Windows 가 아니거나 `win32` 기능을 끄면 두 함수는 늘 `None` 이다.
//!
//! 찾은 창을 평가 대상(`cc`)으로 쓰려면 [`WindowInfo::app_uri`] 를 premium-discount-schema 의
//! `envelope::normalize_cc` 에 넘긴다 (`app://code - insiders.exe` → `code-insiders.exe`).

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

#[cfg(all(windows, feature = "win32"))]
mod imp {
    use super::WindowInfo;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, HANDLE, HWND, POINT,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
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
}

#[cfg(not(all(windows, feature = "win32")))]
mod imp {
    use super::WindowInfo;

    pub fn window_at(_x: i32, _y: i32) -> Option<WindowInfo> {
        None
    }

    pub fn foreground_window() -> Option<WindowInfo> {
        None
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
}
