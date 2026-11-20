//! 신구 대체 → PD-Verdict 회귀 시험 (모델 없이). 낱말은 실제 Stanza ko 출력(`tests/real_models.rs` 의 같은 문장)을 옮긴 것이다.
//! - 수치 변경 "피해 20% → 15%", 유료 → 무료 "배송비 3,000원 → 무료" 가 옛 평가를 대체한다
//! - 대체는 같은 장소 · 대상(cc · to) 안에서만, 늦은 평가가 이른 평가를
//! - 대체된 평가는 판정 입력 `superseded` 로 근거가 없어도 곧바로 discount (grade −2, 이유 `superseded`)

use premium_discount_nms::embed::HashedNgramEmbedder;
use premium_discount_relay::*;
use premium_discount_verdict::{verdict, Baselines, Case, Obs, Params, Prev, Tier};

fn tok(text: &str, lemma: &str, upos: &str, deprel: &str, head: usize) -> Token {
    Token { text: text.into(), lemma: lemma.into(), upos: upos.into(), deprel: deprel.into(), head }
}

/// "말레니아의 물새 난무 피해가 {value} 감소했다 ."
fn damage(value: &str) -> Record {
    let text = format!("말레니아의 물새 난무 피해가 {value} 감소했다 .");
    Record {
        sentences: vec![Sentence {
            text,
            tokens: vec![
                tok("말레니아의", "말레니아+의", "PROPN", "nmod", 4),
                tok("물새", "물새", "NOUN", "compound", 3),
                tok("난무", "난무", "NOUN", "compound", 4),
                tok("피해가", "피해+가", "NOUN", "nsubj", 6),
                tok(value, value, "NUM", "nummod", 6),
                tok("감소했다", "감소+하+었+다", "VERB", "root", 0),
                tok(".", ".", "PUNCT", "punct", 6),
            ],
        }],
        at_ms: 0,
    }
}

/// "이 가게 배송비는 {predicate} ." (predicate = 3,000원이다 · 무료이다)
fn shipping(predicate: &str, lemma: &str) -> Record {
    Record {
        sentences: vec![Sentence {
            text: format!("이 가게 배송비는 {predicate} ."),
            tokens: vec![
                tok("이", "이", "DET", "det", 3),
                tok("가게", "가게", "NOUN", "compound", 3),
                tok("배송비는", "배송비+는", "NOUN", "dislocated", 4),
                tok(predicate, lemma, "VERB", "root", 0),
                tok(".", ".", "PUNCT", "punct", 4),
            ],
        }],
        at_ms: 0,
    }
}

fn at(mut r: Record, ms: i64) -> Record {
    r.at_ms = ms;
    r
}

#[test]
fn value_changes_supersede_the_older_evaluation_in_the_same_place() {
    let e = HashedNgramEmbedder::default();
    // 해시 n-gram 임베더는 술어 하나만 다른 짧은 문장끼리의 코사인이 granite 보다 낮아(배송비 두 문장: 해시 0.633, granite 0.955)
    // 문장 맞추기 바닥을 낮춘다. 실제 모델의 같은 사례는 기본 설정으로 tests/real_models.rs 가 본다.
    let cfg = RelayConfig { align_cos_floor: 0.5, ..RelayConfig::default() };
    let d20 = at(damage("20%"), 1_000);
    let d15 = at(damage("15%"), 2_000);
    let paid = at(shipping("3,000원이다", "3,000+원+이+다"), 1_000);
    let free = at(shipping("무료이다", "무료+이+다"), 3_000);
    let other_place = at(damage("10%"), 5_000);
    let entries = [
        Entry { id: "d15", cc: "game", to: Some("malenia"), record: &d15 },
        Entry { id: "d20", cc: "game", to: Some("malenia"), record: &d20 },
        Entry { id: "paid", cc: "shop", to: None, record: &paid },
        Entry { id: "free", cc: "shop", to: None, record: &free },
        // 다른 장소의 같은 문장은 견주지 않는다
        Entry { id: "d10", cc: "other-game", to: Some("malenia"), record: &other_place },
    ];
    let s = supersessions(&entries, &e, &cfg);
    let pairs: Vec<(&str, &str)> = s.iter().map(|x| (x.old_id.as_str(), x.new_id.as_str())).collect();
    assert_eq!(pairs, vec![("d20", "d15"), ("paid", "free")], "{s:?}");
    assert_eq!((s[0].changes[0].old_values.clone(), s[0].changes[0].new_values.clone()), (vec!["20%".to_string()], vec!["15%".to_string()]));
    assert_eq!((s[1].changes[0].old_values.clone(), s[1].changes[0].new_values.clone()), (vec!["3,000원".to_string()], vec!["무료".to_string()]));
    assert_eq!(s[1].changes[0].core, vec!["배송비".to_string()]);

    // 늦은 쪽이 옛 값이면(시각이 뒤집히면) 대체가 아니다
    let reversed = [
        Entry { id: "d20", cc: "game", to: None, record: &at(damage("20%"), 9_000) },
        Entry { id: "d15", cc: "game", to: None, record: &at(damage("15%"), 9_000) },
    ];
    assert!(supersessions(&reversed, &e, &cfg).is_empty(), "same time: neither supersedes");
}

#[test]
fn a_superseded_evaluation_is_discounted_at_once() {
    let params = Params::v1();
    let case = |superseded: bool, obs: Vec<Obs>| Case {
        now_ms: 10_000,
        track: "pd".into(),
        prev: Prev { tier: Tier::Unrated, since_ms: 0 },
        obs,
        baseline: Baselines::default(),
        duel: None,
        superseded,
    };
    // 표가 하나도 없어도
    let v = verdict(&case(true, vec![]), &params).unwrap();
    assert_eq!((v.tier, v.grade), (Tier::Discount, Some(-2)));
    assert_eq!(v.reasons.first().map(String::as_str), Some("superseded"));
    // 추천이 많아도 대체되면 discount
    let ups: Vec<Obs> = (0..20).map(|i| Obs { kind: "vote".into(), sign: 1, weight: 1.0, credit: 1.0, at_ms: i }).collect();
    let v = verdict(&case(true, ups.clone()), &params).unwrap();
    assert_eq!((v.tier, v.grade), (Tier::Discount, Some(-2)));
    // 대체되지 않은 같은 평가는 discount 가 아니다
    let v = verdict(&case(false, ups), &params).unwrap();
    assert_ne!(v.tier, Tier::Discount);
    assert!(!v.reasons.iter().any(|r| r == "superseded"));
}
