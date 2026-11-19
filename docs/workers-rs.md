# Cloudflare Workers → workers-rs (2026-11-19)

`workers/cron`(premium-discount-cron)의 TypeScript 를 Rust(workers-rs 0.8.7, wasm32)로 다시 썼습니다. 방식은 logis-center 의 것
(`docs/workers-rs.md`, 커밋 370d67a3)과 같습니다. 목표는 **원본과 같은 동작**입니다: 같은 SQL, 같은 D1 호출 순서, 같은 JSON(키 순서 · 수 표기),
같은 예외 경계(무엇을 잡고 무엇이 새는지). 원본의 버그까지 그대로 옮겨 같음을 확인한 뒤(커밋 56705a3) 버그를 고쳤습니다(아래 "고친 버그").
원본은 커밋 `0538044` 의 `workers/cron/src/*.ts` 입니다. www 워커는 두 단계로 옮기는 중입니다: 1단계(뼈대 · 세션 · 인증 · 요청 제한 ·
레거시 처리기 · 차분 시험대)는 끝났고, 큰 모듈 넷(evaluations · navtrials · graph · analytic)은 2단계입니다 (아래 "www" 절).

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
└─ www/                     premium-discount-worker-www (fetch) — 1단계. 아래 "www" 절
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
cargo test -j 2                                         # common(12 + aws_eth 5 + pako 1) · cron(12) · www(10 + contracts 7)
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

## www (1단계: 뼈대 · 세션 · 인증 · 레거시, 2026-11-19)

원본은 커밋 `35781c0` 의 `workers/www/src/*.ts` 와 그것이 가져다 쓰는 `workers/cron/src/verdict.ts` · `navtrials.ts` 입니다.
1단계는 crate 뼈대 · 처리기 차례 · 작은 모듈 · 레거시 처리기 · 차분 시험대이고, 큰 모듈 넷은 2단계에서 파일 하나씩 갈아 끼웁니다.

### 구조 (`workers/www`, premium-discount-worker-www)

| Rust | 원본 | 상태 |
| --- | --- | --- |
| `lib.rs` | — | 진입점: worker-macros 0.8.7 의 `#[event(fetch)]` 와 같은 꼴의 `fetch` 내보내기를 직접 쓴다. `#[event(fetch)]` 는 처리기 오류를 500 응답으로 삼키지만, 원본은 처리기의 try 밖에서 던진 예외가 fetch 의 거부로 새므로 같은 이름 · 글의 JS 예외로 거부한다(cron 의 `scheduled` 와 같은 방식) |
| `handler.rs` | index.ts 370~404 | session → evaluations → navtrials → analytic → graph → legacy. 앞에서 하는 일은 없다(요청을 그대로 넘김) |
| `legacy.rs` | index.ts 405~1128 | 늘 418 (아래) |
| `session.rs` · `auth.rs` · `ratelimit.rs` · `projection.rs` · `blob.rs` · `render.rs` | session · auth · ratelimit · projection · blob · renderHtml | 옮김 |
| `util.rs` | evaluations.ts 의 `corsHeaders` · `ValidationError` · `parseDomain` · `parseUuid` · `verdictSummaries` | 여러 모듈이 같이 쓰는 것 (CORS · JSON 응답 · IP · pathname, 그리고 graph.ts 가 evaluations.ts 에서 가져다 쓰던 검사 · 판정 요약 — evaluations.rs 와 graph.rs 가 서로의 파일을 건드리지 않게) |
| `evaluations.rs` · `navtrials.rs` · `graph.rs` · `analytic.rs` | 같은 이름의 .ts | **2단계** — 지금은 늘 `Ok(None)` |

**이음매**: 네 모듈은 모두 `#[cfg(target_arch = "wasm32")] pub async fn handle(req: &Req, env: &Env) -> R<Option<Resp>>` 하나를 낸다.
원본 `handleX(request, env): Promise<Response | null>` 과 같은 뜻이다: `Ok(Some)` = 응답, `Ok(None)` = `return null`(다음 처리기로),
`Err(Throw)` = 거부(try 밖 예외 → fetch 밖으로). `req` 는 handler 가 미리 읽은 요청 전부(메서드 · 주소 · 머리글 · cf · 본문 바이트)이고,
원본의 기본 인자 `nowMs = Date.now()` 는 모듈 안에서 `platform::now()` 로 읽는다. 공용 도구는 각 stub 의 머리말에 적었다.

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
TS 원본(`src/*.ts`)은 남겼다: 시험대가 이력에서 원본을 꺼내고 node 시험 98개가 그대로 돈다. **2단계가 끝나기 전에는 이 갈래로 배포하지 않는다**
(큰 모듈 넷의 경로가 레거시 418 로 떨어진다). `wrangler deploy --dry-run`: 1709.87 KiB (gzip 598.96 KiB), 바인딩 확인 통과.

### 차분 시험대 (`scripts/workers-diff/www.mjs`)

```
(cd ../../workers/www && worker-build --release)
node www.mjs                       # 고친 원본 vs Rust — 모두 SAME 이어야 한다
node www.mjs session_issue,auth_verified      # 이름으로 (ONLY=… 도 됨)
MODULE=session,auth node www.mjs   # 모듈로
ORIGINAL=raw node www.mjs          # 고치지 않은 원본 vs Rust — fixes 칸이 있는 시나리오만 DIFF 여야 한다
```

- 원본: `git show 35781c0:<경로>` 로 www 의 .ts 11개와 cron 의 verdict.ts · navtrials.ts 를 `out/www-src/` 에 같은 나무 모양으로 꺼내(esbuild 가 `../../cron/src/…` 를
  따라가게) `fixes/www.mjs` 를 덧대고 묶는다. ethers · pako · aws4fetch 는 시험대의 node_modules 에서 (package.json 에 더함).
- 같은 조건: D1 `DB`(migrations 0001~0009 를 이름순으로) · `apac1_premium_discount`(빈 DB), 씨앗 행(`sql`), 정적 파일 `web/www`(assets, 워커 앞에서 답함),
  변수 · 비밀(`bindings`), 요청마다 가상 시계(`x-harness-clock` 머리글 — 진입 모듈이 시계를 맞추고 지운다), 씨앗 있는 `Math.random`(prelude) ·
  `crypto.getRandomValues` · `crypto.randomUUID`(www_mocks_src.mjs; Rust 의 getrandom 도 부를 때마다 `globalThis.crypto.getRandomValues` 를 찾는다).
- 가짜 바인딩(`www_mocks_src.mjs`, cron 의 mocks_src.mjs 위에): D1 문장마다 · batch 를 시험대에 알림(장애 주입 `faults`), 요청 제한 바인딩 셋
  (`limits` 에 이름이 있을 때만 존재, 답은 통과 · 막힘 · 던짐 · 값), Vectorize(`vec.<op>` 로 답을 바꿀 수 있음), S3(aws4fetch 의 서명을 같은 x-amz-date 로
  aws4fetch AwsV4Signer 가 다시 계산해 `sig: ok|bad` — logis-center commerce.mjs 와 같은 방식, 답은 `s3({ method, path, n })`), 그 밖의 바깥 fetch(`fetch(...)`).
- 견주는 것: 응답(상태 · 정렬한 머리글 · Set-Cookie · 본문은 JSON 이면 값으로), 요청마다의 console 기록 · D1 결과 · 밖으로 샌 예외, 바깥 호출 차례
  (D1 · batch · 요청 제한 · Vectorize · S3(머리글 · 서명 포함) · fetch), D1 끝 상태(모든 표), Vectorize 끝 상태. 요청은 쿠키 상자(`jar`)를 쓸 수 있고,
  머리글 · 본문 · 경로는 앞 응답의 함수일 수 있다(발급받은 세션으로 상태 묻기).
- 무작위: 씨앗 덕분에 앱 세션 hash · token 도 두 판이 같아 그대로 견준다. 정규화(한쪽에만 있는 40 · 64자리 16진 값 → `<random#n>`)는 기본으로 끄고
  `random: true` 시나리오(또는 `NORMALIZE=1`)에서만 켠다. **정규화는 그 값에서 계산한 것끼리의 차이를 가린다**: 켜고 `ORIGINAL=raw` 로 돌리면
  fix-web-session-email-trim(주소 하나만 다름)도 SAME 으로 보인다. 그래서 바꾼 값이 있으면 줄 끝에 `normalized=N` 을 찍고, 앱 세션 발급은 정규화와
  상관없이 각 쪽에서 token = HMAC(SESSION_SECRET, "session|" + hash) · address · mailto 를 따로 확인해 `check` 칸에 남긴다.
- 시나리오는 모듈마다 `scenarios/www_<모듈>.mjs`(모양은 `scenarios/www_common.mjs` 머리말)에 있고 `www_scenarios.mjs` 가 모은다.
  2단계는 `www_evaluations.mjs` · `www_navtrials.mjs` · `www_graph.mjs` · `www_analytic.mjs`(지금은 빈 목록)만 채우면 된다.
  씨앗 · 서명 도구(`evaluation` · `unitVector(Hex)` · `walletHeaders`)도 www_common.mjs 에 있다.

1단계 시나리오가 보내는 경로는 `/session` · `/session/start` 와 레거시(모듈 경로가 아닌 모든 것: `/` · `/?href=…` · `/sessions` · `/SESSION` …)와 정적 파일뿐이다.
큰 모듈 넷의 경로(`/evaluations*` · `/public/evaluations/search` · `/nav/trials` · `/analytic/*` · `/graph*`)는 Rust 판이 아직 레거시로 넘겨 다르므로 보내지 않는다.

| node 시험 | 어디서 |
| --- | --- |
| session.test.ts | `www_session.mjs` (마지막 단언 "로그인한 앱이 저장하면 from 이…" 은 POST /evaluations 라 2단계) |
| auth.test.ts | `www_auth.mjs` — verifiedAddress 의 단언을 GET /session 으로 (1단계에서 세션 확인에 닿는 경로는 /session 뿐) |
| wallet.test.ts | `workers/www/tests/contracts.rs` — 지갑 확인은 navtrials · analytic 경로에서만 쓰여 시험대로는 2단계에서 닿는다. 같은 계약(contracts/wallet_v1.json)과 ethers 가장자리(v 0 · 1 · 35 이상, 높은 s)로 |
| tracker.test.ts | 워커가 아니라 브라우저 스크립트(web/www/static/js/tracker.js)다. 워커 앞의 assets 가 그대로 내보낸다(`legacy_assets_first`) |
| votes.test.ts · supersedes.test.ts | 모두 /evaluations 경로(투표 · 판정 조회 · 대체 보고)와 cron — 2단계 evaluations |

### 결과

| 무엇 | 결과 |
| --- | --- |
| 고친 원본 vs Rust (`node www.mjs`) | 47/47 동일 (session 23 · auth 14 · legacy 10) |
| 고치지 않은 원본 vs Rust (`ORIGINAL=raw`) | 46 동일 · 1 다름 — `session_web_email_padded`(fix-web-session-email-trim) 뿐 |
| cron 시험대 (`node cron.mjs`, mocks 에 batch 를 더한 뒤) | 47/47 동일 (그대로) |
| `cargo test -j 2` (workers/www) | 단위 10 + contracts 7 (지갑 계약 · ethers 가장자리 · blob 계약 · renderHtml 벡터) |
| `cargo check --target wasm32-unknown-unknown` | 통과, 경고 없음 |

### 알려진 차이 · 남겨 둔 것

- `request.cf.country` 가 글이 아닌 참 값이면 원본은 try 밖에서 TypeError 로 던진다. 그 글(`(geoInfo.country || "").toLowerCase is not a function`)은 V8 이
  원본 식을 찍은 것이라 Rust 는 같은 글을 직접 낸다(시험대 `legacy_cf_and_ip` 에서 같음). Cloudflare 는 늘 글을 준다.
- `blob.rs` 의 `decode_blob` 은 DataView · Float32Array 같은 다른 TypedArray 를 바이트로 보지 않는다(js::to_v 가 Uint8Array · ArrayBuffer 만 바이트로 둔다). D1 은 숫자 배열을 준다.
- S3 가 5xx · 429 를 주면 aws4fetch 기본값대로 10번 더 시도한다(50ms × 2^i × 난수, 합 최대 약 51초) — `session_s3_errors` 에서 GET /session 한 번이
  S3 를 11번 부르고 가상 시계로 약 37초 걸린다. 세션 확인을 쓰는 모든 요청(평가 · 투표 · 타임트라이얼)이 S3 장애 때 그만큼 늦어진다.
  동작 설계의 문제라 고치지 않고 원본 그대로 두었다(바꾼다면 `retries` 를 줄이는 것).
- wasm 이 cron(329 KiB)보다 크다(1.7 MiB, gzip 약 600 KiB): k256(서명 복원) · url(idna) 때문이다. 무료 요금제 한도(압축 3 MB) 안이다.

## 고친 버그

| id | 원본 자리 | 무엇이 틀렸나 → 고침 | 드러내는 시나리오 |
| --- | --- | --- | --- |
| fix-return-await | index.ts 101 · 104 · 109 · 119 (`processOne`) | try 안의 `return reject(…)` · `return release(…)` 를 기다리지 않아, 그 UPDATE 가 실패하면 catch 를 지나 scheduled 밖으로 샜다. 그 실행의 나머지 행 · 판정 · 지갑 정리가 모두 멈추고 그 행은 lease(5분)가 끝날 때까지 processing 에 묶였다 → `return await`. 이제 실패는 그 행의 재시도(pending, last_error)로 남고 실행은 이어진다 | `reject_update_fails` · `release_update_fails` |
| fix-weight-own | index.ts 386 (`computeVerdict`) | `PARAMS.weights[v.kind] ?? 0` 이 프로토타입을 거쳐 `constructor` · `toString` 등에 함수를 돌려줘 E · v 가 NaN → eval_verdicts 의 NOT NULL 에 걸려 processVerdicts 가 던졌다. 표시가 지워지지 않아 **그 뒤 모든 실행이 같은 자리에서 실패**(판정 · 지갑 정리 멈춤) → 자기 속성만 본다(`Object.hasOwn`, Rust 는 crate 의 `Params::weight`). www 는 vote · vote_outcome 만 받으므로 www 를 거치지 않고 들어간 표에서만 생긴다 | `verdict_prototype_kind` |
| fix-web-session-email-trim | **www** auth.ts 162 (`checkSession`) | 웹 세션은 email 을 trim 해서 확인(비었나)하고는 `obj.email` 을 그대로 돌려줬다. 앱 세션은 trim 한 값을 돌려주므로, S3 객체의 email 에 앞뒤 공백 · 줄바꿈이 붙으면 같은 사람이 두 갈래에서 **다른 주소**(computeAddress(hashMessage(email)))가 되고, 그 주소가 평가 · 투표 · 타임트라이얼의 from 이 된다 → 확인에 쓴 trim 한 email 을 돌려준다 | `session_web_email_padded` |

수정마다 Rust 와 `scripts/workers-diff/fixes/<워커>.mjs`(cron · www)의 글 패치(같은 id, 공백 무시 · 정해진 횟수만큼 맞아야 적용)가 짝입니다.

## 알려진 차이 (관찰되지 않거나 www 가 막는 것)

- 표 값(`eval_votes.value`)은 crate 의 `Obs.sign`(i32)으로, 시각(`created_at` · `tier_since`)은 i64 로 읽습니다. www 는 값 1 · −1 · 0 과 `Date.now()` 만
  넣으므로 같지만, 정수가 아닌 값이 D1 에 직접 들어가면 원본(수 그대로)과 다릅니다.
- `eval_verdicts.tier` 가 네 등급 밖의 글이면(cron 만 쓰므로 생기지 않음) crate 에는 unrated 로 넘기고 since 는 원본처럼 now 로 둡니다.
- 글의 짝 없는 서로게이트(잘못된 UTF-16)는 Rust 문자열이 담지 못해 U+FFFD 가 됩니다(결정 API 본문의 `state`). www 가 받은 JSON 글에서만 생길 수 있습니다.
- 우리가 직접 던지는 TypeError(속성 읽기)의 글은 V8 과 같게 맞췄지만, 모든 자리를 시험하지는 않았습니다(예: `for (const { id } of …)` 의 구조 분해 오류 글).
- 원본 TS 의 처리기 시험(`workers/cron/test`)은 TS 판을 시험합니다. Rust 판은 차분 시험대와 단위 시험이 맡습니다.
