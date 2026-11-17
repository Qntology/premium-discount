use std::collections::HashSet;
use std::sync::Arc;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    Profane,
}

/// 욕설 필터. 로컬에서 먼저 걸러, 막힌 입력은 임베딩도 로컬 저장도 서버 전송도 하지 않는다.
/// 기본 구현은 `LocalFilter`이고, 모델 기반 필터(예: convaiinnovations/laya-multilingual)도 이 trait만 구현하면 바꿔 끼울 수 있다.
pub trait ProfanityFilter: Send + Sync {
    fn check(&self, text: &str) -> Result<Verdict, AppError>;
}

impl<T: ProfanityFilter + ?Sized> ProfanityFilter for Arc<T> {
    fn check(&self, text: &str) -> Result<Verdict, AppError> {
        (**self).check(text)
    }
}

/// 실행 중에 바꿔 끼울 수 있는 필터. 셸은 사전 필터로 시작하고, 모델 필터(laya)를 받거나 찾으면 바꾼다.
/// 검사 중인 요청은 바꾸기 전 필터로 끝난다 (Arc를 쥐고 검사한다).
pub struct SwitchFilter {
    inner: std::sync::RwLock<(Arc<dyn ProfanityFilter>, &'static str)>,
}

impl Default for SwitchFilter {
    fn default() -> Self {
        Self::dictionary()
    }
}

impl SwitchFilter {
    /// 사전 필터(LocalFilter)로 시작한다
    pub fn dictionary() -> Self {
        Self { inner: std::sync::RwLock::new((Arc::new(LocalFilter::default()), "dictionary")) }
    }

    pub fn set(&self, filter: Arc<dyn ProfanityFilter>, name: &'static str) {
        *self.inner.write().expect("filter lock") = (filter, name);
    }

    /// 지금 쓰는 필터 이름 ("dictionary" | "laya")
    pub fn name(&self) -> &'static str {
        self.inner.read().expect("filter lock").1
    }
}

impl ProfanityFilter for SwitchFilter {
    fn check(&self, text: &str) -> Result<Verdict, AppError> {
        let filter = self.inner.read().expect("filter lock").0.clone();
        filter.check(text)
    }
}

/// 설정 `filter`: 어떤 욕설 필터를 쓸지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterSetting {
    /// laya 모델이 설치되어 있으면 laya, 없으면 사전 (기본값)
    #[default]
    Auto,
    Dictionary,
    Laya,
}

impl FilterSetting {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "dictionary" | "dict" => Ok(Self::Dictionary),
            "laya" | "model" => Ok(Self::Laya),
            other => Err(format!("filter 값 '{other}'를 알 수 없습니다 (auto, dictionary, laya)")),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// LocalFilter: 사전 기반 로컬 필터 (모델 파일도 네트워크도 필요 없다)
//
// 이 필터는 "명백한 욕설"만 막는 1차 관문이다. 정밀도를 우선한다. 애매한 표현을 놓치는 것은 괜찮다:
// 서버(cron)가 결정 모델(decision-model-preview)로 욕설·음란·불법을 다시 판정하고, 거기서 걸리면 거부된다.
// 반대로 멀쩡한 글을 막으면 사용자가 바로 잃으므로 사전을 보수적으로 두고, 부분 문자열 일치는 어간에만 쓴다.
//
// 처리 순서
//   1. 정규화: 전각 → 반각, 영문 소문자. 한글은 그대로 둔다.
//   2. 한국어: 공백으로 나눈 낱말마다 한글 글자(음절·자모)만 남기고 어간을 찾는다. "시.발", "시1발", "병-신"처럼
//      낱말 안에 기호·숫자를 끼운 우회를 잡는다. 공백은 낱말 경계로 존중한다 ("당시 발견"이 "시발"로 걸리면 안 된다).
//      다만 한 글자씩 띄어 쓴 열("시 발", "ㅅ ㅂ")은 이어 붙여 본다. 어간을 품은 정상 낱말("시발점")은 먼저 지운다.
//   3. 영어: 숫자·기호 치환(0→o, 1→i, 3→e, 4→a, 5→s, 7→t, @→a, $→s, !→i)을 거친 낱말 단위로 비교한다.
//      낱말 전체가 사전에 있거나, 어간을 포함하면 욕설이다. "cunt"처럼 다른 낱말 안에 우연히 들어가는 것
//      ("Scunthorpe")은 어간이 아니라 낱말 전체 일치로만 본다. 한 글자씩 이어진 열("f u c k", "f.u.c.k")은 이어 붙여 본다.
// ---------------------------------------------------------------------------------------------

/// 한국어 어간: 낱말(한글 글자만 남긴 것)에서 부분 문자열로 찾는다
const KO_STEMS: &[&str] = &[
    "시발", "씨발", "씨팔", "시팔", "쉬발", "ㅅㅂ", "ㅆㅂ", "ㅅㅃ", "병신", "ㅂㅅ", "븅신", "개새끼", "개세끼", "좆", "존나", "지랄", "ㅈㄹ", "염병",
    "엿먹", "미친놈", "미친년", "썅", "쌍놈", "쌍년", "걸레년", "느금마", "니애미", "니애비", "개년", "씹새", "씹할", "씹년", "씹놈",
];

/// 어간을 품고 있지만 정상인 낱말. 어간 검사 전에 지운다.
const KO_ALLOWED: &[&str] = &["시발점", "시발역", "시발택시", "시발자동차", "병신년"];

/// 영어: 낱말 전체가 일치해야 하는 낱말
const EN_WORDS: &[&str] = &[
    "fuk", "fck", "slut", "whore", "bitch", "bastard", "retard", "faggot", "nigger", "nigga", "cunt", "asshole", "jackass", "dumbass", "dickhead",
    "bullshit",
];

/// 영어: 낱말 안에 들어 있어도 욕설로 보는 어간. 다른 낱말 안에 우연히 들어가는 것은 넣지 않는다.
const EN_STEMS: &[&str] = &["fuck", "shit", "bitch", "whore", "nigger", "faggot", "bastard", "asshole", "motherf", "dickhead", "cocksuck", "bullshit"];

/// 영어 어간을 품고 있지만 정상인 낱말 (어간 검사에서 제외)
const EN_ALLOWED: &[&str] = &["shitake", "shiitake"];

/// 사전 기반 로컬 욕설 필터. `LocalFilter::default()`가 기본 사전이고, `with_words`로 낱말을 더할 수 있다.
#[derive(Debug, Clone)]
pub struct LocalFilter {
    ko_stems: Vec<String>,
    ko_allowed: Vec<String>,
    en_words: HashSet<String>,
    en_stems: Vec<String>,
    en_allowed: HashSet<String>,
}

impl Default for LocalFilter {
    fn default() -> Self {
        Self {
            ko_stems: KO_STEMS.iter().map(|s| s.to_string()).collect(),
            ko_allowed: KO_ALLOWED.iter().map(|s| s.to_string()).collect(),
            en_words: EN_WORDS.iter().map(|s| s.to_string()).collect(),
            en_stems: EN_STEMS.iter().map(|s| s.to_string()).collect(),
            en_allowed: EN_ALLOWED.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl LocalFilter {
    /// 기본 사전에 낱말을 더한다. 한글이 있는 낱말은 한국어 어간, 그 밖에는 영어 낱말 전체 일치로 들어간다.
    /// 빈 문자열은 무시한다 (빈 어간은 모든 글을 막는다).
    pub fn with_words<I, S>(mut self, words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for w in words {
            let w = fold(w.as_ref());
            if w.is_empty() {
                continue;
            }
            if w.chars().any(is_hangul) {
                let compact: String = w.chars().filter(|c| is_hangul(*c)).collect();
                if !compact.is_empty() {
                    self.ko_stems.push(compact);
                }
            } else {
                self.en_words.insert(deleet(&w));
            }
        }
        self
    }

    fn has_korean_profanity(&self, folded: &str) -> bool {
        // 공백으로 나눈 낱말마다 한글 글자만 남긴다 (낱말 안에 낀 기호·숫자·영문은 사라진다)
        let mut candidates: Vec<String> = Vec::new();
        let mut run = String::new(); // 한 글자씩 띄어 쓴 열
        for token in folded.split_whitespace() {
            let hangul: String = token.chars().filter(|c| is_hangul(*c)).collect();
            match hangul.chars().count() {
                0 => {}
                1 => run.push_str(&hangul),
                _ => {
                    if !run.is_empty() {
                        candidates.push(std::mem::take(&mut run));
                    }
                    candidates.push(hangul);
                }
            }
        }
        if !run.is_empty() {
            candidates.push(run);
        }

        candidates.into_iter().any(|mut word| {
            for allowed in &self.ko_allowed {
                if word.contains(allowed.as_str()) {
                    word = word.replace(allowed.as_str(), "");
                }
            }
            self.ko_stems.iter().any(|stem| word.contains(stem.as_str()))
        })
    }

    fn has_english_profanity(&self, folded: &str) -> bool {
        // 영숫자와 치환 기호로 이어진 덩어리를 낱말로 본다. 앞뒤에 붙은 기호("fuck!", "@home")는 떼고 치환한다
        let words: Vec<String> = folded
            .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '@' | '$' | '!' | '|')))
            .map(|w| w.trim_matches(|c: char| matches!(c, '@' | '$' | '!' | '|')))
            .filter(|w| !w.is_empty())
            .map(deleet)
            // 치환 뒤에도 남은 숫자는 낱말을 나눈다
            .flat_map(|w| w.split(|c: char| !c.is_ascii_alphabetic()).filter(|p| !p.is_empty()).map(str::to_string).collect::<Vec<_>>())
            .collect();

        for w in &words {
            if self.en_words.contains(w.as_str()) {
                return true;
            }
            if !self.en_allowed.contains(w.as_str()) && self.en_stems.iter().any(|s| w.contains(s.as_str())) {
                return true;
            }
        }
        // "f u c k", "f.u.c.k": 한 글자짜리 낱말이 이어지면 이어 붙여서 다시 본다
        let mut run = String::new();
        for w in words.iter().map(String::as_str).chain(std::iter::once("")) {
            if w.len() == 1 {
                run.push_str(w);
                continue;
            }
            if run.len() >= 3 && (self.en_words.contains(run.as_str()) || self.en_stems.iter().any(|s| run.contains(s.as_str()))) {
                return true;
            }
            run.clear();
        }
        false
    }
}

impl ProfanityFilter for LocalFilter {
    fn check(&self, text: &str) -> Result<Verdict, AppError> {
        let folded = fold(text);
        if self.has_korean_profanity(&folded) || self.has_english_profanity(&folded) {
            Ok(Verdict::Profane)
        } else {
            Ok(Verdict::Clean)
        }
    }
}

/// 정규화: 전각 영숫자·기호를 반각으로, 영문은 소문자로. 한글은 그대로 둔다.
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            // 전각 ASCII (！ ~ ～) → 반각
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 숫자·기호를 비슷하게 생긴 글자로 되돌린다 (영어 우회 표기)
fn deleet(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '!' | '|' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            _ => c,
        })
        .collect()
}

/// 한글 음절(가-힣)과 호환 자모(ㄱ-ㅎ, ㅏ-ㅣ)
fn is_hangul(c: char) -> bool {
    matches!(c, '\u{AC00}'..='\u{D7A3}' | '\u{3131}'..='\u{318E}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profane(text: &str) -> bool {
        LocalFilter::default().check(text).unwrap() == Verdict::Profane
    }

    #[test]
    fn plain_korean_and_english_are_clean() {
        for t in [
            "좋은 평가입니다",
            "배송이 빨라서 만족합니다. 다시 구매할게요.",
            "The product quality is great and shipping was fast",
            "가격 대비 성능이 훌륭하다",
            "",
            "   ",
        ] {
            assert!(!profane(t), "clean text was blocked: {t:?}");
        }
    }

    #[test]
    fn korean_profanity_is_blocked() {
        for t in ["이 시발 뭐야", "병신 같은 서비스", "개새끼들", "지랄하네", "ㅅㅂ 진짜", "존나 별로"] {
            assert!(profane(t), "profanity was not blocked: {t:?}");
        }
    }

    #[test]
    fn korean_obfuscation_is_blocked() {
        // 점·공백·숫자·영문을 끼워 넣어도 한글 글자만 이어 붙여 본다
        for t in ["시.발", "시 발", "시1발", "씨  발 놈", "병-신", "개 새 끼", "ㅅ ㅂ", "시~~발"] {
            assert!(profane(t), "obfuscated profanity was not blocked: {t:?}");
        }
    }

    #[test]
    fn korean_words_that_contain_a_stem_are_allowed() {
        for t in [
            "서울역이 이 노선의 시발점입니다",
            "시발역에서 출발합니다",
            "그 영화는 보지 마세요",
            "오늘은 일찍 자지 않았어요",
            "병신년 새해 복 많이 받으세요",
            "곧 위기가 닥쳐올 것이다",
            "당시 발견된 유물입니다",
            "이 시 발견은 중요하다",
            "호랑이 새끼야 귀엽다",
        ] {
            assert!(!profane(t), "allowed word was blocked: {t:?}");
        }
    }

    #[test]
    fn english_profanity_is_blocked_with_common_evasions() {
        for t in [
            "this is fucking bad",
            "what a piece of shit",
            "Fuck you",
            "FUCK",
            "sh1t service",
            "f u c k this",
            "f.u.c.k",
            "b!tch",
            "you are an a$$hole",
            "ＦＵＣＫ",
            "bullshit",
            "fuck!",
            "SHIT.",
        ] {
            assert!(profane(t), "profanity was not blocked: {t:?}");
        }
    }

    #[test]
    fn english_words_that_contain_short_stems_are_allowed() {
        for t in [
            "classic class assessment of the assistant",
            "Scunthorpe united",
            "a passenger saw a bassoon",
            "cockpit and peacock",
            "dickens wrote novels",
            "shiitake mushrooms and shitake",
            "I will pass the glass to the grass",
            "title and hello",
        ] {
            assert!(!profane(t), "allowed text was blocked: {t:?}");
        }
    }

    #[test]
    fn single_letters_that_do_not_spell_a_word_are_allowed() {
        assert!(!profane("a b c d e f g"));
        assert!(!profane("I a m h e r e"));
    }

    #[test]
    fn custom_words_extend_the_dictionary() {
        let f = LocalFilter::default().with_words(["badword", "나쁜말", "  ", ""]);
        assert_eq!(f.check("this has badword in it").unwrap(), Verdict::Profane);
        assert_eq!(f.check("이건 나 쁜 말 이다").unwrap(), Verdict::Profane);
        assert_eq!(f.check("badwords are fine").unwrap(), Verdict::Clean, "낱말 전체 일치");
        assert_eq!(f.check("totally fine text").unwrap(), Verdict::Clean, "빈 낱말이 모든 글을 막지 않는다");
        assert_eq!(f.check("좋은 글").unwrap(), Verdict::Clean);
    }

    #[test]
    fn filter_is_usable_through_arc_and_trait_object() {
        let f: Arc<dyn ProfanityFilter> = Arc::new(LocalFilter::default());
        assert_eq!(f.check("시발").unwrap(), Verdict::Profane);
        assert_eq!(f.check("안녕하세요").unwrap(), Verdict::Clean);
    }

    #[test]
    fn a_switch_filter_starts_with_the_dictionary_and_can_be_replaced_at_run_time() {
        struct Everything;
        impl ProfanityFilter for Everything {
            fn check(&self, _: &str) -> Result<Verdict, AppError> {
                Ok(Verdict::Profane)
            }
        }
        let f = SwitchFilter::dictionary();
        assert_eq!(f.name(), "dictionary");
        assert_eq!(f.check("배송이 빨라요").unwrap(), Verdict::Clean);
        f.set(Arc::new(Everything), "laya");
        assert_eq!(f.name(), "laya");
        assert_eq!(f.check("배송이 빨라요").unwrap(), Verdict::Profane);
        assert_eq!(FilterSetting::parse("").unwrap(), FilterSetting::Auto);
        assert_eq!(FilterSetting::parse("LAYA").unwrap(), FilterSetting::Laya);
        assert!(FilterSetting::parse("strict").is_err());
    }
}
