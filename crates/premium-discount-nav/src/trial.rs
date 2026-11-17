//! 타임트라이얼 (`trial/1`): 사용자의 플로우를 레이싱 게임의 타임트라이얼처럼 기록하고, 다음에 같은 길을 갈 때 고스트로 안내한다
//!
//! | 레이싱 | 접근성 내비게이션 |
//! | --- | --- |
//! | 코스 | 한 앱 · 웹 안에서 시작부터 종료까지 지난 체크포인트의 순서 ([`course_hash`]). 앱은 화면(UIA 구조) · 영역, 웹은 페이지(링크값) · 요소(DOM 셀렉터)이고, 스와이프 · 스크롤이면 제스처가 붙는다 |
//! | 런(한 바퀴) | 코스를 한 번 달린 기록: 체크포인트마다 시작부터의 구간 시간과 그 에피소드의 SDS 결정 ([`Run`]) |
//! | 기록실 | 앱 · 웹마다 코스 10개 ([`Collection`]). 같은 코스를 다시 달리면 횟수(중복 카운트)가 는다 |
//! | 고스트 | 내 최고 기록, 다른 사용자의 최고 기록 · 평균. 진행 중에 다음 체크포인트와 앞섬 · 뒤짐을 알려 준다 ([`Collection::guide`]) |
//!
//! - **id**: 웹의 `hashId(cc + pathname)` 처럼 앞의 id 에 다음 조각을 이어 붙여 한 번 해싱한다(구분자 없음).
//!   체크포인트 id = `hashId(<종류> + <해시> + <제스처>)` ([`checkpoint_id`]). 코스 id 는 체인이다: 첫 체크포인트에서
//!   `hashId(<앱 id> + <체크포인트 id>)`, 다음부터 `hashId(<이전 코스 id> + <체크포인트 id>)`, 결승의 값이 그 코스의 id 다
//!   ([`course_chain`], [`course_hash`]). 앞부분이 같은 코스끼리는 그 지점까지의 id 를 함께 갖고, 다른 체크포인트가 나오는
//!   순간 새 id 로 갈라지므로 같은 코스가 두 번 등록되지 않는다. 체크포인트마다 그 지점까지의 코스 id 가 붙는다(`Step::course`).
//! - 플로우 길이는 체크포인트 10개까지다([`MAX_STEPS`]). 더 길면 오래된 체크포인트부터 버리고 끝 쪽 10개로 코스를 만든다.
//! - 콜렉션은 앱 · 웹마다 코스 10개까지다([`MAX_COURSES`]). 새 코스가 11번째가 되면 가장 오래 달리지 않은 코스 자리를 덮어쓴다.
//! - 런의 종류(premium · discount)는 시간이 아니라 그 에피소드의 SDS 결정이다([`crate::FrictionBaselines::decide`]).
//!   코스의 등급은 결정이 난 런의 수로 정한다([`tier`]).
//! - 다른 사용자의 코스(고스트)를 따라 달려 같은 결승에 닿으면, 그 코스의 중간 체크포인트 중 따라간 것은 premium,
//!   고르지 않은(다른 길로 간) 것은 discount 다([`route_marks`]). 체크포인트마다의 이 결정이 영역 · 화면의 등급이 되고,
//!   번호 순서(premium 영역이 앞 번호)에 쓰인다. 결승이 다르면 목적이 다른 것이라 판정하지 않는다.
//! - 끝까지 간(성공한) 에피소드만 런이 된다. 벽시계 시각은 기기를 떠나지 않는다: 런에는 시작부터의 경과 시간(100 ms 단위)과
//!   결승 날짜만 있다. 개인정보 화면(`sensitive_screen`)은 체크포인트로 넣지 않는다.

use std::collections::BTreeMap;
use std::fmt;

use premium_discount_verdict::{Tier, RECIPE_V2};
use serde::{Deserialize, Serialize};

use crate::gesture::Gesture;
use crate::hash::hash_id;
use crate::mark::{normalized, MarkError, MarkType, MarkVector};

pub const TRIAL_SCHEMA: &str = "trial/1";
/// 플로우(코스) 하나의 체크포인트 수 상한
pub const MAX_STEPS: usize = 10;
/// 앱 · 웹마다 콜렉션에 두는 코스 수 상한
pub const MAX_COURSES: usize = 10;
/// 구간 시간의 단위. 이 단위로 반올림해 기기 · 사람마다의 미세한 시간 차이를 지운다
pub const SPLIT_UNIT_MS: u32 = 100;
/// 런 하나의 최대 길이 (에피소드는 5분 넘는 간격에서 잘리므로 10 체크포인트면 이 안에 든다)
pub const MAX_RUN_MS: u32 = 3_600_000;
/// 코스 등급을 매기려면 결정이 난 런이 이만큼 있어야 한다
pub const TIER_MIN_DECIDED: u32 = 3;

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    /// 앱: 화면에 들어왔다 ([`crate::screen_hash`], UIA 구조 지문)
    Screen,
    /// 앱: 영역(번호)을 골랐다 ([`crate::region_hash`])
    Region,
    /// 웹: 페이지에 들어왔다. 링크값(URL 틀, 해시 라우팅 포함)으로 정한다 ([`crate::page_hash`])
    Page,
    /// 웹: 요소를 다뤘다. DOM 셀렉터 틀로 정한다 ([`crate::element_hash`], [`crate::selector_template`])
    Element,
}

impl StepKind {
    pub fn name(self) -> &'static str {
        match self {
            StepKind::Screen => "screen",
            StepKind::Region => "region",
            StepKind::Page => "page",
            StepKind::Element => "element",
        }
    }
}

/// 누름(클릭 · 탭)은 기본 동작이라 체크포인트에 붙이지 않는다. 그래서 마우스로 누른 사람과 손가락으로 누른 사람이 같은 코스가 된다
fn significant(gesture: Option<Gesture>) -> Option<Gesture> {
    gesture.filter(|g| *g != Gesture::Press)
}

/// 코스의 체크포인트 하나. 스와이프 · 스크롤로 지났으면 `gesture` 가 붙는다(같은 해시라도 누름과 다른 체크포인트다)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepRef {
    pub kind: StepKind,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<Gesture>,
}

impl StepRef {
    /// 사람이 읽는 체크포인트 이름: `<종류>:<해시>` 또는 `<종류>:<해시>@<제스처>` (같은 체크포인트인지 견줄 때 쓴다)
    pub fn key(&self) -> String {
        let hash = self.hash.trim().to_ascii_lowercase();
        match significant(self.gesture) {
            Some(g) => format!("{}:{hash}@{}", self.kind.name(), g.name()),
            None => format!("{}:{hash}", self.kind.name()),
        }
    }

    /// 체크포인트 id ([`checkpoint_id`])
    pub fn id(&self) -> Option<String> {
        checkpoint_id(self)
    }
}

/// 체크포인트 id = `hashId(<종류> + <해시> + <제스처>)` (예: `region0x2222…`, `page0xaaaa…scroll-down`). 해시가 0x + 40자로
/// 길이가 정해져 있어 구분자 없이 이어도 겹치지 않는다. 해시가 앱 · 사이트를 담고 있으므로 다른 앱의 체크포인트와도 겹치지 않는다.
/// 고스트 따라가기 판정의 수(nav_counts)도 이 id 로 센다. 서버도 같은 글로 계산한다
pub fn checkpoint_id(step: &StepRef) -> Option<String> {
    let gesture = significant(step.gesture).map_or("", Gesture::name);
    hash_id(&format!("{}{}{gesture}", step.kind.name(), step.hash.trim().to_ascii_lowercase()))
}

/// 기기 안의 체크포인트: 화면 · 페이지에 들어왔거나, 영역 · 요소를 다뤘다. `at_ms` 는 기기 시계이고 기기를 떠나지 않는다.
/// `gesture` 는 [`crate::classify_stroke`] · [`crate::scroll_bursts`] 로 가른 것이다(없으면 이동 · 누름)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub at_ms: i64,
    pub kind: StepKind,
    pub hash: String,
    #[serde(default)]
    pub gesture: Option<Gesture>,
}

/// 런의 체크포인트: 시작부터의 구간 시간과, 있으면 그 화면 · 영역의 벡터(라벨 글 granite · 영역 조각 비전)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// 체크포인트 id ([`checkpoint_id`]). 서버가 다시 계산해 맞춰 본다
    pub id: String,
    /// 시작부터 이 체크포인트까지의 코스 id ([`course_chain`]). 마지막 체크포인트의 값이 런의 코스 id 다
    pub course: String,
    pub kind: StepKind,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<Gesture>,
    pub split_ms: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vectors: Vec<MarkVector>,
}

/// 코스를 한 번 달린 기록. 기기에 쌓고, 인증한 뒤에만 서버로 올린다(`POST /nav/trials`)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub schema: String,
    /// 앱 · 웹 해시 ([`crate::app_hash`])
    pub app: String,
    /// 코스 해시 ([`course_hash`]). 서버는 체크포인트로 다시 계산해 맞춰 본다
    pub course: String,
    pub steps: Vec<Step>,
    /// 결승까지 걸린 시간 (= 마지막 체크포인트의 구간 시간)
    pub total_ms: u32,
    /// 이 런(에피소드)의 SDS 결정. 띠 안이거나 기준선이 없으면 None
    #[serde(rename = "type")]
    pub kind: Option<MarkType>,
    /// 결승 날짜 (UTC, 1970-01-01 부터 센 날)
    pub day: i64,
    pub recipe: String,
    /// 이 런이 따라간 다른 사용자의 코스(고스트, 공개 코스의 해시). 서버는 이 코스와 견줘 체크포인트를 판정한다([`route_marks`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ghost: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialError {
    /// 앱 · 체크포인트 해시가 `0x` + hex 40자가 아니다
    Hash,
    /// 체크포인트가 둘(시작 · 결승)보다 적다
    TooShort,
    /// 체크포인트가 시간순이 아니다
    Order,
    /// [`MAX_RUN_MS`] 를 넘는다
    TooLong,
}

impl fmt::Display for TrialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrialError::Hash => "app and checkpoint hashes must be addresses (0x + 40 hex)",
            TrialError::TooShort => "a run needs a start and a finish checkpoint",
            TrialError::Order => "checkpoints must be in time order",
            TrialError::TooLong => "a run must finish within an hour",
        })
    }
}

impl std::error::Error for TrialError {}

fn address(hash: &str) -> Option<String> {
    let hash = hash.trim().to_ascii_lowercase();
    (hash.len() == 42 && hash.starts_with("0x") && hash[2..].chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

fn round_split(ms: i64) -> u32 {
    let unit = SPLIT_UNIT_MS as i64;
    (((ms.max(0) + unit / 2) / unit) * unit) as u32
}

/// 코스 id 체인: 체크포인트마다 그 지점까지의 코스 id. 첫 값은 `hashId(<앱 id> + <체크포인트 id>)`, 다음부터는
/// `hashId(<이전 코스 id> + <체크포인트 id>)` 다. 앞부분이 같은 코스는 그 지점까지 같은 id 를 갖고, 체크포인트 하나 · 순서 하나만
/// 달라도 그 뒤로는 다른 id 가 된다. 서버(`www/src/navtrials.ts`)도 같은 글로 다시 계산한다
pub fn course_chain(app: &str, steps: &[StepRef]) -> Option<Vec<String>> {
    let mut previous = app.trim().to_ascii_lowercase();
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        previous = hash_id(&format!("{previous}{}", checkpoint_id(step)?))?;
        out.push(previous.clone());
    }
    Some(out)
}

/// 코스 id: 코스 id 체인([`course_chain`])의 마지막 값(결승까지의 코스 id). 서버는 이 id 가 없으면 새 코스를 만든다
pub fn course_hash(app: &str, steps: &[StepRef]) -> Option<String> {
    course_chain(app, steps)?.pop()
}

/// 체크포인트를 코스 순서로 고른다: 시간순인지 보고, 잇달아 같은 체크포인트는 처음 도착한 것 하나로, 끝 쪽 [`MAX_STEPS`] 개만
fn course_points(checkpoints: &[Checkpoint]) -> Result<Vec<(i64, StepRef)>, TrialError> {
    let mut out: Vec<(i64, StepRef)> = Vec::new();
    let mut prev = i64::MIN;
    for c in checkpoints {
        if c.at_ms < prev {
            return Err(TrialError::Order);
        }
        prev = c.at_ms;
        let step = StepRef {
            kind: c.kind,
            hash: address(&c.hash).ok_or(TrialError::Hash)?,
            gesture: significant(c.gesture),
        };
        if out.last().is_some_and(|(_, last)| *last == step) {
            continue;
        }
        out.push((c.at_ms, step));
    }
    if out.len() > MAX_STEPS {
        out.drain(..out.len() - MAX_STEPS);
    }
    Ok(out)
}

impl Run {
    /// 성공한 에피소드의 체크포인트로 런을 만든다. `kind` 는 그 에피소드의 SDS 결정이다
    pub fn from_checkpoints(app: &str, checkpoints: &[Checkpoint], kind: Option<MarkType>) -> Result<Run, TrialError> {
        let app = address(app).ok_or(TrialError::Hash)?;
        let points = course_points(checkpoints)?;
        let (Some((start, _)), Some((finish, _))) = (points.first(), points.last()) else {
            return Err(TrialError::TooShort);
        };
        if points.len() < 2 {
            return Err(TrialError::TooShort);
        }
        if finish - start > MAX_RUN_MS as i64 {
            return Err(TrialError::TooLong);
        }
        let refs: Vec<StepRef> = points.iter().map(|(_, s)| s.clone()).collect();
        let chain = course_chain(&app, &refs).ok_or(TrialError::Hash)?;
        let steps: Vec<Step> = points
            .iter()
            .zip(&chain)
            .map(|((at, s), course)| Step {
                id: s.id().unwrap_or_default(),
                course: course.clone(),
                kind: s.kind,
                hash: s.hash.clone(),
                gesture: s.gesture,
                split_ms: round_split(at - start),
                vectors: Vec::new(),
            })
            .collect();
        Ok(Run {
            schema: TRIAL_SCHEMA.to_string(),
            course: chain.last().cloned().ok_or(TrialError::TooShort)?,
            app,
            total_ms: steps.last().map_or(0, |s| s.split_ms),
            steps,
            kind,
            day: finish.div_euclid(DAY_MS),
            recipe: RECIPE_V2.to_string(),
            ghost: None,
        })
    }

    /// 체크포인트 하나에 벡터를 붙인다 (L2 정규화). 공간 이름이 비었거나 겹치면 [`MarkError::Space`]
    pub fn attach_vectors(&mut self, step: usize, vectors: &[(&str, &[f32])]) -> Result<(), MarkError> {
        let target = self.steps.get_mut(step).ok_or(MarkError::Vector)?;
        let mut out: Vec<MarkVector> = Vec::with_capacity(vectors.len());
        for (space, vector) in vectors {
            let space = space.trim();
            if space.is_empty() || out.iter().any(|v| v.space == space) {
                return Err(MarkError::Space);
            }
            out.push(MarkVector {
                space: space.to_string(),
                vector: normalized(vector).ok_or(MarkError::Vector)?,
            });
        }
        target.vectors = out;
        Ok(())
    }

    pub fn refs(&self) -> Vec<StepRef> {
        self.steps
            .iter()
            .map(|s| StepRef {
                kind: s.kind,
                hash: s.hash.clone(),
                gesture: significant(s.gesture),
            })
            .collect()
    }

    pub fn splits(&self) -> Vec<u32> {
        self.steps.iter().map(|s| s.split_ms).collect()
    }
}

/// 코스의 등급: 결정이 난 런(premium + discount)이 [`TIER_MIN_DECIDED`] 개보다 적으면 Unrated, premium 이 2/3 이상이면
/// Premium, discount 가 2/3 이상이면 Discount, 그 사이는 Neutral. 서버도 같은 규칙이다(정수 비교).
/// 공식 판정(PD-Verdict v2 의 nav 트랙, 피어 순위 포함)이 서버에 생기면 그 값을 쓴다
pub fn tier(premium: u32, discount: u32) -> Tier {
    let decided = premium + discount;
    if decided < TIER_MIN_DECIDED {
        Tier::Unrated
    } else if premium * 3 >= decided * 2 {
        Tier::Premium
    } else if discount * 3 >= decided * 2 {
        Tier::Discount
    } else {
        Tier::Neutral
    }
}

/// 다른 사용자의 코스(고스트)를 따라 달린 런의 체크포인트 판정. 시작과 결승이 고스트와 같을 때만 판정한다(같은 목적지로 가는
/// 길의 비교). 고스트의 중간 체크포인트(시작 · 결승을 뺀 것)마다, 런이 그 체크포인트를 지났으면 premium, 지나지 않았으면
/// (다른 길로 갔으면) discount 다. 런에만 있는 체크포인트는 판정하지 않는다. 시작이나 결승이 다르면 None.
/// 서버(`www/src/navtrials.ts` 의 routeMarks)도 같은 규칙이다
pub fn route_marks(run: &[StepRef], ghost: &[StepRef]) -> Option<Vec<(StepRef, MarkType)>> {
    if run.first()? != ghost.first()? || run.last()? != ghost.last()? {
        return None;
    }
    let mut out: Vec<(StepRef, MarkType)> = Vec::new();
    for step in ghost.iter().skip(1).take(ghost.len().saturating_sub(2)) {
        if out.iter().any(|(s, _)| s == step) {
            continue;
        }
        let kind = if run.contains(step) { MarkType::Premium } else { MarkType::Discount };
        out.push((step.clone(), kind));
    }
    Some(out)
}

/// 체크포인트 하나의 판정 수 (다른 사용자들이 그 체크포인트를 따라갔는가 · 비켜 갔는가)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointTally {
    pub premium: u32,
    pub discount: u32,
}

impl CheckpointTally {
    pub fn tier(&self) -> Tier {
        tier(self.premium, self.discount)
    }
}

/// 콜렉션의 코스 하나 (내 기록)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CourseEntry {
    pub course: String,
    pub steps: Vec<StepRef>,
    /// 달린 횟수 (중복 카운트)
    pub runs: u32,
    pub premium: u32,
    pub discount: u32,
    /// 최고 기록의 구간 시간
    pub best: Vec<u32>,
    /// 마지막 런의 구간 시간
    pub last: Vec<u32>,
    pub last_day: i64,
    /// 덮어쓸 순서: 클수록 최근에 달렸다
    pub touched: u64,
}

impl CourseEntry {
    pub fn best_ms(&self) -> u32 {
        self.best.last().copied().unwrap_or(0)
    }

    pub fn tier(&self) -> Tier {
        tier(self.premium, self.discount)
    }
}

/// 다른 사용자의 기록 (서버 `GET /nav/trials` 의 `others`). 같은 코스를 달린 런이 공개 하한을 넘은 코스만 온다
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublicCourse {
    pub course: String,
    pub steps: Vec<StepRef>,
    /// 모든 사용자의 달린 횟수 (계정마다 하루 한 번으로 셈)
    pub runs: u32,
    pub premium: u32,
    pub discount: u32,
    /// 최고 기록의 구간 시간
    pub record: Vec<u32>,
    /// 평균 구간 시간 (100 ms 단위)
    pub average: Vec<u32>,
    /// 체크포인트마다의 판정 수 (`steps` 와 같은 순서, [`route_marks`] 로 쌓인 것)
    #[serde(default)]
    pub checkpoints: Vec<CheckpointTally>,
}

/// 런을 콜렉션에 넣은 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// 새 코스
    New,
    /// 이미 있던 코스를 다시 달렸다. `best` 면 최고 기록을 갈았다
    Repeat { best: bool },
    /// 새 코스가 11번째라 가장 오래 달리지 않은 코스(`evicted`)를 덮어썼다
    Replaced { evicted: String },
}

/// 진행 중인 런의 안내
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub course: String,
    /// 지나온 체크포인트 수 (코스 앞부분과 같은 수)
    pub passed: usize,
    /// 코스의 체크포인트 수
    pub length: usize,
    /// 다음 체크포인트. None 이면 결승선을 지났다. 앱은 이 해시를 지금 화면의 번호로 바꿔 보여 준다
    pub next: Option<StepRef>,
    /// 다음 체크포인트의 등급: 다른 사용자들이 그 체크포인트를 따라갔는가(premium) · 비켜 갔는가(discount)
    pub next_tier: Tier,
    /// 지금 체크포인트에서의 차이(ms, + 면 뒤짐): 내 최고 기록 · 다른 사용자 최고 기록 · 다른 사용자 평균
    pub vs_best: Option<i64>,
    pub vs_record: Option<i64>,
    pub vs_average: Option<i64>,
    /// 내가 달린 횟수, 모든 사용자가 달린 횟수(공개된 코스만)
    pub my_runs: u32,
    pub all_runs: Option<u32>,
    /// 공개된 코스면 모든 사용자의 결정으로, 아니면 내 결정으로
    pub tier: Tier,
}

/// 앱 · 웹마다 코스 [`MAX_COURSES`] 개까지 두는 기록실 (기기 안)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    /// 앱 해시 → 코스들
    pub apps: BTreeMap<String, Vec<CourseEntry>>,
    #[serde(default)]
    pub clock: u64,
}

impl Collection {
    pub fn courses(&self, app: &str) -> &[CourseEntry] {
        self.apps.get(app).map_or(&[], |v| v.as_slice())
    }

    /// 런을 넣는다. 같은 코스면 횟수 · 결정 수 · 최고 · 마지막 기록을 고치고, 새 코스가 넘치면 가장 오래 달리지 않은 코스를 덮어쓴다
    pub fn add(&mut self, run: &Run) -> Added {
        self.clock += 1;
        let clock = self.clock;
        let courses = self.apps.entry(run.app.clone()).or_default();
        let splits = run.splits();
        let (premium, discount) = match run.kind {
            Some(MarkType::Premium) => (1, 0),
            Some(MarkType::Discount) => (0, 1),
            None => (0, 0),
        };
        if let Some(entry) = courses.iter_mut().find(|e| e.course == run.course) {
            let best = run.total_ms < entry.best_ms();
            entry.runs += 1;
            entry.premium += premium;
            entry.discount += discount;
            if best {
                entry.best = splits.clone();
            }
            entry.last = splits;
            entry.last_day = entry.last_day.max(run.day);
            entry.touched = clock;
            return Added::Repeat { best };
        }
        let entry = CourseEntry {
            course: run.course.clone(),
            steps: run.refs(),
            runs: 1,
            premium,
            discount,
            best: splits.clone(),
            last: splits,
            last_day: run.day,
            touched: clock,
        };
        if courses.len() < MAX_COURSES {
            courses.push(entry);
            return Added::New;
        }
        let oldest = courses.iter().enumerate().min_by_key(|(_, e)| e.touched).map_or(0, |(i, _)| i);
        Added::Replaced {
            evicted: std::mem::replace(&mut courses[oldest], entry).course,
        }
    }

    /// 진행 중인 체크포인트(시작부터 지금까지)로 고스트를 찾는다. 체크포인트 순서가 앞부분과 같은 코스 중에서 내가 가장 많이
    /// 달린 코스, 없으면 다른 사용자가 가장 많이 달린 코스를 고른다. 맞는 코스가 없으면 None
    pub fn guide(&self, app: &str, progress: &[Checkpoint], others: &[PublicCourse]) -> Option<Guide> {
        let points = course_points(progress).ok()?;
        let (start, _) = points.first()?;
        let (now, _) = points.last()?;
        let elapsed = round_split(now - start) as i64;
        let refs: Vec<StepRef> = points.iter().map(|(_, s)| s.clone()).collect();
        let fits = |steps: &[StepRef]| steps.len() >= refs.len() && steps[..refs.len()] == refs[..];

        let mine: Vec<&CourseEntry> = self.courses(app).iter().filter(|e| fits(&e.steps)).collect();
        let theirs: Vec<&PublicCourse> = others.iter().filter(|c| fits(&c.steps)).collect();
        let course = match mine.iter().max_by_key(|e| (e.runs, e.touched)) {
            Some(e) => e.course.clone(),
            None => theirs.iter().max_by_key(|c| c.runs)?.course.clone(),
        };
        let my = mine.iter().find(|e| e.course == course).copied();
        let their = theirs.iter().find(|c| c.course == course).copied();
        let steps = my.map(|e| &e.steps).or(their.map(|c| &c.steps))?;
        let at = refs.len() - 1;
        let delta = |splits: &[u32]| splits.get(at).map(|s| elapsed - *s as i64);
        Some(Guide {
            passed: refs.len(),
            length: steps.len(),
            next: steps.get(refs.len()).cloned(),
            next_tier: their.and_then(|c| c.checkpoints.get(refs.len())).map_or(Tier::Unrated, CheckpointTally::tier),
            vs_best: my.and_then(|e| delta(&e.best)),
            vs_record: their.and_then(|c| delta(&c.record)),
            vs_average: their.and_then(|c| delta(&c.average)),
            my_runs: my.map_or(0, |e| e.runs),
            all_runs: their.map(|c| c.runs),
            tier: match (their, my) {
                (Some(c), _) => tier(c.premium, c.discount),
                (None, Some(e)) => e.tier(),
                (None, None) => Tier::Unrated,
            },
            course,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_hash;

    const S1: &str = "0x1111111111111111111111111111111111111111";
    const R2: &str = "0x2222222222222222222222222222222222222222";
    const S3: &str = "0x3333333333333333333333333333333333333333";
    // ethers 6.17 로 뽑은 값 (www 시험 navtrials.test.ts 와 같은 기준값)
    const APP: &str = "0x39206c971297059b89e4e6fc35844ee87feb0407";
    // 코스 S1 → R2 → S3 의 코스 id 체인 (마지막이 코스 id)
    const CHAIN: [&str; 3] = [
        "0x826797e39016c80c7db39259352ad3af843f0acf",
        "0x517cbe9e479a79a6ef66e403072e9776000d90fc",
        "0xa9c45e19240962f6cb2dfb7aa8381e3ec3d80340",
    ];
    const COURSE: &str = CHAIN[2];
    // 영역 R2 의 체크포인트 id
    const CP_R2: &str = "0x4e9fa56d3569702eb1ec03106e8bd1896a716af5";
    const DAY: i64 = 20_774;

    fn cp(at_ms: i64, kind: StepKind, hash: &str) -> Checkpoint {
        Checkpoint {
            at_ms,
            kind,
            hash: hash.into(),
            gesture: None,
        }
    }

    fn step(kind: StepKind, hash: &str) -> StepRef {
        StepRef {
            kind,
            hash: hash.into(),
            gesture: None,
        }
    }

    fn hash_n(n: usize) -> String {
        format!("0x{n:040x}")
    }

    // 시작 · 영역 · 결승을 `ms` 간격으로 지나는 런
    fn lap(ms: [i64; 2], kind: Option<MarkType>) -> Run {
        let t0 = DAY * DAY_MS;
        let points = [cp(t0, StepKind::Screen, S1), cp(t0 + ms[0], StepKind::Region, R2), cp(t0 + ms[0] + ms[1], StepKind::Screen, S3)];
        Run::from_checkpoints(APP, &points, kind).unwrap()
    }

    #[test]
    fn app_checkpoint_and_course_ids_match_ethers() {
        assert_eq!(app_hash("Shop.Example.com ").unwrap(), APP);
        let refs = [step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, S3)];
        // 체크포인트 id = hashId(종류 + 해시 + 제스처)
        assert_eq!(refs[1].id().unwrap(), CP_R2);
        assert_eq!(hash_id(&format!("region{R2}")).unwrap(), CP_R2);
        assert_ne!(step(StepKind::Screen, R2).id().unwrap(), CP_R2, "종류가 다르면 다른 체크포인트");
        // 코스 id 체인: hashId(앱 id + 체크포인트 id) → hashId(이전 코스 id + 체크포인트 id) → …
        let ids: Vec<String> = refs.iter().map(|r| r.id().unwrap()).collect();
        assert_eq!(hash_id(&format!("{APP}{}", ids[0])).unwrap(), CHAIN[0]);
        assert_eq!(hash_id(&format!("{}{}", CHAIN[0], ids[1])).unwrap(), CHAIN[1]);
        assert_eq!(hash_id(&format!("{}{}", CHAIN[1], ids[2])).unwrap(), CHAIN[2]);
        assert_eq!(course_chain(APP, &refs).unwrap(), CHAIN);
        assert_eq!(course_hash(APP, &refs).unwrap(), COURSE);
        let run = lap([1000, 2000], None);
        assert_eq!(run.steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>(), ids);
        assert_eq!(run.steps.iter().map(|s| s.course.clone()).collect::<Vec<_>>(), CHAIN);
        assert_eq!(run.course, COURSE);
        // 앞부분이 같으면 그 지점까지의 코스 id 를 함께 갖고, 다른 체크포인트부터 갈라진다
        let branch = course_chain(APP, &[step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, &hash_n(0x66))]).unwrap();
        assert_eq!(&branch[..2], &CHAIN[..2]);
        assert_ne!(branch[2], COURSE);
        // 결승에서 멈춘 짧은 코스(S1 → R2)의 id 는 긴 코스의 중간 id 와 같다: 같은 길은 한 번만 등록된다
        assert_eq!(course_hash(APP, &refs[..2]).unwrap(), CHAIN[1]);
        // 순서 · 종류가 다르면 다른 코스
        let swapped = [step(StepKind::Region, R2), step(StepKind::Screen, S1), step(StepKind::Screen, S3)];
        assert_ne!(course_hash(APP, &swapped).unwrap(), COURSE);
        let kind = [step(StepKind::Screen, S1), step(StepKind::Screen, R2), step(StepKind::Screen, S3)];
        assert_ne!(course_hash(APP, &kind).unwrap(), COURSE);
    }

    // 웹 코스: 페이지(링크값) · 요소(DOM 셀렉터)와 제스처. ethers 6.17 로 뽑은 값 (www 시험 navtrials.test.ts 와 같은 기준값)
    const WEB_COURSE: &str = "0xe8c5279f506af6687f43eee19360173c51f44cce";

    fn gcp(at_ms: i64, kind: StepKind, fill: char, gesture: Option<Gesture>) -> Checkpoint {
        Checkpoint {
            at_ms,
            kind,
            hash: format!("0x{}", fill.to_string().repeat(40)),
            gesture,
        }
    }

    #[test]
    fn web_courses_use_pages_elements_and_gestures() {
        let points = [
            gcp(0, StepKind::Page, 'a', None),
            gcp(400, StepKind::Element, 'b', Some(Gesture::Press)), // 누름은 붙지 않는다
            gcp(900, StepKind::Page, 'a', Some(Gesture::ScrollDown)),
            gcp(1000, StepKind::Page, 'a', Some(Gesture::ScrollDown)), // 이어진 스크롤은 하나로
            gcp(1500, StepKind::Element, 'c', Some(Gesture::SwipeLeft)),
            gcp(2100, StepKind::Page, 'd', None),
        ];
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.course, WEB_COURSE);
        let keys: Vec<String> = run.refs().iter().map(StepRef::key).collect();
        assert_eq!(keys[1], format!("element:0x{}", "b".repeat(40)));
        assert_eq!(keys[2], format!("page:0x{}@scroll-down", "a".repeat(40)));
        assert_eq!(run.splits(), vec![0, 400, 900, 1500, 2100]);
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["steps"][3]["kind"], "element");
        assert_eq!(json["steps"][3]["gesture"], "swipe-left");
        assert!(json["steps"][1].get("gesture").is_none(), "누름은 칸이 없다");
        // 마우스로 누른 사람과 손가락으로 누른 사람은 같은 코스, 스와이프로 지난 사람은 다른 코스
        let mut tapped = points.clone();
        tapped[1].gesture = None;
        assert_eq!(Run::from_checkpoints(APP, &tapped, None).unwrap().course, WEB_COURSE);
        tapped[4].gesture = Some(Gesture::Press);
        assert_ne!(Run::from_checkpoints(APP, &tapped, None).unwrap().course, WEB_COURSE);
    }

    #[test]
    fn run_rounds_splits_and_keeps_the_finish_day() {
        let t0 = DAY * DAY_MS + DAY_MS - 1_000; // 자정 1초 전 출발 → 다음 날 결승
        let points = [
            cp(t0, StepKind::Screen, &S1.to_uppercase().replace("0X", "0x")),
            cp(t0 + 1_249, StepKind::Region, R2),
            cp(t0 + 1_300, StepKind::Region, R2), // 같은 체크포인트를 잇달아 → 하나로 (처음 도착 시각)
            cp(t0 + 3_351, StepKind::Screen, S3),
        ];
        let run = Run::from_checkpoints(APP, &points, Some(MarkType::Premium)).unwrap();
        assert_eq!(run.splits(), vec![0, 1_200, 3_400]);
        assert_eq!(run.total_ms, 3_400);
        assert_eq!(run.day, DAY + 1);
        assert_eq!(run.steps[0].hash, S1);
        assert_eq!(run.course, COURSE);

        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["schema"], "trial/1");
        assert_eq!(json["type"], "premium");
        assert_eq!(json["recipe"], "pd-verdict/2");
        assert_eq!(json["steps"][1], serde_json::json!({ "id": CP_R2, "course": CHAIN[1], "kind": "region", "hash": R2, "split_ms": 1200 }));
        assert_eq!(serde_json::to_value(lap([1, 1], None)).unwrap()["type"], serde_json::Value::Null);
        assert_eq!(serde_json::from_value::<Run>(json).unwrap(), run);
    }

    #[test]
    fn long_flows_keep_the_last_ten_checkpoints() {
        let points: Vec<Checkpoint> = (0..13).map(|i| cp(i as i64 * 1_000, StepKind::Screen, &hash_n(i + 1))).collect();
        let run = Run::from_checkpoints(APP, &points, None).unwrap();
        assert_eq!(run.steps.len(), MAX_STEPS);
        assert_eq!(run.steps[0].hash, hash_n(4), "오래된 체크포인트 3개를 버린다");
        assert_eq!(run.steps[0].split_ms, 0, "시계는 남은 첫 체크포인트에서 시작한다");
        assert_eq!(run.total_ms, 9_000);
    }

    #[test]
    fn invalid_runs_are_refused() {
        let ok = [cp(0, StepKind::Screen, S1), cp(1_000, StepKind::Screen, S3)];
        assert_eq!(Run::from_checkpoints("0x12", &ok, None), Err(TrialError::Hash));
        assert_eq!(Run::from_checkpoints(APP, &[cp(0, StepKind::Screen, "nope"), ok[1].clone()], None), Err(TrialError::Hash));
        assert_eq!(Run::from_checkpoints(APP, &ok[..1], None), Err(TrialError::TooShort));
        assert_eq!(Run::from_checkpoints(APP, &[ok[0].clone(), cp(5, StepKind::Screen, S1)], None), Err(TrialError::TooShort));
        assert_eq!(Run::from_checkpoints(APP, &[ok[1].clone(), ok[0].clone()], None), Err(TrialError::Order));
        let slow = [ok[0].clone(), cp(MAX_RUN_MS as i64 + 1, StepKind::Screen, S3)];
        assert_eq!(Run::from_checkpoints(APP, &slow, None), Err(TrialError::TooLong));
    }

    #[test]
    fn vectors_are_normalized_per_checkpoint() {
        let mut run = lap([1000, 1000], None);
        run.attach_vectors(1, &[("granite-97m-r2-cls-v1", &[3.0, 4.0])]).unwrap();
        assert_eq!(run.steps[1].vectors[0].vector, vec![0.6, 0.8]);
        assert_eq!(run.attach_vectors(1, &[("a", &[1.0]), ("a", &[1.0])]), Err(MarkError::Space));
        assert_eq!(run.attach_vectors(1, &[("a", &[0.0])]), Err(MarkError::Vector));
        assert_eq!(run.attach_vectors(9, &[("a", &[1.0])]), Err(MarkError::Vector));
    }

    #[test]
    fn repeats_count_up_and_keep_the_best() {
        let mut c = Collection::default();
        assert_eq!(c.add(&lap([2000, 2000], Some(MarkType::Discount))), Added::New);
        assert_eq!(c.add(&lap([1000, 1500], Some(MarkType::Premium))), Added::Repeat { best: true });
        assert_eq!(c.add(&lap([3000, 3000], None)), Added::Repeat { best: false });
        let e = &c.courses(APP)[0];
        assert_eq!((e.runs, e.premium, e.discount), (3, 1, 1));
        assert_eq!(e.best, vec![0, 1000, 2500]);
        assert_eq!(e.last, vec![0, 3000, 6000]);
        assert_eq!(c.courses(APP).len(), 1);
    }

    #[test]
    fn the_eleventh_course_replaces_the_one_run_longest_ago() {
        let mut c = Collection::default();
        let course = |i: usize| Run::from_checkpoints(APP, &[cp(0, StepKind::Screen, &hash_n(100 + i)), cp(1000, StepKind::Screen, S3)], None).unwrap();
        for i in 0..MAX_COURSES {
            assert_eq!(c.add(&course(i)), Added::New);
        }
        // 0번 코스를 다시 달려 가장 최근이 된다 → 다음에 밀려나는 것은 1번
        c.add(&course(0));
        assert_eq!(c.add(&course(10)), Added::Replaced { evicted: course(1).course });
        assert_eq!(c.courses(APP).len(), MAX_COURSES);
        assert!(c.courses(APP).iter().any(|e| e.course == course(0).course && e.runs == 2));
        // 다른 앱 · 웹은 따로 센다
        let other = Run::from_checkpoints(S1, &[cp(0, StepKind::Screen, S1), cp(1, StepKind::Screen, S3)], None).unwrap();
        assert_eq!(c.add(&other), Added::New);
        assert_eq!(c.courses(S1).len(), 1);
    }

    #[test]
    fn tier_needs_three_decided_runs_and_a_two_thirds_share() {
        assert_eq!(tier(2, 0), Tier::Unrated);
        assert_eq!(tier(2, 1), Tier::Premium);
        assert_eq!(tier(1, 2), Tier::Discount);
        assert_eq!(tier(3, 2), Tier::Neutral);
        assert_eq!(tier(4, 2), Tier::Premium);
        assert_eq!(tier(0, 3), Tier::Discount);
    }

    // www 시험 navtrials.test.ts 와 같은 사례: 고스트 S1 → A → B → 결승, 런 S1 → A → C → 결승
    #[test]
    fn following_the_ghost_is_premium_and_the_road_not_taken_is_discount() {
        let (a, b, c) = (step(StepKind::Region, R2), step(StepKind::Region, &hash_n(0x44)), step(StepKind::Region, &hash_n(0x55)));
        let (start, finish) = (step(StepKind::Screen, S1), step(StepKind::Screen, S3));
        let ghost = [start.clone(), a.clone(), b.clone(), finish.clone()];
        let run = [start.clone(), a.clone(), c.clone(), finish.clone()];
        assert_eq!(route_marks(&run, &ghost), Some(vec![(a.clone(), MarkType::Premium), (b.clone(), MarkType::Discount)]));
        // 고스트를 그대로 따라가면 중간 체크포인트가 모두 premium
        assert_eq!(route_marks(&ghost, &ghost), Some(vec![(a.clone(), MarkType::Premium), (b.clone(), MarkType::Premium)]));
        // 순서가 달라도 지났으면 따라간 것이다
        let swapped = [start.clone(), b.clone(), a.clone(), finish.clone()];
        assert_eq!(route_marks(&swapped, &ghost), Some(vec![(a.clone(), MarkType::Premium), (b.clone(), MarkType::Premium)]));
        // 지름길(중간 없이 결승)이면 고스트의 중간 체크포인트는 모두 discount
        assert_eq!(route_marks(&[start.clone(), finish.clone()], &ghost), Some(vec![(a.clone(), MarkType::Discount), (b.clone(), MarkType::Discount)]));
        // 결승 · 시작이 다르면 목적이 다른 것이라 판정하지 않는다. 중간이 없는 고스트는 판정할 것이 없다
        assert_eq!(route_marks(&[start.clone(), a.clone(), c.clone()], &ghost), None);
        assert_eq!(route_marks(&[c.clone(), a.clone(), finish.clone()], &ghost), None);
        assert_eq!(route_marks(&[], &ghost), None);
        assert_eq!(route_marks(&run, &[start.clone(), finish.clone()]), Some(vec![]));
        assert_eq!(CheckpointTally { premium: 1, discount: 3 }.tier(), Tier::Discount);
    }

    #[test]
    fn a_run_names_the_ghost_it_followed() {
        let mut run = lap([1000, 1000], None);
        assert!(serde_json::to_value(&run).unwrap().get("ghost").is_none(), "고스트가 없으면 칸도 없다");
        run.ghost = Some(COURSE.into());
        let json = serde_json::to_value(&run).unwrap();
        assert_eq!(json["ghost"], COURSE);
        assert_eq!(serde_json::from_value::<Run>(json).unwrap(), run);
    }

    #[test]
    fn guide_follows_the_ghost_to_the_finish() {
        let mut c = Collection::default();
        c.add(&lap([1000, 2000], Some(MarkType::Premium)));
        let t0 = 5 * DAY_MS;
        let others = vec![PublicCourse {
            course: COURSE.into(),
            steps: vec![step(StepKind::Screen, S1), step(StepKind::Region, R2), step(StepKind::Screen, S3)],
            runs: 42,
            premium: 20,
            discount: 4,
            record: vec![0, 800, 1500],
            average: vec![0, 1400, 3500],
            checkpoints: vec![CheckpointTally::default(), CheckpointTally { premium: 1, discount: 3 }, CheckpointTally::default()],
        }];

        // 출발만 했을 때: 다음은 영역 R2, 차이는 0
        let g = c.guide(APP, &[cp(t0, StepKind::Screen, S1)], &others).unwrap();
        assert_eq!((g.passed, g.length), (1, 3));
        assert_eq!(g.next, Some(step(StepKind::Region, R2)));
        assert_eq!((g.vs_best, g.vs_record, g.vs_average), (Some(0), Some(0), Some(0)));
        assert_eq!((g.my_runs, g.all_runs, g.tier), (1, Some(42), Tier::Premium));
        // 다음 체크포인트(R2)는 다른 사용자 넷 중 셋이 비켜 갔다
        assert_eq!(g.next_tier, Tier::Discount);

        // 1.3초에 R2: 내 최고보다 0.3초, 기록보다 0.5초 뒤짐, 평균보다 0.1초 앞섬
        let progress = [cp(t0, StepKind::Screen, S1), cp(t0 + 1_300, StepKind::Region, R2)];
        let g = c.guide(APP, &progress, &others).unwrap();
        assert_eq!(g.next, Some(step(StepKind::Screen, S3)));
        assert_eq!((g.vs_best, g.vs_record, g.vs_average), (Some(300), Some(500), Some(-100)));
        assert_eq!(g.next_tier, Tier::Unrated);

        // 결승선을 지나면 다음은 없다
        let done = [progress[0].clone(), progress[1].clone(), cp(t0 + 2_900, StepKind::Screen, S3)];
        let g = c.guide(APP, &done, &others).unwrap();
        assert_eq!(g.next, None);
        assert_eq!(g.vs_best, Some(-100));

        // 처음 가는 사용자(내 기록 없음)도 다른 사용자의 고스트로 안내받는다
        let g = Collection::default().guide(APP, &progress, &others).unwrap();
        assert_eq!((g.vs_best, g.vs_record, g.my_runs), (None, Some(500), 0));
        // 코스와 다른 길이면 안내하지 않는다
        assert_eq!(c.guide(APP, &[cp(t0, StepKind::Screen, S3)], &others), None);
        assert_eq!(c.guide(APP, &[], &others), None);
    }
}
