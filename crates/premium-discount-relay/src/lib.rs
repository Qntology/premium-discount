//! # premium-discount-relay
//!
//! 같은 주제의 기록끼리 잇기: **Relay Key**(고유명사 · 식별자 · 기호 · 복합 명사 · 따옴표 이름)를 뽑아 다국어 표현을 한 키로
//! 묶고(말레니아 ↔ Malenia), 새 기록이 옛 기록을 **대체**하는지(같은 주장에서 수치 · 단위 · 형용사만 바뀜, "피해 20% → 15%")
//! 판정한다. raon-zena `docs/plans/relative-value-sds.md` 3.4절(신구 대체) · 4.1절(Relay Key)을 처음 구현한 것이다(2026-11-17).
//!
//! 입력은 모델과 상관없는 [`Token`] 문장이다. Stanza(premium-discount-nlp, 기능 `stanza`)의 분석은 [`Sentence::from_stanza`] 로
//! 바꾼다. 임베딩은 premium-discount-nms 의 [`TextEmbedder`](granite 또는 해시)다. 판정 결과의 [`Relation::Supersedes`] 는
//! PD-Verdict 의 `superseded` 입력(옛 기록은 discount)으로 이어진다.
//!
//! **임계값은 SDS 띠다.** 고정 숫자 대신 이번 비교의 코사인 분포에서 `평균 + z·표준편차`(상한 띠)를 구하고, 표본이 모자라거나
//! 띠가 너무 낮으면 바닥값을 쓴다 ([`RelayConfig`]). 바닥값은 granite 실측으로 정했다(README).

use std::collections::{BTreeMap, BTreeSet};

use premium_discount_nms::embed::{cosine, TextEmbedder};
use premium_discount_sds::{matrix, Moments};

/// Stanza 낱말 하나 (UD 표기)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Token {
    pub text: String,
    pub lemma: String,
    /// UPOS (NOUN · PROPN · NUM · ADJ · SYM · X · VERB …)
    pub upos: String,
    /// 의존 관계 (nsubj · obj · root · compound · flat …)
    pub deprel: String,
    /// 머리 낱말 번호 (1부터, 0 은 ROOT)
    pub head: usize,
}

/// 문장 하나: 원문과 낱말들
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sentence {
    pub text: String,
    pub tokens: Vec<Token>,
}

impl Sentence {
    /// Stanza 분석 결과에서 (premium-discount-nlp)
    #[cfg(feature = "stanza")]
    pub fn from_stanza(s: &premium_discount_nlp::stanza::SentenceAnalysis) -> Sentence {
        Sentence {
            text: s.tokens.join(" "),
            tokens: s
                .words
                .iter()
                .map(|w| Token {
                    text: w.text.clone(),
                    lemma: w.lemma.clone(),
                    upos: w.upos.clone(),
                    deprel: w.deprel.clone(),
                    head: w.head,
                })
                .collect(),
        }
    }
}

/// 기록 하나: 문장들과 쓴 시각
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Record {
    pub sentences: Vec<Sentence>,
    pub at_ms: i64,
}

/// Relay Key 의 출처
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyKind {
    /// 따옴표 안 이름 (그대로)
    Quoted,
    /// 고유명사 (PROPN)
    Proper,
    /// 글자와 숫자가 섞인 식별자 (`E0425`, `RTX3050`, `v1.4`)
    Identifier,
    /// 기호 · 외래 토큰 (X · SYM, 예: `wgpu`, `SurfaceError::Outdated`)
    Symbol,
    /// 복합 명사 (`compound` · `flat` 으로 이어진 명사 덩어리)
    Compound,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelayKey {
    /// 정규화한 키 (원형 · 소문자, 따옴표 이름은 그대로)
    pub key: String,
    pub kind: KeyKind,
}

fn lower(s: &str) -> String {
    s.trim().to_lowercase()
}

/// 원형 (없으면 글자). 한국어 Stanza 원형은 형태소를 `+` 로 잇는다(`말레니아+의`, `감소+하+었+다`): 첫 형태소(어간 · 명사)만 쓴다.
fn lemma_or_text(t: &Token) -> String {
    let l = if t.lemma.trim().is_empty() || t.lemma == "_" { &t.text } else { &t.lemma };
    let l = if l.contains('+') && l.len() > 1 { l.split('+').next().unwrap_or(l) } else { l.as_str() };
    lower(l)
}

/// 따옴표(" " · “ ” · ' ' · ‘ ’ · 「 」 · 『 』) 안의 글
pub fn quoted_spans(text: &str) -> Vec<String> {
    const PAIRS: [(char, char); 6] = [('"', '"'), ('\u{201C}', '\u{201D}'), ('\'', '\''), ('\u{2018}', '\u{2019}'), ('\u{300C}', '\u{300D}'), ('\u{300E}', '\u{300F}')];
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(&(_, close)) = PAIRS.iter().find(|(o, _)| *o == chars[i]) {
            if let Some(j) = chars[i + 1..].iter().position(|c| *c == close) {
                let inner: String = chars[i + 1..i + 1 + j].iter().collect();
                if !inner.trim().is_empty() && inner.chars().count() <= 64 {
                    out.push(inner.trim().to_string());
                }
                i += j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_identifier(text: &str) -> bool {
    let letters = text.chars().any(|c| c.is_alphabetic());
    let digits = text.chars().any(|c| c.is_ascii_digit());
    letters && digits && text.chars().count() >= 2
}

/// 문장의 Relay Key (4.1절): 따옴표 이름, 고유명사, 글자 · 숫자 식별자, X · SYM 토큰, 복합 명사 덩어리. 중복은 하나로.
pub fn relay_keys(s: &Sentence) -> Vec<RelayKey> {
    let mut out: BTreeSet<RelayKey> = BTreeSet::new();
    for q in quoted_spans(&s.text) {
        out.insert(RelayKey { key: q, kind: KeyKind::Quoted });
    }
    for t in &s.tokens {
        let surface = t.text.trim();
        if surface.is_empty() {
            continue;
        }
        if t.upos == "PROPN" {
            out.insert(RelayKey { key: lemma_or_text(t), kind: KeyKind::Proper });
        } else if is_identifier(surface) {
            out.insert(RelayKey { key: lower(surface), kind: KeyKind::Identifier });
        } else if matches!(t.upos.as_str(), "X" | "SYM") && surface.chars().any(|c| c.is_alphabetic()) {
            out.insert(RelayKey { key: lower(surface), kind: KeyKind::Symbol });
        }
    }
    // 복합 명사: compound · flat 으로 머리 명사에 붙은 낱말들 + 머리 (문장 순서대로)
    for (h, head) in s.tokens.iter().enumerate() {
        if !matches!(head.upos.as_str(), "NOUN" | "PROPN") {
            continue;
        }
        let mut parts: Vec<(usize, String)> = s
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.head == h + 1 && (t.deprel.starts_with("compound") || t.deprel.starts_with("flat")))
            .map(|(i, t)| (i, lemma_or_text(t)))
            .collect();
        if parts.is_empty() {
            continue;
        }
        parts.push((h, lemma_or_text(head)));
        parts.sort_by_key(|(i, _)| *i);
        let key = parts.into_iter().map(|(_, p)| p).collect::<Vec<_>>().join(" ");
        out.insert(RelayKey { key, kind: KeyKind::Compound });
    }
    out.into_iter().collect()
}

/// 임계값 · 바닥값. 띠는 이번 비교의 코사인 분포에서 `평균 + z·표준편차`, 바닥값보다 낮으면 바닥값.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelayConfig {
    /// 키 묶기의 코사인 바닥 (`relay.cos`)
    pub key_cos_floor: f32,
    /// 키 묶기: 서로 가장 가까운 짝이 다음 후보보다 앞서야 하는 코사인
    pub key_margin: f32,
    /// 문장 맞추기의 코사인 바닥 (`align.cos`)
    pub align_cos_floor: f32,
    /// 상한 띠의 z
    pub band_z: f32,
    /// 띠를 쓰는 최소 표본 수 (그보다 적으면 바닥값)
    pub band_min_samples: u64,
    /// 문장 맞추기에서 같은 줄의 2등보다 앞서야 하는 이중 센터링 잔차
    pub align_margin: f32,
    /// 대체 · 중복으로 보는 Relay Key 포함률 하한 `|K_O ∩ K_N| / |K_O|`
    pub containment: f32,
}

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig {
            key_cos_floor: 0.80,
            key_margin: 0.03,
            align_cos_floor: 0.75,
            band_z: 2.0,
            band_min_samples: 6,
            align_margin: 0.0,
            containment: 0.5,
        }
    }
}

/// 값들의 SDS 상한 띠 (표본이 모자라면 `floor`)
fn upper_band(values: &[f32], floor: f32, cfg: &RelayConfig) -> f32 {
    let m = Moments::of(&values.iter().map(|v| *v as f64).collect::<Vec<_>>());
    if m.n < cfg.band_min_samples {
        return floor;
    }
    ((m.mean + cfg.band_z as f64 * m.sample_sd()) as f32).max(floor)
}

/// 다국어 키 묶기 결과: 키 → 대표 키
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyClusters {
    pub canonical: BTreeMap<String, String>,
    /// 이번에 쓴 코사인 임계값
    pub threshold: f32,
}

impl KeyClusters {
    /// 대표 키 (모르는 키는 그대로)
    pub fn canon(&self, key: &str) -> String {
        self.canonical.get(key).cloned().unwrap_or_else(|| key.to_string())
    }
}

/// 키들을 임베딩 코사인으로 묶는다: 두 키가 **서로 가장 가까운 짝**이고, 코사인이 바닥(`key_cos_floor`) 이상이며 각자의 다음
/// 후보보다 `key_margin` 이상 앞서면 같은 키. 띠(평균 + z·표준편차)는 쓰지 않는다: 키 몇 개의 짧은 이름끼리는 다른 이름도
/// 코사인이 높게 몰려(granite 실측: 말레니아 ↔ malenia 0.833, 라다곤 ↔ radagon 0.848, 다른 이름끼리 0.739 ~ 0.771) 작은 표본의
/// 띠가 같은 이름 짝까지 막는다(0.881). 대표는 ASCII 키가 있으면 그중
/// 사전순 첫째(언어가 달라도 같은 대표가 되도록), 없으면 사전순 첫째. 따옴표 이름 · 식별자는 글자가 같을 때만 묶는다
/// (다른 오류 코드가 닮았다고 하나가 되면 안 된다).
pub fn cluster_keys(keys: &[RelayKey], embedder: &dyn TextEmbedder, cfg: &RelayConfig) -> KeyClusters {
    let uniq: Vec<&RelayKey> = {
        let mut seen = BTreeSet::new();
        keys.iter().filter(|k| seen.insert(k.key.clone())).collect()
    };
    let soft: Vec<usize> = (0..uniq.len()).filter(|&i| matches!(uniq[i].kind, KeyKind::Proper | KeyKind::Compound | KeyKind::Symbol)).collect();
    let texts: Vec<&str> = soft.iter().map(|&i| uniq[i].key.as_str()).collect();
    let vecs = embedder.embed_batch(&texts);
    let mut pairs = Vec::new();
    for a in 0..soft.len() {
        for b in a + 1..soft.len() {
            pairs.push((a, b, cosine(&vecs[a], &vecs[b])));
        }
    }
    let threshold = cfg.key_cos_floor;
    let n = soft.len();
    let mut sim = vec![vec![f32::NEG_INFINITY; n]; n];
    for (a, b, c) in &pairs {
        sim[*a][*b] = *c;
        sim[*b][*a] = *c;
    }
    // 각 키의 가장 가까운 짝과 다음 후보와의 차이
    let best: Vec<Option<(usize, f32)>> = (0..n)
        .map(|a| {
            let mut order: Vec<(usize, f32)> = (0..n).filter(|&b| b != a).map(|b| (b, sim[a][b])).collect();
            order.sort_by(|x, y| y.1.total_cmp(&x.1));
            let first = *order.first()?;
            let second = order.get(1).map(|x| x.1).unwrap_or(f32::NEG_INFINITY);
            (first.1 - second >= cfg.key_margin).then_some(first)
        })
        .collect();
    // 합집합 찾기
    let mut parent: Vec<usize> = (0..uniq.len()).collect();
    fn root(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    for a in 0..n {
        let Some((b, c)) = best[a] else { continue };
        let mutual = matches!(best[b], Some((x, _)) if x == a);
        if mutual && c >= threshold {
            let (ra, rb) = (root(&mut parent, soft[a]), root(&mut parent, soft[b]));
            if ra != rb {
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for i in 0..uniq.len() {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(uniq[i].key.as_str());
    }
    let mut canonical = BTreeMap::new();
    for members in groups.values() {
        let rep = members
            .iter()
            .filter(|m| m.is_ascii())
            .min()
            .or_else(|| members.iter().min())
            .map(|s| s.to_string())
            .unwrap_or_default();
        for m in members {
            canonical.insert(m.to_string(), rep.clone());
        }
    }
    KeyClusters { canonical, threshold }
}

/// 문장의 주장: 핵심 원형(주어 · 목적어 · 뿌리)과 값(수치 · 단위 · 형용사)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Claim {
    pub core: BTreeSet<String>,
    pub values: Vec<String>,
}

/// 3.4절 2단계의 주장 뽑기. 값은 NUM(뒤따르는 단위 낱말 · `%` 를 붙여서)과 ADJ 원형이다.
pub fn claim(s: &Sentence) -> Claim {
    let mut c = Claim::default();
    for (i, t) in s.tokens.iter().enumerate() {
        let rel = t.deprel.split(':').next().unwrap_or("");
        let content = matches!(t.upos.as_str(), "NOUN" | "PROPN" | "VERB" | "ADJ");
        if content && matches!(rel, "nsubj" | "obj" | "root") && t.upos != "ADJ" {
            c.core.insert(lemma_or_text(t));
        }
        if t.upos == "NUM" || (t.text.chars().any(|ch| ch.is_ascii_digit()) && !is_identifier(&t.text)) {
            let mut v = t.text.trim().to_string();
            if let Some(next) = s.tokens.get(i + 1) {
                let unit = next.text.trim();
                if unit == "%" || (next.upos == "NOUN" && next.head == i + 1) || (next.upos == "SYM" && unit.chars().count() <= 3) {
                    v.push_str(unit);
                }
            }
            c.values.push(v);
        } else if t.upos == "ADJ" {
            c.values.push(lemma_or_text(t));
        }
    }
    c.values.sort();
    c
}

/// 바뀐 주장 하나
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// 옛 기록 · 새 기록의 문장 번호
    pub old_sentence: usize,
    pub new_sentence: usize,
    pub core: Vec<String>,
    pub old_values: Vec<String>,
    pub new_values: Vec<String>,
}

/// 두 기록의 관계 (3.4절 4단계)
#[derive(Debug, Clone, PartialEq)]
pub enum Relation {
    /// 새 기록이 옛 기록을 대체한다: 옛 기록은 discount (PD-Verdict `superseded`)
    Supersedes { changes: Vec<Change>, containment: f32 },
    /// 새 기록이 옛 기록을 바뀜 없이 담는다: 중복으로 합친다
    Duplicate { containment: f32 },
    /// 일부만 겹친다: 의미 연결만
    SemanticLink { aligned: usize, containment: f32 },
    Unrelated { containment: f32 },
}

/// 비교의 중간 결과 (진단)
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Comparison {
    /// (옛 문장, 새 문장, 코사인)
    pub aligned: Vec<(usize, usize, f32)>,
    pub align_threshold: f32,
    pub containment: f32,
}

fn core_overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// 옛 기록 `old` 와 새 기록 `new` 를 견준다 (3.4절):
/// 1. 문장 맞추기: 새 × 옛 문장 코사인 행렬 → 이중 센터링 → 1:1 배정, 원래 코사인이 띠(바닥 `align_cos_floor`) 이상인 쌍만.
/// 2. 주장 견주기: 맞춘 쌍의 핵심 원형이 절반 이상 같고 값이 다르면 바뀐 주장.
/// 3. 포함률: 옛 기록 Relay Key(대표 키로) 중 새 기록에도 있는 비율.
/// 4. 판정: 바뀐 주장이 있고 포함률이 하한 이상이고 새 기록이 더 늦으면 대체, 옛 문장이 모두 바뀜 없이 맞춰지고 포함률이
///    하한 이상이면 중복, 일부만 맞으면 의미 연결.
pub fn compare(old: &Record, new: &Record, keys: &KeyClusters, embedder: &dyn TextEmbedder, cfg: &RelayConfig) -> (Relation, Comparison) {
    let old_texts: Vec<&str> = old.sentences.iter().map(|s| s.text.as_str()).collect();
    let new_texts: Vec<&str> = new.sentences.iter().map(|s| s.text.as_str()).collect();
    let (ov, nv) = (embedder.embed_batch(&old_texts), embedder.embed_batch(&new_texts));
    let raw: Vec<Vec<f32>> = nv.iter().map(|n| ov.iter().map(|o| cosine(n, o).max(0.0)).collect()).collect();
    let cells: Vec<f32> = raw.iter().flatten().copied().collect();
    let align_threshold = upper_band(&cells, cfg.align_cos_floor, cfg).min(0.999);
    let centered = matrix::double_center(&raw);
    let assigned = matrix::exclusive_assign_by_score(&centered, -0.99, cfg.align_margin);
    let mut aligned = Vec::new();
    for (n, a) in assigned.iter().enumerate() {
        if let Some((o, _, _)) = a {
            let c = raw[n][*o];
            if c >= align_threshold {
                aligned.push((*o, n, c));
            }
        }
    }
    aligned.sort_by_key(|(o, n, _)| (*o, *n));
    let set = |r: &Record| -> BTreeSet<String> { r.sentences.iter().flat_map(relay_keys).map(|k| keys.canon(&k.key)).collect() };
    let (ko, kn) = (set(old), set(new));
    let containment = if ko.is_empty() { 0.0 } else { ko.intersection(&kn).count() as f32 / ko.len() as f32 };
    let mut changes = Vec::new();
    for (o, n, _) in &aligned {
        let (co, cn) = (claim(&old.sentences[*o]), claim(&new.sentences[*n]));
        if core_overlap(&co.core, &cn.core) >= 0.5 && co.values != cn.values && !(co.values.is_empty() && cn.values.is_empty()) {
            changes.push(Change {
                old_sentence: *o,
                new_sentence: *n,
                core: co.core.intersection(&cn.core).cloned().collect(),
                old_values: co.values,
                new_values: cn.values,
            });
        }
    }
    let comparison = Comparison {
        aligned: aligned.clone(),
        align_threshold,
        containment,
    };
    let covered = containment >= cfg.containment;
    let relation = if !changes.is_empty() && covered && new.at_ms > old.at_ms {
        Relation::Supersedes { changes, containment }
    } else if changes.is_empty() && covered && !old.sentences.is_empty() && aligned.len() == old.sentences.len() {
        Relation::Duplicate { containment }
    } else if !aligned.is_empty() {
        Relation::SemanticLink { aligned: aligned.len(), containment }
    } else {
        Relation::Unrelated { containment }
    };
    (relation, comparison)
}

#[cfg(test)]
mod tests {
    use super::*;
    use premium_discount_nms::embed::HashedNgramEmbedder;

    fn tok(text: &str, lemma: &str, upos: &str, deprel: &str, head: usize) -> Token {
        Token {
            text: text.into(),
            lemma: lemma.into(),
            upos: upos.into(),
            deprel: deprel.into(),
            head,
        }
    }

    /// "Malenia waterfowl dance damage reduced by 20 %" 을 손으로 분석한 문장 (값만 바꿔 쓴다)
    fn patch_note(value: &str) -> Sentence {
        Sentence {
            text: format!("Malenia waterfowl dance damage reduced by {value} % in patch 1.04"),
            tokens: vec![
                tok("Malenia", "Malenia", "PROPN", "compound", 4),
                tok("waterfowl", "waterfowl", "NOUN", "compound", 3),
                tok("dance", "dance", "NOUN", "compound", 4),
                tok("damage", "damage", "NOUN", "nsubj", 5),
                tok("reduced", "reduce", "VERB", "root", 0),
                tok("by", "by", "ADP", "case", 7),
                tok(value, value, "NUM", "obl", 5),
                tok("%", "%", "SYM", "nmod", 7),
                tok("in", "in", "ADP", "case", 10),
                tok("patch", "patch", "NOUN", "obl", 5),
                tok("1.04", "1.04", "NUM", "nummod", 10),
            ],
        }
    }

    #[test]
    fn keys_cover_names_identifiers_symbols_compounds_and_quotes() {
        let s = Sentence {
            text: "wgpu 가 \"Surface lost\" 오류 E0425 를 낸다 (Malenia 보스)".into(),
            tokens: vec![
                tok("wgpu", "wgpu", "X", "nsubj", 6),
                tok("Surface lost", "surface lost", "NOUN", "obj", 6),
                tok("E0425", "E0425", "NOUN", "obj", 6),
                tok("Malenia", "Malenia", "PROPN", "compound", 5),
                tok("보스", "보스", "NOUN", "obl", 6),
                tok("낸다", "내다", "VERB", "root", 0),
            ],
        };
        let keys = relay_keys(&s);
        let has = |k: &str, kind: KeyKind| keys.iter().any(|x| x.key == k && x.kind == kind);
        assert!(has("wgpu", KeyKind::Symbol), "{keys:?}");
        assert!(has("e0425", KeyKind::Identifier), "{keys:?}");
        assert!(has("malenia", KeyKind::Proper), "{keys:?}");
        assert!(has("Surface lost", KeyKind::Quoted), "{keys:?}");
        assert!(has("malenia 보스", KeyKind::Compound), "{keys:?}");
        assert_eq!(quoted_spans("「보스」와 “Malenia” 그리고 'E0425'"), vec!["보스", "Malenia", "E0425"]);
    }

    #[test]
    fn claims_keep_values_with_their_units() {
        let c = claim(&patch_note("20"));
        assert!(c.core.contains("damage") && c.core.contains("reduce"), "{c:?}");
        assert!(c.values.contains(&"20%".to_string()), "{c:?}");
        assert!(c.values.contains(&"1.04".to_string()), "{c:?}");
    }

    #[test]
    fn a_newer_note_with_a_changed_value_supersedes_the_old_one() {
        let e = HashedNgramEmbedder::default();
        let cfg = RelayConfig::default();
        let old = Record { sentences: vec![patch_note("20")], at_ms: 1_000 };
        let new = Record { sentences: vec![patch_note("15")], at_ms: 2_000 };
        let keys = cluster_keys(&[old.clone(), new.clone()].iter().flat_map(|r| r.sentences.iter().flat_map(relay_keys)).collect::<Vec<_>>(), &e, &cfg);
        let (rel, cmp) = compare(&old, &new, &keys, &e, &cfg);
        match rel {
            Relation::Supersedes { changes, containment } => {
                assert_eq!(changes.len(), 1);
                assert!(changes[0].old_values.contains(&"20%".to_string()) && changes[0].new_values.contains(&"15%".to_string()));
                assert!(containment >= 0.99, "{containment}");
            }
            other => panic!("{other:?} {cmp:?}"),
        }
        // 같은 내용을 다시 쓰면 중복, 옛 시각이면 대체가 아님
        assert!(matches!(compare(&old, &Record { at_ms: 3_000, ..old.clone() }, &keys, &e, &cfg).0, Relation::Duplicate { .. }));
        assert!(!matches!(compare(&new, &old, &keys, &e, &cfg).0, Relation::Supersedes { .. }), "older notes never supersede");
        // 다른 이야기
        let other = Record {
            sentences: vec![Sentence {
                text: "Inventory potions are in the second bag".into(),
                tokens: vec![tok("potions", "potion", "NOUN", "nsubj", 0)],
            }],
            at_ms: 4_000,
        };
        assert!(matches!(compare(&old, &other, &keys, &e, &cfg).0, Relation::Unrelated { .. }));
    }

    #[test]
    fn identical_keys_cluster_and_identifiers_never_merge_by_look() {
        let e = HashedNgramEmbedder::default();
        let cfg = RelayConfig::default();
        let k = |key: &str, kind| RelayKey { key: key.into(), kind };
        let c = cluster_keys(&[k("e0425", KeyKind::Identifier), k("e0426", KeyKind::Identifier), k("malenia", KeyKind::Proper)], &e, &cfg);
        assert_eq!(c.canon("e0425"), "e0425");
        assert_eq!(c.canon("e0426"), "e0426");
        assert_eq!(c.canon("unknown"), "unknown");
        assert!(c.threshold >= cfg.key_cos_floor);
    }
}
