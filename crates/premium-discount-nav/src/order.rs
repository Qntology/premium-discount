//! 번호 매기기

use std::cmp::Ordering;
use std::collections::HashMap;

use premium_discount_verdict::Tier;
use serde::{Deserialize, Serialize};

use crate::region::Region;

/// 번호를 받는 영역 수 (키보드 1 ~ 9). 나머지는 번호 없이 목록 뒤에 남고, 목록 끝은 늘 "직접 입력"이다
pub const MAX_NUMBERED: usize = 9;

/// 읽는 순서의 줄 높이 (창 높이의 5 %): 중심이 같은 줄에 있으면 왼쪽부터 읽는다
const ROW: f32 = 0.05;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Numbered {
    /// 1 ~ 9. 앞의 9개 밖이면 None
    pub number: Option<u8>,
    /// 이 영역의 판정 등급 (판정이 없으면 None)
    pub tier: Option<Tier>,
    pub region: Region,
}

fn tier_rank(tier: Option<Tier>) -> u8 {
    match tier {
        Some(Tier::Premium) => 0,
        Some(Tier::Discount) => 2,
        _ => 1,
    }
}

fn order(a: &Numbered, b: &Numbered) -> Ordering {
    let ((ax, ay), (bx, by)) = (a.region.rect.center(), b.region.rect.center());
    tier_rank(a.tier)
        .cmp(&tier_rank(b.tier))
        .then(a.region.role.priority().cmp(&b.region.role.priority()))
        .then(((ay / ROW).floor() as i32).cmp(&((by / ROW).floor() as i32)))
        .then(ax.total_cmp(&bx))
}

/// 영역에 번호를 매긴다. `tiers` 는 위치 단서(`anchor`) → 판정 등급이다(로컬 판정, 서버 판정을 받았으면 그것).
/// premium 영역이 앞, discount 영역이 뒤로 가고, 같은 등급 안에서는 종류(입력 → 버튼 → 링크 → 메뉴 → 본문 → 그림 → 기타),
/// 그다음 읽는 순서(위 → 아래, 왼쪽 → 오른쪽)다. 앞의 [`MAX_NUMBERED`] 개가 번호 1 ~ 9 를 받는다.
pub fn number(regions: Vec<Region>, tiers: &HashMap<String, Tier>) -> Vec<Numbered> {
    let mut items: Vec<Numbered> = regions
        .into_iter()
        .map(|region| Numbered {
            number: None,
            tier: tiers.get(&region.anchor).copied(),
            region,
        })
        .collect();
    items.sort_by(order);
    for (i, item) in items.iter_mut().take(MAX_NUMBERED).enumerate() {
        item.number = Some(i as u8 + 1);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::region;
    use crate::{Role, Source};

    fn anchors(items: &[Numbered]) -> Vec<&str> {
        items.iter().map(|n| n.region.anchor.as_str()).collect()
    }

    #[test]
    fn premium_first_discount_last_then_role_then_reading_order() {
        let regions = vec![
            region("link-b", Role::Link, "도움말", (0.6, 0.02, 0.1, 0.03), Source::Tree),
            region("link-a", Role::Link, "공지", (0.4, 0.02, 0.1, 0.03), Source::Tree),
            region("button", Role::Button, "검색", (0.7, 0.2, 0.1, 0.05), Source::Tree),
            region("input", Role::Input, "검색어", (0.1, 0.2, 0.5, 0.05), Source::Tree),
            region("ad", Role::Image, "", (0.0, 0.5, 0.3, 0.3), Source::Vision),
            region("menu", Role::Menu, "메뉴", (0.0, 0.0, 0.1, 0.05), Source::Tree),
        ];
        let tiers = HashMap::from([("menu".to_string(), Tier::Premium), ("input".to_string(), Tier::Discount)]);
        let out = number(regions, &tiers);
        assert_eq!(anchors(&out), ["menu", "button", "link-a", "link-b", "ad", "input"]);
        assert_eq!(out.iter().map(|n| n.number).collect::<Vec<_>>(), (1..=6).map(Some).collect::<Vec<_>>());
        assert_eq!(out[0].tier, Some(Tier::Premium));
        assert_eq!(out[5].tier, Some(Tier::Discount));
    }

    #[test]
    fn only_nine_numbers() {
        let regions: Vec<Region> = (0..12)
            .map(|i| region(&format!("a{i:02}"), Role::Link, "", (0.0, i as f32 * 0.08, 0.1, 0.05), Source::Tree))
            .collect();
        let out = number(regions, &HashMap::new());
        assert_eq!(out.len(), 12);
        assert_eq!(out[8].number, Some(9));
        assert!(out[9..].iter().all(|n| n.number.is_none()));
        assert_eq!(anchors(&out)[..3], ["a00", "a01", "a02"]);
    }
}
