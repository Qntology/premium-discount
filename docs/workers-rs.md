# Cloudflare Workers → workers-rs (2026-11-19)

`workers/cron`(premium-discount-cron)의 TypeScript 를 Rust(workers-rs 0.8.7, wasm32)로 다시 썼습니다. 방식은 logis-center 의 것
(`docs/workers-rs.md`, 커밋 370d67a3)과 같습니다. 목표는 **원본과 같은 동작**입니다: 같은 SQL, 같은 D1 호출 순서, 같은 JSON(키 순서 · 수 표기),
같은 예외 경계(무엇을 잡고 무엇이 새는지). 원본의 버그까지 그대로 옮겨 같음을 확인한 뒤(커밋 56705a3) 버그를 고쳤습니다(아래 "고친 버그").
원본은 커밋 `0538044` 의 `workers/cron/src/*.ts` 입니다. www 워커는 아직 TypeScript 이고 다음 단계입니다.

## 구조

```
workers/                    자기 Cargo 작업 공간 (wasm32). 루트 Cargo.toml 의 exclude = ["workers"]
├─ Cargo.toml  .cargo/      worker 0.8.7 고정, 산출물 workers/target, wasm 의 getrandom 백엔드(wasm_js)
├─ migrations/              D1 스키마 (www · cron 이 같이 씀, 그대로)
├─ common/                  premium-discount-worker-common (logis-center workers/common 370d67a3 에서 옮김, 이제 이쪽이 주인)
│  ├─ jsv.rs                JS 값 모형 V: 참조 공유 · undefined/null · 던지는 속성 읽기 · JS 키 순서 · ryu-js 수 표기 · JSON
│  ├─ js.rs                 D1 · Vectorize · fetch 를 JS 그대로 부름 (wasm 전용)
│  ├─ web.rs · aws.rs · eth.rs · gzip.rs · date.rs · util.rs · platform.rs   (www 가 쓸 것: aws4fetch · ethers · pako …)
│  └─ tests/                aws_eth(aws4fetch · ethers 벡터) · pako_array(pako 벡터) — logis-center 의 시험 · 벡터 그대로
├─ cron/                    premium-discount-cron (scheduled)
│  ├─ lib.rs                진입점: scheduled = 대기열 → 판정 → 지갑 정리, 예외는 거부로 샌다
│  ├─ queue.rs              평가 대기열 (claim · 부모 확인 · 욕설 검사 · 저장 · 거부 · 재시도 · 한도에서 저장 여부 확인)
│  ├─ verdicts.rs           판정 (표시 · sweep · 기준선 · 피어 다시 판정 · JSON 한 문장 쓰기)
│  ├─ verdict.rs            premium-discount-verdict crate 를 원본과 같은 비트로 쓰는 자리 (JS Math · 가중치)
│  ├─ moderation.rs · vector.rs · navtrials.rs
│  ├─ wrangler.json         바인딩 · 크론 · 변수는 TS 판 그대로, main = build/index.js
│  └─ src/*.ts · test/      원본 TypeScript (남김: www 가 cron/src/verdict.ts · navtrials.ts 를 가져다 쓴다)
└─ www/                     아직 TypeScript
scripts/workers-diff/       차분 시험대 (아래)
```

- **logis-center 에서 가져온 것**: `jsv` · `js` · `platform` · `web` · `aws` · `eth` · `gzip` · `date` · `util` 과 시험 `aws_eth.rs` · `pako_array.rs` 와 그 JSON 벡터.
  바이트 그대로이고 바꾼 곳은 셋입니다: `util::hash_str` 가 logis-schema 대신 같은 코드(premium-discount-identity + 다시 해시)를 품음,
  `js` 에 `bind_exact`(undefined 를 null 로 바꾸지 않는 bind) · `first` 를 더함, `js::to_throw` 가 Error 가 아닌 던진 값을 이름 없는 Throw 로 둠
  (원본의 `err instanceof Error ? err.message : 'unknown error'` 를 가르려고). 가져오지 않은 것: `tables`(logis 지역 · 언어 표) · `dirty_json`(LLM 출력 고치기).
  logis-center 는 다른 엔진 crate 처럼 나중에 git 리비전으로 이 crate 를 쓸 예정이고, 지금 logis-center 는 바꾸지 않았습니다.
- **다시 쓴 PD crate**: `premium-discount-verdict`(PD-Verdict v1 의 E · v · 기준선 · 등급, serde 만 의존). 원본 `verdict.ts` 와 crate 는 같은 계약
  (`contracts/verdict_v1.json`)을 통과하는 같은 식입니다. 다른 것은 둘:
  1. **초월 함수의 마지막 자리**: wasm32 의 libm `pow` · `tanh` 는 V8(fdlibm)과 마지막 비트가 다를 때가 있습니다(무작위 입력 200만 개에서 `2^x` 4.5 %,
     `tanh` 10 %). 그대로면 D1 에 쓰는 E · v · r 이 원본과 비트까지 같지 않습니다 — `libm-probe` 기능으로 빌드하면 시험대가 `verdict_hysteresis` ·
     `verdict_thresholds` · `verdict_decay_bits` 를 DIFF 로 잡고(E · v · r 243칸 중 33칸, 등급 · grade 는 모두 같음), 둘 다 계약의 허용 오차 안입니다.
     그래서 crate 에 고리를 더했습니다: `Math { pow, tanh }` · `evidence_in` · `judge_in` (기존 `evidence` · `judge` 는 `Math::STD` 로 그대로).
     cron 은 JS 의 `Math.pow` · `Math.tanh` 를 넘깁니다(V8 에서 `2 ** x` 와 `Math.pow(2, x)` 는 300만 개 입력에서 모두 같은 비트).
  2. **가중치 조회**: 원본 `PARAMS.weights[v.kind] ?? 0` 은 프로토타입을 거쳐 `constructor` 같은 이름에 함수를 돌려줍니다. crate 의 `Params::weight` 는 모르는
     종류를 0 으로 봅니다. 계약 쪽이 맞아 원본을 고쳤습니다(fix-weight-own).
  `premium-discount-nav` 는 쓰지 않았습니다: cron 이 쓰는 지갑 보관 기한(walletMaxAgeDays)은 crate 에 없고, crate 는 identity · sds 까지 끌어옵니다.
- **TS 원본**: logis-center 는 옮긴 뒤 TS 를 지웠지만, 여기서는 남겼습니다. www 가 `cron/src/verdict.ts`(RECIPE · Tier) · `navtrials.ts`(TRIAL_POLICY 등)를
  가져다 쓰고, `workers/cron/test`(41개)도 그대로 돕니다. 배포되는 것은 Rust 판입니다(`wrangler.json` 의 `main`).
- **진입점**: `#[event(scheduled)]` 는 처리기가 늘 성공으로 끝나게 감쌉니다. 원본은 대기열 · 판정에서 새는 예외가 scheduled 를 거부로 끝내므로
  (Workers 가 실패로 기록), worker-macros 0.8.7 이 만드는 것과 같은 꼴의 wasm-bindgen 내보내기 `scheduled` 를 직접 써서 `Result` 를 돌려줍니다.

## 빌드

```
cd workers
cargo test -j 2                                         # common(12 + aws_eth 5 + pako 1) · cron(12)
cargo check -j 2 --target wasm32-unknown-unknown        # 경고 없음
cd cron && worker-build --release                       # build/index.js(20 KiB) · index_bg.wasm(329 KiB)
```

배포: `cd workers/cron && npx wrangler deploy` — `build.command` 가 `cargo install -q worker-build@0.8.7 --locked && worker-build --release` 입니다.
배포 PC 에 Rust 와 `wasm32-unknown-unknown` 표적이 있어야 합니다. workers-rs 0.9 는 `cloudflare:workers` 의 `tracing.getActiveSpan()` 을 부르는데 지금 Miniflare/workerd 에
없어 0.8.7 로 고정했습니다(logis-center 와 같음).

## 차분 시험대 (`scripts/workers-diff`)

원본(`git show 0538044:workers/cron/src/*.ts` 를 esbuild `platform: "browser"` 로 묶음)과 Rust 빌드(`workers/cron/build`)를 Miniflare 에서 같은 조건으로 돌립니다:

- 같은 D1: `workers/migrations` 0001 ~ 0009 를 이름순으로 적용하고 같은 씨앗 행을 넣음 (`test-support/fakes.ts` 의 seedQueue · seedEvaluation · seedVote 와 같은 기본값).
- 같은 시계: 진입 모듈이 `scheduledTime` 으로 `Date.now()` 를 맞춤(`prelude.mjs`, UTC Date · 씨앗 있는 Math.random 은 logis-center 그대로).
- 진입 모듈이 `env` 를 Proxy 로 감쌈(`mocks_src.mjs`): **D1** 은 진짜 Miniflare D1 을 감싸 문장마다 (SQL · 묶은 값 · 방법)을 시험대에 알리고, 시험대는
  장애(`faults`)나 실행 뒤 끼워 넣을 문장(`race`: www 가 그 사이 표를 넣는 경합)을 돌려줌. **Vectorize** 는 시험대가 저장소 · 장애를 쥔 가짜
  (값은 형 · 길이 · 비트 FNV 로). **결정 API** 는 `outboundService` 가 시나리오의 `decide(text)` 로 답함.
- 견주는 것: D1 호출 전부(순서 · SQL 글 · 묶은 값 · 방법 · 결과 크기), Vectorize 호출, 바깥 fetch(주소 · 머리글 · 본문), console 기록, 밖으로 샌 예외,
  실행 결과(ok · exception), D1 끝 상태(모든 표), Vectorize 끝 상태 — JSON 바이트로.

```
# Node 22. 의존성은 저장소 밖에 설치하고 node_modules 를 정션으로 (node_modules 는 .gitignore)
cd scripts/workers-diff && npm install
(cd ../../workers/cron && worker-build --release)
node cron.mjs                     # 고친 원본 vs Rust — 모두 SAME 이어야 한다. node cron.mjs verdict_thresholds 로 하나만
ORIGINAL=raw node cron.mjs        # 고치지 않은 원본 vs Rust — 고친 버그가 닿는 시나리오(fixes 칸)만 DIFF 여야 한다
```

시나리오(`cron_scenarios.mjs`, 47개)는 `workers/cron/test/*.test.ts` 의 단언을 모두 옮긴 것(시나리오마다 `covers` 칸)과 가장자리입니다:
빈 대기열, lease, 20건 한도, 7일 보관, 답글 · 부모(기다림 · 거부 · 없음), Vectorize 장애 · 재실행, 욕설 · 음란 · 불법 사유, 결정 API 의 모양이 다른 응답 ·
JSON 아닌 본문 · 429 · 서버 오류 · fetch 거부 · 키 없음 · 주소 없음, 잘못된 벡터(길이 · TEXT · NULL), 다른 실행이 먼저 집은 행, 재시도 한도의 다섯 갈래
(stored · partial · 벡터만 · 확인 실패 · 되돌리기 실패), D1 장애(예외가 샘 · catch 가 잡음), 판정(기준선 세 범위 · 머무르기 · 게스트 · 작성자 · 최신 표 ·
경합 · sweep 감쇠 · 25건 sweep 과 피어 · 사라진 평가 · 낮은 근거 · 문턱과 단계 대체 · 대체됨 · 시각이 제각각인 표 400여 건), 지갑 타임트라이얼 정리.
`verdict.test.ts` · `verdict_v2.test.ts`(계약 사례)는 crate 의 시험(`cargo test -p premium-discount-verdict`)이 같은 계약 파일로 맡고,
`moderation.test.ts` · `vector.test.ts` 의 순수 함수 단언은 `workers/cron` 의 단위 시험으로도 옮겼습니다.

## 결과

| 무엇 | 결과 |
| --- | --- |
| 고치기 전 (56705a3): 원본 vs Rust | 44/44 동일 |
| 고친 뒤: 고친 원본 vs Rust (`node cron.mjs`) | 47/47 동일 |
| 고친 뒤: 고치지 않은 원본 vs Rust (`ORIGINAL=raw`) | 44 동일 · 3 다름 — 다른 것은 고친 버그가 닿는 `reject_update_fails` · `release_update_fails`(fix-return-await, 원본은 첫 실행이 예외로 끝남) · `verdict_prototype_kind`(fix-weight-own, 원본은 두 실행 모두 `NOT NULL constraint failed: eval_verdicts.evidence`) 뿐 |
| `libm-probe` (JS Math 대신 wasm libm) | `verdict_hysteresis` · `verdict_thresholds` · `verdict_decay_bits` DIFF (E · v · r 243칸 중 33칸이 마지막 자리) — 시험대가 비트 차이를 잡는다 |
| `cargo test -j 2` (workers) | common 12 + aws_eth 5 + pako_array 1, cron 12 통과 |
| `cargo test -p premium-discount-verdict` | 6 통과 (초월 함수 고리 시험 포함) |
| `cargo check --target wasm32-unknown-unknown` | 통과, 경고 없음 |
| node 시험 (cron · www) | 41 · 98 통과 (그대로) |
| 루트 `cargo metadata` | 성공, 멤버 21 (workers 는 빠짐) |
| `wrangler deploy --dry-run` (workers/cron) | 빌드 명령(worker-build 0.8.7) · 바인딩(DB · EVAL_VECTORIZE · DECISION_ENDPOINT) 확인 통과, 361.30 KiB (gzip 129.52 KiB) |

## 고친 버그

| id | 원본 자리 | 무엇이 틀렸나 → 고침 | 드러내는 시나리오 |
| --- | --- | --- | --- |
| fix-return-await | index.ts 101 · 104 · 109 · 119 (`processOne`) | try 안의 `return reject(…)` · `return release(…)` 를 기다리지 않아, 그 UPDATE 가 실패하면 catch 를 지나 scheduled 밖으로 샜다. 그 실행의 나머지 행 · 판정 · 지갑 정리가 모두 멈추고 그 행은 lease(5분)가 끝날 때까지 processing 에 묶였다 → `return await`. 이제 실패는 그 행의 재시도(pending, last_error)로 남고 실행은 이어진다 | `reject_update_fails` · `release_update_fails` |
| fix-weight-own | index.ts 386 (`computeVerdict`) | `PARAMS.weights[v.kind] ?? 0` 이 프로토타입을 거쳐 `constructor` · `toString` 등에 함수를 돌려줘 E · v 가 NaN → eval_verdicts 의 NOT NULL 에 걸려 processVerdicts 가 던졌다. 표시가 지워지지 않아 **그 뒤 모든 실행이 같은 자리에서 실패**(판정 · 지갑 정리 멈춤) → 자기 속성만 본다(`Object.hasOwn`, Rust 는 crate 의 `Params::weight`). www 는 vote · vote_outcome 만 받으므로 www 를 거치지 않고 들어간 표에서만 생긴다 | `verdict_prototype_kind` |

수정마다 Rust 와 `scripts/workers-diff/fixes/cron.mjs` 의 글 패치(같은 id, 공백 무시 · 정해진 횟수만큼 맞아야 적용)가 짝입니다.

## 알려진 차이 (관찰되지 않거나 www 가 막는 것)

- 표 값(`eval_votes.value`)은 crate 의 `Obs.sign`(i32)으로, 시각(`created_at` · `tier_since`)은 i64 로 읽습니다. www 는 값 1 · −1 · 0 과 `Date.now()` 만
  넣으므로 같지만, 정수가 아닌 값이 D1 에 직접 들어가면 원본(수 그대로)과 다릅니다.
- `eval_verdicts.tier` 가 네 등급 밖의 글이면(cron 만 쓰므로 생기지 않음) crate 에는 unrated 로 넘기고 since 는 원본처럼 now 로 둡니다.
- 글의 짝 없는 서로게이트(잘못된 UTF-16)는 Rust 문자열이 담지 못해 U+FFFD 가 됩니다(결정 API 본문의 `state`). www 가 받은 JSON 글에서만 생길 수 있습니다.
- 우리가 직접 던지는 TypeError(속성 읽기)의 글은 V8 과 같게 맞췄지만, 모든 자리를 시험하지는 않았습니다(예: `for (const { id } of …)` 의 구조 분해 오류 글).
- 원본 TS 의 처리기 시험(`workers/cron/test`)은 TS 판을 시험합니다. Rust 판은 차분 시험대와 단위 시험이 맡습니다.
