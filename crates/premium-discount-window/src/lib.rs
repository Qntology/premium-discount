//! 오버레이 창 계산. premium-discount 앱의 3단계 오버레이(`web/desktop/stage.ts`)와 raon-zena 의 마이크로 뷰포트 오버레이
//! (`zena-core/src/window.rs`)가 함께 쓰는 순수 함수, 그리고 점 아래 창 · 앞 창을 알아내는 작은 Win32 모듈이다.
//!
//! * [`stage`] – 단계(0 알약 · 1 미니 피드 · 2 서랍)의 크기와 옮기기, 누르기와 끌기 구분, 창 자리(오른쪽 아래 모서리 고정).
//!   `web/desktop/stage.ts` 를 같은 수로 옮겼다. 두 쪽이 `tests/stage_cases.json` 한 표를 읽어 같은 답을 내는지 본다
//!   (`tests/gen_stage_cases.mjs` 가 stage.ts 를 돌려 만든 표).
//! * [`layer`] – 확장 창 스타일(`WS_EX_*`) 비트, 불투명도(알파), 프레임 간격. raon-zena `zena-core::window` 에서 가져왔고,
//!   그쪽 타입(`WindowLayerMode` · `ReactionLevel`) 대신 [`LayerFlags`] · [`Activity`] 를 받는다.
//! * [`dock`] – 끌어 둔 창이 제자리(작업 영역 오른쪽 아래)로 돌아가는 0.45초 움직임 (cubic ease-out).
//! * [`identify`] – 점 아래 창([`window_at`])과 앞 창([`foreground_window`])의 프로세스 · 실행 파일 · 제목, 그리고
//!   캡처 모니터와 겹치는 보이는 창 모두([`visible_windows_intersecting`], Z 순서 위부터).
//!   Windows 에서 `win32` 기능(기본으로 켜짐)일 때만 실제로 찾고, 그 밖에는 늘 `None` · 빈 목록이다.
//! * [`surface`] – 그 창 목록으로 보는 개인 화면 신호 ([`assess_surfaces`]): 캡처 보호(`WDA_EXCLUDEFROMCAPTURE`) 창 ·
//!   결제 오버레이 · 민감 제목은 화면 전체 잠금, 알림 토스트는 그 자리만 가림. 위 창에 남김없이 가린 창은 치지 않는다.
//!   Win32 없이 돈다.
//!
//! 비동기 런타임 · Tauri · winit 에 의존하지 않는다. 창을 실제로 옮기고 스타일을 바꾸는 것은 부르는 쪽이다
//! (앱은 `main.ts` 의 "단계 창", raon-zena 는 `apps/raon-zena` 의 `SetWindowLongPtrW` · `SetWindowPos`).
//!
//! **픽셀 단위.** 크기([`Size`])와 여백은 논리 픽셀, 위치([`Position`]) · 작업 영역([`WorkArea`]) · 모서리([`Anchor`])는
//! 물리 픽셀이다. 배율(scale factor)로 논리를 물리로 바꾼다. [`window_at`] 의 좌표도 물리 픽셀이다 (프로세스가
//! 모니터별 DPI 인식일 때. Tauri 앱은 그렇다).
//!
//! **`cc` 로 넘기기.** [`WindowInfo::app_uri`] 가 주는 `app://code.exe` 를 premium-discount-schema 의
//! `envelope::normalize_cc` 에 넣으면 서버가 받는 `cc` 가 된다 (`code.exe`). 이 crate 는 schema 에 의존하지 않는다.
//!
//! **raon-zena 에서 쓰기.** `WindowState` 는 [`LayerFlags`] 로 옮겨 [`ex_style`] · [`LayerFlags::alpha`] 를 부른다:
//! `click_through` = 모드가 `GhostClickThrough`, `topmost` · `typing` 은 그대로. 프레임 간격은 말하는 중이거나 Lv3 이면
//! [`Activity::Busy`], Lv2 면 [`Activity::Light`], 그 밖은 [`Activity::Idle`] 이다.

pub mod dock;
pub mod identify;
pub mod layer;
pub mod stage;
pub mod surface;

pub use dock::{dock_step, ease_out_cubic, DOCK_DURATION};
pub use identify::{
    exclude_self, foreground_window, visible_windows_intersecting, window_at, VisibleWindowEntry,
    WindowInfo,
};
pub use layer::{
    alpha_u8, ex_style, frame_interval, Activity, LayerFlags, HOVER_ALPHA, HOVER_OPACITY,
    PILL_ALPHA, PILL_OPACITY, VIEWPORT, VIEWPORT_H, VIEWPORT_W, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};
pub use stage::{
    classify_pointer, dock_position, effective_stage, grows_to, layout_mode, next_stage,
    place_window, Anchor, LayoutMode, Platform, PointerAction, Position, Size, Stage, StageEvent,
    WorkArea, DOCK_MARGIN, DRAG_THRESHOLD, STAGE_SIZE,
};
pub use surface::{
    assess_surfaces, classify_window, covered_by, sensitive_title, OsSurfaceVerdict, PxRect,
    SurfaceHit, SurfaceSignal, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE,
};
