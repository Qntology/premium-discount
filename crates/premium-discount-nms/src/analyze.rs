//! NLP-NMS v2, step ②–③: what each word of an utterance does — content, function word, auxiliary / request
//! predicate, or negation — so that only content words can name a domain and negated requests are not acted on.
//!
//! * [`StanzaAnalyzer`] (feature `stanza`) runs the Stanza pipeline (`zena-nlp`, ONNX): UPOS, content stem (lemma),
//!   dependency relation per eojeol. Function units are those logis-center drops from search queries
//!   (`STANZA_DROP_TAGS`: ADP, PUNCT, PART, SCONJ, CCONJ, PRON) when they hold no content morpheme; a verb that
//!   follows a predicate as its `compound`/`aux` with an auxiliary lemma (보 · 주 · 드리 …) is an auxiliary
//!   ("써 봐" → 보, "변신해 줘" → 주); 마 · 말 after a predicate is the prohibitive "-지 마", 안 · 못 before one and
//!   `Polarity=Neg` are negations.
//! * [`RuleAnalyzer`] gives the same roles from closed word lists when no model is installed.
//!
//! Negation scope: a negation covers its clause — the words since the previous clause end (a connective "-고 ·
//! -서" predicate or an earlier negation) — so "끄지 말고 계속 켜 둬" negates only "끄지 말고".
//!
//! 긴 부정 · 이중 부정 · 역접 절 (피드백 해석용, 두 분석기가 같게 본다):
//!
//! * 긴 부정 "-지 않- · -지 못하-"("나쁘지 않았어", "별로진 않네", "가지 못했어")와 내용어 뒤의 "아니다"("추천은 아니야")는
//!   부정(Negation)이다. 금지 "-지 마" 처럼 절 처음까지 거슬러 덮는다.
//! * 가벼운 동사 앞의 짧은 부정("추천 안 해")은 그 앞 명사까지 덮는다 ("추천하지 않아" 와 같은 뜻). 붙여 쓴 "안돼 · 못해" 도 부정이다.
//! * 부정이 겹치면 낱말마다 덮은 수를 센다([`negation_counts`]): "안 좋지 않아" 의 "좋지" 는 2 — 이중 부정은 약한 긍정이다.
//! * 절 나누기([`clauses`]): 역접 · 양보 연결어미(-는데 · -ㄴ데 · -던데 · -지만 · -어도 · -건만 · -길래)로 끝난 절, 그리고 접속
//!   부사(근데 · 하지만 · 그래도 · but · however) 앞 절은 양보 전제절이고, 화자의 결론은 그 뒤 주절에 있다. 영어 "though" 는 반대로
//!   뒤 절이 양보절이다. 문장 끝 "-는데." 처럼 뒤에 내용어가 없으면 경계가 아니다.
//! * 구간 하나(사전 적중)의 절 위치 · 부정 수는 [`read_span`] 이 한 번에 준다: raon-zena 의 피드백 추출(zena-verdict)이 주절의
//!   적중만 쓰고, 부정 수가 홀수면 뒤집고 2 이상 짝수면 약한 긍정으로 본다.

use crate::normalize::{Normalized, Role, Word};
use serde::Serialize;

pub trait Analyzer: Send + Sync {
    fn name(&self) -> &'static str;
    /// Set roles (and analyzer lemmas) on the words of a normalised utterance, then mark negation scopes.
    fn analyze(&self, n: &mut Normalized);
}

/// No analysis: every word is content (the v1 router).
pub struct NoAnalyzer;

impl Analyzer for NoAnalyzer {
    fn name(&self) -> &'static str {
        "none"
    }
    fn analyze(&self, _n: &mut Normalized) {}
}

/// Korean auxiliary / request predicates that follow a main predicate.
const AUX_SURFACES: &[&str] = &[
    "봐",
    "봐요",
    "봐라",
    "봐줘",
    "볼래",
    "보자",
    "줘",
    "줘요",
    "주세요",
    "줄래",
    "줄래요",
    "주라",
    "드려",
    "드릴게",
];
/// Lemmas of the same, as Stanza gives them.
#[cfg(feature = "stanza")]
const AUX_LEMMAS: &[&str] = &["보", "주", "드리", "버리", "놓", "두"];
/// The prohibitive auxiliary "-지 마 / 말다".
const PROHIBIT_SURFACES: &[&str] = &["마", "마라", "마요", "말고", "말아", "말아줘", "말아요", "말자", "말래"];
#[cfg(feature = "stanza")]
const PROHIBIT_LEMMAS: &[&str] = &["마", "말"];
/// Negating adverbs before a predicate.
const NEG_ADVERBS: &[&str] = &["안", "못", "not", "never", "don't", "dont"];
/// Korean determiners before a noun ("이 보스", "그 패턴").
const KO_DETERMINERS: &[&str] = &["이", "그", "저", "이런", "그런", "저런"];
/// English function words (tokens Stanza would tag ADP / DET / PRON / PART).
const EN_FUNCTION: &[&str] = &[
    "the", "a", "an", "to", "of", "in", "on", "at", "for", "with", "me", "my", "you", "your", "it", "please",
];
/// 긴 부정이 붙는 앞말의 끝: "나쁘**지** 않아", "나쁘**지는** 않아", "별로**진** 않네", "나쁘**지도** 않고", "나쁘**질** 않아"
const LONG_NEG_HOSTS: &[&str] = &["지", "지는", "지도", "지를", "진", "질"];
/// 긴 부정의 뒷말: "-지 않다" 의 않-, "-지 못하다" 의 못하- ("않았어", "않네", "못했어", "못한다")
const LONG_NEG_PREFIXES: &[&str] = &["않", "못하", "못해", "못했", "못한", "못할", "못함", "못합"];
/// 서술격 부정 "아니다" ("추천은 아니야", "나쁜 건 아니고"). 내용어 뒤에서만 부정이다: 발화 첫 "아니" 는 감탄사다.
const COPULA_NEG: &[&str] = &[
    "아니야", "아니에요", "아니예요", "아니다", "아니었어", "아니었다", "아냐", "아닌데", "아니고", "아님", "아니지", "아닙니다", "아니라",
    "아니네", "아닌",
];
/// 짧은 부정이 붙어 한 낱말이 된 서술어 ("하면 안돼", "추천 못해"): 그 낱말과 바로 앞 내용어를 덮는다.
const FUSED_SHORT_NEG: &[&str] = &[
    "안돼", "안돼요", "안됨", "안되", "안됐어", "안됐다", "안된다", "안해", "안해요", "안함", "안했어", "안했다", "안한다", "못해",
    "못해요", "못함", "못했어", "못했다", "못한다", "못하겠어",
];
/// 긴 부정이 한 낱말에 붙은 꼴 ("나쁘지않았어", "가지못했어")
const FUSED_LONG_NEG: &[&str] = &["지않", "진않", "지못하", "지못해", "지못했", "지못한", "지못할", "지못함"];
/// 다음 절을 여는 역접 접속 부사 · 접속사: 그 앞 절이 양보 전제절이고 뒤 절이 주절이다 ("딜은 좋아 근데 비추")
const ADVERSATIVE_CONJ: &[&str] =
    &["근데", "그런데", "하지만", "그렇지만", "그치만", "그래도", "그러나", "다만", "반면", "반면에", "but", "however"];
/// 뒤 절을 양보절로 만드는 영어 접속사 ("good though it drains stamina": 앞이 주절)
const CONCESSIVE_CONJ: &[&str] = &["though", "although"];
/// "-ㄴ데 · -어도" 처럼 보이지만 연결어미가 아닌 낱말
const NOT_ADVERSATIVE: &[&str] = &["가운데", "한가운데", "적어도", "아무래도", "그래도"];

fn ends_any(s: &str, tails: &[&str]) -> bool {
    tails.iter().any(|t| s.ends_with(t))
}

/// 한글 음절의 받침 번호 (0 은 받침 없음, 4 는 ㄴ). 한글 음절이 아니면 None.
fn jong(c: char) -> Option<u32> {
    let u = c as u32;
    (0xAC00..=0xD7A3).contains(&u).then(|| (u - 0xAC00) % 28)
}

/// A predicate form that can host an auxiliary or a prohibition ("써", "변신해", "끄지", "알려주지").
fn is_predicate_like(w: &Word) -> bool {
    if matches!(w.upos.as_str(), "VERB" | "ADJ" | "AUX") {
        return true;
    }
    // Hangul syllables are atomic, so the common verb-final syllables are listed as they are written.
    ends_any(
        &w.surface,
        &[
            "지", "어", "아", "여", "해", "워", "와", "져", "게", "줘", "써", "봐", "가", "꺼", "켜", "떠", "놔", "둬", "차",
        ],
    )
}

/// 긴 부정의 뒷말인가: "-지" 로 끝난 앞말 뒤의 "않- · 못하-" ("나쁘지 않았어", "가지 못했어"), 또는 앞말 뒤의 "아니다"
/// ("추천은 아니야"). 두 분석기가 같이 쓴다.
fn is_long_negation(prev: &Word, w: &Word) -> bool {
    let s = w.surface.as_str();
    let host = prev.surface.chars().count() >= 2 && ends_any(&prev.surface, LONG_NEG_HOSTS);
    (host && LONG_NEG_PREFIXES.iter().any(|p| s.starts_with(p))) || (COPULA_NEG.contains(&s) && prev.role != Role::Function)
}

/// 가벼운 동사 ("추천 안 **해**", "공략 못 **했어**", "적용 안 **돼**"): 뜻은 앞 명사가 진다
fn is_light_verb(w: &Word) -> bool {
    ["하", "해", "했", "한", "할", "함", "합", "돼", "됐", "된", "될", "됨", "되"]
        .iter()
        .any(|p| w.surface.starts_with(p))
}

/// A clause-ending connective predicate ("끄지 말고", "와서").
fn ends_clause(w: &Word) -> bool {
    w.role != Role::Function && ends_any(&w.surface, &["고", "서", "는데", "지만"]) && w.surface.chars().count() >= 2
}

/// 역접 · 양보 연결어미로 끝난 서술어인가: -는데 · -ㄴ데("쎈데", "좋은데", "보스인데") · -던데 · -지만 · -건만 · -길래 · -더라도 ·
/// -어도/-아도("좋아도", "먹어도", 줄어든 "해도 · 봐도 · 어려워도")
pub fn adversative_ending(w: &Word) -> bool {
    let s = w.surface.as_str();
    let cs: Vec<char> = s.chars().collect();
    if w.role == Role::Function || cs.len() < 2 || NOT_ADVERSATIVE.contains(&s) {
        return false;
    }
    if ends_any(s, &["는데", "던데", "지만", "건만", "길래", "더라도"]) {
        return true;
    }
    let (last, prev) = (cs[cs.len() - 1], cs[cs.len() - 2]);
    // -ㄴ데: 앞 음절 받침이 ㄴ
    if last == '데' && jong(prev) == Some(4) {
        return true;
    }
    if last == '도' {
        // 받침 있는 줄기 + 어도/아도 ("좋아도"; "말레니아도" 는 받침이 없어 조사 "도")
        if matches!(prev, '어' | '아') && cs.len() >= 3 && jong(cs[cs.len() - 3]).is_some_and(|j| j != 0) {
            return true;
        }
        // 줄어든 꼴 + 도
        if matches!(prev, '해' | '봐' | '써' | '와' | '줘' | '돼' | '워' | '여' | '져' | '쳐' | '려' | '켜' | '놔' | '둬') {
            return true;
        }
    }
    false
}

fn is_adversative_conj(w: &Word) -> bool {
    ADVERSATIVE_CONJ.contains(&w.surface.as_str())
}

/// 한 낱말 안에 부정이 붙어 있나 ("나쁘지않았어", "앱끄지마", "안돼")
fn fused_negation(w: &Word) -> bool {
    let s = w.surface.as_str();
    let n = s.chars().count();
    FUSED_SHORT_NEG.contains(&s)
        || (n > 2 && FUSED_LONG_NEG.iter().any(|t| s.contains(t)))
        || (n > 2 && ["지마", "지말고", "지말아", "지마라"].iter().any(|t| s.ends_with(t)))
}

fn cover(counts: &mut [u8], a: usize, b: usize) {
    for c in &mut counts[a..=b] {
        *c = c.saturating_add(1);
    }
}

/// 낱말마다 그 낱말을 덮는 부정의 수 (0 이면 부정 아님). 부정 범위 규칙은 이 모듈 머리말 그대로다:
///
/// * 짧은 부정("안 가")은 다음 낱말을 앞으로 덮는다. 다음이 가벼운 동사("추천 안 해")면 그 앞 내용어까지.
/// * 금지 · 긴 부정("끄지 마", "나쁘지 않아")은 절 처음까지 뒤로 덮는다. 바로 앞 서술어가 짧은 부정의 대상이면("안 좋지 않아")
///   그 짧은 부정 너머까지 덮어, 그 낱말들은 2가 된다(이중 부정).
/// * 붙여 쓴 짧은 부정("하면 안돼")은 그 낱말과 바로 앞 내용어, 붙여 쓴 긴 부정("나쁘지않았어")은 절 처음부터 그 낱말까지,
///   붙여 쓴 금지("앱끄지마")는 원래 규칙대로 앞의 붙여 쓴 금지 뒤부터.
/// * 절은 연결어미(-고 · -서), 역접 · 양보 연결어미([`adversative_ending`]), 역접 접속 부사(근데 · 하지만 · but)에서 끊긴다.
pub fn negation_counts(words: &[Word]) -> Vec<u8> {
    let n = words.len();
    let mut counts = vec![0u8; n];
    if n == 0 {
        return counts;
    }
    // 절의 처음 (절 경계와 뒤로 덮는 부정이 옮긴다)
    let mut clause_start = 0;
    // 앞으로 덮는 부정이 끝난 다음 자리: 그 뒤의 뒤로 덮는 부정은 여기서부터 덮는다 (원래 규칙)
    let mut adverb_end = 0;
    // 마지막 짧은 부정의 범위 (이중 부정 판단)
    let mut last_adverb: Option<(usize, usize)> = None;
    for i in 0..n {
        let w = &words[i];
        let s = w.surface.as_str();
        if w.role == Role::Negation {
            let adverb = NEG_ADVERBS.contains(&s) && w.upos != "AUX";
            if adverb {
                // A negating adverb ("안 가") scopes forward over its predicate.
                let b = (i + 1).min(n - 1);
                let start = clause_start.max(adverb_end);
                let a = if b > i && i > start && is_light_verb(&words[b]) && words[i - 1].is_content() { i - 1 } else { i };
                cover(&mut counts, a, b);
                last_adverb = Some((a, b));
                adverb_end = b + 1;
            } else {
                // A prohibition ("끄지 마") or a long negation ("나쁘지 않아") scopes backward over its clause.
                let start = match last_adverb {
                    Some((a, b)) if b + 1 == i => clause_start.min(a),
                    _ => clause_start.max(adverb_end),
                };
                cover(&mut counts, start, i);
                clause_start = i + 1;
                last_adverb = None;
            }
        } else if FUSED_SHORT_NEG.contains(&s) {
            let start = clause_start.max(adverb_end);
            let a = if i > start && words[i - 1].is_content() { i - 1 } else { i };
            cover(&mut counts, a, i);
            clause_start = i + 1;
            last_adverb = None;
        } else if s.chars().count() > 2 && FUSED_LONG_NEG.iter().any(|t| s.contains(t)) {
            cover(&mut counts, clause_start.max(adverb_end), i);
            clause_start = i + 1;
            last_adverb = None;
        } else if is_adversative_conj(w) {
            clause_start = i + 1;
            last_adverb = None;
        } else if (ends_clause(w) || adversative_ending(w)) && i > 0 && words[i - 1].role != Role::Negation {
            // "와서" closes a clause unless the clause is the negated "-지 말고" itself (handled above).
            let next_is_neg = words.get(i + 1).is_some_and(|w| w.role == Role::Negation);
            if !next_is_neg {
                clause_start = i + 1;
                last_adverb = None;
            }
        }
    }
    // A prohibition fused into one word ("끄지마", "가지말고").
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        let s = w.surface.as_str();
        if ["지마", "지말고", "지말아", "지마라"].iter().any(|t| s.ends_with(t)) && s.chars().count() > 2 {
            cover(&mut counts, start, i);
            start = i + 1;
        }
    }
    counts
}

/// Mark every word in the scope of a negation (`Word::negated`).
pub fn mark_negation(words: &mut [Word]) {
    let counts = negation_counts(words);
    for (w, c) in words.iter_mut().zip(counts) {
        if c > 0 {
            w.negated = true;
        }
    }
}

/// 절 하나: 낱말 범위와 양보절 여부, 부정소 수 ([`clauses`])
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Clause {
    /// 낱말 범위 `[start, end)`
    pub start: usize,
    pub end: usize,
    /// 양보 · 역접 전제절 ("딜은 쎈데", "좋긴 하지만", "근데" 앞 절, "though" 뒤 절): 화자의 결론은 다른 절(주절)에 있다
    pub concessive: bool,
    /// 이 절의 부정소 수 (짧은 부정 "안 · 못", 긴 부정 "-지 않- · -지 못하-", 금지 "-지 마", "아니다", 붙여 쓴 "안돼 · 못해")
    pub negators: u8,
}

impl Clause {
    pub fn contains(&self, i: usize) -> bool {
        i >= self.start && i < self.end
    }
}

/// 발화를 절로 나눈다 (읽는 순서, 낱말이 없으면 빈 목록). 경계:
///
/// * 역접 · 양보 연결어미([`adversative_ending`])로 끝난 낱말 뒤 — 그 절은 양보절. 뒤에 내용어가 없으면(문장 끝 "-는데.") 경계가 아니다.
/// * 역접 접속 부사(근데 · 하지만 · 그래도 · but · however) 앞 — 앞 절이 양보절, 접속 부사는 뒤 절의 첫 낱말.
/// * 영어 "though · although"(발화 처음 · 끝이 아닐 때) 앞 — 뒤 절이 양보절.
/// * 연결어미 "-고 · -서" 뒤 (보조 용언 구성 "-고 싶다 · -고 있다" 와 부정 "-지 말고" 는 빼고) — 양보절 아님.
pub fn clauses(words: &[Word]) -> Vec<Clause> {
    let n = words.len();
    if n == 0 {
        return Vec::new();
    }
    let content_after = |i: usize| words[i + 1..].iter().any(|w| w.is_content());
    // (경계 뒤 첫 낱말 번호, 경계 앞 절이 양보절인가, 경계 뒤 절이 양보절인가)
    let mut cuts: Vec<(usize, bool, bool)> = Vec::new();
    for i in 0..n {
        let w = &words[i];
        let s = w.surface.as_str();
        if i > 0 && is_adversative_conj(w) {
            cuts.push((i, true, false));
        } else if i > 0 && i + 1 < n && CONCESSIVE_CONJ.contains(&s) {
            cuts.push((i, false, true));
        } else if i + 1 < n && adversative_ending(w) && content_after(i) {
            cuts.push((i + 1, true, false));
        } else if i + 1 < n && w.role != Role::Negation && ends_any(s, &["고", "서"]) && s.chars().count() >= 2 && w.role != Role::Function {
            let next = words[i + 1].surface.as_str();
            let auxiliary = ["싶", "있", "계", "나서", "말", "보"].iter().any(|p| next.starts_with(p)) || words[i + 1].role == Role::Negation;
            if !auxiliary {
                cuts.push((i + 1, false, false));
            }
        }
    }
    cuts.dedup_by_key(|c| c.0);
    let mut out = Vec::new();
    let mut start = 0;
    let mut concessive_next = false;
    for &(at, before, after) in &cuts {
        if at <= start || at >= n {
            continue;
        }
        out.push(Clause {
            start,
            end: at,
            concessive: before || concessive_next,
            negators: 0,
        });
        start = at;
        concessive_next = after;
    }
    out.push(Clause {
        start,
        end: n,
        concessive: concessive_next,
        negators: 0,
    });
    for c in &mut out {
        let k = words[c.start..c.end].iter().filter(|w| w.role == Role::Negation || fused_negation(w)).count();
        c.negators = k.min(u8::MAX as usize) as u8;
    }
    out
}

/// 부정 수를 읽는 법
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Polarity {
    /// 부정 없음
    Affirmed,
    /// 부정이 홀수 번: 뜻이 뒤집힌다 ("추천 안 해", "별로지 않았어")
    Negated,
    /// 부정이 2 이상 짝수 번: 이중 부정 = 약한 긍정 ("안 좋지 않아")
    DoubleNegated,
}

impl Polarity {
    pub fn from_count(negations: u8) -> Polarity {
        match negations {
            0 => Polarity::Affirmed,
            k if k % 2 == 1 => Polarity::Negated,
            _ => Polarity::DoubleNegated,
        }
    }
}

/// 구간 하나(사전 적중 등)의 절 위치와 부정 ([`read_span`]): 피드백 추출이 "역접이면 주절만", "부정이면 뒤집기"를 그대로 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SpanReading {
    /// 구간 마지막 낱말이 든 절 ([`clauses`] 의 번호)
    pub clause: usize,
    /// 그 절이 양보 · 역접 전제절이 아니다 (화자의 결론). 역접 복합문에서는 이쪽의 평가만 쓴다.
    pub main: bool,
    /// 구간 낱말을 덮는 부정 수의 최댓값 ([`negation_counts`]): 1 이면 부정, 2 이면 이중 부정
    pub negations: u8,
}

impl SpanReading {
    pub fn polarity(&self) -> Polarity {
        Polarity::from_count(self.negations)
    }
}

/// 낱말 범위 `[start, end)` 의 절 위치와 부정 수. 범위가 비었거나 낱말 밖이면 마지막 낱말로 본다.
pub fn read_span(words: &[Word], start: usize, end: usize) -> SpanReading {
    if words.is_empty() {
        return SpanReading { clause: 0, main: true, negations: 0 };
    }
    let n = words.len();
    let last = end.clamp(1, n) - 1;
    let start = start.min(last);
    let cl = clauses(words);
    let clause = cl.iter().position(|c| c.contains(last)).unwrap_or(cl.len() - 1);
    let counts = negation_counts(words);
    SpanReading {
        clause,
        main: !cl[clause].concessive,
        negations: counts[start..=last].iter().copied().max().unwrap_or(0),
    }
}

/// Closed-class rules (no model).
pub struct RuleAnalyzer;

impl RuleAnalyzer {
    pub fn roles(words: &mut [Word]) {
        for i in 0..words.len() {
            let s = words[i].surface.as_str();
            let after_predicate = i > 0 && is_predicate_like(&words[i - 1]);
            let long_negation = i > 0 && is_long_negation(&words[i - 1], &words[i]);
            words[i].role = if (PROHIBIT_SURFACES.contains(&s) && after_predicate) || long_negation || NEG_ADVERBS.contains(&s) {
                Role::Negation
            } else if AUX_SURFACES.contains(&s) && after_predicate {
                Role::Auxiliary
            } else if EN_FUNCTION.contains(&s)
                || ADVERSATIVE_CONJ.contains(&s)
                || CONCESSIVE_CONJ.contains(&s)
                || (KO_DETERMINERS.contains(&s) && i + 1 < words.len())
            {
                Role::Function
            } else {
                Role::Content
            };
        }
    }
}

impl Analyzer for RuleAnalyzer {
    fn name(&self) -> &'static str {
        "rules"
    }
    fn analyze(&self, n: &mut Normalized) {
        Self::roles(&mut n.words);
        mark_negation(&mut n.words);
    }
}

/// The Stanza pipeline (ONNX) for Korean and the other converted languages; falls back to [`RuleAnalyzer`] for a
/// language it cannot analyze or when the units do not line up with the words.
#[cfg(feature = "stanza")]
pub struct StanzaAnalyzer {
    /// The app language used to break ties in language detection.
    pub ui_language: String,
}

#[cfg(feature = "stanza")]
impl StanzaAnalyzer {
    /// Opens the model of `lang` now (the first open takes about half a second) so the first utterance is not slow.
    pub fn warm(lang: &str) -> anyhow::Result<()> {
        premium_discount_nlp::stanza::pipeline(lang).map(|_| ())
    }

    fn roles(words: &mut [Word], qa: &premium_discount_nlp::stanza::QueryAnalysis) {
        // 구문 머리 (단위 번호): 구문 압축 구간(crate::span::head_arg_spans)이 쓴다
        let parsed: Vec<Vec<usize>> = qa.units.iter().map(|u| u.words.iter().map(|w| w.head).collect()).collect();
        let heads = crate::span::unit_heads(&parsed);
        for (i, u) in qa.units.iter().enumerate() {
            let lemma = u.words.first().map(|w| w.lemma.as_str()).unwrap_or("");
            // A predicate before, by the parser or by its ending ("가지 마": the tagger may read 가지 as a noun).
            let after_predicate = i > 0 && is_predicate_like(&words[i - 1]);
            // 긴 부정 "-지 않-": 파서가 Polarity=Neg 를 주지 않아도 규칙 분석기와 같게
            let long_negation = i > 0 && is_long_negation(&words[i - 1], &words[i]);
            let neg_feat = u.words.iter().any(|w| w.feats.contains("Polarity=Neg"));
            let w = &mut words[i];
            w.upos = u.upos.clone();
            w.deprel = u.deprel.clone();
            w.head = heads.get(i).copied().flatten();
            if !u.stem.is_empty() && u.stem != w.surface {
                w.alt = Some(u.stem.to_lowercase());
            }
            let verbal = matches!(u.upos.as_str(), "VERB" | "AUX");
            w.role = if (verbal && PROHIBIT_LEMMAS.contains(&lemma) && after_predicate)
                || neg_feat
                || long_negation
                || (u.upos == "ADV" && NEG_ADVERBS.contains(&w.surface.as_str()))
            {
                Role::Negation
            } else if verbal && after_predicate && AUX_LEMMAS.contains(&lemma) {
                // "-아/어 보다 · 주다" after the main predicate, whatever relation the parser gave it.
                Role::Auxiliary
            } else if (u.functional && premium_discount_nlp::stanza::is_drop_tag(&u.upos)) || u.upos == "DET" {
                // Determiners ("이 보스") name nothing either.
                Role::Function
            } else {
                Role::Content
            };
        }
    }
}

#[cfg(feature = "stanza")]
impl Analyzer for StanzaAnalyzer {
    fn name(&self) -> &'static str {
        "stanza"
    }
    fn analyze(&self, n: &mut Normalized) {
        let lang = premium_discount_nlp::stanza::detect_query_lang(&n.text, &self.ui_language);
        let analysis = lang.and_then(|l| premium_discount_nlp::stanza::analyze_query(&n.text, l).ok());
        match analysis {
            Some(qa) if qa.whitespace_units && qa.units.len() == n.words.len() => Self::roles(&mut n.words, &qa),
            _ => RuleAnalyzer::roles(&mut n.words),
        }
        mark_negation(&mut n.words);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::normalize;

    fn roles(text: &str) -> Vec<(String, Role, bool)> {
        let mut n = normalize(text);
        RuleAnalyzer.analyze(&mut n);
        n.words.into_iter().map(|w| (w.surface, w.role, w.negated)).collect()
    }

    fn analyzed(text: &str) -> Vec<Word> {
        let mut n = normalize(text);
        RuleAnalyzer.analyze(&mut n);
        n.words
    }

    fn at(words: &[Word], surface: &str) -> usize {
        words.iter().position(|w| w.surface == surface).unwrap_or_else(|| panic!("{surface} not in {words:?}"))
    }

    #[test]
    fn auxiliaries_requests_and_negation_scopes() {
        let r = roles("안경이랑 헤드셋 써 봐");
        assert_eq!(r[3].1, Role::Auxiliary, "{r:?}");
        assert!(r.iter().all(|x| !x.2));
        let r = roles("앱 끄지 마");
        assert_eq!(r[2].1, Role::Negation);
        assert!(r.iter().all(|x| x.2), "the whole clause is negated: {r:?}");
        let r = roles("끄지 말고 계속 켜 둬");
        assert_eq!(
            r.iter().map(|x| x.2).collect::<Vec<_>>(),
            [true, true, false, false, false],
            "{r:?}"
        );
        let r = roles("앱끄지마");
        assert!(r[0].2, "fused prohibition");
        let r = roles("나 오늘 안 가");
        assert_eq!((r[2].1, r[3].2, r[0].2), (Role::Negation, true, false), "{r:?}");
        let r = roles("이거 봐 봐");
        assert_eq!(r[1].1, Role::Content, "a lone 봐 after a noun is the main verb");
        assert_eq!(r[2].1, Role::Auxiliary);
    }

    #[test]
    fn fc01_long_form_negation_and_litotes_survive_without_a_model() {
        // "우회하긴 했는데 나쁘지 않았어": 긴 부정은 부정, 양보절은 덮지 않고, 주절의 "나쁘지" 는 부정 1번 (약한 긍정의 재료)
        let w = analyzed("우회하긴 했는데 나쁘지 않았어");
        let (bad, not) = (at(&w, "나쁘지"), at(&w, "않았어"));
        assert_eq!(w[not].role, Role::Negation, "{w:?}");
        assert!(w[bad].negated && w[not].negated);
        assert!(!w[0].negated && !w[1].negated, "the concessive clause is not negated: {w:?}");
        let r = read_span(&w, bad, bad + 1);
        assert_eq!((r.main, r.negations, r.polarity()), (true, 1, Polarity::Negated), "{r:?}");
        let cl = clauses(&w);
        assert_eq!(cl.len(), 2, "{cl:?}");
        assert!(cl[0].concessive && !cl[1].concessive);
        assert_eq!((cl[0].negators, cl[1].negators), (0, 1));

        // 같은 문장 뒤에 명시적 "추천!" 이 붙으면 그 낱말은 부정 밖이다 (합산은 zena-verdict)
        let w = analyzed("우회하긴 했는데 나쁘지 않았어, 추천!");
        let rec = at(&w, "추천");
        assert!(!w[rec].negated, "{w:?}");
        let r = read_span(&w, rec, rec + 1);
        assert_eq!((r.main, r.negations), (true, 0));

        // 요청문 "다른 공략 추천해 줘": "줘" 는 요청 보조 용언이고 부정은 없다 (서법 게이트는 zena-verdict 가 이것으로 막는다)
        let w = analyzed("다른 공략 추천해 줘");
        assert_eq!(w[at(&w, "줘")].role, Role::Auxiliary, "{w:?}");
        assert!(w.iter().all(|x| !x.negated));

        // 다른 긴 부정 꼴: "-지는 · -진 · -지 못하-", 붙여 쓴 꼴
        for text in ["별로지 않았어", "별로진 않네", "나쁘지는 않았어", "그 길로는 가지 못했어"] {
            let w = analyzed(text);
            let last = w.len() - 1;
            assert_eq!(w[last].role, Role::Negation, "{text}: {w:?}");
            assert_eq!(read_span(&w, last - 1, last).negations, 1, "{text}");
        }
        let w = analyzed("나쁘지않았어");
        assert_eq!(read_span(&w, 0, 1).negations, 1, "fused long negation");
    }

    #[test]
    fn double_negation_counts_two_and_reads_as_mildly_positive() {
        let w = analyzed("그거 안 좋지 않아");
        let good = at(&w, "좋지");
        assert_eq!(negation_counts(&w), vec![1, 2, 2, 1], "{w:?}");
        let r = read_span(&w, good, good + 1);
        assert_eq!((r.negations, r.polarity()), (2, Polarity::DoubleNegated));
        assert_eq!(clauses(&w)[0].negators, 2);
        // 서로 다른 서술어의 부정 둘은 겹치지 않는다
        let w = analyzed("안 가 창 닫지 마");
        assert_eq!(negation_counts(&w), vec![1, 1, 1, 1, 1], "{w:?}");
    }

    #[test]
    fn a_short_negation_before_a_light_verb_covers_the_noun_before_it() {
        let w = analyzed("그거 추천 안 해");
        assert!(w[at(&w, "추천")].negated, "{w:?}");
        assert!(!w[0].negated);
        let w = analyzed("그거 추천 못해");
        assert!(w[at(&w, "추천")].negated && w[at(&w, "못해")].negated, "fused short negation: {w:?}");
        let w = analyzed("추천은 아니야");
        assert_eq!(w[1].role, Role::Negation, "{w:?}");
        assert!(w[0].negated);
        // 발화 첫 "아니" 는 감탄사
        let w = analyzed("아니야 이거 좋아");
        assert!(w.iter().all(|x| !x.negated), "{w:?}");
        // "안경" 은 부정이 아니다
        let w = analyzed("안경 써 봐");
        assert!(w.iter().all(|x| !x.negated));
    }

    #[test]
    fn fc02_adversative_clause_boundary_lets_the_main_clause_win() {
        // "딜은 쎈데 스태미나가 너무 딸려서 비추": -ㄴ데 경계, 앞절은 양보절, "비추" 는 주절
        let w = analyzed("딜은 쎈데 스태미나가 너무 딸려서 비추");
        let cl = clauses(&w);
        assert!(cl[0].concessive && (cl[0].start, cl[0].end) == (0, 2), "{cl:?}");
        assert!(cl[1..].iter().all(|c| !c.concessive), "{cl:?}");
        let strong = at(&w, "쎈데");
        let bad = at(&w, "비추");
        assert!(!read_span(&w, strong, strong + 1).main);
        let r = read_span(&w, bad, bad + 1);
        assert_eq!((r.main, r.negations), (true, 0), "{r:?}");

        // 접속 부사: 앞이 양보절, 접속 부사부터 주절
        let w = analyzed("딜은 좋아 근데 너무 느려");
        let cl = clauses(&w);
        assert_eq!(cl.len(), 2, "{cl:?}");
        assert!(cl[0].concessive && !cl[1].concessive && cl[1].start == at(&w, "근데"));
        assert_eq!(w[at(&w, "근데")].role, Role::Function);
        // 영어: but 뒤가 주절, though 뒤가 양보절
        let w = analyzed("the build is strong but stamina drains fast");
        let cl = clauses(&w);
        assert!(cl[0].concessive && !cl[1].concessive, "{cl:?}");
        let w = analyzed("i recommend it though the boss is hard");
        let cl = clauses(&w);
        assert!(!cl[0].concessive && cl[1].concessive, "{cl:?}");
    }

    #[test]
    fn sentence_final_adversative_endings_and_look_alikes_are_not_boundaries() {
        // 뒤에 내용어가 없는 감탄 종결 "-던데" 는 주절 그대로
        let w = analyzed("이 길 좋던데");
        assert_eq!(clauses(&w).len(), 1);
        assert!(read_span(&w, 2, 3).main);
        // 명사 "가운데", 부사 "적어도", 조사 "도" 는 경계가 아니다
        for text in ["화면 가운데 버튼 눌러", "적어도 20% 줄었어", "말레니아도 어려워"] {
            assert_eq!(clauses(&analyzed(text)).len(), 1, "{text}");
        }
        // 양보 "-어도": 받침 + 어도, 줄어든 "해도"
        for text in ["좋아도 비추", "아무리 해도 안 깨져", "어려워도 추천"] {
            let w = analyzed(text);
            assert!(clauses(&w)[0].concessive, "{text}: {:?}", clauses(&w));
        }
        // "-고 싶다" 는 절을 끊지 않는다
        assert_eq!(clauses(&analyzed("나 이 길로 가고 싶어")).len(), 1);
        assert_eq!(clauses(&analyzed("문 열고 들어가")).len(), 2);
    }

    #[test]
    fn negation_scope_stops_at_an_adversative_boundary() {
        // "쎈데" 는 -ㄴ데 경계: 뒤 절의 긴 부정이 앞 절을 덮지 않는다
        let w = analyzed("딜은 쎈데 별로지 않아");
        assert!(!w[0].negated && !w[1].negated, "{w:?}");
        assert!(w[2].negated && w[3].negated);
        let w = analyzed("좋아 근데 추천은 아니야");
        assert!(!w[0].negated, "{w:?}");
        assert!(w[at(&w, "추천은")].negated);
    }

    #[test]
    fn empty_and_out_of_range_spans_are_safe() {
        assert!(clauses(&[]).is_empty());
        assert_eq!(read_span(&[], 0, 0), SpanReading { clause: 0, main: true, negations: 0 });
        let w = analyzed("보스 패턴");
        assert_eq!(read_span(&w, 5, 9).clause, 0);
        assert_eq!(Polarity::from_count(3), Polarity::Negated);
    }
}
