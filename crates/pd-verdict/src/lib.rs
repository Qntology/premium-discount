//! PD-Verdict: 평가(또는 화면 · 영역) 하나를 같은 스코프 안에서 premium(추천) / discount(비추천)으로 판정한다.
//!
//! 규칙의 기준은 premium-discount 의 계약이다. `contracts/gen_verdict_v1.py` · `gen_verdict_v2.py`(참조 구현)가
//! `contracts/verdict_v1.json` · `verdict_v2.json`(매개변수 + 사례 + 기대값)을 만들고, 구현들은 그 사례를 그대로 통과한다.
//! 서버 cron(TypeScript)은 투표로 전역 판정을, 이 crate 는 앱과 raon-zena 의 로컬 판정을 낸다.
//!
//! 1. 관측마다 `u = weight × credit × 2^(−age / H)` (`age = now − at_ms`, 음수는 0). 철회(`sign` 0)는 세지 않는다.
//! 2. `E = Σu`, `v = Σ(u × sign) / (E + k)`.
//! 3. 기준선: secondary → primary → global 중 `n ≥ min_obs` 인 첫 단계. 있으면 `r = tanh((v − mean) / (sd + eps))`.
//! 4. 등급: 대체됨 → discount, `E < min_evidence` → unrated, premium · discount 들어가기, 직전 등급 머무르기
//!    (히스테리시스), 나머지 neutral.
//! 5. grade: premium +2(`r ≥ strong`) 또는 +1, neutral 0, discount −2(대체됨 또는 `r ≤ −strong`) 또는 −1, unrated 없음.
//!
//! v2(`pd-verdict/2`)는 규칙이 같고 매개변수만 넓다: 관측 종류 [`IMPLICIT`](접근성 행동 로그, 가중치 0.3)와
//! 트랙 [`NAV`](화면 · 영역, web 과 같은 값)가 더해졌다. 판정 행은 recipe 를 남기고, recipe 가 다른 판정끼리는 비교하지 않는다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const RECIPE_V1: &str = "pd-verdict/1";
pub const RECIPE_V2: &str = "pd-verdict/2";

/// 접근성 내비게이션의 행동 로그가 에피소드마다 남기는 관측 종류 (v2)
pub const IMPLICIT: &str = "implicit";
/// 화면 · 영역 판정 트랙 (v2)
pub const NAV: &str = "nav";

const DAY_MS: f64 = 86_400_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Secondary,
    Primary,
    Global,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Secondary, Level::Primary, Level::Global];

    pub fn name(self) -> &'static str {
        match self {
            Level::Secondary => "secondary",
            Level::Primary => "primary",
            Level::Global => "global",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Premium,
    Neutral,
    Discount,
    Unrated,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Premium => "premium",
            Tier::Neutral => "neutral",
            Tier::Discount => "discount",
            Tier::Unrated => "unrated",
        }
    }
}

/// 관측 하나 (표, 결과 점수, 대화 속 피드백, 행동 로그의 implicit …)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Obs {
    pub kind: String,
    /// +1 premium 쪽, −1 discount 쪽, 0 철회
    pub sign: i32,
    pub weight: f64,
    /// 0..1 (결과 점수의 몫. 표는 1)
    pub credit: f64,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub n: u64,
    pub mean: f64,
    pub sd: f64,
}

/// 스코프 단계마다 판정된 값의 분포 (판정하는 항목 자신도 포함)
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Baselines {
    #[serde(default)]
    pub secondary: Option<Baseline>,
    #[serde(default)]
    pub primary: Option<Baseline>,
    #[serde(default)]
    pub global: Option<Baseline>,
}

impl Baselines {
    pub fn get(&self, level: Level) -> Option<Baseline> {
        match level {
            Level::Secondary => self.secondary,
            Level::Primary => self.primary,
            Level::Global => self.global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinObs {
    pub secondary: u64,
    pub primary: u64,
    pub global: u64,
}

impl MinObs {
    pub fn get(&self, level: Level) -> u64 {
        match level {
            Level::Secondary => self.secondary,
            Level::Primary => self.primary,
            Level::Global => self.global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackParams {
    pub half_life_ms: f64,
    pub min_obs: MinObs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub k: f64,
    pub eps: f64,
    pub enter: f64,
    pub exit: f64,
    pub floor: f64,
    pub strong: f64,
    pub min_evidence: f64,
    pub duel_rate: f64,
    pub duel_min: u32,
    pub tracks: BTreeMap<String, TrackParams>,
    /// 관측 종류별 기본 가중치 (관측을 만드는 쪽이 `Obs::weight` 에 넣는다. 판정 함수는 weight 만 본다)
    pub weights: BTreeMap<String, f64>,
}

impl Params {
    /// `pd-verdict/1` (계약의 `params` 와 같아야 한다. 시험이 비교한다)
    pub fn v1() -> Params {
        let track = |days: f64, secondary, primary, global| TrackParams {
            half_life_ms: days * DAY_MS,
            min_obs: MinObs {
                secondary,
                primary,
                global,
            },
        };
        Params {
            k: 2.0,
            eps: 0.05,
            enter: 0.5,
            exit: 0.3,
            floor: 0.1,
            strong: 0.8,
            min_evidence: 2.0,
            duel_rate: 0.65,
            duel_min: 3,
            tracks: BTreeMap::from([
                ("pd".to_string(), track(90.0, 5, 10, 30)),
                ("drop".to_string(), track(14.0, 6, 3, 10)),
                ("web".to_string(), track(14.0, 8, 4, 12)),
                ("chat".to_string(), track(14.0, 6, 3, 10)),
                ("coach".to_string(), track(14.0, 6, 3, 10)),
            ]),
            weights: BTreeMap::from(
                [
                    ("vote", 1.0),
                    ("vote_outcome", 0.5),
                    ("manual", 2.0),
                    ("feedback", 1.0),
                    ("outcome", 1.0),
                    ("reaction", 0.6),
                    ("engage", 0.2),
                    ("model", 0.3),
                ]
                .map(|(k, w)| (k.to_string(), w)),
            ),
        }
    }

    /// `pd-verdict/2`: v1 + 트랙 `nav`(web 과 같은 값) + 관측 종류 `implicit`(0.3)
    pub fn v2() -> Params {
        let mut p = Params::v1();
        let web = p.tracks["web"];
        p.tracks.insert(NAV.to_string(), web);
        p.weights.insert(IMPLICIT.to_string(), 0.3);
        p
    }

    /// recipe 이름의 매개변수. 모르는 이름은 None
    pub fn for_recipe(recipe: &str) -> Option<Params> {
        match recipe {
            RECIPE_V1 => Some(Params::v1()),
            RECIPE_V2 => Some(Params::v2()),
            _ => None,
        }
    }

    pub fn track(&self, name: &str) -> Option<&TrackParams> {
        self.tracks.get(name)
    }

    /// 관측 종류의 기본 가중치 (모르는 종류는 0)
    pub fn weight(&self, kind: &str) -> f64 {
        self.weights.get(kind).copied().unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prev {
    pub tier: Tier,
    pub since_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Duel {
    pub wins: u32,
    pub losses: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub evidence: f64,
    pub value: f64,
    pub level: Option<Level>,
    pub rank: Option<f64>,
    pub tier: Tier,
    pub grade: Option<i32>,
    pub since_ms: i64,
    pub reasons: Vec<String>,
}

/// `now_ms` 시점 관측들의 근거량 `E` 와 값 `v`
pub fn evidence(obs: &[Obs], now_ms: i64, track: &TrackParams, params: &Params) -> (f64, f64) {
    let (mut e, mut s) = (0.0, 0.0);
    for o in obs {
        if o.sign == 0 {
            continue;
        }
        let age = (now_ms - o.at_ms).max(0) as f64;
        let u = o.weight * o.credit * 2f64.powf(-age / track.half_life_ms);
        e += u;
        s += u * o.sign as f64;
    }
    (e, s / (e + params.k))
}

/// [`judge`] 가 `E` · `v` 말고 더 보는 것
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Context<'a> {
    pub track: &'a TrackParams,
    pub baselines: &'a Baselines,
    pub prev: Prev,
    pub now_ms: i64,
    pub duel: Option<Duel>,
    pub superseded: bool,
}

/// `E` 와 `v` 를 아는 항목의 등급 (피어를 다시 판정할 때 관측을 다시 읽지 않는다)
pub fn judge(evidence: f64, value: f64, cx: &Context<'_>, params: &Params) -> Verdict {
    let (e, v) = (evidence, value);
    let base = Level::ALL
        .into_iter()
        .find_map(|lv| cx.baselines.get(lv).filter(|b| b.n >= cx.track.min_obs.get(lv)).map(|b| (lv, b)));
    let level = base.map(|(lv, _)| lv);
    let r = base.map(|(_, b)| ((v - b.mean) / (b.sd + params.eps)).tanh());
    let (win, loss) = match cx.duel {
        Some(d) if d.wins + d.losses >= params.duel_min => {
            let meetings = (d.wins + d.losses) as f64;
            (Some(d.wins as f64 / meetings), Some(d.losses as f64 / meetings))
        }
        _ => (None, None),
    };
    let prev = cx.prev.tier;

    let mut reasons = Vec::new();
    let tier = if cx.superseded {
        reasons.push("superseded".to_string());
        Tier::Discount
    } else if e < params.min_evidence {
        reasons.push("low_evidence".to_string());
        Tier::Unrated
    } else {
        let rank_high = r.is_some_and(|r| r >= params.enter);
        let rank_low = r.is_some_and(|r| r <= -params.enter);
        let duel_win = win.is_some_and(|w| w >= params.duel_rate);
        let duel_loss = loss.is_some_and(|l| l >= params.duel_rate);
        if v >= params.floor && (rank_high || duel_win) {
            reasons.push(if rank_high { "rank_high" } else { "duel_win" }.to_string());
            Tier::Premium
        } else if v <= -params.floor && (rank_low || duel_loss) {
            reasons.push(if rank_low { "rank_low" } else { "duel_loss" }.to_string());
            Tier::Discount
        } else if prev == Tier::Premium && v > 0.0 && (r.is_some_and(|r| r >= params.exit) || (r.is_none() && duel_win)) {
            reasons.push("held".to_string());
            Tier::Premium
        } else if prev == Tier::Discount && v < 0.0 && (r.is_some_and(|r| r <= -params.exit) || (r.is_none() && duel_loss)) {
            reasons.push("held".to_string());
            Tier::Discount
        } else {
            if rank_high || rank_low || duel_win || duel_loss {
                reasons.push("floor".to_string());
            }
            Tier::Neutral
        }
    };
    reasons.push(match level {
        Some(lv) => format!("level:{}", lv.name()),
        None => "no_baseline".to_string(),
    });

    let grade = match tier {
        Tier::Premium => Some(if r.is_some_and(|r| r >= params.strong) { 2 } else { 1 }),
        Tier::Discount => Some(if cx.superseded || r.is_some_and(|r| r <= -params.strong) {
            -2
        } else {
            -1
        }),
        Tier::Neutral => Some(0),
        Tier::Unrated => None,
    };
    let since_ms = if tier == prev { cx.prev.since_ms } else { cx.now_ms };
    Verdict {
        evidence: e,
        value: v,
        level,
        rank: r,
        tier,
        grade,
        since_ms,
        reasons,
    }
}

/// 계약 사례 하나의 입력
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub now_ms: i64,
    pub track: String,
    pub prev: Prev,
    pub obs: Vec<Obs>,
    #[serde(default)]
    pub baseline: Baselines,
    #[serde(default)]
    pub duel: Option<Duel>,
    #[serde(default)]
    pub superseded: bool,
}

/// 관측에서 판정까지. 모르는 트랙이면 None
pub fn verdict(case: &Case, params: &Params) -> Option<Verdict> {
    let track = params.track(&case.track)?;
    let (e, v) = evidence(&case.obs, case.now_ms, track, params);
    let cx = Context {
        track,
        baselines: &case.baseline,
        prev: case.prev,
        now_ms: case.now_ms,
        duel: case.duel,
        superseded: case.superseded,
    };
    Some(judge(e, v, &cx, params))
}

/// 한 스코프의 판정된 값들로 만든 기준선 (모집단 표준편차)
pub fn baseline_of(values: &[f64]) -> Baseline {
    let n = values.len();
    if n == 0 {
        return Baseline { n: 0, mean: 0.0, sd: 0.0 };
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let variance = values.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    Baseline {
        n: n as u64,
        mean,
        sd: variance.sqrt(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // 계약은 이 저장소의 contracts/ 에 있다 (crates/pd-verdict/src 에서 세 단계 위)
    const CONTRACT_V1: &str = include_str!("../../../contracts/verdict_v1.json");
    const CONTRACT_V2: &str = include_str!("../../../contracts/verdict_v2.json");

    #[derive(Deserialize)]
    struct Contract {
        recipe: String,
        tolerance: f64,
        params: Params,
        cases: Vec<Named>,
    }

    #[derive(Deserialize)]
    struct Named {
        name: String,
        #[serde(flatten)]
        case: Case,
        expect: Verdict,
    }

    fn check(text: &str, recipe: &str, params: Params, min_cases: usize) -> Contract {
        let c: Contract = serde_json::from_str(text).unwrap();
        assert_eq!(c.recipe, recipe);
        assert_eq!(c.params, params, "{recipe} 매개변수");
        assert_eq!(Params::for_recipe(recipe).as_ref(), Some(&params));
        assert!(c.cases.len() >= min_cases, "{recipe} 사례 수 {}", c.cases.len());
        let close = |a: Option<f64>, b: Option<f64>, what: &str| match (a, b) {
            (Some(a), Some(b)) => assert!((a - b).abs() <= c.tolerance, "{what}: {a} vs {b}"),
            _ => assert_eq!(a, b, "{what}"),
        };
        for n in &c.cases {
            let got = verdict(&n.case, &params).unwrap();
            let want = &n.expect;
            close(Some(got.evidence), Some(want.evidence), &format!("{recipe} {} evidence", n.name));
            close(Some(got.value), Some(want.value), &format!("{recipe} {} value", n.name));
            close(got.rank, want.rank, &format!("{recipe} {} rank", n.name));
            assert_eq!(got.level, want.level, "{recipe} {} level", n.name);
            assert_eq!(got.tier, want.tier, "{recipe} {} tier", n.name);
            assert_eq!(got.grade, want.grade, "{recipe} {} grade", n.name);
            assert_eq!(got.since_ms, want.since_ms, "{recipe} {} since", n.name);
            assert_eq!(got.reasons, want.reasons, "{recipe} {} reasons", n.name);
        }
        c
    }

    #[test]
    fn v1_params_and_every_case_match_the_contract() {
        check(CONTRACT_V1, RECIPE_V1, Params::v1(), 19);
    }

    #[test]
    fn v2_params_and_every_case_match_the_contract() {
        let v2 = check(CONTRACT_V2, RECIPE_V2, Params::v2(), 25);
        // v2 는 v1 사례를 모두 담고, 그 결과는 v1 과 같다
        let v1 = check(CONTRACT_V1, RECIPE_V1, Params::v1(), 19);
        for old in &v1.cases {
            let same = v2.cases.iter().find(|n| n.name == old.name).unwrap_or_else(|| panic!("v2 에 {} 가 없다", old.name));
            assert_eq!(same.case, old.case, "{} 입력", old.name);
            assert_eq!(same.expect, old.expect, "{} 기대값", old.name);
        }
        assert!(v2.cases.iter().any(|n| n.case.track == NAV && n.case.obs.iter().any(|o| o.kind == IMPLICIT)));
    }

    #[test]
    fn v2_only_adds_the_nav_track_and_the_implicit_weight() {
        let (v1, v2) = (Params::v1(), Params::v2());
        assert_eq!(v2.track(NAV), v1.track("web"));
        assert_eq!(v2.weight(IMPLICIT), 0.3);
        assert_eq!(v1.weight(IMPLICIT), 0.0);
        let mut back = v2.clone();
        back.tracks.remove(NAV);
        back.weights.remove(IMPLICIT);
        assert_eq!(back, v1);
        assert_eq!(Params::for_recipe("pd-verdict/9"), None);
    }

    #[test]
    fn verdicts_serialize_with_the_contract_field_names() {
        let c: Value = serde_json::from_str(CONTRACT_V2).unwrap();
        let first: Case = serde_json::from_value(c["cases"][0].clone()).unwrap();
        let v = serde_json::to_value(verdict(&first, &Params::v2()).unwrap()).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let want: Vec<&str> = c["cases"][0]["expect"].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), want.len());
        for k in want {
            assert!(keys.contains(&k), "{k}");
        }
        assert_eq!(v["tier"], "premium");
        assert_eq!(v["level"], "secondary");
    }

    #[test]
    fn unknown_tracks_and_weights() {
        let params = Params::v2();
        let case = Case {
            now_ms: 0,
            track: "nope".into(),
            prev: Prev {
                tier: Tier::Unrated,
                since_ms: 0,
            },
            obs: vec![],
            baseline: Baselines::default(),
            duel: None,
            superseded: false,
        };
        assert!(verdict(&case, &params).is_none());
        assert_eq!(params.weight("manual"), 2.0);
        assert_eq!(params.weight("unknown"), 0.0);
        let b = baseline_of(&[1.0, -1.0, 1.0, -1.0]);
        assert_eq!((b.n, b.mean, b.sd), (4, 0.0, 1.0));
        assert_eq!(baseline_of(&[]).n, 0);
    }
}
