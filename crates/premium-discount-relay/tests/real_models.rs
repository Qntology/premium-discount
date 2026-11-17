//! 실제 모델(Stanza · granite)로 하는 Relay 시험. 모델이 없으면 건너뛴다.
//! `cargo test -p premium-discount-relay --features neural --test real_models -- --nocapture`
#![cfg(feature = "neural")]

use premium_discount_nms::embed::{cosine, GraniteEmbedder, TextEmbedder};
use premium_discount_relay::*;

fn analyze(lang: &str, text: &str) -> Option<Vec<Sentence>> {
    let p = premium_discount_nlp::stanza::pipeline(lang).ok()?;
    let mut g = p.lock().ok()?;
    let out = g.analyze(text).ok()?;
    Some(out.iter().map(Sentence::from_stanza).collect())
}

fn setup() -> Option<GraniteEmbedder> {
    if premium_discount_nlp::models::granite_dir().is_none() || premium_discount_nlp::models::stanza_dir("ko").is_none() {
        eprintln!("skipped: granite or Stanza ko not installed");
        return None;
    }
    match GraniteEmbedder::load_default() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipped: granite did not load ({e:#})");
            None
        }
    }
}

#[test]
fn korean_patch_note_with_a_changed_value_supersedes() {
    let Some(g) = setup() else { return };
    let cfg = RelayConfig::default();
    let Some(old) = analyze("ko", "말레니아의 물새 난무 피해가 20% 감소했다.") else {
        eprintln!("skipped: Stanza ko did not run (ONNX Runtime?)");
        return;
    };
    let new = analyze("ko", "말레니아의 물새 난무 피해가 15% 감소했다.").unwrap();
    for s in old.iter().chain(&new) {
        let toks: Vec<String> = s.tokens.iter().map(|t| format!("{}/{}/{}/{}", t.text, t.lemma, t.upos, t.deprel)).collect();
        eprintln!("{} :: {}", s.text, toks.join(" "));
        eprintln!("  keys {:?}  claim {:?}", relay_keys(s), claim(s));
    }
    let old = Record { sentences: old, at_ms: 1 };
    let new = Record { sentences: new, at_ms: 2 };
    let all: Vec<RelayKey> = old.sentences.iter().chain(&new.sentences).flat_map(relay_keys).collect();
    let keys = cluster_keys(&all, &g, &cfg);
    let (rel, cmp) = compare(&old, &new, &keys, &g, &cfg);
    eprintln!("relation {rel:?}\n  {cmp:?}");
    assert!(matches!(rel, Relation::Supersedes { .. }), "{rel:?}");
}

#[test]
fn cross_language_names_measured() {
    let Some(g) = setup() else { return };
    // 묶기의 바닥값을 정한 실측: 같은 이름의 한 · 영 표기와, 닮았지만 다른 이름
    for (a, b) in [("말레니아", "malenia"), ("라다곤", "radagon"), ("말레니아", "라다곤"), ("malenia", "radagon"), ("엘든 링", "elden ring")] {
        let (va, vb) = (g.embed(a), g.embed(b));
        eprintln!("cos({a}, {b}) = {:.4}", cosine(&va, &vb));
    }
    let cfg = RelayConfig::default();
    let k = |key: &str| RelayKey { key: key.into(), kind: KeyKind::Proper };
    let c = cluster_keys(&[k("말레니아"), k("malenia"), k("radagon"), k("라다곤")], &g, &cfg);
    eprintln!("threshold {:.4} canonical {:?}", c.threshold, c.canonical);
    assert_ne!(c.canon("malenia"), c.canon("radagon"), "different names never merge");
    assert_eq!(c.canon("말레니아"), "malenia", "the Korean and English names of one boss share a key");
    assert_eq!(c.canon("라다곤"), "radagon");
}
