//! premium-discount-nav: 접근성 내비게이션의 코어. premium-discount 앱과 raon-zena 가 같은 규칙을 쓰도록 라이브러리로 둔다.
//!
//! 1. 화면에서 모은 후보 영역(접근성 트리 · 비전 · 이벤트 카메라)을 하나로 합치고([`merge`]), 판정 등급을 반영해
//!    번호 1 ~ 9 를 매긴다([`number`]).
//! 2. 행동 기록(번호 선택 · 화면 변화 · form change)을 에피소드로 자르고, SDS 로 premium · discount 를 결정한다
//!    ([`split_episodes`], [`FrictionBaselines::decide`]). 늘 결정할 수는 없다: 띠 안이면 결정하지 않는다.
//! 3. 결정이 난 것만 결정 기록([`NavMark`], `navmark/1`)이 된다: 해시 · 벡터들 · 종류 + 날짜. 계정 · IP · 원시 이벤트 ·
//!    입력값 · 화면 글자가 없고, 기기에만 쌓는다(게스트도). 해시는 서버와 같은 `hashId` 다([`hash_id`]).
//! 4. 시작부터 결승까지의 플로우를 타임트라이얼(`trial/1`)로 기록한다: 코스 = 체크포인트(화면 · 영역 해시) 10개까지,
//!    런 = 구간 시간 + SDS 결정, 앱 · 웹마다 코스 10개의 콜렉션(같은 코스는 횟수로), 진행 중에는 내 최고 기록 ·
//!    다른 사용자의 기록을 고스트로 안내한다([`Run`], [`Collection`], [`Collection::guide`]). 서버로는 런만 올린다.
//!    체크포인트는 앱이면 화면(UIA 구조) · 영역, 웹이면 페이지(링크값, [`page_hash`]) · 요소(DOM 셀렉터, [`selector_template`],
//!    [`element_hash`])다. 스와이프 · 스크롤로 지난 체크포인트에는 제스처가 붙는다([`classify_stroke`], [`scroll_bursts`]).
//! 5. 같은 화면에 처음 들어온 사용자를 위한 영역 지도([`NavMap`], `navmap/1`)도 만든다. 이름은 개인정보를 거른다([`scrub_name`]).
//! 6. 개인정보 화면(비밀번호 · 결제 칸, [`sensitive_screen`])은 트리로만 번호를 매기고 기록 · 지도 · 체크포인트를 만들지 않는다.
//!
//! 모델(SigLIP2 · granite) · 저장소(LanceDB) · HTTP 에 의존하지 않는다. 벡터는 앱이 임베딩해서 넘긴다.

mod behavior;
mod dom;
mod gesture;
mod hash;
mod mark;
mod navmap;
mod order;
mod privacy;
mod region;
mod trial;

pub use behavior::{split_episodes, BehaviorParams, Decision, Episode, Event, FrictionBaselines, Judgement, Signal, Welford};
pub use dom::{selector_template, DomNode, SELECTOR_DEPTH};
pub use gesture::{classify_stroke, rage_presses, scroll_bursts, Gesture, GestureParams, PointerKind, PointerSample, ScrollBurst, ScrollSample, Stroke};
pub use hash::{app_hash, element_hash, hash_id, hash_message, page_hash, region_hash, screen_hash, structure_template, url_template};
pub use mark::{region_label, screen_label, MarkError, MarkType, MarkVector, NavMark, LABEL_SPACE, MARK_SCHEMA};
pub use navmap::{MapRegion, NavMap, NAVMAP_SCHEMA};
pub use order::{number, Numbered, MAX_NUMBERED};
pub use privacy::{scrub_anchor, scrub_name, NAME_MAX_CHARS};
pub use region::{merge, sensitive_screen, Rect, Region, Role, Source, MERGE_IOU};
pub use trial::{
    checkpoint_id, course_chain, course_hash, route_marks, tier, Added, Checkpoint, CheckpointTally, Collection, CourseEntry, Guide, PublicCourse, Run, Step, StepKind, StepRef, TrialError, MAX_COURSES,
    MAX_RUN_MS, MAX_STEPS, SPLIT_UNIT_MS, TIER_MIN_DECIDED, TRIAL_SCHEMA,
};

#[cfg(test)]
pub(crate) mod testing {
    use crate::{Rect, Region, Role, Source};

    pub fn region(anchor: &str, role: Role, name: &str, rect: (f32, f32, f32, f32), source: Source) -> Region {
        Region {
            anchor: anchor.into(),
            role,
            name: name.into(),
            rect: Rect::new(rect.0, rect.1, rect.2, rect.3).unwrap(),
            source,
            score: 1.0,
            changed: false,
            sensitive: false,
        }
    }
}
