//! 로컬 격리 장부 → PD-Verdict 회귀 시험 (모델 없이, 시나리오 3 Part 3 · CR-09).
//! - 남의 옛 글이 남의 새 글에 대체되면 로컬에서만 격리한다: 로컬 판정 입력 `superseded` 로 곧바로 discount(−2), pd_supersede 는 부르지 않는다
//! - 대체한 새 글이 전역 discount(grade ≤ −1, E ≥ 3.0)로 떨어지면 옛 글은 저절로 복권된다
//! - 내 글끼리의 같은 (cc, to) 대체는 둘 다 stored 가 된 뒤에 한 번만 서버에 보낸다

use premium_discount_nms::embed::HashedNgramEmbedder;
use premium_discount_relay::*;
use premium_discount_verdict::{verdict, Baselines, Case, Obs, Params, Prev, Tier, Verdict};

fn tok(text: &str, lemma: &str, upos: &str, deprel: &str, head: usize) -> Token {
    Token { text: text.into(), lemma: lemma.into(), upos: upos.into(), deprel: deprel.into(), head }
}

/// "말레니아의 물새 난무 피해가 {value} 감소했다 ." (실제 Stanza ko 출력을 옮긴 것, tests/supersede_verdict.rs 와 같다)
fn damage(value: &str, at_ms: i64) -> Record {
    Record {
        sentences: vec![Sentence {
            text: format!("말레니아의 물새 난무 피해가 {value} 감소했다 ."),
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
        at_ms,
    }
}

/// 옛 글의 로컬 판정: 격리 중이면 `superseded` 입력이 켜진다. 옛 글에는 추천 표가 몇 개 있다.
fn local_verdict(superseded: bool) -> Verdict {
    let ups: Vec<Obs> = (0..8).map(|i| Obs { kind: "vote".into(), sign: 1, weight: 1.0, credit: 1.0, at_ms: i }).collect();
    let case = Case {
        now_ms: 10_000,
        track: "pd".into(),
        prev: Prev { tier: Tier::Unrated, since_ms: 0 },
        obs: ups,
        baseline: Baselines::default(),
        duel: None,
        superseded,
    };
    verdict(&case, &Params::v1()).unwrap()
}

#[test]
fn cr09_third_party_quarantine_reinstates_itself_and_my_pending_pair_reports_once() {
    let e = HashedNgramEmbedder::default();
    let cfg = RelayConfig::default();
    let cc = "eldenring.exe";
    let (old, new) = (damage("20%", 1_000), damage("15%", 2_000));
    let entries = [
        Entry { id: "their-old", cc, to: Some("cp-7"), record: &old },
        Entry { id: "their-new", cc, to: Some("cp-7"), record: &new },
    ];
    let report = curate_cross_scope(&entries, &e, &cfg);
    let s = report.superseder("their-old").expect("the newer third-party guide supersedes the older one");
    assert_eq!(s.value_diff, Some(("20%".into(), "15%".into())));

    // 남의 글끼리: 같은 (cc, to) 이고 둘 다 stored 여도 서버에 보내지 않는다 (403 방지)
    let facts = SupersedeFacts { signed_in: true, sharing_consent: true, old_stored: true, new_stored: true, ..Default::default() };
    let sync = supersede_sync(s.old_to.as_deref(), s.new_to.as_deref(), &facts);
    assert_eq!(sync, SupersedeSync::LocalOnly(LocalOnlyReason::ThirdParty));
    let mut ledger = QuarantineLedger::new();
    assert!(ledger.record(QuarantineEntry::from_supersession(s, sync.into(), 3_000)));
    assert_eq!(ledger.entries()[0].sync_state, QuarantineSyncState::ThirdPartyLocal);
    assert!(ledger.drain_ready_server_reports(|id| Some(format!("srv-{id}")), |_| Standing::default()).is_empty(), "pd_supersede is not called");

    // 감지 즉시 로컬 격리: 옛 글은 discount(−2, superseded)
    let unknown = |_: &str| Standing::default();
    assert!(ledger.is_quarantined(cc, "their-old", unknown));
    let v = local_verdict(ledger.is_quarantined(cc, "their-old", unknown));
    assert_eq!((v.tier, v.grade), (Tier::Discount, Some(-2)));
    assert_eq!(v.reasons.first().map(String::as_str), Some("superseded"));

    // 근거가 모자란 전역 discount(E 2.9)로는 아직 복권하지 않는다
    let weak = |id: &str| if id == "their-new" { Standing { global_grade: Some(-1), global_evidence: 2.9, ..Default::default() } } else { Standing::default() };
    assert!(ledger.is_quarantined(cc, "their-old", weak));
    // 새 글이 전역 discount(grade −1, E 3.2)로 떨어지면 옛 글은 곧바로 복권된다
    let fell = |id: &str| if id == "their-new" { Standing { global_grade: Some(-1), global_evidence: 3.2, ..Default::default() } } else { Standing::default() };
    assert!(!ledger.is_quarantined(cc, "their-old", fell));
    assert_eq!(ledger.final_version(cc, "their-old", fell), "their-old");
    let v = local_verdict(ledger.is_quarantined(cc, "their-old", fell));
    assert_ne!(v.tier, Tier::Discount);
    assert!(!v.reasons.iter().any(|r| r == "superseded"));

    // 내 글끼리의 같은 (cc, to) 대체: 새 글이 pending 인 동안은 보류, stored 가 된 뒤 한 번만 보낸다
    let (mine_old, mine_new) = (damage("20%", 4_000), damage("15%", 5_000));
    let entries = [
        Entry { id: "my-old", cc, to: Some("cp-9"), record: &mine_old },
        Entry { id: "local-new", cc, to: Some("cp-9"), record: &mine_new },
    ];
    let report = curate_cross_scope(&entries, &e, &cfg);
    let s = report.superseder("my-old").expect("my newer guide supersedes my older one");
    assert!(s.same_scope());
    let pending = SupersedeFacts { old_mine: true, new_mine: true, signed_in: true, sharing_consent: true, old_stored: true, new_stored: false };
    let sync = supersede_sync(s.old_to.as_deref(), s.new_to.as_deref(), &pending);
    assert_eq!(sync, SupersedeSync::Defer);
    assert!(ledger.record(QuarantineEntry::from_supersession(s, sync.into(), 6_000)));
    let mut stored_new = false;
    let ids = |stored_new: bool| {
        move |id: &str| match id {
            "my-old" => Some("srv-my-old".to_string()),
            "local-new" if stored_new => Some("srv-my-new".to_string()),
            _ => None,
        }
    };
    assert!(ledger.drain_ready_server_reports(ids(stored_new), unknown).is_empty(), "still pending: nothing is sent");
    stored_new = true;
    assert_eq!(ledger.drain_ready_server_reports(ids(stored_new), unknown), vec![("srv-my-old".to_string(), "srv-my-new".to_string())]);
    assert!(ledger.drain_ready_server_reports(ids(stored_new), unknown).is_empty(), "exactly one pd_supersede call");
    // 그 사이에도 내 옛 글은 로컬에서 곧바로 격리되어 있었다
    assert!(ledger.is_quarantined(cc, "my-old", unknown));
}
