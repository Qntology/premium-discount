//! Qwen3.5-0.8B 실측: 출력 헤드 Trimming(16,384)과 양자화(Q8_0)가 결과를 얼마나 바꾸는지, 얼마나 빨라지는지.
//!
//!   cargo run --release -j 1 --example qwen_probe -- <Qwen3.5-0.8B 폴더> [q8_0]
//! 출력: 페이지마다 전체 헤드 대 잘라 낸 헤드의 그리디 토큰 일치, 초안(JSON), 토큰당 시간. 마지막에 교사 강제 NLL.

use std::path::PathBuf;
use std::time::Instant;

use premium_discount_app::items_draft::{corpus, draft, draft_trimmed, SYSTEM};
use premium_discount_app::qwen35::{chat_prompt, plan_vocab, Qwen35};
use premium_discount_app::Bits;

const PAGES: &[(&str, &str)] = &[
    ("겨울 패딩 최대 70% 할인! 오늘 자정까지 쿠폰 적용 시 추가 10% 할인됩니다. 무료 배송.", "https://shop.example/padding"),
    ("Hand-stitched Italian leather briefcase, full-grain calfskin, lifetime warranty. Limited edition of 200.", "https://atelier.example/briefcase"),
    ("장인이 한 땀 한 땀 만든 수제 가죽 지갑. 이탈리아산 베지터블 가죽, 평생 수선 보증, 한정 수량 50개.", "https://craft.example/wallet"),
    ("Flash sale: 2-for-1 on all T-shirts this weekend only. Use code SUMMER for an extra 15% off.", "https://outlet.example/tees"),
    ("스마트폰 케이스 1+1 특가 9,900원. 오늘만 이 가격! 리뷰 작성 시 적립금 2천원 추가 지급.", "https://mall.example/case"),
    ("Swiss automatic chronograph, 42mm 18k rose gold case, sapphire crystal, hand-finished movement, 5-year warranty.", "https://watch.example/chrono"),
    ("프리미엄 한우 1++ 등심 선물세트. 산지 직송, 고급 보자기 포장, 명절 한정 판매.", "https://meat.example/hanwoo"),
    ("Clearance! Up to 80% off last season's sneakers. Free returns, while stocks last.", "https://kicks.example/clearance"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&args[0]);
    let bits = (args.get(1).map(String::as_str) == Some("q8_0")).then_some(Bits::Q8_0);
    let t = Instant::now();
    let model = Qwen35::load(&dir, bits).expect("모델 로드");
    println!("load {:?} (quantized: {})", t.elapsed(), bits.is_some());
    let t = Instant::now();
    let full = model.full_head().expect("전체 헤드");
    println!("full head {:?} ({} rows, {:.0}MB F32)", t.elapsed(), full.vocab(), full.vocab() as f64 * 1024.0 * 4.0 / 1e6);
    let t = Instant::now();
    let plan = plan_vocab(&model.tokenizer, &corpus(), 16_384).expect("어휘 마이닝");
    println!("vocab plan {:?}: required {}, fill {}", t.elapsed(), plan.required.len(), plan.fill.len());

    let (mut same_total, mut tokens_total) = (0, 0);
    let (mut t_full, mut t_trim, mut n_full, mut n_trim) = (0f64, 0f64, 0usize, 0usize);
    let mut drafts_same = 0;
    for (page, link) in PAGES {
        let prompt = model.encode(&chat_prompt(SYSTEM, &format!("{page}\n\nlink: {link}")));
        // 요청마다의 잘라 낸 헤드: 고정 어휘 + 이 페이지의 토큰
        let page_ids: Vec<u32> = [model.encode(page), model.encode(link)].concat();
        let trimmed = model.trimmed_head(plan.ids_for(&page_ids)).expect("잘라 낸 헤드");
        assert_eq!(trimmed.vocab(), 16_384);
        let stop = |_: &[u32]| false;
        let t = Instant::now();
        let a = model.generate(&prompt, 32, &full, &stop).unwrap();
        t_full += t.elapsed().as_secs_f64();
        n_full += a.len();
        let t = Instant::now();
        let b = model.generate(&prompt, 32, &trimmed, &stop).unwrap();
        t_trim += t.elapsed().as_secs_f64();
        n_trim += b.len();
        let same = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        same_total += same;
        tokens_total += a.len();
        let d_full = draft(&model, &full, page, link).unwrap();
        let d_trim = draft_trimmed(&model, &plan, page, link).unwrap();
        if d_full == d_trim {
            drafts_same += 1;
        }
        println!("same {same}/{}  draft(full) {}", a.len(), serde_json::to_string(&d_full).unwrap());
        if d_full != d_trim {
            println!("           draft(trim) {}", serde_json::to_string(&d_trim).unwrap());
        }
    }
    println!(
        "greedy tokens identical {same_total}/{tokens_total}, drafts identical {drafts_same}/{}; per token (incl. prefill) full {:.0}ms, trimmed {:.0}ms",
        PAGES.len(),
        t_full * 1e3 / n_full as f64,
        t_trim * 1e3 / n_trim as f64
    );
    // 교사 강제 NLL (전체 헤드): 페이지 프롬프트 + 그 페이지의 초안 JSON
    let mut nll = 0f32;
    for (page, link) in PAGES {
        let d = draft(&model, &full, page, link).unwrap();
        let text = format!("{}{}", chat_prompt(SYSTEM, &format!("{page}\n\nlink: {link}")), serde_json::to_string(&d).unwrap());
        nll += model.nll(&model.encode(&text), &full).unwrap();
    }
    println!("mean NLL {:.5} (quantized: {})", nll / PAGES.len() as f32, bits.is_some());
}
