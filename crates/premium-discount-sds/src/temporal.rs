//! Candle-free temporal forecasting and consistency gate ported from `noto-ts`
//! (`holt_damped`, `forecast`, `Predictor`, `temporal_gate_with_forecast`).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

pub const HOLDOUT: usize = 4;
pub const MIN_POINTS: usize = 6;
pub const HISTORY_CAP: f64 = 8.0;
pub const HISTORY_ONLY_MIN_N: u64 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoltFit {
    pub alpha: f64,
    pub beta: f64,
    pub phi: f64,
    pub sse: f64,
    pub sigma: f64,
}

/// Fits a multiplicative/additive damped-trend Holt linear model over an offline grid
/// and forecasts `horizon` steps with prediction residual standard deviations.
pub fn holt_damped(y: &[f64], horizon: usize) -> (Vec<f64>, Vec<f64>, HoltFit) {
    let n = y.len();
    if n == 0 {
        return (
            vec![0.0; horizon],
            vec![0.0; horizon],
            HoltFit { alpha: 0.0, beta: 0.0, phi: 1.0, sse: 0.0, sigma: 0.0 },
        );
    }
    if n < 3 {
        let last = *y.last().unwrap();
        return (
            vec![last; horizon],
            vec![0.0; horizon],
            HoltFit { alpha: 1.0, beta: 0.0, phi: 1.0, sse: 0.0, sigma: 0.0 },
        );
    }
    let mut best = HoltFit { alpha: 0.5, beta: 0.1, phi: 0.98, sse: f64::INFINITY, sigma: 0.0 };
    for ai in 1..=9 {
        let alpha = ai as f64 / 10.0;
        for &beta in [0.05, 0.1, 0.2, 0.3].iter() {
            for &phi in [0.8, 0.9, 0.95, 0.98, 1.0].iter() {
                let mut l = y[0];
                let mut b = y[1] - y[0];
                let mut sse = 0.0;
                for &yt in &y[1..] {
                    let pred = l + phi * b;
                    let e = yt - pred;
                    sse += e * e;
                    let l_new = alpha * yt + (1.0 - alpha) * (l + phi * b);
                    b = beta * (l_new - l) + (1.0 - beta) * phi * b;
                    l = l_new;
                }
                if sse < best.sse {
                    best = HoltFit { alpha, beta, phi, sse, sigma: 0.0 };
                }
            }
        }
    }
    let mut l = y[0];
    let mut b = y[1] - y[0];
    for &yt in &y[1..] {
        let l_new = best.alpha * yt + (1.0 - best.alpha) * (l + best.phi * b);
        b = best.beta * (l_new - l) + (1.0 - best.beta) * best.phi * b;
        l = l_new;
    }
    best.sigma = (best.sse / (n - 1).max(1) as f64).sqrt();
    let mut point = Vec::with_capacity(horizon);
    let mut sd = Vec::with_capacity(horizon);
    let mut damp = 0.0;
    let mut var_acc = 1.0;
    for h in 1..=horizon {
        if h >= 2 {
            let c = best.alpha * (1.0 + best.beta * damp);
            var_acc += c * c;
        }
        damp += best.phi.powi(h as i32);
        point.push(l + damp * b);
        sd.push(best.sigma * var_acc.sqrt());
    }
    (point, sd, best)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channels {
    Univariate,
    Independent,
    Mixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exog {
    None,
    PastOnly,
    KnownFuture,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Caps {
    pub min_obs: usize,
    pub max_horizon: usize,
    pub channels: Channels,
    pub exog: Exog,
    pub statics: bool,
    pub quantiles: &'static [f64],
    pub irregular_time: bool,
    pub cost: u8,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            min_obs: 1,
            max_horizon: usize::MAX,
            channels: Channels::Univariate,
            exog: Exog::None,
            statics: false,
            quantiles: &[],
            irregular_time: true,
            cost: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Band {
    pub point: Vec<f64>,
    pub lo: Option<Vec<f64>>,
    pub hi: Option<Vec<f64>>,
    pub quantiles: Option<(Vec<f64>, Vec<Vec<f64>>)>,
}

/// Pluggable predictor trait allowing neural forecasters (e.g. Granite TTM-R3) to inject into `forecast`.
pub trait Predictor {
    fn name(&self) -> &str;
    fn rev(&self) -> String {
        "v1".into()
    }
    fn min_history(&self) -> usize;
    fn max_horizon(&self) -> usize {
        usize::MAX
    }
    fn caps(&self) -> Caps {
        Caps {
            min_obs: self.min_history(),
            max_horizon: self.max_horizon(),
            ..Caps::default()
        }
    }
    fn predict(&self, series: &[f64], horizon: usize) -> Result<Band, String>;
}

pub struct HoltPredictor;

impl Predictor for HoltPredictor {
    fn name(&self) -> &str {
        "holt"
    }
    fn min_history(&self) -> usize {
        3
    }
    fn caps(&self) -> Caps {
        Caps {
            min_obs: 3,
            max_horizon: usize::MAX,
            quantiles: &[0.1, 0.5, 0.9],
            cost: 1,
            ..Caps::default()
        }
    }
    fn predict(&self, series: &[f64], horizon: usize) -> Result<Band, String> {
        let (point, sd, _) = holt_damped(series, horizon);
        let lo = point.iter().zip(&sd).map(|(p, s)| p - 1.2816 * s).collect();
        let hi = point.iter().zip(&sd).map(|(p, s)| p + 1.2816 * s).collect();
        Ok(Band { point, lo: Some(lo), hi: Some(hi), quantiles: None })
    }
}

pub struct LastPredictor;

impl Predictor for LastPredictor {
    fn name(&self) -> &str {
        "last"
    }
    fn min_history(&self) -> usize {
        1
    }
    fn predict(&self, series: &[f64], horizon: usize) -> Result<Band, String> {
        let last = *series.last().ok_or("empty series")?;
        Ok(Band { point: vec![last; horizon], lo: None, hi: None, quantiles: None })
    }
}

pub struct EwmaPredictor {
    pub alpha: f64,
}

impl Predictor for EwmaPredictor {
    fn name(&self) -> &str {
        "ewma"
    }
    fn min_history(&self) -> usize {
        2
    }
    fn predict(&self, series: &[f64], horizon: usize) -> Result<Band, String> {
        let mut it = series.iter();
        let mut level = *it.next().ok_or("empty series")?;
        for v in it {
            level = self.alpha * v + (1.0 - self.alpha) * level;
        }
        Ok(Band { point: vec![level; horizon], lo: None, hi: None, quantiles: None })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryMse {
    pub method: String,
    pub mse: f64,
    pub n: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodWeight {
    pub method: String,
    pub rmse: f64,
    pub history_rmse: Option<f64>,
    pub history_n: u64,
    pub weight: f64,
    #[serde(default)]
    pub point1: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<f64>,
    #[serde(default)]
    pub rev: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forecast {
    pub point: Vec<f64>,
    pub p10: Vec<f64>,
    pub p90: Vec<f64>,
    pub weights: Vec<MethodWeight>,
    pub rmse_fused: f64,
    pub holdout: usize,
    pub method: String,
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantiles: Option<(Vec<f64>, Vec<Vec<f64>>)>,
}

fn variance(y: &[f64]) -> f64 {
    if y.len() < 2 {
        return 0.0;
    }
    let m = y.iter().sum::<f64>() / y.len() as f64;
    y.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (y.len() - 1) as f64
}

pub fn forecast(series: &[f64], horizon: usize, extra: &[&dyn Predictor], history: &[HistoryMse]) -> Forecast {
    forecast_with_prior(series, horizon, extra, history, None)
}

pub fn forecast_with_prior(
    series: &[f64],
    horizon: usize,
    extra: &[&dyn Predictor],
    history: &[HistoryMse],
    prior_s0: Option<f64>,
) -> Forecast {
    let horizon = horizon.max(1);
    let mut notes = Vec::new();
    let clean: Vec<f64> = series.iter().copied().filter(|v| v.is_finite()).collect();
    if clean.len() != series.len() {
        notes.push(format!("excluded {} non-finite points", series.len() - clean.len()));
    }
    let n = clean.len();
    if n == 0 {
        return Forecast {
            point: vec![0.0; horizon],
            p10: vec![0.0; horizon],
            p90: vec![0.0; horizon],
            weights: vec![],
            rmse_fused: 0.0,
            holdout: 0,
            method: "none".into(),
            notes: vec!["empty series".into()],
            quantiles: None,
        };
    }

    let holt = HoltPredictor;
    let last = LastPredictor;
    let ewma = EwmaPredictor { alpha: 0.3 };
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut pool: Vec<&dyn Predictor> = Vec::with_capacity(3 + extra.len());
    for p in [&last as &dyn Predictor, &ewma, &holt].into_iter().chain(extra.iter().copied()) {
        if seen_names.insert(p.name().to_string()) {
            pool.push(p);
        } else {
            notes.push(format!("{}: duplicate predictor name skipped", p.name()));
        }
    }

    if n < MIN_POINTS {
        let sample_var = variance(&clean);
        let (pt, sd, method) = if let Some(s0) = prior_s0.filter(|s| s.is_finite() && *s > 0.0) {
            if n >= 3 {
                let b = ewma.predict(&clean, horizon).unwrap();
                let nu0 = 4.0;
                let df = (n - 1) as f64;
                let sd = ((nu0 * s0 * s0 + df * sample_var) / (nu0 + df)).sqrt().max(1e-4);
                (b.point[0], sd, "ewma")
            } else {
                (*clean.last().unwrap(), s0.max(sample_var.sqrt()).max(1e-4), "last")
            }
        } else {
            (*clean.last().unwrap(), sample_var.sqrt(), "last")
        };
        let widen = |h: usize, sign: f64| pt + sign * 1.2816 * sd * (h as f64).sqrt();
        return Forecast {
            point: vec![pt; horizon],
            p10: (1..=horizon).map(|h| widen(h, -1.0)).collect(),
            p90: (1..=horizon).map(|h| widen(h, 1.0)).collect(),
            weights: vec![],
            rmse_fused: sd,
            holdout: 0,
            method: method.into(),
            notes,
            quantiles: None,
        };
    }

    let hz = HOLDOUT.min(n / 3).max(2);
    let split = n - hz;
    let eps = (0.01 * variance(&clean)).max(1e-12);

    struct Scored {
        idx: usize,
        rmse: f64,
        eff_mse: f64,
        history: Option<(f64, u64)>,
    }
    let mut scored = Vec::new();
    for (idx, p) in pool.iter().enumerate() {
        if hz > p.max_horizon() {
            continue;
        }
        let hist = history.iter().find(|h| h.method == p.name() && h.n > 0 && h.mse.is_finite());
        if split < p.min_history() {
            if n >= p.min_history() {
                if let Some(h) = hist.filter(|h| h.n >= HISTORY_ONLY_MIN_N) {
                    scored.push(Scored {
                        idx,
                        rmse: h.mse.max(0.0).sqrt(),
                        eff_mse: h.mse.max(0.0),
                        history: Some((h.mse.max(0.0).sqrt(), h.n)),
                    });
                }
            }
            continue;
        }
        let mse_res: Result<f64, String> = if p.caps().cost < 2 {
            let mut sq = 0.0;
            let mut cnt = 0usize;
            for t in split..n {
                if t < p.min_history() {
                    continue;
                }
                if let Ok(b) = p.predict(&clean[..t], 1) {
                    if !b.point.is_empty() && b.point[0].is_finite() {
                        sq += (b.point[0] - clean[t]).powi(2);
                        cnt += 1;
                    }
                }
            }
            if cnt > 0 { Ok(sq / cnt as f64) } else { Err("empty".into()) }
        } else {
            match p.predict(&clean[..split], hz) {
                Ok(b) if b.point.len() >= hz && b.point.iter().all(|v| v.is_finite()) => {
                    Ok(b.point[..hz].iter().zip(&clean[split..]).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / hz as f64)
                }
                Ok(_) => Err("bad output".into()),
                Err(e) => Err(e),
            }
        };
        let Ok(mse) = mse_res else { continue };
        let eff_mse = match hist {
            Some(h) => {
                let nh = (h.n as f64).min(HISTORY_CAP);
                (nh * h.mse + mse) / (nh + 1.0)
            }
            None => mse,
        };
        scored.push(Scored {
            idx,
            rmse: mse.sqrt(),
            eff_mse,
            history: hist.map(|h| (h.mse.max(0.0).sqrt(), h.n)),
        });
    }
    if scored.is_empty() {
        let l = *clean.last().unwrap();
        let sd = variance(&clean).sqrt();
        return Forecast {
            point: vec![l; horizon],
            p10: vec![l - 1.2816 * sd; horizon],
            p90: vec![l + 1.2816 * sd; horizon],
            weights: vec![],
            rmse_fused: sd,
            holdout: hz,
            method: "last".into(),
            notes,
            quantiles: None,
        };
    }

    struct Live {
        w: MethodWeight,
        band: Band,
    }
    let mut live: Vec<Live> = Vec::new();
    for s in &scored {
        let p = pool[s.idx];
        if horizon > p.max_horizon() {
            continue;
        }
        if let Ok(b) = p.predict(&clean, horizon) {
            if b.point.len() >= horizon && b.point[..horizon].iter().all(|v| v.is_finite()) {
                let pts: Vec<f64> = b.point.iter().take(horizon).copied().collect();
                let pt1 = pts[0];
                live.push(Live {
                    w: MethodWeight {
                        method: p.name().to_string(),
                        rmse: s.rmse,
                        history_rmse: s.history.map(|h| h.0),
                        history_n: s.history.map_or(0, |h| h.1),
                        weight: 0.0,
                        point1: pt1,
                        points: pts,
                        rev: p.rev(),
                    },
                    band: b,
                });
            }
        }
    }
    let eff_of = |name: &str| {
        scored
            .iter()
            .find(|s| pool[s.idx].name() == name)
            .map(|s| s.eff_mse)
            .unwrap_or(f64::INFINITY)
    };
    let inv: Vec<f64> = live.iter().map(|l| 1.0 / (eff_of(&l.w.method) + eps)).collect();
    let total: f64 = inv.iter().sum::<f64>().max(1e-300);
    for (l, w) in live.iter_mut().zip(&inv) {
        l.w.weight = w / total;
    }
    let rmse_fused = live.iter().map(|l| l.w.weight * eff_of(&l.w.method)).sum::<f64>().sqrt();
    let mut point = vec![0.0; horizon];
    for l in &live {
        for (i, v) in l.band.point.iter().enumerate().take(horizon) {
            point[i] += v * l.w.weight;
        }
    }
    let band_src = live
        .iter()
        .filter(|l| l.band.lo.is_some() && l.band.hi.is_some())
        .max_by(|a, b| a.w.weight.total_cmp(&b.w.weight));
    let quantiles = band_src.and_then(|l| l.band.quantiles.clone());
    let mut p10 = vec![0.0; horizon];
    let mut p90 = vec![0.0; horizon];
    for i in 0..horizon {
        let (lo_off, hi_off) = match band_src {
            Some(l) => (
                (l.band.point[i] - l.band.lo.as_ref().unwrap()[i]).max(0.0),
                (l.band.hi.as_ref().unwrap()[i] - l.band.point[i]).max(0.0),
            ),
            None => (0.0, 0.0),
        };
        let sig = 1.2816 * rmse_fused * (((i + 1) as f64) / hz as f64).sqrt() * std::f64::consts::SQRT_2;
        p90[i] = point[i] + (hi_off * hi_off + sig * sig).sqrt();
        p10[i] = point[i] - (lo_off * lo_off + sig * sig).sqrt();
    }
    let label = live
        .iter()
        .map(|l| format!("{} {:.2}", l.w.method, l.w.weight))
        .collect::<Vec<_>>()
        .join(", ");
    Forecast {
        point,
        p10,
        p90,
        weights: live.into_iter().map(|l| l.w).collect(),
        rmse_fused,
        holdout: hz,
        method: format!("fusion({label})"),
        notes,
        quantiles,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateConfig {
    pub widen_sd: f64,
    pub min_history: usize,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self { widen_sd: 1.0, min_history: 6 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalOutcome {
    Abstained,
    Accepted,
    Warn,
    Held,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GateBand {
    pub k: f64,
    pub z_g: f64,
    pub s_g: f64,
}

impl Default for GateBand {
    fn default() -> Self {
        Self { k: 1.0, z_g: 2.33, s_g: 0.05 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalVerdict {
    pub outcome: TemporalOutcome,
    pub observed: f64,
    pub expected: Option<f64>,
    pub lo: Option<f64>,
    pub hi: Option<f64>,
    pub z: Option<f64>,
    pub method: String,
    pub reason: String,
    #[serde(default)]
    pub inside80: bool,
    #[serde(default)]
    pub severity: u8,
}

impl TemporalVerdict {
    pub fn passes(&self) -> bool {
        self.outcome != TemporalOutcome::Held
    }
}

pub fn norm_cdf(z: f64) -> f64 {
    if !z.is_finite() {
        return 0.5;
    }
    let sign = if z < 0.0 { -1.0 } else { 1.0 };
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592)
            * t
            * (-x * x).exp();
    (0.5 * (1.0 + sign * y)).clamp(0.0, 1.0)
}

pub fn temporal_verdict(observed: f64, f: &Forecast, band: &GateBand) -> TemporalVerdict {
    if !observed.is_finite() || f.point.is_empty() {
        return TemporalVerdict {
            outcome: TemporalOutcome::Held,
            observed,
            expected: None,
            lo: None,
            hi: None,
            z: None,
            method: f.method.clone(),
            reason: "observation not finite".into(),
            inside80: false,
            severity: 100,
        };
    }
    let p50 = f.point[0];
    let scale = (band.k.max(0.1) * band.s_g.max(1e-6)).max(1e-6);
    let half_gate = band.z_g.max(1.2816) * scale;
    let lo = p50 - half_gate;
    let hi = p50 + half_gate;
    let z = (observed - p50) / scale;
    let az = z.abs();
    let inside80 = az <= 1.2816;
    let inside_gate = az <= band.z_g.max(1.2816);
    let outcome = if inside80 {
        TemporalOutcome::Accepted
    } else if inside_gate {
        TemporalOutcome::Warn
    } else {
        TemporalOutcome::Held
    };
    let sev = if inside80 {
        0
    } else {
        let span = (band.z_g - 1.2816).max(1e-3);
        (((az - 1.2816) / span).clamp(0.0, 1.0) * 100.0).round() as u8
    };
    TemporalVerdict {
        outcome,
        observed,
        expected: Some(p50),
        lo: Some(lo),
        hi: Some(hi),
        z: Some(z),
        method: f.method.clone(),
        reason: format!("{outcome:?} z={z:+.2}"),
        inside80,
        severity: sev,
    }
}

pub fn temporal_gate(
    history: &[f64],
    observed: f64,
    sds_sd: Option<f64>,
    extra: &[&dyn Predictor],
    past_errors: &[HistoryMse],
    cfg: &GateConfig,
) -> TemporalVerdict {
    temporal_gate_with_forecast(history, observed, sds_sd, extra, past_errors, cfg).0
}

pub fn temporal_gate_with_forecast(
    history: &[f64],
    observed: f64,
    sds_sd: Option<f64>,
    extra: &[&dyn Predictor],
    past_errors: &[HistoryMse],
    cfg: &GateConfig,
) -> (TemporalVerdict, Option<Forecast>) {
    if !observed.is_finite() {
        return (
            TemporalVerdict {
                outcome: TemporalOutcome::Held,
                observed,
                expected: None,
                lo: None,
                hi: None,
                z: None,
                method: "none".into(),
                reason: "observation not finite".into(),
                inside80: false,
                severity: 100,
            },
            None,
        );
    }
    let usable = history.iter().filter(|v| v.is_finite()).count();
    if usable < cfg.min_history {
        return (
            TemporalVerdict {
                outcome: TemporalOutcome::Abstained,
                observed,
                expected: None,
                lo: None,
                hi: None,
                z: None,
                method: "none".into(),
                reason: format!("insufficient history {usable} < {}", cfg.min_history),
                inside80: true,
                severity: 0,
            },
            None,
        );
    }
    let f = forecast(history, 1, extra, past_errors);
    let widen = cfg.widen_sd * sds_sd.unwrap_or(0.0).max(0.0);
    let (p50, lo, hi) = (f.point[0], f.p10[0] - widen, f.p90[0] + widen);
    let half = ((hi - lo) / 2.0).max(1e-9);
    let z = (observed - p50) / (half / 1.2816);
    let inside = observed >= lo && observed <= hi;
    let verdict = TemporalVerdict {
        outcome: if inside { TemporalOutcome::Accepted } else { TemporalOutcome::Held },
        observed,
        expected: Some(p50),
        lo: Some(lo),
        hi: Some(hi),
        z: Some(z),
        method: f.method.clone(),
        reason: if inside {
            "within band".into()
        } else if observed > hi {
            format!("above band ({observed:.3} > {hi:.3})")
        } else {
            format!("below band ({observed:.3} < {lo:.3})")
        },
        inside80: observed >= f.p10[0] && observed <= f.p90[0],
        severity: if inside { 0 } else { ((z.abs() - 1.2816) / 2.5 * 100.0).clamp(0.0, 100.0).round() as u8 },
    };
    (verdict, Some(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady(n: usize, level: f64) -> Vec<f64> {
        (0..n)
            .map(|i| level + 0.02 * (((i * 7 + 3) % 5) as f64 - 2.0) / 2.0)
            .collect()
    }

    #[test]
    fn holt_follows_linear_trend() {
        let y: Vec<f64> = (0..20).map(|i| 1.0 + 0.5 * i as f64).collect();
        let (p, sd, fit) = holt_damped(&y, 3);
        assert!(fit.sigma < 0.05);
        assert!((p[0] - 11.0).abs() < 0.3);
        assert!(p[2] > p[1] && sd[2] >= sd[0]);
    }

    #[test]
    fn spike_is_held_and_second_consistent_observation_is_accepted() {
        let mut h = steady(20, 0.2);
        let first = temporal_gate(&h, 0.7, Some(0.02), &[], &[], &GateConfig::default());
        assert_eq!(first.outcome, TemporalOutcome::Held);
        assert!(!first.passes());
        h.push(0.7);
        let second = temporal_gate(&h, 0.71, Some(0.02), &[], &[], &GateConfig::default());
        assert_eq!(second.outcome, TemporalOutcome::Accepted);
    }
}
