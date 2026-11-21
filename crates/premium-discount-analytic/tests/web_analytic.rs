//! 시나리오 5 계약 사례 (contracts/web_analytic_v1.json 의 WA-01 ~ WA-08 · WA-11 · WA-12 중 이 crate 의 몫): PUG 정규화 ·
//! SPA 모달 가상 단계 · 재전송에도 같은 사건 id · 가시 경쟁 풀과 위치 편향을 고친 선택 대조 · 마찰 거부권 · 단축 경로 퍼널 ·
//! 같은 경과 시간 기간 비교 · 묘비. 수집 스크립트 쪽(첫 묶음 < 8 KB 에 떠남, 아이콘 단추 이름표, 모달 감지)은
//! workers/www/tests/tracker.test.ts 가 같은 사례를 본다.

use std::collections::BTreeMap;

use premium_discount_analytic::blob;
use premium_discount_analytic::event::{clean_href, deterministic_event_id, modal_href, Element, EventKind, VirtualSurface, WebEvent};
use premium_discount_analytic::forget::{cutoff_id, decode_row_event, ingest_decision, IngestDecision, TombstoneGuard};
use premium_discount_analytic::funnel::{
    canonical_of, choice_key, classify_choice, post_pick_windows, trace_session, AbandonPolicy, ChoiceBook, ChoiceOutcome, ChoiceParams, ChoiceTally,
    DebiasConfig, DebiasedChoiceBook, SiteAnalysis, WebElementKind,
};
use premium_discount_analytic::pug;
use premium_discount_analytic::query::{AnalyticDictionary, QueryParser};
use premium_discount_analytic::report::{ConversionComparison, ReportConfidence};
use premium_discount_analytic::time::resolve_comparison_window;
use premium_discount_nav::{hash_id, page_hash, url_template, BehaviorParams, DomNode, MarkType, StepKind};
use premium_discount_verdict::{Params, Tier};
use serde_json::{json, Value};

const PUG_V1: &str = include_str!("../../../contracts/analytic_pug_v1.json");
const BLOB_V1: &str = include_str!("../../../contracts/analytic_blob_v1.json");
/// 시나리오 5 에서 더한 수집 규칙의 사례 (tracker.test.ts 도 같은 파일을 읽는다)
const S5_CASES: &str = include_str!("data/web_analytic_s5_cases.json");

const LIST: &str = "https://shop.example.com/list";
const DETAIL: &str = "https://shop.example.com/goods/view/1234";
const CART: &str = "https://shop.example.com/order/cart";
const CHECKOUT: &str = "https://shop.example.com/order/checkout";
const DONE: &str = "https://shop.example.com/order/complete";
const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
// 2026-11-17 (화) 13:00 KST
const TUESDAY_13: i64 = 1_794_841_200_000 + 13 * HOUR;

/// 결정론적 의사 난수 (xorshift)
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn node(tag: &str, id: Option<&str>, classes: &[&str], attrs: &[(&str, &str)], nth: u32) -> DomNode {
    DomNode {
        tag: tag.into(),
        id: id.map(Into::into),
        classes: classes.iter().map(|c| c.to_string()).collect(),
        attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        nth_of_type: nth,
    }
}

/// 상품 목록의 `slot` 번째 자리에 놓인 상품 카드 (같은 틀, 글만 다름)
fn card_at(product: usize, slot: usize) -> Element {
    Element {
        chain: vec![node("li", None, &["card"], &[], slot as u32 + 1), node("ul", Some("products"), &[], &[], 1)],
        pug: format!("li\n    a\n        span | 상품 {product}\n    span | 무료배송\n"),
    }
}

fn card(i: usize) -> Element {
    card_at(i, i)
}

fn button(id: &str, text: &str) -> Element {
    Element { chain: vec![node("button", Some(id), &["btn"], &[], 1), node("div", None, &["pay"], &[], 1)], pug: format!("button | {text}") }
}

fn ev(kind: EventKind, href: &str, sid: &str, seq: u32, t_ms: i64) -> WebEvent {
    let mut e = WebEvent::new(kind, href);
    e.sid = sid.into();
    e.seq = seq;
    e.t_ms = t_ms;
    e
}

fn click(href: &str, sid: &str, seq: u32, t_ms: i64, target: Element, related: Vec<Element>) -> WebEvent {
    let mut e = ev(EventKind::Click, href, sid, seq, t_ms);
    e.target = Some(target);
    e.related = related;
    e
}

/// 목록에서 `chosen` 카드를 눌렀다. 형제는 `visible` 중 고른 것을 뺀 것
fn pick(sid: usize, chosen: usize, visible: &[usize]) -> WebEvent {
    let related = visible.iter().copied().filter(|i| *i != chosen).map(card).collect();
    click(LIST, &format!("s{sid}"), 1, 3_000, card(chosen), related)
}

fn key(page: &str, el: &Element) -> String {
    canonical_of(&page_hash(page).unwrap(), el).stable_key
}

#[test]
fn wa01_live_stock_is_stripped_and_product_numbers_kept() {
    let page = page_hash(LIST).unwrap();
    // A: 같은 키보드 카드, 재고 · 남은 시간만 바뀌었다
    let keyboard = |stock: u32, timer: &str| Element { chain: card(0).chain, pug: format!("li\n    span | 키보드\n    span | 남은 수량 {stock}개\n    span | {timer} 마감\n") };
    let (a3, a1) = (canonical_of(&page, &keyboard(3, "14:22")), canonical_of(&page, &keyboard(1, "14:21")));
    assert_eq!(a3.stable_key, a1.stable_key);
    assert_eq!(a3.display_label, "키보드");
    // 예전 키(원래 글)는 둘을 다른 후보로 갈라 선택 대조가 흩어진다
    assert_ne!(choice_key(&page, &keyboard(3, "14:22")), choice_key(&page, &keyboard(1, "14:21")));
    // B: 같은 셀렉터 틀의 상품 카드는 번호로 갈린다
    assert_ne!(canonical_of(&page, &card(1)).stable_key, canonical_of(&page, &card(2)).stable_key);
    assert_eq!(canonical_of(&page, &card(1)).display_label, "상품 1 무료배송");
    // 예전 PUG 계약 13 사례는 한 글자도 바뀌지 않는다
    let v: Value = serde_json::from_str(PUG_V1).unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 13);
    for c in cases {
        assert_eq!(pug::html_to_pug(c["html"].as_str().unwrap()), c["pug"].as_str().unwrap(), "{}", c["name"]);
    }
}

#[test]
fn wa02_icon_buttons_keep_their_label_and_inputs_stay_empty() {
    assert_eq!(pug::html_to_pug("<button aria-label=\"장바구니\"><svg><path d=\"M0\"/></svg></button>"), "button | [장바구니]\n");
    assert_eq!(pug::html_to_pug("<a aria-label=\"주문 20261121\"><svg/></a>"), "a | [주문 #]\n");
    let input = pug::html_to_pug("<input value=\"홍길동\" aria-label=\"이름\">");
    assert_eq!(input, "input\n");
    assert!(!input.contains("홍길동") && !input.contains("이름"));
    // 받는 쪽의 개인정보 거르기를 다시 지나도 이름표는 그대로다 (거르기는 줄 끝의 줄바꿈만 떼어 낸다)
    let mut e = ev(EventKind::Click, "https://shop.example.com/mypage", "s", 1, 10);
    e.target = Some(Element { chain: vec![node("a", None, &[], &[], 1)], pug: pug::html_to_pug("<a aria-label=\"주문 20261121\"><svg/></a>") });
    assert_eq!(e.sanitized().unwrap().target.unwrap().pug, "a | [주문 #]");
}

#[test]
fn wa03_a_cart_drawer_becomes_a_virtual_cart_checkpoint() {
    const PRODUCT: &str = "https://shop.example.com/product/1";
    let drawer = modal_href(PRODUCT, "cart").unwrap();
    assert_eq!(drawer, "https://shop.example.com/product/1#/__modal/cart");
    // 가상 주소는 주소 정리 · URL 틀을 그대로 지나고, 상품 페이지와 다른 페이지다
    assert_eq!(clean_href(&drawer).as_deref(), Some(drawer.as_str()));
    assert_eq!(url_template(&drawer).as_deref(), Some("shop.example.com/product/:id#/__modal/cart"));
    assert_ne!(page_hash(&drawer), page_hash(PRODUCT));
    // 고정 층이어도 모달의 뿌리는 PUG 에 남는다
    let html = "<div role=\"dialog\" aria-modal=\"true\" style=\"position: fixed; right: 0\"><h2>장바구니</h2><button>주문하기</button></div>";
    assert_eq!(pug::html_to_pug(html), "div\n    h2 | 장바구니\n    button | 주문하기\n");

    // 주소가 /product/1 그대로인 채 드로어가 열린 세션: 상품 → 드로어(장바구니) → 결제 → 제출 → 완료
    let enter: WebEvent = serde_json::from_value(json!({"type": "enter", "href": drawer, "sid": "s", "seq": 3, "t_ms": 2_500, "surface": "modal_drawer"})).unwrap();
    let enter = enter.sanitized().unwrap();
    assert_eq!(enter.surface, Some(VirtualSurface::ModalDrawer));
    assert_eq!(AnalyticDictionary::builtin().page_kind_of(&enter.href).as_deref(), Some("cart"));
    let mut order = click(&drawer, "s", 4, 4_000, button("order", "주문하기"), vec![]);
    order.surface = Some(VirtualSurface::ModalDrawer);
    let session = vec![
        ev(EventKind::Enter, PRODUCT, "s", 1, 0),
        click(PRODUCT, "s", 2, 2_000, button("add", "장바구니 담기"), vec![]),
        enter,
        order,
        ev(EventKind::Enter, CHECKOUT, "s", 5, 6_000),
        ev(EventKind::Submit, CHECKOUT, "s", 6, 8_000),
        ev(EventKind::Enter, DONE, "s", 7, 9_000),
    ];
    let trace = trace_session(&session);
    let drawer_page = page_hash(&drawer).unwrap();
    assert!(trace.checkpoints.iter().any(|c| c.kind == StepKind::Page && c.hash == drawer_page), "the drawer is a page checkpoint");
    assert_eq!(trace.path.len(), 4);
    let mut site = SiteAnalysis::new("shop.example.com");
    site.add_session(&session);
    assert_eq!(site.dag.kinds.get(&drawer_page).map(String::as_str), Some("cart"));
    let b = site.branch_summary("goods_view", "order_complete");
    assert_eq!((b.entered, b.cart_entered, b.classic_completed, b.fast_track_completed), (1, 1, 1, 0), "{b:?}");
}

#[test]
fn wa04_a_resent_batch_inserts_nothing_twice() {
    // 무거운 형제 30개가 붙은 누름 30건 + 마지막 떠남. 2초 뒤 같은 묶음을 다시 보냈고(서버가 받은 시각만 다르다), 다시 보낸 묶음은
    // 떠날 때처럼 형제가 8개로 줄어 있다
    let mut batch: Vec<WebEvent> = (1..=30u32).map(|i| click(LIST, "s1", i, i as i64 * 700, card(i as usize), (0..30).map(card).collect())).collect();
    let mut leave = ev(EventKind::Leave, LIST, "s1", 31, 30_000);
    leave.dirty = Some(1);
    batch.push(leave);
    let cc = hash_id("shop.example.com").unwrap();
    let session = hash_id(&format!("{cc}s1")).unwrap();
    // web_events (id TEXT PRIMARY KEY, INSERT OR IGNORE) 와 같은 표
    let mut table: BTreeMap<String, WebEvent> = BTreeMap::new();
    let mut inserted = 0;
    for resend in 0..2i64 {
        for e in &batch {
            let mut got = e.clone();
            got.at_ms = 1_800_000_000_000 + resend * 2_000;
            if resend == 1 {
                got.related.truncate(8);
            }
            let id = deterministic_event_id(&cc, &session, &got).expect("tracker events carry seq · t_ms");
            if let std::collections::btree_map::Entry::Vacant(v) = table.entry(id) {
                v.insert(got);
                inserted += 1;
            }
        }
    }
    assert_eq!((inserted, table.len()), (31, 31), "the resend inserted nothing");
    // 떠남 사건은 형제가 없어 첫 묶음(< 8 KB)에 늘 들어간다 (묶음 나누기는 tracker.test.ts 가 본다)
    assert!(serde_json::to_string(&batch[30]).unwrap().len() < 300);
}

#[test]
fn wa05_only_visible_peers_are_candidates_and_large_grids_still_decide() {
    let page = page_hash(LIST).unwrap();
    // DOM 에 카드 80개, 뷰포트에는 8개(자리 0 ~ 7). 수집 스크립트는 보인 형제 7개만 보낸다. 같은 카드(자리 2)를 여섯 번 고른다
    let visible: Vec<usize> = (0..8).collect();
    let mut book = DebiasedChoiceBook::default();
    for i in 0..6 {
        assert!(book.observe_event(&pick(i, 2, &visible)));
    }
    assert_eq!(book.items.len(), 8, "n_eff = 8");
    assert!((8..80).all(|hidden| !book.items.contains_key(&canonical_of(&page, &card(hidden)).stable_key)), "no exposure, so no passed-over vote, for unseen cards");
    let spread: f64 = book.items.values().map(|t| t.expected).sum();
    assert!((spread - 6.0).abs() < 1e-9, "each choice spreads exactly one expected pick: {spread}");
    // 포아송-이항: 자리 2 의 기저율 3^−0.35 / Σ(k^−0.35, k = 1..8)
    let p = 3f64.powf(-0.35) / (1..=8).map(|k| (k as f64).powf(-0.35)).sum::<f64>();
    let chosen = book.items[&canonical_of(&page, &card(2)).stable_key];
    assert_eq!((chosen.picks, chosen.exposures), (6, 6));
    assert!((chosen.expected - 6.0 * p).abs() < 1e-9 && (chosen.variance - 6.0 * p * (1.0 - p)).abs() < 1e-9, "{chosen:?}");

    // 예전 사건이 DOM 전체(79개)를 형제로 보냈으면 고른 자리 ±8 안의 가까운 순으로 16개까지만 후보다
    let all: Vec<usize> = (0..80).collect();
    let mut near_top = DebiasedChoiceBook::default();
    near_top.observe_event(&pick(0, 2, &all));
    assert_eq!(near_top.items.len(), 11, "slots 0 ..= 10");
    let mut deep = DebiasedChoiceBook::default();
    deep.observe_event(&pick(0, 40, &all));
    assert_eq!(deep.items.len(), 16);
    let has = |b: &DebiasedChoiceBook, i: usize| b.items.contains_key(&canonical_of(&page, &card(i)).stable_key);
    assert!(has(&deep, 32) && !has(&deep, 48) && !has(&deep, 31));

    // 분산이 작은 큰 그리드(보인 카드 10개, 노출 8번)라도 여덟 번 모두 고른 카드는 결정된다 (분산 하한 0.8 을 두지 않는다)
    let ten: Vec<usize> = (0..10).collect();
    let mut grid = DebiasedChoiceBook::default();
    for i in 0..8 {
        grid.observe_event(&pick(i, 2, &ten));
    }
    let row = grid.diagnose(&canonical_of(&page, &card(2)).stable_key).unwrap();
    assert!(row.variance < 0.8 && row.z_choice.unwrap() > 2.0, "{row:?}");
    assert_eq!(row.kind, WebElementKind::GoldenPath);
}

#[test]
fn wa06_position_bias_is_removed_and_unpopular_options_stay_neutral() {
    let page = page_hash(LIST).unwrap();
    // 맨 위 자리에 고정된 추천 상품(100)과 돌아가며 놓이는 상품 30개. 방문자는 자리의 기저율((k+1)^−0.35)대로만 누른다
    let weights: Vec<f64> = (0..10).map(|k| ((k + 1) as f64).powf(-0.35)).collect();
    let total: f64 = weights.iter().sum();
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let mut uniform = ChoiceBook::default();
    let mut book = DebiasedChoiceBook::default();
    for i in 0..600 {
        let offset = rng.below(30) as usize;
        let products: Vec<usize> = std::iter::once(100).chain((0..9).map(|j| (offset + j * 3) % 30)).collect();
        let r = (rng.next() % 1_000_000) as f64 / 1_000_000.0 * total;
        let mut acc = 0.0;
        let slot = weights.iter().position(|w| {
            acc += w;
            r < acc
        });
        let slot = slot.unwrap_or(9);
        let related = products.iter().enumerate().filter(|(s, _)| *s != slot).map(|(s, p)| card_at(*p, s)).collect();
        let e = click(LIST, &format!("s{i}"), 1, 3_000, card_at(products[slot], slot), related);
        uniform.observe_event(&e);
        book.observe_event(&e);
    }
    let pinned = card_at(100, 0);
    let old = uniform.items[&choice_key(&page, &pinned).unwrap()];
    assert_eq!(old.decide(&ChoiceParams::default()), Some(MarkType::Premium), "1/n calls the top slot a winner: {old:?}");
    let row = book.diagnose(&canonical_of(&page, &pinned).stable_key).unwrap();
    assert!(row.z_choice.unwrap().abs() < 2.0, "picked only as often as its slot: {row:?}");
    assert_ne!(row.tier, Tier::Premium);

    // 가로 색상 라디오 4개 (고르게 1/4): 브라운은 40번 중 4번만 골라지지만 고른 뒤 마찰이 없다
    let colors = ["블랙", "화이트", "네이비", "브라운"];
    let swatch = |i: usize| Element {
        chain: vec![node("label", None, &["swatch"], &[("role", "radio")], i as u32 + 1), node("div", Some("colors"), &[], &[], 1)],
        pug: format!("label | {}", colors[i]),
    };
    let mut colors_book = DebiasedChoiceBook::default();
    let mut colors_uniform = ChoiceBook::default();
    for i in 0..40 {
        let chosen = if i % 10 == 0 { 3 } else { i % 3 };
        let e = click(DETAIL, &format!("c{i}"), 1, 1_000, swatch(chosen), (0..4).filter(|j| *j != chosen).map(swatch).collect());
        colors_book.observe_event(&e);
        colors_uniform.observe_event(&e);
        colors_book.observe_post_pick(&key(DETAIL, &swatch(chosen)), &page_hash(DETAIL).unwrap(), 0.0, true, false, 0.25);
    }
    let brown = colors_book.diagnose(&key(DETAIL, &swatch(3))).unwrap();
    assert!(brown.z_choice.unwrap() <= -2.0 && brown.post_friction_z.unwrap() < 1.0, "{brown:?}");
    assert_eq!((brown.kind, brown.tier), (WebElementKind::BenignIgnored, Tier::Neutral));
    assert_eq!(brown.implicit_obs(&Params::v2(), 0), None, "no negative vote for a flawless unpopular colour");
    assert_eq!(colors_uniform.items[&choice_key(&page_hash(DETAIL).unwrap(), &swatch(3)).unwrap()].decide(&ChoiceParams::default()), Some(MarkType::Discount));
    // 기획의 수치: 선택 z −2.2 · 고른 뒤 마찰 z −0.3 → BenignIgnored, 마찰 z +1.2 면 DeadWeight
    let t = ChoiceTally { picks: 4, exposures: 40, expected: 10.0, variance: 7.5 };
    let outcome = ChoiceOutcome { picks: 4, completed: 4, abandoned: 0 };
    assert_eq!(classify_choice(&t, Some(-0.3), &outcome, &DebiasConfig::default()), WebElementKind::BenignIgnored);
    assert_eq!(classify_choice(&t, None, &outcome, &DebiasConfig::default()), WebElementKind::BenignIgnored);
    assert_eq!(classify_choice(&t, Some(1.2), &outcome, &DebiasConfig::default()), WebElementKind::DeadWeight);
}

/// 상세 → 바로 구매 → 결제 → 제출 → 완료 (장바구니를 거치지 않는다)
fn fast_track(sid: &str) -> Vec<WebEvent> {
    vec![
        ev(EventKind::Enter, DETAIL, sid, 1, 0),
        click(DETAIL, sid, 2, 2_000, button("buy-now", "바로 구매"), vec![button("add-cart", "장바구니 담기")]),
        ev(EventKind::Enter, CHECKOUT, sid, 3, 4_000),
        click(CHECKOUT, sid, 4, 5_000, button("easy-pay", "간편결제"), vec![]),
        ev(EventKind::Submit, CHECKOUT, sid, 5, 5_500),
        ev(EventKind::Enter, DONE, sid, 6, 7_000),
    ]
}

/// 상세 → 장바구니 담기 → 장바구니 → 주문 → 결제 → 제출 → 완료
fn classic(sid: &str) -> Vec<WebEvent> {
    vec![
        ev(EventKind::Enter, DETAIL, sid, 1, 0),
        click(DETAIL, sid, 2, 2_000, button("add-cart", "장바구니 담기"), vec![button("buy-now", "바로 구매")]),
        ev(EventKind::Enter, CART, sid, 3, 4_000),
        click(CART, sid, 4, 6_000, button("order", "주문하기"), vec![]),
        ev(EventKind::Enter, CHECKOUT, sid, 5, 8_000),
        ev(EventKind::Submit, CHECKOUT, sid, 6, 9_500),
        ev(EventKind::Enter, DONE, sid, 7, 11_000),
    ]
}

/// 상세 → 장바구니 담기 → 장바구니에서 떠남
fn cart_drop(sid: &str) -> Vec<WebEvent> {
    vec![
        ev(EventKind::Enter, DETAIL, sid, 1, 0),
        click(DETAIL, sid, 2, 2_000, button("add-cart", "장바구니 담기"), vec![button("buy-now", "바로 구매")]),
        ev(EventKind::Enter, CART, sid, 3, 4_000),
        ev(EventKind::Leave, CART, sid, 4, 9_000),
    ]
}

/// 비교 탭: 다른 상품 상세에서 각인 문구를 적어 보고 닫았다
fn closed_tab(sid: &str, product: usize, at_ms: i64) -> Vec<WebEvent> {
    let href = format!("https://shop.example.com/goods/view/{product}");
    let mut engrave = ev(EventKind::Change, &href, sid, 2, 3_000);
    engrave.target = Some(Element { chain: vec![node("input", Some("engrave"), &[], &[("type", "text")], 1)], pug: "input".into() });
    engrave.field = Some("text".into());
    let mut leave = ev(EventKind::Leave, &href, sid, 3, 9_000);
    leave.dirty = Some(1);
    let mut v = vec![ev(EventKind::Enter, &href, sid, 1, 0), engrave, leave];
    v.iter_mut().for_each(|e| e.at_ms = at_ms + e.t_ms);
    v
}

#[test]
fn wa07_fast_track_sessions_do_not_pollute_cart_drops_and_comparison_tabs_are_exempt() {
    // A: 바로 구매 20 세션 · 장바구니 경로 4 세션 · 장바구니 이탈 2 세션
    let mut sessions: Vec<Vec<WebEvent>> = Vec::new();
    for i in 0..26 {
        sessions.push(match i % 13 {
            0 => cart_drop(&format!("d{i}")),
            1 | 2 => classic(&format!("c{i}")),
            _ => fast_track(&format!("f{i}")),
        });
    }
    let mut site = SiteAnalysis::new("shop.example.com");
    site.ingest_cohort(&sessions);
    let b = site.branch_summary("goods_view", "order_complete");
    assert_eq!((b.entered, b.completed, b.fast_track_completed, b.classic_completed), (26, 24, 20, 4), "{b:?}");
    assert_eq!((b.cart_entered, b.cart_dropped), (6, 2), "fast-track sessions are not in the cart denominator");
    // 예전 순서 퍼널은 단축 경로 세션 20개를 상품 단계의 이탈로 센다
    let steps: Vec<String> = [DETAIL, CART, CHECKOUT, DONE].iter().map(|u| page_hash(u).unwrap()).collect();
    assert_eq!(site.funnel(&steps)[0].dropped, 20);
    // 바로 구매는 추천 경로로 올라가고, 단축 경로를 연 요소로 집계된다
    let buy_now = key(DETAIL, &button("buy-now", "바로 구매"));
    assert!(b.golden_paths.iter().any(|r| r.stable_key == buy_now), "{:?}", b.golden_paths);
    let first = b.fast_track_elements.first().unwrap();
    assert_eq!((first.stable_key.as_str(), first.sessions, first.golden), (buy_now.as_str(), 20, true));
    assert!(b.fast_track_mean_ms.unwrap() < b.classic_mean_ms.unwrap());

    // B: 같은 탭 묶음에서 상품 상세 5탭을 열어 견주고, 1탭은 결제를 끝내고 4탭은 닫았다
    let base = 1_800_000_000_000;
    let mut buyer = classic("t1");
    buyer.iter_mut().for_each(|e| e.at_ms = base + 60_000 + e.t_ms);
    let mut tabs = vec![buyer];
    tabs.extend((2..=5).map(|k| closed_tab(&format!("t{k}"), k, base + k as i64 * 1_000)));
    let grouped_tabs: Vec<Vec<WebEvent>> = tabs.iter().map(|t| t.iter().cloned().map(|mut e| { e.tab_group = Some("tg-1".into()); e }).collect()).collect();
    let mut grouped = SiteAnalysis::new("shop.example.com");
    grouped.ingest_cohort(&grouped_tabs);
    assert_eq!((grouped.dag.abandoned, grouped.dag.comparison_browse_exempt), (0, 4), "closed comparison tabs carry no abandonment");
    // 묶음이 없으면 닫힌 탭 넷 모두 이탈이다 (적은 글이 옵션이 아니라서 옵션 확인 면제도 아니다)
    let mut loose = SiteAnalysis::new("shop.example.com");
    loose.ingest_cohort(&tabs);
    assert_eq!((loose.dag.abandoned, loose.dag.comparison_browse_exempt), (4, 0));
    // 30분 뒤의 전환은 근거가 아니다
    let mut late_tabs = grouped_tabs.clone();
    late_tabs[0].iter_mut().for_each(|e| e.at_ms += 30 * 60_000);
    let mut late = SiteAnalysis::new("shop.example.com");
    late.ingest_cohort(&late_tabs);
    assert_eq!(late.dag.abandoned, 4);
}

/// 결제 페이지에서 쿠폰 적용을 누르고 헤매다 떠남 (광클 · 쿠폰 코드 칸 다시 고침 · 바꾼 칸을 두고 떠남)
fn coupon_trap(sid: &str) -> Vec<WebEvent> {
    let mut v = vec![
        ev(EventKind::Enter, CHECKOUT, sid, 1, 0),
        click(CHECKOUT, sid, 2, 2_000, button("coupon", "쿠폰 적용"), vec![button("easy-pay", "간편결제")]),
    ];
    let mut rage = click(CHECKOUT, sid, 3, 2_500, button("coupon", "쿠폰 적용"), vec![]);
    rage.rage = true;
    v.push(rage);
    for (i, t) in [3_000, 5_000, 7_000].iter().enumerate() {
        let mut c = ev(EventKind::Change, CHECKOUT, sid, 4 + i as u32, *t);
        c.target = Some(Element { chain: vec![node("input", Some("coupon-code"), &[], &[("type", "text")], 1)], pug: "input".into() });
        c.field = Some("text".into());
        v.push(c);
    }
    let mut leave = ev(EventKind::Leave, CHECKOUT, sid, 7, 12_000);
    leave.dirty = Some(1);
    v.push(leave);
    v
}

fn coupon_done(sid: &str) -> Vec<WebEvent> {
    vec![
        ev(EventKind::Enter, CHECKOUT, sid, 1, 0),
        click(CHECKOUT, sid, 2, 2_000, button("coupon", "쿠폰 적용"), vec![button("easy-pay", "간편결제")]),
        ev(EventKind::Submit, CHECKOUT, sid, 3, 6_000),
        ev(EventKind::Enter, DONE, sid, 4, 8_000),
    ]
}

fn easy_pay(sid: &str) -> Vec<WebEvent> {
    vec![
        ev(EventKind::Enter, CHECKOUT, sid, 1, 0),
        click(CHECKOUT, sid, 2, 2_000, button("easy-pay", "간편결제"), vec![button("coupon", "쿠폰 적용")]),
        ev(EventKind::Submit, CHECKOUT, sid, 3, 2_500),
        ev(EventKind::Enter, DONE, sid, 4, 4_000),
    ]
}

#[test]
fn wa08_a_popular_coupon_button_with_friction_is_a_trap_not_a_golden_path() {
    let coupon = key(CHECKOUT, &button("coupon", "쿠폰 적용"));
    // 고른 뒤 구간이 쿠폰 코드 칸의 다시 고침 · 광클 · 떠남을 쿠폰 단추의 몫으로 센다 (인과 귀속)
    let windows = post_pick_windows(&coupon_trap("probe"), &BehaviorParams::default(), AbandonPolicy::Always);
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].choice.stable_key, coupon);
    assert!(windows[0].abandoned && !windows[0].completed);
    assert!((windows[0].friction - 10.0).abs() < 1e-9, "rage 3 + retried 2 × 2 + abandoned 3: {}", windows[0].friction);

    // 40 세션: 30 세션이 쿠폰을 먼저 고르고(그중 대부분이 헤매다 떠남), 10 세션은 간편결제로 바로 끝낸다
    let mut site = SiteAnalysis::new("shop.example.com");
    for i in 0..40 {
        let s = format!("s{i}");
        site.add_session(&match i % 4 {
            0 => easy_pay(&s),
            _ if i % 5 == 1 => coupon_done(&s),
            _ => coupon_trap(&s),
        });
    }
    let row = site.debiased.diagnose(&coupon).unwrap();
    assert!(row.z_choice.unwrap() >= 2.0 && row.post_friction_z.unwrap() >= 1.0, "{row:?}");
    assert_eq!((row.kind, row.tier), (WebElementKind::FrictionTrap, Tier::Discount));
    // 마찰 거부권: 많이 골랐어도 +1 대신 −1 만 낸다
    assert_eq!(row.implicit_obs(&Params::v2(), 1_000).map(|o| (o.sign, o.weight)), Some((-1, 0.3)));
    let b = site.branch_summary("checkout", "order_complete");
    assert_eq!(b.primary_bottleneck.as_ref().map(|r| r.stable_key.as_str()), Some(coupon.as_str()));
    assert!(b.golden_paths.iter().all(|r| r.stable_key != coupon));
    // 기획의 수치: 선택 z +2.8 에 고른 뒤 마찰 z +1.9 → 함정, +0.4 면 추천 경로
    let t = ChoiceTally { picks: 29, exposures: 40, expected: 20.0, variance: 10.0 };
    let cfg = DebiasConfig::default();
    assert_eq!(classify_choice(&t, Some(1.9), &ChoiceOutcome::default(), &cfg), WebElementKind::FrictionTrap);
    assert_eq!(classify_choice(&t, Some(0.4), &ChoiceOutcome::default(), &cfg), WebElementKind::GoldenPath);
    // 마찰이 낮아도 절반 이상 고르고 바로 떠나면 함정
    assert_eq!(classify_choice(&t, Some(0.0), &ChoiceOutcome { picks: 10, completed: 2, abandoned: 6 }, &cfg), WebElementKind::FrictionTrap);
}

#[test]
fn wa11_this_week_vs_last_week_is_pace_matched_and_small_samples_are_not_judged() {
    let q = QueryParser::lexical(AnalyticDictionary::builtin()).parse("이번 주 vs 지난주 전환율", "ko", TUESDAY_13);
    let w = q.comparison.clone().expect("two periods make a comparison");
    assert!(w.pace_matched);
    assert_eq!(w.current_end_ms - w.current_start_ms, 37 * HOUR);
    assert_eq!(w.previous_end_ms - w.previous_start_ms, 37 * HOUR, "last week is cut at the same +37 h");
    assert_eq!(w.current_start_ms - w.previous_start_ms, 7 * DAY);
    assert_eq!(w.label, "this_week (+37.0h) vs last_week (+37.0h)");
    assert_eq!(Some(w.clone()), resolve_comparison_window(Some("this_week"), Some("last_week"), "ko", TUESDAY_13));

    // 이번 주 창에는 세션이 4건뿐: 표본 부족, 전환율 변화를 계산하지 않고 단정하는 말도 없다
    let mut now = SiteAnalysis::new("shop.example.com");
    for i in 0..4 {
        now.add_session(&if i == 0 { cart_drop(&format!("n{i}")) } else { classic(&format!("n{i}")) });
    }
    let mut before = SiteAnalysis::new("shop.example.com");
    for i in 0..40 {
        before.add_session(&classic(&format!("b{i}")));
    }
    let (cur, prev) = (now.branch_summary("goods_view", "order_complete"), before.branch_summary("goods_view", "order_complete"));
    let c = ConversionComparison::new(w, (cur.entered, cur.completed), (prev.entered, prev.completed));
    assert_eq!(c.confidence, ReportConfidence::InsufficientSample { sessions: 4, min_required: 15 });
    assert_eq!(c.delta_pct_pt, None);
    let line = c.line();
    for word in ["급락", "폭락", "급증", "하락", "떨어", "줄었", "plunge", "drop", "crash", "%p"] {
        assert!(!line.contains(word), "{word} in {line}");
    }
}

#[test]
fn wa12_tombstones_block_a_resync_and_the_blob_cases_still_restore() {
    // forget(host, before_ms) 와 forget(ids)
    let mut guard = TombstoneGuard::default();
    assert_eq!(guard.forget_before("Shop.Example.com", 2_000), 2_000);
    assert!(guard.forget_id("e5"));
    // 묘비 테이블 모양(id, deleted_at)은 그대로: 컷오프는 `cutoff:<host>` 행이다
    let rows = [(cutoff_id("shop.example.com"), 2_000), ("e5".to_string(), 9_999)];
    let reread = TombstoneGuard::from_rows(rows.iter().map(|(id, at)| (id.as_str(), *at)));
    assert_eq!(reread, guard);
    // 백그라운드 재동기화가 같은 쪽을 다시 받는다: 컷오프 이전 · id 묘비는 막고, 구조화한 행은 두고, 나머지만 넣는다
    let resync = [("e1", 1_000, None), ("e2", 2_000, None), ("e3", 3_000, Some(5_000)), ("e4", 4_000, Some(0)), ("e5", 5_000, None), ("e6", 6_000, None)];
    let decisions: Vec<IngestDecision> = resync.iter().map(|(id, at, upd)| ingest_decision(&reread, "shop.example.com", id, *at, *upd)).collect();
    use IngestDecision::*;
    assert_eq!(decisions, [BlockedTombstoned, BlockedTombstoned, KeptStructured, Insert, BlockedTombstoned, Insert]);

    // BLOB 계약 19 사례는 그대로 풀리고, 사건 행 사이에 섞인 손상 행 하나는 broken 으로 따로 센 채 묶음을 끝까지 넘긴다
    let v: Value = serde_json::from_str(BLOB_V1).unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 19);
    for c in cases {
        assert_eq!(blob::decode(&c["input"]), c["expect"].as_object().unwrap().clone(), "{}", c["name"]);
    }
    let mut datas: Vec<Value> = cases.iter().filter(|c| c["expect"].get("href").is_some()).map(|c| c["input"].clone()).collect();
    let valid = datas.len();
    assert_eq!(valid, 8);
    datas.insert(3, Value::String("H4sI-not-gzip-at-all".into()));
    let (mut ok, mut broken) = (0, 0);
    for d in &datas {
        match decode_row_event(d, 1_000) {
            Some(e) => {
                assert_eq!((e.href.as_str(), e.at_ms), ("https://shop.example.com/checkout", 1_000));
                ok += 1;
            }
            None => broken += 1,
        }
    }
    assert_eq!((ok, broken), (valid, 1));
}

#[test]
fn an_option_check_on_a_product_page_is_not_an_abandonment_but_a_checkout_form_is() {
    let option_check = |href: &str, rage: bool| {
        let mut size = ev(EventKind::Change, href, "s", 2, 2_000);
        size.target = Some(Element { chain: vec![node("select", Some("size"), &[], &[], 1)], pug: "select".into() });
        size.field = Some("select".into());
        let mut v = vec![ev(EventKind::Enter, href, "s", 1, 0), size];
        if rage {
            let mut r = click(href, "s", 3, 2_500, button("add", "담기"), vec![]);
            r.rage = true;
            v.push(r);
        }
        let mut leave = ev(EventKind::Leave, href, "s", 4, 5_000);
        leave.dirty = Some(1);
        v.push(leave);
        v
    };
    let mut site = SiteAnalysis::new("shop.example.com");
    site.ingest_cohort(&[option_check(DETAIL, false), option_check(CHECKOUT, false), option_check(DETAIL, true)]);
    assert_eq!((site.dag.exploratory_exempt, site.dag.abandoned), (1, 2), "{:?}", site.dag);
    // 예전 add_session 은 모두 센다
    let mut old = SiteAnalysis::new("shop.example.com");
    old.add_session(&option_check(DETAIL, false));
    assert_eq!((old.dag.exploratory_exempt, old.dag.abandoned), (0, 1));
}

#[test]
fn picking_then_closing_a_product_tab_is_an_abandonment_only_without_the_cohort_rules() {
    let swatch = |i: u32| Element { chain: vec![node("label", None, &["swatch"], &[("role", "radio")], i + 1)], pug: format!("label | 색 {i}") };
    let tab = |href: &str| vec![ev(EventKind::Enter, href, "s", 1, 0), click(href, "s", 2, 1_000, swatch(0), vec![swatch(1), swatch(2)]), ev(EventKind::Leave, href, "s", 3, 4_000)];
    let abandoned = |href: &str, policy: AbandonPolicy| post_pick_windows(&tab(href), &BehaviorParams::default(), policy)[0].abandoned;
    assert!(abandoned(DETAIL, AbandonPolicy::Always));
    assert!(!abandoned(DETAIL, AbandonPolicy::ComparisonBrowse));
    assert!(!abandoned(DETAIL, AbandonPolicy::ExploreAware));
    // 결제 페이지에서 고르고 그냥 떠난 것은 규칙과 상관없이 바로 떠남이다
    assert!(abandoned(CHECKOUT, AbandonPolicy::ExploreAware));
}

#[test]
fn scenario_5_collection_rules_match_the_tracker() {
    let v: Value = serde_json::from_str(S5_CASES).unwrap();
    for c in v["pug"].as_array().unwrap() {
        assert_eq!(pug::html_to_pug(c["html"].as_str().unwrap()), c["pug"].as_str().unwrap(), "{}", c["name"]);
    }
    for c in v["modal_href"].as_array().unwrap() {
        assert_eq!(modal_href(c["href"].as_str().unwrap(), c["slug"].as_str().unwrap()).as_deref(), c["expect"].as_str(), "{c}");
    }
    for c in v["slug"].as_array().unwrap() {
        assert_eq!(premium_discount_analytic::event::modal_slug(c["raw"].as_str().unwrap()), c["expect"].as_str().unwrap(), "{c}");
    }
}

#[test]
fn the_debiased_book_round_trips_as_json() {
    let mut book = DebiasedChoiceBook::default();
    for i in 0..12 {
        book.observe_event(&pick(i, i % 3, &(0..6).collect::<Vec<_>>()));
    }
    book.observe_post_pick(&key(LIST, &card(0)), &page_hash(LIST).unwrap(), 1.5, false, true, 0.25);
    let back: DebiasedChoiceBook = serde_json::from_value(serde_json::to_value(&book).unwrap()).unwrap();
    assert_eq!(back, book);
    assert!(book.slot_priors.values().map(|s| s.exposures).sum::<u64>() >= 50, "the empirical slot correction is active");
}
