//! 확장 창 스타일(`WS_EX_*`) · 불투명도 · 프레임 간격. raon-zena `zena-core/src/window.rs` 의 계산을 그쪽 타입 없이 옮겼다.
//!
//! 스타일 값은 `SetWindowLongPtrW(GWL_EXSTYLE, ..)` 에, 알파는 `SetLayeredWindowAttributes(.., LWA_ALPHA)` 에 그대로 넣는다.
//! 계산에는 Win32 가 필요 없어서 모든 플랫폼에서 빌드된다.

use crate::stage::Size;
use std::time::Duration;

// Win32 확장 창 스타일 (winuser.h).
pub const WS_EX_TOPMOST: u32 = 0x0000_0008;
pub const WS_EX_TRANSPARENT: u32 = 0x0000_0020;
pub const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
pub const WS_EX_LAYERED: u32 = 0x0008_0000;
pub const WS_EX_NOACTIVATE: u32 = 0x0800_0000;

/// raon-zena 마이크로 뷰포트: 스왑 체인이 모니터 전체가 아니라 캐릭터만 덮는다. 논리 픽셀.
pub const VIEWPORT_W: u32 = 350;
pub const VIEWPORT_H: u32 = 450;
pub const VIEWPORT: Size = Size::new(VIEWPORT_W, VIEWPORT_H);

/// 0단계 알약의 불투명도 (style.css 의 82%, 기획서의 Auto-Dock 82%).
pub const PILL_OPACITY: f64 = 0.82;
/// 고스트 모드에서 커서가 캐릭터 위에 있을 때의 불투명도. 밑의 게임이 비쳐 보이게 한다.
pub const HOVER_OPACITY: f64 = 0.30;

/// [`PILL_OPACITY`] 의 알파 (209).
pub const PILL_ALPHA: u8 = alpha_u8(PILL_OPACITY);
/// [`HOVER_OPACITY`] 의 알파 (76). zena-core 의 `HOVER_ALPHA` 와 같다.
pub const HOVER_ALPHA: u8 = alpha_u8(HOVER_OPACITY);

/// 불투명도(0 ~ 1)를 `SetLayeredWindowAttributes` 의 알파(0 ~ 255)로. 소수는 버린다 (zena-core 의 `(255.0 * 0.30) as u8`
/// = 76 과 같게). 범위 밖은 0 · 255 로 자르고, NaN 은 0 이다.
pub const fn alpha_u8(opacity: f64) -> u8 {
    if opacity >= 1.0 {
        255
    } else if opacity > 0.0 {
        (255.0 * opacity) as u8
    } else {
        0
    }
}

/// 창 층의 상태. raon-zena 는 `WindowState` 에서 옮겨 담는다 (`click_through` = 모드가 `GhostClickThrough`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LayerFlags {
    /// 클릭이 창을 지나 밑의 창으로 간다 (고스트 모드).
    pub click_through: bool,
    /// 늘 맨 위.
    pub topmost: bool,
    /// 오버레이 안의 입력 칸이 열려 있다: 창이 포커스와 키보드 · IME 입력을 받는다 (닫을 때까지).
    pub typing: bool,
}

impl LayerFlags {
    /// 지금 상태에 쓸 확장 스타일. 늘 `LAYERED | TOOLWINDOW`, 입력 중이 아니면 `NOACTIVATE`, 맨 위면 `TOPMOST`,
    /// 클릭 통과이고 입력 중이 아니면 `TRANSPARENT` (입력 칸이 열리면 클릭을 받아야 한다).
    pub const fn ex_style(self) -> u32 {
        let mut s = WS_EX_LAYERED | WS_EX_TOOLWINDOW;
        if !self.typing {
            s |= WS_EX_NOACTIVATE;
        }
        if self.topmost {
            s |= WS_EX_TOPMOST;
        }
        if self.click_through && !self.typing {
            s |= WS_EX_TRANSPARENT;
        }
        s
    }

    /// 커서 위치에 따른 알파. 클릭 통과 중에 커서가 위에 있으면(입력 중이 아닐 때) [`HOVER_ALPHA`], 그 밖은 255.
    pub const fn alpha(self, cursor_over: bool) -> u8 {
        if self.click_through && cursor_over && !self.typing {
            HOVER_ALPHA
        } else {
            255
        }
    }
}

/// [`LayerFlags::ex_style`] 의 함수 모양.
pub const fn ex_style(flags: LayerFlags) -> u32 {
    flags.ex_style()
}

/// 화면이 얼마나 바쁜가. 프레임 간격을 고른다.
/// raon-zena 는 말하는 중이거나 Lv3 이면 `Busy`, Lv2 면 `Light`, 그 밖은 `Idle` 로 옮긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Activity {
    /// 가만히 있음: 15 fps
    #[default]
    Idle,
    /// 가벼운 움직임 (Lv2 신호): 24 fps
    Light,
    /// 말하기 · 움직임 (Lv3): 60 fps
    Busy,
}

impl Activity {
    pub const fn fps(self) -> u32 {
        match self {
            Activity::Idle => 15,
            Activity::Light => 24,
            Activity::Busy => 60,
        }
    }
}

/// 다음 프레임까지 기다릴 시간 (`ControlFlow::WaitUntil` 같은 곳에 쓴다). 숨은 창은 입력이 올 때만 깨우므로 `None`.
/// 마이크로초 단위로 버린다: 60 fps → 16 666 µs, 24 fps → 41 666 µs, 15 fps → 66 666 µs.
pub fn frame_interval(activity: Activity, visible: bool) -> Option<Duration> {
    if !visible {
        return None;
    }
    Some(Duration::from_micros(1_000_000 / activity.fps() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ex_style_table() {
        const BASE: u32 = WS_EX_LAYERED | WS_EX_TOOLWINDOW;
        // (click_through, topmost, typing) → 스타일
        let table = [
            ((false, false, false), BASE | WS_EX_NOACTIVATE),
            ((false, false, true), BASE),
            (
                (false, true, false),
                BASE | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            ),
            ((false, true, true), BASE | WS_EX_TOPMOST),
            (
                (true, false, false),
                BASE | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            ),
            ((true, false, true), BASE),
            (
                (true, true, false),
                BASE | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
            ),
            ((true, true, true), BASE | WS_EX_TOPMOST),
        ];
        for ((click_through, topmost, typing), want) in table {
            let flags = LayerFlags {
                click_through,
                topmost,
                typing,
            };
            assert_eq!(ex_style(flags), want, "{flags:?}");
            assert_eq!(flags.ex_style(), want);
        }
    }

    #[test]
    fn ghost_mode_matches_zena_core() {
        // zena-core WindowState::default() = 고스트 · 맨 위 · 입력 아님
        let ghost = LayerFlags {
            click_through: true,
            topmost: true,
            typing: false,
        };
        assert_eq!(
            ghost.ex_style(),
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
        );
        assert_eq!(ghost.ex_style(), 0x0808_00A8);
        let pet = LayerFlags {
            click_through: false,
            ..ghost
        };
        assert_eq!(pet.ex_style() & WS_EX_TRANSPARENT, 0);
        assert_eq!(ghost.alpha(true), 76);
        assert_eq!(ghost.alpha(false), 255);
        assert_eq!(pet.alpha(true), 255);
        let typing = LayerFlags {
            typing: true,
            ..ghost
        };
        assert_eq!(
            typing.ex_style() & (WS_EX_TRANSPARENT | WS_EX_NOACTIVATE),
            0,
            "입력 칸이 포커스를 받는다"
        );
        assert_eq!(typing.alpha(true), 255);
    }

    #[test]
    fn alphas() {
        assert_eq!(PILL_ALPHA, 209);
        assert_eq!(HOVER_ALPHA, 76);
        assert_eq!(HOVER_ALPHA, (255.0 * 0.30) as u8, "zena-core 와 같은 식");
        assert_eq!(alpha_u8(1.0), 255);
        assert_eq!(alpha_u8(2.0), 255);
        assert_eq!(alpha_u8(0.0), 0);
        assert_eq!(alpha_u8(-0.5), 0);
        assert_eq!(alpha_u8(f64::NAN), 0);
        assert_eq!(alpha_u8(0.5), 127);
        assert_eq!(alpha_u8(crate::Stage::Feed.opacity()), 229);
        assert_eq!(alpha_u8(crate::Stage::Drawer.opacity()), 255);
    }

    #[test]
    fn frame_pacing() {
        assert_eq!(
            frame_interval(Activity::Busy, true),
            Some(Duration::from_micros(16_666))
        );
        assert_eq!(
            frame_interval(Activity::Light, true),
            Some(Duration::from_micros(41_666))
        );
        assert_eq!(
            frame_interval(Activity::Idle, true),
            Some(Duration::from_micros(66_666))
        );
        for a in [Activity::Idle, Activity::Light, Activity::Busy] {
            assert_eq!(frame_interval(a, false), None, "숨은 창은 입력이 올 때만");
        }
        assert_eq!(Activity::default(), Activity::Idle);
    }

    #[test]
    fn viewport() {
        assert_eq!(VIEWPORT, Size::new(350, 450));
    }
}
