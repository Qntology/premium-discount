# Cloudflare Workers → workers-rs (2026-11-19)

`workers/cron`(premium-discount-cron)의 TypeScript 를 Rust(workers-rs 0.8.7, wasm32)로 다시 썼습니다. 방식은 logis-center 의 것
(`docs/workers-rs.md`, 커밋 370d67a3)과 같습니다. 목표는 **원본과 같은 동작**입니다: 같은 SQL, 같은 D1 호출 순서, 같은 JSON(키 순서 · 수 표기),
같은 예외 경계(무엇을 잡고 무엇이 새는지). 원본의 버그까지 그대로 옮겨 같음을 확인한 뒤(커밋 56705a3) 버그를 고쳤습니다(아래 "고친 버그").
원본은 커밋 `0538044` 의 `workers/cron/src/*.ts` 입니다. www 워커도 두 단계로 옮겼습니다: 1단계(뼈대 · 세션 · 인증 · 요청 제한 ·
레거시 처리기 · 차분 시험대)와 2단계(큰 모듈 넷: evaluations · navtrials · graph · analytic). 그 뒤 www 에 보안 · 경합 고침 둘(지갑 서명 한 번만 쓰기 ·
타임트라이얼의 조건 건 쓰기)을 더했습니다 (아래 "www" 절 · "고친 버그").

## 구조

```
workers/                    자기 Cargo 작업 공간 (wasm32). 루트 Cargo.toml 의 exclude = ["workers"]
├─ Cargo.toml  .cargo/      worker 0.8.7 고정, 산출물 workers/target, wasm 의 getrandom 백엔드(wasm_js)
├─ migrations/              D1 스키마 (www · cron 이 같이 씀). 0010_wallet_nonces 는 fix-wallet-replay 가 더함
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
└─ www/                     premium-discount-worker-www (fetch). 아래 "www" 절
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
cargo test -j 2                                         # common(12 + aws_eth 5 + pako 1) · cron(12) · www(41 + contracts 8)
cargo check -j 2 --target wasm32-unknown-unknown        # 경고 없음
cd cron && worker-build --release                       # build/index.js(20 KiB) · index_bg.wasm(329 KiB)
```

배포: `cd workers/cron && npx wrangler deploy` — `build.command` 가 `cargo install -q worker-build@0.8.7 --locked && worker-build --release` 입니다.
배포 PC 에 Rust 와 `wasm32-unknown-unknown` 표적이 있어야 합니다. workers-rs 0.9 는 `cloudflare:workers` 의 `tracing.getActiveSpan()` 을 부르는데 지금 Miniflare/workerd 에
없어 0.8.7 로 고정했습니다(logis-center 와 같음).

## 차분 시험대 (`scripts/workers-diff`)

원본(`git show 0538044:workers/cron/src/*.ts` 를 esbuild `platform: "browser"` 로 묶음)과 Rust 빌드(`workers/cron/build`)를 Miniflare 에서 같은 조건으로 돌립니다:

- 같은 D1: `workers/migrations` 0001 ~ 0010 을 이름순으로 적용하고 같은 씨앗 행을 넣음 (`test-support/fakes.ts` 의 seedQueue · seedEvaluation · seedVote 와 같은 기본값).
- 같은 시계: 진입 모듈이 `scheduledTime` 으로 `Date.now()` 를 맞춤(`prelude.mjs`, UTC Date · 씨앗 있는 Math.random 은 logis-center 그대로).
- 진입 모듈이 `env` 를 Proxy 로 감쌈(`mocks_src.mjs`): **D1** 은 진짜 Miniflare D1 을 감싸 문장마다 (SQL · 묶은 값 · 방법)을 시험대에 알리고, 시험대는
  장애(`faults`)나 실행 뒤 끼워 넣을 문장(`race`: www 가 그 사이 표를 넣는 경합)을 돌려줌. (www 시험대가 더한 `before` · batch 의 `after` 는 cron 이 쓰지 않는다.) **Vectorize** 는 시험대가 저장소 · 장애를 쥔 가짜
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

## www (2026-11-19)

원본은 커밋 `35781c0` 의 `workers/www/src/*.ts` 와 그것이 가져다 쓰는 `workers/cron/src/verdict.ts` · `navtrials.ts` 입니다. 두 단계로 옮겼습니다:
1단계는 crate 뼈대 · 처리기 차례 · 작은 모듈 · 레거시 처리기 · 차분 시험대이고, 2단계는 큰 모듈 넷(evaluations · navtrials · graph · analytic)을
파일 하나씩 갈아 끼운 것입니다. 옮기면서 찾은 원본 버그 일곱(1단계 하나 · evaluations 하나 · analytic 다섯)을 고쳤고, 그 뒤 보안 · 경합 고침 둘
(fix-wallet-replay · fix-nav-lost-update)을 더했습니다. 이 둘은 D1 호출 자체가 원본과 다릅니다(새 표 `wallet_nonces` · 조건 건 쓰기). 아래 "고친 버그".

### 구조 (`workers/www`, premium-discount-worker-www)

| Rust | 원본 | 내용 |
| --- | --- | --- |
| `lib.rs` | — | 진입점: worker-macros 0.8.7 의 `#[event(fetch)]` 와 같은 꼴의 `fetch` 내보내기를 직접 쓴다. `#[event(fetch)]` 는 처리기 오류를 500 응답으로 삼키지만, 원본은 처리기의 try 밖에서 던진 예외가 fetch 의 거부로 새므로 같은 이름 · 글의 JS 예외로 거부한다(cron 의 `scheduled` 와 같은 방식) |
| `handler.rs` | index.ts 370~404 | session → evaluations → navtrials → analytic → graph → legacy. 앞에서 하는 일은 없다(요청을 그대로 넘김) |
| `legacy.rs` | index.ts 405~1128 | 늘 418 (아래) |
| `session.rs` · `ratelimit.rs` · `projection.rs` · `blob.rs` · `render.rs` | session · ratelimit · projection · blob · renderHtml | 옮김 (1단계) |
| `auth.rs` | auth.ts | 세션 확인(S3 hash 객체) · 앱 세션 발급 · 지갑 서명 확인(EIP-191, k256 로 복원). 지갑 서명을 받는 경로는 모두 `verify_wallet_request_once`(서명 확인 + 한 번만 쓰기, fix-wallet-replay)를 쓴다 |
| `util.rs` | evaluations.ts 의 `corsHeaders` · `ValidationError` · `parseDomain` · `parseUuid` · `verdictSummaries` | 여러 모듈이 같이 쓰는 것 (CORS · JSON 응답 · IP · pathname · D1 의 바뀐 행 수, 그리고 graph.ts 가 evaluations.ts 에서 가져다 쓰던 검사 · 판정 요약 — evaluations.rs 와 graph.rs 가 서로의 파일을 건드리지 않게) |
| `evaluations.rs` | evaluations.ts | 저장(대기열) · 검색(앱 · 공개, tier) · status · 투표 · 대체 보고 · 판정 조회. RECIPE 는 premium-discount-verdict 의 RECIPE_V1 (2단계) |
| `navtrials.rs` | navtrials.ts | /nav/trials (GET · POST · DELETE, 세션 또는 지갑). cron 의 navtrials.ts 에서 가져다 쓰던 상수 · dayOf · walletOldestDay · tier 는 사본. 쓰기는 조건 건 쓰기(fix-nav-lost-update, 파일 머리말) (2단계) |
| `graph.rs` · `graph/view.rs` | graph.ts | /graph · /graph/local · /graph/similar · /graph/view (VIEW_HTML 은 원본 처리기로 뽑은 원시 문자열) (2단계) |
| `analytic.rs` · `analytic/event.rs` · `json.rs` · `inflate.rs` | analytic.ts | /analytic/events (수집 batch · 주인 읽기 · 지우기) · /analytic/sites (지갑 · 세션 주인, 첫 화면 meta 확인). 받은 본문은 JS 의 JSON.parse · pako ungzip 과 같이 받아들인다 (2단계) |

**이음매**: 네 모듈은 모두 `#[cfg(target_arch = "wasm32")] pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>>` 하나를 낸다.
원본 `handleX(request, env): Promise<Response | null>` 과 같은 뜻이다: `Ok(Some)` = 응답, `Ok(None)` = `return null`(다음 처리기로),
`Err(Throw)` = 거부(try 밖 예외 → fetch 밖으로). `req` 는 handler 가 미리 읽은 요청 전부(메서드 · 주소 · 머리글 · cf · 본문 바이트)이고,
원본의 기본 인자 `nowMs = Date.now()` 는 모듈 안에서 `platform::now()` 로 읽는다.

**레거시 처리기**: 모듈이 모두 null 을 돌려준 요청은 어떤 것이든 418 "I'm a teapot!" 으로 끝난다. 572 의 `hashId(cookies.cc + pathname)` 에서
`cookies` 는 어디에도 선언되지 않아 ReferenceError 이고, id 가 주소면 630 의 `env['premium_dicount__items']`(zoneRegion 이 '' 라 이런 이름,
어느 설정에도 없다)가 TypeError, 바인딩이 있더라도 인자의 `${cookies.team}` 이 ReferenceError 다. 그래서 631~1120(페이지 · 작업 트리,
PUT · POST(update_from_rust · 링크 등록) · GET(목록 · renderHtml) · OPTIONS)은 어떤 바인딩 · 요청으로도 실행될 수 없어 옮기지 않았고
(`legacy.rs` 머리말), 그 앞(URIError · `new URL` · CSP 머리글 · ReferenceError · TypeError 의 차례와 console 기록)은 그대로 옮겼다.
머리글 검사와 주소 해석은 원본과 같은 런타임 객체(`Headers` · `URL`)를 불러 같은 오류 글을 낸다. `renderHtml` 은 실행될 수 없는 GET 갈래에서만
불리지만 옮겨 두고, 원본 함수로 뽑은 벡터(`workers/www/tests/render_vectors.json`, `scripts/workers-diff/www_render_vectors.mjs`)와 바이트를 견준다
(원본 그대로: 템플릿 리터럴의 `"\f0f3"` 은 폼 피드 + "0f3").

**wrangler**: `workers/www/wrangler.json` 은 `main: build/index.js` · worker-build 0.8.7 빌드 명령으로 바꿨고, 바인딩(D1 둘 · Vectorize · AI · 요청 제한 셋 ·
R2 · assets `../../web/www/`) · `migrations_dir: "../migrations"` · vars 는 그대로다. TS 번들에만 뜻이 있는 `upload_source_maps` · `rules`(*.html 을 글로)는 뺐다.
`wrangler.test.json`(시험 배포 premium-discount-test, 원격 Vectorize)도 같은 방식으로 Rust 빌드를 가리키게 했고 바인딩은 그대로다.
TS 원본(`src/*.ts`)은 남겼다: 시험대가 이력에서 원본을 꺼내고 node 시험 98개가 그대로 돈다(TS 원본을 시험하므로 이번 고침 둘은 들어가지 않는다).
**배포 전에 D1 에 `0010_wallet_nonces.sql` 을 적용한다**(`wrangler d1 migrations apply`): 표가 없으면 지갑 서명 요청이 D1 오류로 500 이 된다.
`wrangler deploy --dry-run` 은 1단계 때 1709.87 KiB (gzip 598.96 KiB), 바인딩 확인 통과였다(2단계 뒤로는 다시 재지 않았다).

### 차분 시험대 (`scripts/workers-diff/www.mjs`)

```
(cd ../../workers/www && worker-build --release)
node www.mjs                                  # 고친 원본 vs Rust — 모두 SAME 이어야 한다
node www.mjs session_issue,auth_verified      # 이름으로 (ONLY=… 도 됨)
MODULE=navtrials node www.mjs                 # 모듈로: session · auth · legacy · evaluations · navtrials · graph · analytic · wallet (쉼표로 여럿)
MODULE=wallet ORIGINAL=raw node www.mjs       # 고치지 않은 원본 vs Rust — fixes 칸이 있는 시나리오만 DIFF 여야 한다 (MODULE · ONLY 와 함께 써도 된다)
```

- 원본: `git show 35781c0:<경로>` 로 www 의 .ts 11개와 cron 의 verdict.ts · navtrials.ts 를 `out/www-src/` 에 같은 나무 모양으로 꺼내(esbuild 가 `../../cron/src/…` 를
  따라가게) `fixes/www*.mjs`(www · www_analytic · www_evaluations · www_graph(빈 목록) · www_security)를 덧대고 묶는다. ethers · pako · aws4fetch 는 시험대의 node_modules 에서.
- 같은 조건: D1 `DB`(migrations 0001~0010 을 이름순으로) · `apac1_premium_discount`(빈 DB), 씨앗 행(`sql`), 정적 파일 `web/www`(assets, 워커 앞에서 답함),
  변수 · 비밀(`bindings`), 요청마다 가상 시계(`x-harness-clock` 머리글 — 진입 모듈이 시계를 맞추고 지운다), 씨앗 있는 `Math.random`(prelude) ·
  `crypto.getRandomValues` · `crypto.randomUUID`(www_mocks_src.mjs; Rust 의 getrandom 도 부를 때마다 `globalThis.crypto.getRandomValues` 를 찾는다).
- 가짜 바인딩(`www_mocks_src.mjs`, cron 의 mocks_src.mjs 위에): D1 문장마다 · batch 를 시험대에 알림(장애 주입 `faults`), 요청 제한 바인딩 셋
  (`limits` 에 이름이 있을 때만 존재, 답은 통과 · 막힘 · 던짐 · 값), Vectorize(`vec.<op>` 로 답을 바꿀 수 있음), S3(aws4fetch 의 서명을 같은 x-amz-date 로
  aws4fetch AwsV4Signer 가 다시 계산해 `sig: ok|bad` — logis-center commerce.mjs 와 같은 방식, 답은 `s3({ method, path, n })`), 그 밖의 바깥 fetch(`fetch(...)`).
- **경합(`race`)**: 맞는 문장(`match` 글 · `op`)을 실행한 뒤(`after`) 또는 실행하기 바로 전(`before`, batch 에도) 같은 DB 에 끼워 넣을 문장 — 다른 요청이
  그 사이 쓴 것을 흉내 낸다(evaluations 의 같은 request_id, fix-wallet-replay 의 같은 서명 둘, fix-nav-lost-update 의 동시 쓰기). 칸마다 `times` 번까지
  (기본 1), 여럿이면 목록으로 준다. `before` 와 batch 의 `after` 는 mocks_src.mjs 에 더했고 cron 시험대는 쓰지 않는다(47/47 그대로).
- 견주는 것: 응답(상태 · 정렬한 머리글 · Set-Cookie · 본문은 JSON 이면 값으로), 요청마다의 console 기록 · D1 결과(바뀐 행 수까지) · 밖으로 샌 예외, 바깥 호출 차례
  (D1 · batch · 요청 제한 · Vectorize · S3(머리글 · 서명 포함) · fetch), D1 끝 상태(모든 표), Vectorize 끝 상태. 요청은 쿠키 상자(`jar`)를 쓸 수 있고,
  머리글 · 본문 · 경로는 앞 응답의 함수일 수 있다(발급받은 세션으로 상태 묻기). gzip 바이트(D1 의 묶은 값 · 끝 상태 · 응답의 base64)는 풀어서 견준다.
- 무작위: 씨앗 덕분에 앱 세션 hash · token 도 두 판이 같아 그대로 견준다. 정규화(한쪽에만 있는 40 · 64자리 16진 값 → `<random#n>`)는 기본으로 끄고
  `random: true` 시나리오(또는 `NORMALIZE=1`)에서만 켠다. **정규화는 그 값에서 계산한 것끼리의 차이를 가린다**: 켜고 `ORIGINAL=raw` 로 돌리면
  fix-web-session-email-trim(주소 하나만 다름)도 SAME 으로 보인다. 그래서 바꾼 값이 있으면 줄 끝에 `normalized=N` 을 찍고, 앱 세션 발급은 정규화와
  상관없이 각 쪽에서 token = HMAC(SESSION_SECRET, "session|" + hash) · address · mailto 를 따로 확인해 `check` 칸에 남긴다.
- 시나리오는 모듈마다 `scenarios/www_<모듈>.mjs`(모양은 `scenarios/www_common.mjs` 머리말)에 있고 `www_scenarios.mjs` 가 모은다.
  씨앗 · 서명 도구(`evaluation` · `unitVector(Hex)` · `walletHeaders` · `walletMessage` · `walletNonce`)는 www_common.mjs 에 있다.

| 모듈 | 시나리오 | 옮긴 node 시험 · 다룬 것 |
| --- | --- | --- |
| session | 23 | session.test.ts (마지막 단언 "로그인한 앱이 저장하면 from 이…" 은 evaluations 에서) |
| auth | 14 | auth.test.ts — verifiedAddress 의 단언을 GET /session 으로 |
| legacy | 10 | 레거시 처리기(418 의 차례 · console) · assets. tracker.test.ts 의 tracker.js 는 워커 앞의 assets 가 그대로 낸다(`legacy_assets_first`) |
| evaluations | 73 | evaluations · votes · supersedes(www 쪽) 시험과 session.test.ts 마지막 단언, SCENARIOS.md 의 R · V · GA · ID · PR · SR · RL, 가장자리(BOM · 잘못된 UTF-8 · 중복 키 · -0 표 · Vectorize 장애 · D1 장애 · 경합) |
| navtrials | 45 | navtrials.test.ts 의 단언 전부와 가장자리 38, fix-nav-lost-update 의 경합 · 장애 7 (`race_*` · `batch_fault_releases_registration`) |
| graph | 28 | graph.test.ts, SCENARIOS GR-01~16 · R-06~08 · RL-04 · RL-05, premium-discount-mcp 의 GET /graph* 요청 그대로 |
| analytic | 26 | analytic.test.ts, tracker.js 의 실제 요청, 가장자리(본문 형식 · gzip · 크기 · 요청 제한 · CORS · 메서드 · 주인 인증 · 커서 · D1 장애), 고친 버그 6 |
| wallet | 8 | fix-wallet-replay: 같은 요청 · 다른 쿼리 · 새 서명 · 기한 경계 · 다른 서명 꼴 · 동시 둘 · 기한 지난 키 지우기 · D1 장애 (navtrials · analytic 의 지갑 경로) |

wallet.test.ts(계약 · 위조 · 5분)는 `workers/www/tests/contracts.rs` 가 같은 계약(contracts/wallet_v1.json)과 ethers 가장자리(v 0 · 1 · 35 이상, 높은 s)로 맡고,
한 번만 쓰기 키(서명한 글의 SHA-256 · 기한 · 쿼리와 서명 꼴에 상관없음)도 거기서 본다.

**지갑 서명을 다시 쓰던 시나리오**: fix-wallet-replay 뒤로 같은 서명을 두 번 받던 시나리오는 요청마다 새로 서명하게 바꿨다(덮는 단언은 그대로) —
`www_navtrials_delete`(두 번째 DELETE 를 1 ms 뒤 서명), `www_navtrials_mixed_request`(두 번째 앱의 GET), analytic 의 `owner()` 가 부를 때마다 서명 시각을
1 ms 늘림(`sites_wallet` · `events_read`(다음 쪽 셋도 쪽마다 새 서명) · `owner_edges` · `since_infinity`). `www_navtrials_auth_and_format` 는 같은 글을 셋
보내지만 받는 것은 하나라(본문이 다름 · 5분 지남) 그대로다. 다시 쓰면 막히는 것은 `www_wallet_*` 가 본다.

### 결과

| 무엇 | 결과 |
| --- | --- |
| 고친 원본 vs Rust (`node www.mjs`) | 227/227 동일 (session 23 · auth 14 · legacy 10 · evaluations 73 · navtrials 45 · graph 28 · analytic 26 · wallet 8) |
| 고치지 않은 원본 vs Rust (`ORIGINAL=raw`) | 165 동일 · 62 다름 — 다른 것은 모두 fixes 칸이 있는 시나리오(예상 밖 0). 앞 고침들의 10 (session_web_email_padded · session_states · save_address_forms · search_to_forms · analytic `fix_*` 6)에 fix-wallet-replay 28(새 8 + 지갑 서명을 받는 navtrials 13 · analytic 7) · fix-nav-lost-update 35(새 7 + navtrials 28)가 더해졌다(둘 다인 것 · 앞 고침과 겹치는 것이 있다) |
| 위 62 중 이번 고침 둘만 닿는 기존 시나리오 37 (10 + 새 15 + 37 = 62) | 원본과 응답(상태 · 머리글 · 본문) · console · D1 끝 상태(wallet_nonces 빼고)가 모두 같다 — 다른 것은 D1 호출뿐이다. 하나만 끝 상태가 다르다: `stored_rows_odd`(D1 에 직접 넣은 premium `'x'` 를 원본 JS 는 "x1", 고친 판은 SQL 산술로 1) |
| cron 시험대 (`node cron.mjs`, mocks 에 before · batch after 를 더한 뒤) | 47/47 동일 |
| `cargo test -j 2` (workers) | common 12 + aws_eth 5 + pako_array 1, cron 12, www 단위 41 + contracts 8 (한 번만 쓰기 키 시험 더함) |
| `cargo check --target wasm32-unknown-unknown` | 통과, 경고 없음 |
| node 시험 (www · cron, TS 원본) | 98 · 41 통과 (그대로) |

### 알려진 차이 · 남겨 둔 것

- `request.cf.country` 가 글이 아닌 참 값이면 원본은 try 밖에서 TypeError 로 던진다. 그 글(`(geoInfo.country || "").toLowerCase is not a function`)은 V8 이
  원본 식을 찍은 것이라 Rust 는 같은 글을 직접 낸다(시험대 `legacy_cf_and_ip` 에서 같음). Cloudflare 는 늘 글을 준다.
- `blob.rs` 의 `decode_blob` 은 DataView · Float32Array 같은 다른 TypedArray 를 바이트로 보지 않는다(js::to_v 가 Uint8Array · ArrayBuffer 만 바이트로 둔다). D1 은 숫자 배열을 준다.
- S3 가 5xx · 429 를 주면 aws4fetch 기본값대로 10번 더 시도한다(50ms × 2^i × 난수, 합 최대 약 51초) — `session_s3_errors` 에서 GET /session 한 번이
  S3 를 11번 부르고 가상 시계로 약 37초 걸린다. 세션 확인을 쓰는 모든 요청(평가 · 투표 · 타임트라이얼)이 S3 장애 때 그만큼 늦어진다.
  동작 설계의 문제라 고치지 않고 원본 그대로 두었다(바꾼다면 `retries` 를 줄이는 것).
- wasm 이 cron(329 KiB)보다 크다(지금 2.03 MiB, gzip 약 739 KiB — 1단계 때 1.7 MiB): k256(서명 복원) · url(idna) 과 2단계 모듈 때문이다. 무료 요금제 한도(압축 3 MB) 안이다.
- graph `/graph/local` 은 스레드의 가장 오래된 200건(ASC)만 준다: 200건 넘는 스레드에서 새 평가를 고르면 그 평가가 응답에 없을 수 있다(`graph_limit_200`). 동작 결정이 필요해 원본 그대로.
- graph `/graph/similar` 는 D1 을 확인하지 않아, D1 행이 없는 벡터도 판정 없는(unrated) 노드로 나온다(SCENARIOS GR-13, `graph_similar_phantom`). 원본 그대로.
- evaluations 의 같은 `request_id` 다시 보내기는 cc · ref 만 견준다: 글이 다르면 첫 항목으로 200 을 돌려준다(주석은 409 라고 한다). 원본 그대로.
- www 의 navtrials 상수(NAV_FIRST_DAY · NAV_RECIPE · TRIAL_POLICY · TRIAL_SCHEMA · dayOf · walletOldestDay · tier)는 cron 의 navtrials 에서 가져다 쓰던 것의 사본이다
  (cron crate 를 의존하면 그 `scheduled` 내보내기가 www 에 들어온다). 두 곳을 같이 고쳐야 한다.
- analytic 의 gzip 바이트는 pako 와 다르다(Rust 는 flate2): 같은 글을 다른 바이트로 압축한다. 시험대는 풀어서 견준다. 받는 쪽(앱 · crates/premium-discount-analytic)은 풀어 읽으므로 상관없다.
- JSON 입력의 짝 없는 서로게이트는 Rust 문자열이 담지 못해 U+FFFD 가 된다(analytic 의 저장 JSON 은 JS 와 같은 `\udXXX` 로 따로 맞췄다).
- **fix-wallet-replay 의 결과**: 같은 지갑 · 같은 ms · 같은 메서드 · 경로 · 본문의 요청 둘(예: 쿼리만 다른 GET 둘을 한꺼번에)은 서명 글이 같아 둘째가 401 이다.
  계약이 쿼리를 서명하지 않는 한 서버는 이것을 다시 쓰기와 가를 수 없다 — 클라이언트는 요청마다 새로 서명해야 한다(같은 ms 면 시각을 1 ms 늘림).
  서명은 확인되는 순간 소모되므로, 그 뒤 500(비밀 없음 · D1 오류)이 나도 같은 서명으로 다시 보내면 401 이다(다시 시도는 새 서명으로).
  지금 이 저장소(apps/core 는 세션 머리글만 보냄)와 raon-zena(premium-discount-identity 를 의존하지만 `sign_request` 를 부르지 않음)에는 지갑 서명 요청을
  보내는 클라이언트가 없어 다시 시도 문제는 아직 없다. 키는 서명 창(서명 시각 + 5분) 뒤로 60초 더 두고 지운다
  (`WALLET_NONCE_GRACE_MS`): 시계가 조금 늦은 다른 isolate 가 막 지운 키의 서명을 받는 틈을 막는 여유이고, 받는지는 여전히 시각 검사(±5분)만 정한다.
  그만큼 표에 남는 행이 늘어난다(최근 약 11분의 지갑 요청 수).
- **fix-nav-lost-update 의 결과**: 다시 시도를 다 쓰면 500 이지만 batch 의 나머지(콜렉션 · 등록 · 판정 수 · 벡터)는 이미 쓰였다 — 같은 런을 다시 보내면
  duplicate 라 합계에는 그 런이 끝내 들어가지 않는다(`race_retry_exhausted`). 등록 되돌리기는 batch 가 던졌을 때 하므로, D1 이 batch 를 실제로는 쓰고
  답만 잃은 경우에는 되돌리기가 틀린다(그 사용자의 다음 런이 한 번 더 셀 수 있다). 밀어내기를 다시 할 때는 이 요청이 쓴 행을 빼고 가장 오래된 것을 고른다.
  D1 에 직접 넣은 숫자 아닌 수(`premium = 'x'`)는 이제 SQL 산술이다(원본 JS 는 `"x" + 1` = "x1", 고친 판은 1 — `stored_rows_odd`).

## 고친 버그

| id | 원본 자리 | 무엇이 틀렸나 → 고침 | 드러내는 시나리오 |
| --- | --- | --- | --- |
| fix-return-await | index.ts 101 · 104 · 109 · 119 (`processOne`) | try 안의 `return reject(…)` · `return release(…)` 를 기다리지 않아, 그 UPDATE 가 실패하면 catch 를 지나 scheduled 밖으로 샜다. 그 실행의 나머지 행 · 판정 · 지갑 정리가 모두 멈추고 그 행은 lease(5분)가 끝날 때까지 processing 에 묶였다 → `return await`. 이제 실패는 그 행의 재시도(pending, last_error)로 남고 실행은 이어진다 | `reject_update_fails` · `release_update_fails` |
| fix-weight-own | index.ts 386 (`computeVerdict`) | `PARAMS.weights[v.kind] ?? 0` 이 프로토타입을 거쳐 `constructor` · `toString` 등에 함수를 돌려줘 E · v 가 NaN → eval_verdicts 의 NOT NULL 에 걸려 processVerdicts 가 던졌다. 표시가 지워지지 않아 **그 뒤 모든 실행이 같은 자리에서 실패**(판정 · 지갑 정리 멈춤) → 자기 속성만 본다(`Object.hasOwn`, Rust 는 crate 의 `Params::weight`). www 는 vote · vote_outcome 만 받으므로 www 를 거치지 않고 들어간 표에서만 생긴다 | `verdict_prototype_kind` |
| fix-web-session-email-trim | **www** auth.ts 162 (`checkSession`) | 웹 세션은 email 을 trim 해서 확인(비었나)하고는 `obj.email` 을 그대로 돌려줬다. 앱 세션은 trim 한 값을 돌려주므로, S3 객체의 email 에 앞뒤 공백 · 줄바꿈이 붙으면 같은 사람이 두 갈래에서 **다른 주소**(computeAddress(hashMessage(email)))가 되고, 그 주소가 평가 · 투표 · 타임트라이얼의 from 이 된다 → 확인에 쓴 trim 한 email 을 돌려준다 | `session_web_email_padded` |
| fix-eval-address-normalize | www evaluations.ts 91 (`parseAddress`) | `ethers.isAddress` 를 지난 글을 `value.toLowerCase()` 로만 바꿔 0x 없는 40자리 · ICAP `XE…` 가 그대로 from · to · bcc 와 검색의 to 가 되어 같은 주소가 두 꼴로 저장(0x 없는 ZeroAddress 는 검사를 빠져나감) → `ethers.getAddress(value).toLowerCase()` | `save_address_forms` · `search_to_forms` |
| fix-analytic-gzip-cap | www analytic.ts 217 (`readBody`) | gzip 본문을 상한 없이 풀어 256 KB gzip 이 약 250 MB 로 풀림(메모리 한도 128 MB) → 푼 크기 1 MiB 넘으면 413 | `fix_gzip_cap` · `fix_gzip_bomb` |
| fix-analytic-sites-preflight | analytic.ts 381 | POST /analytic/sites 사전 요청에 수집용 CORS 를 돌려줘 브라우저에서 주인 등록이 막힘 → /analytic/sites 는 늘 주인 CORS | `fix_sites_preflight` |
| fix-analytic-empty-host | analytic.ts 179 | 호스트 빈 주소가 `hashId(null)` TypeError 500 → 400 | `fix_empty_host` |
| fix-analytic-sid-surrogate | analytic.ts 178 · 250 | 64자로 자른 sid 가 서로게이트 가운데면 hashId 가 던져 500 → 400 | `fix_sid_surrogate` |
| fix-analytic-list-limit | analytic.ts 358 | `limit=2.5` 가 그대로 LIMIT 에 묶여 500 → Math.floor | `fix_list_limit` |
| fix-wallet-replay | **www** auth.ts 209 (`verifyWalletRequest`) · navtrials.ts 623 · analytic.ts 393 | 지갑 서명 글(contracts/wallet_v1.json)에는 경로만 있고 쿼리가 없는데 같은 서명을 다시 쓰는 것을 막지 않아, 잡은 서명을 5분 안에 다른 쿼리로 다시 보낼 수 있었다: `DELETE /nav/trials?app=X` 를 `DELETE /nav/trials` 로 보내면 **콜렉션 전부**가 지워지고, GET 은 쿼리만 바꿔 다른 앱의 내 콜렉션 · analytic 주인 읽기를 할 수 있었다 → 계약은 그대로 두고 서버가 받은 서명을 기억한다(`verify_wallet_request_once`): 서명 확인이 맞으면 서명한 글의 SHA-256 을 서명 시각 + 5분(시각 검사가 받는 마지막 때) + 60초(isolate 사이 시계 차이 여유, 지우기에만 씀)까지 D1 `wallet_nonces`(migrations/0010)에 `INSERT … ON CONFLICT DO NOTHING` 으로 잡고, 바뀐 행이 0 이면(이미 쓴 서명 — 동시에 온 같은 요청 둘도 하나만 1) 틀린 서명과 같은 401. 같은 batch 에서 기한 지난 키를 10개까지 지운다(요청 하나가 하나를 넣으므로 표는 최근 약 11분의 지갑 요청 수로 묶인다). 키가 서명 바이트가 아니라 글이라 같은 글의 다른 서명 꼴(v 0 · 1, 대문자)도 막힌다. 닿는 경로: /nav/trials GET · POST · DELETE, /analytic/sites GET · POST · DELETE, /analytic/events 주인 GET · DELETE | `www_wallet_*` 8개 · 지갑 서명을 받는 navtrials 13 · analytic 7 시나리오(D1 호출 · wallet_nonces 만 다름) |
| fix-nav-lost-update | **www** navtrials.ts 412~533 (`storeRuns`) | nav_trials · nav_courses 를 읽은 값으로 계산한 절대값으로 upsert(`"runs" = excluded."runs"`, `"sums" = excluded."sums"` …)해, 같은 코스의 첫 등록 둘이 동시에 오면 runs 가 1 이 되고(하나를 잃음) 있는 코스 · 내 콜렉션의 동시 갱신도 하나를 잃었다. 같은 사용자의 같은 요청 둘은 등록을 SELECT 로 미리 봐 둘 다 처음 등록으로 세어 합계 · 판정 수(nav_counts)에 두 번 들어갔고, 밀어내기는 그 사이 다시 달린 코스를 지웠다 → 낙관적 동시성: 등록은 batch 앞에서 `INSERT … ON CONFLICT DO NOTHING`(바뀐 행 1 = 처음, 뒤에서 실패하면 되돌림), 수는 상대 갱신(`"runs" = "runs" + ?`) · last_day 는 `MAX`, 읽은 값에서 계산한 JSON(best · record · sums)과 밀어내기는 읽은 값(글 · touched)에 조건을 건 UPDATE · DELETE 로 batch 에 두고, 처음 넣는 행은 `ON CONFLICT DO NOTHING`. batch 결과에서 바뀐 행이 0 인 것만 다시 읽어 계산해 3번까지 다시 쓰고(첫 등록이 졌으면 갱신 갈래로, created 는 false), 다 쓰면 500 | `race_first_registration` · `race_existing_course_update` · `race_same_user_identical_runs` · `race_retry_exhausted` · `race_trial_row_update` · `race_eviction_retouched` · `batch_fault_releases_registration` · POST 가 쓰기까지 가는 navtrials 시나리오 28개(D1 호출만 다름, `stored_rows_odd` 는 끝 상태도) |

수정마다 Rust 와 `scripts/workers-diff/fixes/*.mjs`(cron · www · www_analytic · www_evaluations · www_security)의 글 패치(같은 id, 공백 무시 · 정해진 횟수만큼
맞아야 적용, 정규식이면 그대로)가 짝입니다. www 의 마지막 둘(fix-wallet-replay · fix-nav-lost-update)은 원본에 없는 D1 호출을 더하므로 `ORIGINAL=raw` 에서
지갑 서명을 받거나 타임트라이얼을 쓰는 시나리오가 모두 다르다(D1 호출만 — 응답 · 끝 상태는 "www · 결과" 의 확인대로 같다, `stored_rows_odd` 하나만 빼고).

## 알려진 차이 (관찰되지 않거나 www 가 막는 것)

- 표 값(`eval_votes.value`)은 crate 의 `Obs.sign`(i32)으로, 시각(`created_at` · `tier_since`)은 i64 로 읽습니다. www 는 값 1 · −1 · 0 과 `Date.now()` 만
  넣으므로 같지만, 정수가 아닌 값이 D1 에 직접 들어가면 원본(수 그대로)과 다릅니다.
- `eval_verdicts.tier` 가 네 등급 밖의 글이면(cron 만 쓰므로 생기지 않음) crate 에는 unrated 로 넘기고 since 는 원본처럼 now 로 둡니다.
- 글의 짝 없는 서로게이트(잘못된 UTF-16)는 Rust 문자열이 담지 못해 U+FFFD 가 됩니다(결정 API 본문의 `state`). www 가 받은 JSON 글에서만 생길 수 있습니다.
- 우리가 직접 던지는 TypeError(속성 읽기)의 글은 V8 과 같게 맞췄지만, 모든 자리를 시험하지는 않았습니다(예: `for (const { id } of …)` 의 구조 분해 오류 글).
- 원본 TS 의 처리기 시험(`workers/cron/test`)은 TS 판을 시험합니다. Rust 판은 차분 시험대와 단위 시험이 맡습니다.
