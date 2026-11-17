//! 웹 행동 분석 Track 1 (모델 없음): 수집 사건 → 웹 체크포인트 · 선택 대조 · 마찰 에피소드 · 퍼널
//!
//! 1. **체크포인트**: 페이지는 [`premium_discount_nav::page_hash`](링크값), 요소는 페이지 + DOM 셀렉터 틀
//!    ([`premium_discount_nav::element_hash`]). 접근성 내비게이션의 웹 코스와 같은 해시다.
//! 2. **선택 대조 (고른 요소 vs 지나친 형제)**: 누르거나 바꾼 요소(TARGET)와 같은 목록에서 고르지 않은 형제(RELATED)를 한
//!    선택 묶음으로 본다. 묶음의 크기가 n 이면 아무 정보 없이 고를 때 각 후보가 뽑힐 확률은 1/n 이다. 후보마다 노출 수 ·
//!    뽑힌 수 · 기댓값(Σ1/n) · 분산(Σ(1/n)(1−1/n))을 쌓고, `z = (뽑힌 수 − 기댓값) / √분산` 이 띠(`band`)를 넘을 때만 premium ·
//!    discount 로 결정한다([`ChoiceTally::decide`]). 띠 안이면 결정하지 않는다(SDS 와 같은 태도).
//!
//!    기획서는 "고른 TARGET 은 premium, 비켜 간 RELATED 는 discount 로 바로 센다" 였다. 그 수를 등급 규칙(2/3)에 넣으면
//!    목록이 길수록 모든 후보가 discount 가 된다: 10개 목록에서 무작위로 고르기만 해도 후보마다 discount 가 9 배 많다
//!    ([`ChoiceTally::naive_tier`], 시험 `the_plan_rule_marks_a_random_list_discount`). 그래서 수는 함께 남기되 결정은
//!    기댓값과 견준다.
//!
//!    같은 틀의 목록(상품 카드)은 셀렉터 틀이 같아 요소 해시도 같다(셀렉터는 데이터 자리를 지운다). 그래서 선택 대조의 키는
//!    요소 해시 + 글(PUG 의 글만, [`crate::pug::text_of`])이다([`choice_key`]).
//! 3. **마찰 에피소드**: 사건을 접근성 내비게이션의 신호로 바꾸고(누름 `Picked`, 광클 `RageClick`, 머묾 `Hovered`, 첫 입력
//!    `Engaged` · 다시 고침 `Retried`, 스크롤 뒤집기 `Rescrolled`, 뒤로 `Backtracked`, 제출 `Submitted`, 바꾼 칸을 두고 떠남
//!    `Abandoned`, 들어와서 첫 동작까지 `Searched`), 세션을 에피소드로 잘라([`premium_discount_nav::split_episodes`]) 같은
//!    사이트의 다른 페이지 · 요소와 견준다([`premium_discount_nav::FrictionBaselines::decide`]).
//! 4. **퍼널**: 고른 페이지 순서(상품 → 장바구니 → 결제 → 완료)마다 도달한 세션 수와 다음 단계로 못 간 수.

use std::collections::{BTreeMap, HashMap};

use premium_discount_nav::{self as nav, BehaviorParams, Checkpoint, Episode, Event, FrictionBaselines, MarkType, Signal, StepKind};
use premium_discount_verdict::Tier;
use serde::{Deserialize, Serialize};

use crate::event::{Element, EventKind, WebEvent};
use crate::pug;

/// 들어와서 첫 동작까지 이보다 오래 걸렸으면 찾는 데 걸린 시간(`Searched`)으로 센다
pub const SEARCH_MIN_MS: i64 = 1_000;
/// 머묾으로 세는 최소 시간 (tracker-logis-center 와 같은 500 ms)
pub const HOVER_MIN_MS: u32 = 500;
/// 라벨 글자 수
const LABEL_CHARS: usize = 40;

/// 요소의 체크포인트 해시 (페이지 + 셀렉터 틀). 조상 모양이 없거나 이상하면 None
pub fn element_of(page: &str, el: &Element) -> Option<String> {
    let selector = nav::selector_template(&el.chain)?;
    nav::element_hash(page, &selector)
}

fn label_of(el: &Element) -> String {
    pug::text_of(&el.pug).chars().take(LABEL_CHARS).collect()
}

/// 선택 대조의 키: 요소 해시 + 글. 같은 틀의 형제(목록의 카드)는 글로 갈린다. 조상 모양이 없으면 페이지 + 글
pub fn choice_key(page: &str, el: &Element) -> Option<String> {
    let text = pug::text_of(&el.pug).to_lowercase();
    let element = element_of(page, el).unwrap_or_else(|| page.to_string());
    if text.is_empty() && el.chain.is_empty() {
        return None;
    }
    nav::hash_id(&format!("choice/1|{element}|{text}"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceParams {
    /// z 띠: 이만큼 기댓값을 벗어나야 결정한다 (2.0 ≈ 한쪽 2.3 %)
    pub band: f64,
    /// 노출이 이만큼은 있어야 결정한다
    pub min_exposures: u32,
}

impl Default for ChoiceParams {
    fn default() -> Self {
        ChoiceParams { band: 2.0, min_exposures: 5 }
    }
}

/// 후보 하나의 선택 기록
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ChoiceTally {
    /// 골라진 수 (TARGET)
    pub picks: u32,
    /// 묶음에 보인 수 (TARGET + RELATED)
    pub exposures: u32,
    /// 아무 정보 없이 골랐을 때의 기댓값 Σ 1/n
    pub expected: f64,
    /// 그 분산 Σ (1/n)(1 − 1/n)
    pub variance: f64,
}

impl ChoiceTally {
    /// 지나친 수 (RELATED 로만 보인 수)
    pub fn passed_over(&self) -> u32 {
        self.exposures - self.picks
    }

    pub fn z(&self) -> Option<f64> {
        (self.variance > 0.0).then(|| (self.picks as f64 - self.expected) / self.variance.sqrt())
    }

    pub fn decide(&self, p: &ChoiceParams) -> Option<MarkType> {
        if self.exposures < p.min_exposures {
            return None;
        }
        match self.z()? {
            z if z >= p.band => Some(MarkType::Premium),
            z if z <= -p.band => Some(MarkType::Discount),
            _ => None,
        }
    }

    /// 기획서 규칙: 고른 것은 premium, 지나친 것은 discount 로 바로 세어 등급(2/3)을 낸 것. 비교용으로만 남긴다
    pub fn naive_tier(&self) -> Tier {
        nav::tier(self.picks, self.passed_over())
    }
}

/// 선택 대조 장부: 키 → 기록, 키 → 라벨 · 페이지
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChoiceBook {
    pub items: BTreeMap<String, ChoiceTally>,
    pub labels: BTreeMap<String, String>,
    pub pages: BTreeMap<String, String>,
}

impl ChoiceBook {
    /// 선택 하나: `shown` 중에서 `chosen` 을 골랐다(`shown` 에 `chosen` 이 없으면 더한다, 같은 키는 하나로). 후보가 둘보다
    /// 적으면 대조가 아니라서 세지 않는다(false)
    pub fn observe(&mut self, chosen: &str, shown: &[&str]) -> bool {
        let mut set: Vec<&str> = Vec::with_capacity(shown.len() + 1);
        for k in std::iter::once(chosen).chain(shown.iter().copied()) {
            if !set.contains(&k) {
                set.push(k);
            }
        }
        if set.len() < 2 {
            return false;
        }
        let q = 1.0 / set.len() as f64;
        for k in &set {
            let t = self.items.entry(k.to_string()).or_default();
            t.exposures += 1;
            t.expected += q;
            t.variance += q * (1.0 - q);
            if *k == chosen {
                t.picks += 1;
            }
        }
        true
    }

    /// 누름 · 바꿈 사건의 TARGET vs RELATED. 다른 사건이거나 형제가 없으면 false
    pub fn observe_event(&mut self, e: &WebEvent) -> bool {
        if !matches!(e.kind, EventKind::Click | EventKind::Change) || e.rage || e.related.is_empty() {
            return false;
        }
        // 고른 것은 형제와 같은 단위(목록 항목)로 센다. 목록 항목이 따로 없으면 누른 요소
        let (Some(page), Some(target)) = (nav::page_hash(&e.href), e.item.as_ref().or(e.target.as_ref())) else {
            return false;
        };
        let Some(chosen) = choice_key(&page, target) else {
            return false;
        };
        let mut shown: Vec<String> = Vec::new();
        for r in &e.related {
            if let Some(k) = choice_key(&page, r) {
                self.labels.entry(k.clone()).or_insert_with(|| label_of(r));
                self.pages.entry(k.clone()).or_insert_with(|| page.clone());
                shown.push(k);
            }
        }
        self.labels.entry(chosen.clone()).or_insert_with(|| label_of(target));
        self.pages.entry(chosen.clone()).or_insert_with(|| page.clone());
        let refs: Vec<&str> = shown.iter().map(String::as_str).collect();
        self.observe(&chosen, &refs)
    }
}

/// 세션 하나를 신호 · 체크포인트로 바꾼 것
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionTrace {
    pub sid: String,
    /// 페이지에 들어옴 · 요소를 다룸 (타임트라이얼 코스의 체크포인트, `at_ms` 는 세션 경과 시간)
    pub checkpoints: Vec<Checkpoint>,
    /// 마찰 신호 (`anchor` 는 요소 해시, 없으면 페이지 해시)
    pub events: Vec<Event>,
    /// 신호의 anchor → 페이지 해시
    pub page_of: HashMap<String, String>,
    /// 들어온 페이지 순서 (같은 페이지가 이어지면 하나)
    pub path: Vec<String>,
}

/// 한 세션의 사건(같은 `sid`)을 순서대로 신호 · 체크포인트로 바꾼다
pub fn trace_session(events: &[WebEvent]) -> SessionTrace {
    let mut sorted: Vec<&WebEvent> = events.iter().collect();
    sorted.sort_by_key(|e| (e.seq, e.t_ms));
    let mut out = SessionTrace { sid: sorted.first().map(|e| e.sid.clone()).unwrap_or_default(), ..Default::default() };
    let mut entered_at: Option<i64> = None;
    let mut acted = false;
    let mut changed: Vec<String> = Vec::new();
    let mut dirty: Vec<String> = Vec::new();
    for e in sorted {
        let Some(page) = nav::page_hash(&e.href) else { continue };
        let element = e.target.as_ref().and_then(|t| element_of(&page, t));
        let anchor = element.clone().unwrap_or_else(|| page.clone());
        out.page_of.insert(anchor.clone(), page.clone());
        out.page_of.insert(page.clone(), page.clone());
        let mut push = |signal: Signal| out.events.push(Event { at_ms: e.t_ms, signal, anchor: Some(anchor.clone()) });
        if e.kind == EventKind::Enter || out.path.last() != Some(&page) {
            if out.path.last() != Some(&page) {
                out.path.push(page.clone());
                out.checkpoints.push(Checkpoint { at_ms: e.t_ms, kind: StepKind::Page, hash: page.clone(), gesture: None });
                changed.clear();
            }
            entered_at = Some(e.t_ms);
            acted = false;
        }
        let first_action = matches!(e.kind, EventKind::Click | EventKind::Change | EventKind::Submit) && !acted;
        if first_action {
            acted = true;
            if let Some(t0) = entered_at {
                let ms = e.t_ms - t0;
                if ms >= SEARCH_MIN_MS {
                    push(Signal::Searched { ms: ms.min(u32::MAX as i64) as u32 });
                }
            }
        }
        match e.kind {
            EventKind::Enter => {}
            EventKind::Click if e.rage => push(Signal::RageClick),
            EventKind::Click => {
                push(Signal::Picked);
                if let Some(h) = &element {
                    out.checkpoints.push(Checkpoint { at_ms: e.t_ms, kind: StepKind::Element, hash: h.clone(), gesture: None });
                }
            }
            EventKind::Hover => {
                if e.dwell_ms.unwrap_or(0) >= HOVER_MIN_MS {
                    push(Signal::Hovered);
                }
            }
            EventKind::Change => {
                if changed.contains(&anchor) {
                    push(Signal::Retried);
                } else {
                    changed.push(anchor.clone());
                    push(Signal::Engaged);
                }
                if !dirty.contains(&anchor) {
                    dirty.push(anchor.clone());
                }
            }
            EventKind::Scroll => {
                let times = e.flips.unwrap_or(0);
                if times > 0 {
                    push(Signal::Rescrolled { times });
                }
            }
            EventKind::Submit => {
                push(Signal::Submitted);
                dirty.clear();
            }
            EventKind::Back => push(Signal::Backtracked),
            EventKind::Leave => {
                if e.dirty.unwrap_or(0) > 0 || !dirty.is_empty() {
                    push(Signal::Abandoned);
                }
                dirty.clear();
            }
        }
    }
    out
}

/// 페이지 · 요소 하나의 마찰 결정 수
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrictionTally {
    pub label: String,
    pub episodes: u32,
    pub premium: u32,
    pub discount: u32,
    pub friction_sum: f64,
}

impl FrictionTally {
    pub fn tier(&self) -> Tier {
        nav::tier(self.premium, self.discount)
    }

    pub fn mean_friction(&self) -> f64 {
        if self.episodes == 0 {
            0.0
        } else {
            self.friction_sum / self.episodes as f64
        }
    }

    fn add(&mut self, friction: f64, mark: Option<MarkType>) {
        self.episodes += 1;
        self.friction_sum += friction;
        match mark {
            Some(MarkType::Premium) => self.premium += 1,
            Some(MarkType::Discount) => self.discount += 1,
            None => {}
        }
    }
}

/// 퍼널 한 단계
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunnelStep {
    pub page: String,
    pub label: String,
    /// 앞 단계를 차례로 지나 이 단계에 닿은 세션 수
    pub reached: u32,
    /// 이 단계에 닿고 다음 단계로 가지 못한 세션 수 (마지막 단계는 0)
    pub dropped: u32,
}

/// 사이트 하나의 분석 (기기 안). 세션을 넣을 때마다 쌓인다
#[derive(Debug, Clone, Default)]
pub struct SiteAnalysis {
    pub host: String,
    pub params: BehaviorParams,
    pub choice_params: ChoiceParams,
    pub baselines: FrictionBaselines,
    pub choices: ChoiceBook,
    pub pages: BTreeMap<String, FrictionTally>,
    pub elements: BTreeMap<String, FrictionTally>,
    pub paths: Vec<Vec<String>>,
    pub sessions: u32,
}

impl SiteAnalysis {
    pub fn new(host: &str) -> SiteAnalysis {
        SiteAnalysis { host: host.trim().to_ascii_lowercase(), ..Default::default() }
    }

    /// 세션 하나(같은 `sid` 의 사건)를 넣는다: 선택 대조, 에피소드 결정, 퍼널 경로
    pub fn add_session(&mut self, events: &[WebEvent]) -> Vec<(Episode, nav::Decision)> {
        for e in events {
            self.choices.observe_event(e);
        }
        let labels = labels_of(events);
        let trace = trace_session(events);
        self.sessions += 1;
        if !trace.path.is_empty() {
            self.paths.push(trace.path.clone());
        }
        let page_of = |e: &Event| e.anchor.as_ref().and_then(|a| trace.page_of.get(a)).cloned();
        // 의미 거리 대신 페이지가 바뀌었는가 (같은 페이지 안의 간격은 덜 자른다)
        let episodes = nav::split_episodes(&trace.events, |a, b| if page_of(a) == page_of(b) { 0.25 } else { 1.0 }, &self.params);
        let mut out = Vec::new();
        for ep in episodes {
            let Some(page) = ep.events.last().and_then(|e| page_of(e)) else { continue };
            let region = ep.focus().filter(|a| *a != page).map(str::to_string);
            let d = self.baselines.decide(&self.host, &page, region.as_deref(), &ep, &self.params);
            let pt = self.pages.entry(page.clone()).or_default();
            if pt.label.is_empty() {
                pt.label = labels.get(&page).cloned().unwrap_or_default();
            }
            pt.add(d.friction, d.screen.mark);
            if let (Some(r), Some(j)) = (&region, &d.region) {
                let et = self.elements.entry(r.clone()).or_default();
                if et.label.is_empty() {
                    et.label = labels.get(r).cloned().unwrap_or_default();
                }
                et.add(d.friction, j.mark);
            }
            out.push((ep, d));
        }
        out
    }

    /// 페이지 순서 `steps` 의 퍼널. 세션 경로에서 단계를 차례로(사이에 다른 페이지가 있어도) 지났는지 본다
    pub fn funnel(&self, steps: &[String]) -> Vec<FunnelStep> {
        let mut reached = vec![0u32; steps.len()];
        for path in &self.paths {
            let mut at = 0usize;
            for page in path {
                if at < steps.len() && *page == steps[at] {
                    reached[at] += 1;
                    at += 1;
                }
            }
        }
        steps
            .iter()
            .enumerate()
            .map(|(i, page)| FunnelStep {
                page: page.clone(),
                label: self.pages.get(page).map(|p| p.label.clone()).unwrap_or_default(),
                reached: reached[i],
                dropped: if i + 1 < steps.len() { reached[i] - reached[i + 1] } else { 0 },
            })
            .collect()
    }

    /// 마찰이 큰(discount 쪽) 요소부터: 퍼널 병목 후보
    pub fn bottlenecks(&self) -> Vec<(&str, &FrictionTally)> {
        let mut v: Vec<(&str, &FrictionTally)> = self.elements.iter().map(|(k, t)| (k.as_str(), t)).filter(|(_, t)| t.discount > 0).collect();
        v.sort_by(|a, b| b.1.discount.cmp(&a.1.discount).then(b.1.mean_friction().total_cmp(&a.1.mean_friction())));
        v
    }
}

/// 해시 → 사람이 읽는 라벨 (페이지는 URL 틀, 요소는 PUG 의 글)
fn labels_of(events: &[WebEvent]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for e in events {
        let Some(page) = nav::page_hash(&e.href) else { continue };
        out.entry(page.clone()).or_insert_with(|| nav::url_template(&e.href).unwrap_or_default());
        if let Some(t) = &e.target {
            if let Some(h) = element_of(&page, t) {
                out.entry(h).or_insert_with(|| label_of(t));
            }
        }
    }
    out
}
