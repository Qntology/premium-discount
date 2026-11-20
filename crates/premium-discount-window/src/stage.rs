//! 3단계 오버레이 창의 규칙: 단계 크기, 단계 옮기기, 누르기와 끌기 구분, 창 자리. `web/desktop/stage.ts` 를 같은 수로 옮겼다.
//!
//! * 0단계 알약(360x56): 동기화 점과 최근 한 줄. 누르면 1단계, 끌면 창을 옮긴다.
//! * 1단계 미니 피드(360x260): 최근 내 평가 3개와 빠른 입력. 맨 위 줄을 누르면 2단계, Esc 나 접기는 0단계.
//! * 2단계 서랍(400x580): 검색, 걸러 보기, 타임라인, 계정 · 지갑 · 모델. Esc 는 1단계.
//!
//! 모바일(Android · iOS)은 창 크기를 바꿀 수 없으므로 2단계를 전체 화면으로만 쓴다 ([`LayoutMode::Full`]).
//!
//! stage.ts 를 고치면 `tests/gen_stage_cases.mjs` 로 `tests/stage_cases.json` 을 다시 만들고 여기도 맞춘다.
//! Rust 시험(`tests/stage_cases.rs`)과 TS 시험(`web/desktop/stage.test.ts`)이 그 표를 함께 읽는다.

/// 크기, 논리 픽셀.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Size {
        Size { width, height }
    }

    /// 넓이. [`grows_to`] 가 단계 크기를 견줄 때 쓴다.
    pub const fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// 창의 왼쪽 위 위치, 물리 픽셀. 왼쪽 · 위 모니터에서는 음수다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

impl Position {
    pub const fn new(x: i32, y: i32) -> Position {
        Position { x, y }
    }
}

/// 모니터의 작업 영역 (작업 표시줄을 뺀 곳). 물리 픽셀.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 지킬 오른쪽 아래 모서리 (사용자가 끌어 옮긴 창). 물리 픽셀.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub right: i32,
    pub bottom: i32,
}

impl Anchor {
    /// 지금 창의 바깥 위치와 바깥 크기(물리 픽셀)로 모서리를 잡는다. main.ts 의
    /// `{ right: p.x + s.width, bottom: p.y + s.height }` 와 같다.
    pub fn from_outer(position: Position, width: u32, height: u32) -> Anchor {
        Anchor {
            right: saturate(position.x as i64 + width as i64),
            bottom: saturate(position.y as i64 + height as i64),
        }
    }
}

/// 단계. 숫자(0 · 1 · 2)는 TS 의 `Stage` 와 같다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// 0단계 알약
    Pill = 0,
    /// 1단계 미니 피드
    Feed = 1,
    /// 2단계 서랍
    Drawer = 2,
}

/// 단계마다 창 크기 (논리 픽셀), 단계 번호 순서. web/desktop/style.css 의 `--stage-w` / `--stage-h` 와 같은 값이다.
pub const STAGE_SIZE: [Size; 3] = [Size::new(360, 56), Size::new(360, 260), Size::new(400, 580)];

/// 화면 가장자리(작업 영역)와의 여백, 논리 픽셀.
pub const DOCK_MARGIN: u32 = 16;

/// 이만큼(논리 픽셀) 움직이기 전에 손을 떼면 누르기, 넘으면 끌기.
pub const DRAG_THRESHOLD: f64 = 5.0;

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::Pill, Stage::Feed, Stage::Drawer];

    /// TS 의 단계 번호 (0 · 1 · 2).
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// 0 · 1 · 2 만 받는다.
    pub const fn from_index(index: u8) -> Option<Stage> {
        match index {
            0 => Some(Stage::Pill),
            1 => Some(Stage::Feed),
            2 => Some(Stage::Drawer),
            _ => None,
        }
    }

    /// 이 단계의 창 크기 ([`STAGE_SIZE`]).
    pub const fn size(self) -> Size {
        STAGE_SIZE[self as usize]
    }

    /// 배경 불투명도 (style.css): 알약 82%, 미니 피드 90%, 서랍은 불투명. 창 전체 알파로 쓰려면 [`crate::alpha_u8`].
    pub const fn opacity(self) -> f64 {
        match self {
            Stage::Pill => crate::layer::PILL_OPACITY,
            Stage::Feed => 0.90,
            Stage::Drawer => 1.0,
        }
    }
}

/// 단계를 옮기는 사건. 문자열은 TS 의 `StageEvent` 와 같다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageEvent {
    /// 알약을 누름 (`pill`)
    Pill,
    /// 미니 피드의 맨 위 줄을 누름 (`feed-top`)
    FeedTop,
    /// 펼치기 (`expand`)
    Expand,
    /// Esc (`escape`)
    Escape,
    /// 접기 (`collapse`)
    Collapse,
}

impl StageEvent {
    pub const ALL: [StageEvent; 5] = [
        StageEvent::Pill,
        StageEvent::FeedTop,
        StageEvent::Expand,
        StageEvent::Escape,
        StageEvent::Collapse,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            StageEvent::Pill => "pill",
            StageEvent::FeedTop => "feed-top",
            StageEvent::Expand => "expand",
            StageEvent::Escape => "escape",
            StageEvent::Collapse => "collapse",
        }
    }

    /// TS 의 이름(`pill` · `feed-top` · `expand` · `escape` · `collapse`)만 받는다.
    pub fn parse(s: &str) -> Option<StageEvent> {
        StageEvent::ALL.into_iter().find(|e| e.as_str() == s)
    }
}

/// 단계 옮기기. `ready` 가 false 면(앱이 아직 시작하지 못했으면) 알약을 눌렀을 때 바로 시작 안내가 있는 2단계로 가고,
/// 2단계에서 Esc 는 알약으로 돌아간다. TS 는 `ready` 를 빼면 true 다.
pub fn next_stage(current: Stage, event: StageEvent, ready: bool) -> Stage {
    match event {
        StageEvent::Pill => match current {
            Stage::Pill if ready => Stage::Feed,
            Stage::Pill => Stage::Drawer,
            other => other,
        },
        StageEvent::FeedTop | StageEvent::Expand => Stage::Drawer,
        StageEvent::Escape => match current {
            Stage::Drawer if ready => Stage::Feed,
            _ => Stage::Pill,
        },
        StageEvent::Collapse => Stage::Pill,
    }
}

/// 누르기와 끌기.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerAction {
    Click,
    Drag,
}

impl PointerAction {
    /// TS 의 `'click'` · `'drag'`.
    pub const fn as_str(self) -> &'static str {
        match self {
            PointerAction::Click => "click",
            PointerAction::Drag => "drag",
        }
    }
}

/// 누른 점에서 움직인 거리(논리 픽셀)로 누르기와 끌기를 가른다. 거리가 문턱과 같으면 끌기다 (문턱 [`DRAG_THRESHOLD`]).
pub fn classify_pointer(dx: f64, dy: f64, threshold: f64) -> PointerAction {
    if dx.hypot(dy) >= threshold {
        PointerAction::Drag
    } else {
        PointerAction::Click
    }
}

/// 실행되는 곳. TS 의 `Platform` (`desktop` · `android` · `ios`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Desktop,
    Android,
    Ios,
}

/// 데스크톱은 오버레이(단계마다 창 크기를 바꾼다), 모바일은 전체 화면.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutMode {
    Overlay,
    Full,
}

pub fn layout_mode(platform: Platform) -> LayoutMode {
    match platform {
        Platform::Desktop => LayoutMode::Overlay,
        Platform::Android | Platform::Ios => LayoutMode::Full,
    }
}

/// 전체 화면에서는 늘 2단계다.
pub fn effective_stage(mode: LayoutMode, stage: Stage) -> Stage {
    match mode {
        LayoutMode::Full => Stage::Drawer,
        LayoutMode::Overlay => stage,
    }
}

/// 창을 커지거나 작아지게 할 때 오른쪽 아래 모서리를 고정한다.
/// `anchor` 가 없으면 작업 영역 오른쪽 아래에 `margin` 을 두고 붙이고, 있으면(사용자가 끌어 옮긴 창) 그 모서리를 지킨다.
/// 어느 쪽이든 창 전체가 작업 영역 안에 들어오게 자르고, 작업 영역보다 큰 창은 왼쪽 위에 맞춘다.
///
/// `size` · `margin` 은 논리, `area` · `anchor` · 결과는 물리 픽셀이다. 배율이 유한한 양수가 아니면 1 로 본다.
/// 논리를 물리로 바꿀 때 JS `Math.round` 처럼 반올림한다 (반은 위로). stage.ts 와 같은 정수를 낸다.
pub fn place_window(
    size: Size,
    area: WorkArea,
    scale: f64,
    anchor: Option<Anchor>,
    margin: u32,
) -> Position {
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let w = js_round(size.width as f64 * s) as i64;
    let h = js_round(size.height as f64 * s) as i64;
    let m = js_round(margin as f64 * s) as i64;
    let (ax, ay) = (area.x as i64, area.y as i64);
    let (far_x, far_y) = (ax + area.width as i64, ay + area.height as i64);
    let right = anchor.map_or(far_x.saturating_sub(m), |a| a.right as i64);
    let bottom = anchor.map_or(far_y.saturating_sub(m), |a| a.bottom as i64);
    Position {
        x: saturate(clamp(right.saturating_sub(w), ax, far_x.saturating_sub(w))),
        y: saturate(clamp(bottom.saturating_sub(h), ay, far_y.saturating_sub(h))),
    }
}

/// 제자리: 작업 영역 오른쪽 아래에 [`DOCK_MARGIN`] 을 두고 붙인 위치. 처음 띄울 때와 [`crate::dock_step`] 의 목적지.
pub fn dock_position(size: Size, area: WorkArea, scale: f64) -> Position {
    place_window(size, area, scale, None, DOCK_MARGIN)
}

/// 커지는 쪽이면 먼저 옮기고 키운다 (키운 뒤 옮기면 잠깐 화면 밖으로 나간다). 작아지면 먼저 줄이고 옮긴다.
pub fn grows_to(from: Stage, to: Stage) -> bool {
    to.size().area() > from.size().area()
}

/// JS `Math.round`: 가장 가까운 정수, 반은 +무한대 쪽 (`-2.5` → `-2`). Rust 의 `round` 는 반을 0 에서 먼 쪽으로 보낸다.
fn js_round(v: f64) -> f64 {
    let f = v.floor();
    if v - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// TS 의 `Math.max(lo, Math.min(v, Math.max(lo, hi)))`: `hi < lo` 면 `lo`.
fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    lo.max(v.min(lo.max(hi)))
}

fn saturate(v: i64) -> i32 {
    v.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: WorkArea = WorkArea {
        x: 0,
        y: 0,
        width: 1920,
        height: 1040,
    };

    #[test]
    fn stage_sizes_and_numbers() {
        assert_eq!(Stage::Pill.size(), Size::new(360, 56));
        assert_eq!(Stage::Feed.size(), Size::new(360, 260));
        assert_eq!(Stage::Drawer.size(), Size::new(400, 580));
        for s in Stage::ALL {
            assert_eq!(Stage::from_index(s.index()), Some(s));
        }
        assert_eq!(Stage::from_index(3), None);
        assert_eq!(DOCK_MARGIN, 16);
        assert_eq!(DRAG_THRESHOLD, 5.0);
        assert_eq!(Stage::Pill.opacity(), 0.82);
        assert_eq!(Stage::Drawer.opacity(), 1.0);
    }

    #[test]
    fn event_names_round_trip() {
        for e in StageEvent::ALL {
            assert_eq!(StageEvent::parse(e.as_str()), Some(e));
        }
        assert_eq!(StageEvent::parse("feed-top"), Some(StageEvent::FeedTop));
        assert_eq!(StageEvent::parse("feed_top"), None);
        assert_eq!(StageEvent::parse(""), None);
    }

    #[test]
    fn stage_transitions() {
        use Stage::*;
        use StageEvent as E;
        assert_eq!(next_stage(Pill, E::Pill, true), Feed);
        assert_eq!(
            next_stage(Feed, E::Pill, true),
            Feed,
            "1단계에는 알약이 없다"
        );
        assert_eq!(next_stage(Feed, E::FeedTop, true), Drawer);
        assert_eq!(next_stage(Feed, E::Expand, true), Drawer);
        assert_eq!(next_stage(Pill, E::Expand, true), Drawer);
        assert_eq!(next_stage(Drawer, E::Escape, true), Feed);
        assert_eq!(next_stage(Feed, E::Escape, true), Pill);
        assert_eq!(next_stage(Pill, E::Escape, true), Pill);
        assert_eq!(next_stage(Drawer, E::Collapse, true), Pill);
        assert_eq!(next_stage(Feed, E::Collapse, true), Pill);
        // 앱이 시작하지 못했으면 바로 시작 안내(2단계)로 가고, Esc 는 알약으로
        assert_eq!(next_stage(Pill, E::Pill, false), Drawer);
        assert_eq!(next_stage(Drawer, E::Escape, false), Pill);
    }

    #[test]
    fn click_until_five_pixels() {
        let t = DRAG_THRESHOLD;
        assert_eq!(classify_pointer(0.0, 0.0, t), PointerAction::Click);
        assert_eq!(
            classify_pointer(3.0, 3.0, t),
            PointerAction::Click,
            "대각선 4.24px"
        );
        assert_eq!(
            classify_pointer(4.0, 3.0, t),
            PointerAction::Drag,
            "정확히 5px"
        );
        assert_eq!(classify_pointer(0.0, 4.999, t), PointerAction::Click);
        assert_eq!(classify_pointer(-6.0, 0.0, t), PointerAction::Drag);
        assert_eq!(classify_pointer(0.0, -2.0, t), PointerAction::Click);
        assert_eq!(
            classify_pointer(f64::NAN, 0.0, t),
            PointerAction::Click,
            "JS 처럼 NaN 은 누르기"
        );
        assert_eq!(PointerAction::Drag.as_str(), "drag");
    }

    #[test]
    fn docks_bottom_right_with_scaled_margin() {
        assert_eq!(
            place_window(Stage::Pill.size(), HD, 1.0, None, DOCK_MARGIN),
            Position::new(1920 - 360 - 16, 1040 - 56 - 16)
        );
        // 150% 배율: 크기와 여백 모두 물리 픽셀로
        let hi = WorkArea {
            x: 0,
            y: 0,
            width: 2880,
            height: 1560,
        };
        assert_eq!(
            place_window(Stage::Drawer.size(), hi, 1.5, None, DOCK_MARGIN),
            Position::new(2880 - 600 - 24, 1560 - 870 - 24)
        );
        assert_eq!(
            dock_position(Stage::Drawer.size(), hi, 1.5),
            Position::new(2256, 666)
        );
        // 왼쪽 모니터 (x 가 음수)
        let left = WorkArea {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            place_window(Stage::Feed.size(), left, 1.0, None, DOCK_MARGIN),
            Position::new(-360 - 16, 1080 - 260 - 16)
        );
        // 왼쪽 모니터 150%
        let left_hi = WorkArea {
            x: -2880,
            y: 0,
            width: 2880,
            height: 1560,
        };
        assert_eq!(
            place_window(Stage::Pill.size(), left_hi, 1.5, None, DOCK_MARGIN),
            Position::new(-540 - 24, 1560 - 84 - 24)
        );
    }

    #[test]
    fn odd_scales_fall_back_to_one() {
        let one = place_window(Stage::Pill.size(), HD, 1.0, None, DOCK_MARGIN);
        for bad in [0.0, -1.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                place_window(Stage::Pill.size(), HD, bad, None, DOCK_MARGIN),
                one,
                "배율 {bad}"
            );
        }
    }

    #[test]
    fn keeps_dragged_corner_inside_work_area() {
        let a = |right, bottom| Some(Anchor { right, bottom });
        assert_eq!(
            place_window(Stage::Drawer.size(), HD, 1.0, a(1000, 600), DOCK_MARGIN),
            Position::new(600, 20)
        );
        assert_eq!(
            place_window(Stage::Drawer.size(), HD, 1.0, a(370, 70), DOCK_MARGIN),
            Position::new(0, 0)
        );
        assert_eq!(
            place_window(Stage::Pill.size(), HD, 1.0, a(5000, 5000), DOCK_MARGIN),
            Position::new(1920 - 360, 1040 - 56)
        );
        let tiny = WorkArea {
            x: 0,
            y: 0,
            width: 300,
            height: 400,
        };
        assert_eq!(
            place_window(Stage::Drawer.size(), tiny, 1.0, None, DOCK_MARGIN),
            Position::new(0, 0)
        );
        // 극단값에서도 넘치지 않는다
        let far = a(i32::MAX, i32::MIN);
        let p = place_window(Stage::Drawer.size(), HD, 1e300, far, u32::MAX);
        assert_eq!(p, Position::new(0, 0));
    }

    #[test]
    fn anchor_from_outer_rect() {
        let a = Anchor::from_outer(Position::new(-500, 100), 360, 56);
        assert_eq!(
            a,
            Anchor {
                right: -140,
                bottom: 156
            }
        );
    }

    #[test]
    fn grow_first_when_bigger() {
        use Stage::*;
        assert!(grows_to(Pill, Feed));
        assert!(grows_to(Feed, Drawer));
        assert!(grows_to(Pill, Drawer));
        assert!(!grows_to(Drawer, Pill));
        assert!(!grows_to(Feed, Pill));
        assert!(!grows_to(Feed, Feed));
    }

    #[test]
    fn mobile_is_always_full_drawer() {
        assert_eq!(layout_mode(Platform::Desktop), LayoutMode::Overlay);
        assert_eq!(layout_mode(Platform::Android), LayoutMode::Full);
        assert_eq!(layout_mode(Platform::Ios), LayoutMode::Full);
        assert_eq!(
            effective_stage(LayoutMode::Full, Stage::Pill),
            Stage::Drawer
        );
        assert_eq!(
            effective_stage(LayoutMode::Full, Stage::Feed),
            Stage::Drawer
        );
        assert_eq!(
            effective_stage(LayoutMode::Overlay, Stage::Pill),
            Stage::Pill
        );
        assert_eq!(
            effective_stage(LayoutMode::Overlay, Stage::Feed),
            Stage::Feed
        );
    }

    #[test]
    fn rounds_like_math_round() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(0.49999999999999994), 0.0);
        assert_eq!(js_round(61.60000000000001), 62.0);
        assert_eq!(js_round(-0.4), 0.0);
    }
}
