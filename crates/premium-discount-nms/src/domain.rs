//! 도메인 점수: 사전의 글 노드마다 은행(bank) 항목 하나 ([`DomainSpec`] → [`DomainEntry`]).
//!
//! 항목의 실제 내용(`payload`, 예: Raon Zena 의 `zena_lexicon::Action`)은 부르는 쪽의 타입 `M` 이고, 점수 계산은 보지 않는다.
//! 사전 형식(logis-center `bias.json`, raon-zena `zena_lexicon`)에서 [`DomainSpec`] 목록을 만드는 것도 부르는 쪽이다.
//!
//! Every text node of the dictionary becomes one bank entry. A span of the user's words is scored the way
//! Qntology/logis-center scores fields against `bias.json`:
//!
//! 1. **exact match before vectors** — the span equals an `exact_match` entry (surface, josa-peeled lemma or
//!    request-stripped stem, spaces ignored) → 0.95; or, for Hangul, an entry is a prefix of the span's last word
//!    with at most two syllables left over (logis-center's agglutinative prefix match: "클리어했다" → "클리어")
//!    → 0.90. The longest entry wins; two domains tied on the same length → no exact verdict.
//! 2. **bias / prejudice vectors** — `bias` = max-pool cosine against the node's bias phrases (times the phrase
//!    weight), `prej` = max-pool cosine against its prejudice phrases (look-alikes that mean something else),
//!    `score = max(bias − prej·w, 0)` with `w = 0.3` for ≤ 2-word spans and `0.7` otherwise.
//!
//! The dictionary's `function` group (request endings, fillers) explains words away: a span made only of them
//! never names a domain.

use crate::analyze::Analyzer;
use crate::embed::{cosine, TextEmbedder};
use crate::korean::is_hangul;
use crate::normalize::{compact, normalize, Word};
use serde::Serialize;
use std::collections::HashSet;

/// How a domain was caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Hit {
    Exact,
    Prefix,
    Vector,
}

pub const EXACT_SCORE: f32 = 0.95;
pub const PREFIX_SCORE: f32 = 0.90;

/// 은행 항목 하나를 만드는 재료: 키, 부르는 쪽의 내용, bias 글(무게), prejudice 글(닮았지만 다른 뜻), 정확 일치 글
#[derive(Debug, Clone, PartialEq)]
pub struct DomainSpec<M> {
    pub key: String,
    pub payload: M,
    pub bias: Vec<(String, f32)>,
    pub prejudice: Vec<String>,
    pub exact: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DomainEntry<M> {
    pub key: String,
    pub payload: M,
    bias: Vec<(Vec<f32>, f32)>,
    prej: Vec<Vec<f32>>,
    /// `(compact form, word count)` of each exact-match entry.
    exact: Vec<(String, usize)>,
}

pub struct DomainBank<M, E: TextEmbedder> {
    entries: Vec<DomainEntry<M>>,
    function: HashSet<String>,
    pub embedder: E,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DomainScore {
    /// Index into the bank (`DomainBank::entry`).
    pub index: usize,
    pub score: f32,
    pub bias: f32,
    pub prej: f32,
    pub hit: Hit,
}

impl<M: Clone, E: TextEmbedder> DomainBank<M, E> {
    /// `specs` 의 항목과 `function_phrases`(요청 어미 · 군말: 이것만으로 된 구간은 도메인을 부르지 않는다)로 만든다.
    pub fn from_specs(specs: Vec<DomainSpec<M>>, function_phrases: &[String], embedder: E) -> Self {
        Self::build(specs, function_phrases, embedder, false)
    }

    /// [`Self::from_specs`] 와 같되, 각 글을 내용어만으로 임베딩한다(규칙 분석기가 요청 어미 · 보조 용언을 뗀다:
    /// "리본 달아 봐" → "리본 달아"). 분석기를 돌릴 때 구간도 그렇게 임베딩되므로, 같은 무리의 공통 어미가 글을 닮게 하지 않는다.
    pub fn from_specs_content(specs: Vec<DomainSpec<M>>, function_phrases: &[String], embedder: E) -> Self {
        Self::build(specs, function_phrases, embedder, true)
    }

    fn build(specs: Vec<DomainSpec<M>>, function_phrases: &[String], embedder: E, content_only: bool) -> Self {
        // Every bias / prejudice phrase in one batch (one forward pass for a neural embedder).
        let phrases: Vec<String> = specs
            .iter()
            .flat_map(|n| n.bias.iter().map(|(t, _)| t.clone()).chain(n.prejudice.iter().cloned()))
            .map(|p| {
                if content_only {
                    let mut n = normalize(&p);
                    crate::analyze::RuleAnalyzer.analyze(&mut n);
                    Self::span_text(&n.words)
                } else {
                    p
                }
            })
            .collect();
        let refs: Vec<&str> = phrases.iter().map(String::as_str).collect();
        let mut vecs = embedder.embed_batch(&refs).into_iter();
        let entries = specs
            .into_iter()
            .map(|n| DomainEntry {
                bias: n.bias.iter().map(|(_, w)| (vecs.next().unwrap_or_default(), *w)).collect(),
                prej: n.prejudice.iter().map(|_| vecs.next().unwrap_or_default()).collect(),
                exact: n
                    .exact
                    .iter()
                    .map(|e| (compact(&normalize(e).text), normalize(e).words.len().max(1)))
                    .filter(|(c, _)| !c.is_empty())
                    .collect(),
                key: n.key,
                payload: n.payload,
            })
            .collect();
        let function = function_phrases
            .iter()
            .flat_map(|p| {
                let n = normalize(p);
                let whole = compact(&n.text);
                n.words.into_iter().map(|w| compact(&w.surface)).chain(std::iter::once(whole))
            })
            .filter(|w| !w.is_empty())
            .collect();
        Self {
            entries,
            function,
            embedder,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entry(&self, i: usize) -> &DomainEntry<M> {
        &self.entries[i]
    }
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.key == key)
    }

    /// A word the function bank explains away (request ending, filler).
    pub fn is_function_word(&self, w: &Word) -> bool {
        [&w.surface, &w.lemma, &w.stem].iter().any(|f| self.function.contains(&compact(f)))
    }

    /// Whether a span can name a domain: at least one content word (analyzer role) that the function bank does
    /// not explain away.
    pub fn eligible(&self, words: &[Word]) -> bool {
        words.iter().any(|w| w.is_content() && !self.is_function_word(w))
    }

    /// The text a span is embedded as: its content words (all words when it has none).
    pub fn span_text(words: &[Word]) -> String {
        let content: Vec<&str> = words.iter().filter(|w| w.is_content()).map(|w| w.surface.as_str()).collect();
        if content.is_empty() {
            words.iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ")
        } else {
            content.join(" ")
        }
    }

    /// STEP 1 (logis-center): exact or agglutinative-prefix match of a span against every `exact_match` entry.
    pub fn exact(&self, words: &[Word]) -> Option<(usize, Hit)> {
        if words.is_empty() {
            return None;
        }
        let n = words.len();
        let head: String = words[..n - 1].iter().map(|w| compact(&w.surface)).collect();
        let last = &words[n - 1];
        // The span with its last word as written, josa-peeled, request-stripped, or as the analyzer's lemma.
        let forms: Vec<String> = [Some(&last.surface), Some(&last.lemma), Some(&last.stem), last.alt.as_ref()]
            .into_iter()
            .flatten()
            .map(|f| format!("{head}{}", compact(f)))
            .collect();
        let surface = &forms[0];
        // (entry, matched length, exact beats prefix)
        let mut hits: Vec<(usize, usize, Hit)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            for (ec, ew) in &e.exact {
                let len = ec.chars().count();
                // Spacing does not matter for an exact hit ("종료해 줘" = "종료해줘"); a prefix only extends the
                // span's last word, so the entry must have as many words as the span.
                if forms.iter().any(|f| f == ec) {
                    hits.push((i, len, Hit::Exact));
                } else if *ew == n && len >= 2 && surface.starts_with(ec.as_str()) && ec.chars().last().is_some_and(is_hangul) {
                    let rest: Vec<char> = surface[ec.len()..].chars().collect();
                    if !rest.is_empty() && rest.len() <= 2 && rest.iter().all(|c| is_hangul(*c)) {
                        hits.push((i, len, Hit::Prefix));
                    }
                }
            }
        }
        let rank = |h: &(usize, usize, Hit)| (h.1, h.2 == Hit::Exact);
        let top = hits.iter().map(rank).max()?;
        let mut winners: Vec<&(usize, usize, Hit)> = hits.iter().filter(|h| rank(h) == top).collect();
        winners.dedup_by_key(|h| h.0);
        // The longest entry wins; two domains tied on it give no exact verdict (logis-center).
        (winners.len() == 1).then(|| (winners[0].0, winners[0].2))
    }

    /// Every domain scored for each span, in bank order (`out[span][domain]`): the spans' content texts are
    /// embedded in one batch, exact / prefix hits override their domain's vector score.
    pub fn raw_scores(&self, spans: &[&[Word]]) -> Vec<Vec<DomainScore>> {
        let texts: Vec<String> = spans.iter().map(|s| Self::span_text(s)).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vecs = self.embedder.embed_batch(&refs);
        spans.iter().zip(&vecs).map(|(s, v)| self.scores_for(s, v)).collect()
    }

    /// Every domain scored for `words` (a span), best first.
    pub fn score_words(&self, words: &[Word]) -> Vec<DomainScore> {
        let v = self.embedder.embed(&Self::span_text(words));
        let mut out = self.scores_for(words, &v);
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out
    }

    fn scores_for(&self, words: &[Word], v: &[f32]) -> Vec<DomainScore> {
        let pw = if words.len() <= 2 { 0.3 } else { 0.7 };
        let exact = self.exact(words);
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let bias = e.bias.iter().map(|(b, w)| cosine(v, b) * w).fold(0f32, f32::max);
                let prej = e.prej.iter().map(|p| cosine(v, p)).fold(0f32, f32::max);
                let mut s = DomainScore {
                    index: i,
                    score: (bias - prej * pw).max(0.0),
                    bias,
                    prej,
                    hit: Hit::Vector,
                };
                if let Some((j, h)) = exact {
                    if j == i {
                        // An exact / prefix match confirms the domain whatever the vectors say.
                        let x = if h == Hit::Exact { EXACT_SCORE } else { PREFIX_SCORE };
                        s.score = s.score.max(x);
                        s.hit = h;
                    }
                }
                s
            })
            .collect()
    }

    /// Every domain scored for free text, best first.
    pub fn score(&self, text: &str) -> Vec<DomainScore> {
        self.score_words(&normalize(text).words)
    }

    /// The best domain of a span, unless the span has no content word.
    pub fn best(&self, words: &[Word]) -> Option<DomainScore> {
        if !self.eligible(words) {
            return None;
        }
        self.score_words(words).into_iter().next()
    }

    /// Exact tokens shared across two or more distinct domains in this bank (ambiguous vocabulary mask).
    pub fn cross_domain_mask(&self) -> HashSet<String> {
        let mut owner_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for e in &self.entries {
            let mut per_entry: HashSet<String> = HashSet::new();
            for (ex, _) in &e.exact {
                per_entry.insert(ex.clone());
            }
            for tok in per_entry {
                *owner_counts.entry(tok).or_insert(0) += 1;
            }
        }
        owner_counts
            .into_iter()
            .filter_map(|(tok, cnt)| (cnt >= 2).then_some(tok))
            .collect()
    }

    /// Bank-neutral score normalization: divides centered scores by overall standard deviation
    /// (`net_sd`) and subtracts the Gumbel expected extreme-value maximum `sqrt(2 * ln(N))`.
    pub fn bank_neutral_scores(raw: &[DomainScore]) -> Vec<f32> {
        let n = raw.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            return vec![raw[0].score];
        }
        let nf = n as f64;
        let mean = raw.iter().map(|r| r.score as f64).sum::<f64>() / nf;
        let var = raw.iter().map(|r| (r.score as f64 - mean).powi(2)).sum::<f64>() / nf;
        let net_sd = var.sqrt().max(1e-6);
        let gumbel = (2.0 * nf.ln()).sqrt();
        raw.iter()
            .map(|r| (((r.score as f64 - mean) / net_sd) - gumbel) as f32)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashedNgramEmbedder;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Act {
        Quit,
        Cheer,
        Stock,
    }

    fn bank() -> DomainBank<Act, HashedNgramEmbedder> {
        let spec = |key: &str, payload: Act, bias: &[&str], prej: &[&str], exact: &[&str]| DomainSpec {
            key: key.into(),
            payload,
            bias: bias.iter().map(|b| (b.to_string(), 1.0)).collect(),
            prejudice: prej.iter().map(|p| p.to_string()).collect(),
            exact: exact.iter().map(|e| e.to_string()).collect(),
        };
        DomainBank::from_specs(
            vec![
                spec("system.exit", Act::Quit, &["앱 꺼 줘", "종료해 줘"], &["게임 종료 화면"], &["종료해 줘", "꺼 줘"]),
                spec("emotion.celebrate", Act::Cheer, &["이겼다", "클리어했다"], &[], &["클리어"]),
                spec("market.down", Act::Stock, &["주가 떨어졌어"], &[], &["떡락"]),
            ],
            &["해 줘".to_string(), "좀".to_string(), "진짜".to_string()],
            HashedNgramEmbedder::default(),
        )
    }

    #[test]
    fn exact_and_agglutinative_prefix_matches_come_before_vectors() {
        let b = bank();
        let w = |t: &str| normalize(t).words;
        let key = |r: Option<(usize, Hit)>| r.map(|(i, h)| (b.entry(i).key.clone(), h));
        assert_eq!(key(b.exact(&w("클리어"))), Some(("emotion.celebrate".into(), Hit::Exact)));
        assert_eq!(key(b.exact(&w("클리어했다"))), Some(("emotion.celebrate".into(), Hit::Prefix)), "two syllables of ending");
        assert_eq!(key(b.exact(&w("떡락했어"))), Some(("market.down".into(), Hit::Prefix)));
        assert_eq!(key(b.exact(&w("종료해 줘"))), Some(("system.exit".into(), Hit::Exact)), "spaces are ignored");
        assert_eq!(b.exact(&w("클리어하였습니다")), None, "more than two syllables left over");
        assert_eq!(b.exact(&w("clearance")), None, "prefix matching is for Hangul only");
        assert_eq!(b.entry(b.score("떡락 가즈아")[0].index).payload, Act::Stock);
    }

    #[test]
    fn function_words_alone_name_nothing_and_the_payload_is_carried() {
        let b = bank();
        assert!(b.best(&normalize("해 줘").words).is_none());
        assert!(b.best(&normalize("좀 진짜").words).is_none());
        let best = b.best(&normalize("종료해 줘").words).unwrap();
        assert_eq!(b.entry(best.index).payload, Act::Quit);
        assert_eq!(b.index_of("market.down"), Some(2));
    }

    #[test]
    fn cross_domain_mask_and_bank_neutral_gumbel() {
        let spec = |key: &str, payload: Act, exact: &[&str]| DomainSpec {
            key: key.into(),
            payload,
            bias: vec![("shared".into(), 1.0)],
            prejudice: vec![],
            exact: exact.iter().map(|e| e.to_string()).collect(),
        };
        let b = DomainBank::from_specs(
            vec![
                spec("a", Act::Quit, &["shared", "alpha"]),
                spec("b", Act::Cheer, &["shared", "beta"]),
            ],
            &[],
            HashedNgramEmbedder::default(),
        );
        let mask = b.cross_domain_mask();
        assert!(mask.contains("shared") && !mask.contains("alpha"));
        let scores = vec![
            DomainScore { index: 0, score: 0.9, bias: 0.9, prej: 0.0, hit: Hit::Vector },
            DomainScore { index: 1, score: 0.3, bias: 0.3, prej: 0.0, hit: Hit::Vector },
        ];
        let z = DomainBank::<Act, HashedNgramEmbedder>::bank_neutral_scores(&scores);
        assert!(z[0] > z[1] && z[0].is_finite() && z[1].is_finite());
    }
}
