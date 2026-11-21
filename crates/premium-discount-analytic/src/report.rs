//! 운영자 리포트의 결정론적 조각 (시나리오 5): 기간 비교의 표본 신뢰도와 단정하지 않는 비교 문장
//!
//! 기간 비교 창은 [`crate::time::resolve_comparison_window`] 가 낸다. 진행 중인 기간끼리는 같은 경과 시간으로 잘라서, 화요일
//! 13시의 이번 주 37시간을 지난주 168시간과 견주어 "−78 % 폭락" 이라고 말하지 않는다. 그래도 어느 한쪽 창의 세션이
//! [`MIN_FUNNEL_SESSIONS`] 보다 적으면 전환율 변화를 계산하지 않고 [`ReportConfidence::InsufficientSample`] 을 낸다. 그때의
//! 문장([`ConversionComparison::line`])에는 오르내림을 단정하는 말이 없다.
//!
//! 앱(`apps/core/src/analytic.rs` 의 리포트)과 raon-zena 의 운영자 브리프가 같은 값을 쓰도록 여기 둔다. 모델 · 저장소에 기대지 않는다.

use serde::{Deserialize, Serialize};

use crate::time::ComparisonWindow;

/// 전환율 변화를 말하려면 두 창 모두 이만큼의 세션(들어온 단계에 닿은 세션)이 있어야 한다
pub const MIN_FUNNEL_SESSIONS: u32 = 15;
/// 병목 · 추천 순위표에 올리는 요소의 최소 노출 수 ([`crate::funnel::DebiasConfig::min_exposures`] 의 기본값과 같다)
pub const MIN_RANKED_EXPOSURES: u32 = 5;

/// 리포트 수치의 표본 신뢰도
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReportConfidence {
    Sufficient { sessions: u32, prev_sessions: Option<u32> },
    /// 표본이 모자라다: 변화를 계산하지 않는다 (`sessions` 는 두 창 중 적은 쪽)
    InsufficientSample { sessions: u32, min_required: u32 },
}

impl ReportConfidence {
    /// 현재 창 · 비교 창(없으면 현재 창만)의 세션 수로 정한다
    pub fn of(sessions: u32, prev_sessions: Option<u32>) -> ReportConfidence {
        let smallest = prev_sessions.map_or(sessions, |p| p.min(sessions));
        if smallest < MIN_FUNNEL_SESSIONS {
            ReportConfidence::InsufficientSample { sessions: smallest, min_required: MIN_FUNNEL_SESSIONS }
        } else {
            ReportConfidence::Sufficient { sessions, prev_sessions }
        }
    }

    pub fn is_sufficient(&self) -> bool {
        matches!(self, ReportConfidence::Sufficient { .. })
    }
}

/// 두 창의 전환율 비교
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversionComparison {
    pub window: ComparisonWindow,
    /// 현재 창: 들어온 세션 · 끝낸 세션
    pub sessions: u32,
    pub converted: u32,
    /// 비교 창: 들어온 세션 · 끝낸 세션
    pub prev_sessions: u32,
    pub prev_converted: u32,
    pub confidence: ReportConfidence,
    /// 전환율 변화 (%p, 현재 − 비교). 표본이 모자라면 None (계산하지 않는다)
    pub delta_pct_pt: Option<f64>,
}

fn rate(sessions: u32, converted: u32) -> Option<f64> {
    (sessions > 0).then(|| converted.min(sessions) as f64 / sessions as f64)
}

impl ConversionComparison {
    /// `current` · `previous` 는 (들어온 세션, 끝낸 세션)이다. 예: 창마다 [`crate::funnel::SiteAnalysis::branch_summary`] 의
    /// (`entered`, `completed`)
    pub fn new(window: ComparisonWindow, current: (u32, u32), previous: (u32, u32)) -> ConversionComparison {
        let confidence = ReportConfidence::of(current.0, Some(previous.0));
        let delta_pct_pt = match (confidence.is_sufficient(), rate(current.0, current.1), rate(previous.0, previous.1)) {
            (true, Some(now), Some(before)) => Some((now - before) * 100.0),
            _ => None,
        };
        ConversionComparison { window, sessions: current.0, converted: current.1, prev_sessions: previous.0, prev_converted: previous.1, confidence, delta_pct_pt }
    }

    /// 현재 창의 전환율 (0 ~ 1)
    pub fn rate(&self) -> Option<f64> {
        rate(self.sessions, self.converted)
    }

    /// 비교 창의 전환율 (0 ~ 1)
    pub fn prev_rate(&self) -> Option<f64> {
        rate(self.prev_sessions, self.prev_converted)
    }

    /// 사람이 읽는 한 줄 (리포트의 줄 · raon-zena 운영자 브리프의 전환율 변화). 표본이 모자라면 수치의 오르내림을 말하지 않는다
    pub fn line(&self) -> String {
        match (&self.confidence, self.prev_rate(), self.rate(), self.delta_pct_pt) {
            (ReportConfidence::Sufficient { .. }, Some(before), Some(now), Some(delta)) => {
                format!("전환율 {:.1}% → {:.1}% ({delta:+.1}%p), {}", before * 100.0, now * 100.0, self.window.label)
            }
            (ReportConfidence::InsufficientSample { sessions, min_required }, ..) => {
                format!("표본 부족: 세션 {sessions}건 (최소 {min_required}건)이라 전환율 변화는 판단하지 않습니다 ({})", self.window.label)
            }
            _ => format!("전환율을 견줄 세션이 없습니다 ({})", self.window.label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> ComparisonWindow {
        ComparisonWindow {
            current_start_ms: 0,
            current_end_ms: 37 * 3_600_000,
            previous_start_ms: -7 * 86_400_000,
            previous_end_ms: -7 * 86_400_000 + 37 * 3_600_000,
            pace_matched: true,
            label: "this_week (+37.0h) vs last_week (+37.0h)".into(),
            current_key: "this_week".into(),
            previous_key: "last_week".into(),
        }
    }

    #[test]
    fn enough_sessions_give_a_signed_delta() {
        let c = ConversionComparison::new(window(), (50, 7), (40, 8));
        assert_eq!(c.confidence, ReportConfidence::Sufficient { sessions: 50, prev_sessions: Some(40) });
        assert!((c.delta_pct_pt.unwrap() - (-6.0)).abs() < 1e-9);
        assert_eq!(c.line(), "전환율 20.0% → 14.0% (-6.0%p), this_week (+37.0h) vs last_week (+37.0h)");
    }

    #[test]
    fn the_smaller_window_decides_the_confidence() {
        assert_eq!(ReportConfidence::of(40, Some(9)), ReportConfidence::InsufficientSample { sessions: 9, min_required: MIN_FUNNEL_SESSIONS });
        assert_eq!(ReportConfidence::of(15, None), ReportConfidence::Sufficient { sessions: 15, prev_sessions: None });
        let json = serde_json::to_value(ReportConfidence::of(4, Some(30))).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "insufficient_sample", "sessions": 4, "min_required": 15}));
        let empty = ConversionComparison::new(window(), (0, 0), (0, 0));
        assert_eq!(empty.delta_pct_pt, None);
        assert!(empty.line().starts_with("표본 부족"));
    }
}
