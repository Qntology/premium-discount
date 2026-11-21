//! 후보 영역과 합치기

use serde::{Deserialize, Serialize};

/// 겹친 후보를 하나로 보는 IoU (noto-sandbox 의 타일 스캔 NMS 와 같은 값)
pub const MERGE_IOU: f32 = 0.4;
/// 이벤트 후보의 넓이 중 이만큼 이상이 트리 · 비전 영역 안에 들었으면 따로 남기지 않고 그 영역이 바뀐 것으로 본다
/// (큰 카드 안의 작은 숫자만 바뀐 경우, [`merge_absorbing`])
pub const CONTAIN_ABSORB: f32 = 0.70;

/// 영역의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Input,
    Button,
    Link,
    Menu,
    Text,
    Image,
    Other,
}

impl Role {
    /// 같은 등급 안에서 앞 번호를 받는 순서 (입력 → 버튼 → 링크 → 메뉴 → 본문 → 그림 → 기타)
    pub(crate) fn priority(self) -> u8 {
        match self {
            Role::Input => 0,
            Role::Button => 1,
            Role::Link => 2,
            Role::Menu => 3,
            Role::Text => 4,
            Role::Image => 5,
            Role::Other => 6,
        }
    }

    /// 화면에 보이는 종류 이름
    pub fn label(self) -> &'static str {
        match self {
            Role::Input => "입력창",
            Role::Button => "버튼",
            Role::Link => "링크",
            Role::Menu => "메뉴",
            Role::Text => "본문",
            Role::Image => "그림",
            Role::Other => "영역",
        }
    }
}

/// 후보를 낸 곳. 겹치면 트리 > 비전 > 이벤트 순으로 남긴다
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// 접근성 트리 (웹 DOM · ARIA, 앱 UIA): 역할과 이름을 안다
    Tree,
    /// 비전 (SigLIP2 앵커 + 잉크): 종류와 경계만 안다
    Vision,
    /// 이벤트 카메라 · form change: 바뀐 곳만 안다
    Event,
}

impl Source {
    fn rank(self) -> u8 {
        match self {
            Source::Tree => 0,
            Source::Vision => 1,
            Source::Event => 2,
        }
    }
}

/// 창 기준 상대 좌표 (0 ~ 1)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// 창 밖으로 나간 부분은 잘라 낸다. 넓이가 없으면 None
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Option<Rect> {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return None;
        }
        let (x0, y0) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        let (x1, y1) = ((x + w).clamp(0.0, 1.0), (y + h).clamp(0.0, 1.0));
        (x1 > x0 && y1 > y0).then(|| Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    fn overlap(&self, other: &Rect) -> f32 {
        let ix = ((self.x + self.w).min(other.x + other.w) - self.x.max(other.x)).max(0.0);
        let iy = ((self.y + self.h).min(other.y + other.h) - self.y.max(other.y)).max(0.0);
        ix * iy
    }

    pub fn iou(&self, other: &Rect) -> f32 {
        let inter = self.overlap(other);
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }

    /// 이 사각형의 넓이 중 `outer` 안에 든 몫 (0 ~ 1). IoU 와 달리 작은 사각형이 큰 사각형 안에 들면 1 이다
    pub fn containment(&self, outer: &Rect) -> f32 {
        let area = self.area();
        if area <= 0.0 {
            0.0
        } else {
            self.overlap(outer) / area
        }
    }

    /// 두 중심 사이의 거리 (창 기준)
    pub fn center_distance(&self, other: &Rect) -> f32 {
        let ((ax, ay), (bx, by)) = (self.center(), other.center());
        (ax - bx).hypot(ay - by)
    }
}

/// 후보 영역 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    /// 위치 단서: 웹은 ARIA 경로, 앱은 UIA AutomationId, 비전은 격자 칸. 같은 화면에서 같은 영역이면 같은 값
    pub anchor: String,
    pub role: Role,
    /// 트리가 준 이름. 비전 · 이벤트 후보는 비어 있다
    pub name: String,
    pub rect: Rect,
    pub source: Source,
    /// 비전 · 이벤트 후보의 확신 (트리는 1)
    pub score: f32,
    /// 이벤트 카메라 · form change 가 이 영역이 바뀌었다고 알렸다
    #[serde(default)]
    pub changed: bool,
    /// 개인정보 칸: 비밀번호 입력, 결제 정보 칸 등 (트리가 알려 준다. 예: `type=password`, `autocomplete="cc-*"`)
    #[serde(default)]
    pub sensitive: bool,
}

/// 개인정보 화면인가: 비밀번호 · 결제 정보 칸이 하나라도 있으면 그렇다. 이런 화면은 접근성 트리로만 번호를 매기고,
/// 비전 · 클라우드 결정 · 행동 기록 · 지도 올리기를 하지 않는다([`merge`] 와 [`crate::NavMap::from_regions`] 가 지킨다)
pub fn sensitive_screen(regions: &[Region]) -> bool {
    regions.iter().any(|r| r.sensitive)
}

impl Region {
    /// 번호 목록에 보일 이름. 트리가 이름을 주지 않았으면 "오른쪽 위 버튼"처럼 위치와 종류로 붙인다
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        format!("{} {}", position_words(&self.rect), self.role.label())
    }
}

/// 영역 중심이 창의 어느 쪽인지 (가로 · 세로를 셋으로 나눔)
fn position_words(rect: &Rect) -> &'static str {
    let (cx, cy) = rect.center();
    let col = if cx < 1.0 / 3.0 { 0 } else if cx > 2.0 / 3.0 { 2 } else { 1 };
    let row = if cy < 1.0 / 3.0 { 0 } else if cy > 2.0 / 3.0 { 2 } else { 1 };
    match (row, col) {
        (0, 0) => "왼쪽 위",
        (0, 1) => "위",
        (0, _) => "오른쪽 위",
        (1, 0) => "왼쪽",
        (1, 1) => "가운데",
        (1, _) => "오른쪽",
        (_, 0) => "왼쪽 아래",
        (_, 1) => "아래",
        _ => "오른쪽 아래",
    }
}

/// 후보를 하나로 합친다. 겹친(IoU ≥ `iou`) 후보는 트리 > 비전 > 이벤트, 같은 출처면 확신이 높은 것 하나만 남긴다.
/// 이벤트 후보는 남은 영역과 겹치면 그 영역에 `changed` 만 표시하고, 겹치는 영역이 없을 때만 "바뀐 영역"으로 남는다.
/// 개인정보 화면([`sensitive_screen`])이면 트리 후보만 남긴다. 결과는 입력 순서를 따른다.
pub fn merge(candidates: Vec<Region>, iou: f32) -> Vec<Region> {
    merge_with(candidates, iou, None)
}

/// [`merge`] 에 포함률 흡수를 더한 것: 어떤 영역과도 IoU 로 겹치지 않는 이벤트 후보라도 넓이의 `contain` 몫 이상이 남은 트리 · 비전
/// 영역 안에 들었으면 "바뀐 영역"으로 따로 남기지 않고 그 영역(여럿이면 가장 작은 것)에 `changed` 만 표시한다. 큰 카드 안의 작은
/// 숫자만 바뀌어도 카드가 바뀐 것으로 보이고, 번호가 조각나지 않는다. 무점멸 번호([`crate::StickyNumberer`])가 쓴다
pub fn merge_absorbing(candidates: Vec<Region>, iou: f32, contain: f32) -> Vec<Region> {
    merge_with(candidates, iou, Some(contain))
}

fn merge_with(candidates: Vec<Region>, iou: f32, contain: Option<f32>) -> Vec<Region> {
    let candidates = if sensitive_screen(&candidates) {
        candidates.into_iter().filter(|c| c.source == Source::Tree).collect()
    } else {
        candidates
    };
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let (ra, rb) = (&candidates[a], &candidates[b]);
        ra.source
            .rank()
            .cmp(&rb.source.rank())
            .then(rb.score.total_cmp(&ra.score))
            .then(a.cmp(&b))
    });
    let mut kept: Vec<usize> = Vec::new();
    let mut changed = vec![false; candidates.len()];
    for i in order {
        let c = &candidates[i];
        let overlap: Vec<usize> = kept.iter().copied().filter(|&k| candidates[k].rect.iou(&c.rect) >= iou).collect();
        // 이벤트는 트리 · 비전 다음에 오므로 이때 남은 영역은 모두 정해져 있다
        let parent = contain.filter(|_| overlap.is_empty() && c.source == Source::Event).and_then(|contain| {
            kept.iter()
                .copied()
                .filter(|&k| candidates[k].source != Source::Event && c.rect.containment(&candidates[k].rect) >= contain)
                .min_by(|&a, &b| candidates[a].rect.area().total_cmp(&candidates[b].rect.area()))
        });
        if let Some(k) = parent {
            changed[k] = true;
        } else if overlap.is_empty() {
            kept.push(i);
            changed[i] |= c.changed || c.source == Source::Event;
        } else if c.source == Source::Event || c.changed {
            for k in overlap {
                changed[k] = true;
            }
        }
    }
    kept.sort_unstable();
    kept.into_iter()
        .map(|i| {
            let mut r = candidates[i].clone();
            r.changed = changed[i];
            if r.source == Source::Event {
                r.role = Role::Other;
            }
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::region;

    #[test]
    fn rects_are_clipped_to_the_window() {
        let r = Rect::new(0.9, -0.1, 0.3, 0.3).unwrap();
        assert_eq!((r.x, r.y), (0.9, 0.0));
        assert!((r.w - 0.1).abs() < 1e-6 && (r.h - 0.2).abs() < 1e-6);
        assert!(Rect::new(1.2, 0.0, 0.1, 0.1).is_none());
        assert!(Rect::new(0.0, 0.0, 0.0, 0.5).is_none());
        assert!(Rect::new(f32::NAN, 0.0, 0.1, 0.1).is_none());
        let a = Rect::new(0.0, 0.0, 0.2, 0.2).unwrap();
        let b = Rect::new(0.1, 0.0, 0.2, 0.2).unwrap();
        assert!((a.iou(&b) - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(a.iou(&Rect::new(0.5, 0.5, 0.1, 0.1).unwrap()), 0.0);
    }

    #[test]
    fn the_tree_wins_and_events_only_mark_changes() {
        let tree = region("form>input#q", Role::Input, "검색어", (0.1, 0.1, 0.4, 0.05), Source::Tree);
        let mut vision = region("cell:3,1", Role::Button, "", (0.12, 0.1, 0.38, 0.05), Source::Vision);
        vision.score = 0.9;
        let event_on_tree = region("roi:0", Role::Other, "", (0.1, 0.1, 0.4, 0.05), Source::Event);
        let lone_vision = region("cell:8,8", Role::Image, "", (0.7, 0.7, 0.2, 0.2), Source::Vision);
        let lone_event = region("roi:1", Role::Button, "", (0.1, 0.8, 0.1, 0.1), Source::Event);
        let out = merge(vec![vision, tree.clone(), event_on_tree, lone_vision.clone(), lone_event], MERGE_IOU);
        assert_eq!(out.len(), 3, "{out:#?}");
        assert_eq!(out[0].anchor, tree.anchor);
        assert!(out[0].changed, "트리 영역 위의 이벤트는 바뀜 표시만 남긴다");
        assert_eq!(out[1].anchor, lone_vision.anchor);
        assert!(!out[1].changed);
        assert_eq!((out[2].anchor.as_str(), out[2].role, out[2].changed), ("roi:1", Role::Other, true));
    }

    #[test]
    fn privacy_screens_keep_tree_candidates_only() {
        let mut password = region("form>input#pw", Role::Input, "비밀번호", (0.3, 0.4, 0.4, 0.05), Source::Tree);
        password.sensitive = true;
        let login = region("form>button", Role::Button, "로그인", (0.3, 0.5, 0.2, 0.05), Source::Tree);
        let vision = region("cell:2,7", Role::Image, "", (0.0, 0.0, 0.2, 0.2), Source::Vision);
        let event = region("roi:0", Role::Other, "", (0.8, 0.8, 0.1, 0.1), Source::Event);
        let all = vec![password, login, vision, event];
        assert!(sensitive_screen(&all));
        let out = merge(all, MERGE_IOU);
        assert_eq!(out.iter().map(|r| r.anchor.as_str()).collect::<Vec<_>>(), ["form>input#pw", "form>button"]);
        assert!(!sensitive_screen(&[region("a", Role::Link, "", (0.0, 0.0, 0.1, 0.1), Source::Tree)]));
    }

    #[test]
    fn among_vision_candidates_the_surer_one_stays() {
        let mut a = region("cell:1,1", Role::Button, "", (0.1, 0.1, 0.2, 0.1), Source::Vision);
        let mut b = region("cell:1,2", Role::Link, "", (0.11, 0.1, 0.2, 0.1), Source::Vision);
        a.score = 0.4;
        b.score = 0.8;
        let out = merge(vec![a, b], MERGE_IOU);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, Role::Link);
    }

    #[test]
    fn containment_is_the_share_of_the_inner_rect() {
        let card = Rect::new(0.1, 0.1, 0.4, 0.3).unwrap();
        let digit = Rect::new(0.2, 0.2, 0.05, 0.03).unwrap();
        assert!((digit.containment(&card) - 1.0).abs() < 1e-5);
        assert!(digit.iou(&card) < MERGE_IOU, "IoU 로는 겹친 것으로 보지 않는다");
        let edge = Rect::new(0.45, 0.2, 0.1, 0.05).unwrap();
        assert!((edge.containment(&card) - 0.5).abs() < 1e-5);
        assert_eq!(card.containment(&Rect::new(0.8, 0.8, 0.1, 0.1).unwrap()), 0.0);
        assert!((digit.center_distance(&Rect::new(0.25, 0.2, 0.05, 0.03).unwrap()) - 0.05).abs() < 1e-6);
    }

    #[test]
    fn contained_events_mark_the_smallest_parent_changed() {
        let card = region("main>section.card", Role::Text, "주문 요약", (0.1, 0.1, 0.5, 0.4), Source::Tree);
        let button = region("main>section.card>button", Role::Button, "결제", (0.15, 0.35, 0.2, 0.1), Source::Tree);
        let vision = region("cell:7,1", Role::Image, "", (0.7, 0.1, 0.2, 0.2), Source::Vision);
        // 카드 안 버튼의 숫자만 바뀌었다 · 비전 그림 안에서 바뀌었다 · 빈 배경에서 바뀌었다
        let in_button = region("roi:0", Role::Other, "", (0.16, 0.36, 0.05, 0.04), Source::Event);
        let in_vision = region("roi:1", Role::Other, "", (0.75, 0.15, 0.05, 0.05), Source::Event);
        let lone = region("roi:2", Role::Other, "", (0.1, 0.8, 0.05, 0.05), Source::Event);
        let all = vec![card.clone(), button.clone(), vision.clone(), in_button.clone(), in_vision.clone(), lone.clone()];
        let out = merge_absorbing(all.clone(), MERGE_IOU, CONTAIN_ABSORB);
        let anchors: Vec<&str> = out.iter().map(|r| r.anchor.as_str()).collect();
        assert_eq!(anchors, [card.anchor.as_str(), button.anchor.as_str(), vision.anchor.as_str(), "roi:2"]);
        assert_eq!(out.iter().map(|r| r.changed).collect::<Vec<_>>(), [false, true, true, true], "가장 작은 부모(버튼)만 바뀜 표시");
        // 포함률 흡수가 없는 merge 는 예전처럼 작은 이벤트를 따로 남긴다
        assert_eq!(merge(all, MERGE_IOU).len(), 6);
    }

    #[test]
    fn unnamed_regions_are_named_by_place_and_role() {
        let r = region("cell:9,0", Role::Button, "  ", (0.8, 0.02, 0.1, 0.05), Source::Vision);
        assert_eq!(r.display_name(), "오른쪽 위 버튼");
        let r = region("cell:4,4", Role::Image, "", (0.4, 0.4, 0.2, 0.2), Source::Vision);
        assert_eq!(r.display_name(), "가운데 그림");
        let r = region("nav>a", Role::Link, "로그인", (0.0, 0.9, 0.1, 0.05), Source::Tree);
        assert_eq!(r.display_name(), "로그인");
    }
}
