//! 비밀 지우기: 시드 구문 · 개인 키 · 화면에서 읽은 민감한 글을 메모리에서 0 으로 덮는다.
//!
//! 지갑이 쓰는 `zeroize` crate 를 그대로 다시 내보낸다. 컴파일러가 "곧 버릴 값" 이라며 덮어쓰기를 빼 버리지 못하게
//! 휘발성 쓰기를 쓰므로, 손으로 `write_volatile` 을 짜는 대신 이것을 쓴다. 이 crate 를 링크하는 쪽(내비게이션의 라벨
//! 정리, raon-zena 의 채팅 PII 세탁기)이 같은 방법으로 지운다.

pub use zeroize::{Zeroize, Zeroizing};

use crate::bip39::seed_phrase_spans;

/// 문자열을 0 으로 덮고 비운다 (빈 문자열이 되고, 잡아 둔 메모리도 0 이 된다)
pub fn wipe_string(s: &mut String) {
    s.zeroize();
}

/// 바이트를 0 으로 덮는다 (길이는 그대로)
pub fn wipe_bytes(bytes: &mut [u8]) {
    bytes.zeroize();
}

/// `text` 의 바이트 범위 `span` 을 `mask` 로 바꾼다. 원래 버퍼는 0 으로 덮고 버린다 (새 버퍼에 옮겨 쓴다).
/// 범위가 글 밖이거나 글자 경계가 아니면 글 전체를 지운다: 지울 곳을 잘못 짚었을 때 비밀을 남기지 않는 쪽으로
pub fn redact_span(text: &mut String, span: (usize, usize), mask: &str) {
    redact_spans(text, &[span], mask);
}

/// 글 속 시드 구문([`crate::detect_seed_phrase_span`])을 모두 `mask` 로 바꾸고 바꾼 구간 수를 돌려준다.
/// 바꿀 것이 있으면 원래 버퍼는 0 으로 덮는다
pub fn redact_seed_phrases(text: &mut String, mask: &str) -> usize {
    let spans = seed_phrase_spans(text);
    if !spans.is_empty() {
        redact_spans(text, &spans, mask);
    }
    spans.len()
}

/// 앞에서부터 겹치지 않는 범위들을 `mask` 로 바꾼다. 범위가 하나라도 잘못이면 글 전체를 지운다
fn redact_spans(text: &mut String, spans: &[(usize, usize)], mask: &str) {
    let ok = spans.windows(2).all(|w| w[0].1 <= w[1].0)
        && spans
            .iter()
            .all(|&(s, e)| s <= e && e <= text.len() && text.is_char_boundary(s) && text.is_char_boundary(e));
    if !ok {
        text.zeroize();
        return;
    }
    let mut out = String::with_capacity(text.len() + mask.len() * spans.len());
    let mut at = 0;
    for &(s, e) in spans {
        out.push_str(&text[at..s]);
        out.push_str(mask);
        at = e;
    }
    out.push_str(&text[at..]);
    text.zeroize();
    *text = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";

    #[test]
    fn wiping_zeroes_the_whole_allocation() {
        let mut s = String::from(SEED);
        let (ptr, cap) = (s.as_ptr(), s.capacity());
        wipe_string(&mut s);
        assert!(s.is_empty());
        assert_eq!(s.capacity(), cap, "잡아 둔 메모리는 그대로 (다시 쓸 수 있다)");
        // SAFETY: 할당은 아직 s 가 갖고 있고(용량 그대로), zeroize 가 cap 바이트를 모두 썼다
        let bytes = unsafe { std::slice::from_raw_parts(ptr, cap) };
        assert!(bytes.iter().all(|&b| b == 0));
        let mut b = [1u8, 2, 3];
        wipe_bytes(&mut b);
        assert_eq!(b, [0; 3]);
    }

    #[test]
    fn spans_are_replaced_and_a_bad_span_wipes_everything() {
        let mut s = String::from("카드 1234 끝");
        redact_span(&mut s, (7, 11), "####");
        assert_eq!(s, "카드 #### 끝");
        let mut bad = String::from("시드 구문");
        redact_span(&mut bad, (1, 2), "x");
        assert!(bad.is_empty(), "글자 경계가 아니면 다 지운다");
        let mut out = String::from("abc");
        redact_span(&mut out, (2, 9), "x");
        assert!(out.is_empty(), "글 밖이면 다 지운다");
    }

    #[test]
    fn ps12_seed_phrase_in_chat_is_wiped_and_a_public_address_passes() {
        // 채팅 입력에 BIP-39 12단어: 잡아서 원본 버퍼를 통째로 0 으로 지운다 (LLM · 대화 · 보관함 · 사전으로 넘길 글이 없다)
        let mut chat = format!("이거 내 지갑 복구 구문이야 {SEED}");
        assert!(crate::detect_seed_phrase_span(&chat).is_some());
        let (ptr, cap) = (chat.as_ptr(), chat.capacity());
        wipe_string(&mut chat);
        assert!(chat.is_empty());
        // SAFETY: 할당은 아직 chat 이 갖고 있고(용량 그대로), zeroize 가 cap 바이트를 모두 썼다
        assert!(unsafe { std::slice::from_raw_parts(ptr, cap) }.iter().all(|&b| b == 0));
        // 0x + 40자 공개 주소(premium.discount 계정 주소)와 64자 16진수는 시드 구문이 아니다: 그대로 지나간다
        let address = crate::Wallet::from_phrase(SEED).unwrap().address().to_string();
        let mut message = format!("내 주소는 {address} 이고 해시는 0x{} 야", "ab".repeat(32));
        assert_eq!(crate::detect_seed_phrase_span(&message), None);
        let before = message.clone();
        assert_eq!(redact_seed_phrases(&mut message, "[REDACTED_SEED]"), 0);
        assert_eq!(message, before);
        // 시드 구문만 가려 넘기는 길: 나머지 글은 그대로
        let mut mixed = format!("{address} 로 보내 줘. 복구 구문은 {SEED}");
        assert_eq!(redact_seed_phrases(&mut mixed, "[REDACTED_SEED]"), 1);
        assert_eq!(mixed, format!("{address} 로 보내 줘. 복구 구문은 [REDACTED_SEED]"));
    }

    #[test]
    fn seed_phrases_in_chat_are_masked() {
        let mut chat = format!("이거 내 지갑이야 {SEED} 맞지? 다시 {SEED}");
        assert_eq!(redact_seed_phrases(&mut chat, "[REDACTED_SEED]"), 2);
        assert_eq!(chat, "이거 내 지갑이야 [REDACTED_SEED] 맞지? 다시 [REDACTED_SEED]");
        let mut clean = String::from("오늘 보스전 어땠어? Defeat the boss to unlock the next level");
        assert_eq!(redact_seed_phrases(&mut clean, "[REDACTED_SEED]"), 0);
        assert_eq!(clean, "오늘 보스전 어땠어? Defeat the boss to unlock the next level");
    }
}
