//! 행동 기록 → 에피소드 → SDS 결정 (기기 안)
//!
//! 번호 선택 · 이벤트 카메라 · form change 에서 나온 사건을 에피소드로 자르고([`split_episodes`]), 에피소드마다
//! 마찰 점수와 성공 여부를 구한다. 그 값을 같은 스코프 피어의 지난 에피소드 분포(Welford)와 견줘, SDS 띠를 넘을 때만
//! premium 또는 discount 로 결정한다([`FrictionBaselines::decide`]). 화면은 같은 앱 · 사이트의 다른 화면 → 전역과,
//! 영역은 같은 화면의 다른 영역 → 앱 → 전역과 견준다. 판정하는 항목 자신의 에피소드는 기준선에서 뺀다: 늘 매끄러운
//! 화면이 자기 자신과 견줘져 결정이 사라지지 않게 하기 위해서다. 띠 안이거나 기준선이 아직 없으면 결정하지 않는다.
//! 늘 결정할 수는 없고, 결정이 난 것만 서버 기록([`crate::NavMark`])이 된다.
//! 사건에는 입력값 · 화면 글자가 없다. 마찰 가중치는 초깃값이고 실측으로 고친다.
//!
//! 진행 중인 에피소드는 기준선을 바꾸지 않고 읽는다([`FrictionBaselines::peek_live`], `&self`). 기준선에는 끝난 에피소드만
//! 한 번씩 들어간다([`LiveEpisodeWindow::flush_completed`] → `decide`). 마찰이 몰린 영역은 번호를 고르지 않았어도 잡고
//! ([`Episode::bottleneck_focus`]), 첫 동작 전에 화면을 훑는 시간은 판독할 때만 가상 `Searched` 로 더한다. 이 탐색 시계는
//! 자리 비움 · 개인정보 잠금 동안 멈추고, 전투 중 연타는 광클로 넣지 않는다. 기준선이 아직 없으면 절대 임계값으로 경보
//! 단계를 정한다([`FrictionAlertStage`]). 개인정보 잠금이 시작되면 지난 5초 사건을 지우고, 잠금을 걸친 에피소드는 기록하지 않는다.

use std::collections::{BTreeMap, HashMap};

use premium_discount_identity::wipe_string;
use premium_discount_verdict::{Level, MinObs, Params, Tier, NAV};
use serde::{Deserialize, Serialize};

use crate::mark::MarkType;
use crate::resume::frozen_overlap_ms;

/// 행동 기록의 사건 종류 (tracker-logis-center 의 사건 정의를 따르되 값은 담지 않는다)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    /// 번호를 골랐다
    Picked,
    /// 고른 영역에서 입력 · 실행을 시작했다
    Engaged,
    /// form 을 오류 없이 제출했다
    Submitted,
    /// 제출이 오류로 돌아왔거나 같은 칸을 다시 고쳤다
    Retried,
    /// 700 ms 안에 같은 곳을 세 번 이상 눌렀다
    RageClick,
    /// 화면이 크게 바뀐 뒤 첫 동작까지 걸린 시간
    Searched { ms: u32 },
    /// 500 ms 이상 머물기만 하고 떠났다
    Hovered,
    /// 되돌아가기 · 번호 목록 다시 열기 · 포커스 왕복
    Backtracked,
    /// 입력한 뒤 제출 없이 떠났다
    Abandoned,
    /// 스크롤 방향을 뒤집으며 찾았다 (뒤집은 수, [`crate::scroll_bursts`])
    Rescrolled { times: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub at_ms: i64,
    pub signal: Signal,
    /// 사건이 난 영역의 위치 단서 (없으면 화면 전체)
    #[serde(default)]
    pub anchor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorParams {
    pub rage_click: f64,
    pub abandoned: f64,
    pub retried: f64,
    pub backtracked: f64,
    pub hovered: f64,
    /// 찾는 데 걸린 1초마다
    pub search_per_s: f64,
    /// 스크롤 방향을 한 번 뒤집을 때마다
    #[serde(default = "default_rescroll")]
    pub rescroll: f64,
    /// SDS 띠: 기준선 평균에서 표준편차의 몇 배를 넘어야 결정하는가
    pub band: f64,
    /// 표준편차가 0 에 가까울 때 z 가 튀지 않게 더하는 값 (마찰 점수 단위)
    pub eps: f64,
    /// 이보다 긴 간격에서는 늘 에피소드를 자른다
    pub max_gap_ms: i64,
    /// 간격 z 규칙은 사건이 이만큼 이상일 때만 쓴다 (logis-center U-1)
    pub min_events_to_split: usize,
    /// 기준선의 표본 하한 (secondary = 화면, primary = 앱 · 사이트, global = 전역). 판정 계약 nav 트랙과 같은 값
    pub min_obs: MinObs,
    /// 진행 중인 에피소드의 실시간 판독 ([`FrictionBaselines::peek_live`])
    #[serde(default)]
    pub live: LiveParams,
}

fn default_rescroll() -> f64 {
    0.5
}

impl Default for BehaviorParams {
    fn default() -> Self {
        BehaviorParams {
            rage_click: 3.0,
            abandoned: 3.0,
            retried: 2.0,
            backtracked: 1.0,
            hovered: 0.5,
            search_per_s: 0.5,
            rescroll: default_rescroll(),
            band: 1.0,
            eps: 0.25,
            max_gap_ms: 300_000,
            min_events_to_split: 4,
            min_obs: Params::v2().track(NAV).expect("pd-verdict/2 의 nav 트랙").min_obs,
            live: LiveParams::default(),
        }
    }
}

/// 실시간 경보의 임계값 ([`FrictionAlertStage`]). 초깃값이고 실측으로 고친다
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveParams {
    /// 기준선이 있을 때 PreWarm: z 가 `band` 의 이 배수 이상
    pub prewarm_band: f64,
    /// 기준선이 있을 때 CoachReady: z 가 `band` 의 이 배수 이상이고, 복합 징후가 있고, 마찰이 `min_coach_friction` 이상
    pub coach_band: f64,
    /// CoachReady 의 마찰 하한. 기준선의 표준편차가 아주 작을 때 머묾 한 번으로 z 가 튀어 코칭하지 않게 한다
    pub min_coach_friction: f64,
    /// 기준선이 없을 때(콜드스타트) PreWarm: 마찰이 이 값 이상이고 마찰 사건이 `cold_prewarm_events` 개 이상
    pub cold_prewarm_friction: f64,
    pub cold_prewarm_events: usize,
    /// 콜드스타트 CoachReady: 마찰이 이 값 이상이고 복합 징후가 있다
    pub cold_coach_friction: f64,
    /// 복합 징후: 광클 한 번, 다시 고침 `coach_retries` 번, 또는 탐색 `coach_search_ms` 이상
    pub coach_retries: usize,
    pub coach_search_ms: u32,
    /// 첫 동작 전 탐색이 이만큼 넘어야 가상 `Searched` 로 센다 (premium-discount-analytic 의 `SEARCH_MIN_MS` 와 같은 1초)
    pub search_min_ms: u32,
}

impl Default for LiveParams {
    fn default() -> Self {
        LiveParams {
            prewarm_band: 1.0,
            coach_band: 1.6,
            min_coach_friction: 2.5,
            cold_prewarm_friction: 4.5,
            cold_prewarm_events: 2,
            cold_coach_friction: 7.5,
            coach_retries: 2,
            coach_search_ms: 12_000,
            search_min_ms: 1_000,
        }
    }
}

/// 활동 사이 간격이 이보다 길면 자리 비움(AFK)으로 보고, 그 간격은 탐색 시간에 넣지 않는다
pub const IDLE_AFK_CUTOFF_MS: i64 = 8_000;
/// 개인정보 잠금이 시작되면 이만큼 거슬러 올라간 사건까지 지운다
pub const PRIVACY_PURGE_MS: i64 = 5_000;

/// 같은 일을 하려던 사건 묶음
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub events: Vec<Event>,
}

impl Episode {
    pub fn start_ms(&self) -> i64 {
        self.events.first().map_or(0, |e| e.at_ms)
    }

    pub fn end_ms(&self) -> i64 {
        self.events.last().map_or(0, |e| e.at_ms)
    }

    /// 마찰 점수: 사건마다 가중치를 더한다
    pub fn friction(&self, p: &BehaviorParams) -> f64 {
        self.events.iter().map(|e| weight(&e.signal, p)).sum()
    }

    /// 제출했거나, 입력을 시작하고 버리지 않았다
    pub fn succeeded(&self) -> bool {
        let has = |want: fn(&Signal) -> bool| self.events.iter().any(|e| want(&e.signal));
        has(|s| matches!(s, Signal::Submitted)) || (has(|s| matches!(s, Signal::Engaged)) && !has(|s| matches!(s, Signal::Abandoned)))
    }

    /// 마지막으로 번호를 고른 영역 (영역 결정에 쓴다)
    pub fn focus(&self) -> Option<&str> {
        self.events.iter().rev().find(|e| matches!(e.signal, Signal::Picked)).and_then(|e| e.anchor.as_deref())
    }

    /// 마찰이 몰린 영역: 위치 단서마다 사건의 마찰 가중치를 더해 가장 큰 곳이다(같으면 마지막 마찰 사건이 늦은 곳).
    /// [`Episode::focus`] 는 고른 영역만 보지만, 광클 · 다시 고침은 번호를 고르지 않고도 나므로 그 영역의 마찰로 잡는다.
    /// 위치 단서가 없는 사건(화면 전체)은 세지 않고, 마찰이 있는 영역이 없으면 None
    pub fn bottleneck_focus(&self, p: &BehaviorParams) -> Option<&str> {
        self.bottleneck_except(p, None)
    }

    /// [`Episode::bottleneck_focus`] 에서 위치 단서 `skip`(화면 해시)도 뺀다
    fn bottleneck_except(&self, p: &BehaviorParams, skip: Option<&str>) -> Option<&str> {
        // (위치 단서, 마찰 합, 마지막 마찰 사건의 시각, 그 순번)
        let mut sums: Vec<(&str, f64, i64, usize)> = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            let w = weight(&e.signal, p);
            let Some(anchor) = e.anchor.as_deref().filter(|a| Some(*a) != skip && w > 0.0) else {
                continue;
            };
            match sums.iter_mut().find(|s| s.0 == anchor) {
                Some(s) => *s = (anchor, s.1 + w, e.at_ms, i),
                None => sums.push((anchor, w, e.at_ms, i)),
            }
        }
        sums.into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)))
            .map(|s| s.0)
    }
}

/// 사건 하나의 마찰 가중치
fn weight(signal: &Signal, p: &BehaviorParams) -> f64 {
    match *signal {
        Signal::RageClick => p.rage_click,
        Signal::Abandoned => p.abandoned,
        Signal::Retried => p.retried,
        Signal::Backtracked => p.backtracked,
        Signal::Hovered => p.hovered,
        Signal::Searched { ms } => p.search_per_s * ms as f64 / 1000.0,
        Signal::Rescrolled { times } => p.rescroll * times as f64,
        Signal::Picked | Signal::Engaged | Signal::Submitted => 0.0,
    }
}

fn mean_sd(xs: &[f64]) -> (f64, f64) {
    if xs.is_empty() {
        return (0.0, 0.0);
    }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
    (mean, var.sqrt())
}

/// 시간순 사건을 에피소드로 자른다. logis-center 의 U-1 규칙: 이웃 사건의 `distance`(의미 거리. 예: granite 임베딩의
/// 1 − 코사인, 모르면 1)에 시간 간격의 z 값을 곱하고, 그 값이 평균 + 표준편차를 넘는 곳에서 자른다(사건이
/// `min_events_to_split` 개 이상일 때). 간격이 `max_gap_ms` 보다 길면 늘 자른다.
pub fn split_episodes(events: &[Event], distance: impl Fn(&Event, &Event) -> f64, p: &BehaviorParams) -> Vec<Episode> {
    let gaps: Vec<f64> = events.windows(2).map(|w| (w[1].at_ms - w[0].at_ms).max(0) as f64).collect();
    split_at_gaps(events, &gaps, distance, p)
}

/// [`split_episodes`] 와 같되, 이웃 사건 사이 간격에서 동결 구간(개인정보 잠금 · 유예 · 영수증 보호의 `(시작, 끝)` ms,
/// [`crate::PrivacyStateMachine::active_frozen_spans`])과 겹친 시간을 뺀다([`crate::frozen_overlap_ms`]). 본인인증으로 6분을
/// 보내도 `max_gap_ms` 나 간격 z 로 에피소드가 잘리지 않는다. 동결 구간이 없으면 [`split_episodes`] 와 같다
pub fn split_episodes_with_freezes(events: &[Event], distance: impl Fn(&Event, &Event) -> f64, p: &BehaviorParams, frozen: &[(i64, i64)]) -> Vec<Episode> {
    let gaps: Vec<f64> = events
        .windows(2)
        .map(|w| ((w[1].at_ms - w[0].at_ms) - frozen_overlap_ms(w[0].at_ms, w[1].at_ms, frozen)).max(0) as f64)
        .collect();
    split_at_gaps(events, &gaps, distance, p)
}

fn split_at_gaps(events: &[Event], gaps: &[f64], distance: impl Fn(&Event, &Event) -> f64, p: &BehaviorParams) -> Vec<Episode> {
    let Some(first) = events.first() else {
        return Vec::new();
    };
    let mut cut: Vec<bool> = gaps.iter().map(|g| *g > p.max_gap_ms as f64).collect();
    if events.len() >= p.min_events_to_split {
        let (gap_mean, gap_sd) = mean_sd(gaps);
        let scores: Vec<f64> = gaps
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let z = if gap_sd > 0.0 { (g - gap_mean) / gap_sd } else { 0.0 };
                distance(&events[i], &events[i + 1]) * z
            })
            .collect();
        let (score_mean, score_sd) = mean_sd(&scores);
        for (i, s) in scores.iter().enumerate() {
            if score_sd > 0.0 && *s > score_mean + score_sd {
                cut[i] = true;
            }
        }
    }
    let mut out = Vec::new();
    let mut current = vec![first.clone()];
    for (i, next) in events.iter().enumerate().skip(1) {
        if cut[i - 1] {
            out.push(Episode {
                events: std::mem::take(&mut current),
            });
        }
        current.push(next.clone());
    }
    out.push(Episode { events: current });
    out
}

/// 한 스코프의 마찰 분포 (Welford, 모집단 표준편차). 수식은 premium-discount-sds 의 [`Moments`](premium_discount_sds::Moments) 이고,
/// 직렬화 모양(`n` · `mean` · `m2`)도 같다.
pub use premium_discount_sds::Moments as Welford;

/// 항목 하나(화면 또는 영역)의 결정
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judgement {
    /// premium · discount. 띠 안이거나 기준선이 아직 없으면 None
    pub mark: Option<MarkType>,
    /// 견준 단계 (secondary = 같은 화면, primary = 같은 앱 · 사이트, global = 전역)
    pub level: Option<Level>,
    pub z: Option<f64>,
}

/// 에피소드 하나의 결정
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub friction: f64,
    pub succeeded: bool,
    /// 화면 결정
    pub screen: Judgement,
    /// 이 에피소드가 다룬 영역의 결정 (영역을 넘기지 않았으면 None)
    pub region: Option<Judgement>,
}

/// 성공했고 마찰이 기준선보다 띠만큼 낮으면 premium, 마찰이 띠만큼 높으면 discount
fn judge(friction: f64, succeeded: bool, levels: &[(Level, Welford)], p: &BehaviorParams) -> Judgement {
    let base = premium_discount_sds::backoff(levels, |lv| p.min_obs.get(lv));
    let level = base.map(|(lv, _)| lv);
    let z = base.map(|(_, w)| (friction - w.mean) / (w.population_sd() + p.eps));
    let mark = match z {
        Some(z) if succeeded && z <= -p.band => Some(MarkType::Premium),
        Some(z) if z >= p.band => Some(MarkType::Discount),
        _ => None,
    };
    Judgement { mark, level, z }
}

/// 에피소드 마찰의 스코프 기준선. 기기 안에만 둔다 (서버로 가는 것은 결정뿐)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrictionBaselines {
    /// 영역 해시 → 그 영역을 다룬 에피소드의 분포
    pub regions: HashMap<String, Welford>,
    /// 화면 해시 → 분포
    pub screens: HashMap<String, Welford>,
    /// `cc` → 분포
    pub apps: HashMap<String, Welford>,
    pub global: Welford,
}

impl FrictionBaselines {
    /// 에피소드 하나를 결정하고 기준선에 넣는다. 견주는 분포는 이 에피소드를 넣기 전의 것이고, 판정하는 항목 자신의
    /// 에피소드는 뺀다. `region` 은 이 에피소드가 다룬 영역의 해시다(번호를 고른 영역. 없으면 화면만 결정한다).
    pub fn decide(&mut self, cc: &str, screen: &str, region: Option<&str>, episode: &Episode, p: &BehaviorParams) -> Decision {
        let friction = episode.friction(p);
        let succeeded = episode.succeeded();
        let (screen_judgement, region_judgement) = self.judge_episode(cc, screen, region, friction, succeeded, p);
        if let Some(r) = region {
            self.regions.entry(r.to_string()).or_default().push(friction);
        }
        self.screens.entry(screen.to_string()).or_default().push(friction);
        self.apps.entry(cc.to_string()).or_default().push(friction);
        self.global.push(friction);
        Decision {
            friction,
            succeeded,
            screen: screen_judgement,
            region: region_judgement,
        }
    }

    /// 진행 중인 에피소드를 읽기만 한다(`&self`: 기준선은 한 비트도 바뀌지 않는다). 견주는 분포와 스코프 백오프는
    /// [`FrictionBaselines::decide`] 와 같아서, 가상 탐색이 없으면 z 도 decide 가 넣기 직전에 낼 값과 같다. 영역은 마찰이
    /// 몰린 곳([`Episode::bottleneck_focus`], `screen` 과 같은 위치 단서는 뺀다)이고, 그런 곳이 없으면 마지막으로 고른 곳이다.
    ///
    /// 첫 동작 전 탐색 시간(`probe.active_unacted_search_ms`)이 `live.search_min_ms` 이상이면 가상 `Searched` 로 마찰에 더한다.
    /// 사건 목록은 그대로 두고, 사건에 이미 `Searched` 가 있으면(첫 동작 때 잰 값) 겹쳐 더하지 않는다. 고스트보다 늦었으면
    /// (`probe.ghost_overrun_ms`) 탐색 지연이 헤맴이라는 확신이 커지므로 지연에 더해 복합 징후를 앞당긴다.
    /// 경보 단계는 [`FrictionAlertStage`] 의 규칙대로 정한다
    pub fn peek_live(&self, cc: &str, screen: &str, episode: &Episode, probe: &LiveProbeContext, p: &BehaviorParams) -> LiveFrictionReading {
        let live = &p.live;
        let committed_friction = episode.friction(p);
        let committed_search = episode
            .events
            .iter()
            .filter_map(|e| match e.signal {
                Signal::Searched { ms } => Some(ms),
                _ => None,
            })
            .max();
        let virtual_search = (committed_search.is_none() && probe.active_unacted_search_ms >= live.search_min_ms).then_some(probe.active_unacted_search_ms);
        let effective_friction = committed_friction + virtual_search.map_or(0.0, |ms| weight(&Signal::Searched { ms }, p));
        let bottleneck = episode.bottleneck_except(p, Some(screen));
        let region = bottleneck.or_else(|| episode.focus().filter(|a| *a != screen));
        let (screen_judgement, region_judgement) = self.judge_episode(cc, screen, region, effective_friction, episode.succeeded(), p);

        let count = |want: fn(&Signal) -> bool| episode.events.iter().filter(|e| want(&e.signal)).count();
        let rage = count(|s| matches!(s, Signal::RageClick));
        let retried = count(|s| matches!(s, Signal::Retried));
        let friction_events = episode.events.iter().filter(|e| weight(&e.signal, p) > 0.0).count() + usize::from(virtual_search.is_some());
        let search_ms = i64::from(virtual_search.or(committed_search).unwrap_or(0));
        let stall_ms = if search_ms > 0 { search_ms + i64::from(probe.ghost_overrun_ms) } else { 0 };
        let compound = rage >= 1 || retried >= live.coach_retries || stall_ms >= i64::from(live.coach_search_ms);
        let z = [screen_judgement.z, region_judgement.as_ref().and_then(|j| j.z)].into_iter().flatten().reduce(f64::max);
        let stage = match z {
            Some(z) if z >= live.coach_band * p.band && effective_friction >= live.min_coach_friction && compound => FrictionAlertStage::CoachReady,
            Some(z) if z >= live.prewarm_band * p.band => FrictionAlertStage::PreWarm,
            Some(_) => FrictionAlertStage::Calm,
            None if effective_friction >= live.cold_coach_friction && compound => FrictionAlertStage::CoachReady,
            None if effective_friction >= live.cold_prewarm_friction && friction_events >= live.cold_prewarm_events => FrictionAlertStage::PreWarm,
            None => FrictionAlertStage::Calm,
        };
        LiveFrictionReading {
            committed_friction,
            effective_friction,
            screen: screen_judgement,
            region: region_judgement,
            bottleneck_anchor: bottleneck.map(str::to_string),
            dominant_signal: dominant_signal(&episode.events, virtual_search, probe.now_ms, p),
            stage,
            cold_start_fallback: z.is_none(),
        }
    }

    /// 넣기 전의 분포로 화면 · 영역을 판정한다. 판정하는 항목 자신의 에피소드는 뺀다 (decide · peek_live 가 같은 식을 쓴다)
    fn judge_episode(&self, cc: &str, screen: &str, region: Option<&str>, friction: f64, succeeded: bool, p: &BehaviorParams) -> (Judgement, Option<Judgement>) {
        let app = self.apps.get(cc).copied().unwrap_or_default();
        let this_screen = self.screens.get(screen).copied().unwrap_or_default();
        let screen_judgement = judge(
            friction,
            succeeded,
            &[(Level::Primary, app.without(&this_screen)), (Level::Global, self.global.without(&this_screen))],
            p,
        );
        let region_judgement = region.map(|r| {
            let this_region = self.regions.get(r).copied().unwrap_or_default();
            judge(
                friction,
                succeeded,
                &[
                    (Level::Secondary, this_screen.without(&this_region)),
                    (Level::Primary, app.without(&this_region)),
                    (Level::Global, self.global.without(&this_region)),
                ],
                p,
            )
        });
        (screen_judgement, region_judgement)
    }
}

/// 실시간 마찰 경보 단계. 순서가 있다(`Calm < PreWarm < CoachReady`): 예열하는 쪽은 `stage >= PreWarm` 으로 본다
/// (기준선이 없을 때는 PreWarm 을 거치지 않고 CoachReady 가 될 수 있다).
///
/// | 단계 | 기준선이 있을 때 (z) | 기준선이 없을 때 (콜드스타트) |
/// | --- | --- | --- |
/// | PreWarm | z ≥ `prewarm_band` × band | 마찰 ≥ `cold_prewarm_friction` 이고 마찰 사건 ≥ `cold_prewarm_events` |
/// | CoachReady | z ≥ `coach_band` × band, 마찰 ≥ `min_coach_friction`, 복합 징후 | 마찰 ≥ `cold_coach_friction`, 복합 징후 |
///
/// z 는 화면 · 영역 판정 중 큰 값이고, 복합 징후는 광클 한 번 · 다시 고침 `coach_retries` 번 · 탐색 `coach_search_ms` 이상
/// (고스트보다 늦은 시간을 더해서) 중 하나다. 값은 [`LiveParams`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrictionAlertStage {
    /// 순항: 개입하지 않는다
    Calm,
    /// 마찰 징후: 말하지 않고 뒤에서 앱 프로세스만 미리 깨운다 (premium-discount-mcp 예열)
    PreWarm,
    /// 병목 확정: 끼어들어도 되는 때(raon-zena 의 끼어들기 문)에 먼저 돕는다
    CoachReady,
}

/// 실시간 판독에 넘기는 지금의 활동 상태 ([`LiveEpisodeWindow::probe`] 가 만든다)
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveProbeContext {
    pub now_ms: i64,
    /// 화면에 들어온 뒤 첫 동작 없이 화면을 훑은 시간 (자리 비움 · 개인정보 잠금 동안은 시계가 멈춘다)
    pub active_unacted_search_ms: u32,
    /// 타임트라이얼 고스트의 다음 체크포인트 도착 예정을 넘긴 시간 (`LiveGuide::live_overrun_ms`)
    pub ghost_overrun_ms: u32,
}

/// 기준선을 바꾸지 않고 낸 실시간 판독
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveFrictionReading {
    /// 일어난 사건만의 마찰 합
    pub committed_friction: f64,
    /// 가상 탐색 지연(`Searched`)까지 더한 마찰. 판정 · 경보는 이 값으로 한다
    pub effective_friction: f64,
    pub screen: Judgement,
    /// 마찰이 몰린 영역(없으면 마지막으로 고른 영역)의 판정. 둘 다 없으면 None
    pub region: Option<Judgement>,
    /// 마찰(광클 · 다시 고침 · 머묾)이 몰린 영역 ([`Episode::bottleneck_focus`])
    pub bottleneck_anchor: Option<String>,
    /// 마찰을 가장 많이 낸 신호 (같은 종류는 합친다: `Searched` 는 ms, `Rescrolled` 는 횟수의 합). 마찰이 없으면 None
    pub dominant_signal: Option<Signal>,
    pub stage: FrictionAlertStage,
    /// 기준선이 아직 없어(표본이 하한 미만) 콜드스타트 절대 임계값으로 단계를 정했다
    pub cold_start_fallback: bool,
}

/// 신호 종류마다 마찰을 더해 가장 큰 것 (같으면 마지막 사건이 늦은 것). 가상 탐색은 지금 시각의 `Searched` 로 센다
fn dominant_signal(events: &[Event], virtual_search: Option<u32>, now_ms: i64, p: &BehaviorParams) -> Option<Signal> {
    let mut kinds: Vec<(Signal, f64, i64)> = Vec::new();
    let virtual_event = virtual_search.map(|ms| (Signal::Searched { ms }, now_ms));
    for (signal, at_ms) in events.iter().map(|e| (e.signal, e.at_ms)).chain(virtual_event) {
        let w = weight(&signal, p);
        if w <= 0.0 {
            continue;
        }
        match kinds.iter_mut().find(|k| std::mem::discriminant(&k.0) == std::mem::discriminant(&signal)) {
            Some(k) => {
                k.0 = match (k.0, signal) {
                    (Signal::Searched { ms: a }, Signal::Searched { ms: b }) => Signal::Searched { ms: a.saturating_add(b) },
                    (Signal::Rescrolled { times: a }, Signal::Rescrolled { times: b }) => Signal::Rescrolled { times: a.saturating_add(b) },
                    (same, _) => same,
                };
                k.1 += w;
                k.2 = k.2.max(at_ms);
            }
            None => kinds.push((signal, w, at_ms)),
        }
    }
    kinds.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2))).map(|k| k.0)
}

/// 첫 동작으로 치는 사건: 누름 · 입력 · 제출. 머묾 · 스크롤 뒤집기 · 되돌아가기는 화면을 훑는 활동이다
fn acts(signal: &Signal) -> bool {
    matches!(signal, Signal::Picked | Signal::Engaged | Signal::Submitted | Signal::Retried | Signal::RageClick)
}

/// 진행 중인 에피소드 하나(한 화면). 사건을 받아 실시간 판독([`LiveEpisodeWindow::peek`])에 쓰고, 화면을 떠나거나 제출로
/// 끝나면 끝난 에피소드를 한 번만 내준다([`LiveEpisodeWindow::flush_completed`] → [`FrictionBaselines::decide`]).
///
/// 첫 동작 전 탐색 시계는 활동(포인터 이동 · 스크롤 · 머묾 · 사건) 사이의 간격을 더한다. 간격이 [`IDLE_AFK_CUTOFF_MS`] 보다
/// 길면 자리 비움이라 그 간격은 넣지 않고, 첫 동작(누름 · 입력 · 제출) 뒤와 개인정보 잠금 동안은 시계가 멈춘다.
/// 개인정보 잠금이 시작되면([`LiveEpisodeWindow::begin_privacy_lock`]) 지난 [`PRIVACY_PURGE_MS`] 의 사건을 지우고, 잠금을
/// 걸친 에피소드는 내주지 않는다. 사건을 지울 때 위치 단서 글자는 0 으로 덮어 쓴다(zeroize).
#[derive(Debug, Clone, Default)]
pub struct LiveEpisodeWindow {
    pub cc: String,
    pub screen: String,
    pub entered_at_ms: i64,
    /// 마지막 활동 시각
    pub last_pointer_active_ms: i64,
    /// 이 화면에서 첫 동작을 했다. 그 뒤로 탐색 시계는 멈춘다
    pub acted_on_screen: bool,
    episode: Episode,
    search_ms: i64,
    locked: bool,
    tainted: bool,
}

impl LiveEpisodeWindow {
    /// `now_ms` 에 `cc` 의 화면 `screen` 에 들어왔다
    pub fn new(cc: &str, screen: &str, now_ms: i64) -> LiveEpisodeWindow {
        LiveEpisodeWindow {
            cc: cc.to_string(),
            screen: screen.to_string(),
            entered_at_ms: now_ms,
            last_pointer_active_ms: now_ms,
            ..Default::default()
        }
    }

    /// 지금까지의 (끝나지 않은) 에피소드
    pub fn episode(&self) -> &Episode {
        &self.episode
    }

    pub fn events(&self) -> &[Event] {
        &self.episode.events
    }

    /// 사건이 되지 않는 활동 (포인터 이동 · 스크롤 · 머묾). 첫 동작 전이면 지난 활동부터의 간격을 탐색 시간에 더한다.
    /// 잠금 중에는 받지 않는다
    pub fn note_activity(&mut self, now_ms: i64) {
        if self.locked {
            return;
        }
        let gap = now_ms - self.last_pointer_active_ms;
        if !self.acted_on_screen && gap > 0 && gap <= IDLE_AFK_CUTOFF_MS {
            self.search_ms += gap;
        }
        self.last_pointer_active_ms = self.last_pointer_active_ms.max(now_ms);
    }

    /// 사건 하나를 넣는다. 사건도 활동이고, 누름 · 입력 · 제출은 첫 동작이라 탐색 시계를 멈춘다. `combat_exempt` 면
    /// (전투 중 유효 타격이거나 바깥으로 뻗는 전투 흐름이 함께 났다: raon-zena 의 반응 문맥이 정한다) 광클은 넣지 않는다.
    /// 전투 연타는 UI 마찰이 아니다. 잠금 중이면 버린다. 넣었으면 true
    pub fn push_filtered(&mut self, event: Event, combat_exempt: bool) -> bool {
        if self.locked {
            return false;
        }
        self.note_activity(event.at_ms);
        let exempt = combat_exempt && matches!(event.signal, Signal::RageClick);
        if exempt || acts(&event.signal) {
            self.acted_on_screen = true;
        }
        if exempt {
            return false;
        }
        self.episode.events.push(event);
        true
    }

    /// 첫 동작 없이 화면을 훑은 시간 (자리 비움 · 잠금 · 첫 동작 뒤에는 멈춘 시계)
    pub fn active_unacted_search_ms(&self) -> u32 {
        self.search_ms.clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn probe(&self, now_ms: i64, ghost_overrun_ms: u32) -> LiveProbeContext {
        LiveProbeContext {
            now_ms,
            active_unacted_search_ms: self.active_unacted_search_ms(),
            ghost_overrun_ms,
        }
    }

    /// 지금 에피소드의 실시간 판독 ([`FrictionBaselines::peek_live`]). 잠금 중에는 경보를 내지 않는다(Calm)
    pub fn peek(&self, baselines: &FrictionBaselines, now_ms: i64, ghost_overrun_ms: u32, p: &BehaviorParams) -> LiveFrictionReading {
        let mut reading = baselines.peek_live(&self.cc, &self.screen, &self.episode, &self.probe(now_ms, ghost_overrun_ms), p);
        if self.locked {
            reading.stage = FrictionAlertStage::Calm;
        }
        reading
    }

    /// 키보드 Tab/화살표·UIA 포커스·단일 스위치 입력 등 비포인터 활성 신호를 기록해 AFK 오판을 막는다 (Phase 7 Part 1).
    pub fn note_non_pointer_activity(&mut self, now_ms: i64) {
        self.note_activity(now_ms);
    }

    /// 화면 텍스트 글자 수(`visible_chars`)에 비례한 독해 여유 시간 동안 정체/초과 카운터를 면책한 실시간 판독 (Phase 7 Part 3).
    pub fn peek_with_reading_grace(
        &self,
        baselines: &FrictionBaselines,
        now_ms: i64,
        ghost_overrun_ms: u32,
        visible_chars: usize,
        p: &BehaviorParams,
    ) -> LiveFrictionReading {
        let grace = reading_grace_ms(visible_chars);
        let probe = LiveProbeContext {
            now_ms,
            active_unacted_search_ms: self.active_unacted_search_ms().saturating_sub(grace),
            ghost_overrun_ms: ghost_overrun_ms.saturating_sub(grace),
        };
        let mut reading = baselines.peek_live(&self.cc, &self.screen, &self.episode, &probe, p);
        if self.locked {
            reading.stage = FrictionAlertStage::Calm;
        }
        reading
    }

    /// `cutoff_ms` 와 그 뒤의 사건을 지운다(위치 단서 글자는 0 으로 덮어 쓴 뒤 버린다). 지운 사건 수
    pub fn rollback_recent(&mut self, cutoff_ms: i64) -> usize {
        let before = self.episode.events.len();
        let (mut dropped, kept): (Vec<Event>, Vec<Event>) = std::mem::take(&mut self.episode.events).into_iter().partition(|e| e.at_ms >= cutoff_ms);
        self.episode.events = kept;
        wipe_events(&mut dropped);
        before - self.episode.events.len()
    }

    /// 개인정보 잠금이 시작됐다(privacy epoch): 잠금 [`PRIVACY_PURGE_MS`] 전부터의 사건을 지우고, 이 에피소드는 기록하지 않는다
    /// (기준선 · 결정 기록에 아무것도 들어가지 않는다). 잠금 동안 사건 · 활동은 버리고 탐색 시계는 멈춘다
    pub fn begin_privacy_lock(&mut self, t_lock_ms: i64) {
        self.rollback_recent(t_lock_ms - PRIVACY_PURGE_MS);
        self.locked = true;
        self.tainted = true;
    }

    /// 잠금이 풀렸다(안전 복귀): 잠금을 걸친 에피소드는 버리고, 같은 화면에서 새 에피소드를 시작한다
    pub fn end_privacy_lock(&mut self, now_ms: i64) {
        if !self.locked {
            return;
        }
        self.locked = false;
        let screen = std::mem::take(&mut self.screen);
        self.restart(screen, now_ms);
    }

    pub fn privacy_locked(&self) -> bool {
        self.locked
    }

    /// 끝나면 기준선에 넣어도 되는가 (잠금 중이거나 잠금을 걸친 에피소드면 false)
    pub fn recordable(&self) -> bool {
        !self.locked && !self.tainted
    }

    /// 화면을 떠났거나 제출로 끝났다: 끝난 에피소드를 내주고(기준선에는 이것만 한 번 넣는다) `next_screen` 에서 새로 시작한다.
    /// 사건이 없거나 기록하면 안 되는 에피소드면 None 이고, 그 사건은 지운다
    pub fn flush_completed(&mut self, next_screen: &str, now_ms: i64) -> Option<Episode> {
        let recordable = self.recordable();
        let mut done = std::mem::take(&mut self.episode);
        self.restart(next_screen.to_string(), now_ms);
        if recordable && !done.events.is_empty() {
            return Some(done);
        }
        wipe_events(&mut done.events);
        None
    }

    fn restart(&mut self, screen: String, now_ms: i64) {
        wipe_events(&mut self.episode.events);
        self.screen = screen;
        self.entered_at_ms = now_ms;
        self.last_pointer_active_ms = now_ms;
        self.acted_on_screen = false;
        self.search_ms = 0;
        // 잠금 중에 시작한 에피소드도 잠금을 걸친다
        self.tainted = self.locked;
    }
}

/// 사건의 위치 단서를 0 으로 덮고(zeroize, premium-discount-identity) 사건을 비운다
fn wipe_events(events: &mut Vec<Event>) {
    for e in events.iter_mut() {
        if let Some(anchor) = e.anchor.as_mut() {
            wipe_string(anchor);
        }
    }
    events.clear();
}

/// Phase 7 Part 3: Calm Compass 독해 여유 시간(글자당 120 ms).
pub const READING_MS_PER_CHAR: u32 = 120;

/// 화면 가시 텍스트 글자 수(`visible_chars`)에 비례한 독해 면책 시간(ms, 최대 120초).
pub fn reading_grace_ms(visible_chars: usize) -> u32 {
    (visible_chars as u64).saturating_mul(u64::from(READING_MS_PER_CHAR)).min(120_000) as u32
}

/// Phase 4 Part 3: 화면·장면 전환 직후 1-Hop 역추적 관찰 창(8초)과 감쇠 계수(0.85).
pub const CROSS_BOUNDARY_BOUNCE_WINDOW_MS: i64 = 8_000;
pub const CROSS_BOUNDARY_HOP_DECAY: f64 = 0.85;

/// 비DOM(무확장 UIA · 데스크톱 앱 · 인게임 배지) 범용 선택-마찰 교차 분류 (Phase 4 Part 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UniversalChoiceKind {
    /// 고선호 + 저마찰 완주 → Premium (+1)
    GoldenPath,
    /// 고마찰 / 즉발 역류 / 패배 → Discount (-1 거부권)
    FrictionTrap,
    /// 저선호이나 무마찰 정상 동작 → Neutral (억울한 Discount 오염 방지)
    BenignIgnored,
    /// 표본 부족
    UnratedCold,
}

impl UniversalChoiceKind {
    pub fn tier(self) -> Tier {
        match self {
            UniversalChoiceKind::GoldenPath => Tier::Premium,
            UniversalChoiceKind::FrictionTrap => Tier::Discount,
            UniversalChoiceKind::BenignIgnored => Tier::Neutral,
            UniversalChoiceKind::UnratedCold => Tier::Unrated,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UniversalChoiceEntry {
    pub picks: u32,
    pub exposures: u32,
    pub expected: f64,
    pub variance: f64,
    pub post_windows: u32,
    pub post_friction_sum: f64,
    pub abandon_or_defeat: u32,
}

impl UniversalChoiceEntry {
    pub fn z_choice(&self) -> Option<f64> {
        (self.variance > 1e-6).then(|| (f64::from(self.picks) - self.expected) / self.variance.sqrt())
    }

    pub fn mean_post_friction(&self) -> f64 {
        if self.post_windows == 0 {
            0.0
        } else {
            self.post_friction_sum / f64::from(self.post_windows)
        }
    }

    pub fn fail_rate(&self) -> f64 {
        if self.post_windows == 0 {
            0.0
        } else {
            f64::from(self.abandon_or_defeat) / f64::from(self.post_windows)
        }
    }
}

/// 무확장 UIA · 네이티브 앱 · 인게임 가시 후보 풀(`visible_regions`, 최대 16개)에 대한 범용 포아송-이항 선택 대조 및
/// 1-Hop 화면 전환 역추적 마찰 장부 (Phase 4 Part 3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UniversalChoiceBook {
    pub entries: BTreeMap<String, UniversalChoiceEntry>,
    pub post_baseline: Welford,
}

impl UniversalChoiceBook {
    /// 가시 후보 풀(`visible_keys`, 최대 16개)에서 `chosen` 을 선택했다.
    /// `vertical_list` 면 `(k + 1)^-0.35` 위치 감쇠 사전분포, 가로 탭·인게임 갈림길이면 균등 `1/n` 사전분포를 쓴다.
    pub fn observe_pick(&mut self, chosen: &str, visible_keys: &[&str], vertical_list: bool) -> bool {
        let mut pool: Vec<&str> = Vec::with_capacity(visible_keys.len().min(16) + 1);
        for &k in visible_keys.iter().take(16) {
            if !pool.contains(&k) {
                pool.push(k);
            }
        }
        if !pool.contains(&chosen) && pool.len() < 16 {
            pool.push(chosen);
        }
        if pool.len() < 2 {
            return false;
        }
        let weights: Vec<f64> = (0..pool.len())
            .map(|k| if vertical_list { ((k + 1) as f64).powf(-0.35) } else { 1.0 })
            .collect();
        let wsum: f64 = weights.iter().sum::<f64>().max(1e-9);
        for (&key, w) in pool.iter().zip(&weights) {
            let p = *w / wsum;
            let e = self.entries.entry(key.to_string()).or_default();
            e.exposures += 1;
            e.expected += p;
            e.variance += p * (1.0 - p);
            if key == chosen {
                e.picks += 1;
            }
        }
        true
    }

    /// 선택 `chosen` 직후 같은 화면 마찰(`same_screen_friction`)과 다음 화면 도착 초기(`arrival_dwell_ms <= 8s`)
    /// 즉발 역류·포기·패배 마찰(`arrival_friction`)을 `γ_hop = 0.85` 로 역귀속 합산한다.
    pub fn observe_post_pick_1hop(
        &mut self,
        chosen: &str,
        same_screen_friction: f64,
        arrival_dwell_ms: Option<i64>,
        arrival_bounce_or_defeat: bool,
        arrival_friction: f64,
    ) {
        let hop_triggered = arrival_bounce_or_defeat
            && arrival_dwell_ms.is_some_and(|d| (0..=CROSS_BOUNDARY_BOUNCE_WINDOW_MS).contains(&d));
        let total_friction = same_screen_friction.max(0.0)
            + if hop_triggered { CROSS_BOUNDARY_HOP_DECAY * arrival_friction.max(0.0) } else { 0.0 };
        let e = self.entries.entry(chosen.to_string()).or_default();
        e.post_windows += 1;
        e.post_friction_sum += total_friction;
        if hop_triggered {
            e.abandon_or_defeat += 1;
        }
        self.post_baseline.push(total_friction);
    }

    /// 선택 선호도와 1-Hop 인과 마찰 거부권을 결합해 [`UniversalChoiceKind`] 를 판정한다.
    pub fn classify(&self, key: &str) -> UniversalChoiceKind {
        let Some(e) = self.entries.get(key) else {
            return UniversalChoiceKind::UnratedCold;
        };
        let mean_f = e.mean_post_friction();
        let post_z = if self.post_baseline.n >= 3 {
            Some((mean_f - self.post_baseline.mean) / (self.post_baseline.population_sd() + 0.25))
        } else {
            None
        };
        let high_friction = post_z.is_some_and(|z| z >= 1.0) || (e.post_windows >= 2 && e.fail_rate() >= 0.5);
        if high_friction && (e.picks >= 2 || e.post_windows >= 2) {
            return UniversalChoiceKind::FrictionTrap;
        }
        if e.exposures < 5 {
            return UniversalChoiceKind::UnratedCold;
        }
        match e.z_choice() {
            Some(z) if z >= 2.0 && e.picks >= 3 => UniversalChoiceKind::GoldenPath,
            Some(z) if z <= -2.0 && e.expected >= 2.5 => UniversalChoiceKind::BenignIgnored,
            _ => UniversalChoiceKind::UnratedCold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(at_ms: i64, signal: Signal) -> Event {
        Event { at_ms, signal, anchor: None }
    }

    fn episode(signals: &[Signal]) -> Episode {
        Episode {
            events: signals.iter().enumerate().map(|(i, s)| ev(i as i64 * 1000, *s)).collect(),
        }
    }

    fn at(at_ms: i64, signal: Signal, anchor: &str) -> Event {
        Event {
            at_ms,
            signal,
            anchor: Some(anchor.into()),
        }
    }

    /// 같은 앱의 다른 화면에서 보통 마찰(1 · 2 · 3, 평균 2)의 에피소드 n개
    fn peers(b: &mut FrictionBaselines, cc: &str, n: usize, p: &BehaviorParams) {
        for i in 0..n {
            let signals: &[Signal] = match i % 3 {
                0 => &[Signal::Backtracked],
                1 => &[Signal::Retried],
                _ => &[Signal::Retried, Signal::Backtracked],
            };
            b.decide(cc, &format!("0xpeer{i}"), None, &episode(signals), p);
        }
    }

    /// 기준선의 모든 분포를 (이름, n, mean 비트, m2 비트)로
    fn ledger_bits(b: &FrictionBaselines) -> Vec<(String, u64, u64, u64)> {
        let mut out = vec![("global".to_string(), b.global.n, b.global.mean.to_bits(), b.global.m2.to_bits())];
        for (scope, map) in [("region", &b.regions), ("screen", &b.screens), ("app", &b.apps)] {
            out.extend(map.iter().map(|(k, w)| (format!("{scope}:{k}"), w.n, w.mean.to_bits(), w.m2.to_bits())));
        }
        out.sort();
        out
    }

    #[test]
    fn pf01_peek_live_leaves_the_ledger_untouched_and_matches_decide() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        peers(&mut b, "shop.example", 10, &p);
        let live = Episode {
            events: vec![
                at(0, Signal::Picked, "#1"),
                at(1_000, Signal::Engaged, "#2"),
                at(2_000, Signal::Retried, "#2"),
                at(3_000, Signal::Hovered, "#3"),
            ],
        };
        let probe = LiveProbeContext {
            now_ms: 3_500,
            ..Default::default()
        };
        let (before, snapshot) = (ledger_bits(&b), b.clone());
        let first = b.peek_live("shop.example", "0xlive", &live, &probe, &p);
        for _ in 1..50 {
            assert_eq!(b.peek_live("shop.example", "0xlive", &live, &probe, &p), first);
        }
        assert_eq!(ledger_bits(&b), before, "50번 읽어도 n · mean · m2 가 비트 단위로 같다");
        assert_eq!(b, snapshot);

        // decide 가 넣기 직전에 낼 z 와 같다 (화면 · 영역 모두)
        assert_eq!(first.bottleneck_anchor.as_deref(), Some("#2"));
        let decided = b.clone().decide("shop.example", "0xlive", first.bottleneck_anchor.as_deref(), &live, &p);
        let (live_z, decided_z) = (first.screen.z.expect("같은 앱 피어 10개가 기준선이다"), decided.screen.z.unwrap());
        assert!((live_z - decided_z).abs() < 1e-12, "{live_z} vs {decided_z}");
        assert_eq!(first.screen.level, decided.screen.level);
        let (live_rz, decided_rz) = (first.region.unwrap().z.unwrap(), decided.region.unwrap().z.unwrap());
        assert!((live_rz - decided_rz).abs() < 1e-12);
        assert_eq!((first.committed_friction, first.effective_friction), (decided.friction, decided.friction), "가상 탐색이 없으면 같다");
    }

    #[test]
    fn pf02_friction_without_a_pick_names_its_own_region_and_combat_bursts_are_exempt() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        peers(&mut b, "shop.example", 12, &p);
        // A: #1 을 정상으로 고른 뒤 #2 에서 입력 · 다시 고침 두 번 · 광클 (번호는 다시 고르지 않았다)
        let mut w = LiveEpisodeWindow::new("shop.example", "0xcoupon", 0);
        for e in [
            at(1_000, Signal::Picked, "#1"),
            at(2_000, Signal::Engaged, "#2"),
            at(3_000, Signal::Retried, "#2"),
            at(4_000, Signal::Retried, "#2"),
            at(4_500, Signal::RageClick, "#2"),
        ] {
            assert!(w.push_filtered(e, false));
        }
        assert_eq!(w.episode().focus(), Some("#1"), "focus 는 고른 영역만 본다");
        assert_eq!(w.episode().bottleneck_focus(&p), Some("#2"));
        let r = w.peek(&b, 5_000, 0, &p);
        assert_eq!(r.bottleneck_anchor.as_deref(), Some("#2"), "#1 로 잘못 돌리지 않는다");
        assert_eq!(r.stage, FrictionAlertStage::CoachReady, "{r:?}");
        assert!(!r.cold_start_fallback);
        assert_eq!(r.dominant_signal, Some(Signal::Retried), "다시 고침 4.0 > 광클 3.0");

        // B: 전투 중(유효 타격) 3연타는 광클로 넣지 않는다
        let mut combat = LiveEpisodeWindow::new("game.exe", "0xboss", 0);
        for t in [100, 300, 500] {
            assert!(!combat.push_filtered(at(t, Signal::RageClick, "cell:4,4"), true));
        }
        assert!(combat.events().is_empty());
        assert_eq!(combat.peek(&b, 600, 0, &p).stage, FrictionAlertStage::Calm);
        assert_eq!(combat.peek(&FrictionBaselines::default(), 600, 0, &p).stage, FrictionAlertStage::Calm);
        // 전투가 아니면 같은 연타는 광클이다
        assert!(combat.push_filtered(at(700, Signal::RageClick, "cell:4,4"), false));
        assert_eq!(combat.events().len(), 1);
    }

    #[test]
    fn pf03_active_search_before_any_action_counts_but_afk_time_does_not() {
        let p = BehaviorParams::default();
        let empty = FrictionBaselines::default();
        // A: 빈 기준선. 14초 동안 포인터만 움직이며 누르지 않다가 광클 한 번
        let mut w = LiveEpisodeWindow::new("new.example", "0xfirst", 0);
        for t in (500..=14_000).step_by(500) {
            w.note_activity(t);
        }
        assert_eq!(w.active_unacted_search_ms(), 14_000);
        let searching = w.peek(&empty, 14_000, 0, &p);
        assert_eq!((searching.committed_friction, searching.effective_friction), (0.0, 7.0), "가상 Searched 14초 = 0.5 × 14");
        assert!(w.events().is_empty(), "사건 목록은 그대로다");
        assert!(w.push_filtered(at(14_000, Signal::RageClick, "#apply"), false));
        w.note_activity(15_000);
        assert_eq!(w.active_unacted_search_ms(), 14_000, "첫 동작에서 탐색 시계가 멈춘다");
        let r = w.peek(&empty, 15_000, 0, &p);
        assert!(r.cold_start_fallback);
        assert!(r.stage >= FrictionAlertStage::PreWarm);
        assert_eq!(r.stage, FrictionAlertStage::CoachReady, "{r:?}");
        assert_eq!(r.effective_friction, 10.0);
        assert_eq!(r.dominant_signal, Some(Signal::Searched { ms: 14_000 }));

        // B: 포인터가 멈춘 채(자리 비움) 60초
        let mut afk = LiveEpisodeWindow::new("new.example", "0xfirst", 0);
        let r = afk.peek(&empty, 60_000, 0, &p);
        assert_eq!((r.stage, r.effective_friction), (FrictionAlertStage::Calm, 0.0));
        afk.note_activity(60_000);
        assert_eq!(afk.active_unacted_search_ms(), 0, "돌아와 움직여도 60초 간격은 넣지 않는다");
        assert_eq!(afk.peek(&empty, 60_000, 0, &p).stage, FrictionAlertStage::Calm);
    }

    #[test]
    fn the_search_clock_freezes_when_idle_and_resumes_on_activity() {
        let mut w = LiveEpisodeWindow::new("a.example", "0xs", 0);
        w.note_activity(5_000);
        w.note_activity(30_000); // 25초 간격: 자리 비움
        assert_eq!(w.active_unacted_search_ms(), 5_000);
        w.note_activity(36_000);
        assert_eq!(w.active_unacted_search_ms(), 11_000);
        w.note_activity(35_000); // 늦게 온 활동은 시계를 되돌리지 않는다
        assert_eq!((w.active_unacted_search_ms(), w.last_pointer_active_ms), (11_000, 36_000));
        // 머묾 · 스크롤 뒤집기는 활동이지만 첫 동작은 아니다
        w.push_filtered(at(38_000, Signal::Hovered, "#menu"), false);
        assert!(!w.acted_on_screen);
        assert_eq!(w.active_unacted_search_ms(), 13_000);
        w.push_filtered(at(39_000, Signal::Picked, "#menu"), false);
        w.note_activity(40_000);
        assert!(w.acted_on_screen);
        assert_eq!(w.active_unacted_search_ms(), 14_000);
    }

    #[test]
    fn a_ghost_overrun_confirms_a_search_stall() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        peers(&mut b, "shop.example", 12, &p);
        let mut w = LiveEpisodeWindow::new("shop.example", "0xmaze", 0);
        for t in (1_000..=10_000).step_by(1_000) {
            w.note_activity(t);
        }
        // 탐색 10초(마찰 5.0)는 z 로 띠를 넘지만 복합 징후(12초)가 아니다
        assert_eq!(w.peek(&b, 10_000, 0, &p).stage, FrictionAlertStage::PreWarm);
        // 고스트보다 2.5초 늦었다: 지연 10 + 2.5 ≥ 12초
        assert_eq!(w.peek(&b, 10_000, 2_500, &p).stage, FrictionAlertStage::CoachReady);
        // 첫 동작 때 잰 Searched 가 사건에 있으면 가상 탐색을 겹쳐 더하지 않는다
        w.push_filtered(at(10_000, Signal::Searched { ms: 10_000 }, "0xmaze"), false);
        let r = w.peek(&b, 10_000, 0, &p);
        assert_eq!((r.committed_friction, r.effective_friction), (5.0, 5.0));
    }

    #[test]
    fn coach_ready_needs_a_friction_floor_even_when_the_baseline_is_flat() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        for i in 0..12 {
            b.decide("calm.example", &format!("0xpeer{i}"), None, &episode(&[Signal::Picked, Signal::Submitted]), &p);
        }
        // 피어가 모두 0 이라 머묾 한 번(0.5)도 z = 2 지만, 마찰이 2.5 아래면 코칭하지 않는다
        let hover = Episode {
            events: vec![at(0, Signal::Hovered, "#x")],
        };
        let rage = Episode {
            events: vec![at(0, Signal::RageClick, "#x")],
        };
        let probe = LiveProbeContext::default();
        let r = b.peek_live("calm.example", "0xlive", &hover, &probe, &p);
        assert!(r.screen.z.unwrap() >= p.live.coach_band * p.band);
        assert_eq!(r.stage, FrictionAlertStage::PreWarm);
        assert_eq!(b.peek_live("calm.example", "0xlive", &rage, &probe, &p).stage, FrictionAlertStage::CoachReady);
    }

    #[test]
    fn bottleneck_focus_weighs_friction_then_recency() {
        let p = BehaviorParams::default();
        let ep = Episode {
            events: vec![
                at(0, Signal::Retried, "#a"),
                at(1_000, Signal::Hovered, "#b"),
                at(2_000, Signal::Backtracked, "#b"),
                at(2_500, Signal::Hovered, "#b"),
                at(3_000, Signal::Picked, "#c"),
            ],
        };
        // #a 2.0, #b 2.0: 같으면 마지막 마찰이 늦은 #b
        assert_eq!(ep.bottleneck_focus(&p), Some("#b"));
        let mut heavier = ep.clone();
        heavier.events.push(at(4_000, Signal::Hovered, "#a"));
        assert_eq!(heavier.bottleneck_focus(&p), Some("#a"));
        // 고르기만 했거나 위치 단서가 없으면 병목이 아니다
        let calm = Episode {
            events: vec![at(0, Signal::Picked, "#c"), ev(1_000, Signal::RageClick)],
        };
        assert_eq!(calm.bottleneck_focus(&p), None);
        // 실시간 판독은 화면 해시와 같은 위치 단서(페이지 전체)를 병목으로 보지 않는다
        let page = Episode {
            events: vec![at(0, Signal::RageClick, "0xpage"), at(500, Signal::Hovered, "#d")],
        };
        let r = FrictionBaselines::default().peek_live("a.example", "0xpage", &page, &LiveProbeContext::default(), &p);
        assert_eq!(r.bottleneck_anchor.as_deref(), Some("#d"));
        assert_eq!(page.bottleneck_focus(&p), Some("0xpage"));
    }

    #[test]
    fn ps08_privacy_lock_purges_the_last_five_seconds_and_records_nothing() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        peers(&mut b, "bank.example", 12, &p);
        let t_lock = 60_000;
        let mut w = LiveEpisodeWindow::new("bank.example", "0xtransfer", 40_000);
        for e in [
            at(t_lock - 9_000, Signal::Picked, "main>a#menu"),
            at(t_lock - 3_000, Signal::Picked, "keypad>btn7"),
            at(t_lock - 2_000, Signal::Retried, "form>input#pin"),
            at(t_lock - 1_000, Signal::RageClick, "keypad>btn2"),
        ] {
            assert!(w.push_filtered(e, false));
        }
        let before = ledger_bits(&b);
        w.begin_privacy_lock(t_lock);
        assert!(w.events().iter().all(|e| e.at_ms < t_lock - PRIVACY_PURGE_MS), "지난 5초 사건은 지운다: {:?}", w.events());
        assert!(w.events().iter().all(|e| !e.anchor.as_deref().unwrap_or("").contains("keypad")));
        assert_eq!(w.events().len(), 1);
        assert!(w.privacy_locked() && !w.recordable());
        // 잠금 동안 들어오는 사건 · 활동은 버리고, 경보도 내지 않는다
        assert!(!w.push_filtered(at(t_lock + 500, Signal::RageClick, "keypad>btn5"), false));
        let searched = w.active_unacted_search_ms();
        w.note_activity(t_lock + 1_000);
        assert_eq!(w.active_unacted_search_ms(), searched, "잠금 동안 탐색 시계는 멈춘다");
        assert_eq!(w.peek(&b, t_lock + 1_000, 0, &p).stage, FrictionAlertStage::Calm);
        // 화면을 떠나도 잠금을 걸친 에피소드는 내주지 않는다: 기준선 · 결정 기록에 0건
        assert_eq!(w.flush_completed("0xdone", t_lock + 2_000), None);
        assert_eq!(ledger_bits(&b), before);
        // 풀리면 새 에피소드: 잠금 전 사건은 남지 않고, 잠금 뒤 사건만 기록된다
        w.end_privacy_lock(t_lock + 8_000);
        assert!(!w.privacy_locked() && w.recordable() && w.events().is_empty());
        assert_eq!((w.screen.as_str(), w.entered_at_ms), ("0xdone", t_lock + 8_000));
        assert!(w.push_filtered(at(t_lock + 9_000, Signal::Submitted, "form>button"), false));
        let after = w.flush_completed("0xnext", t_lock + 9_500).expect("잠금 뒤 에피소드는 기록한다");
        assert_eq!(after.events.len(), 1);
        assert_eq!(after.events[0].anchor.as_deref(), Some("form>button"));
    }

    #[test]
    fn rollback_recent_drops_events_from_the_cutoff_on() {
        let mut w = LiveEpisodeWindow::new("a.example", "0xs", 0);
        for t in [1_000, 2_000, 3_000] {
            w.push_filtered(at(t, Signal::Hovered, "#h"), false);
        }
        assert_eq!(w.rollback_recent(2_000), 2);
        assert_eq!(w.events().iter().map(|e| e.at_ms).collect::<Vec<_>>(), [1_000]);
        assert_eq!(w.rollback_recent(10_000), 0);
        assert!(w.recordable(), "되돌리기만으로는 기록을 막지 않는다");
    }

    #[test]
    fn a_live_window_hands_each_episode_over_once() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        let mut w = LiveEpisodeWindow::new("shop.example", "0xcart", 0);
        w.push_filtered(at(1_000, Signal::Picked, "#pay"), false);
        w.push_filtered(at(2_000, Signal::Submitted, "#pay"), false);
        for _ in 0..20 {
            w.peek(&b, 2_500, 0, &p);
        }
        assert_eq!(b.global.n, 0, "읽기만 해서는 쌓이지 않는다");
        let done = w.flush_completed("0xdone", 3_000).expect("끝난 에피소드");
        b.decide(&w.cc, "0xcart", done.focus(), &done, &p);
        assert_eq!(b.global.n, 1);
        assert_eq!(w.flush_completed("0xnext", 4_000), None, "사건이 없으면 내주지 않는다");
        assert_eq!((w.screen.as_str(), w.acted_on_screen, w.active_unacted_search_ms()), ("0xnext", false, 0));
    }

    #[test]
    fn freezes_are_taken_out_of_episode_gaps() {
        let p = BehaviorParams::default();
        let times = [0, 1_000, 2_000, 3_000, 60_000, 61_000, 62_000];
        let events: Vec<Event> = times.iter().map(|t| ev(*t, Signal::Picked)).collect();
        // 동결 구간이 없으면 split_episodes 와 같다
        assert_eq!(split_episodes_with_freezes(&events, |_, _| 1.0, &p, &[]), split_episodes(&events, |_, _| 1.0, &p));
        // 6분 본인인증이 끼어도 (잠금 구간을 빼면 2초 간격) 한 에피소드다
        let auth = [ev(0, Signal::Picked), ev(1_000, Signal::Engaged), ev(1_000 + 362_000, Signal::Submitted)];
        assert_eq!(split_episodes(&auth, |_, _| 1.0, &p).len(), 2);
        let frozen = [(1_500, 1_500 + 360_000)];
        assert_eq!(split_episodes_with_freezes(&auth, |_, _| 1.0, &p, &frozen).len(), 1);
        // 3초짜리 동결이 긴 간격을 지우면 간격 z 로도 자르지 않는다
        let one = split_episodes_with_freezes(&events, |_, _| 1.0, &p, &[(3_500, 59_500)]);
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn behavior_params_without_live_thresholds_still_load() {
        let mut json = serde_json::to_value(BehaviorParams::default()).unwrap();
        json.as_object_mut().unwrap().remove("live");
        let p: BehaviorParams = serde_json::from_value(json).unwrap();
        assert_eq!(p, BehaviorParams::default());
        let stage = serde_json::to_value(FrictionAlertStage::CoachReady).unwrap();
        assert_eq!(stage, serde_json::json!("coach_ready"));
    }

    #[test]
    fn friction_and_success() {
        let p = BehaviorParams::default();
        let smooth = episode(&[Signal::Picked, Signal::Engaged, Signal::Submitted]);
        assert_eq!((smooth.friction(&p), smooth.succeeded()), (0.0, true));
        let rescrolled = episode(&[Signal::Rescrolled { times: 3 }, Signal::Picked, Signal::Submitted]);
        assert_eq!((rescrolled.friction(&p), rescrolled.succeeded()), (1.5, true));
        let rough = episode(&[Signal::Picked, Signal::RageClick, Signal::Engaged, Signal::Abandoned]);
        assert_eq!((rough.friction(&p), rough.succeeded()), (6.0, false));
        let slow = episode(&[Signal::Searched { ms: 4000 }, Signal::Hovered]);
        assert_eq!(slow.friction(&p), 2.5);
        let mut picked = smooth.clone();
        picked.events[0].anchor = Some("form>input#q".into());
        assert_eq!(picked.focus(), Some("form>input#q"));
    }

    #[test]
    fn episodes_split_on_long_gaps() {
        let p = BehaviorParams::default();
        let times = [0, 1000, 2000, 3000, 60_000, 61_000, 62_000];
        let events: Vec<Event> = times.iter().map(|t| ev(*t, Signal::Picked)).collect();
        let eps = split_episodes(&events, |_, _| 1.0, &p);
        assert_eq!(eps.iter().map(|e| e.events.len()).collect::<Vec<_>>(), [4, 3]);
        // 사건이 적으면 간격 z 규칙은 쓰지 않고, 5분이 넘는 간격에서만 자른다
        let few = [ev(0, Signal::Picked), ev(1000, Signal::Engaged), ev(400_000, Signal::Picked)];
        assert_eq!(split_episodes(&few, |_, _| 1.0, &p).len(), 2);
        assert!(split_episodes(&[], |_, _| 1.0, &p).is_empty());
        // 의미 거리가 0 이면(같은 일) 간격이 길어도 z 규칙으로는 자르지 않는다
        assert_eq!(split_episodes(&events, |_, _| 0.0, &p).len(), 1);
    }

    #[test]
    fn decisions_need_a_baseline_and_must_leave_the_band() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        let first = b.decide("example.com", "0xscreen", None, &episode(&[Signal::Picked, Signal::Submitted]), &p);
        assert_eq!((first.screen.mark, first.screen.level), (None, None), "기준선이 없으면 결정하지 않는다");
        assert_eq!(first.region, None);

        // 다른 화면 · 앱에서 보통 마찰(1 ~ 3)의 에피소드 12개 → 전역 기준선만 생긴다
        for i in 0..12 {
            let signals: &[Signal] = match i % 3 {
                0 => &[Signal::Backtracked],
                1 => &[Signal::Retried],
                _ => &[Signal::Retried, Signal::Backtracked],
            };
            b.decide(&format!("app{i}.example"), &format!("0xother{i}"), None, &episode(signals), &p);
        }
        let smooth = b.decide("new.example", "0xnew", None, &episode(&[Signal::Picked, Signal::Engaged, Signal::Submitted]), &p);
        assert_eq!((smooth.screen.mark, smooth.screen.level), (Some(MarkType::Premium), Some(Level::Global)));
        let rough = b.decide("new.example", "0xnew", None, &episode(&[Signal::RageClick, Signal::RageClick, Signal::Abandoned]), &p);
        assert_eq!(rough.screen.mark, Some(MarkType::Discount));
        let usual = b.decide("new.example", "0xnew", None, &episode(&[Signal::Retried, Signal::Engaged]), &p);
        assert_eq!(usual.screen.mark, None, "띠 안이면 결정하지 않는다: {usual:?}");
        // 성공하지 못했으면 마찰이 낮아도 premium 이 아니다
        let quiet_fail = b.decide("new.example", "0xnew", None, &episode(&[Signal::Picked]), &p);
        assert_ne!(quiet_fail.screen.mark, Some(MarkType::Premium));
    }

    #[test]
    fn a_screen_that_is_always_smooth_keeps_its_premium() {
        // 화면은 자기 자신이 아니라 다른 화면과 견준다: 늘 매끄러운 화면은 계속 premium 이다
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        for i in 0..12 {
            b.decide("other.example", &format!("0xother{i}"), None, &episode(&[Signal::Retried, Signal::Backtracked]), &p);
        }
        for _ in 0..10 {
            let d = b.decide("shop.example", "0xsmooth", None, &episode(&[Signal::Picked, Signal::Submitted]), &p);
            assert_eq!(d.screen.mark, Some(MarkType::Premium), "{d:?}");
        }
        assert_eq!(b.screens["0xsmooth"].n, 10);
    }

    #[test]
    fn regions_are_compared_with_the_other_regions_of_their_screen() {
        let p = BehaviorParams::default();
        let mut b = FrictionBaselines::default();
        for _ in 0..8 {
            b.decide("example.com", "0xcart", Some("0xbutton-a"), &episode(&[Signal::Retried, Signal::Retried]), &p);
        }
        let d = b.decide("example.com", "0xcart", Some("0xbutton-b"), &episode(&[Signal::Picked, Signal::Submitted]), &p);
        let region = d.region.expect("영역을 넘겼으면 영역도 결정한다");
        assert_eq!((region.mark, region.level), (Some(MarkType::Premium), Some(Level::Secondary)));
        // 화면은 다른 화면이 없어 기준선이 없다
        assert_eq!(d.screen.level, None);
        // 늘 마찰이 큰 영역은, 같은 화면의 다른 영역이 쌓이면 discount
        for _ in 0..8 {
            b.decide("example.com", "0xcart", Some("0xbutton-b"), &episode(&[Signal::Picked, Signal::Submitted]), &p);
        }
        let a = b.decide("example.com", "0xcart", Some("0xbutton-a"), &episode(&[Signal::Retried, Signal::Retried]), &p);
        assert_eq!(a.region.unwrap().mark, Some(MarkType::Discount));
    }

    #[test]
    fn welford_matches_the_population_sd_and_can_drop_a_part() {
        let mut w = Welford::default();
        for x in [1.0, -1.0, 1.0, -1.0] {
            w.push(x);
        }
        assert_eq!((w.n, w.mean), (4, 0.0));
        assert!((w.population_sd() - 1.0).abs() < 1e-12);

        let (a, b) = ([0.5, 2.0, 3.5, 1.0, 4.0], [10.0, 12.0, 9.0]);
        let (mut all, mut part, mut rest) = (Welford::default(), Welford::default(), Welford::default());
        for x in a.iter().chain(b.iter()) {
            all.push(*x);
        }
        for x in b {
            part.push(x);
        }
        for x in a {
            rest.push(x);
        }
        let got = all.without(&part);
        assert_eq!(got.n, rest.n);
        assert!((got.mean - rest.mean).abs() < 1e-12 && (got.population_sd() - rest.population_sd()).abs() < 1e-9, "{got:?} vs {rest:?}");
        assert_eq!(all.without(&Welford::default()), all);
        assert_eq!(part.without(&all).n, 0);
    }

    #[test]
    fn universal_choice_book_1hop_trap_and_benign_ignored_and_reading_grace() {
        let mut book = UniversalChoiceBook::default();
        let pool = ["btn_trap", "btn_safe", "btn_rare", "btn_aux"];
        // Users click btn_trap heavily (attractive dark pattern), then bounce/die in 3s on arrival!
        for _ in 0..8 {
            book.observe_pick("btn_trap", &pool, false);
            book.observe_post_pick_1hop("btn_trap", 0.0, Some(3_000), true, 4.0);
        }
        for _ in 0..6 {
            book.observe_pick("btn_safe", &pool, false);
            book.observe_post_pick_1hop("btn_safe", 0.0, Some(12_000), false, 0.0);
        }
        assert_eq!(book.classify("btn_trap"), UniversalChoiceKind::FrictionTrap);
        assert_eq!(book.classify("btn_rare"), UniversalChoiceKind::BenignIgnored);

        // Non-pointer activity keeps exploration clock alive; reading grace suppresses stall alerts.
        let p = BehaviorParams::default();
        let baselines = FrictionBaselines::default();
        let mut win = LiveEpisodeWindow::new("app.exe", "0xpage", 0);
        win.note_non_pointer_activity(5_000);
        win.note_non_pointer_activity(10_000);
        win.note_non_pointer_activity(16_000);
        assert_eq!(win.active_unacted_search_ms(), 16_000);
        let raw = win.peek(&baselines, 16_000, 0, &p);
        let calm = win.peek_with_reading_grace(&baselines, 16_000, 0, 200, &p);
        assert!(raw.effective_friction > calm.effective_friction);
        assert_eq!(calm.stage, FrictionAlertStage::Calm);
    }
}
