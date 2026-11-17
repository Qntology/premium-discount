//! 서버에 올리는 영역 지도 (`navmap/1`)

use serde::{Deserialize, Serialize};

use crate::privacy::{scrub_anchor, scrub_name};
use crate::region::{sensitive_screen, Rect, Region, Role};

pub const NAVMAP_SCHEMA: &str = "navmap/1";

/// 지도의 영역 하나: 종류 · 짧은 이름 · 창 기준 상대 좌표 · 위치 단서뿐이다
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapRegion {
    pub anchor: String,
    pub role: Role,
    pub name: String,
    pub rect: Rect,
}

/// 같은 화면에 처음 들어온 사용자가 받는 지도. 화면 내용(픽셀 · 글자 · 입력값 · 임베딩)은 없고 구조만 있다
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavMap {
    pub schema: String,
    /// 앱(`code.exe`) 또는 사이트(`example.com`)
    pub cc: String,
    /// 화면 키(`hashId`). 원문 URL · 창 제목은 들어오지 않는다
    pub screen_key: String,
    pub regions: Vec<MapRegion>,
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

impl NavMap {
    /// 영역에서 지도를 만든다. 이름 · 위치 단서는 개인정보를 거르고, 좌표는 소수 셋째 자리로 반올림한다.
    /// 바뀜 표시 · 확신 · 출처는 기기에만 남는다. 개인정보 화면이면 지도를 만들지 않는다(None)
    pub fn from_regions(cc: &str, screen_key: &str, regions: &[Region]) -> Option<NavMap> {
        if sensitive_screen(regions) {
            return None;
        }
        Some(NavMap {
            schema: NAVMAP_SCHEMA.to_string(),
            cc: cc.to_string(),
            screen_key: screen_key.to_string(),
            regions: regions
                .iter()
                .map(|r| MapRegion {
                    anchor: scrub_anchor(&r.anchor),
                    role: r.role,
                    name: scrub_name(&r.name),
                    rect: Rect {
                        x: round3(r.rect.x),
                        y: round3(r.rect.y),
                        w: round3(r.rect.w).max(0.001),
                        h: round3(r.rect.h).max(0.001),
                    },
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::region;
    use crate::Source;
    use serde_json::Value;

    #[test]
    fn the_map_keeps_structure_only() {
        let mut r = region("form#acct-98765432>input", Role::Input, "계좌 110-123-456789 입력", (0.12345, 0.5, 0.0004, 0.1), Source::Tree);
        r.changed = true;
        r.score = 0.7;
        let mut pw = region("form>input#pw", Role::Input, "비밀번호", (0.3, 0.4, 0.4, 0.05), Source::Tree);
        pw.sensitive = true;
        assert_eq!(NavMap::from_regions("bank.example", "0xabc", &[r.clone(), pw]), None, "개인정보 화면은 지도를 올리지 않는다");
        let map = NavMap::from_regions("bank.example", "0xabc", &[r]).unwrap();
        assert_eq!(map.schema, NAVMAP_SCHEMA);
        let m = &map.regions[0];
        assert_eq!(m.anchor, "form#acct-#>input");
        assert_eq!(m.name, "계좌 입력");
        assert_eq!((m.rect.x, m.rect.w), (0.123, 0.001));

        let v: Value = serde_json::to_value(&map).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 4);
        for k in ["schema", "cc", "screen_key", "regions"] {
            assert!(keys.contains(&k), "{k}");
        }
        let region_keys: Vec<&str> = v["regions"][0].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(region_keys.len(), 4, "바뀜 · 확신 · 출처는 지도에 없다: {region_keys:?}");
        assert_eq!(v["regions"][0]["role"], "input");
    }
}
