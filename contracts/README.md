# 계약 (contracts)

이 폴더는 premium-discount 가 소유하는 계약입니다. 다른 구현(서버 cron, 앱, raon-zena)은 여기에 맞춥니다.

| 파일 | 내용 |
| --- | --- |
| `gen_verdict_v1.py` | PD-Verdict v1 참조 구현. 실행하면 `verdict_v1.json` 을 다시 씁니다 |
| `verdict_v1.json` | 매개변수(`params`)와 사례 19개(입력 + 기대 결과). 구현들이 통과해야 하는 기준 |
| `gen_verdict_v2.py` | PD-Verdict v2 참조 구현. v1 의 함수를 그대로 쓰고 매개변수만 넓힙니다. 실행하면 `verdict_v2.json` 을 다시 씁니다 |
| `verdict_v2.json` | v2 매개변수와 사례 25개(v1 사례 19개 + 접근성 내비게이션 사례 6개) |
| `gen_wallet_v1.mjs` | 인증의 지갑 갈래(사용자가 시드 구문을 보관하는 익명 계정) 참조 구현. ethers 로 `wallet_v1.json` 을 다시 씁니다 |
| `wallet_v1.json` | 시드 구문 → 주소 · 개인 키 2건, EIP-191 서명 3건, 요청 서명 2건, 거절할 시드 구문 3건. 서버(`workers/www/src/auth.ts`)와 `crates/premium-discount-identity` 가 같은 값을 냅니다 |
| `gen_analytic_blob_v1.mjs` | 웹 분석 사건 BLOB 코덱의 사례를 pako 로 만듭니다(`analytic_blob_v1.json` 을 다시 씀) |
| `analytic_blob_v1.json` | D1 `data` 칸의 여러 모양(바이트 배열 · Buffer · `{"0":…}` · base64 · JSON · 원시값) 19 사례. 서버 `workers/www/src/blob.ts` 와 `crates/premium-discount-analytic` 의 `blob` 이 같은 객체를 냅니다(logis-center BUG-RT-2/3/4 사례 포함) |
| `analytic_pug_v1.json` | 요소 → 속성 없는 PUG 13 사례. 수집 스크립트(`web/www/static/js/tracker.js` 의 `toPug`, 브라우저 DOM)와 `premium-discount-analytic` 의 `html_to_pug` 가 같은 결과를 냅니다 |

```bash
python contracts/gen_verdict_v1.py   # premium-discount 폴더에서
python contracts/gen_verdict_v2.py
node contracts/gen_wallet_v1.mjs     # workers/www/node_modules 의 ethers 를 씁니다
node contracts/gen_analytic_blob_v1.mjs   # workers/www/node_modules 의 pako 를 씁니다
```

## 지갑 갈래: 요청 서명 (`pd-wallet/1`)

- 지갑은 BIP-39 영어 12단어, 경로 `m/44'/60'/0'/0/0` 입니다(ethers `Wallet.createRandom` · `Wallet.fromPhrase` 의 기본값). 사용자가 시드 구문을 보관하고, 다른 기기에서는 시드 구문으로 같은 주소를 되살립니다.
- 요청마다 아래 글(줄바꿈 `\n`, 끝 줄바꿈 없음)에 EIP-191 서명을 하고, 헤더 `x-pd-address` · `x-pd-time` · `x-pd-signature` 로 보냅니다.

```text
premium.discount request v1
address: <소문자 주소>
time: <유닉스 ms>
method: <대문자 메서드>
path: <경로, 쿼리 없이>
body-sha256: <본문 바이트의 SHA-256, 소문자 hex>
```

- 서버는 시각이 5분 안이고, 서명에서 되살린 주소가 `x-pd-address` 와 같을 때만 받습니다. 같은 서명을 다시 보내는 것은 받는 쪽이 다룹니다(투표는 `request_id` 로 버리고, 타임트라이얼은 계정 없는 합계를 계정마다 코스 하나에 하루 한 번만 셉니다).
- 서명은 RFC 6979 결정적이라 같은 키 · 같은 글이면 같은 바이트입니다. 그래서 구현끼리 서명 자체를 비교해 시험합니다.

## PD-Verdict v1: 추천(premium) / 비추천(discount)

평가 한 건이 같은 범위(스코프)의 다른 평가들보다 높게 매겨지면 추천, 낮게 매겨지면 비추천입니다. 판정은 순수 함수 하나입니다. 관측, 스코프 기준선, 직전 등급을 넣으면 값 · 순위 · 등급 · grade · 이유 코드가 나옵니다.

1. **가중치와 감쇠**: 관측마다 `u = weight × credit × 2^(−age / H)` 를 구합니다. `age = now − at_ms` 이고 미래 시각은 0 으로 봅니다. `sign` 0(철회)은 세지 않습니다.
2. **근거량과 값**: `E = Σu`, `v = Σ(u × sign) / (E + k)` 입니다. v 는 −1 과 1 사이이고, 근거가 적으면 0 쪽으로 당겨집니다.
3. **기준선**: secondary(`cc` + `to`) → primary(`cc`) → global(트랙 전체) 순서로, 판정 가능한 노드 수 `n` 이 `min_obs` 이상인 첫 단계를 씁니다. 평균과 모집단 표준편차를 쓰고, 노드 자신도 포함합니다. 단계가 있으면 `r = tanh((v − mean) / (sd + eps))` 입니다.
4. **등급**은 아래 순서로 정합니다.
    - 대체된 기록은 `discount` 입니다.
    - `E < min_evidence` 이면 `unrated` 입니다.
    - `v ≥ floor` 이고 (`r ≥ enter` 또는 대결 승률 ≥ `duel_rate`)이면 `premium` 입니다.
    - `v ≤ −floor` 이고 (`r ≤ −enter` 또는 패배율 ≥ `duel_rate`)이면 `discount` 입니다.
    - 직전이 premium 이고 `v > 0`, `r ≥ exit`(r 이 없으면 승률 조건)이면 premium 을 유지합니다. discount 도 대칭으로 유지합니다.
    - 나머지는 `neutral` 입니다.
5. **grade**: premium 은 +2(`r ≥ strong`) 또는 +1, neutral 은 0, discount 는 −2(대체됨 또는 `r ≤ −strong`) 또는 −1, unrated 는 null 입니다.
6. **이유 코드**: 결정 이유 하나(`superseded` · `low_evidence` · `rank_high` · `duel_win` · `rank_low` · `duel_loss` · `held` · `floor` 중 하나, 또는 없음) 뒤에 `level:<단계>` 또는 `no_baseline` 이 붙습니다.
7. **since_ms**: 등급이 직전과 같으면 직전 값을, 바뀌면 now 를 씁니다.

대결(`duel`)은 만남이 `duel_min` 이상일 때만 봅니다. 서버는 v1 에서 대결과 대체를 쓰지 않습니다.

## PD-Verdict v2: 접근성 내비게이션의 행동 관측

v2(`pd-verdict/2`)는 규칙(위 1 ~ 7)이 v1 과 같고, 매개변수 두 가지만 더합니다. 그래서 v1 사례 19개는 v2 에서도 결과가 같습니다(`gen_verdict_v2.py` 가 확인합니다).

- **관측 종류 `implicit`(가중치 0.3)**: 접근성 내비게이션의 행동 로그가 에피소드마다 남기는 관측입니다. 번호 선택 · 화면 변화 · form change 로 에피소드의 성공과 마찰을 재고, 스코프 기준선에서 SDS 띠를 넘을 때만 +1(premium 쪽) 또는 −1(discount 쪽)을 남깁니다. 사용자가 입력하지 않아도 생기는 관측이라 표(1.0)보다 가볍습니다. 그래서 행동만으로는 에피소드 7개(E = 2.1)부터 판정됩니다.
- **트랙 `nav`**: 화면 · 영역 판정입니다. web 트랙과 같은 값(반감기 14일, `min_obs` secondary 8 · primary 4 · global 12)으로 시작합니다. 스코프는 영역이면 secondary = `cc` + 화면 키, primary = `cc` 이고, 화면이면 secondary 없이 primary = `cc` 입니다.

서버는 아직 v1 로 판정합니다. 타임트라이얼을 받는 곳(`/nav/trials`, `nav_trials` · `nav_courses` · `nav_counts` · `nav_vectors` 표)은 있고, `nav_counts`(코스 해시마다 결정이 난 런의 날짜별 개수)를 v2 `implicit` 관측으로 바꿔 판정하는 cron 은 다음 단계입니다.

## 매개변수를 바꿀 때

값을 하나라도 바꾸면 `RECIPE` 를 올리고(`pd-verdict/2`) 새 파일로 냅니다. 판정 행은 recipe 를 남기므로, recipe 가 다른 판정끼리는 비교하지 않습니다. 구현들은 자기 상수가 `params` 와 같은지 시험합니다.

| 구현 | 위치 | 시험 |
| --- | --- | --- |
| 참조 (Python) | `contracts/gen_verdict_v1.py`, `gen_verdict_v2.py` | 이 파일이 기대값을 만듭니다 |
| 서버 cron (TypeScript) | `workers/cron/src/verdict.ts` | `workers/cron/test/verdict.test.ts`(v1), `workers/cron/test/verdict_v2.test.ts`(v2, 계약의 `params` 를 넘겨서) |
| 앱 E2E 가짜 서버 | `apps/core/tests/e2e/mock_server.py` | 참조 구현을 그대로 import |
| Rust 라이브러리 | `crates/premium-discount-verdict` (`Params::v1()`, `Params::v2()`) | `cargo test -p premium-discount-verdict` (premium-discount 폴더에서): 두 계약의 매개변수와 사례 전부, v2 가 v1 사례를 그대로 담는지 |
| raon-zena (Rust) | `crates/zena-verdict` | 사본(`contracts/verdict_v1.json`)의 해시, 옆 저장소 원본과의 대조, 사례 전부. `premium-discount-verdict` 로 옮길 예정 |

## 관측 종류별 기본 가중치

| 종류 | 만드는 곳 | 가중치 |
| --- | --- | --- |
| `vote` | 서버: 로그인 확인된 남의 글 표 | 1.0 (게스트 · 자기 글 표는 0) |
| `vote_outcome` | 서버: raon-zena 가 동의를 받고 보낸 결과 확인 표 | 0.5 |
| `manual` | raon-zena: ▲/▼ 수동 보정 | 2.0 |
| `feedback` | raon-zena: 대화 속 추천/비추천 | 1.0 × credit |
| `outcome` | raon-zena: 승리 · 패배 · 해결 | 1.0 × credit |
| `reaction` | raon-zena: 감사 · 환호 · 좌절 | 0.6 × credit |
| `engage` | raon-zena: 체류 · 재드롭 · 고정 | 0.2 |
| `model` | raon-zena: AI 판정(로컬 전용) | 0.3 |
| `implicit` (v2) | 접근성 내비게이션: 행동 로그의 에피소드(성공 · 마찰) | 0.3 |
