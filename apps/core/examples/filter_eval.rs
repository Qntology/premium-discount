//! 욕설 필터 평가: 사전(LocalFilter)과 laya-multilingual을 같은 라벨 문장으로 비교한다.
//! 라벨은 이 앱의 기준(상대를 향한 욕설·비속어·모욕이면 1)으로 직접 붙인 소규모 시험 세트다 (일반화된 정확도가 아니다).
//!
//!   cargo run --release -j 1 --example filter_eval -- <laya 폴더> [q8_0]

use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::laya::{LayaFilter, LayaModel, DEFAULT_THRESHOLD};
use premium_discount_app::{Bits, LocalFilter, ProfanityFilter, Verdict};

const CASES: &[(&str, u8)] = &[
    ("시발 이게 뭐야 돈 아깝다", 1),
    ("판매자 진짜 개새끼네 연락도 안 받음", 1),
    ("병신같은 서비스 다신 안 씀", 1),
    ("This is a fucking scam, you piece of shit.", 1),
    ("ㅅㅂ 배송 진짜 느리네", 1),
    ("씨1발 장난하냐", 1),
    ("미친놈아 환불해줘", 1),
    ("꺼져 이 사기꾼아", 1),
    ("판매자 완전 쓰레기 같은 인간이네", 1),
    ("Go to hell, you idiot seller.", 1),
    ("What a bunch of morons running this shop.", 1),
    ("니애미 이따위로 팔지마라", 1),
    ("존나 비싸고 품질은 개똥", 1),
    ("You stupid bitch, refund me now.", 1),
    ("배송이 빨라서 만족합니다. 다시 구매할게요.", 0),
    ("당시 발견된 문제는 바로 해결되었습니다.", 0),
    ("포장이 조금 아쉬웠지만 제품은 좋아요.", 0),
    ("The seller was polite and the package arrived early.", 0),
    ("Shipping took forever and nobody answered my emails.", 0),
    ("시발점이 어디인지 확인해 주세요.", 0),
    ("개나리 색상이 정말 예쁘네요.", 0),
    ("병원 가는 길에 받았어요.", 0),
    ("가격 대비 별로입니다. 추천하지 않아요.", 0),
    ("응답이 너무 느려서 화가 났습니다.", 0),
    ("The product is terrible and I want my money back.", 0),
    ("Das Paket kam beschädigt an, aber der Support war freundlich.", 0),
    ("この商品はとても良かったです。", 0),
    ("제품 설명과 실제가 달라 실망했습니다.", 0),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&args[0]);
    let bits = (args.get(1).map(String::as_str) == Some("q8_0")).then_some(Bits::Q8_0);
    let t = Instant::now();
    let laya = LayaFilter::new(LayaModel::load(&dir, bits).expect("laya 로드"));
    println!("laya load {:?} (quantized: {})", t.elapsed(), bits.is_some());
    let dict = LocalFilter::default();
    let score = |name: &str, preds: &[u8]| {
        let (mut tp, mut fp, mut tn, mut fn_) = (0, 0, 0, 0);
        for ((_, label), p) in CASES.iter().zip(preds) {
            match (label, p) {
                (1, 1) => tp += 1,
                (0, 1) => fp += 1,
                (0, 0) => tn += 1,
                _ => fn_ += 1,
            }
        }
        println!("{name:<22} 정확도 {}/{}  잡은 욕설 {tp}/{}  오탐 {fp}/{}", tp + tn, CASES.len(), tp + fn_, fp + tn);
    };
    let (mut d_pred, mut l_pred, mut both_pred) = (Vec::new(), Vec::new(), Vec::new());
    let t = Instant::now();
    for (text, label) in CASES {
        let d = (dict.check(text).unwrap() == Verdict::Profane) as u8;
        let p = laya.profanity_probability(text).unwrap();
        let l = (p >= DEFAULT_THRESHOLD) as u8;
        let both = (laya.check(text).unwrap() == Verdict::Profane) as u8;
        d_pred.push(d);
        l_pred.push(l);
        both_pred.push(both);
        let mark = |x: u8| if x == *label { " " } else { "✗" };
        println!("{label} 사전 {d}{} laya P={p:.3} {l}{} 합침 {both}{}  {text}", mark(d), mark(l), mark(both));
    }
    println!("laya 1건 평균 {:?}", t.elapsed() / CASES.len() as u32);
    score("사전(LocalFilter)", &d_pred);
    score("laya 단독 (P>=0.5)", &l_pred);
    score("사전 + laya (앱 기본)", &both_pred);
}
