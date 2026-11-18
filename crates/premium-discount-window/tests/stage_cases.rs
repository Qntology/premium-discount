//! `stage_cases.json`(web/desktop/stage.ts 를 돌려 만든 표)을 읽어 Rust 쪽이 같은 답을 내는지 본다.
//! 같은 표를 web/desktop/stage.test.ts 도 읽는다. 표를 다시 만들려면 `tests/gen_stage_cases.mjs`.

use premium_discount_window::{
    classify_pointer, grows_to, next_stage, place_window, Anchor, Position, Size, Stage,
    StageEvent, WorkArea, DOCK_MARGIN, DRAG_THRESHOLD, STAGE_SIZE,
};
use serde_json::Value;

fn cases() -> Value {
    let text = include_str!("stage_cases.json");
    serde_json::from_str(text).expect("stage_cases.json 은 JSON 이다")
}

fn list<'a>(all: &'a Value, key: &str) -> &'a [Value] {
    let v = all[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} 가 없다"));
    assert!(!v.is_empty(), "{key} 가 비었다");
    v
}

fn int(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("정수가 아니다: {v}"))
}

fn size(v: &Value) -> Size {
    Size::new(int(&v["width"]) as u32, int(&v["height"]) as u32)
}

fn stage(v: &Value) -> Stage {
    Stage::from_index(int(v) as u8).unwrap_or_else(|| panic!("단계가 아니다: {v}"))
}

#[test]
fn constants_match_typescript() {
    let all = cases();
    let c = &all["constants"];
    let sizes: Vec<Size> = c["stage_size"]
        .as_array()
        .unwrap()
        .iter()
        .map(size)
        .collect();
    assert_eq!(sizes, STAGE_SIZE.to_vec());
    assert_eq!(int(&c["dock_margin"]), DOCK_MARGIN as i64);
    assert_eq!(c["drag_threshold"].as_f64(), Some(DRAG_THRESHOLD));
}

#[test]
fn place_window_matches_typescript() {
    let all = cases();
    for c in list(&all, "place_window") {
        let a = &c["area"];
        let area = WorkArea {
            x: int(&a["x"]) as i32,
            y: int(&a["y"]) as i32,
            width: int(&a["width"]) as u32,
            height: int(&a["height"]) as u32,
        };
        let anchor = match &c["anchor"] {
            Value::Null => None,
            v => Some(Anchor {
                right: int(&v["right"]) as i32,
                bottom: int(&v["bottom"]) as i32,
            }),
        };
        let scale = c["scale"].as_f64().unwrap();
        let margin = int(&c["margin"]) as u32;
        let want = Position::new(
            int(&c["expected"]["x"]) as i32,
            int(&c["expected"]["y"]) as i32,
        );
        assert_eq!(
            place_window(size(&c["size"]), area, scale, anchor, margin),
            want,
            "{}",
            c["name"]
        );
    }
}

#[test]
fn next_stage_matches_typescript() {
    let all = cases();
    let rows = list(&all, "next_stage");
    assert_eq!(
        rows.len(),
        3 * 5 * 2,
        "단계 3 x 사건 5 x ready 2 를 모두 본다"
    );
    for c in rows {
        let event = StageEvent::parse(c["event"].as_str().unwrap())
            .unwrap_or_else(|| panic!("모르는 사건: {c}"));
        let ready = c["ready"].as_bool().unwrap();
        assert_eq!(
            next_stage(stage(&c["current"]), event, ready),
            stage(&c["expected"]),
            "{c}"
        );
    }
}

#[test]
fn classify_pointer_matches_typescript() {
    let all = cases();
    for c in list(&all, "classify_pointer") {
        let got = classify_pointer(
            c["dx"].as_f64().unwrap(),
            c["dy"].as_f64().unwrap(),
            c["threshold"].as_f64().unwrap(),
        );
        assert_eq!(got.as_str(), c["expected"].as_str().unwrap(), "{c}");
    }
}

#[test]
fn grows_to_matches_typescript() {
    let all = cases();
    for c in list(&all, "grows_to") {
        assert_eq!(
            grows_to(stage(&c["from"]), stage(&c["to"])),
            c["expected"].as_bool().unwrap(),
            "{c}"
        );
    }
}
