//! Utterance normalisation: wake-word removal, punctuation/whitespace cleanup, ASCII lowercase,
//! and per-word lemma candidates.

use crate::korean::{is_hangul, noun_lemma, predicate_stem};

use std::sync::RwLock;

/// 앞에 붙은 호출어(웨이크 워드)로 볼 말. 기본은 없음: 이 crate 는 어느 캐릭터 · 앱의 이름도 모른다. 부르는 앱이
/// [`set_wake_words`] 로 정한다(Raon Zena: "제나야", "라온 제나" …). 먼저 맞는 것 하나만 떼고 `addressed` 를 켠다.
static WAKE_WORDS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// 호출어 목록을 정한다 (소문자로 비교한다). 프로세스 전체에 한 번 정해 두는 값이다.
pub fn set_wake_words<S: AsRef<str>>(words: &[S]) {
    let list: Vec<String> = words.iter().map(|w| w.as_ref().to_lowercase()).filter(|w| !w.is_empty()).collect();
    *WAKE_WORDS.write().unwrap_or_else(|p| p.into_inner()) = list;
}

/// 지금 호출어 목록
pub fn wake_words() -> Vec<String> {
    WAKE_WORDS.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// What a word does in the utterance (set by an [`crate::analyze::Analyzer`]; [`Role::Content`] without one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub enum Role {
    /// Carries meaning: may name a domain.
    #[default]
    Content,
    /// Particles, conjunctions, pronouns, punctuation (Stanza ADP/PART/SCONJ/CCONJ/PRON/PUNCT).
    Function,
    /// An auxiliary or request predicate after the main one: "써 **봐**", "변신해 **줘**".
    Auxiliary,
    /// A negation of the predicate before it: "끄지 **마**", "**안** 가".
    Negation,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Word {
    pub surface: String,
    /// Noun lemma (josa peeled) and predicate stem (request ending stripped).
    pub lemma: String,
    pub stem: String,
    /// The analyzer's lemma / content stem (Stanza), another form for exact matching.
    pub alt: Option<String>,
    /// Universal POS and dependency relation when an analyzer gave them (empty otherwise).
    pub upos: String,
    pub deprel: String,
    /// 구문 머리: 이 낱말이 기대는 낱말의 번호(0부터, 같은 발화 안). 뿌리이거나 구문 분석이 없으면 None.
    /// Stanza 분석기가 채우고([`crate::span::unit_heads`]), 모델 없이 시험할 때는 손으로 채운다.
    pub head: Option<usize>,
    pub role: Role,
    /// Inside the scope of a negation ("앱 끄지 마": 앱, 끄지 and 마).
    pub negated: bool,
}

impl Word {
    /// Whether the word can name a domain.
    pub fn is_content(&self) -> bool {
        self.role == Role::Content
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub text: String,
    pub words: Vec<Word>,
    pub addressed: bool,
}

pub fn normalize(raw: &str) -> Normalized {
    let lower: String = raw.chars().map(|c| if c.is_ascii() { c.to_ascii_lowercase() } else { c }).collect();
    let mut text = lower.trim().to_string();
    let mut addressed = false;
    for w in wake_words() {
        if let Some(rest) = text.strip_prefix(w.as_str()) {
            text = rest.trim_start_matches([',', '!', '~', ' ', '.']).to_string();
            addressed = true;
            break;
        }
    }
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || is_hangul(c) || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    let words: Vec<Word> = cleaned
        .split_whitespace()
        .map(|w| Word {
            surface: w.to_string(),
            lemma: noun_lemma(w),
            stem: predicate_stem(w),
            ..Default::default()
        })
        .collect();
    let text = words.iter().map(|w| w.surface.as_str()).collect::<Vec<_>>().join(" ");
    Normalized { text, words, addressed }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(crate) fn zena() {
        set_wake_words(&["제나야", "제나", "라온 제나", "라온제나", "hey zena", "zena", "raon zena"]);
    }

    #[test]
    fn strips_wake_word_and_punctuation() {
        zena();
        let n = normalize("제나야, 방금 그 보스 패턴 어떻게 피해?");
        assert!(n.addressed);
        assert_eq!(n.text, "방금 그 보스 패턴 어떻게 피해");
        assert_eq!(n.words.len(), 6);
        let n = normalize("Hey Zena, EXIT the app!");
        assert_eq!(n.text, "exit the app");
    }
}

/// 공백을 빼고 소문자로 (정확 일치 비교용: "종료해 줘" = "종료해줘")
pub fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}
