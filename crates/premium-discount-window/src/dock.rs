//! 제자리로 돌아가기 (Auto-Dock): 끌어 둔 창을 지금 자리에서 제자리([`crate::dock_position`], 작업 영역 오른쪽 아래)로
//! 0.45초 동안 옮긴다. 처음에 빠르고 끝에서 느려지는 cubic ease-out 이다.
//!
//! 부르는 쪽이 타이머(프레임)마다 지난 시간으로 [`dock_step`] 을 불러 나온 위치로 창을 옮기고, `done` 이면 멈춘다.
//! 위치는 물리 픽셀이다.

use crate::stage::Position;
use std::time::Duration;

/// 돌아가는 데 걸리는 시간.
pub const DOCK_DURATION: Duration = Duration::from_millis(450);

/// cubic ease-out: `1 - (1 - t)^3`. `t` 는 0 ~ 1 로 자르고, NaN 은 0 으로 본다.
pub fn ease_out_cubic(t: f64) -> f64 {
    if t.is_nan() || t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let u = 1.0 - t;
    1.0 - u * u * u
}

/// `elapsed` 만큼 지났을 때의 위치와 끝났는지. 끝나면(또는 `duration` 이 0 이면) 정확히 `to` 를 준다.
/// 가운데 위치는 [`ease_out_cubic`] 으로 섞어 가장 가까운 정수로 반올림한다.
pub fn dock_step(
    from: Position,
    to: Position,
    elapsed: Duration,
    duration: Duration,
) -> (Position, bool) {
    if elapsed >= duration {
        return (to, true);
    }
    let k = ease_out_cubic(elapsed.as_secs_f64() / duration.as_secs_f64());
    let mix = |a: i32, b: i32| (a as f64 + (b as f64 - a as f64) * k).round() as i32;
    (Position::new(mix(from.x, to.x), mix(from.y, to.y)), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints_and_shape() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(0.5), 0.875);
        assert_eq!(ease_out_cubic(-1.0), 0.0);
        assert_eq!(ease_out_cubic(3.0), 1.0);
        assert_eq!(ease_out_cubic(f64::NAN), 0.0);
        // 처음에 빠르다: 시간의 절반이 지나기 전에 거리의 절반을 넘는다
        assert!(ease_out_cubic(0.25) > 0.5);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease_out_cubic(i as f64 / 100.0);
            assert!(v > last, "늘 앞으로만 간다");
            last = v;
        }
    }

    #[test]
    fn steps_from_current_to_dock() {
        let from = Position::new(600, 20);
        let to = Position::new(1544, 968);
        assert_eq!(DOCK_DURATION, Duration::from_millis(450));
        assert_eq!(
            dock_step(from, to, Duration::ZERO, DOCK_DURATION),
            (from, false)
        );
        // 절반 시간: 거리의 87.5%
        let (mid, done) = dock_step(from, to, Duration::from_millis(225), DOCK_DURATION);
        assert!(!done);
        assert_eq!(mid, Position::new(600 + 826, 20 + 830)); // 944 * 0.875 = 826, 948 * 0.875 = 829.5 → 830
        assert_eq!(
            dock_step(from, to, DOCK_DURATION, DOCK_DURATION),
            (to, true)
        );
        assert_eq!(
            dock_step(from, to, Duration::from_secs(5), DOCK_DURATION),
            (to, true)
        );
    }

    #[test]
    fn steps_toward_negative_and_never_overshoot() {
        let from = Position::new(-100, 900);
        let to = Position::new(-2256, 466);
        let mut last = from;
        for ms in (0..=450).step_by(15) {
            let (p, done) = dock_step(from, to, Duration::from_millis(ms), DOCK_DURATION);
            assert!(p.x <= last.x && p.x >= to.x, "{ms}ms: {p:?}");
            assert!(p.y <= last.y && p.y >= to.y, "{ms}ms: {p:?}");
            assert_eq!(done, ms == 450);
            last = p;
        }
        assert_eq!(last, to);
    }

    #[test]
    fn zero_duration_jumps() {
        let to = Position::new(5, 6);
        assert_eq!(
            dock_step(Position::new(0, 0), to, Duration::ZERO, Duration::ZERO),
            (to, true)
        );
    }
}
