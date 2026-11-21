//! 32 × 18 휘도 썸네일과 깨우기 규칙. 화면 감시기(raon-zena `watcher.rs`)는 몇 초마다 받은 프레임을 이 썸네일로 줄여
//! 직전 썸네일과 견주고, 바뀐 것이 없으면 비전 인코더를 건너뛴다.
//!
//! 화면 전체의 평균 변화만 보면(0 ~ 255 휘도로 3 미만이면 그대로), 구석에 작게 뜬 2단계 인증 · 로그인 창(화면의 3 ~ 5 %)은
//! 평균을 3 도 못 바꿔 걸러진다. 그래서 3 × 3 칸 블록(화면의 약 1.6 %) 가운데 가장 많이 바뀐 블록의 평균 변화가 18 을
//! 넘어도 깨운다 ([`WakeRule`]).
//!
//! 썸네일 말고는 픽셀을 남기지 않는다. 썸네일도 버릴 때(Drop)와 [`LumaThumb::wipe`] 때 0 으로 덮는다.

use zeroize::Zeroize;

/// 썸네일 칸 수 (가로 · 세로)
pub const THUMB_W: usize = 32;
pub const THUMB_H: usize = 18;

/// 32 × 18 칸의 평균 휘도 (0 ~ 255, BT.601 `0.299 R + 0.587 G + 0.114 B`), 행 우선
#[derive(Clone, Debug, PartialEq)]
pub struct LumaThumb {
    luma: Vec<f32>,
}

impl LumaThumb {
    /// RGB8 프레임에서. 각 칸은 그 칸에 드는 픽셀의 평균이다 (raon-zena watcher 의 `thumbnail` 과 같은 계산)
    pub fn from_rgb(rgb: &[u8], w: usize, h: usize) -> LumaThumb {
        let mut luma = vec![0f32; THUMB_W * THUMB_H];
        let mut count = vec![0u32; THUMB_W * THUMB_H];
        for y in 0..h {
            let ty = y * THUMB_H / h.max(1);
            for x in 0..w {
                let tx = x * THUMB_W / w.max(1);
                let p = &rgb[(y * w + x) * 3..(y * w + x) * 3 + 3];
                luma[ty * THUMB_W + tx] += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
                count[ty * THUMB_W + tx] += 1;
            }
        }
        luma.iter_mut().zip(&count).for_each(|(v, c)| *v /= (*c).max(1) as f32);
        LumaThumb { luma }
    }

    /// 칸들의 휘도 (지운 썸네일은 비어 있다)
    pub fn values(&self) -> &[f32] {
        &self.luma
    }

    /// 화면 전체의 변화: 칸별 휘도 차 절댓값의 평균. 한쪽을 지웠으면(칸 수가 다르면) 무한대
    pub fn mean_change(&self, other: &LumaThumb) -> f32 {
        if self.luma.len() != other.luma.len() || self.luma.is_empty() {
            return f32::INFINITY;
        }
        self.luma.iter().zip(&other.luma).map(|(a, b)| (a - b).abs()).sum::<f32>() / self.luma.len() as f32
    }

    /// 국소 변화: `block` × `block` 칸 블록마다 칸별 휘도 차 절댓값을 평균하고, 그 가운데 가장 큰 값.
    /// 한쪽을 지웠으면 무한대
    pub fn max_block_change(&self, other: &LumaThumb, block: usize) -> f32 {
        if self.luma.len() != other.luma.len() || self.luma.len() != THUMB_W * THUMB_H {
            return f32::INFINITY;
        }
        let b = block.clamp(1, THUMB_H);
        let diff: Vec<f32> = self.luma.iter().zip(&other.luma).map(|(a, b)| (a - b).abs()).collect();
        let mut best = 0f32;
        for y0 in 0..=THUMB_H - b {
            for x0 in 0..=THUMB_W - b {
                let sum: f32 = (y0..y0 + b).map(|y| diff[y * THUMB_W + x0..y * THUMB_W + x0 + b].iter().sum::<f32>()).sum();
                best = best.max(sum / (b * b) as f32);
            }
        }
        best
    }

    /// 휘도를 0 으로 덮고 비운다. 개인 화면으로 바뀌어 판독을 그만둘 때 부른다
    pub fn wipe(&mut self) {
        self.luma.zeroize();
    }
}

impl Drop for LumaThumb {
    fn drop(&mut self) {
        self.luma.zeroize();
    }
}

/// 왜 깨웠는가 (또는 건너뛰는가)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// 견줄 썸네일이 없다 (처음이거나 지웠다)
    First,
    /// 화면 전체가 바뀌었다
    Global,
    /// 화면 일부(블록)만 크게 바뀌었다: 구석에 작게 뜬 창
    Local,
    /// 그대로다: 인코더를 건너뛴다
    Still,
}

impl Wake {
    /// 인코더를 돌리는가
    pub fn wakes(self) -> bool {
        self != Wake::Still
    }
}

/// 언제 인코더를 깨우는가. 휘도 단위는 0 ~ 255
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WakeRule {
    /// 화면 전체 평균 변화가 이만큼이면 깨운다 (raon-zena `WatcherConfig::change_min` 의 3.0)
    pub mean_min: f32,
    /// 국소 블록 한 변의 칸 수
    pub block: usize,
    /// 가장 많이 바뀐 블록의 평균 변화가 이만큼이면 깨운다
    pub block_min: f32,
}

impl Default for WakeRule {
    fn default() -> Self {
        WakeRule {
            mean_min: 3.0,
            block: 3,
            block_min: 18.0,
        }
    }
}

impl WakeRule {
    /// 직전 썸네일(`prev`, 없거나 지웠으면 처음으로 본다)과 지금 썸네일로 깨울지 정한다
    pub fn check(&self, prev: Option<&LumaThumb>, cur: &LumaThumb) -> Wake {
        let Some(prev) = prev.filter(|p| !p.luma.is_empty() && p.luma.len() == cur.luma.len()) else {
            return Wake::First;
        };
        if prev.mean_change(cur) >= self.mean_min {
            Wake::Global
        } else if prev.max_block_change(cur, self.block) >= self.block_min {
            Wake::Local
        } else {
            Wake::Still
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, v: u8) -> Vec<u8> {
        vec![v; w * h * 3]
    }

    #[test]
    fn the_thumbnail_matches_the_watcher() {
        let t = LumaThumb::from_rgb(&flat(64, 36, 200), 64, 36);
        assert_eq!(t.values().len(), THUMB_W * THUMB_H);
        assert!((t.values()[0] - 200.0).abs() < 1e-3);
        let mut half = flat(64, 36, 200);
        half[..64 * 18 * 3].iter_mut().for_each(|b| *b = 0);
        let u = LumaThumb::from_rgb(&half, 64, 36);
        assert_eq!(t.mean_change(&t.clone()), 0.0);
        assert!(t.mean_change(&u) > 50.0, "watcher 의 changed 와 같은 값 (위 절반이 검게)");
    }

    #[test]
    fn ps04_small_corner_modal_wakes_the_watcher_through_a_local_block() {
        // 640 × 360: 썸네일 칸 하나가 20 × 20 픽셀. 오른쪽 아래 구석에 화면의 4 % 인 128 × 72 로그인 창
        let (w, h) = (640, 360);
        let before = flat(w, h, 60);
        let mut after = before.clone();
        for y in h - 72..h {
            for x in w - 128..w {
                after[(y * w + x) * 3..(y * w + x) * 3 + 3].fill(110);
            }
        }
        let (a, b) = (LumaThumb::from_rgb(&before, w, h), LumaThumb::from_rgb(&after, w, h));
        let mean = a.mean_change(&b);
        assert!((mean - 2.0).abs() < 0.01, "전체 평균은 2/255 로 3/255 에 못 미친다: {mean}");
        assert!(a.max_block_change(&b, 3) >= 18.0);
        let rule = WakeRule::default();
        assert_eq!(rule.check(Some(&a), &b), Wake::Local);
        assert!(rule.check(Some(&a), &b).wakes());
        // 예전 규칙(전체 평균만)이면 건너뛰었을 프레임이다
        let mean_only = WakeRule {
            block_min: f32::INFINITY,
            ..rule
        };
        assert_eq!(mean_only.check(Some(&a), &b), Wake::Still);
    }

    #[test]
    fn global_changes_noise_and_wiped_thumbnails() {
        let (w, h) = (320, 180);
        let rule = WakeRule::default();
        let a = LumaThumb::from_rgb(&flat(w, h, 60), w, h);
        assert_eq!(rule.check(None, &a), Wake::First);
        assert_eq!(rule.check(Some(&a), &a.clone()), Wake::Still);
        // 화면 전체가 밝아짐
        assert_eq!(rule.check(Some(&a), &LumaThumb::from_rgb(&flat(w, h, 70), w, h)), Wake::Global);
        // 잔떨림(±1)은 그대로
        let noisy: Vec<u8> = (0..w * h * 3).map(|i| if (i / 3) % 2 == 0 { 61 } else { 59 }).collect();
        assert_eq!(rule.check(Some(&a), &LumaThumb::from_rgb(&noisy, w, h)), Wake::Still);
        // 지운 썸네일과는 견주지 않는다: 다음 프레임은 처음처럼 깨운다
        let mut gone = a.clone();
        gone.wipe();
        assert!(gone.values().is_empty());
        assert_eq!(rule.check(Some(&gone), &a), Wake::First);
        assert_eq!(gone.mean_change(&a), f32::INFINITY);
        assert_eq!(gone.max_block_change(&a, 3), f32::INFINITY);
    }
}
