//! 행동 기록 → 에피소드 → SDS 결정 (기기 안)
//!
//! 번호 선택 · 이벤트 카메라 · form change 에서 나온 사건을 에피소드로 자르고([`split_episodes`]), 에피소드마다
//! 마찰 점수와 성공 여부를 구한다. 그 값을 같은 스코프 피어의 지난 에피소드 분포(Welford)와 견줘, SDS 띠를 넘을 때만
//! premium 또는 discount 로 결정한다([`FrictionBaselines::decide`]). 화면은 같은 앱 · 사이트의 다른 화면 → 전역과,
//! 영역은 같은 화면의 다른 영역 → 앱 → 전역과 견준다. 판정하는 항목 자신의 에피소드는 기준선에서 뺀다: 늘 매끄러운
//! 화면이 자기 자신과 견줘져 결정이 사라지지 않게 하기 위해서다. 띠 안이거나 기준선이 아직 없으면 결정하지 않는다.
//! 늘 결정할 수는 없고, 결정이 난 것만 서버 기록([`crate::NavMark`])이 된다.
//! 사건에는 입력값 · 화면 글자가 없다. 마찰 가중치는 초깃값이고 실측으로 고친다.

use std::collections::HashMap;

use premium_discount_verdict::{Level, MinObs, Params, NAV};
use serde::{Deserialize, Serialize};

use crate::mark::MarkType;

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
        }
    }
}

/// 같은 일을 하려던 사건 묶음
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        self.events
            .iter()
            .map(|e| match e.signal {
                Signal::RageClick => p.rage_click,
                Signal::Abandoned => p.abandoned,
                Signal::Retried => p.retried,
                Signal::Backtracked => p.backtracked,
                Signal::Hovered => p.hovered,
                Signal::Searched { ms } => p.search_per_s * ms as f64 / 1000.0,
                Signal::Rescrolled { times } => p.rescroll * times as f64,
                Signal::Picked | Signal::Engaged | Signal::Submitted => 0.0,
            })
            .sum()
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
    let Some(first) = events.first() else {
        return Vec::new();
    };
    let gaps: Vec<f64> = events.windows(2).map(|w| (w[1].at_ms - w[0].at_ms).max(0) as f64).collect();
    let mut cut: Vec<bool> = gaps.iter().map(|g| *g > p.max_gap_ms as f64).collect();
    if events.len() >= p.min_events_to_split {
        let (gap_mean, gap_sd) = mean_sd(&gaps);
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
}
