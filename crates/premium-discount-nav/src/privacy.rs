//! 이름 · 위치 단서의 개인정보 거르기 (영역 지도를 서버에 올리기 전에)
//!
//! 사람 이름처럼 모양만으로 알 수 없는 개인정보는 거르지 못한다. 그래서 지도는 동의한 경우에만 올리고,
//! 서버는 영역 이름을 평가와 같은 검수(욕설 · 개인정보 검사)에 한 번 더 넣는다.

/// 지도에 넣는 이름의 최대 글자 수
pub const NAME_MAX_CHARS: usize = 40;

/// 숫자가 이만큼 이상 든 낱말은 개인정보로 본다 (전화 · 카드 · 계좌 · 주민등록번호 조각)
const PRIVATE_DIGITS: usize = 4;

/// 이름에서 개인정보로 보이는 낱말을 뺀다: 이메일, 숫자가 네 개 이상 든 낱말. 남은 낱말을 공백 하나로 잇고
/// [`NAME_MAX_CHARS`] 글자에서 자른다. 연도 · 가격처럼 숫자가 긴 낱말도 함께 빠진다 (거르는 쪽으로 기운다).
pub fn scrub_name(name: &str) -> String {
    let kept: Vec<&str> = name.split_whitespace().filter(|w| !looks_private(w)).collect();
    let joined = kept.join(" ");
    joined.chars().take(NAME_MAX_CHARS).collect::<String>().trim_end().to_string()
}

fn looks_private(word: &str) -> bool {
    let email = word.split_once('@').is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.'));
    email || word.chars().filter(|c| c.is_ascii_digit()).count() >= PRIVATE_DIGITS
}

/// 위치 단서에서 네 자리 이상 이어진 숫자를 `#` 로 바꾼다 (`#user-123456` → `#user-#`).
/// 화면마다 바뀌는 id 를 지워 같은 영역이 같은 단서를 갖게 하는 몫도 있다.
pub fn scrub_anchor(anchor: &str) -> String {
    let mut out = String::with_capacity(anchor.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= PRIVATE_DIGITS {
            out.push('#');
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in anchor.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_words_are_dropped() {
        assert_eq!(scrub_name("홍길동 hong@example.com 님에게 보내기"), "홍길동 님에게 보내기");
        assert_eq!(scrub_name("010-1234-5678 로 전화"), "로 전화");
        assert_eq!(scrub_name("카드 1234 5678 9012 3456"), "카드");
        assert_eq!(scrub_name("  검색   입력창 "), "검색 입력창");
        assert_eq!(scrub_name("2쪽 다음"), "2쪽 다음");
        assert_eq!(scrub_name("@멘션"), "@멘션");
        let long = "가".repeat(60);
        assert_eq!(scrub_name(&long).chars().count(), NAME_MAX_CHARS);
    }

    #[test]
    fn long_digit_runs_in_anchors_become_hashes() {
        assert_eq!(scrub_anchor("main>form#user-123456>input:nth(2)"), "main>form#user-#>input:nth(2)");
        assert_eq!(scrub_anchor("TextBox_20261116_99"), "TextBox_#_99");
        assert_eq!(scrub_anchor("cell:12,7"), "cell:12,7");
    }
}
