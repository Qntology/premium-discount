//! Zero-shot screen domains ([`VisionDomainSpec`], e.g. Raon Zena's dictionary `vision` nodes): every domain's bias and prejudice prompts as
//! prompt-ensembled anchors (computed by the text tower once, stored in LanceDB), scored against an image embedding
//! the way noto-sandbox's `VisionBank` scores entities — `bias` = the best calibrated bias anchor (times its phrase
//! weight), `prejudice` = the best prejudice anchor, `score = bias − w·prejudice` — with SigLIP's own calibration,
//! the pair probability `σ(exp(t)·cos + b)`.

use crate::tower::cosine;
use std::collections::HashMap;
pub use premium_discount_sensor::VisionKind;

/// One screen domain to anchor: its prompts (the anchors are looked up by prompt text).
#[derive(Clone, Debug, PartialEq)]
pub struct VisionDomainSpec {
    pub key: String,
    pub kind: VisionKind,
    /// What the host does when the domain is read (Raon Zena: the text node whose expression Zena plays).
    pub link: String,
    pub cooldown_s: u32,
    pub bias: Vec<(String, f32)>,
    pub prejudice: Vec<String>,
}

pub struct VisionDomain {
    pub key: String,
    pub kind: VisionKind,
    pub link: String,
    pub cooldown_s: u32,
    bias: Vec<(Vec<f32>, f32)>,
    prej: Vec<Vec<f32>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisionScore {
    pub key: String,
    pub kind: VisionKind,
    pub link: String,
    pub cooldown_s: u32,
    /// Probability of the best bias prompt.
    pub bias: f32,
    /// Probability of the best prejudice prompt.
    pub prejudice: f32,
    pub score: f32,
    /// Cosine of the best bias anchor.
    pub cos: f32,
}

pub struct AnchorBank {
    pub domains: Vec<VisionDomain>,
    /// Background phrases' anchors: they compete with the labels in the contrast heat.
    pub background: Vec<Vec<f32>>,
    /// `exp(logit_scale)`.
    pub scale: f32,
    pub bias: f32,
    pub prejudice_weight: f32,
}

/// When a screen reading counts: the best domain's cosine reaches its kind's floor and leads the runner-up by a
/// margin. SigLIP's pair probability swings from ~0 to ~1 over a few hundredths of cosine (scale 100, bias −13.3), so
/// the decision is made on the cosine. Measured on synthetic screens (docs/EXPRESSION.md): six screens with content
/// at 0.094–0.198 leading by ≥ 0.077; a blank gradient at 0.019 and a desktop at 0.048, leading by ≤ 0.02.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Acceptance {
    pub scene: f32,
    pub event: f32,
    /// Lower: when in doubt, treat a screen as private.
    pub privacy: f32,
    pub margin: f32,
}

impl Default for Acceptance {
    fn default() -> Self {
        Acceptance {
            scene: 0.08,
            event: 0.09,
            privacy: 0.07,
            margin: 0.03,
        }
    }
}

impl Acceptance {
    /// The best domain if the reading counts.
    pub fn accept<'a>(&self, scores: &'a [VisionScore]) -> Option<&'a VisionScore> {
        let top = scores.iter().max_by(|a, b| a.cos.total_cmp(&b.cos))?;
        let second = scores.iter().filter(|s| s.key != top.key).map(|s| s.cos).fold(-1.0f32, f32::max);
        let floor = match top.kind {
            VisionKind::Scene => self.scene,
            VisionKind::Event => self.event,
            VisionKind::Privacy => self.privacy,
        };
        (top.cos >= floor && top.cos - second >= self.margin).then_some(top)
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

impl AnchorBank {
    /// The domains whose bias prompts all have anchors. Prejudice prompts that are background phrases
    /// (`is_background`) are left out: they compete through [`AnchorBank::background`] instead.
    pub fn from_specs(
        specs: &[VisionDomainSpec],
        background: &[String],
        is_background: impl Fn(&str) -> bool,
        anchors: &HashMap<String, Vec<f32>>,
        scale: f32,
        bias: f32,
    ) -> AnchorBank {
        let domains = specs
            .iter()
            .filter(|v| v.bias.iter().all(|(p, _)| anchors.contains_key(p)))
            .map(|v| VisionDomain {
                key: v.key.clone(),
                kind: v.kind,
                link: v.link.clone(),
                cooldown_s: v.cooldown_s,
                bias: v.bias.iter().filter_map(|(p, w)| anchors.get(p).map(|a| (a.clone(), *w))).collect(),
                prej: v
                    .prejudice
                    .iter()
                    .filter(|p| !is_background(p))
                    .filter_map(|p| anchors.get(p).cloned())
                    .collect(),
            })
            .collect();
        let background = background.iter().filter_map(|b| anchors.get(b).cloned()).collect();
        AnchorBank {
            domains,
            background,
            scale,
            bias,
            prejudice_weight: 0.5,
        }
    }

    /// One domain per label with a single bias anchor (checks and examples).
    pub fn from_vectors(labels: &[String], vectors: &[Vec<f32>], scale: f32, bias: f32) -> AnchorBank {
        let domains = labels
            .iter()
            .zip(vectors)
            .map(|(l, v)| VisionDomain {
                key: l.clone(),
                kind: VisionKind::Scene,
                link: String::new(),
                cooldown_s: 0,
                bias: vec![(v.clone(), 1.0)],
                prej: Vec::new(),
            })
            .collect();
        AnchorBank {
            domains,
            background: Vec::new(),
            scale,
            bias,
            prejudice_weight: 0.5,
        }
    }

    /// An empty bank (no anchors yet) with the model's calibration.
    pub fn empty(scale: f32, bias: f32) -> AnchorBank {
        AnchorBank {
            domains: Vec::new(),
            background: Vec::new(),
            scale,
            bias,
            prejudice_weight: 0.5,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }

    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.domains.iter().position(|d| d.key == key)
    }

    pub fn prob(&self, cos: f32) -> f32 {
        sigmoid(self.scale * cos + self.bias)
    }

    fn one(&self, d: &VisionDomain, v: &[f32]) -> (f32, f32, f32) {
        let (mut b, mut c) = (0f32, -1f32);
        for (a, w) in &d.bias {
            let cos = cosine(v, a);
            b = b.max(self.prob(cos) * w);
            c = c.max(cos);
        }
        let p = d.prej.iter().map(|a| self.prob(cosine(v, a))).fold(0f32, f32::max);
        (b, p, c)
    }

    /// Every domain scored for an image embedding, best first.
    pub fn score(&self, v: &[f32]) -> Vec<VisionScore> {
        let mut out: Vec<VisionScore> = self
            .domains
            .iter()
            .map(|d| {
                let (b, p, c) = self.one(d, v);
                VisionScore {
                    key: d.key.clone(),
                    kind: d.kind,
                    link: d.link.clone(),
                    cooldown_s: d.cooldown_s,
                    bias: b,
                    prejudice: p,
                    score: b - self.prejudice_weight * p,
                    cos: c,
                }
            })
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out
    }

    /// Domain `i`'s pair probability on one cell's dense feature (bias − w·prejudice).
    pub fn heat(&self, i: usize, cell: &[f32]) -> f32 {
        let (b, p, _) = self.one(&self.domains[i], cell);
        b - self.prejudice_weight * p
    }

    /// noto-sandbox's contrast heat of one cell for `labels`: a softmax, at the model's temperature, over each
    /// label's best bias anchor and the background phrases — how much more this cell looks like label k than like
    /// the other labels and the background. Pair probabilities of single cells (MaskCLIP value path) are tiny on
    /// screens, so the minesweeper seeds and the gaze peak use this relative heat.
    pub fn contrast(&self, labels: &[usize], cell: &[f32]) -> Vec<f32> {
        let logits: Vec<f32> = labels
            .iter()
            .map(|&i| {
                self.domains[i]
                    .bias
                    .iter()
                    .map(|(a, _)| cosine(cell, a))
                    .fold(f32::NEG_INFINITY, f32::max)
                    * self.scale
            })
            .chain(self.background.iter().map(|b| cosine(cell, b) * self.scale))
            .collect();
        let m = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let z: f32 = logits.iter().map(|l| (l - m).exp()).sum();
        logits.iter().take(labels.len()).map(|l| (l - m).exp() / z).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(seed: f32) -> Vec<f32> {
        let v: Vec<f32> = (0..32).map(|i| ((i as f32 + 1.0) * seed).sin()).collect();
        crate::tower::unit(v)
    }

    #[test]
    fn the_nearest_anchor_wins_and_prejudice_lowers_a_look_alike() {
        let spec = |key: &str, kind: VisionKind, bias: &[&str], prej: &[&str]| VisionDomainSpec {
            key: key.into(),
            kind,
            link: String::new(),
            cooldown_s: kind.default_cooldown_s(),
            bias: bias.iter().map(|b| (b.to_string(), 1.0)).collect(),
            prejudice: prej.iter().map(|p| p.to_string()).collect(),
        };
        let specs = vec![
            spec("vision.game_victory", VisionKind::Event, &["a victory screen in a video game"], &["a game over screen", "a screen"]),
            spec("vision.game_defeat", VisionKind::Event, &["a game over screen"], &[]),
        ];
        let mut anchors = HashMap::new();
        for (i, p) in ["a victory screen in a video game", "a game over screen", "a screen"].iter().enumerate() {
            anchors.insert(p.to_string(), unit(0.1 + i as f32 * 0.37));
        }
        let background = vec!["a screen".to_string()];
        let bank = AnchorBank::from_specs(&specs, &background, |t| t == "a screen", &anchors, 10.0, -2.0);
        assert_eq!(bank.domains.len(), 2);
        assert_eq!(bank.background.len(), 1, "background prompts compete separately");
        let s = bank.score(&anchors["a game over screen"]);
        assert_eq!(s[0].key, "vision.game_defeat");
        assert!(s[0].bias > 0.99);
        // "a game over screen" is the victory domain's prejudice: its score is pulled down.
        let win = s.iter().find(|x| x.key == "vision.game_victory").unwrap();
        assert!(win.prejudice > 0.99 && win.score < win.bias);
        // Domains without anchors are left out until the anchors exist.
        anchors.remove("a game over screen");
        assert!(AnchorBank::from_specs(&specs, &background, |_| false, &anchors, 10.0, -2.0).index_of("vision.game_defeat").is_none());
    }

    #[test]
    fn a_reading_counts_when_its_best_cosine_is_high_and_clear() {
        let sc = |key: &str, kind: VisionKind, cos: f32| VisionScore {
            key: key.into(),
            kind,
            link: String::new(),
            cooldown_s: 0,
            bias: 0.0,
            prejudice: 0.0,
            score: 0.0,
            cos,
        };
        let a = Acceptance::default();
        // the measured screens
        let code = [
            sc("vision.coding", VisionKind::Scene, 0.094),
            sc("vision.game_victory", VisionKind::Event, 0.017),
        ];
        assert_eq!(a.accept(&code).map(|s| s.key.as_str()), Some("vision.coding"));
        let desktop = [
            sc("vision.coding", VisionKind::Scene, 0.048),
            sc("vision.document", VisionKind::Scene, 0.028),
        ];
        assert!(a.accept(&desktop).is_none(), "too weak");
        let close = [
            sc("vision.coding", VisionKind::Scene, 0.12),
            sc("vision.document", VisionKind::Scene, 0.10),
        ];
        assert!(a.accept(&close).is_none(), "not clear");
        let login = [
            sc("vision.private", VisionKind::Privacy, 0.075),
            sc("vision.document", VisionKind::Scene, 0.02),
        ];
        assert!(a.accept(&login).is_some(), "private screens need less evidence");
    }
}
