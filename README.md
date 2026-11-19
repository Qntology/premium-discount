# premium-discount 평가 시스템 정리 (초안)

> **2026-10-27 갱신**: 임베딩 모델은 `google/embeddinggemma-2`(768차원)로 바뀌었습니다. 아래 본문의 granite·384 표기는 변경 이전 기록입니다. 현재 기준은 맨 아래 "11. 변경 기록"과 임베딩 계약(9-4)입니다.
>
> 이 문서는 현재 코드(`www/`, `cron/`, `app/`)와 wrangler 설정을 읽고 정리한 것입니다.
> "현재 상태"와 "목표 설계"를 분리해서 적었고, 확인이 필요한 부분은 `확인 필요`로 표시했습니다.

---

## 1. 폴더 구성

> **2026-11-19 갱신 (26절)**: logis-center 와 같은 배치로 옮겼습니다. 이 아래 2 ~ 25절은 그때의 경로(`app/` · `app/ui` · `app/src-tauri` ·
> `www/` · `cron/` · `crates/` 따로 빌드)로 적은 기록이라 그대로 두었습니다. 옛 경로 → 새 경로는 26-1 의 표를 보세요.

| 폴더 | 역할 | 기술 |
|---|---|---|
| `Cargo.toml` · `Cargo.lock` · `.cargo/config.toml` | 루트 작업 공간 하나 (`crates/*` + `apps/core` + `apps/desktop`), 산출물은 루트 `target/` | Rust |
| `apps/core` | 헤드리스 코어 `premium-discount-app` (LanceDB 저장소 · 모델 · 동기화 · `serve_lines`, Tauri 없음) | Rust |
| `apps/desktop` | 데스크톱 · 모바일 셸 `premium-discount-desktop` (Tauri v2, Android `gen/android`, 사이드카) | Rust + Tauri |
| `crates/` | 공용 라이브러리 `premium-discount-*` 17개 + `onnxruntime` · `onnxruntime-sys` (raon-zena · logis-center 가 git 리비전으로 씀) | Rust |
| `web/desktop` | 앱 웹뷰 화면 (esbuild, `npm test`) | TypeScript |
| `web/www` | www Worker 가 싣는 정적 파일 (`assets`) | JS · CSS |
| `workers/www` | 평가 확인 서비스 Worker (`wrangler.json` · `wrangler.test.json`) | Cloudflare Workers (TypeScript) |
| `workers/cron` | 대기열 · 판정 · 지갑 기록 정리 cron Worker | Cloudflare Workers (TypeScript) |
| `workers/migrations` | D1 마이그레이션 (www 의 `DB` 바인딩 `migrations_dir`, Node 시험의 가짜 D1 도 이것을 적용) | SQL |
| `contracts/` · `test-support/` | 언어 사이 계약 사례 · Node 시험 도우미 | JSON · Python · JS |

<details><summary>처음(2026-10) 폴더 구성</summary>

| 폴더 | 역할 | 기술 | 상태 |
|---|---|---|---|
| `app/` | 평가 생성 클라이언트 (욕설 필터, 질문 입력) | Rust + Tauri | **비어 있음** (신규 개발 대상) |
| `www/` | 평가 확인 서비스 (Worker, 웹 UI) | Cloudflare Workers (TypeScript) | 기존 items/pages/tasks 로직 존재, 평가 기능 미구현 |
| `cron/` | 임시 스케줄러 Worker (1분마다 `tasks` 처리) | Cloudflare Workers (cron) | 임시 코드, **수정 대상 아님** |

</details>

---

## 2. 현재 Cloudflare 리소스

### 2-1. D1 데이터베이스 (`www/wrangler.json`, `cron/wrangler.json` 기준, 2026-11-02 갱신)

| 바인딩 | database_name | database_id | 쓰는 Worker |
|---|---|---|---|
| `DB` | premium_discount | `f7ec6d8c-4274-44e7-94e9-d25410026f4b` | www, cron (**둘이 같아야 함**) |
| `apac1_premium_discount` | apac1_premium_discount | `a8a16911-35a1-4dca-9b4c-95fbe82a7da1` | www (레거시) |

> 이 표는 이전에 `e5bd6003-…`, `2c67be5c-…`로 적혀 있었습니다. git 기록상 커밋 `5ae6eec`("D1 ID 갱신")에서 wrangler 설정이 바뀔고 README는 따라가지 않았습니다.
> **실제 계정의 리소스와 일치하는지는 확인하지 못했습니다** (확인할 때 `CLOUDFLARE_API_TOKEN`이 없었습니다). 배포 전에 아래를 실행해 위 ID와 비교하세요.
> ```bash
> npx wrangler d1 list
> npx wrangler vectorize list
> ```
> `www`와 `cron`의 `DB` ID가 다르면 대기열을 아무도 처리하지 않습니다.

### 2-2. Vectorize 인덱스 (2026-11-02 갱신)

| 바인딩 | index_name | 쓰는 곳 | 비고 |
|---|---|---|---|
| `EVAL_VECTORIZE` | premium-discount-eval | `www/wrangler.json`, `cron/wrangler.json` | 768, cosine. 두 Worker가 같은 인덱스를 봐야 함 |
| `EVAL_VECTORIZE` | premium-discount-eval-test-768 | `www/wrangler.test.json` (`remote: true`) | 테스트용 |

이전 표의 `premium-discount`, `apac1-premium-discount` 바인딩은 운영 설정에서 제거되었습니다 (11절 참고). `wrangler.test.json`에는 남아 있습니다.

### 2-3. 기타 바인딩
- `AI` : Workers AI
- `CDN` : R2 버킷 `cdn`
- `ASSETS` : `./public/` 정적 파일

---

## 3. 현재 DB 스키마

> **주의**: `www/migrations/0001_*.sql`과 `cron/migrations/0001_*.sql`은 파일명이 같지만 내용이 다릅니다.
> 두 Worker가 같은 D1을 공유하므로 `items` 테이블 정의가 서로 충돌합니다. (아래 표에서 차이를 표시)

### 3-1. `www/migrations/0001_create_comments_table.sql`

**items**

| 컬럼 | 타입 | 설명 (코드 주석 기준) |
|---|---|---|
| id | TEXT PK | 의견 ID |
| type | TEXT | 0: draft, 1: premium, 2: discount |
| from | TEXT | 작성 AI 주소 |
| to | TEXT | 주소 |
| cc | TEXT | 도메인 |
| inf | TEXT | 참조값 |
| ref | TEXT | 유사값 |
| digest | TEXT | digest(title) |
| data | BLOB NULL | 글 내용 |
| created_at | INTEGER | 생성일 |
| updated_at | INTEGER | 수정일 |

**crons**: `id, cc, bcc, job(BLOB), ref, created_at, updated_at`
**console**: `id, bcc, log, created_at`

### 3-2. `cron/migrations/0001_create_comments_table.sql`

| 테이블 | 주요 컬럼 | 차이점 |
|---|---|---|
| usage | id, type, user, team, zone, date | www에 없음 |
| items | id, type, from, to, cc, **bcc**, ref, digest, data, created_at, updated_at | www와 `inf`/`bcc` 차이 |
| sales | 상품/가격/배송 관련 컬럼 다수 (sale_price, shipping_fee 등) | 평가와 무관한 커머스 테이블 |
| event | 쿠폰/할인 코드 (code, discount, usage_limit 등) | 평가와 무관 |
| tracking | 배송 추적 (carrier, no, recipient_address 등) | 평가와 무관 |
| talks, users, pages, views, tasks | 공통 패턴 `id, type, from, to, cc, bcc, ref, data, created_at, updated_at` | users에 `flag, amount` 추가 |
| zones | id, pool | |
| crons, console | www와 동일 | |

> 두 마이그레이션 모두 **평가 전용 테이블은 없습니다.** 템플릿(comments) 흔적이 남아 있는 상태입니다.

---

## 4. 목표 평가 시스템 흐름

### 4-1. 작성(저장) 흐름

```
[app: Tauri/Rust]
  사용자 입력
    │
    ▼
욕설 필터 (convaiinnovations/laya-multilingual)
    ├─ 욕설 감지 → 저장하지 않고 종료 (사용자에게 거부 응답)
    └─ 통과
         │
         ▼
  벡터화 (google/embeddinggemma-2)
         │
         ▼
  [서버: Cloudflare Worker]  ※ 원문은 저장하지 않음
    ├─ from/to/cc/bcc + 메타데이터 → D1 (evaluations 테이블)
    └─ 벡터 + metadata(cc, to)  → Vectorize (id로 D1과 연결)
```

### 4-2. 조회 흐름

```
[app: Tauri/Rust]
  질문 입력
    │
    ▼
[www: Cloudflare Worker]
  질문 벡터화 (google/embeddinggemma-2)
    │
    ▼
  Vectorize 질의 (topK, 유사도 점수 반환)
    │
    ▼
  점수 + 연결된 원문 → 응답 → 앱에서 점수 표시
```

---

## 5. 저장 데이터 범위 및 테이블 (갱신: 벡터 + 주소만 저장)

**원문은 서버에 저장하지 않습니다.** 서버(D1·Vectorize)에는 주소/도메인 메타데이터와 벡터값만 저장합니다.

### 5-1. 주소 필드 출처 (현재 `www/src/index.ts` 기준)

| 필드 | 현재 코드에서 값 | 익명(미로그인) 기본값 |
|---|---|---|
| `from` | 로그인 세션의 `cookies.address` (지갑 주소, `premium.discount` 인증 쿠키) | `ethers.ZeroAddress` (`0x000…0`) |
| `to` | 로그인 세션의 `cookies.team` (팀/수신 주소) | `ethers.ZeroAddress` |
| `cc` | 요청 `href`의 호스트에서 계산한 도메인. 마지막 두 라벨(`example.com`)을 쓰고, `twoPartDomains` 목록에 있으면 세 라벨(`example.co.kr` 계열)을 씀. `href`가 `origin`을 포함할 때만 계산됨 | `''` |
| `bcc` | 현재 코드에서 사실상 항상 빈 문자열(`""`). 값을 채우는 로직 없음 | `""` |
| `ref` | `hashId(team + cc + pathname + search)` 해시. 페이지 식별용 | - |
| `id` | 페이지: `hashId(cc + pathname)` 또는 `req.query.id`(주소) / 항목: `hashId(link + ZeroAddress)` | - |

- 주소 필드는 `ethers.isAddress` 검증을 거친 값만 씁니다. (익명은 ZeroAddress)
- 현재 `www`의 SQL 대부분이 `'${cookies.team}'`처럼 문자열 보간을 쓰므로, 평가 API는 `bind`로 작성해야 합니다.

### 5-2. D1 테이블 (신규 — 기존 스키마에는 없음)

> `root`, `ref`는 기존 스키마에 없던 컬럼입니다. 평가 시스템을 위해 새로 추가합니다.

```sql
CREATE TABLE IF NOT EXISTS evaluations (
    "id"         TEXT PRIMARY KEY,           -- UUID v7, Vectorize ID와 동일
    "from"       TEXT NOT NULL,              -- 작성자 주소 (임시: 클라이언트 값, 없으면 게스트 주소)
    "to"         TEXT,                       -- 팀/수신, 없으면 NULL
    "cc"         TEXT NOT NULL,              -- 등록 도메인 (예: example.com)
    "bcc"        TEXT,                       -- 보류
    "ref"        TEXT,                       -- 부모 평가 id (단일), 최상위면 NULL
    "root"       TEXT NOT NULL,              -- 스레드 최상위 평가 id (최상위 자신이면 자기 id)
    "created_at" INTEGER NOT NULL,           -- Date.now() 밀리초
    "updated_at" INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_evaluations_ref        ON evaluations ("ref");
CREATE INDEX IF NOT EXISTS idx_evaluations_root       ON evaluations ("root");
CREATE INDEX IF NOT EXISTS idx_evaluations_cc_created ON evaluations ("cc", "created_at");
```

**저장 규칙**
- `ref`는 부모 평가 `id` 하나만 가집니다. 최상위 평가는 `NULL`입니다.
- 부모가 있으면 Worker가 D1에서 부모 존재 여부를 확인하고, 부모와 `cc`가 같은지 검사합니다.
- 최상위 평가는 `root = id`, 답글은 `root = 부모.root`로 저장합니다.
- 평가는 추가만 하고 수정하지 않습니다(append-only). 따라서 순환 참조가 생기지 않습니다.

### 5-3. Vectorize 레코드 (벡터 + 필터용 메타데이터)

| 항목 | 값 |
|---|---|
| `id` | `evaluations.id` |
| `values` | 클라이언트(Rust)가 보낸 벡터 |
| `metadata` | `{ "cc", "to", "ref", "root" }` |

- 필터 용도: `cc`(도메인), `root`(스레드 전체), `ref`(직계 답글), `to`(팀)
- 필터에 쓰려면 각 필드에 **metadata index**를 만들어야 합니다. 인덱스 개수 상한은 공식 문서로 확인이 필요합니다.
- 원문은 metadata에 넣지 않습니다.

## 6. 개선 제안 및 확인 필요 사항

### 🔴 반드시 확인
1. **욕설 필터를 클라이언트(Tauri)에만 두면 우회 가능합니다.**
   서버에서도 같은 필터를 한 번 더 돌리거나, 서버 저장 전 검증을 필수로 두어야 합니다.
2. **HF 모델은 Workers 런타임에서 직접 실행할 수 없습니다.**
   `google/embeddinggemma-2`와 `laya-multilingual`을 어디서 돌릴지 정해야 합니다.
   - (a) Workers AI 카탈로그에 해당 모델이 있는지 확인 (제가 확인하지 못함)
   - (b) 별도 추론 서버/컨테이너에서 실행
   - (c) Tauri(Rust)에서 ONNX/candle로 실행 — 모델 용량이 앱 번들에 크게 영향
3. **저장과 조회의 임베딩 모델·전처리·정규화는 반드시 동일해야 합니다.**
   한쪽만 다른 모델을 쓰면 유사도 점수가 의미 없어집니다. 임베딩은 서버 한 곳에서만 수행하는 것을 권장합니다.
4. **Vectorize 인덱스 차원은 생성 후 변경할 수 없습니다.**
   embeddinggemma-2 모델의 출력 차원을 확인한 뒤, 기존 `premium-discount` 인덱스가 맞는지 확인하고 아니면 새 인덱스를 만들어야 합니다.
   (www/src/index.ts 주석에는 `embeddinggemma-300m 768차원` 계획이 적혀 있어, 기존 인덱스가 그 기준일 가능성이 있습니다.)
5. **"평가"의 정의가 필요합니다.**
   - 질문과 저장된 평가 전체를 비교하는가?
   - 상위 몇 개(topK)를 보여주는가?
   - 점수 임계값이 있는가? 점수 범위와 표시 방식은?

### 🟡 권장
6. **SQL 문자열 보간 제거**: `www/src/index.ts`에 `WHERE "to" = '${cookies.team}'` 형태가 있습니다. SQL 인젝션 위험이므로 `prepare(...).bind(...)`로 바꿔야 합니다. 평가 API는 처음부터 bind를 쓰세요.
7. **평가 DB 분리**: 두 wrangler.json이 같은 D1을 공유하고 마이그레이션 내용이 달라, 평가 테이블을 기존 DB에 넣으면 충돌 가능성이 큽니다.
8. **주소/벡터만 저장**: 원문을 저장하지 않으므로 원문 기반 재검사·재임베딩·디버깅은 서버에서 할 수 없습니다.
9. **공개 조회 API 보호**: 조회 한 번마다 임베딩과 Vectorize 호출 비용이 발생합니다. 인증 또는 rate limit이 필요합니다.
10. **거부 응답 정의**: 욕설로 차단됐을 때 앱에 어떤 메시지/코드를 보낼지 정해야 합니다. 차단 사유를 로그로 남길지도 결정이 필요합니다.
11. **임계값 튜닝**: 욕설 필터의 판정 임계값은 샘플 데이터로 오탐/미탐을 확인한 뒤 정하세요.

---

## 7. 다음 단계 (제안)

1. embeddinggemma-2 모델의 출력 차원과 Workers AI 카탈로그 여부 확인
2. 임베딩 실행 위치 결정 (6-2 중 택1)
3. 평가 전용 D1 생성 + `evaluations` 마이그레이션 작성
4. 평가 전용 Vectorize 인덱스 생성 (차원/메트릭 cosine 권장)
5. `www/`에 저장 API(`POST`)와 조회 API(`POST`) 구현 (bind 사용)
6. `app/`에 Tauri 프로젝트 생성 후 욕설 필터 → 저장/조회 연결

---

## 8. 결정 사항 (2026-10-12)

**결정**: 임베딩은 Rust(`app`)에서 수행하고, 저장·조회 모두 클라이언트가 만든 벡터를 신뢰합니다. 서버는 벡터를 다시 만들지 않습니다.

> 이 결정은 위 **4. 목표 평가 시스템 흐름**과 **6. 개선 제안** 중 임베딩 위치와 관련된 내용을 대체합니다.

### 8-1. 저장 경로
```
[app] 입력 → 욕설 필터(laya) → 임베딩(embeddinggemma-2) → POST /evaluations { from, to, cc, vector, ref }
[www] 벡터·주소 검증 → D1 evaluations INSERT (주소 메타데이터)
                     → Vectorize upsert (id, vector, metadata { cc, to })
```

### 8-2. 조회 경로
```
[app] 질문 → 임베딩(embeddinggemma-2) → POST /evaluations/search { vector, topK, cc? }
[www] 벡터 검증 → env.<VECTORIZE 바인딩>.query(vector, { topK, filter: { cc }, returnMetadata: true })
      → 응답: [{ id, score }] (필요 시 D1에서 주소 결합)
```
- 조회 응답에는 원문이 없으므로, 앱은 **점수와 주소 정보만** 표시할 수 있습니다.

### 8-3. 서버가 반드시 하는 검증
- 벡터 길이 = 인덱스 차원, 모든 값이 유한한 숫자(`NaN`/`Infinity` 거부)
- 벡터 노름(norm)이 정상 범위인지 확인 (정규화 여부를 모델 기준으로 고정)
- 벡터 길이가 768인지 검사하고, 다르면 거부
- 인증 + 사용자 단위 rate limit (조회·저장 모두)
- SQL은 `prepare().bind()`만 사용

### 8-4. 잔여 위험 (수용한 부분)
- 클라이언트 벡터를 신뢰하므로, 서버는 벡터가 실제로 원문에서 만들어졌는지 확인할 수 없습니다.
- **서버가 원문을 받지도 저장하지도 않으므로, 서버 측 욕설 재검사는 불가능합니다.** 욕설 방어는 클라이언트 필터에만 의존하게 됩니다. 우회 가능성을 받아들일지 결정이 필요합니다.
- 벡터 자체도 원문 정보를 일부 담을 수 있으므로, 벡터도 민감 정보로 취급하는 것을 권장합니다.
- 잘못된 데이터가 들어가면 `id` 기준으로 D1과 Vectorize에서 함께 삭제하는 관리 경로가 필요합니다.

### 8-5. 다음 작업
1. embeddinggemma-2 출력 차원과 정규화 방식 확인 → Vectorize 인덱스 생성(cosine 권장)
2. `www/`에 `POST /evaluations`, `POST /evaluations/search` 구현
3. `app/`에 Tauri 프로젝트 생성, 임베딩·필터 모듈 구현
4. 같은 입력에 대해 Rust 임베딩과 기준 값(Python 등)의 결과가 일치하는지 검증

---

## 9. 구현 현황 (로직만 작성, 모델 미다운로드)

### 9-1. 완료
- `www/migrations/0002_create_evaluations.sql`: `evaluations` 테이블과 인덱스 (`ref`, `root`, `cc + created_at`)
- `www/src/evaluations.ts`
  - `POST /evaluations`: 입력 검증, 부모 확인(같은 `cc`만 허용), UUID v7 생성, D1 저장, Vectorize upsert. Vectorize 실패 시 D1 행 롤백
  - `POST /evaluations/search`: `topK` 1~50, 필터(`cc`, `root`, `ref`, `to`)는 검색 전에 적용
  - CORS: `tauri://localhost`, `http://tauri.localhost` 허용
- `www/src/index.ts`: 맨 앞에서 `/evaluations`, `/evaluations/search` 경로만 가로챔. 기존 로직은 변경하지 않음
- 게스트 주소: `from`이 없으면 IP를 `GUEST_SALT`와 함께 keccak256 해싱해 주소를 만듦. 응답의 `from`을 앱이 저장해서 이후 요청에 사용. 같은 IP면 같은 주소가 나옴

### 9-2. 배포 전 필요한 작업
1. Vectorize 인덱스 생성 (차원 768, cosine) 후 메타데이터 인덱스 추가 (`cc`, `to`, `ref`, `root`)
   ```bash
   npx wrangler vectorize create premium-discount-eval --dimensions=768 --metric=cosine
   npx wrangler vectorize create-metadata-index premium-discount-eval --property-name=cc --type=string
   npx wrangler vectorize create-metadata-index premium-discount-eval --property-name=to --type=string
   npx wrangler vectorize create-metadata-index premium-discount-eval --property-name=ref --type=string
   npx wrangler vectorize create-metadata-index premium-discount-eval --property-name=root --type=string
   ```
   (명령은 wrangler 버전에 따라 다를 수 있으니 실행 전 확인)
2. `www/wrangler.json`에 `EVAL_VECTORIZE` 바인딩 추가 — **완료** (인덱스를 만들기 전에는 배포가 실패합니다)
   ```json
   { "binding": "EVAL_VECTORIZE", "index_name": "premium-discount-eval" }
   ```
3. 마이그레이션 적용 (검토 후): `npx wrangler d1 migrations apply DB --remote`
4. 평가용 D1을 따로 둘지 결정 (현재는 기존 `DB` 바인딩 사용)
5. 게스트 주소용 솔트 설정: `npx wrangler secret put GUEST_SALT` (미설정이면 저장 요청이 500을 반환)

### 9-3. 미결정 항목
- `from` 검증 방식 (지갑 서명 또는 토큰). 임시로 클라이언트 값을 그대로 신뢰하고, 인증은 나중에 구현
- rate limit
- 평가 DB 분리 여부

### 9-4. 임베딩 계약 (Rust 쪽에서 반드시 지켜야 함)
- 모델: `google/embeddinggemma-2`, 출력 768차원, Apache 2.0
- 풀링: 풀링 방식은 실제 모델을 붙일 때 모델 카드 기준으로 확인합니다. granite의 mean 풀링 결정(10-4)은 granite 제거로 무효입니다. 모델 카드의 예제가 L2 정규화를 직접 하므로, 클라이언트에서 L2 정규화 후 전송 (서버는 norm이 1±0.01이 아니면 거부)
- 토크나이저: 모델 레포의 토크나이저를 그대로 사용
- 입력 길이: 모델 최대 32,768 토큰. 잘림 정책은 미정
- 모델 파일은 리포지터리에 넣지 않습니다. 로컬에 받아 `EMBEDDINGGEMMA_DIR`로 지정해 테스트합니다 (11절).

### 9-5. Vectorize 한도 (공식 문서 기준)
- 벡터 차원 최대 1536 (현재 768 사용)
- 메타데이터 10KiB/벡터, 메타데이터 인덱스 최대 10개 (현재 4개)
- 인덱싱 값 최대 64바이트 → `cc`는 64자 이하로 제한
- ID 최대 64바이트 (UUID v7은 36자)
- topK: 메타데이터 포함 시 최대 50, 미포함 시 최대 100

### 9-6. 기존 코드에서 발견한 문제 (이번 작업에서 수정하지 않음)
- `www/src/index.ts` 약 420행: `var userRegion = userRegion[FLAG]`는 같은 이름의 변수를 참조해서 `undefined[FLAG]`가 되어 TypeError가 날 가능성이 높습니다. 실행해서 확인하지는 않았습니다. 평가 경로는 앞에서 가로채므로 영향을 받지 않습니다. 수정 여부는 별도로 결정이 필요합니다.

### 9-7. app (Rust 클라이언트 로직) 진행 상황
- 기존 `app`(git 기록의 app-scanner: 크롤링/분석 에이전트)은 무시하고, 평가 전용 crate로 새로 작성했습니다. 기존 파일은 git 기록에만 남아 있습니다.
- `app/src/pipeline.rs`: `Evaluator::submit`(입력 검사 → 욕설 필터 → 임베딩 → 저장), `Evaluator::search`(임베딩 → 벡터 검색). 욕설 필터는 저장 경로에만 적용합니다.
- `app/src/embedding.rs`: `Embedder` trait, L2 정규화, 계약 검사(768차원, norm ≈ 1).
- `app/src/filter.rs`: `ProfanityFilter` trait (laya 모델 미연결).
- `app/src/api.rs`: 서버 DTO와 `HttpBackend` (`/evaluations`, `/evaluations/search`). 요청에 원문은 포함하지 않습니다.
- 테스트 41개 통과 (`cargo test --lib`, 가짜 임베딩·필터·백엔드 사용). 상세는 10-5, 10-9, 10-10.
- 남은 작업: laya 필터 구현 (모델 파일 필요), Tauri 셸 연결, 게스트 주소 보관. embeddinggemma-2 텍스트 추론은 11절에서 연결했습니다.

### 9-8. 공개 응답 투영 + 그래프 API (구현 완료, 배포 전)

**결정 반영**: 노드 종류는 평가·도메인·작성자 모두 포함, 유사도 엣지 포함, 공개 (인증 없음). 노드 수 상한은 보류.

**공개 응답 규칙**
- D1에는 전체 컬럼을 저장합니다. 웹 응답에는 `from`, `to` 원값을 넣지 않습니다.
- 작성자는 `AUTHOR_KEY_SECRET`으로 HMAC-SHA256을 계산한 표시용 키(앞 12자리)로만 보여줍니다 (`www/src/projection.ts`).
- 공개 검색(`/public/evaluations/search`) 응답은 `id, score, cc, root, ref`만 포함합니다. 앱 검색 응답에는 `to`가 들어 있습니다 (10-1).
- 공개 검색에서 `to` 필터는 400으로 막았습니다. 주소 조건 조회로 추론하는 것을 막기 위해서입니다.
- (2차 정리) 앱의 `SearchOptions.to`는 그대로 씁니다. 서버 앱 검색 경로가 `to`를 허용합니다 (10-1).

**그래프 API** (`www/src/graph.ts`, 공개)
| 경로 | 설명 |
|---|---|
| `GET /graph?cc=example.com` | 도메인 범위 그래프 (평가 노드, 도메인 허브, 작성자 허브) |
| `GET /graph/local?id=...` | 선택한 평가가 속한 스레드(같은 root) 그래프 |
| `GET /graph/similar?id=...&topK=10` | 선택한 평가와 벡터가 비슷한 평가 (같은 cc 안, 벡터는 응답에 없음) |
| `GET /graph/view` | 그래프 뷰 페이지 (d3 force 레이아웃, cdnjs d3@7.9.0) |

엣지 종류: `belongs`(평가→도메인), `authored`(평가→작성자), `reply`(답글→부모, 같은 응답 안에 부모가 있을 때), `similar`(유사도, 점수 포함)

**보류 항목 (주석으로만 남김)**
- `GRAPH_NODE_LIMIT_TEMP = 200`: 노드 수 상한. 임시값이며 확정 필요 (`TODO(결정 보류)`)
- 인증, rate limit: 공개 상태이므로 `/graph/similar`는 Vectorize 질의 비용이 발생합니다. 확정 필요.

**배포 전 설정 명령**
```bash
npx wrangler secret put AUTHOR_KEY_SECRET   # 표시용 작성자 키 비밀 (필수)
npx wrangler secret put GUEST_SALT          # 게스트 주소 솔트 (필수)
```
- `/graph/similar`는 `EVAL_VECTORIZE` 바인딩이 있어야 동작합니다 (9-2 참고).

**검증**: `evaluations.ts`, `graph.ts`, `projection.ts`는 strict TypeScript 검사를 통과했습니다. 실제 Worker 실행과 D1/Vectorize 연동은 아직 확인하지 않았습니다.

**대시보드**: 별도 D1 사용 여부(A/B)는 아직 결정되지 않아 이번 구현 범위에서 제외했습니다.


---

## 10. 2차 정리 (2026-10-12): 검색 범위, 인증 연결 준비, 배포 설정

### 10-1. 검색 범위
| 경로 | 호출 주체 | `to` 필터 | 응답의 `to` |
|---|---|---|---|
| `POST /evaluations/search` | 앱 | 허용 (0x000…는 400) | 포함 |
| `POST /public/evaluations/search` | 공개 웹 | 400으로 거부 | 제외 |

- 경로 분리는 **웹 정책**일 뿐 접근 제어가 아닙니다. 인증이 없어서 누구나 앱 경로를 직접 호출하면 `to` 조회가 가능합니다. 인증을 붙이기 전까지의 잔여 위험입니다.

### 10-2. 세션 전달과 검증
- 브라우저: `hash`, `token` 쿠키 (OAuth3 쿠키 형식 그대로)
- 앱: `x-oauth-hash`, `x-oauth-token` 헤더. 요청마다 항상 보내며, 기본값은 익명 자리값(10-8)입니다. 로그인 세션은 `HttpBackend::with_session`으로 바꿉니다. 평문 헤더이므로 **HTTPS에서만 쓰세요.**
- `www/src/auth.ts`는 두 방식을 모두 읽습니다. 헤더가 하나라도 있으면 헤더만 씁니다. 형식(hash 40자리, token 64자리 hex)이 맞지 않으면 익명으로 봅니다. `0x` 접두사는 있어도 없어도 됩니다. hash 또는 token이 전부 0이면(앱의 익명 자리값) 세션이 아니므로 익명으로 봅니다.
- **검증은 아직 연결되지 않았습니다.** `verifySession()`은 항상 `null`을 돌려주므로 저장되는 `from`은 지금과 같습니다 (검증 주소 → 클라이언트 값(임시) → 게스트 주소).
- 검증 방법 두 가지:
  - S3 `hash/<hash>` 객체를 **읽기 전용**으로 조회해 token, email, ip(`CF-Connecting-IP`)를 확인합니다. 읽기 전용 AWS 자격 증명이 필요합니다. **권장.**
  - api/cdn 세션 응답을 씁니다. 그런데 api의 GET 분기에는 S3 업로드와 DB insert·delete가 있고, cdn 루트 분기에도 쓰기 경로가 있어서 검증용으로는 쓰지 않습니다.
- 검증이 연결되면 `from` = `computeAddress(hashMessage(email))`입니다 (oauth.network과 같은 식).

### 10-3. QR 로그인 (`www/public/static/js/auth.js`, `content.js`)
- QR의 메일 호스트와 세션 호출 대상을 `logis.center`에서 `premium.discount`로 바꿨습니다 (16곳, `README.md` 포함).
- QR 메일 주소는 `<hash>.premium.discount@oauth.email`입니다. SES는 DKIM·DMARC·SPF가 통과한 메일만 받아 `hash/<hash>`에 `email`을 기록합니다.
- `auth.js` 구문 오류를 고쳤습니다. `render` 메서드 안의 `await Upsert`(545·550행)를 `Promise.resolve().then(...).catch(...)`로 바꿨습니다. 렌더링은 저장을 기다리지 않고, 실패는 기존처럼 무시합니다. 이전에는 파일 전체가 파싱되지 않아서 `/`의 로그인 스크립트가 아예 실행되지 않았습니다.
- 확인: 렌더링된 `/`를 헤드리스 Chromium에서 열었을 때 스크립트 오류가 없었고, 그래프 카드가 보였으며, 버튼은 `/graph/view?cc=example.com`으로 이동했습니다. QR 자체는 외부 OAuth3(cdn)가 필요해서 로컬에서는 확인하지 못했습니다.
- **확인 필요**: `auth.js`와 `content.js`는 `https://premium.discount?hash=…&token=…` 형태로 요청합니다(발자국 기록, 크론 저장). 이 요청을 받는 엔드포인트가 새 Worker에는 없습니다. 레거시 `index.ts`는 `cookies`가 정의되지 않은 상태라서 이 기능은 실패할 가능성이 큽니다. 로그인 흐름은 cdn/api를 쓰므로 영향이 없을 것으로 보지만 확인하지는 못했습니다.

### 10-4. 사용자 위임 항목에 대한 결정 (되돌릴 수 있음)
- **게스트 주소**: IP 해시(`GUEST_SALT`) 유지. 익명 `hash` 주소나 `0x000…` 작성자로 바꾸는 안은 보류합니다 (2026-10-12 사용자 확인: 유지).
- **대시보드**: 옵션 A(같은 D1) 채택. 트래픽이 생기면 옵션 B(별도 읽기 모델)를 다시 봅니다.
- **granite 풀링(무효)**: 사용자 원본 `app/src/granite.rs`(mean 풀링)를 그대로 둡니다. 모델 카드는 CLS라서 실제 모델로 기준 값을 비교하기 전까지는 미확정입니다.
- **그래프 노드 상한**: 임시 200과 TODO 주석만 둡니다.

### 10-5. Rust 앱
- `api.rs`: `Match`를 서버 응답 구조(`id, score, cc, root, ref, to` 평평한 필드)에 맞췄습니다. 이전에는 `metadata` 객체를 기대해서, 응답의 필드가 조용히 빈 값이 되는 문제가 있었습니다.
- `Metadata` 타입은 제거했습니다 (`lib.rs` 재노출 포함).
- `HttpBackend`는 `Session`을 들고 있고, 기본값은 익명 자리값(`Session::anonymous()`)입니다. 모든 요청에 `x-oauth-hash`/`x-oauth-token` 헤더를 보냅니다.
- `HttpBackend::with_session(hash, token)`으로 로그인 세션을 바꿉니다.
- `cargo test --lib` 41개 통과. 헤더는 로컬 소켓으로 실제 전송된 값을 확인했습니다.

### 10-6. 배포 순서
1. Vectorize 인덱스 생성: `npx wrangler vectorize create premium-discount-eval --dimensions=768 --metric=cosine`
2. 메타데이터 인덱스 추가: `cc`, `to`, `ref`, `root` (9-2의 명령에서 이름만 `premium-discount-eval`로 바꿔 실행)
3. 1번이 끝나야 `EVAL_VECTORIZE` 바인딩으로 배포할 수 있습니다.
4. 시크릿: `npx wrangler secret put GUEST_SALT`, `npx wrangler secret put AUTHOR_KEY_SECRET`
5. 마이그레이션: `npx wrangler d1 migrations apply DB --remote` (`0002`, `0003` 모두 적용)
6. 배포: `npx wrangler deploy`
7. 확인: `/graph?cc=…`, `/graph/view?cc=…`, `/public/evaluations/search`에 `to`를 보내면 400인지 확인

### 10-7. 검증한 것과 못 한 것
- **검증함**
  - 엔드포인트 통합 테스트 40개 (10-10의 멱등 키 10개 포함): 메모리 D1 + 가짜 Vectorize. 저장, 답글 규칙, 앱/공개 검색 범위, 그래프 API, 그래프 응답에 원문 주소가 없는지 확인.
  - 세션 헤더/쿠키 파싱 테스트 13개 (익명 자리값, 한쪽만 0, 0x 접두사, 대소문자).
  - 헤드리스 Chromium으로 `/` 페이지 확인 (스크립트 오류 없음, 그래프 카드 표시, 버튼 이동).
  - `cargo test --lib` 41개 통과 (10-9, 10-10의 테스트 포함).
- **못 함**: 실제 Cloudflare 배포, 실제 D1·Vectorize 호출, embeddinggemma-2 모델 실행(모델 미다운로드), 실제 OAuth3 QR 로그인, 서버 세션 검증.
- **남은 일**: 서버 세션 검증(읽기 전용 AWS 키 필요), 앱에서 세션을 얻는 방법(로그인 화면), premium.discount 발자국/크론 엔드포인트, 노드 상한.

### 10-8. 앱 익명 자리값 (인터넷이 없을 때)
- 웹은 첫 요청에서 서버(cdn)가 hash/token을 만들어 줍니다. 앱에는 그 과정이 없으므로, 세션이 없을 때는 자리값을 씁니다. `x-oauth-hash: 0x` + 0 40자리, `x-oauth-token: 0x` + 0 64자리 (`api::ANONYMOUS_HASH`, `ANONYMOUS_TOKEN`).
- 서버는 hash 또는 token이 전부 0이면 세션으로 보지 않고 게스트 주소로 저장합니다. 같은 IP에서 자리값으로 보낸 저장과 헤더 없이 보낸 저장은 같은 게스트 주소가 됩니다 (`integration.cjs`의 2b 검사).
- 자리값은 **인증이 아닙니다.** 0으로 된 값은 서버 검증 전에 걸러지므로, 검증이 연결된 뒤에도 항상 익명입니다.
- 인터넷이 된 뒤 세션을 얻는 방법은 아직 정하지 않았습니다. cdn 코드를 읽어 보니 유효한 Referer로 루트를 요청하면 `Set-Cookie`로 hash/token을 준다는 경로가 있지만, 앱에서 이를 실행하거나 테스트하지는 않았습니다.
- 결정 (2026-10-12, 사용자 확인):
  1. 오프라인 저장 큐: **큐에 쌓아 들어온 순서대로 전송.** 구현은 10-9.
  2. 익명 저장의 작성자: **IP 해시 게스트 유지.** `0x000…` 작성자로 통일하지 않습니다 (10-4).

### 10-9. 오프라인 저장 큐 (`app/src/queue.rs`)
- 인터넷이 없으면 저장 요청(벡터, cc, from, to, 부모)을 큐에 쌓고, 연결되면 **들어온 순서대로** 보냅니다.
- 호출: `Evaluator::submit_queued(input, &mut queue)`. 서버에 저장됐으면 `Saved`, 연결이 없거나 앞선 항목을 기다리면 `Queued { local_id }`를 돌려줍니다. 욕설로 막힌 입력은 큐에 들어가지 않습니다.
- 앱은 시작할 때와 연결이 돌아왔을 때 `Evaluator::flush_queue(&mut queue)`를 호출합니다. 큐가 스스로 재시도하지 않습니다.
- 답글: 부모가 아직 서버에 없으면 `SubmitInput::local_parent`에 부모의 로컬 id를 넣습니다. 부모가 먼저 보내진 뒤 그 서버 id(`ref`)로 바뀌어 전송됩니다. 부모가 이미 보내졌으면 큐에 넣을 때 바로 서버 id로 바꿉니다.
- 실패 처리:

  | 실패 | 처리 |
  |---|---|
  | 연결 실패·시간 초과 (`AppError::Offline`) | 큐를 멈추고 항목을 그대로 둡니다. 재시도 횟수는 세지 않습니다. |
  | 서버 5xx, 429, 408, 기타 HTTP 오류 | 최대 5번(`MAX_SERVER_ATTEMPTS`)까지 재시도한 뒤 버립니다. |
  | 그 밖의 4xx (예: 400) | 바로 버립니다. |
  | 거부된 부모의 답글 | 서버로 보내지 않고 함께 버립니다. |

  버린 항목은 `FlushReport.rejected`로 알려 줍니다. 사용자에게 어떻게 보여 줄지는 아직 정하지 않았습니다.
- 저장: `OfflineQueue::to_json()`으로 문자열을 얻어 기기에 저장하고 `from_json()`으로 되살립니다. 보낸 항목의 id 매핑도 함께 들어가므로 재시작 뒤에도 답글이 부모를 찾습니다. 실제 디스크 쓰기는 Tauri 셸을 붙일 때 구현합니다.
- 한계 (사용자 결정 전 확인할 것):
  - **중복 저장**: 멱등 키(`request_id`)로 막았습니다 (10-10). 같은 키로 다시 온 요청은 저장하지 않고 처음 결과를 돌려줍니다. 남은 경계: 같은 키의 요청이 거의 동시에 들어와 먼저 온 요청의 벡터 저장이 실패하면, 행이 지워지면서 벡터만 남을 수 있습니다. 드문 경우이며 정리 작업이 필요합니다.
  - **큐 상한 없음**: 오래 오프라인이면 벡터가 계속 쌓입니다 (벡터 1개는 768차원 실수). 상한과 오래된 항목 정리 규칙이 필요합니다.
  - 보낸 항목의 id 매핑은 큐 파일에 계속 남습니다. 항목당 수십 바이트입니다.
  - 욕설 필터와 임베딩은 오프라인에서도 돌아야 합니다. embeddinggemma-2 텍스트 추론은 `EmbeddingGemma::load`로 연결했습니다 (11절).
- 검증: 큐 테스트 8개, 파이프라인 테스트 6개, 연결 거부 테스트 1개 (`cargo test --lib` 41개 통과, 멱등 키 테스트 포함). 가짜 백엔드로 연결 끊김·서버 오류·영구 거부·재시작 시나리오를 확인했습니다.
- 아직 안 한 것: 실제 Worker를 상대로 한 네트워크 끊김·복구 테스트, 디스크 저장, 앱 화면.

### 10-10. 멱등 키 (`request_id`): 중복 저장 방지
- 앱은 저장 한 건마다 `new_request_id()`(난수 16바이트를 hex 32자리로)로 키를 만듭니다. 큐에 넣을 때 키도 함께 보관하고, 재시도할 때 같은 키를 보냅니다.
- 서버 (`POST /evaluations`, 본문의 `request_id`):
  - 형식: 16~64자, `A-Z a-z 0-9 _ -`. 형식이 맞지 않으면 400.
  - 같은 키가 이미 있으면 새로 저장하지 않고 **200**으로 처음 결과(`id`, `root`, `ref`, `from`)를 돌려줍니다.
  - 같은 키인데 `cc`나 `ref`가 다르면 **409**입니다.
  - 처음 요청이 벡터를 쓰기 전에 끊겼을 수 있으므로, 재시도 때 Vectorize에 벡터가 없으면 이번 요청의 벡터로 채웁니다. 이미 있는 벡터는 덮어쓰지 않습니다.
  - 키가 없는 요청은 예전처럼 저장합니다 (`request_id` 열이 NULL이고, 유니크 검사에서 제외됩니다).
- 저장 위치: `evaluations.request_id` 열과 부분 유니크 인덱스 (`www/migrations/0003_add_request_id.sql`).
- 검증: 통합 테스트 40개 통과 (새 10개: 재시도 200과 같은 id, 행 1개, 벡터 1개, 다른 cc 409, 잘못된 키 400, 벡터 유실 복구, 키 없는 요청 201). `cargo test --lib` 41개 통과 (응답 유실 시나리오에서 가짜 서버가 같은 키를 한 번만 저장).
- 한계: 실제 D1과 Vectorize에서는 확인하지 못했습니다. 같은 키가 거의 동시에 들어오는 경우의 경계는 10-9에 적었습니다.
- 배포: `0003`은 `0002` 뒤에 적용합니다 (10-6).

---

## 11. 변경 기록 (2026-10-27)

| 항목 | 이전 | 현재 | 근거 |
|---|---|---|---|
| 임베딩 모델 | granite-embedding-97m (384차원, ModernBERT 구현) | google/embeddinggemma-2 (768차원, 텍스트·이미지·오디오·비디오 공통 공간) | granite 구현은 Gemma 추론에 재사용할 수 없어 제거. 텍스트·이미지·오디오·비디오 프레임 추론 연결 (`app/src/embeddinggemma_text.rs`, `embeddinggemma_vision.rs`, `embeddinggemma_audio.rs`, `embeddinggemma.rs`). 비디오 디코딩은 없음 |
| 벡터 정규화 | 임베더 내부(granite)에서만 수행 | 파이프라인이 `l2_normalize` 후 계약 검사 (저장·검색 공통) | 임베더가 정규화하지 않아도 계약을 지키도록 |
| 운영 Vectorize | 없음 | `premium-discount-eval` (768, cosine) + 메타데이터 인덱스 cc·to·ref·root | 배포 전 필수 |
| 오프라인 큐 | 메모리만 | `save_to`/`load_from`으로 디스크 저장 (임시 파일 후 이름 변경). 깨진 파일은 에러 | 앱 종료 시 소실 방지. 호출 시점은 앱이 정함 |
| 큐 상한 | 없음 | 1,000건 (임시값). 넘으면 `QueueFull` | 무한 증가 방지 |
| 401·403·404 응답 | 즉시 폐기 | 재시도 후 폐기 (5회) | 설정·라우팅 오류로 데이터를 잃지 않게 |
| 미사용 Vectorize 바인딩 | `premium-discount`, `apac1-premium-discount` (계정에 없음) | 제거 | 배포 실패 원인. 코드에서 참조하지 않음 |
| 레거시 `index.ts` 434행 | `undefined[FLAG]`로 TypeError → 500 | 매핑 객체로 조회, 실패는 try 안에서 418 | 전역 500 방지. 레거시 기능 자체는 여전히 동작하지 않음 |
| 옛 파일 | `app/embedding.rs` (루트, 384 상수) | 삭제 | 크레이트에 포함되지 않았음 |

### 대기열 설계 (2026-10-27 추가, 8-4 결정을 대체)

- **원문 전송**: 8-4의 "서버는 원문을 받지 않는다"는 결정을 뒤집었습니다. 앱은 원문과 SHA-256 해시와 벡터를 보냅니다. 서버는 해시가 원문과 맞는지 확인하고, 맞지 않으면 저장하지 않고 422를 돌려줍니다.
- **POST /evaluations**: 바로 저장하지 않고 `eval_queue`(0004)에 넣고 202 `{id, root, ref, from, status: "pending"}`을 돌려줍니다. 같은 `request_id`는 같은 결과를 돌려줍니다.
- **cron/ (1분마다)**: `premium-discount-cron` Worker가 대기열을 최대 20건씩 집어 Alibaba 결정 API(`decision-model-preview-2026-09-24`)로 욕설 여부를 판정합니다 (15절부터는 욕설·음란·불법 세 질문을 한 번의 호출로 묻습니다).
  - 깨끗하면 D1 `evaluations` → Vectorize → 대기열 삭제 순서로 저장합니다. 다시 실행해도 결과가 같습니다.
  - 욕설이면 `rejected`, 10회 실패하면 `failed`. 두 경우 원문과 벡터를 지우고, 7일 뒤 행도 지웁니다.
  - 응답을 해석할 수 없으면 clean으로 보지 않고 재시도합니다 (fail-closed).
  - 답글은 부모가 저장된 뒤에만 저장합니다. 부모가 거부되면 답글도 거부합니다.
- **상한과 한계**: 1분에 20건, 원문 8KB 이하(임시값). 처리 지연은 최대 1분 + Vectorize 반영 시간입니다.
- **아직 아님**: 클라이언트 laya 실행(모델 파일 필요), Alibaba 계정 승인과 콘솔 확인, 배포, 시크릿 설정, 0004 적용. 앱의 `text`·`text_hash` 전송과 202 처리는 구현했습니다.
- **알아둘 점**: 앱은 202를 저장 완료로 취급합니다. 나중에 욕설로 거부되면 앱은 알 수 없습니다. 앱 쪽에서 상태 조회가 필요합니다.

### embeddinggemma-2 설정 (config.json 기준, 가중치 없이 확인, 2026-10-27)

- 텍스트 백본: 24층, hidden 512, intermediate 2048, 헤드 4개 (KV 2개, head_dim 256). 전체 attention 레이어는 5·11·17·23번 (KV 1개, head_dim 512). 나머지는 슬라이딩 attention (창 512).
- RoPE base: 전체 1,000,000 / 슬라이딩 10,000. 활성화 gelu_pytorch_tanh, RMS norm eps 1e-6. 어휘 262,144.
- 출력: 768차원. 풀링은 mean, 이후 L2 정규화. 프롬프트는 질의 `task: search result | query: `, 문서 `title: none | text: `.
- 비전 소프트 토큰은 이미지 한 장당 280개. 단일 파일 `model.safetensors` 약 1.49GB (bf16).
- **확인되지 않은 것**: `hidden_size_per_layer_input` 512의 역할은 PLE 문맥 투영으로만 씁니다. 출력 프로젝션 위치는 체크포인트 키(`embedding_projection`)로 확인했습니다 (아래 텍스트 추론 연결 절). 설정 기준 텍스트 계산 값(레이어 약 118M + 임베딩 약 134M ≈ 252M)은 모델 카드의 270M과 약 18M 차이가 납니다.

### 텍스트 추론 연결 (2026-10-27)

- 구현: `app/src/embeddinggemma_text.rs` (candle, CPU, f32). config.json 구조(24층, 전체 attention 4개, KV 헤드, RoPE)를 따릅니다. `EmbeddingGemma::load(dir)`는 `tokenizer.json`과 `model.safetensors`를 읽고, `embed`는 받은 문자열을 그대로 토큰화해 임베딩합니다. 역할 접두어는 호출하는 쪽이 `with_prompt`로 붙입니다.
- 가중치: `language_model.*` 텐서만 읽습니다. 이미지·오디오 인코더 텐서는 이름으로 찾지 않습니다.
- 출력 프로젝션: `language_model.embedding_projection` (512→768, bias 없음). 평균 풀링과 선형이라 풀링 전후 순서와 결과가 같습니다. 평균 풀링 뒤 L2 정규화.
- 검증: Python 참조 구현(transformers 5.17의 Gemma4TextModel, 양방향 모드)과 fixture 6건을 비교했습니다. 토큰 id는 모두 같고 코사인 유사도는 1.000000입니다 (기준 0.999). 558토큰 문서는 양방향 슬라이딩 창(257)을 넘어서, 마스크 차이가 있으면 결과에 드러나는 길이입니다.
  - 실행: `EMBEDDINGGEMMA_DIR=<모델 폴더> cargo test --test embeddinggemma_reference -- --ignored --nocapture` (기본 `cargo test`에서는 건너뜁니다)
  - fixture 생성 스크립트는 리포지터리 밖의 로컬 참조 폴더에 있습니다. 리포지터리에서 재생성하려면 옮겨야 합니다.
- 측정 (release, CPU): 로드 2.6초, 15~24토큰 문장 약 0.12~0.16초, 558토큰 문서 2.3초. 프로세스 최대 작업 집합 약 1.7GB (폴링 값). debug 빌드는 558토큰에 72초가 걸렸습니다.
- 확인되지 않은 것:
  - 공식 `embedding_gemma2` 구현은 보지 못했습니다. transformers 개발 버전 설치가 거부되어, 설치된 transformers의 Gemma4TextModel로 참조를 만들었습니다. 그래서 참조와 같다는 것은 같은 해석을 따른다는 뜻입니다.
  - 양방향 어텐션은 config.json에 없어서 기본값으로 양방향을 골랐습니다 (문장 샘플에서 관련·무관 문장의 차이가 더 큼). 공식 구현이 인과 어텐션이면 결과가 달라집니다.
  - 토큰별 PLE 테이블이 체크포인트에 없어서 문맥 투영만 씁니다 (Gemma4TextModel의 `input_ids=None` 경로와 같음).
  - 모델 카드의 파라미터 수(270M)와 설정 기준 계산(약 252M)의 차이는 그대로입니다.

### 멀티모달 추론 연결 (2026-10-27)

- 구현: 비전 타워 (`app/src/embeddinggemma_vision.rs`: 크기 조정, 패치, 16층 타워, 3x3 풀링, 투영), 오디오 타워 (`app/src/embeddinggemma_audio.rs`: log-mel, 12층 오디오 타워, 투영), 조립 (`app/src/embeddinggemma.rs`의 `embed_parts`와 `Part`).
- 조립 순서: `[BOS] 조각들 [EOS]`. 이미지와 비디오 프레임은 boi/eoi, 오디오는 boa/eoa로 감쌉니다. 풀링과 L2 정규화는 텍스트와 같습니다.
- 검증 (transformers 5.17 참조, 같은 가중치): 텍스트 6건 코사인 1.000000. 이미지 4장은 토큰 수가 모두 같고 토큰별 코사인 0.999988 이상. 오디오 log-mel 최대 오차 4.8e-7, 토큰 코사인 0.999999. 조립 5건(텍스트, 이미지 포함 텍스트, 오디오, 이미지+오디오, 비디오 프레임 2장)은 코사인 0.999995 이상이고 토큰 수가 참조와 같습니다.
  - 실행: `EMBEDDINGGEMMA_DIR=<모델 폴더> cargo test --release --test embeddinggemma_multimodal -- --ignored --nocapture`
- 측정 (release, CPU): 이미지가 들어간 케이스는 약 7~10초 (타워와 텍스트 백본 합산). 오디오 1초 클립은 약 0.5초. 조립 테스트 전체 35초, 최대 작업 집합 약 1.9GB (폴링 값).
- 추정으로 채운 부분:
  - 조립 배치 (boi/eoi, boa/eoa, 풀링 범위)는 설치된 processor와 config.json을 따랐습니다. 공식 embedding_gemma2 (transformers 5.18 dev) 코드는 확인하지 못했습니다.
  - 비디오 timestamp는 넣지 않습니다 (processor_config의 add_timestamps false). 설치된 processor는 mm:ss 텍스트를 넣으므로, 이 부분이 실제 모델과 다를 수 있습니다.
  - 비디오 프레임 선택 (1 fps, 최대 32장, 균등 분할)은 설치본에 없는 옵션이라 재구성했습니다.
  - 이미지 처리기는 torchvision이 없어서 로직을 복사했습니다. 크기 조정은 토치 연산을 직접 따랐고, 큰 이미지에서 픽셀 값 1 차이가 일부 있습니다.
- 아직 없는 것: 비디오 파일 디코딩과 프레임 추출, 오디오 파일 디코딩 (PCM 변환). 오디오 30초 상한과 processor의 audio_seq_length 280의 관계는 확인하지 못했습니다.
- 메모리: 임베더는 텍스트, 비전, 오디오 가중치를 모두 f32로 올려 읽습니다. 메모리가 문제가 되면 bf16 저장과 지연 로드를 검토합니다.

### 남은 항목 (이번 변경에 포함되지 않음)

- **욕설 필터 구현**: laya 모델 연결 필요 (모델 파일 다운로드 허가 필요)
- **비디오·오디오 파일 디코딩**: 프레임과 PCM을 얻는 디코더는 아직 없습니다 (`embed_parts`는 디코딩된 입력을 받습니다). 실행 진입점(바이너리·Tauri 커맨드)도 없음
- **토큰 예산 검사 연결**: 텍스트는 임베더 안에서 토큰 수로 검사합니다 (`EmbeddingGemma::embed`). 파이프라인의 `check_budget` 호출은 아직 없습니다.
- **응답 본문 파싱 실패**: 서버는 저장했는데 앱은 거부로 기록하는 문제는 남아 있음
- **운영 시크릿**: `GUEST_SALT`, `AUTHOR_KEY_SECRET` 설정 여부 확인 필요
- **배포**: `npx wrangler deploy`는 아직 실행하지 않음 (시크릿 확인 후)
- **레거시 웹 기능**: 바인딩 매핑(국가→DB)을 정하기 전까지 동작하지 않음. 쿼리스트링 토큰 전송(`auth.js`)과 SQL 문자열 보간도 수정 필요


---

## 12. 동기화 보강 (2026-11-02)

점검에서 찾은 문제 중 앞쪽 네 가지를 처리했습니다. 로컬 DB 종류를 정하지 않아도 되는 범위입니다. 로컬 저장소는 기존 JSON 큐 파일 그대로입니다.

### 12-1. 앱: 접수 뒤 최종 결과 추적 (`app/src/queue.rs`)
- 서버가 202 `pending`으로 받은 항목은 큐에서 빠지면서 `tracking`(request_id → 로컬 id, 서버 id)에 기록됩니다.
- `OfflineQueue::sync_status(backend)` (파이프라인에서는 `Evaluator::sync_queue_status`)가 추적 중인 항목마다 `/evaluations/status`를 조회합니다.

  | 서버 응답 | 처리 |
  |---|---|
  | `stored` | 추적 종료. `SyncReport.stored` |
  | `rejected`, `failed` | 추적 종료, 거부로 기록. `SyncReport.rejected` (이유 포함). 이후 이 항목을 부모로 하는 답글은 `enqueue`에서 막힙니다 |
  | `pending`, `processing` | 계속 추적 |
  | 404 | 서버가 기록을 지움(거부 기록은 7일 뒤 삭제). 추적 종료, `SyncReport.missing` |
  | 연결 실패, 5xx, 401 등 | 추적을 유지하고 거기서 멈춤 (`SyncReport.stopped`) |
- 호출 시점은 앱이 정합니다. 권장: 시작할 때, `flush_queue` 뒤, 이후 주기적으로.
- `tracking`은 `#[serde(default)]`라서 이 필드가 없는 이전 큐 파일도 그대로 읽힙니다.
- 한계: 추적 중인 항목 수에는 상한이 없습니다. 서버가 계속 `pending`이면 쌓입니다. 사용자에게 어떻게 보여 줄지는 아직 정하지 않았습니다.

### 12-2. 앱: 큐 자동 저장
- `OfflineQueue::open(path)`로 열면 경로를 기억하고 다음 시점에 스스로 저장합니다.
  - `enqueue` 직후. 저장에 실패하면 항목을 큐에서 빼고 번호도 되돌린 뒤 `AppError::Storage`를 돌려줍니다 (저장되지 않은 항목은 받지 않음).
  - `flush` 끝, `sync_status` 끝. 바뀐 것이 있을 때만 한 번 씁니다. 항목마다 쓰면 벡터가 든 파일을 매번 다시 쓰게 되기 때문입니다. 저장 실패는 `FlushReport.persist_error`, `SyncReport.persist_error`로 알립니다.
- 서버가 받았는데 파일에 쓰기 전에 앱이 죽으면 같은 `request_id`로 다시 보내지고, 서버가 중복 저장하지 않습니다.
- `OfflineQueue::default()`와 `load_from`/`save_to`는 예전처럼 호출하는 쪽이 저장합니다.

### 12-3. cron: `failed` 전환 전 저장 여부 확인 (`cron/src/index.ts`)
시도 횟수 한도(10회)에 닿으면 바로 `failed`로 쓰지 않고 `inspectStored`로 D1과 Vectorize(`getByIds`)를 확인합니다.

| D1 | Vectorize | 처리 |
|---|---|---|
| 있음 | 있음 | 저장은 끝났음. 큐 행만 삭제 (`failed` 아님) |
| 있음 | 없음 | 부분 저장. Vectorize 삭제 시도 → D1 행 삭제 → `failed` |
| 없음 | 있음 | 위와 같음 (고아 벡터 정리) |
| 없음 | 없음 | `failed` |
| 확인 또는 정리 중 오류 | | `failed`로 바꾸지 않고 `pending`으로 둠. 원문과 벡터 보존, 다음 실행에서 다시 봄 |

- 같이 바꾼 것: 답글 처리에서 부모의 **대기열 행이 있는지 먼저** 봅니다. 이전에는 부모가 D1에만 저장된 중간 상태(Vectorize 미완료)에서도 답글을 저장했고, 이후 부모를 되돌리면 답글만 남을 수 있었습니다. 지금은 부모 행이 남아 있는 동안 답글을 기다립니다.
- 한계: 부모의 큐 행 삭제가 계속 실패하면 답글도 계속 기다립니다. 부모가 같은 방식으로 재시도되므로 결국 정리됩니다.

### 12-4. 리소스 ID 정리
2-1, 2-2절을 `wrangler.json` 기준으로 고쳤습니다. 실제 계정과의 일치 여부는 위 안내의 명령으로 확인해야 합니다.

### 12-5. 검증한 것과 못 한 것
- **검증함**
  - `cargo test --lib` 106개 통과 (새 테스트 9개: 추적 후 stored, 나중에 거부된 항목과 답글 차단, 동기화 중 연결 끊김과 5xx에서 추적 유지, 404, JSON 왕복과 이전 파일 호환, 자동 저장 시점, 저장 실패 시 enqueue 거부, 변화 없을 때 쓰지 않음).
  - cron은 esbuild로 번들한 뒤 가짜 D1·Vectorize로 4가지 시나리오를 실행했습니다 (부분 저장 롤백, 큐 삭제만 실패, 확인 실패 시 보존, 아무것도 없는 경우). 이 시뮬레이션은 저장소에 넣지 않았습니다.
- **못 함**: 실제 Cloudflare 리소스 조회(API 토큰 없음), 실제 D1·Vectorize 동작, cron의 `tsc` 타입 검사(`cron/`에 의존성이 설치돼 있지 않음), 앱에서 `sync_queue_status`를 부르는 실제 호출부(앱 셸이 아직 없음).


---

## 13. 컬럼 규칙 확정과 로컬 저장 구조 (2026-11-02)

### 13-1. 결정 사항
| 항목 | 결정 |
|---|---|
| `cc` | **평문 도메인 유지** (`example.com`, 소문자, 64자 이하). `hashId` 해시로 바꾸지 않습니다. Vectorize 메타데이터 필터 값이자 64바이트 인덱스 제한 때문입니다 |
| 시간 | **밀리초(UTC epoch)로 고정**. 레거시 예시 주석(`"1773529348"`)은 초 단위였으나 새 스키마에는 쓰지 않습니다 |
| `bcc` | **보류**. 서버는 항상 NULL, 로컬은 `""` |
| 저장소 | **A: Rust가 원본, Dexie는 캐시**. logis-center와 같은 구조입니다 (`commerce/src/main.ts`의 `LogisAppDB`, `lib/db.ts`: 쓰기는 Rust 먼저, 그다음 Dexie. 읽기는 Dexie 먼저, 없으면 Rust) |

로컬 원본의 저장 엔진은 **LanceDB**로 정했고 구현했습니다 (14절). 이전에는 "미정"이었습니다.

### 13-2. 표준 컬럼 (서버 `evaluations`, 로컬 저장소, Dexie가 같은 의미를 씁니다)
| 종류 | 컬럼 | 형태 | 서버(D1)에서 없음 | 로컬(Dexie)에서 없음 | 누가 정하는가 |
|---|---|---|---|---|---|
| ID | `id` | UUID v7 (36자) | 필수 | 접수 전에는 `""` | **서버**가 접수할 때. Vectorize id와 같음 |
| ID | `ref` | 부모 평가의 UUID | NULL (최상위) | `""` | 앱이 부모를 지정, 서버가 검증 |
| ID | `root` | 스레드 최상위 UUID (최상위는 자기 id) | 필수 | 접수 전에는 `""` | 서버 |
| 주소 | `from` | `0x`+40자리 소문자 | 필수 | 접수 전에는 `""` | **서버가 최종 결정**: 검증된 세션 > 클라이언트 값(임시) > IP 해시 게스트 주소. 로컬은 응답의 `from`으로 채움 |
| 주소 | `to` | `0x`+40자리 소문자 | **NULL** (ZeroAddress도 NULL로 저장) | `""` | 앱 |
| 도메인 | `cc` | 평문 소문자 도메인 | 필수 | 필수 | 앱 |
| 보류 | `bcc` | - | NULL | `""` | 쓰지 않음 |
| 시간 | `created_at`, `updated_at` | 밀리초 정수 | 필수 | 필수 | 서버가 접수 시각을 기록 (대기열의 `created_at`이 그대로 `evaluations`로 감) |

- **`ref`의 뜻**: 레거시 `items`에서는 페이지 해시였지만, `evaluations`에서는 **부모 평가의 UUID**입니다. 새 테이블이라 충돌은 없습니다.
- **로컬 전용 컬럼**(`request_id`, `text`, `text_hash`, `vector`, `state`, `attempts`)은 봉투 컬럼이 아니므로 `data.*`에 둡니다 (logis-center의 "봉투 12개는 늘리지 않는다" 규칙).
- **로컬 기본 키는 `request_id`**입니다. `id`는 서버 응답을 받은 뒤에 채웁니다.

### 13-3. NULL과 빈 문자열 변환 규칙
- 서버(D1, Vectorize)는 "없음"을 **NULL**(Vectorize 메타데이터는 키 생략)로 씁니다.
- Dexie는 **`""`**로 씁니다. IndexedDB 사양상 null은 유효한 키가 아니라서 null 값을 가진 행은 그 인덱스에서 빠지기 때문입니다 (logis-center의 `normalizeEnvelope`가 `String(x ?? "")`로 처리하는 이유로 추정합니다).
- 변환은 **한 곳**에서만 합니다: 서버 응답을 로컬에 넣을 때 NULL → `""`, 서버로 보낼 때 `""` → 생략. 아직 구현하지 않았습니다 (로컬 저장 구현 시 함께).

### 13-4. 서버와 앱의 검증 규칙 일치표
앱이 서버보다 느슨하면 서버가 400으로 거부하고, 오프라인 큐는 그 항목을 영구 거부로 버립니다. 그래서 앱이 **같거나 더 엄격**해야 합니다.

| 값 | 서버 (`www/src/evaluations.ts`) | 앱 (`app/src/pipeline.rs`) | 상태 |
|---|---|---|---|
| `cc` | `/^([a-z0-9-]{1,63}\.)+[a-z]{2,63}$/`, 64자 이하 | 같은 규칙 + 라벨 앞뒤 `-` 금지 | **이번에 수정**: TLD에 숫자·하이픈이 들어간 도메인(`example.123`)을 앱이 통과시키던 문제 |
| 주소 | `ethers.isAddress`, 소문자로 저장, ZeroAddress는 NULL | 42자 hex, 소문자로 바꿔 전송 | 일치. 소문자만 보내므로 체크섬 검사에 걸리지 않습니다 (ethers 동작 기준, 실행으로 확인하지는 않음) |
| 검색 `to` 필터 | ZeroAddress면 400 | ZeroAddress면 전송 전에 거부 | **이번에 수정** |
| 저장 `to` | ZeroAddress는 NULL로 저장 | 그대로 전송 | 일치 (의도된 동작) |
| UUID (`ref`, `root`) | 36자 hex, 대소문자 무시 | 소문자로 정규화 | 일치. 둘 다 v7인지는 검사하지 않음 |
| `request_id` | 16~64자 `A-Za-z0-9_-` | 32자 hex 생성 | 일치 |
| 벡터 | 768차원, 유한값, norm 1±0.01 | 같음 (`check_contract`) | 일치 |
| 원문 | 8192바이트 이하, SHA-256 일치 | 같은 상한, trim한 문자열로 해시 | 일치 |

### 13-5. BLOB 읽기 수정 (`cron/src/vector.ts`)
- D1은 `ArrayBuffer`로 쓴 BLOB을 **읽을 때 바이트 숫자 배열(`number[]`)로 돌려줍니다** (D1 Worker API 문서의 타입 변환 표). 이전 cron은 `new Float32Array(row.vector as ArrayBuffer)`로 읽어서, 실제 D1이라면 길이 3072의 가짜 벡터가 만들어지고 Vectorize `upsert`가 차원 불일치로 실패했을 가능성이 큽니다.
- `toVector`가 `ArrayBuffer`, 뷰, 바이트 배열을 모두 받고, 길이(3072바이트)와 바이트 값 범위를 검사한 뒤 정렬된 새 버퍼로 읽습니다. D1 행을 쓰기 **전에** 변환하므로 잘못된 BLOB은 D1에 흔적을 남기지 않습니다.
- 테스트(저장소에 포함, 의존성 없음): `node --experimental-strip-types --test cron/test/vector.test.ts` (`premium-discount` 폴더에서). Node 22.6 이상이 필요합니다.

### 13-6. 검증한 것과 못 한 것
- **검증함**
  - `cargo test --lib` 113개 통과 (새 테스트 7개: `cc` 허용·거부 규칙, 63자 라벨 경계, 주소 소문자화, UUID, 검색 `to` ZeroAddress 거부, 저장 `to` ZeroAddress 허용).
  - `toVector` 테스트 7개 통과 (ArrayBuffer, 바이트 배열, 오프셋이 있는 뷰, 길이 오류, 바이트가 아닌 값, 지원하지 않는 형태, 오류 메시지에 값이 안 들어감).
  - cron 전체 시뮬레이션: 가짜 D1이 **실제 D1처럼 바이트 배열을 돌려주는** 경우에도 768차원 벡터가 원본과 정확히 같은 값으로 `upsert`되고, 길이가 틀린 BLOB은 D1에 흔적 없이 재시도 대기로 가는 것을 확인했습니다. 12절의 4가지 시나리오도 다시 통과했습니다. 이 시뮬레이션은 저장소에 넣지 않았습니다.
- **못 함**: 실제 D1에서 BLOB이 어떤 형태로 읽히는지 직접 확인하지 못했습니다 (문서 기준). cron의 `tsc` 타입 검사 (`cron/`에 의존성 없음). 실제 Cloudflare 환경 실행.

### 13-7. 남은 일 (2026-11-02: 아래 세 항목과 저장 엔진 결정은 14절에서 처리했습니다)
- 로컬 저장 구현 시 `PendingSave`에 `created_at`(밀리초)을 추가해야 합니다. 지금 큐에는 시간 필드가 없습니다.
- 로컬 원본 엔진 결정 (JSON 유지 / LanceDB / SQLite). 빌드 환경 부담은 LanceDB의 `protoc` 요구와 링크 메모리 문제(`noto-sandbox` 기록)를 고려해야 합니다.
- 13-3의 NULL ↔ `""` 변환 함수와 Dexie 스키마 (logis-center의 봉투 12컬럼 방식을 따름).
- 레거시 `items`의 `cc`, `ref` 의미는 건드리지 않았습니다. 레거시 경로는 현재 동작하지 않습니다 (11절).


---

## 14. 로컬 원본 저장소(LanceDB)와 화면 캐시(Dexie) (2026-11-02)

13-7의 남은 일을 처리했습니다. 로컬 원본은 **LanceDB**, 화면 캐시는 Dexie입니다.

### 14-1. 흐름
```
입력 → Evaluator::submit_outbox
        ① 검사·욕설 필터·임베딩
        ② Outbox::enqueue   → LanceDB에 행을 먼저 씁니다 (state = queued). 쓰지 못하면 큐에도 넣지 않습니다
        ③ Outbox::flush     → 서버 POST → 행 갱신 (stored | pending | rejected)
        ④ Outbox::sync_status → pending 행의 최종 결과를 서버에 물어 행 갱신
Rust LanceDB (원본) ──cache_rows_since(since)──▶ Dexie (화면 캐시, app/ui/localdb.ts)
```
- `OfflineQueue`(메모리 상태 기계)는 그대로 두고, `Outbox`가 상태가 바뀔 때마다 **해당 행의 바뀐 컬럼만** 씁니다. JSON 큐처럼 벡터가 든 파일 전체를 다시 쓰지 않습니다.
- 앱을 다시 열면 `queued`와 `pending` 행에서 큐를 되살립니다 (순서는 `created_at`, 같으면 `local-N` 번호).

### 14-2. 테이블 `evaluations_local` (`app/src/local.rs`)
| 컬럼 | 형태 | 비고 |
|---|---|---|
| `request_id` | Utf8 | **기본 키** (멱등 키). 쓰기는 `merge_insert(request_id)`라서 다시 써도 중복되지 않음 |
| `local_id` | Utf8 | `local-N`. 답글의 부모 지정용. 재시작 뒤에도 번호가 이어짐 |
| `id`, `root`, `ref` | Utf8, nullable | 서버가 정한 UUID, 스레드 최상위, 서버 id로 정해진 부모 |
| `parent_local` | Utf8, nullable | 부모가 아직 서버에 없을 때 부모의 로컬 id |
| `cc` | Utf8 | 평문 도메인 |
| `from`, `to`, `bcc` | Utf8, nullable | `from`은 서버 응답으로 채움. `bcc`는 항상 NULL(보류) |
| `text`, `text_hash` | Utf8 | **원문은 서버에 남지 않으므로 이 저장소가 유일한 원본** |
| `vector` | FixedSizeList<f32, 768>, nullable | 코사인 거리로 검색 |
| `state` | Utf8 | `queued` `pending` `stored` `rejected` `failed` `missing` |
| `attempts` | Int32 | 재시도 횟수 (재시작 뒤에도 유지) |
| `reason` | Utf8, nullable | 거부·실패 이유 |
| `created_at`, `updated_at` | Int64 | **밀리초** |

- 이 저장소에서 "없음"은 **NULL**입니다. Dexie로 내보낼 때만 `""`로 바꿉니다 (`none_to_empty`, `to_cache_row`. 13-3의 변환 규칙이 코드로 한 곳에 있습니다).
- 이미 있는 테이블의 컬럼이 이 버전의 스키마와 다르면 **조용히 덮어쓰지 않고 오류로 멈춥니다**.

### 14-3. 상태 전이
| 시점 | 행 상태 |
|---|---|
| `enqueue` | `queued` (`created_at`, 원문, 벡터 기록) |
| 서버가 200(저장됨) | `stored` + `id`, `root`, `ref`, `from` 기록 |
| 서버가 202(접수) | `pending` + 같은 컬럼 기록. 큐의 추적 목록에 들어감 |
| `sync_status`가 stored | `stored` |
| 서버가 거부·실패 (flush 응답, 영구 오류, `sync_status`) | `rejected` + `reason`. 이 행을 부모로 하는 답글은 앱을 다시 켜도 막힘 |
| 서버가 404 | `missing` (부모로는 쓸 수 없음) |
| 일시 오류 재시도 | `queued` 유지, `attempts` 증가 |

### 14-4. 중간에 앱이 죽었을 때
- **enqueue 중**: 행을 먼저 쓰고 큐에 넣습니다. 행을 쓰지 못하면 큐에서도 빼고 번호도 되돌립니다.
- **서버가 받았는데 행을 갱신하기 전에 죽음**: 다시 열면 그 행은 `queued`입니다. 같은 `request_id`로 다시 보내면 서버가 중복 저장하지 않고 처음 결과를 돌려줍니다 (테스트: `a_crash_after_the_server_accepted_resends_with_the_same_request_id`).
- 갱신 쓰기가 실패하면 `FlushReport.persist_error`, `SyncReport.persist_error`로 알립니다 (메모리 상태는 이미 바뀜).

### 14-5. 로컬 검색 (내 글)
- `Evaluator::search_local(text, top_k, cc, &outbox)`: 질의 접두어(`task: search result | query: …`)로 임베딩해서 LanceDB 코사인 검색을 합니다. 서버에 가지 않고 **원문과 함께** 돌려줍니다 (`LocalMatch.text`).
- `score`는 코사인 유사도(1에 가까울수록 비슷함)로, 서버 검색의 `score`와 같은 방향입니다.
- `rejected`, `failed`, `missing` 행과 벡터가 없는 행은 검색에서 빠집니다. `top_k`는 1~50, `cc`는 서버와 같은 규칙으로 검증합니다.

### 14-6. 화면 캐시 Dexie (`app/ui/localdb.ts`)
- Rust에서만 채웁니다 (`Outbox::cache_rows_since(since_ms)` → 행 JSON). 화면은 직접 평가를 만들거나 고치지 않습니다. 벡터는 보내지 않습니다.
- logis-center의 봉투 규칙(`from`, `to`, `cc`, `bcc`, `ref` 이름, 나머지는 `data.*`)을 따르되 **두 가지가 다릅니다**: 기본 키가 `request_id`이고(`id`는 접수 전에 `""`이라 키가 될 수 없음), `root`와 `state`는 인덱스라서 최상위에 둡니다.
- 스키마: `request_id, id, root, ref, cc, from, to, state, created_at, updated_at, [cc+state], [root+created_at], [state+created_at]`.
- `syncFromRust(db, fetchRows)`: 커서(마지막 `updated_at`)를 한 칸 겹쳐서 요청하고, 받은 행을 **모두 검사한 뒤에** 쓰고, 행을 쓴 다음에 커서를 씁니다. 한 행이라도 틀리면 아무것도 쓰지 않습니다. 커서를 지우면 Rust에서 처음부터 다시 채웁니다.
- **계약 픽스처** `app/ui/fixtures/cache_rows.json`: Rust(`to_cache_row`)와 JS(`normalizeRow`)가 같은 파일로 검증합니다. 한쪽 모양을 바꾸면 양쪽 테스트가 깨집니다.

### 14-7. 이전 JSON 큐 옮기기
`Outbox::import_json_queue(path)`: 못 보낸 항목은 `queued`로, 접수만 된 항목은 `pending`으로 옮깁니다. 같은 `request_id`는 건너뛰므로 다시 실행해도 안전합니다. 이전 `local-N` 번호는 현재 가장 큰 번호만큼 밀어서 겹치지 않게 하고, 답글의 부모 지정도 따라 바꿉니다.
- **한계**: 이전 JSON에는 접수 완료된 항목의 원문과 벡터가 없습니다. 그런 행은 원문이 비어 있고 벡터가 없어서 검색에 나오지 않습니다.
- `OfflineQueue`의 JSON 저장(`open`, `save_to`)과 `submit_queued`는 남겨 두었습니다 (이전 파일을 읽는 용도와 기존 테스트).

### 14-8. 빌드 환경 (이 PC에서 측정)
- `protoc`가 필요합니다. 이 PC에는 `C:\protoc\bin\protoc.exe`(35.1)가 있고, 빌드할 때 `PROTOC` 환경 변수로 지정했습니다. PATH에도 잡혀 있지만 백그라운드 실행에서는 환경 변수를 직접 넘겨야 했습니다.
- 의존성을 처음 컴파일할 때 약 3분 (`CARGO_BUILD_JOBS=4`, RAM 15.9GB 중 여유 7GB), 이후 테스트 빌드는 30~50초였습니다. `noto-sandbox` 기록에 있던 링크 메모리 부족은 이 PC에서는 나타나지 않았습니다. 더 작은 장비에서는 `CARGO_BUILD_JOBS`를 낮추세요.
- `app/target` 폴더가 12.7GB입니다 (기존 candle 빌드 포함, LanceDB만의 몫은 분리해 측정하지 않았습니다). git에는 포함되지 않습니다.

### 14-9. 검증한 것
- `cargo test --lib` **153개 통과** (이전 113개 + 40개). **실제 LanceDB**를 임시 폴더에 열어서 확인했습니다.
  - `local.rs` 17개: 열고 다시 열기, `request_id` 기준 갱신(중복 없음), 모든 컬럼과 NULL 왕복, 벡터 없이 읽기, 일부 컬럼만 갱신하고 NULL로 바꾸기, 따옴표가 든 값과 주입 시도, 코사인 순위와 점수(동일 1.0, 45도 약 0.707, 직교 0), 거부·실패·벡터 없는 행 제외와 `cc` 필터, 빈 테이블 검색, 차원 오류, 다른 스키마 거부, 숫자 기준 최대 번호, `optimize` 뒤에도 데이터 유지, 계약 픽스처.
  - `outbox.rs` 17개: 쓰기 선행, 재시작 후 큐 복원과 순서, 서버 필드 기록, `pending` 추적의 재시작 유지, 늦은 거부와 재시작 뒤 답글 차단, 큐에 있는 부모와 저장된 부모에 대한 답글, 모르는 부모 거부, 서버 수락 뒤 크래시 재전송(서버는 한 번만 저장), 재시도 횟수 유지, 영구 거부, 404, 쓰기 실패 시 큐 롤백, 로컬 검색, Dexie 행 모양, JSON 이전(번호 이동, 중복 방지, 이전 뒤 전송).
  - `pipeline.rs` 6개: `submit_outbox`, 욕설 입력은 로컬에도 안 남음, 오프라인 후 재시작·전송, `search_local`, 검증, `sync_outbox_status`.
- `node --experimental-strip-types --test app/ui/localdb.test.ts` **17개 통과** (Node 22.6 이상. Dexie는 설치하지 않고 같은 인터페이스의 메모리 가짜 사용).
- cron `toVector` 테스트 7개와 cron 시뮬레이션은 변경이 없고 그대로 통과합니다 (13절).

### 14-10. 한계와 못 한 것
- **원문과 벡터가 로컬 디스크에 평문으로 저장됩니다.** LanceDB 폴더 암호화는 하지 않았습니다.
- **벡터 인덱스가 없어서 검색은 전수 비교입니다.** 확인한 규모는 30건뿐이고, 수만 건에서의 속도는 측정하지 않았습니다.
- **`optimize`가 파일 수를 바로 줄이지는 않았습니다.** 작은 쓰기 60번 뒤 데이터 파일이 60개, `optimize` 뒤 61개였습니다. 기본 정리가 7일 지난 버전만 지우기 때문으로 보이며, 장기 디스크 증가는 확인하지 못했습니다. 앱을 열 때 자동 실행하고(`Outbox::open`), `Outbox::maintain()`으로도 부를 수 있습니다.
- **쓰는 쪽은 한 곳이라고 가정합니다** (`Outbox`가 `&mut`). 앱을 두 프로세스로 띄워 같은 폴더를 쓰면 LanceDB 커밋이 충돌할 수 있는데 확인하지 않았습니다.
- **Dexie는 실제 IndexedDB로 확인하지 못했습니다.** 메모리 가짜로 로직만 검증했습니다. 화면 셸(Tauri 명령으로 `cache_rows_since`를 노출하고 `syncFromRust`를 부르는 곳)은 아직 없습니다.
- 로컬에서 평가를 지우는 기능은 없습니다 (그래서 Dexie 쪽 삭제 표식도 필요 없습니다).
- 서버가 기록을 지운 항목(`missing`)은 부모로 쓸 수 없게 했습니다. 서버가 "부모를 찾을 수 없다"고 거부하기 때문입니다.
- `pending`인데 서버 `id`가 없는 행은 손상으로 보고 `Outbox::open`이 오류로 멈춥니다 (조용히 무시하지 않음).

## 15. 보완 개발 (2026-11-14, 스키마 무수정)

**원칙**: 외부 멀티모달 API를 쓰지 않고, DB 스키마를 바꾸지 않았습니다. `www/migrations/`, `app/src/local.rs`(`LocalRecord`), `app/ui/localdb.ts`(Dexie)는 변경이 없습니다 (`git diff`로 확인). 앱 큐 JSON(`PendingSave`)에 선택 필드 `bcc`가 늘었지만 `#[serde(default)]`라 이전 파일도 그대로 읽힙니다.

### 15-1. 항목별 결과

| 영역 | 항목 | 결과 | 비고 |
|---|---|---|---|
| cron | `index.ts` 하단에 잘못 붙은 `www` 코드 제거 | 완료 | `tsc --noEmit`, `wrangler deploy --dry-run` 번들 통과. 일회용 `_fix_tail.cjs`도 삭제 |
| cron | 욕설·음란(`nsfw`)·불법(`illegal`) 세 질문을 한 호출에 | 완료 | 하나라도 0.5 이상이면 거부. 거부 사유(`last_error`)는 걸린 질문 이름(예: `nsfw,illegal`). 질문이 하나라도 빠진 응답은 통과시키지 않는다 (fail-closed) |
| cron | 거부·실패 행의 `text_hash` 소거 | 완료 | 더미 해시(`0`×64)로 덮어쓴다. 같은 이유로 `bcc`(콘텐츠 해시 주소)도 NULL로 지운다 |
| cron | 부모가 `pending`일 때 답글 순서 | 변경 없음 | 이미 구현되어 있었다 (부모가 대기열에 있으면 `release`). 테스트로 고정했다 |
| app | `ProfanityFilter` 구현체 | 완료 (`LocalFilter`) | 사전 기반이라 문맥은 모른다. 정밀도 우선(예: "당시 발견"을 "시발"로 오탐하지 않는다). laya 모델 필터는 **붙이지 못했다** |
| app | 실행 진입점과 UI 명령 | 완료 | `cmd_*` 11개(`commands.rs`), 표준 입출력 실행 파일(`main.rs`), 화면 래퍼(`ui/commands.ts`). Tauri 창은 아래 행 |
| app | **웹뷰 화면** (입력 폼, 내 평가 상태 목록, 검색) | 완료 (Tauri 창 제외) | `app/ui`. 폼에서 `cmd_submit_evaluation`을 부르고, Rust가 내보낸 행을 Dexie 캐시에 반영해 전송 대기·검수 중·등록됨·거부됨(사유)을 보여 준다. 검색은 `cmd_search_merged`. 로그인 화면은 아래 QR 로그인 행 |
| app | **Tauri 창** (`app/src-tauri`) | 완료 (Linux에서 확인) | Tauri v2 크레이트. 코어는 첫 명령에서 만들고, 만들지 못하면 화면이 원인과 해결 방법(모델 폴더 지정)을 보여 준다. 설정은 환경 변수 > `config.json` > 기본값. **설치 파일 번들과 Windows·macOS는 확인하지 못했다.** **Android(arm64) 디버그 APK 빌드까지 성공, 기기 실행은 미확인** (15-6) |
| app/www | **QR 로그인** | 완료 (세션 발급·메일 도착 부분은 가정, 아래 15-5) | 서버가 세션 발급(`POST /session/start`)과 확인(`GET /session`), 앱이 QR 로그인 창·세션 저장·복원·로그아웃. 로그인하면 등록의 `from`이 내 계정 주소가 된다 |
| app | 옛 `app/embedding.rs` 삭제 | 해당 없음 | 이 브랜치에는 이미 없었다 |
| app | 토큰 예산 사전 검사 | 완료 (텍스트 경로) | 필터·모델·서버 호출 전에 `InvalidInput`. 이미지·오디오·비디오 조각 경로(`embed_parts`)는 바꾸지 않았다 |
| app | `bcc` 연결 | 완료 | `SubmitInput`→`SaveRequest`→서버, 대기열→`LocalRecord`→화면 캐시까지 같은 값. 값 계산(`hashId`)은 호출하는 쪽이 한다 |
| app | 2xx 응답 본문 실패 복구 | 완료 | 본문이 끊기거나 JSON이 잘리면 `Offline`(횟수 없이 같은 `request_id`로 재전송). 계약과 다른 완전한 본문은 새 `BadResponse`(재시도 한도 있음) |
| app | 로컬 AI 스케줄러 (`items`/`crons`, Qwen3.5 0.8B) | **미착수** | 모델이 필요하고, `items` 연동 계약이 저장소에 코드로 정의되어 있지 않다 (`www/src/index.ts` 주석뿐) |
| www | `verifiedAddress` 연결 | 완료 (기본 꺼짐) | S3 `hash/<hash>` 조회(`aws4fetch`). `SESSION_S3_*` 설정이 있을 때만 동작하고, 무엇이든 실패하면 기존처럼 null. **객체 형식(token, email, ip)은 `auth.ts` 주석의 TODO에서 가정한 것이라 실제 객체로 확인해야 한다** |
| www | CORS 허용 헤더 | 완료 | `Content-Type, x-oauth-hash, x-oauth-token`. status 조회를 위해 GET도 허용 |
| www | 요청 제한 | 완료 | Workers Rate Limiting 바인딩(IP 키). 저장 분당 20, 읽기(검색·그래프) 분당 60은 **임시값**. status 조회는 제한하지 않는다 |
| www | 그래프 뷰 로컬 원문 오버레이 | 완료 | 내 기기의 `PremiumDiscountLocal`에 있는 글만 패널에 원문을 보여 준다. 실제 Chromium으로 확인 |
| www/app | 로컬+서버 검색 병합 | 완료 (앱 쪽) | `Evaluator::search_merged`, `cmd_search_merged`. 방금 쓴 글이 서버 검색에 나오기 전에도 보인다. 검색 화면 자체는 이 저장소에 없다 |

### 15-2. 배포 전 설정

- **cron**: 마이그레이션은 필요 없다 (0005까지 그대로). `wrangler deploy`만 하면 된다.
- **www**: `wrangler.json`의 `ratelimits` 두 개(`EVAL_WRITE_LIMITER`, `EVAL_READ_LIMITER`)는 배포하면 만들어진다. 임계값은 운영하면서 조정한다.
- **세션 검증을 켜려면** 시크릿 네 개가 모두 필요하다: `SESSION_S3_BUCKET`, `SESSION_S3_REGION`, `SESSION_S3_ACCESS_KEY_ID`, `SESSION_S3_SECRET_ACCESS_KEY` (`s3:GetObject`만 허용한 읽기 전용 키). 켜기 전에 실제 `hash/<hash>` 객체 한 건으로 형식을 확인한다. 하나라도 없으면 꺼진 채로 동작한다.

### 15-3. 검증한 것

- cron: `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test cron/test/*.test.ts` **26개 통과**. D1은 실제 SQLite(`node:sqlite`)에 `www/migrations/0002~0005`를 적용한 가짜라서, `cron/src/index.ts`의 SQL이 그대로 실행된다. 일부러 `text_hash` 소거를 되돌려 보니 3개가 실패했다.
- www: `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts` **22개 통과** (세션 검증은 가짜 S3, 서명은 실제 계산). `tsc`는 레거시 `index.ts`·`renderHtml.ts`의 기존 오류만 남고 변경한 파일은 오류가 없다. `wrangler deploy --dry-run` 통과.
- app: `cargo test --lib` **213개 통과**(기존 153개 + 60개). 2xx 복구 테스트는 로컬 TCP 서버로 끊긴 응답을 만들고, 일부러 `Offline` 분류를 `BadResponse`로 바꾸면 실패한다. `cargo clippy` 경고 수는 변경 전과 같다 (15개).
- app UI: `node --experimental-strip-types --test app/ui/*.test.ts` **21개 통과**.
- **실모델 검증 (2026-11-14, 같은 날 추가)**: `google/embeddinggemma-2`를 받아서 확인했다 (CPU, 릴리스 빌드).
  - `cargo test --release --test embeddinggemma_reference -- --ignored`: Python 참조와 **6개 케이스 모두 코사인 1.000000** (558토큰 긴 입력 포함, 한 건당 약 0.15초, 긴 입력 3초).
  - `app/tests/e2e/`: 실제 모델로 `main`을 끝까지 실행. 가짜 서버로 저장 3건(서버가 받은 벡터는 768차원, norm 1.0, `bcc` 전달), 욕설 입력은 서버로 가지 않고 로컬에도 남지 않음, 한국어 질의로 영어 글 찾기(0.799)와 병합 검색(`both`로 중복 제거), 캐시 동기화, `flush` 뒤 `pending`→`stored`까지 통과. 시작 3초, 저장 0.15~0.18초.
  - 질의와 문서는 모델 접두어가 달라 같은 문장도 점수가 1.0이 아니라 0.951이다 (정상).
- **앱 화면 (2026-11-14)**: `app/ui`의 단위 테스트 30개(문구·상태·오류 변환 등)와 `app/tests/e2e/ui_e2e.mjs` **12단계 통과**. 실제 모델이 연결된 실행 파일을 `dev-server.mjs` 브리지로 붙이고 Chromium으로 화면을 직접 썼다 (서버만 가짜). 입력 오류 안내, 욕설 차단(서버 전송·목록 없음), 등록→검수 중→등록됨/거부됨, 답글의 `ref` 전달, 검색(내 글은 원문, 다른 사용자는 원문 없음), 오프라인 저장→복구, 새로고침 뒤 목록 유지(**실제 IndexedDB**에서 Dexie 캐시 확인), 다크 테마. 캡처를 눈으로 확인해 단언이 놓친 문제 4건(검색 버튼 줄바꿈, 목록 깜빡임, 출처 라벨 오표기, `display:flex`가 `hidden`을 덮어쓰던 답글 막대)과 거부 사유 형식 변환 오류를 고쳤다. 거부 사례는 가짜 서버가 흉내 낸다 (실제 판정은 cron의 결정 모델).
- **QR 로그인과 Tauri 창 (2026-11-14, 같은 날 추가)**
  - www `node --test` 63개(세션 15개 포함), 앱 `cargo test --lib` **213개**, Tauri 크레이트 설정 테스트 4개, UI 단위 **46개** (QR은 `jsQR`로 매트릭스를 읽어 mailto 링크를 확인).
  - `ui_login_e2e.mjs` 15단계(Chromium, 모바일 메일 링크 4단계 포함): 브라우저에서 실제로 그린 QR을 스캐너로 디코드, 주소 복사, 메일 도착 → 로그인, 세션 파일 0600, 로그인한 세션 헤더로 등록(서버가 내 계정으로 저장), 재시작 복원, 로그아웃, 서버 미설정 안내, 닫으면 폴링 중지, 다크·390px.
  - `tauri_e2e.mjs` 10단계(**진짜 Tauri 창**, Xvfb + WebKitWebDriver, 실제 모델): 모델 폴더가 없을 때 시작 실패 안내, `window.__TAURI__`와 IPC로 첫 화면, 실제 키 입력(한글 포함)으로 등록(768차원 단위 벡터), 욕설 차단, QR 로그인, 세션 파일, 로그인 세션 헤더로 등록, 앱 재시작 뒤 로그인과 목록 유지, 로그아웃.
  - 일부러 구현을 깨서 확인: 세션 검증(발급하지 않은 hash·변조된 token, 3개 실패), 겹친 갱신 합치기(3개 실패).
  - 캡처와 진단으로 찾아 고친 문제: 로그인 전 "게스트"가 먼저 보이던 것, 겹친 갱신(등록 직후의 목록 갱신이 진행 중인 갱신과 겹치면 버려지던 것 — E2E에서 관찰한 것이 아니라 코드를 따라가며 찾은 가능성이고 단위 테스트로 고정). Tauri E2E의 한 번의 실패는 앱 문제가 아니라 WebView 저장소가 실행 사이에 남던 검증 격리 문제였다.
- 그래프 뷰 오버레이: Playwright + Chromium으로 4가지 상황 확인 (내 글, 남의 글, 노드 전환 경쟁, 캐시 없음). 네트워크는 가짜로 응답하게 했다.

### 15-4. 확인하지 못한 것과 한계

- **Alibaba 결정 API에 질문 세 개를 동시에 보내는 것**은 기획안의 설명(최대 16개)을 따랐고 실제로 호출해 보지 않았다. 질문 문구도 임시다. 허용하지 않으면 응답이 실패하고 재시도 뒤 `failed`가 된다 (통과되지는 않는다).
- `LocalFilter`는 사전이 작다. 오탐 방지를 위해 일부러 보수적이며, 놓친 것은 서버 결정 모델이 걸러야 한다.
- **laya-multilingual은 아직 붙이지 못했다.** 모델과 구조는 확인했다: mmBERT-base(ModernBERT 22층, 768) 인코더 + 직접 학습한 결정 헤드(2층 트랜스포머, 옵션 마커 점수기, 질문 유형 임베딩). Alibaba 결정 API와 같은 계열의 모델이지만 단순 분류기가 아니라서, Rust(candle)로 옮기려면 질문 시퀀스 조립(`[CLS] <유형> 지시문 [SEP] [MASK] 옵션… [SEP] 상태 [SEP]`)까지 재구현하고 PyTorch 참조와 수치를 맞춰야 한다. 모델 카드가 밝힌 한계(보정되지 않아 과신, `noul`이 "예"를 낮게 보고할 수 있음) 때문에 한국어 욕설 판정 품질은 라벨링된 자료로 따로 검증해야 한다.
- 세션 검증은 S3 객체 형식 가정에 기대고 있고, 요청마다 S3를 한 번 읽는다 (캐시 없음).
- 로컬 검색은 `cc` 필터만 지원한다. `root`·`ref`·`to` 필터가 있으면 병합 검색은 서버 결과만 쓴다.

### 15-5. QR 로그인과 Tauri 창: 설계와 한계

**설계 결정 (되돌릴 수 있음)**: 앱 세션을 이 저장소의 Worker가 발급합니다. 외부 `cdn.oauth.network`의 세션 발급 규격이 저장소에 없어서, 규격을 추측하지 않고 저장소가 필요 없는 방식으로 만들었습니다. 규격을 알려 주시면 발급만 바꿔 끼울 수 있습니다 (`app/src/api.rs`의 `session_start`).
- 발급: `token = HMAC-SHA256(SESSION_SECRET, "session|" + hash)`. 저장하지 않고 검증할 때 다시 계산합니다. `hash`는 QR에 보이지만 `token`은 앱에만 전달되므로 hash만 아는 사람은 로그인할 수 없습니다.
- 로그인 완료: 웹의 QR과 같은 `mailto:<hash>.premium.discount@oauth.email`로 사용자가 메일을 보내면 SES가 S3 `hash/<hash>`에 `email`을 기록하고(10-3), 서버가 그 `email`을 확인하면 끝납니다. `from`은 `computeAddress(hashMessage(email))`입니다. 메일은 폰에서 보내므로 앱 세션은 ip를 비교하지 않습니다.
- 기존 웹 세션(S3의 token 일치) 검증은 그대로입니다.

**배포 전 확인할 것**
- 시크릿 `SESSION_SECRET` 설정 (`npx wrangler secret put SESSION_SECRET`). 없으면 `POST /session/start`가 503이고 앱은 "로그인이 아직 설정되지 않았습니다"를 보여 줍니다. 이 값을 바꾸면 발급된 모든 앱 세션이 무효가 됩니다.
- **가정**: SES가 메일을 받으면 `hash/<hash>` 객체가 **미리 없어도** `email`을 기록한다고 봤습니다 (README 10-3의 설명). 앱 세션의 hash는 cdn이 만들지 않았기 때문입니다. 실제 메일 한 통으로 확인해야 합니다. 객체가 미리 있어야 한다면 앱 세션은 메일이 도착해도 `pending`에 머뭅니다.
- S3 읽기 키가 `s3:GetObject`만 가지면 없는 키에도 403이 옵니다. 서버는 403과 404를 모두 "아직 없음"으로 보므로 권한 설정 오류가 "영원히 대기"로 나타납니다. 구분하려면 `s3:ListBucket`을 허용합니다.

**한계**
- QR을 본 사람이 자기 메일을 보내면 그 앱이 그 사람 계정으로 로그인됩니다 (웹의 QR 로그인과 같은 성질). QR은 앱 화면에만 보이고 15분 뒤 만료됩니다.
- 세션 파일(`session.json`)은 평문이고 소유자만 읽도록(0600) 합니다. 운영체제 키체인에는 넣지 않았습니다.
- Tauri 창은 Linux(WebKitGTK)에서 가상 디스플레이로만 실행해 봤습니다. 설치 파일 번들, Windows(WebView2), macOS는 확인하지 못했습니다. 디버그 빌드는 모델 로드에 약 40초가 걸립니다 (릴리스는 약 3초).
- WebView 캐시(Dexie)는 Rust 쪽 데이터가 지워져도 이전 행을 그대로 보여 줍니다. 로컬 삭제 기능이 없다는 설계(14절) 때문에 삭제 표식이 없습니다.
- 모델 폴더를 고르는 화면은 없습니다. 설정 파일이나 환경 변수로 지정합니다.

### 15-6. 모바일(Android) 대응과 메일 링크 로그인 (2026-11-14)

**질문: Tauri 앱을 활용해 Android 앱을 만들 수 있는가?** **됩니다. 이 환경에서 실제로 arm64 Android용 디버그 APK까지 빌드했습니다.** 다만 기기나 에뮬레이터에서 실행해 보지는 못했습니다 (아래 "확인하지 못한 것").

**한 일**
- **로그인 방식 분기**: Rust가 `cmd_app_status.platform`(`android`·`ios`·`desktop`)을 알려 주고, 모바일에서는 QR 대신 **"메일 앱으로 로그인"** 링크를 보여 줍니다. 누르면 `mailto:<hash>.premium.discount@oauth.email`(제목·본문 채움)로 메일 앱이 열리고, 사용자가 **보내기만 누르면** 됩니다. 서버·세션·폴링은 QR과 같습니다 (`cmd_login_start`/`cmd_login_status`). 메일 앱에서 돌아오면 바로 한 번 확인합니다.
- **모바일 빌드 구조**: `src-tauri`를 `lib.rs`(앱 본체, `run()`에 `#[cfg_attr(mobile, tauri::mobile_entry_point)]`)와 얇은 `main.rs`로 나누고 `[lib] crate-type = ["staticlib", "cdylib", "rlib"]`을 선언했습니다. 데스크톱 동작은 그대로입니다 (진짜 창 E2E 10단계 통과).
- **외부 링크 열기**: `tauri-plugin-opener`를 넣고 권한은 `mailto:*`만 허용합니다 (`opener:default`는 http(s)도 열어서 쓰지 않음).
- **Android 프로젝트**: `cargo tauri android init`으로 `app/src-tauri/gen/android`(Gradle 프로젝트, 소스만 584KB)를 만들어 저장소에 넣었습니다. 패키지 `discount.premium.app`, `minSdk 24`(Android 7.0), 권한은 `INTERNET`뿐입니다. `local.properties`와 빌드 산출물은 제외됩니다.
- **빌드에서 막혔던 것 1건**: 첫 시도에서 candle이 쓰는 `gemm-f16`이 aarch64에서 `instruction requires: fullfp16`으로 컴파일되지 않았습니다. 최적화 없는 빌드에서 fp16 어셈블리 헬퍼(`#[inline]`)가 fp16 기능이 꺼진 함수로 따로 만들어지기 때문입니다 (릴리스는 인라인되어 통과할 것으로 보이나 이번에 확인하지 않았습니다). `gemm-common`·`gemm-f16`만 `opt-level = 3`으로 올려 해결했습니다 (`[profile.dev.package.*]`, 두 매니페스트에 같은 값). 그 외 의존성(LanceDB·datafusion·candle·onig·ring)은 **수정 없이 aarch64 Android로 컴파일됐습니다.**
- **디스크**: 모바일 빌드는 의존성 전부를 `.a`·`.so`로 묶어서 디버그 정보가 있으면 수 GB가 됩니다. `[profile.dev] debug = 0`을 두 매니페스트에 같은 값으로 넣었습니다 (다르면 같은 의존성을 두 번 빌드합니다). 그래서 디버그 빌드의 역추적에는 줄 번호가 없습니다.

**빌드 방법 (이 환경에서 실행해 성공한 순서)**
```bash
# 준비: JDK 17+(여기서는 21), Android SDK(platform-tools, platforms;android-34, build-tools;34.0.0), NDK 27.2(ndk;27.2.12479018)
export ANDROID_HOME=/opt/android-sdk NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018
rustup target add aarch64-linux-android            # init이 나머지 세 타깃도 설치한다
cargo install tauri-cli --version "^2" --locked    # 2.12.4로 확인
cd app/ui && npm install && npm run build && cd ../src-tauri
cargo tauri android build --debug --target aarch64 --apk
# → gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```
다른 PC에서는 `gen/android`가 이미 있으니 `init`은 필요 없고 SDK·NDK 경로만 맞추면 됩니다. 이 클라우드 환경에서는 SDK 내려받는 호스트(`dl.google.com`)를 허용 목록에 추가해야 했습니다.

**검증한 것**
- 디버그 APK **475MB**(심볼이 들어 있는 `.so` 489MB 포함. 릴리스 빌드는 하지 않았고 훨씬 작아질 것으로 예상되지만 수치는 모릅니다). `aapt2 dump badging`: 패키지 `discount.premium.app`, `sdkVersion 24`, `native-code: arm64-v8a`, 권한 `INTERNET`. `.so`는 AArch64 ELF이고 Tauri 안드로이드 연결부의 JNI 진입점(`Java_discount_premium_app_Rust_onCreate`, `_ipc`, `_handleRequest` 등)을 내보냅니다.
- UI 단위 테스트 **46개** (플랫폼 판정·`mailto` 링크 인코딩 포함). `ui_login_e2e.mjs` **15단계**(Chromium, `?platform=android`로 모바일 화면): 링크가 올바른 `mailto:`를 가리키는지, 메일이 도착하면 로그인되는지, 메일 앱에서 돌아올 때(`visibilitychange`) 바로 확인하는지 포함.
- 진짜 Tauri 창(`tauri_e2e.mjs`): `platform`이 `desktop`으로 오고, 외부 열기는 https가 거부되고 `mailto:`는 권한을 통과합니다 — **10단계 모두 통과**. `mailto:` 열기 호출 자체의 결과는 실행마다 달랐습니다 (첫 실행은 `gio open` 실패, 이후 실행은 성공 보고). 화면이 없는 가상 환경이라 메일 앱이 실제로 뜨는지는 볼 수 없으므로, **권한이 통과하는 것까지만 확인했고 메일 앱이 실제로 열리는 것은 확인하지 못했습니다.** 권한을 일부러 `opener:default`로 넓히면 이 검사가 실패합니다 (https를 열려고 시도함).
- `cargo test --lib` 213개, `src-tauri` 설정 테스트 4개 통과.

**확인하지 못한 것 / 한계**
- **기기·에뮬레이터 실행**: 이 환경에는 KVM이 없어 에뮬레이터를 못 돌립니다. 설치, 웹뷰에서의 화면 동작, 모델 로드, 메일 앱 열기, 앱 복귀 뒤 로그인 확인은 **전부 실기기에서 확인해야 합니다.**
- **모델**: embeddinggemma-2는 약 1.5GB이고 이미지·오디오 타워까지 올리면 메모리가 약 3.2GB입니다. 폰에서는 부담일 수 있어 텍스트 전용 로드 설정이 필요할 수 있습니다 (지금은 없음). 앱에는 모델 내려받기가 없습니다. Android에서는 환경 변수를 쓸 수 없고 설정 파일(`config.json`)이 앱 전용 폴더에 있어 사용자가 모델을 직접 넣기 어렵습니다 → 앱이 모델을 내려받는 기능이 필요합니다.
- **데이터 폴더**: `app_data_dir()`(Tauri가 플랫폼별로 풀어 줌)를 쓰지만 Android에서의 실제 경로와 쓰기는 확인하지 못했습니다.
- **빌드 범위**: aarch64 디버그 APK만 만들었습니다. 릴리스(최적화·서명), armv7·x86(에뮬레이터용) 타깃, AAB(스토어 배포)는 시도하지 않았습니다. 디버그 빌드의 모델 추론은 매우 느릴 것이라(데스크톱 디버그도 모델 로드 40초) 실제 폰 성능은 릴리스로 따로 봐야 합니다.
- **메일 링크 로그인**: 메일 앱이 설치·설정되어 있어야 하고 사용자가 보내기를 눌러야 합니다 (자동 전송 불가). `mailto:`를 처리하는 앱이 없으면 열리지 않아서 화면에 주소도 같이 보여 줍니다(직접 보내기). SES가 메일을 받기까지의 지연도 있습니다.
- iOS는 같은 구조로 가능하지만 Mac과 Xcode가 필요합니다. 시도하지 않았습니다.

## 16. 메모리 최적화, 모델 내려받기, 텍스트 전용, 양자화 (2026-11-14)

요청: logis-center의 비전·언어 가중치 스위칭(VRAM 최적화)을 참고하고, "4계층 피라미드 양자화·비대칭 Trimming·Fused 런타임" 지시서를 반영하며,
앱의 모델 내려받기와 텍스트 전용 로드 설정을 같이 만든다. 이 앱은 CPU(candle)로 돌기 때문에 VRAM 대신 **RAM**이 대상이다.
모든 cargo 빌드·테스트는 지시서대로 `-j 1`로 돌렸고, 단위 테스트와 부분 프로브(`examples/model_probe.rs`)를 먼저 맞춘 뒤 전체 참조 테스트를 돌렸다.

### 16-1. 결과 요약 (같은 PC, 릴리스, 실제 모델)

| 항목 | 전 | 후 |
|---|---|---|
| 텍스트 참조 테스트 최대 상주 메모리 | 4,494MB | **1,000MB** (F32) / **711MB** (Q8_0) |
| 로드 직후 상주 메모리 | (비전·오디오까지 F32로 상주) | 659MB (F32) / **300MB (Q8_0)** |
| 로드 시간 | 5.3초 | 2.0초 (F32) / 2.1초 (Q8_0) |
| 참조 벡터와의 코사인 (텍스트 6건) | 1.000000 | 1.000000 (F32) / 0.999906 (Q8_0) |
| 모델 받기 | 사람이 1.52GB를 받아 폴더 지정 | 앱에서 **텍스트 전용 574MB** 또는 전체 1.52GB |

### 16-2. 지시서 대조 (실제 모델 파일·실측으로 확인)

| 지시서 항목 | 결과 | 근거 |
|---|---|---|
| 24층, H=512, 전역 어텐션 5·11·17·23, 어휘 262,144, 투영 형상 | 맞음 | `config.json`, safetensors 헤더 |
| `embed_tokens` 536.8MB | F32로 펼친 크기다. 원본은 **BF16 268MB** | 헤더 (모든 텐서 BF16, 1,376개) |
| 임베딩 표 디스크 mmap 직결 + 입력 행만 읽기, 특수 토큰·역할 접두어 행 상주 | **완료** | `embeddinggemma_tokens.rs`. 예전 경로와 비트 단위로 같음 (BF16·F16·F32 시험) |
| 어휘를 자르지 않음 (UNK 0%) | 그대로 | 표 전체를 매핑에 둔다 |
| 모달리티 타워 지연 상주 (Lazy Residency) | **완료** | `residency.rs` (logis-center 방식, 16-3) |
| 무손실 원본에서 1회 직접 양자화, 재양자화 금지 | **지킴** | BF16 → F32(정확) → 한 번 양자화 |
| 관문 투영·정규화·layer_scalar는 F32 누산 | **지킴** | 출력 투영(512→768), PLE 전역 투영, 모든 RMSNorm, layer_scalar는 F32 |
| 4계층 피라미드 (Q8_0 / Q6_K·Q5_K / Q4_K) | **0.999 불변식을 못 지켜 채택하지 않음** | 지시서 배치의 최소 코사인 0.996975 (16-5) |
| 고정 할당이 아니라 실측 민감도로 배분 | **실측했다** → 결론은 전부 Q8_0 | 층별·묶음별 표 (16-5) |
| 전역 어텐션 4개 층(5·11·17·23)이 급소 | **층 23만 맞음** | Q4_K 단독: 층 23 6.05e-3, 층 5·11·17은 4.5~5.2e-4로 이웃 층과 비슷, 층 0·2가 더 민감 |
| PLE 블록 전 층 보호 | **실측으로는 가장 둔감한 묶음** | Q6_K 전 층: PLE 1.5e-4 < MLP 2.1e-4 < down 3.5e-4 < 어텐션 3.9e-4 |
| On-the-Fly(JIT) Fused 런타임 | **층 단위로 계산 직전에 F32로 풀어 F32 GEMM** | candle CPU에는 융합 커널이 없다. 그 대신 이 방식이 정확도·속도 모두 나았다 (16-5) |
| 바뀐 코드의 비전·오디오 수치 보호 | 지킴 | 비전 4건 0.999988 이상, 오디오 0.999999, 멀티모달 5건 0.999994 이상 (지연 로드 경로) |
| JIT 캐시 GGUF + `recipe_key` 지문 | **하지 않음** | Q8_0 양자화는 로드에 0.5초만 더한다. 캐시(138MB 파일과 무효화 규칙)를 둘 이유가 없었다 |
| Qwen3.5-0.8B `lm_head` 16,384 Trimming, `items` 스케줄러 | **하지 않음** | 이 저장소에 Qwen 실행 코드가 없다. logis-center의 `models/qwen3_5`를 옮기는 별도 작업이고, `items` 연동 계약도 아직 없다 (15-1) |
| laya-multilingual 양자화 면제 | 해당 없음 | 아직 붙이지 못했다 (15-4). 붙이면 F32로 둔다 |

### 16-3. 타워 지연 상주 (logis-center `model/residency.rs`를 CPU 메모리로)

logis-center는 생성 모델 ↔ 임베딩 모델을 VRAM에서 교차 상주시킨다 (슬롯, 실측 비용 장부, 예산 판정, 전환, 계산 중 언로드 방지). 같은 원칙으로 비전·오디오 타워를 다룬다.
- 상태는 슬롯에서만 유도한다: `disabled`(설정으로 끔) / `not_installed`(파일에 없음) / `standby` / `resident`.
- 타워는 그 입력(이미지·비디오·오디오)이 처음 올 때 올린다. 비용은 로드 전후 `VmRSS` 차이로 실측한다 (실측 비전 665MB). 실측 전에는 디스크 크기 x 2(BF16→F32)로 과대 추정한다.
- 메모리(`MemAvailable`)가 모자라면 다른 타워를 내리고(전환), 그래도 모자라면 거절한다 ("메모리가 부족해…"). 앱이 죽는 것보다 낫다.
- 계산 중인 타워는 `Arc`로 쥐므로 내려도 계산이 끝날 때 메모리가 풀린다 (빈 슬롯을 보고 틀린 값을 내지 않는다).
- `/proc`가 없는 운영체제(Windows·macOS)에서는 예산 판정과 실측을 건너뛴다. 지연 로드는 그대로다.
- 상태는 `cmd_app_status.model`, `cmd_model_status.residency`로 본다.

### 16-4. 모델 내려받기와 텍스트 전용

- **텍스트 전용(574.3MB)**: 원본 `model.safetensors`의 헤더(171KB)와 `language_model.*` 413개 텐서 구간만 HTTP Range로 받아 `model-text.safetensors`로 조립한다. 원본 안에서 이 구간이 **한 덩어리(542.0MB)**라 요청 하나로 된다. 이미지·오디오 타워 945MB를 받지 않는다. 토크나이저 32MB 포함.
- **전체(1.52GB)**: 원본 그대로. 받으면 텍스트 전용 파일은 지운다 (전체가 포함하므로).
- **고정과 검증**: 리비전 `f9972ac`, 파일별 SHA-256 (HF API의 LFS 해시와 내려받은 파일의 해시가 같음을 확인), 텍스트 구간은 헤더 해시와 구간 해시로 확인한다. 받는 중에는 `.part`에 쓰고 해시가 맞을 때만 이름을 바꾼다. 끊기면 이어받고(재시도 4회), 멈추면 `.part`를 남겨 다음에 이어받는다. 저장 공간이 모자라면 그렇게 안내한다.
- **화면**: 모델이 없으면 "모델이 필요합니다"와 두 선택지(텍스트 전용 권장), 진행률, 멈추기·이어받기를 보여 주고, 끝나면 앱이 바로 시작한다 (셸이 설치 세대를 보고 실패했던 코어를 다시 만든다).
- **설정**: `modalities = auto | text` (`text`면 전체 파일이 있어도 타워를 올리지 않는다), `model_base_url` (기본 Hugging Face. 해시가 고정이라 다른 내용은 설치되지 않는다). 환경 변수 `PD_MODALITIES`, `PD_MODEL_BASE_URL`.
- **실제 Hugging Face**에서 텍스트 전용을 받아(20초, 574.3MB) 그 파일로 텍스트 참조 테스트를 통과했다 (코사인 1.000000, 상주 656MB). 이미지 입력은 "전체 모델을 내려받으면 쓸 수 있습니다"로 거절된다.

### 16-5. 양자화 실측 (텍스트 참조 6건의 최소 코사인, 층 투영 9종 x 24층)

계산 방법 두 가지를 비교했다. **Dequant**(기본): 양자화해 보관하고 계산 직전에 층 하나씩 F32로 풀어 F32 GEMM. **Native**: candle 양자화 행렬곱(활성값도 8비트로 바꿔 내적).

| 레시피 | Dequant 최소 코사인 | Native 최소 코사인 | 층 가중치 | 6건 시간 (Dequant / Native) |
|---|---|---|---|---|
| F32 | 1.000000 | - | 520.1MB | 3.5초 |
| 전부 Q8_0 | **0.999906** | 0.999756 | 138.1MB | 4.1초 / 31.4초 |
| 전부 Q6_K | 0.998875 | 0.997880 | 106.7MB | 4.2초 / 20.6초 |
| 전부 Q5_K | 0.995946 | - | 89.4MB | 4.8초 |
| 전부 Q4_K | 0.986217 | 0.985473 | 73.1MB | 4.1초 / 18.7초 |
| 지시서 4계층 초안 | 0.996975 | 0.996966 | 106.2MB | 4.2초 / 20.6초 |
| 실측 혼합안 (gate·up 층 3~21만 Q6_K) | 0.999809 | - | 128.5MB | 4.5초 |

- 층별 민감도(그 층만 Q4_K)와 묶음별 민감도(전 층의 그 묶음만 각 비트)는 `app/tests/fixtures/quant_sensitivity.jsonl`. 묶음별 오차(1-cos)는 거의 더해진다 (Q6_K 묶음 넷의 합 1.10e-3 ≈ 전부 Q6_K 1.13e-3).
- **결정**: 0.999 불변식 아래에서 Q4_K·Q5_K를 쓸 자리가 사실상 없고, Q6_K를 섞어도 Q8_0보다 7%만 줄어든다. 그래서 **Q8_0 하나**로 했다. 설정 `quant = auto(기본: 모바일 Q8_0, 데스크톱 F32) | off | q8_0`. 4·5·6비트는 설정으로 고를 수 없다 (실측 도구로만).
- **Q8_0 검증**: 텍스트 참조 6건 최소 0.999906, 멀티모달 참조 5건 최소 0.999892(오디오) — 같은 테스트를 `PD_QUANT=q8_0`으로 돌렸다. 상주 메모리(로드 직후) 659MB → 300MB. 짧은 글 한 건 약 0.15초 → 0.28초 (층을 풀어 쓰는 비용).
- **norm**: 출력은 L2 정규화 뒤라 |norm-1| ≤ 4.2e-7.

### 16-6. 하지 않은 것과 한계

- 비전·오디오 타워 자체의 양자화는 하지 않았다. 지연 상주로 텍스트만 쓸 때는 메모리에 없고, 이 앱의 화면은 텍스트만 쓴다.
- 서버(Vectorize)와 다른 기기의 벡터는 F32로 만든 것이고 모바일 기본(Q8_0)은 코사인 0.9999 수준으로 다르다. 검색 순위에 영향이 없는 차이로 봤지만, 어떤 설정으로 만든 벡터인지 DB에 남기지는 않는다 (스키마 무수정 원칙).
- 지연 로드한 비전 타워의 이미지 한 장 인코딩은 이 PC에서 약 15초다 (전과 같다).
- Android 실기기에서 내려받기·양자화를 돌려 보지는 못했다 (15-6). 최대 상주 메모리 1,000MB(F32)·711MB(Q8_0)는 이 PC(Linux x86_64)의 측정이다. Q8_0의 최대치가 정상 상태(300MB)보다 높은 것은 로드 중에 텐서를 하나씩 F32로 펼쳤다가 양자화하는 순간과 파일 매핑 페이지 때문이다.

### 16-7. 검증

- 단위: `cargo test --lib -j 1` **233개** (임베딩 표 비트 일치, 타워 상주 8개, 내려받기 6개: 텍스트 구간만 받기, 끊김 이어받기, 해시 불일치와 Range 미지원 서버, 취소 후 재개, 전체 설치, 헤더 구간 검사, 양자화 4개), Tauri 설정 5개, 화면 48개.
- 실모델: 텍스트·비전·오디오·멀티모달 참조 테스트 (F32와 Q8_0), 고정 매니페스트 = 실제 파일 (`pinned_manifest_matches_the_real_model_file`).
- E2E: `ui_model_e2e.mjs` 7단계 (가짜 HF가 실제 파일을 Range로 답함: 진행률, 멈추기, 멈춘 곳부터 이어받기, 원본 1,489MB 중 텍스트 구간만 받음, 받은 모델로 등록), `tauri_e2e.mjs` 11단계 (진짜 창 안에서 내려받기·설치·시작 포함), 기존 `ui_e2e.mjs` 12단계·`ui_login_e2e.mjs` 15단계 그대로 통과.

## 17. 남은 항목 마무리: 양자화 캐시, 타워 양자화, laya 필터, Qwen3.5 스케줄러, Android (2026-11-14)

16절에서 "하지 않은 것"으로 남긴 다섯 가지를 진행했다. Python 참조는 이 환경에 CPU 전용 torch 2.15 + transformers 5.23을 설치해 만들었다
(`download.pytorch.org`를 허용 목록에 추가). 모든 cargo 실행은 `-j 1`.

### 17-1. 양자화 결과 캐시와 recipe_key (`app/src/qcache.rs`)

- 원본 BF16에서 한 번 양자화한 결과를 `<모델 폴더>/.pd-cache/<component>-<recipe_key>.gguf`에 쓰고, 지문이 같을 때만 그대로 읽는다.
  지문 = 원본 길이 + **헤더와 데이터 표본 64블록**의 SHA-256 + recipe_key(예: `eg2_text_q8_0_v1`) + 형식 판.
  헤더만으로는 구조가 같은 다른 체크포인트를 구별하지 못한다 (시험으로 확인해 표본을 더했다). 표본 사이 몇 바이트만 바뀐 파일은 알아채지 못한다.
- 실측 (Q8_0): 캐시 없음 로드 2.66초·최대 558MB → **캐시 사용 1.31초·최대 395MB** (F32는 1.61초·918MB). 캐시를 만든 실행과 읽은 실행의 벡터가 비트 단위로 같다.

### 17-2. 비전·오디오 타워 양자화

- 비전 블록과 오디오 ClipLinear 가중치를 Q8_0으로 (관문 투영·패치·위치·합성곱·정규화·클립 경계는 F32). 타워도 같은 캐시를 쓴다.
- 층별 민감도(그 층만 Q8_0, 토큰 최소 코사인, `app/tests/fixtures/tower_sensitivity.txt`): 비전 층 0이 0.999584로 가장 민감하고 14·15층이 0.99995로 가장 둔감
  (지시서의 {0, 1, 14, 15} 보호 중 0·1만 맞았다). 오디오는 **층 8이 0.998776**으로 다른 층(0.99982 이상)보다 한 자릿수 크다. 이 둘만 F32로 둔다.
- 결과: 비전 선형층 604MB → 188MB, 오디오 1,158MB → 378MB. **최종 768차원 벡터(멀티모달 참조 5건) 최소 0.999863**.
  토큰 단위는 오디오 0.999381, 비전 0.997774라 양자화 시 토큰 기준을 0.995로 명시했다 (F32 기준 0.999는 그대로).

### 17-3. laya-multilingual 로컬 욕설 필터 (`app/src/laya.rs`)

- laya 0.4.31 패키지 코드대로 이식: mmBERT-base(ModernBERT 22층) + 질문 유형 임베딩 + 결정 헤드 2층 + [MASK] 점수기. 입력 조립도 한 줄씩 맞췄다.
- **참조와 일치**: 진짜 laya + torch로 만든 22건(909토큰 긴 입력 포함)과 입력 토큰 완전 일치, 로짓 차이 최대 1.07e-5, 답 22/22.
  Q8_0이면 로짓이 최대 0.19 움직여(답은 같음) 데스크톱은 F32(지시서의 양자화 면제), 모바일만 Q8_0.
- 평가(직접 라벨 붙인 28문장, `app/examples/filter_eval.rs`): 사전 24/28, laya 25/28, **사전 + laya 28/28 (욕설 14/14, 오탐 0/14)**.
  사전은 욕 단어 없는 모욕("Go to hell, you idiot seller.")을, laya는 축약·변형(ㅅㅂ, 씨1발)을 놓쳐 둘을 함께 쓴다. 소규모 세트라 일반화된 정확도가 아니다.
- 앱: 작성 칸 아래 "욕설 필터: 기본 사전 · 더 정확한 모델 받기 (678.2MB)" → 받으면 앱을 다시 켜지 않고 laya로 바뀐다 (`SwitchFilter`).
  설정 `filter = auto | dictionary | laya`, `filter_model_dir`.

### 17-4. Qwen3.5-0.8B 스케줄러와 출력 헤드 Trimming (`app/src/qwen35.rs`, `app/src/items_draft.rs`)

- transformers의 `modeling_qwen3_5.py`대로 이식: Gated DeltaNet 18층 + 전역 어텐션 6층, (1+w) RMSNorm, 출력 게이트, 부분 RoPE.
  **참조와 일치**: 채팅 템플릿·프롬프트 토큰 완전 일치, 그리디 20/20 토큰(두 프롬프트), 로짓 차이 최대 6.77e-5.
- **비대칭 Trimming**: 입력 임베딩(248,320 x 1024)은 파일 매핑에 그대로 두고(어휘 전체), 출력 헤드만 16,384개로.
  반드시 넣는 것: 바이트 256(어떤 UTF-8도 바이트로 낼 수 있는 폴백) + 특수 토큰 + 한글 완성형 6,793 + 다바이트 문자 조각 944 + JSON·도메인 토큰,
  그리고 **요청마다 그 페이지의 토큰**(모델이 옮겨 쓸 단어). 나머지는 병합 순위로 채운다.

| 실측 (8페이지, F32) | 전체 헤드 248,320 | 잘라 낸 헤드 16,384 |
|---|---|---|
| 헤드 메모리 | 1,017MB | 67MB |
| 토큰당 시간 | 343ms | 253ms (-26%) |
| 한국어 4건 | 기준 | 토큰·초안 완전 일치 |
| 영어 4건 | 기준 | 15~17토큰부터 표현이 달라짐 (내용은 유지) |
| type(1 premium / 2 discount) | 기준 | 8/8 같음 |

  페이지 토큰을 더하기 전에는 영어 초안 하나가 "sintered sintered…"로 망가졌다. 영어 어휘(12만 개 이상)를 16,384개에 다 담을 수 없어 영어는 완전히 같지 않다.
- **양자화(ΔNLL)**: Q8_0이면 평균 NLL 2.84420 → 2.84940 (+0.0052). 층 가중치 약 2GB → 약 0.53GB. 다른 모델과 같이 `quant=auto`면 모바일만 Q8_0.
- **초안 생성**: `{type, head, body, link, digest}`를 필드 단위로 강제한다 (type은 "1"/"2" 토큰 중에서만, 문자열은 닫는 따옴표까지) → 언제나 올바른 JSON.
  0.8B 모델이라 분류가 틀릴 수 있다 (F32 실측 8건 중 "스마트폰 케이스 1+1 특가"를 premium으로 봤다). **digest는 제목의 SHA-256으로 정했다**: www의 `items` 주석은 "Digest(title)"뿐이고 계산 규칙이 저장소에 없다. 서버와 맞춰야 한다.
- 앱: 오른쪽 "페이지 초안 (로컬 AI · 실험)" 카드. 모델이 없으면 "초안 모델 받기 (1.52GB)": 원본 1.75GB 중 **텍스트 가중치 구간(1,504.8MB, 한 덩어리)만** Range로 받는다.
  명령 `cmd_item_draft {text, link}`, 설정 `draft_model_dir`. items 테이블 스키마는 바꾸지 않았고, 초안을 서버에 저장하는 경로는 만들지 않았다 (계약이 없다).

### 17-5. Android

- 이 환경에는 KVM이 없어 에뮬레이터·실기기 실행은 여전히 못 한다. 대신 위의 모든 코드로 arm64 디버그 APK를 다시 빌드했다:
  **480MB, 직렬 빌드(`CARGO_BUILD_JOBS=1`) 14분 47초**, 패키지 `discount.premium.app`, minSdk 24, arm64-v8a, 권한 INTERNET, JNI 진입점 24개.
  `.so`에 새 모듈이 들어 있다 (심볼 수: laya 255, qwen35 281, qcache 89, items_draft 508, model_fetch 643, residency 80). 추가 우회는 필요 없었다 (15-6의 gemm-f16 설정 그대로).
- 휴대폰에서의 메모리·속도(laya, Qwen 초안, 타워 양자화)는 확인하지 못했다. `quant=auto`가 모바일에서 Q8_0을 고르는 것까지가 코드로 정한 부분이다.

### 17-6. 검증

- 단위: 코어 `cargo test --lib -j 1` **238개**, Tauri 설정 5개, 화면 **50개**. 명령 계약 15개 (모델·필터·초안 명령 포함).
- 실모델 참조: embeddinggemma 텍스트·비전·오디오·멀티모달(F32와 Q8_0), 캐시 비트 일치, laya 22건, Qwen3.5 두 프롬프트, 고정 매니페스트(laya·Qwen) = 실제 파일.
- E2E: `ui_model_e2e.mjs` **12단계** (텍스트 전용 받기·멈추기·이어받기 → 등록 → 필터 모델 받기 → 사전이 통과시킨 모욕을 laya가 막음 → 초안 모델 텍스트 부분만 받기 → 초안 생성),
  `tauri_e2e.mjs` **11단계** (진짜 창, 모든 변경 뒤). `ui_e2e.mjs`(12)·`ui_login_e2e.mjs`(15)는 laya·Qwen 셸 변경 **전**에 마지막으로 돌렸다 (같은 흐름을 tauri_e2e가 진짜 창에서 다시 확인했다).
- 시험 격리 문제 2건을 고쳤다 (앱 문제 아님): 이전 실행의 WebDriver가 포트를 쥐고 남아 새 실행이 옛 앱과 대화한 것, 문서 포털 FUSE 마운트가 XDG 폴더를 잡은 것.

## 18. 추천 / 비추천: 투표와 판정 (2026-11-16)

premium-discount 는 동기화와 전역 추천/비추천 판정을 맡습니다. 판정 규칙의 기준도 이 저장소에 있습니다. raon-zena(캐릭터 챗)는 같은 규칙을 기기 안에서 돌려 로컬 판정을 냅니다. 로컬 판정은 서버로 올리지 않습니다. 역할 분담과 스키마 전체는 정의서 "Raon Zena × premium-discount — 추천/비추천 역할 분담 · 스키마 정의서"에 있습니다.

### 18-1. 판정 규칙 (PD-Verdict v1, `contracts/`)

- `contracts/gen_verdict_v1.py` 가 참조 구현입니다. 이것이 `contracts/verdict_v1.json`(매개변수 + 사례 19개)을 만듭니다. 규칙의 문장 설명은 `contracts/README.md` 에 있습니다.
- 계산 순서는 다음과 같습니다. 축소 평균 값 → secondary(`cc`+`to`) · primary(`cc`) · global 기준선(표본 하한 백오프) → `tanh` 상대 순위 → 등급(히스테리시스, 절대 하한) → grade(−2 ~ +2) · 이유 코드.
- 구현 세 개가 같은 사례를 통과합니다.
    - cron 의 TypeScript(`cron/src/verdict.ts`, 시험 `cron/test/verdict.test.ts`)
    - raon-zena 의 Rust(`crates/zena-verdict`)
    - 앱 E2E 가짜 서버(참조 구현을 import)

### 18-2. 서버 (`www/migrations/0006_votes_verdicts.sql`)

| 표 | 내용 |
| --- | --- |
| `eval_votes` | 투표 기록. 덮어쓰지 않고 쌓으며, (평가, 투표자, 종류)마다 가장 최근 행이 유효한 표입니다. 늦게 온 옛 `request_id` 재시도가 새 표를 되돌리지 않게 하기 위해서입니다. 값 0 은 철회입니다. |
| `eval_verdicts` | 평가별 판정: E, v, up, down, level, rank, tier, grade, tier_since, reasons, recipe |
| `verdict_scopes` | 범위별 기준선 스냅샷 (`n`, 평균, 모집단 표준편차) |
| `verdict_dirty` | 다시 계산할 평가 (투표 API 가 표시하고 cron 이 지움) |

- **`POST /evaluations/votes`** `{request_id, id, value: 1|-1|0, kind?: vote|vote_outcome}` 입니다. 쓰기 제한(평가 저장과 같은 20/분)을 받습니다.
    - 처음 받으면 202 `{id, value, kind, counted, status}` 를 돌려줍니다.
    - 같은 `request_id` 를 다시 받으면 200 으로 처음 본문을 돌려줍니다. 내용(평가 · 종류 · 값)이 다르면 409 입니다.
    - 저장되지 않은 평가면 404 `{error: "evaluation not found"}` 입니다.
    - 투표자는 서버가 정합니다(확인된 세션, 없으면 게스트). 클라이언트가 보낸 `from` 은 쓰지 않습니다.
    - 재시도에서는 투표자를 비교하지 않습니다. 사이에 게스트 IP 가 바뀌거나 세션 확인이 잠깐 실패해도 같은 요청으로 답합니다.
    - **세는 표**는 확인된 세션의 남의 글 표뿐입니다(`counted`). 게스트 · 자기 글 표는 저장되지만 가중치가 0 입니다. 따라서 `SESSION_S3_*` 가 설정되지 않은 서버에서는 어떤 표도 판정에 들어가지 않습니다.
- **`GET /evaluations/verdicts?ids=`**(1 ~ 50개, 읽기 제한)는 요청 순서대로 판정을 돌려줍니다. 판정 행이 없는 id 는 `unrated` 입니다. 투표자는 어떤 응답에도 나가지 않습니다.
- **검색**(앱 · 공개)에는 선택 인자 `tier`(`premium` | `discount` | `rated`)가 있습니다. 결과마다 `verdict: {tier, grade, rank, evidence} | null` 이 붙습니다. 등급 조건이 있으면 Vectorize 에서 topK 의 세 배까지 받아 거른 뒤 자릅니다.
- **그래프**: 평가 노드에 `tier` · `grade` 가 붙고, 응답에 등급별 개수 `summary` 가 붙습니다.
- **cron**(1분, 대기열 다음)은 아래 순서로 판정을 처리합니다.
    1. 표시된 평가 최대 50개와, 가장 오래전에 계산한 판정 25개(감쇠 반영)의 E · v 를 다시 계산합니다.
    2. 건드린 범위의 기준선을 다시 계산합니다.
    3. 그 평가들과, 건드린 secondary · primary 범위의 피어(범위마다 500개까지, 오래 판정한 순)를 다시 판정합니다.
    4. 판정 결과는 200행씩 한 문장으로 씁니다.

### 18-3. 앱 (`app/src/votes.rs`, 명령 16개)

- LanceDB 표가 둘 늘었습니다(같은 폴더). 평가 보관함(`Outbox`)이 함께 엽니다.
    - `votes_local`: 투표 보관함. (평가, 종류)마다 못 보낸 표는 하나만 두고, 바꾸면 새 `request_id` 로 다시 만듭니다.
    - `verdicts_cache`: 판정 캐시.
- 새 명령은 두 개입니다.
    - `cmd_vote {id, value, kind?}` → `{status: sent, counted}` | `{status: queued}`
    - `cmd_verdicts {ids, refresh?}` → `{verdicts, missing, stale, remote_error}`. 캐시가 10분보다 오래됐거나 `refresh` 면 서버에 묻습니다.
- 넓힌 명령은 두 가지입니다.
    - `cmd_flush_outbox`: 투표도 보내고 `votes_sent` · `votes_remaining` · `votes_rejected` · `votes_stopped` 를 돌려줍니다.
    - `cmd_search_remote` · `cmd_search_merged`: `tier` 필터를 받고, 결과마다 `verdict` 와 `my_vote` 를 붙입니다. 등급 필터가 있으면 로컬 결과는 섞지 않습니다.
- 투표 재시도 규칙은 보관함과 같습니다. 다른 점은 두 가지입니다. 투표끼리는 순서가 없어 일시 오류는 그 표만 미루고, "evaluation not found" 404 는 곧바로 거부합니다.
- 화면: 검색 결과에 등급 배지(강력 추천 · 추천 · 보통 · 비추천 · 강력 비추천 · 판정 전)가 붙고, 남의 글에 ▲ 추천 / ▼ 비추천 버튼이 붙습니다. 같은 버튼을 다시 누르면 철회합니다.
- 화면 캐시 행(`cmd_sync_cache`, `fixtures/cache_rows.json`)은 바꾸지 않았습니다. 판정은 화면이 `cmd_verdicts` 로 묻습니다.

### 18-4. 검증

| 묶음 | 실행 | 결과 |
| --- | --- | --- |
| cron | `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test cron/test/*.test.ts` | 39개 통과 (계약 3 · 판정 집계 10 포함) |
| www | `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts` | 58개 통과 (투표 · 판정 · 검색 18, 그래프 +3) |
| 앱 단위 | `cargo test --release --lib` (릴리스 의존성 재사용) | 240개 통과 |
| 화면 | `cd app/ui && npm install && npm test` | 50개 통과 |

- 실제 앱 왕복 시험은 raon-zena `premium-discount-mcp` 의 E2E 로 했습니다(가짜 서버 + 실제 모델). 순서는 다음과 같습니다.
    1. 오프라인 보관에서 복구 전송까지 봅니다.
    2. 게스트 표는 `counted=false` 이고, 판정은 근거 0 · `unrated` 입니다.
    3. QR 로그인 뒤 표는 `counted=true` 이고, 판정은 근거 1.0 · 비추천 1 입니다.
    4. 판정 캐시와 검색 결과의 `verdict` 를 확인합니다.
- **Node 22.13 의 `node:sqlite` 는 `?1` 같은 번호 매개변수를 위치 인자로 묶지 않습니다.** 그래서 기존 cron · www 테스트도 이 PC 에서는 21개가 실패하고 있었습니다. 가짜 D1(`test-support/fakes.ts`)이 번호 매개변수를 이름으로 넘기도록 고쳤습니다.
- `wrangler deploy --dry-run` 으로 두 워커가 묶이는지 확인했습니다. www 는 `RECIPE` 만 `cron/src/verdict.ts` 에서 가져옵니다.

### 18-5. 알아 둘 것

- **cron 비용**: 실행마다 global 기준선이 판정 행 전체를 읽고, 건드린 범위의 피어를 다시 씁니다. 데이터가 늘면 D1 의 실행당 쿼리 수 · 쓴 행 수를 지켜봐야 합니다.
- **쓰기 제한 공유**: 투표는 평가 저장과 같은 쓰기 제한(20/분)을 씁니다.
- **판정 경계**: 표가 딱 두 개인 평가(E = 2.0)는 감쇠가 조금만 진행돼도 `min_evidence` 아래로 내려가 기준선에서 빠집니다.
- **레거시**: `items.type`(0 draft · 1 premium · 2 discount) 경로는 그대로 두었습니다. 정리할지는 정의서 10절의 결정 항목입니다.

## 19. 접근성 내비게이션 준비: candle 포크와 라이브러리 crate (2026-11-16)

접근성 내비게이션 기획안(Claude Doc "접근성 내비게이션 MCP 기획안")의 M0 와 M1 첫 단계입니다. 기획안에서 premium-discount 는 캐릭터 · 대화가 없는 접근성 내비게이션 UI 앱이 됩니다. 접근성 로직은 이 저장소의 crate 라이브러리로 만들고, raon-zena 가 가져다 씁니다. 두 앱이 함께 켜져 있으면 premium-discount 가 감지하고 결과를 MCP 로 raon-zena 에 넘깁니다.

### 19-1. candle → candle-rocm-vulkan 포크 (M0)

- `candle-core` · `candle-nn` 을 0.9.2 에서 0.11.0 으로 올리고, `[patch.crates-io]` 로 Qntology/candle-rocm-vulkan 포크(rev `ebaf93d`)로 바꿨습니다. raon-zena 가 `Cargo.lock` 으로 고정한 커밋과 같습니다. `[patch]` 는 빌드하는 쪽 루트 매니페스트의 것만 적용되므로 `app/Cargo.toml` 과 `app/src-tauri/Cargo.toml` 에 같은 줄을 둡니다.
- `safetensors` 를 0.7 에서 0.8 로 올렸습니다. 포크 candle 이 돌려주는 `Dtype` 과 같은 판이어야 `embeddinggemma_tokens.rs` 가 컴파일됩니다.
- 기능 `cuda` · `rocm` · `vulkan` · `metal` 을 더했습니다(raon-zena · logis-center 와 같은 이름). `src-tauri` 는 같은 이름으로 앱 코어에 넘깁니다.
- `app/src/utils.rs`
    - `select_device` · `device_from_env`: `PD_DEVICE`(auto · cpu · cuda · rocm · vulkan · metal)로 장치를 고릅니다. 값이 없으면 CPU 입니다. auto 는 CUDA → ROCm → Vulkan → Metal 순서로 처음 열리는 장치를 씁니다.
    - `GpuDeviceExt::is_cuda_or_rocm` 이 ROCm 도 봅니다.
    - `release_gpu`: GPU 를 다 쓰고 프로세스를 끝내기 전에 메모리 풀을 비웁니다. Windows HIP 는 포크의 비동기 메모리 풀이 남은 채 끝나면 종료 단계에서 멈추고, 그 프로세스가 재부팅 전까지 코어 하나와 VRAM 을 쥡니다(gravity-ocr · noto-sandbox 에서 확인).
- 기존 임베딩(embeddinggemma)은 CPU 그대로입니다. 오디오 타워가 입력을 CPU 에 고정해 두어서, GPU 로 옮기려면 따로 대조가 필요합니다. GPU 는 접근성 비전(SigLIP2)이 처음 씁니다.
- 잠금 파일 두 개에 포크의 플랫폼별 의존성(hip-sys, ash, metal, gemm 0.18 …)이 더해졌습니다. tauri · wry 판은 그대로입니다.
- ROCm 빌드(Windows): HIP SDK 7.2 의 clang 21 은 MSVC 14.51 STL 과 충돌합니다. 그래서 `VCToolsInstallDir` 를 14.44 툴셋으로 지정합니다.

```bash
cd app
VCToolsInstallDir='C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\' \
  CARGO_BUILD_JOBS=2 PROTOC='C:\protoc\bin\protoc.exe' cargo build --release --features rocm
```

### 19-2. 라이브러리 crate (`crates/`, M1)

`crates/` 는 앱과 따로 빌드하는 작업 공간입니다(`cd crates && cargo test`). 라이브러리는 LanceDB · HTTP · Tauri 에 의존하지 않습니다. 그래야 LanceDB 판이 다른 raon-zena(0.41)와 이 앱(0.31)이 같은 라이브러리를 링크할 수 있습니다.

| crate | 내용 |
| --- | --- |
| `premium-discount-verdict` | PD-Verdict 의 Rust 구현. raon-zena `crates/zena-verdict` 의 판정 함수를 옮겨 왔습니다(캐릭터 말투 · 대화 피드백 모듈은 raon-zena 에 남습니다). `Params::v1()` · `Params::v2()` · `Params::for_recipe()` |
| `premium-discount-nav` | 접근성 내비게이션 코어. 후보 영역 합치기(`merge`: IoU 0.4, 트리 > 비전 > 이벤트, 이벤트는 바뀜 표시), 번호 매기기(`number`: premium 이 앞 · discount 가 뒤, 종류, 읽는 순서, 1 ~ 9), 이름 · 위치 단서의 개인정보 거르기(`scrub_name` · `scrub_anchor`), 영역 지도(`NavMap`, `navmap/1`), 행동 기록 → SDS 결정 → 결정 기록(아래) |
| `premium-discount-identity` | 인증의 지갑 갈래(익명 계정): 시드 구문 만들기 · 되살리기(BIP-39 영어 12단어, 경로 m/44'/60'/0'/0/0 — ethers 와 같음), `hashId`, EIP-191 서명 · 주소 복원, 요청 서명(`Wallet::sign_request` → 헤더 `x-pd-address` · `x-pd-time` · `x-pd-signature`). BIP-39 · BIP-32 는 직접 구현했고(`bip32` crate 는 24단어만 받음), 시드 구문 · 개인 키는 지울 때 0 으로 덮습니다 |

**인증은 두 갈래입니다.** 클라우드 세션(익명 아님)은 지금의 QR 메일 로그인(OAuth3 hash · token)입니다. 지갑(익명)은 사용자가 시드 구문을 보관하는 ethers 지갑이고, 요청마다 EIP-191 서명을 붙입니다. 서버는 `www/src/auth.ts` 의 `verifyWalletRequest` 로 확인합니다(시각 5분 안, 서명에서 되살린 주소가 헤더의 주소와 같을 것). 서명 글의 형식과 기준값은 `contracts/gen_wallet_v1.mjs` → `wallet_v1.json` 이고(ethers 6.17 로 뽑음), Rust 와 서버가 같은 계약을 시험합니다. 타임트라이얼 경로(19-3)가 두 갈래를 모두 받습니다.

**행동 기록 → SDS 결정 → 결정 기록**: 결정 기록에는 해시 · 벡터 · 종류(premium/discount)와 날짜만 들어갑니다. 기록은 로그인 없이(게스트도) 기기에 먼저 쌓이고, 클라우드 동기화는 사용자가 원할 때 인증(클라우드 세션 또는 지갑)한 뒤에만 올라갑니다. 서버로는 이 기록이 아니라 타임트라이얼의 런을 올립니다(19-3). 개인정보 화면(비밀번호 · 결제 칸, `sensitive_screen`)에서는 트리로만 번호를 매기고 기록 · 지도를 만들지 않습니다(`merge` 는 트리 후보만 남기고, `NavMap::from_regions` 는 None).

1. `split_episodes`: 사건(번호 선택 · 입력 시작 · 제출 · 재시도 · rage click · 찾는 시간 · 체류 · 되돌아가기 · 포기)을 에피소드로 자릅니다(logis-center U-1: 이웃 사건의 의미 거리 × 시간 간격 z 가 평균 + 표준편차를 넘는 곳, 5분 넘는 간격). 사건에는 입력값 · 화면 글자가 없습니다.
2. `FrictionBaselines::decide`: 에피소드의 마찰 점수(초깃값 가중치)를 피어와 견줍니다. 화면은 같은 앱 · 사이트의 다른 화면 → 전역, 영역은 같은 화면의 다른 영역 → 앱 → 전역입니다. 판정하는 항목 자신의 에피소드는 기준선에서 뺍니다(`Welford::without`). 성공했고 z ≤ −1 이면 premium, z ≥ 1 이면 discount, 그 사이와 기준선이 모자란 경우는 결정하지 않습니다.
3. `NavMark`(`navmark/1`): 결정이 난 화면 · 영역마다 `hash`(서버와 같은 `hashId`, 화면 키 틀: 웹은 `url_template`, 앱은 `structure_template`), `vectors`(공간 이름을 붙인 벡터들, L2 정규화: 라벨 글 `region_label` · `screen_label` 의 granite 벡터 `LABEL_SPACE` 는 두 앱 공용, 영역 조각의 비전 벡터는 앱마다 공간이 다름), `type`, `day`(UTC 날짜), `recipe`. 계정 · 기기 식별자 · IP · 시각 · 원시 이벤트 · 화면 글자는 없습니다. 서버는 `to_obs` 로 같은 해시의 기록을 `implicit` 관측으로 바꿔 판정합니다(하루 하나씩 열흘이면 근거 2.37 로 판정).

`contracts/gen_verdict_v2.py` → `verdict_v2.json` 은 v1 규칙에 관측 종류 `implicit`(가중치 0.3)과 트랙 `nav` 를 더한 v2 계약입니다(사례 25개). 자세한 내용은 `contracts/README.md` 에 있습니다. 서버는 아직 v1 로 판정합니다.

### 19-3. 타임트라이얼: `/nav/trials` (2026-11-17)

사용자의 플로우를 레이싱 게임의 타임트라이얼처럼 기록합니다. 기기 쪽은 `crates/premium-discount-nav` 의 `trial` 모듈이고, 서버는 `www/src/navtrials.ts`, 보관 규칙(`TRIAL_POLICY`)은 `cron/src/navtrials.ts`, 표는 `www/migrations/0007_nav_trials.sql` 입니다.

| 레이싱 | 접근성 내비게이션 |
| --- | --- |
| 코스 | 한 앱 · 웹 안에서 시작부터 결승까지 지난 체크포인트의 순서. id 는 웹의 `hashId(cc + pathname)` 처럼 앞의 id 에 다음 조각을 이어 붙여 해싱합니다(구분자 없음). 체크포인트 id = `hashId(종류 + 해시 + 제스처)`(`checkpoint_id`). 코스 id 는 체인입니다(`course_chain`): 첫 체크포인트에서 `hashId(앱 id + 체크포인트 id)`, 다음부터 `hashId(이전 코스 id + 체크포인트 id)`, 결승의 값이 그 코스의 id 입니다(`course_hash`). 같은 길은 누가 달려도 같은 id 라 두 번 등록되지 않고, 앞부분이 같은 코스는 그 지점까지 같은 id 를 갖다가 다른 체크포인트부터 갈라져 새 코스가 됩니다. 저장하는 체크포인트마다 id 와 그 지점까지의 코스 id(`course`)가 붙습니다 |
| 런(한 바퀴) | 성공한 에피소드 하나: 체크포인트마다 시작부터의 구간 시간(100 ms 단위)과 그 에피소드의 SDS 결정(premium · discount · 없음) |
| 기록실(콜렉션) | 앱 · 웹마다 코스 10개. 같은 코스를 다시 달리면 횟수(중복 카운트) · 결정 수 · 최고 · 마지막 기록이 바뀝니다 |
| 고스트 | 내 최고 기록, 다른 사용자의 최고 기록 · 평균. 진행 중인 체크포인트가 코스 앞부분과 같으면 다음 체크포인트와 앞섬 · 뒤짐(ms)을 알려 줍니다(`Collection::guide`) |

| 규칙 | 값 |
| --- | --- |
| 체크포인트 종류 | 앱은 `screen`(UIA 구조 지문) · `region`(영역), 웹은 `page`(링크값: URL 틀, `#/cart` 같은 해시 라우팅 포함, `page_hash`) · `element`(DOM 셀렉터 틀, `selector_template` → `element_hash`). 셀렉터는 확장이 보낸 조상 노드의 태그 · 안정된 id · class · 허용 속성(`data-testid` · `role` · `name` · `type`)만으로 만들고, 글자 · 입력값 · aria-label · href 는 받지 않습니다. 빌드마다 바뀌는 class(`css-1x2y3z`)와 상태 class(`active`)는 버립니다 |
| 제스처 | 클릭 · 터치(탭)는 누름, 터치 · 펜으로 빠르게 50 px 이상 밀면 스와이프(상하좌우), 300 ms 안에 이어진 스크롤이 100 px 이상이면 스크롤(상하좌우)입니다(`classify_stroke` · `scroll_bursts`, 초깃값). 스와이프 · 스크롤로 지난 체크포인트에는 제스처가 붙어 `element:0x…@swipe-left` 처럼 다른 체크포인트가 되고, 누름은 붙지 않습니다(마우스 · 손가락으로 누른 사람이 같은 코스). 좌표 · 시각은 기기를 떠나지 않습니다. 마찰로도 씁니다: 700 ms 안에 같은 곳을 세 번 누르면 rage, 스크롤 방향을 뒤집을 때마다 `Rescrolled`(0.5) |
| 플로우 길이 | 체크포인트 10개까지. 기기가 더 긴 플로우의 오래된 체크포인트를 버리고 끝 쪽 10개로 코스를 만듭니다. 서버는 11개 이상을 받지 않습니다 |
| 콜렉션 | 계정 · 앱(웹)마다 코스 10개. 새 코스가 11번째면 가장 오래 달리지 않은 코스를 덮어씁니다(`replaced`) |
| 받는 수 | 계정마다의 제한은 없습니다(10분에 100개 규칙은 없앴습니다). 한 요청은 런 10개 · 본문 4 MB 까지이고, 요청 단위의 IP 제한(쓰기 분당 20, 읽기 분당 60)은 평가 API 와 함께 셉니다 |
| 계정 만들기 | 막지 않습니다 |
| 지갑(익명 계정) 코스 | 마지막으로 달린 날 기준 30일까지. 그 지갑이 런을 올릴 때와 cron(1분마다)이 지웁니다. 30일 밖의 런은 받지 않습니다(`expired`) |
| 세션 코스 | 사용자가 지울 때까지(`DELETE /nav/trials[?app=]`) |
| 사용자마다 한 번 등록 `nav_registrations` | 사용자 코스 id = `hashId(계정 키 + 코스 id)` (웹의 `itemId = hashId(team + cc + link)` 방식). 이 id 가 이미 있으면 그 사용자는 그 코스를 등록하지 않습니다(`duplicate`): 공개 합계 · 판정에 들어가지 않고 내 콜렉션의 내 기록(내가 달린 횟수 · 최고 · 마지막)만 고칩니다. 콜렉션에서 밀려나거나 지운 코스를 다시 달려도 다시 등록하지 않도록 콜렉션과 따로 두고 지우지 않습니다(되돌릴 수 없는 해시뿐) |
| 계정 없는 합계 `nav_courses` | 코스 id 마다 등록한 사용자 수(`runs`) · premium · discount · 구간 시간 합 · 최고 기록. 그 코스 id 가 없으면 새로 만들고(`created`), 있으면 처음 등록하는 사용자의 런만 더합니다. 줄지 않고 영원히 둡니다 |
| 다른 사용자에게 보이기 | 등록한 사용자가 11명 이상인 코스만(6차 답 8: 같은 해시의 기록이 10개 이하면 공개하지 않음). 한 사람이 거듭 달려도 공개되지 않습니다. 게스트도 볼 수 있습니다 |
| 코스 등급 | 결정이 난 런이 3개 이상이고 premium 이 2/3 이상이면 premium, discount 가 2/3 이상이면 discount, 그 사이는 neutral, 모자라면 unrated. 기기 · 서버가 같은 정수 비교를 씁니다. 공식 판정(PD-Verdict v2 nav 트랙)이 생기면 그것으로 바꿉니다 |
| 고스트 따라가기 판정 | 런이 따라간 다른 사용자의 공개 코스(`ghost`)를 밝히고 시작 · 결승이 그 코스와 같으면, 그 코스의 중간 체크포인트 중 지난 것은 premium, 지나지 않은(다른 길로 간) 것은 discount 로 셉니다(`route_marks` · `routeMarks`, 기기와 서버가 같은 표로 시험). 결승이 다르면 목적이 다른 것이라 판정하지 않습니다. 판정 수는 체크포인트 id 로 세고, 고스트를 받는 쪽에는 체크포인트마다의 수와 등급(`checkpoints`)을 줍니다 |
| `nav_counts` | (해시, 날짜, 종류) 개수. 해시는 코스(센 런 중 결정이 난 것) 또는 체크포인트(고스트 따라가기 판정). 판정 cron 의 입력 |
| 대표 벡터 `nav_vectors` | 체크포인트에 벡터를 붙여 보내면 (해시, 공간)마다 처음 받은 것 하나 |

- **API**: `POST /nav/trials` 본문 `{ "runs": [trial/1, …] }` → 런마다 `stored` · `repeat`(+ `best`) · `replaced`(+ `evicted`) · `expired` · `invalid`(+ `error`)와 `created`(그 코스 id 가 서버에 처음 생겼는지) · `duplicate`(이 사용자가 이미 등록한 코스인지), 고스트를 밝혔으면 `route`(따라간 · 비켜 간 체크포인트 수, 판정하지 않았으면 null). `GET /nav/trials?app=<앱 해시>` → `mine`(인증했을 때 내 콜렉션, 최근에 달린 순) · `others`(공개 코스: 횟수 · premium · discount · 등급 · `record` · `average` 고스트 · 체크포인트마다의 `checkpoints`). 인증을 보냈는데 틀리면 401, 안 보내면 게스트로 `others` 만 줍니다.
- **인증**: 지갑 헤더(`x-pd-address` · `x-pd-time` · `x-pd-signature`)가 하나라도 있으면 지갑으로만, 없으면 세션(QR 메일 로그인)으로 확인합니다. 계정 칸은 `HMAC(AUTHOR_KEY_SECRET, "nav-account|<종류>|<주소>")` 라 표에 지갑 주소 · 이메일이 그대로 남지 않습니다.
- **서버가 다시 확인하는 것**: 체크포인트 id(보냈으면)와 코스 id 를 다시 계산해 맞춰 보고(체크포인트 id 는 hashId 라 하나에 0.4 ms 남짓 들어, isolate 안에 담아 둡니다), 구간 시간은 0 에서 시작해 줄지 않으며 100 ms 의 배수, 잇달아 같은 체크포인트 없음, `total_ms` = 마지막 구간 시간, 한 시간 안, 날짜는 2026-01-01 ~ 내일입니다.
- **남는 것**: 계정 만들기를 막지 않으므로 지갑을 많이 만들면 등록한 사용자 수 · 등급을 부풀릴 수 있습니다. 콜렉션 · 합계 갱신은 읽고 고쳐 쓰기라, 같은 계정이 동시에 두 요청을 보내면 하나가 덮이거나 합계에 두 번 들어갈 수 있습니다(등록 표 자체는 하나만 남습니다). 등록 표는 사용자가 지워도 남습니다(다시 등록하지 않기 위해서).
- **아직 없는 것**: `nav_counts` 를 v2 `implicit` 관측으로 바꿔 판정하는 cron, 벡터 색인(Vectorize), 앱의 체크포인트 수집 · 고스트 화면(M2). 0007 마이그레이션은 실제 D1 에 아직 적용하지 않았습니다.

### 19-4. 검증

| 묶음 | 실행 | 결과 |
| --- | --- | --- |
| 앱 단위 (CPU, 포크) | `cd app && cargo test --release --lib` | 242개 통과, 1개 무시(기존 240 + 장치 선택 2). 포크로 바꾼 첫 빌드는 29분: 공유 의존성 판이 바뀌어 lance 묶음까지 다시 컴파일 |
| 앱 ROCm 빌드 | `cargo test --release --lib --features rocm --no-run` (`VCToolsInstallDir` = 14.44) | 컴파일 성공(2분 42초) |
| GPU 대조 (ROCm, RX 6600) | 위에서 만든 시험 실행 파일에 `utils::tests::gpu_backend_runs_and_matches_cpu --exact` (PATH 에 HIP SDK `bin`) | 통과: 장치 `Rocm { gpu_id: 0 }`, 0.93초. 프로세스는 정상 종료했고, 남은 프로세스 · VRAM 점유는 없었습니다 |
| 데스크톱 창 (Tauri) | `cd app/src-tauri && cargo build --release` | 빌드 성공(34분, lance 묶음까지 다시 컴파일). 실행은 하지 않음 |
| crates | `cd crates && cargo test` | 60개 통과: `premium-discount-verdict` 5개(v1 사례 19개 · v2 사례 25개), `premium-discount-nav` 46개(`hashId` 는 ethers 기준값 4개와 같음, 행동 기록 → 결정 기록 → 서버 판정 한 줄 시험, 개인정보 화면 시험, 타임트라이얼 11개: 앱 · 코스 해시가 ethers 와 같음, 구간 시간 반올림 · 결승 날짜, 끝 쪽 10개, 코스 10개 덮어쓰기, 고스트 안내, 고스트 따라가기 판정, 웹 코스(페이지 · 요소 · 제스처, ethers 기준값)), 웹 링크 · 셀렉터 5개(해시 라우팅, 같은 틀의 링크는 같은 페이지, 셀렉터에서 해시 class · 상태 class · 글자 빠짐, 깊이 5), 제스처 5개(탭 · 클릭 · 스와이프 4방향, 드래그 · 길게 누름 제외, 스크롤 묶음 · 뒤집기, rage), `premium-discount-identity` 9개(시드 구문 → 개인 키 · 주소, 서명이 ethers 와 바이트까지 같음, 단어표는 공식 english.txt 의 SHA-256 과 같음) |
| cron | `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test cron/test/*.test.ts` | 41개 통과(v2 계약 2개 포함) |
| www | `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts` | 78개 통과. 지갑 서명 확인 4개(계약의 요청, 본문 · 경로 · 주소 · 형식 · 시각 위조, 새 ethers 지갑)와 타임트라이얼 16개(체크포인트 id · 코스 id 체인이 Rust 와 같음, 같은 길은 한 번만 등록 · 갈라진 길은 이전 코스 id 에 이어 새 코스, 사용자마다 같은 코스는 한 번만 등록(다시 달림 · 밀려난 뒤 · 지운 뒤), 코스 id 가 없으면 새 코스(created), 런마다 셈, 웹 페이지 · 요소 · 제스처 코스, 제스처까지 보는 따라가기 판정, 고스트 따라가기 판정 표 · 따라감 premium · 비켜 감 discount · 판정하지 않는 경우, 등급 표, 다시 달리기 · 새 코스 생성, 코스 10개 덮어쓰기, 받는 수 제한 없음(150개), 체크포인트 10개 · 형식, 공개 하한 11 · 고스트, 지갑 30일 · cron, 계정 키, 지우기, 인증). 한도를 일부러 늦추면(코스 11 · 체크포인트 11 · 공개 10 · 매번 셈 · 결승 비교 없음 · 세지 않은 런도 판정) 1 ~ 3개씩 실패합니다 |

### 19-5. 알아 둘 것

- **양자화 시험 하나를 나눴습니다.** `quant::tests::the_dequant_kernel_keeps_quantized_storage_and_is_at_least_as_accurate_as_native` 는 "가중치만 양자화한 쪽(Dequant)이 활성값까지 양자화한 쪽(Native)보다 나빠지지 않는다"를 네 비트에서 봤습니다. 포크에서는 K-quant 의 Native 가 비슷하거나 조금 낫습니다.

    | 비트 | Dequant | Native | 차이 |
    | --- | --- | --- | --- |
    | Q8_0 | 0.9998600 | 0.9997638 | Dequant +9.6e-5 |
    | Q6K | 0.9985413 | 0.9985039 | Dequant +3.7e-5 |
    | Q5K | 0.9934505 | 0.9935023 | Native +5.2e-5 |
    | Q4K | 0.9850013 | 0.9851969 | Native +2.0e-4 |

    Q5K 의 양자화 · 역양자화 · 내적 코드는 candle 0.9.2 와 같고, 바뀐 것은 CPU 양자화 행렬곱 경로입니다(포크의 `fast_mmq` · `fast_mmvq`, gemm 0.18). 앱이 쓰는 Q8_0 은 원래의 엄격한 조건을 그대로 지킵니다. K-quant 는 두 커널이 같은 수준(코사인 차이 5e-4 안)인지만 봅니다.
- **GPU 를 쓴 프로세스는 끝나기 전에 `release_gpu` 를 부릅니다.** 이 PC 의 Windows HIP 는 GPU 시험 뒤 프로세스가 종료 단계에서 멈춘 적이 있습니다(noto-sandbox 에서 11번 중 7번). 멈춘 프로세스는 재부팅 전까지 코어 하나와 VRAM 0.8 ~ 1.2 GB 를 쥡니다. 원인은 포크의 비동기 메모리 풀이 남은 채 끝나는 것이었습니다(gravity-ocr). GPU 대조 시험은 `release_gpu` 를 부르고 정상 종료했습니다. GPU 시험은 다른 GPU 작업이 없을 때 한 프로세스에서만 돌립니다(8 GB VRAM).
- **Android 빌드**는 포크로 바꾼 뒤 확인하지 않았습니다.
- 기존 임베딩(embeddinggemma)은 CPU 그대로이므로, 평가 동기화의 임베딩 공간은 바뀌지 않습니다.

## 20. 3단계 오버레이 창 (2026-11-17)

데스크톱 창을 raon-zena 처럼 화면 오른쪽 아래에 붙는 3단계 오버레이로 바꿨습니다. 캐릭터는 없습니다. 앱 코어(`app/src/*`)는 한 줄도 바꾸지 않았습니다. 바꾼 것은 Tauri 셸(`app/src-tauri`)과 화면(`app/ui`)입니다.

### 20-1. 세 단계

| 단계 | 크기 (논리 픽셀) | 보여 주는 것 | 옮기기 |
| --- | --- | --- | --- |
| 0 알약 | 360 x 56 | 동기화 점과 짧은 말(등록됨 · 전송 대기 N건 · 검수 중 N건 · 오프라인 · 설정 필요), 최근 한 줄(1분 안의 알림, 아니면 가장 최근 내 글). 배경은 82% 불투명 | 누르면 1단계. 5px 넘게 움직이면 누르기가 아니라 창 끌기(`startDragging`) |
| 1 미니 피드 | 360 x 260 | 최근 내 평가 3개. 새 글이 맨 아래이고 불투명도는 아래부터 1 · 0.8 · 0.55, 맨 위 줄은 `mask-image` 그라디언트와 `backdrop-filter` 로 흐려집니다. 줄마다 `[cc · 상태]` 와 미리보기, 상태 · 판정 아이콘(⭐ 추천 · 🔻 비추천 · ✓ 등록됨 · 📤 전송 대기 · ⏳ 검수 중 · ⛔ 거부됨 · ⚠ 실패). 아래에 빠른 입력 한 줄 | 맨 위 줄이나 "전체 ▸" 는 2단계. Esc · 접기 버튼 · 창 바깥 누르기(창이 포커스를 잃음)는 0단계. 빠른 입력에 쓰던 글이 있으면 바깥을 눌러도 접지 않습니다 |
| 2 서랍 | 400 x 580 | 검색(`cmd_search_merged`), 걸러 보기 칩 [전체] [⭐ 추천] [🔻 비추천] [⏳ 전송 대기/검수 중] 과 [현재 도메인만], 가운데 타임라인(접이식 평가 쓰기 · 검색 결과 · 내 평가 카드, 추천 / 비추천 버튼, 답글), 아래 접이식 "계정 · 지갑 · 모델" | Esc 는 1단계, 접기 버튼은 0단계, ✕ 는 앱 끄기 |

- 단계 규칙(크기, 옮기기, 누르기 / 끌기 경계 5px, 창 자리, 피드 줄 고르기, 동기화 점, 걸러 보기, 지갑 · 모델 한 줄)은 순수 함수로 `app/ui/stage.ts` 에 두고 `stage.test.ts` 로 시험합니다.
- 창 크기와 위치는 화면이 바꿉니다. `window.__TAURI__.window.getCurrentWindow()` 의 `setSize(LogicalSize)` · `setPosition(PhysicalPosition)` 을 쓰고, 자리는 `currentMonitor()` 의 작업 영역(작업 표시줄을 뺀 곳) 오른쪽 아래에서 16px 띄웁니다. 배율(scaleFactor)을 곱해 물리 픽셀로 계산합니다. 사용자가 끌어 옮긴 뒤에는 지금 창의 오른쪽 아래 모서리를 지키며 커지고 작아지고, 작업 영역 밖으로는 나가지 않게 자릅니다. 커질 때는 먼저 옮기고 키우며, 작아질 때는 먼저 줄이고 옮깁니다. 창 조작은 하나씩 차례로 합니다.
- `data-stage="0|1|2"` 는 `#shell` 에 붙고, 단계마다 보이는 부분은 CSS 가 고릅니다. `style.css` 의 `--stage-w` · `--stage-h` 는 `STAGE_SIZE` 와 같아야 하며 시험이 확인합니다.
- 개발용 브라우저(`dev-server.mjs`)에는 `__TAURI__` 가 없으므로 CSS 만 바뀝니다. 같은 크기의 상자가 페이지 오른쪽 아래에 놓입니다. 오류는 나지 않습니다. 미리보기용으로 `?stage=0|1|2` 를 받습니다.

### 20-2. 창 설정과 권한

- `tauri.conf.json` 의 창: 360 x 56, `transparent` · `decorations: false` · `alwaysOnTop` · `skipTaskbar: false` · `resizable: false` · `shadow: false`. 예전의 `minWidth` 380 · `minHeight` 520 은 알약 높이(56)를 막으므로 지웠습니다.
- macOS 의 투명 창에는 `app.macOSPrivateApi: true` 가 필요합니다(키 이름은 tauri-utils 2.10.2 의 `config.rs` 에서 확인). 이 값을 켜면 tauri-build 가 `tauri` 의존성에 `macos-private-api` 기능이 있는지 확인하고 다르면 빌드를 멈추므로, `app/src-tauri/Cargo.toml` 에 그 기능을 더했습니다. 새 의존성은 생기지 않습니다(`Cargo.lock` 그대로). 이 비공개 API 를 쓰면 Mac App Store 에는 낼 수 없습니다.
- `capabilities/default.json` 에 `core:window:allow-set-size` · `allow-set-position` · `allow-start-dragging` · `allow-close` 를 더했습니다. 모니터 · 창 위치 · 창 크기 읽기(`allow-current-monitor` · `allow-outer-position` · `allow-outer-size`)는 이미 `core:default` 에 있습니다. 항상 위 켜고 끄기는 쓰지 않으므로 `allow-set-always-on-top` 은 넣지 않았습니다.
- 처음 뜰 때 창은 운영체제가 정한 자리에 360 x 56 으로 나타났다가, 화면이 첫 명령을 부르는 동안 오른쪽 아래로 옮겨집니다. 숨긴 채 띄우고 옮긴 뒤 보이게 하는 방법은, 화면 스크립트가 실패하면 창이 영영 보이지 않으므로 쓰지 않았습니다.
- 투명 창의 `backdrop-filter` 는 웹뷰 안의 내용만 흐립니다. 창 뒤의 바탕 화면은 흐려지지 않습니다.

### 20-3. 투표 명령 등록 (고친 것)

- `cmd_vote` · `cmd_verdicts` 는 코어(`app/src/commands.rs`)와 화면(`ui/commands.ts`)에는 있었지만 `src-tauri/src/lib.rs` 의 `generate_handler!` 에 없어서, Tauri 창에서는 추천 / 비추천이 실패했습니다(18절의 작업은 dev-server 브리지로만 확인됨). 다른 명령과 같은 방식으로 감싸 등록했습니다. 인자 타입(`VoteArgs` · `VerdictsArgs`)은 상위 크레이트가 다시 내보내지 않아 `premium_discount_app::commands::` 경로로 가져옵니다.
- 화면은 이제 내 글(서버 id 가 있는 등록된 글)의 판정을 `cmd_verdicts` 로 읽어 피드 아이콘 · 내 평가 배지 · 추천 / 비추천 칩에 씁니다. 코어가 판정을 10분 캐시하므로 10초마다의 확인이 서버에 매번 묻지 않습니다. 명령을 모르는 이전 실행 파일이면 판정 없이 보여 줍니다.

### 20-4. 알아 둘 것

- **피드 명령은 없습니다.** 미니 피드와 알약은 서랍의 "내 평가" 와 같은 행(Rust → Dexie 캐시, 최신순 50개)을 씁니다. 남의 새 평가가 흘러오는 피드가 아닙니다.
- **지갑 명령은 없습니다.** 지갑 칸은 로그인 세션의 계정 주소만 보여 주고 "지갑(시드 구문) 명령은 아직 앱에 없습니다" 라고 적습니다. 시드 지갑 서명은 `crates/premium-discount-identity` 에만 있고 앱 명령으로 이어 붙이지 않았습니다.
- **빠른 입력**은 평가 쓰기와 같은 `cmd_submit_evaluation` 을 현재 도메인(평가 쓰기의 도메인 칸, 마지막에 쓴 값을 기억)으로 부릅니다. 도메인이 비어 있으면 보내지 않고 서랍의 평가 쓰기를 펼쳐 도메인 칸으로 보냅니다. 답글은 서랍에서만 답니다.
- **[현재 도메인만]** 칩은 예전의 "입력한 도메인에서만 찾기" 체크와 같은 칸(`#onlyCc`)입니다. 이제 검색뿐 아니라 내 평가 목록과 검색 결과도 그 도메인으로 거릅니다. 다른 칩은 화면에서 거르기만 합니다(검색을 다시 하지 않습니다).
- **시작 안내**(모델 없음 · 설정 오류)는 2단계 크기로 바로 보여 줍니다. 접으면 알약이 "설정 필요" 를 보여 주고, 누르면 1단계를 건너뛰고 시작 안내로 돌아옵니다. 모델 내려받기 화면은 그대로입니다.
- **모바일(Android · iOS)** 은 창 크기를 바꿀 수 없어 Tauri 가 창 속성을 무시합니다. 화면은 `cmd_app_status` 의 platform(답이 오기 전에는 사용자 에이전트)으로 모바일을 알아보고, 2단계 서랍을 전체 화면으로만 씁니다(`html[data-mode="full"]`). 접기 · 끄기 버튼은 숨기고 Esc 로 단계가 바뀌지 않습니다. 안전 영역 여백(`env(safe-area-inset-*)`)을 둡니다. Android 빌드는 이번에 돌려 보지 않았습니다.
- 서랍 아래 접이식에는 로그인(기존 QR · 메일 로그인 창), 지갑, 모델(임베딩 모델 설치 상태 한 줄, 욕설 필터 laya 받기, items 초안 모델과 초안 만들기)이 있습니다. 예전에 평가 쓰기 아래에 있던 욕설 필터 줄과 오른쪽 열의 초안 카드를 여기로 옮겼습니다.

### 20-5. 검증

| 묶음 | 실행 | 결과 |
| --- | --- | --- |
| 화면 단위 | `cd app/ui && npm test` | 70개 통과(기존 52 + `stage.test.ts` 18: 단계 크기와 CSS 값 일치, 오버레이 · 전체 화면, 단계 옮기기, 5px 경계, 창 자리(배율 150%, 음수 좌표의 왼쪽 모니터, 끌어 옮긴 모서리 고정, 작업 영역 밖 자르기), 커질 때 · 작아질 때 순서, 피드 순서 · 불투명도, 판정 등급 · 아이콘, 동기화 점, 알약 한 줄, 걸러 보기, 지갑 · 모델 한 줄) |
| 화면 묶기 | `npm run build` | 성공 (`dist/main.js` 346 KB) |
| 타입 | `tsc --noEmit --strict … main.ts stage.ts` (전역 tsc 5.6.3) | 오류 없음 |
| Tauri 셸 | `cd app/src-tauri && cargo check` (`CARGO_BUILD_JOBS=2`, `PROTOC`) | 성공, 경고 없음. 처음에는 `ui/dist` 가 없어 `generate_context!` 가 멈췄고(`frontendDist` 경로 없음), `npm run build` 뒤 다시 돌려 통과. 창 권한 이름과 `macos-private-api` 기능 짝도 이 단계(tauri-build)가 확인합니다 |
| 화면 동작 (헤드리스 Edge · CDP, 가짜 명령 서버) | 저장소 밖 스크립트 | 30개 확인 통과, 스크립트 오류 없음. 알약 → 피드 → 맨 위 줄 → 서랍, Esc 한 단계씩, 칩 거르기(5건 → 추천 1 · 대기 2), 도메인 없는 빠른 입력 → 서랍 평가 쓰기, 지갑 · 모델 한 줄, 모바일 전체 화면(가로 넘침 없음, 접기 숨김), 시작 실패 → 2단계 · 알약 "설정 필요", 가짜 Tauri 창 API(배율 1.5, 작업 영역 2880 x 1560)로 창 조작 순서와 값(처음 `pos 2316,1452 → size 360x56`, 1단계 `pos 2316,1146 → size 360x260`, 접기 `size → pos`), 5px 넘는 움직임은 `startDragging` 이고 단계 그대로, 3px 는 누르기 |

GUI 앱 자체(`tauri dev` · `tauri build`)는 돌려 보지 않았습니다. 실제 Windows · macOS 창에서의 투명 배경, 항상 위, 끌기, 작업 표시줄 위치에 따른 자리는 앱을 켜서 한 번 확인해야 합니다.

## 21. raon-zena 와의 연결 · 역할 분담 (2026-11-17)

마스터 기획서(5대 과업) 중 이 저장소 밖에서 한 일과, 이 저장소 기준으로 확인한 것입니다. raon-zena 쪽 기록은 그 저장소의 `docs/MASTER_PLAN_5.md` 입니다.

- **`crates/` 를 raon-zena 가 링크합니다.** raon-zena 작업 공간이 `premium-discount-verdict` · `premium-discount-identity` · `premium-discount-nav` 를 이 저장소의 git 리비전 `540311b` 로 고정해 씁니다. `zena-verdict` 는 판정 수식 사본을 지우고 `premium_discount_verdict` 를 다시 내보내며, `premium-discount-mcp` 의 `hashId` 는 `premium_discount_identity::hash_id` 를 부릅니다. `premium-discount-nav` 는 MCP 도구 `pd_nav_number`(합치기 · 1 ~ 9 번호 · 개인정보 화면)로 연결했습니다. 따라서 이 크레이트들의 공개 API(`Params` · `verdict` · `hash_id` · `merge` · `number` · `Region` 등)를 바꾸면, raon-zena 가 리비전을 올릴 때 함께 고쳐야 합니다.
- **데스크톱 설치본에서 코어 찾기.** raon-zena 의 `premium-discount-mcp` 는 `premium-discount-app`(헤드리스 코어, `serve_lines`)을 `%LOCALAPPDATA%\premium.discount\` · `%ProgramFiles%\premium.discount\` 에서도 찾습니다. 지금의 Tauri 번들은 창 실행 파일만 넣으므로, 이 단계가 동작하려면 코어를 `bundle.externalBin` 사이드카로 함께 넣어야 합니다(아직 하지 않음: 빌드 전에 코어를 대상 삼중항 이름으로 만들어 두는 단계가 필요합니다).
- **embeddinggemma-2 와 granite 의 역할.** 새 코드는 없습니다. 기획서 4절의 표(Q8_0 520.1 → 138.1 MB · 코사인 0.999906, 임베딩 표 mmap, 타워 지연 상주 · 교차 스왑, 비전 층 0 · 오디오 층 8 F32)는 16 ~ 17절에 이미 구현 · 실측되어 있습니다. 숫자 하나만 다릅니다. "짧은 문장 단건 120 ~ 160 ms" 는 F32 기준(약 0.15초)이고, Q8_0 Dequant 경로는 약 0.28초입니다(16-5). granite(`granite-97m-r2-cls-v1`)는 raon-zena 의 실시간 라우팅과 `premium-discount-nav` 라벨 공간에서만 씁니다.
- **SigLIP2.** raon-zena 의 `raon-vision` 이 이제 `google/siglip2-so400m-patch16-naflex` 를 직접 읽습니다(transformers 참조와 이미지 코사인 1.000000 · 텍스트 0.999999). 이 앱의 접근성 비전도 같은 체크포인트를 쓸 계획(19절)이므로, 전처리(평균 · 표준편차 0.5, bilinear)와 위치표 antialias 는 그 구현을 기준으로 맞추면 됩니다.

## 22. 공용 라이브러리 통합 · 이름 표준화 (2026-11-17)

`crates/` 를 9개 crate 로 넓히고 이름의 약어(`pd-*`)를 없앴습니다. crate 목록과 원칙은 [`crates/README.md`](crates/README.md) 에 있습니다.

| 한 일 | 내용 |
| --- | --- |
| 이름 | `pd-identity` · `pd-verdict` · `pd-nav` → `premium-discount-identity` · `-verdict` · `-nav` (Rust 이름 `premium_discount_*`). 계약 값(`pd-verdict/1` · `/2` recipe, `x-pd-*` 헤더, MCP 도구 `pd_*`)은 서버 · DB 와 맞물려 그대로 둡니다 |
| `premium-discount-mcp` | raon-zena 에서 옮겨 왔습니다. raon-zena 의 `zena-avatar-mcp` 에 기대던 MCP 틀은 `premium-discount-mcp-protocol` 로 떼어 냈습니다(raon-zena 는 이것을 다시 내보냅니다) |
| `premium-discount-schema` | `cc` 정규화 · `to`/`bcc` 주소(MCP 에서), 명령 계약(앱 `commands.rs` 에서): 인자 7 · 결과 11 타입, `CommandError`, `COMMANDS`(17) · `ERROR_KINDS`(15). 앱은 다시 내보내고, 앱 안의 타입에 기대는 `VerdictsResult` · `AppStatus` 만 앱에 남았습니다 |
| `premium-discount-filter` | 사전 필터 `LocalFilter` · `Verdict`(앱 `filter.rs` 에서). 앱의 `ProfanityFilter` 트레이트는 오류 타입이 앱의 `AppError` 라 앱에 두고 이 타입을 감쌉니다. `premium-discount-mcp` 는 앱을 띄우기 전에 이 사전으로 먼저 거릅니다(`status: blocked`, `filter: dictionary`) |
| `premium-discount-sds` | `Moments`(Welford 더하기 · 합치기 · 빼기, 모집단 · 표본 분산)와 스코프 백오프. nav 의 마찰 기준선과 raon-zena `zena-sds` 가 씁니다(직렬화 모양 `n` · `mean` · `m2` 그대로) |
| `premium-discount-manifest` | `.part` · SHA-256 · 확인 뒤 이름 바꾸기 · 안전한 상대 경로(앱 `model_fetch.rs` 의 함수들). raon-zena 아바타 카탈로그도 씁니다 |
| 사이드카 | `app/src-tauri/tauri.sidecar.conf.json` + `sidecars.mjs`: 설치본에 `premium-discount-app` · `premium-discount-mcp` 를 함께 넣습니다. 기본 `tauri.conf.json` 에 넣지 않은 이유: tauri-build 는 `externalBin` 파일이 없으면 `cargo build` 자체를 멈춥니다 |
| 바로잡은 것 | `cmd_vote`(20절) 외에, `premium-discount-identity` 의 단어표 해시 시험이 Windows 체크아웃(`core.autocrlf`)에서 실패하던 것: 시험이 줄 끝을 LF 로 맞추고 `.gitattributes` 로 `english.txt` 를 LF 고정 |

**기획서와 다른 점**
- 필터 판정은 기획서의 `Pass` · `Block { reason }` 이 아니라 기존 그대로 `Clean` · `Profane` 입니다. 사전 필터는 실패하지 않으므로 crate 의 `LocalFilter::judge` 는 `Verdict` 를 바로 돌려줍니다.
- 명령은 기획서의 16개가 아니라 17개입니다(코어 13 + 모델 3 + `cmd_item_draft`).
- crates 의 원칙 문구를 "HTTP 없음"에서 "블로킹 `GET /graph*`(ureq) 하나뿐"으로 고쳤습니다(`premium-discount-mcp` 가 들어왔으므로).

**검증**

| 묶음 | 결과 |
| --- | --- |
| `cd crates && cargo test` | 전부 통과: identity 9, verdict 5, nav 46, schema 8 (계약 3 · 주소 · cc 5), filter 8, mcp-protocol 2, mcp 13 + 서버 7 + E2E 1 (환경 변수가 없으면 건너뜀), sds 5, manifest 3 |
| 실제 E2E (`premium-discount-mcp` → 릴리스 `premium-discount-app` → `mock_server.py`, 텍스트 모델) | 통과: 오프라인 queued → 병합 flush → stored, 게스트 · 로그인 투표와 판정, 서버가 받은 768차원 벡터(노름 1.0000003) |
| `premium-discount-mcp` 릴리스 바이너리 MCP 왕복 | 도구 9개, 욕설은 앱 없이 `blocked` |
| `cd app && cargo test --release --lib` | 237 통과, 2 무시 (사전 필터 시험 8개는 `premium-discount-filter` 로 옮겨 감) |
| `app/ui` `npm test` | 70 통과 |
| src-tauri `cargo check` + `TAURI_CONFIG` 로 사이드카 지정 | 통과. tauri-build 가 `binaries/<이름>-x86_64-pc-windows-msvc.exe` 를 삼중항 없는 이름으로 실행 파일 옆에 복사함을 확인. 설치 파일(`tauri build`)은 만들지 않았습니다 |

## 23. 범용 엔진 통합 · 런타임 판 맞추기 (2026-11-17)

raon-zena 의 범용 엔진을 `crates/` 로 옮기고, Relay · 창 계산 crate 를 새로 만들고, 두 저장소의 런타임 판을 맞췄습니다.
- crate 목록과 도메인 경계(이 저장소 = 장소 · 대상 · 화면 영역의 열린 집합, raon-zena = 제나 표현 사전의 닫힌 집합)는 [`crates/README.md`](crates/README.md) 에 있습니다.
- raon-zena 쪽 기록은 그 저장소의 `docs/MASTER_PLAN_ENGINE_UNIFICATION_AND_VERSION_SYNC.md` 입니다.

| crate | 출처 | 한 일 |
| --- | --- | --- |
| `onnxruntime-sys` · `onnxruntime` | logis-center `commerce/crates/onnxruntime(-sys)` (onnxruntime-rs 0.0.14 벤더 판, ORT 1.28.0). 2026-11-18 에 logis-center 판을 기준으로 다시 맞춤 (앞서는 raon-zena 판) | logis-center 판 그대로에, 기본 꺼진 기능 `runtime-load` 만 더함: ONNX Runtime 을 링크하지 않고 실행할 때 불러오고(`dynload`, 찾는 곳 `ORT_DYLIB_PATH` · 실행 파일 옆 · `%LOCALAPPDATA%\{premium.discount,RaonZena,logis-center}\models\onnxruntime` · 빌드가 준비한 라이브러리), 빌드는 라이브러리 준비에 실패해도(`ORT_STRATEGY=skip`, 내려받기 404) 계속함. nlp 의 `stanza` 기능이 켬. GitHub 배포판을 받지 못하면 같은 판을 NuGet `Microsoft.ML.OnnxRuntime` 에서 꺼냄(logis-center 판에 함께 넣은 예비 경로, 2026-11-18 github.com/microsoft/onnxruntime 404). 자체 시험은 돌리지 않음(`[lib] test = false`: bindgen 배치 시험이 Rust 1.99 에서 null 역참조 오류) |
| `premium-discount-nlp` | raon-zena `zena-nlp` | 기능 `granite`(candle) · `stanza`(ONNX). 모델은 `PD_*` → `RAON_*` → `premium.discount` · `RaonZena` · `logis-center` 의 `models` 순서로 찾음 |
| `premium-discount-nms` | raon-zena `nlp-nms-router` 의 중립 부분 | `DomainBank<M, E>`(항목 내용 `M` 은 부르는 쪽 타입, `DomainSpec` 으로 만듦), 호출어는 `set_wake_words` 로 정함(기본 없음) |
| `premium-discount-sds` | raon-zena `zena-sds` 전체 | `Moments` 와 합침. 장부 recipe 그대로 |
| `premium-discount-sensor` | raon-zena `zena-sensor` + `zena-core::geometry` | 화면 종류 `VisionKind` 도 여기로(사전 · 앵커 은행이 같은 값) |
| `premium-discount-vision` | raon-zena `raon-vision` | 사전(`zena-lexicon`) 의존을 뺌: `AnchorBank::from_specs`(`VisionDomainSpec`), `compute_anchors(… forms: Fn …)`. 예제는 CPU |
| `premium-discount-quant` | raon-zena `raon-quant` 의 범용 부분 | 새 `trim`: logis-center 의 어휘 잘라 내기 규칙(아래) |
| `premium-discount-relay` | 새로 만듦 (raon-zena `relative-value-sds.md` 3.4 · 4.1절) | Relay Key 뽑기 · 다국어 키 묶기 · 문장 맞추기 · 바뀐 주장 → 대체(`Supersedes`) · 중복 · 의미 연결 |
| `premium-discount-window` | 새로 만듦 (`app/ui/stage.ts` + raon-zena `zena-core::window`) | 3단계 크기 · 자리 · 누르기와 끌기 · 스타일 비트 · 알파 · 프레임 간격 · 제자리 돌아가기, 점 아래 창 · 앞 창 찾기(Win32). `stage.ts` 와 같은 시험 벡터(`tests/stage_cases.json`)를 Rust · TS 가 함께 읽음 |

**런타임 판 맞추기**

| | 전 (app/) | 전 (raon-zena) | 후 (둘 다) |
| --- | --- | --- | --- |
| lancedb · lance | 0.31 · 8.0.0 | 0.41 · 14.0.2 | 0.41 · 14.0.2 |
| tokenizers | 0.21.4 + 0.22.2(포크 경유) | 0.22.2 | 0.22.2 하나 (`default-features = false`, `onig`) |
| candle 포크 | `rev = ebaf93d` | `branch = master` (잠금은 ebaf93d) | `rev = ebaf93d` (raon-infer 의 `candle-rocm` 포함), crates/ 도 같은 rev |
| datafusion | 53.1 | 54.1 | 54.1 |

앱 코드는 한 줄도 바뀌지 않았습니다. lancedb 0.41 은 앱이 부르는 API 서명이 0.31 과 같습니다. 바뀐 것은 내부입니다(lance 14 의 새 데이터는 파일 형식 V2_2).

**어휘 잘라 내기 규칙 (`premium-discount-quant::trim`)**
- logis-center `models/common/vocab_trim.rs`(PR #84)의 규칙을 이 crate 에서 다시 썼습니다.
  - 남김: 공통 코어 · 문서 언어의 문자 체계 전부 · ASCII 전부 · 도메인 글 · 섞인 한자 상위 4,096, 64의 배수.
  - 행 수 예산을 두는 규칙은 두지 않습니다.
- raon-quant 의 `ColdGuard` 와 합쳤습니다: `trim::slice` → `VocabSlice{kept, cold}`. 규칙이 무엇을 놓쳐도 탐욕 디코딩은 전체 어휘와 같습니다.
- 실제 Qwen3.5 어휘(248,320)에서 남는 행 수:

  | 언어 | 남는 행 |
  | --- | --- |
  | en | 131,328 |
  | ko + en | 138,112 |
  | ja + en | 190,272 |
  | 12개 언어 | 220,160 |

  앱의 초안 모델(`qwen35.rs`)은 지금 16,384행 고정 예산을 쓰고 영어가 15 ~ 17번째 토큰부터 달라집니다(17-4절).
- 이 앱에 그 모델(Qwen3.5-0.8B safetensors)이 이 PC 에 없어, 초안 경로는 바꾸지 않았습니다. 두 가지를 실측한 뒤 정할 일입니다.
  - 새 규칙(영어 전부): 속도 이득이 줄어듭니다.
  - 작은 예산 + ColdGuard: 결과는 같고, 속도는 잘린 행 묶음을 다시 계산하는 횟수에 달립니다.

**검증**

| 묶음 | 결과 |
| --- | --- |
| `cd crates && cargo test --release --workspace` | 전부 통과. 새 crate: nlp 11 + granite 참조 4 · 벤치 1 · Stanza ko 1 · 런타임 5(실제 모델), nms 15(`--features stanza` 도), sds 30 + 장부 파일 6, sensor 12, quant 11, vision 10, relay 4, window 24 + 공유 벡터 5. 경고 없음 |
| Relay 실제 모델 (`--features neural --test real_models`) | 한국어 패치 노트 "피해가 20% 감소" → "15% 감소" 를 `Supersedes`(문장 맞추기 코사인 0.994, 포함률 1.0)로. 키 묶기: 말레니아 → malenia, 라다곤 → radagon, 다른 이름끼리는 따로 |
| google SigLIP2 (`premium-discount-vision` 예제 `ve_xcheck`, CPU) | 옮기기 전과 같음: 이미지 코사인 0.999999 ~ 1.000001, 텍스트 0.999999, 토크나이저 6/6 |
| 창 찾기 (Win32) | 앞 창 · 점 아래 창: `explorer.exe` · `app://explorer.exe` |
| 앱 `cargo test --release --lib` (lancedb 0.41 · tokenizers 0.22) | 237 통과 |
| lance 8 ↔ 14 호환 | 옛 실행 파일(lancedb 0.31)이 쓴 보관함을 새 실행 파일이 읽고(행 2 · 벡터 검색 0.901) 이어 쓰며(행 3), 그 뒤 옛 실행 파일도 다시 읽음 |
| 실제 E2E (`premium-discount-mcp` → 새 앱 → `mock_server.py`) | 통과(14.8초) |
| src-tauri `cargo check` + 사이드카 | 통과 |
| 잠금 파일 | `app/` · `app/src-tauri/` · `crates/` · raon-zena 모두 tokenizers 0.22.2 하나, candle `rev=ebaf93d` |

**기획서와 다른 점**
- "단일 Cargo.lock" 은 저장소가 둘이라 없습니다. 네 잠금 파일이 같은 판을 고르게 맞췄습니다.
- premium-discount-app 은 여전히 프로세스로 둡니다(Tauri · tokio · 앱 상태). 다만 이제 판 충돌은 링크하지 않는 이유가 아닙니다.
- raon-zena 에는 기획서의 트리와 달리 얇은 접착 crate 가 남습니다.
  - `raon-vision`: 사전 → 앵커 은행.
  - `raon-quant`: Raon 피라미드 계획 · 블록 캐시.
  - `zena-sds`: 다시 내보내기, 경로 유지.
- Relay 의 키 묶기는 SDS 띠(평균 + 2σ) 대신 "서로 가장 가까운 짝 + 바닥 0.80 + 차이 0.03" 입니다. 실측 근거:
  - 같은 이름의 한 · 영 짝: 0.833 · 0.848.
  - 다른 이름끼리: 0.739 ~ 0.771.
  - 키 몇 개의 띠는 0.881 이라 아무것도 묶지 못했습니다.
- Relay 의 포함률 하한(0.5)은 아직 고정값입니다. Relay 는 새로 만들었고 아직 부르는 곳이 없습니다(판정 cron 의 `superseded` 입력에 잇는 것은 다음 일).
- `premium-discount-window` 의 누르기와 끌기 · 제자리 돌아가기 · 점 아래 창 찾기는 raon-zena 오버레이가 아직 쓰지 않습니다. `zena-core::window` 는 스타일 비트 · 알파 · 프레임 간격만 이 crate 로 계산합니다.

## 24. 웹 행동 분석 이관: logis-center analytic · tracker-logis-center (2026-11-18)

logis-center 의 웹 행동 분석(analytic) 파이프라인과 tracker-logis-center(사이트에 심는 수집 스크립트 + Cloudflare Worker)를 이 저장소로 옮겼습니다.
- 수집: www Worker 의 `/analytic/*` 와 수집 스크립트 `www/public/static/js/tracker.js`
- 해석 코어: `crates/premium-discount-analytic`
- 기기 쪽 받기 · 구조화 · 리포트: 앱 `app/src/analytic.rs`

같은 기획서의 raon-zena 쪽 일(대화 맥락 · 코스 · 투트랙 RAG 배선, 언어팩)은 그 저장소의 `docs/MASTER_PLAN_GLOBAL_LOCAL_COURSE_RAG.md` 입니다.

| 원본 | 옮긴 곳 | 한 일 |
| --- | --- | --- |
| tracker-logis-center `tracker/tracker.js` (ethers · marked 묶음 포함 537 KB) | `www/public/static/js/tracker.js` (묶음 없음) | 누름 · 머묾 · 바꿈에 들어옴 · 스크롤 · 제출 · 뒤로 · 떠남을 더함. `outerHTML` 대신 속성 없는 PUG + 셀렉터용 조상 모양을 브라우저에서 만듦. 원본 결함 네 가지를 고침(24-5) |
| tracker-logis-center `workers/src/index.ts` | `www/src/analytic.ts`, `www/migrations/0008_web_analytics.sql` | 사이트마다 쌓고 확인된 주인에게만 돌려줌. 방문자 쿠키 · S3 계정 · IP · 지역은 받지도 남기지도 않음 |
| logis-center `analytic.rs` · `parsing.rs` · `ai_utils.rs` (`strip_pug_attributes`, `pug_line_parts`, `sanitize_llm_input`, HTML → PUG) | `premium-discount-analytic::pug` | 그대로 옮기되 입력값을 내지 않고, 숨은 글을 접지 않음 |
| logis-center `utils/time_guide.rs` | `premium-discount-analytic::time` | `this_week` · `last_week` 를 더함. 날짜는 모델이 아니라 코드가 사용자 시계로 정함 |
| logis-center `model/query_analytic.rs` 의 사전 단계 + `full_bias.json` 의 분석 항목 | `premium-discount-analytic::query`, `data/analytic_dictionary.json` | 사건 5 · 행동 4 · 페이지 10 · 기간 7(+2) · 계절 4 · 인사이트 3. 일치는 premium-discount-nms 도메인 은행 |
| logis-center `runtime.ts` `decodeAnalyticBlob` | `www/src/blob.ts`, `premium-discount-analytic::blob` | BUG-RT-2 · 3 · 4 를 고침. 두 쪽이 `contracts/analytic_blob_v1.json` 을 함께 통과 |
| logis-center `modes/analytic.ts` (받기 · DELTA GUARD), `commerce.ts` (TOMBSTONE GUARD) | `app/src/analytic.rs` | LanceDB `web_events_local` · `web_event_tombstones` |
| (없음) | `premium-discount-analytic::funnel` | Track 1: 웹 체크포인트 · 선택 대조 · 마찰 에피소드 · 퍼널 (premium-discount-nav 규칙) |

### 24-1. 흐름

1. 사이트 주인이 사이트에 두 줄을 넣고 앱에서 사이트를 확인합니다(`cmd_analytic_site {host}`).
   ```html
   <meta name="premium-discount-site" content="<주인 주소>">
   <script src="https://premium.discount/static/js/tracker.js" async></script>
   ```
   - 주인 주소는 QR 로그인 세션이면 `hashId(이메일)` 입니다(logis-center oauth 의 사이트 확인 meta 와 같은 식). 지갑 서명(`x-pd-*`)으로 확인할 수도 있습니다.
   - Worker 가 `https://<host>/` 를 받아 meta 를 봅니다. 한 사이트에 주인이 여럿일 수 있습니다.
2. 방문자의 브라우저가 사건을 모아 `POST /analytic/events` 로 보냅니다.
   - `text/plain` JSON 이라 사전 요청이 없습니다.
   - 5초마다 · 20개마다 보냅니다. 떠날 때는 keepalive 로 4개씩 보냅니다(keepalive 본문 한도 64 KB).
3. Worker 가 모양을 검사하고 다시 다듬어(`sanitizeEvent`) D1 `web_events` 에 넣습니다.
   - `cc = hashId(호스트)`, `session = hashId(cc + sid)`, `data = gzip(JSON)`, `updated_at = 0`(원시).
   - 브라우저가 보낸 `Origin` 의 호스트와 사건 주소의 호스트가 다르면 403 입니다.
4. 앱 `cmd_analytic_sync {host}` 이 사건을 받아 정리합니다.
   - `GET /analytic/events?host&since&after_id` 를 (시각, id) 커서로 빠짐없이 받습니다.
   - BLOB 을 풀어 LanceDB 에 넣습니다.
   - 기기에서 구조화합니다(24-4).
5. 앱 `cmd_analytic_report {host, query, lang}` 이 리포트를 냅니다.
   - 질의 해석 → 기간 → 그 기간의 사건으로 퍼널 · 병목 · 선택 대조를 구합니다.
   - 요약글을 벡터 검색합니다.
   - 사람이 읽는 `lines` 몇 줄을 냅니다. raon-zena 가 `[참고]` 줄로도 쓸 수 있는 모양입니다.

명령은 공용 계약(`premium-discount-schema` `COMMANDS`, 17 → 21)에 들어갔습니다. 함께 등록한 곳은 다음과 같습니다.
- 앱 코어(`dispatch`)
- Tauri 창
- 화면(`ui/commands.ts`)
- 계약 파일(`ui/fixtures/command_contract.json`)

### 24-2. 보내는 것과 보내지 않는 것 (`web-event/1`)

| tracker-logis-center | 여기 |
| --- | --- |
| `action`: 누른 요소의 `outerHTML` | `target`: 속성 없는 PUG(`{들여쓰기}{태그} \| {글}`) + 셀렉터용 조상 모양(태그 · id · class · `role`/`name`/`type`/시험용 속성 · 같은 태그 중 순서, 깊이 5) |
| (없음) | `item`: 누른 요소가 목록 안에 있으면 그 목록 항목(형제와 같은 단위) |
| `relate`: 같은 목록의 형제 `outerHTML` (그림 주소만 지움, 최대 100) | `related`: 같은 모양 (최대 100) |
| 주소 전체(쿼리 포함) | 쿼리 · 문서 안 위치를 뺀 주소(해시 라우팅 `#/…` 만 남김) |
| 쿠키의 `hash` · `token` (S3 에 방문자 계정을 만듦) | 탭마다 새로 만드는 무작위 `sid`. 서버에는 `hashId(cc + sid)` 만 남음 |

- 보내지 않습니다.
  - 입력값: 바꿈 사건은 칸의 종류(`text` · `select` …)만 보냅니다. 비밀번호 칸은 사건이 없습니다.
  - PUG 의 `input` · `textarea` 값: 결제 화면에 미리 채운 이름 · 주소가 값으로 들어 있기 때문입니다.
  - 속성(`href` · `src` · `value` · `style` …), 숨은 층(`position: fixed/absolute`), 스크립트, `data-pd-ignore` 영역.
- 글의 이메일과, 숫자가 네 개 이상 든 낱말은 `#` 로 바꿉니다. 전화 · 카드 · 주문 번호를 막으려는 것이고, 1,000 이상의 가격도 함께 가려집니다. 브라우저 · Worker · Rust 가 같은 규칙으로 한 번씩 거릅니다.
- GPC(`navigator.globalPrivacyControl`) · DNT 를 켠 방문자는 수집하지 않습니다.
- 크기: tracker-logis-center 의 어림식(토큰 어림 × 5 > 7,000)을 그대로 씁니다. 다만 넘치면 형제를 반씩 줄이고, 그래도 크면 버립니다. 원본은 바로 버렸습니다.

### 24-3. 고른 요소 vs 지나친 형제: 기획서와 다른 판정

- 기획서의 규칙: "고른 TARGET 은 Premium, 같은 화면에서 비켜 간 RELATED 는 Discount 로 즉시 집계"하고, 등급 규칙(2/3)을 적용합니다.
- 문제: 목록이 길수록 모든 후보가 Discount 가 됩니다. 시험 `the_plan_rule_marks_a_random_list_discount` 로 확인했습니다.

| 경우 (상품 10개 목록) | 기획서 규칙 | 여기의 규칙 |
| --- | --- | --- |
| 아무 정보 없이 무작위로 2,000번 고름 | 10개 모두 Discount | 결정하지 않음(띠 2σ 를 넘은 것이 하나 이하) |
| 상품 0 을 40 %, 상품 9 는 한 번도 안 고름 | 상품 0 도 Neutral(40 % < 2/3) | 상품 0 Premium, 상품 9 Discount |

- 여기의 규칙은 선택 묶음의 크기를 n 이라 할 때, 아무 정보 없이 고를 확률 1/n 을 기댓값으로 씁니다.
  - 후보마다 노출 · 뽑힘 · 기댓값 Σ1/n · 분산 Σ(1/n)(1−1/n)을 쌓습니다.
  - z 가 ±2 를 넘고 노출이 5 이상일 때만 결정합니다.
  - 기획서의 수(뽑힘 · 지나침)는 비교용으로 남겼습니다(`ChoiceTally::naive_tier`).
- 같은 틀의 카드(상품 목록)는 셀렉터 틀이 같아 요소 해시도 같습니다. 셀렉터는 데이터 자리를 지우기 때문입니다. 그래서 선택 대조의 키는 요소 해시 + PUG 의 글입니다.
- 기획서 4.2 의 쇼핑몰 시나리오를 그대로 시험에 넣었습니다. 세션 40개: 30개는 간편결제로 완료하고, 10개는 쿠폰 칸에서 스크롤을 뒤집고 광클하고 다시 고치다 떠납니다.
  - 퍼널: 상세 → 장바구니 → 결제 → 완료가 40 · 40 · 40 · 30, 결제에서 10 세션이 빠집니다.
  - 병목 1위: '쿠폰 적용' (마찰 discount).
  - 선택 대조: 간편결제 Premium · 쿠폰 Discount (z = ±3.16).
  - 앱 리포트("어제 결제 이탈 원인이 뭐야?")가 기간 · 인사이트(`conversion_insight`) · 퍼널 · 병목을 냅니다.

### 24-4. 질의 해석과 기기 구조화

- **질의**
  - 사전은 logis-center `full_bias.json` 의 분석 항목입니다. 12개 언어의 정확 일치 글이 들어 있습니다.
  - 일치는 premium-discount-nms 의 도메인 은행이 합니다. 띄어쓰기를 무시하고, 한국어 조사를 떼고, 교착어는 접두로 맞춥니다. 일본어 · 중국어처럼 띄어 쓰지 않는 글은 낱말 안에서 찾습니다(`昨日のページビュー` → 어제 · 유입).
  - 같은 갈래에서 둘이 걸리면 더 긴 글이 이깁니다("결제 이탈" > "이탈" > "원인").
- **날짜는 코드가 정합니다.**
  - 해석은 기간 키(`last_week`) · 계절 키(`summer`)만 고르고, 범위는 사용자의 시계로 계산합니다. 시계는 언어에서 정합니다: 시간대 오프셋, 남반구면 계절을 여섯 달 어긋나게, ISO 주(월요일 시작).
  - 계절은 이미 시작한 가장 최근 회차입니다. 11월의 "겨울"은 작년 겨울입니다.
  - 평가 시각 고정은 `PD_EVAL_NOW_MS` 이고, 없으면 logis-center 의 `LOGIS_EVAL_NOW_MS` 를 읽습니다.
- **구조화 (Track 2)**: logis-center 는 이 자리에서 Qwen3.5 2B 에 사건마다 한 번 물어 `action` · `summary` · `relate` 를 받았습니다. 여기서는 모델 없는 요약글을 embeddinggemma(768, 문서 프롬프트)로 임베딩하고 `updated_at` 을 찍습니다.
  - 요약글 모양: `click | cart shop.example.com/order/cart | 간편결제 | not chosen: 쿠폰 적용`
  - 기간 · 페이지 · 선택 대조 · 마찰은 Track 1 이 결정론적으로 내므로, 모델 요약은 그 위의 문장일 뿐입니다.
  - 초안 모델(Qwen3.5-0.8B)은 이 PC 에 가중치가 없어 붙이지 않았습니다.
- **가드**
  - DELTA GUARD: 구조화된 행(`updated_at > 0`)은 서버의 원시 사건으로 덮지 않습니다. LanceDB merge 의 조건(`target.updated_at = 0`)으로도 막습니다.
  - TOMBSTONE GUARD: 지운 사건 id 는 묘비 테이블에 남아 다음 받기에서 다시 들어오지 않습니다.

### 24-5. 원본에서 고친 결함

| 어디 | 결함 | 고친 것 |
| --- | --- | --- |
| tracker-logis-center Worker | `pageId = hashId(cookies.cc + pathname)` 를 `cookies.cc` 를 정하기 전에 계산해 `undefined/…` 로 해싱 | 세션 해시는 cc 를 정한 뒤에 계산 |
| tracker-logis-center Worker | GET 이 `href` 만 바꾸면 누구의 사이트든 1,000건을 돌려줌 | 확인된 주인만, 커서로 |
| tracker-logis-center `tracker.js` | 광클을 요소와 상관없이 셈(카드를 누르고 쿠폰을 두 번 누르면 광클) | 같은 요소를 700 ms 안에 세 번 |
| tracker-logis-center `tracker.js` | 바꿈 디바운스가 하나라 600 ms 안에 다른 칸을 바꾸면 앞 칸의 사건이 사라짐 | 칸마다 디바운스 |
| tracker-logis-center `tracker.js` | 카드 안의 링크를 누르면 링크와 카드의 모양이 달라 형제를 하나도 못 보냄 | `item` = 목록 항목, 형제는 항목의 형제 |
| tracker-logis-center `tracker.js` | 페이지 배경(html)에 머물면 페이지 전체가 실려 나감(크기 상한에 걸려야 버려짐) | html · body 대상은 보내지 않음 |
| logis-center `runtime.ts` | BUG-RT-2(50 바이트 이하 base64 gzip 을 풀지 않음) · BUG-RT-3(Uint8Array 조각을 밑의 버퍼 전체로 읽음) · BUG-RT-4(`"42"` · `"null"` 을 그대로 돌려줌) | 셋 다. 계약 사례 19개 |
| logis-center PUG | 한 줄로 접을 때 숨은 층 · 고르지 않은 option 의 글까지 모음, 버려질 base64 아이콘 하나가 링크 글의 접기를 막음, 조각을 문서로 파싱해 `html` 부터 냄(B23) | 내보내지 않는 요소는 접기 · 데이터 판정에서도 뺌, 조각의 뿌리부터 |

### 24-6. 기획서와 다른 점

- 기획서의 이름이 실제 코드와 다릅니다.
  - `structure_analytic_event` 는 없고 `run_analytic_structuring` 입니다.
  - `strip_pug_attributes` 는 `ai_utils.rs` 가 아니라 `analytic.rs` 에 있습니다.
  - `sales_insight` 는 없고 `conversion_insight` 입니다.
  - logis-center 의 `insight` · `metrics` · `operators` 는 분석 파이프라인이 쓰지 않습니다(커머스 질의용). 여기서는 `insight` 만 옮겼습니다. 영어 문장뿐이라 한국어 · 일본어 · 중국어 정확 일치 글을 더했습니다(`pd_exact_match`).
- `time_guide.rs` 에는 "이번 주"가 없습니다. `this_week` · `last_week` 를 더했습니다.
- 기획서의 "TARGET Premium / RELATED Discount" 대신 기댓값 대비 z 로 결정합니다(24-3).
- LLM 구조화(Qwen3.5 2B)는 옮기지 않았습니다(24-4).
- README 10-3 의 레거시 발자국(`auth.js` · `content.js`)은 logis 콘솔 화면에서 온 코드라 수집 경로에 잇지 않았습니다. 수집은 새 Worker 경로뿐입니다.
- 사이트 확인은 logis-center 의 `api.oauth.network` client_id/secret 대신 meta + 세션 · 지갑입니다.

### 24-7. 배포와 남은 위험

1. 마이그레이션: `npx wrangler d1 migrations apply DB --remote` (`0008_web_analytics.sql`).
2. `wrangler.json` 에 요청 제한 `ANALYTIC_WRITE_LIMITER`(namespace 1003, IP 마다 분당 120)를 더했습니다. 없으면 평가 쓰기 제한을 함께 씁니다.
3. 수집 스크립트는 `public/static/js/tracker.js` 로 함께 배포됩니다.

남은 위험은 다음과 같습니다.
- 누구나 남의 사이트 이름으로 가짜 사건을 보낼 수 있습니다. 브라우저는 Origin 으로 막지만 직접 보낸 요청은 막지 못하고, 요청 제한만 받습니다. 주인만 읽으므로 새는 것은 없지만 리포트가 오염될 수 있습니다.
- 지갑 서명은 경로만 덮고 쿼리를 덮지 않습니다(평가 · 타임트라이얼과 같음). 5분 안에 서명을 가로챈 쪽이 같은 주인의 다른 사이트를 읽을 수 있습니다.
- 보관 기한 cron 은 없습니다. 주인이 `DELETE /analytic/events?host&before` 로 지웁니다.

### 24-8. 검증

| 묶음 | 결과 |
| --- | --- |
| `cd crates && cargo test -p premium-discount-analytic` | 33 통과: 단위 24(시간 6 · BLOB 4 · 사건 3 · PUG 5 · 질의 6), 계약 2(BLOB 19 · PUG 13 사례), 퍼널 7(무작위 목록 · 우세한 선택 · 카드 안 링크 · 4.2 시나리오) |
| www (`node --test www/tests/*.test.ts`) | 94 통과. 분석 10(다듬기 · 수집 · gzip · 다른 사이트 403 · 요청 제한 · meta 확인 · 세션 주인 · 커서 · BLOB 계약 · BUG-RT), 수집 스크립트 6(linkedom: PUG 계약 13 사례 · 조상 모양 · 목록 형제 · 거르기 · 크기) |
| 실제 Chrome (headless, `playwright-core` + 설치된 Chrome) | 사건 13개(들어옴 · 카드 누름 · 쿠폰 3번 · 머묾 · 바꿈 3 · 스크롤 · 제출 · 떠남). 모두 `sanitizeEvent` 통과. 입력값 · 쿼리 · 비밀번호 · `data-pd-ignore` · 숨은 층 글 없음. 광클은 세 번째 쿠폰만, 떠남의 `dirty` 1. 원본 결함 네 가지(24-5)를 이 확인에서 찾음 |
| 앱 `cargo test --release --lib` | 240 통과(기존 237 + 분석 3). 저장소 2(받기 · DELTA · TOMBSTONE · 구조화 · 검색, 어제 기간 리포트: 퍼널 40/40/40/30 · 병목 '쿠폰 적용' · 간편결제 premium), 명령 계층 1(사이트 · 커서대로 두 번 받기 · 리포트 · 지우기 · 저장소 없으면 not_ready). UI 계약 시험이 명령 21개를 모두 부름 |
| `cd crates && cargo test --release --workspace` | 전부 통과, 경고 없음(새 analytic 33 포함, 실제 모델 granite · Stanza 시험 포함) |
| 화면 (`app/ui`) · cron | 71 · 41 통과 |
| `premium-discount-schema` | 8 통과 (명령 21개 = 계약 파일) |
| src-tauri `cargo check` | 통과, 경고 없음 (분석 명령 4개 등록) |

## 25. 신구 대체 연결 · 사이드카 검증 · 웹 분석과 내비게이션 (2026-11-18)

### 25-1. 신구 대체(`Relation::Supersedes`) → PD-Verdict `superseded`

같은 장소(cc) · 대상(to)의 새 평가가 옛 평가의 주장에서 수치 · 단위 · 형용사만 바꿨으면("피해 20% → 15%", "배송비 3,000원 → 무료") 옛 평가는
근거와 상관없이 discount(grade −2, 이유 `superseded`)입니다. 판정 규칙(`premium-discount-verdict` · `cron/src/verdict.ts`)에는 입력이 이미
있었고, 이번에 그 입력을 채우는 길을 이었습니다.

```text
Raon Zena 판정 보조 (Stanza · granite)                premium-discount-app / -mcp                      www · cron
premium_discount_relay::supersessions ──pd_supersede──▶ cmd_supersede ──POST /evaluations/supersedes──▶ eval_supersedes (0009)
  같은 cc · to 묶음, 시간순, 대체 쌍                    판정 캐시를 곧바로 discount −2                 → verdict_dirty → cron 판정 superseded
```

- **relay**: `supersessions(entries, embedder, cfg)` — 같은 cc · to 끼리 시간순(같으면 id 순)으로 놓고 평가마다 더 늦은 평가와 견줘, 한 옛 평가에
  그것을 대체하는 가장 이른 새 평가 하나를 냅니다. 주장 규칙 세 가지를 고쳤습니다(실제 Stanza ko 로 "배송비 3,000원 → 무료" 가 Duplicate 로
  나왔음): 숫자로 시작하는 수+단위(`3,000원`, `3,000원이다`)는 식별자 키가 아니라 값, 한국어 계사 서술어(`무료+이+다`)의 명사는 값, 주제어
  (`dislocated`, 은/는)는 핵심어. 실제 모델(granite · Stanza ko)로 두 사례 모두 Supersedes.
- **서버**: `POST /evaluations/supersedes {request_id, old, new}`. 비교는 Worker 에서 할 수 없어(모델 없음) 검증할 수 있는 것만 봅니다: 둘 다 저장이
  끝난 평가, 같은 cc · to, new 가 더 늦음(400), 없는 평가(404), 확인된 로그인(게스트 401), 보고자는 두 평가 중 하나의 작성자(403 — 남의 평가를
  마음대로 내리지 못하게). 같은 request_id 는 처음 답(200), 다른 쌍이면 409. 받으면 옛 평가를 `verdict_dirty` 에 표시하고, cron 은 판정 행을 읽을 때
  `EXISTS (eval_supersedes …)` 를 함께 읽어 `judge(…, superseded)` 로 넘깁니다. 표가 하나도 없던 평가도 보고만으로 판정 행이 생겨 discount 가 됩니다.
- **앱 · MCP**: 명령 `cmd_supersede {old, new}`(계약 22개째, Tauri 셸 · 화면 `supersede` 포함)와 도구 `pd_supersede`. request_id 는
  `sup-` + sha256(old|new) 앞 48자라 다시 보내도 서버에는 한 번입니다. 서버가 받으면 판정 캐시를 곧바로 discount −2 로 바꾸고(다음 서버 판정도
  같음), 연결이 없으면 `offline` 오류라 부른 쪽이 다음 동기화 때 다시 보냅니다. 서버의 4xx(408 · 429 빼고)는 `rejected`.
- 바로잡은 것: `EvalBackend` 의 `Arc<T>` 위임에 새 메서드가 빠지면 기본 구현(지원 안 함)이 불립니다. 앱 시험이 이것을 잡았습니다.

### 25-2. 사이드카(`externalBin`) 검증

`sidecars.mjs` 를 시험할 수 있게 나눴습니다(`stage` · `stagedName` · `hostTriple`). `sidecars.test.mjs` 가 고정하는 약속: 빌드한 두 실행 파일을
`binaries/<이름>-<삼중항>[.exe]` 로 옮김(Windows · 리눅스 · 교차 빌드 자리), 실행 파일이 없으면 그 경로를 알리고 멈춤, `tauri.sidecar.conf.json`
의 `externalBin` 은 두 사이드카이고 빌드 전에 `sidecars.mjs` 를 부름, 기본 설정에는 사이드카가 없음, 설치 폴더(`productName` = `premium.discount`)와
삼중항을 뗀 이름이 `find_pd_app` · `find_mcp_server` 가 찾는 것과 같음. 셸 `cargo check` 는 기본 설정, 사이드카 설정(`TAURI_CONFIG`)으로 모두 통과하고,
`binaries/` 가 없으면 사이드카 설정은 "resource path … doesn't exist" 로 멈춥니다(그래서 기본 설정에서 뺀 것).

### 25-3. 웹 분석과 접근성 내비게이션

BLOB 코덱의 세 결함(50 바이트 이하 base64 gzip · `subarray` 오프셋 · `"42"`/`"null"` 원시값)과 DELTA GUARD 는 이미 시험(계약 19 사례 · www ·
앱)으로 고정돼 있었습니다. 빠져 있던 것은 사건 → 내비게이션 코스의 끝에서 끝 시험이라 `crates/premium-discount-analytic/tests/funnel.rs` 에
더했습니다: 고른 요소(TARGET)의 체크포인트가 nav 요소 해시(`page_hash` + `selector_template`)이고, 간편결제로 끝낸 30 · 쿠폰으로 늦게 끝낸 10 ·
쿠폰 칸에서 떠난 10 세션에서 쿠폰 칸이 병목(discount)이며, 완주한 런들의 고스트 코스를 서버 규칙(`route_marks`)으로 견주면 간편결제 체크포인트가
30 premium · 10 discount 라 결제 페이지에 막 들어온 방문자에게 nav `guide` 가 다음 단계로 간편결제를 premium 으로 권합니다.

### 25-4. 검증

| 묶음 | 결과 |
| --- | --- |
| `cd crates && cargo test --workspace` | 279 통과 · 0 실패 · 3 무시 (relay 단위 4 · 실제 모델 3 · 대체 → 판정 2, MCP 13, 분석 퍼널 8 포함) |
| www · cron (`node --test www/tests/*.test.ts cron/test/*.test.ts`) | 139 통과 (대체 보고 4) |
| 앱 `cargo test --lib` · 통합 | 241 통과 (`cmd_supersede` 1, UI 계약 22개 명령) |
| 화면 `app/ui` 계약 · 사이드카 `node --test app/src-tauri/sidecars.test.mjs` | 4 · 4 통과 |
| src-tauri `cargo check` | 기본 설정 · 사이드카 설정 통과 |

## 26. 폴더 재배치: logis-center 와 같은 배치 (2026-11-19)

자리와 연결만 바꿨습니다. 동작 · 계약 값 · 의존성 판(아래 26-3 의 통일 26개 빼고)은 그대로이고, TypeScript Worker 를 Rust 로 옮기는 일은 다음 단계입니다.
파일은 `git mv` 로 옮겼고(첫 커밋은 옮기기만, 내용 그대로), 연결은 둘째 커밋에서 고쳤습니다.

### 26-1. 옛 경로 → 새 경로

| 옛 경로 | 새 경로 | 비고 |
| --- | --- | --- |
| `crates/Cargo.toml` · `app/Cargo.toml` · `app/src-tauri/Cargo.toml` (작업 공간 셋, `Cargo.lock` 셋) | 루트 `Cargo.toml` · `Cargo.lock` 하나 | 멤버: `crates/*` 19개 + `apps/core` + `apps/desktop` |
| `app/src-tauri/.cargo/config.toml` (`target-dir = "../target"`) | 루트 `.cargo/config.toml` (`target-dir = "target"`) | 산출물은 루트 `target/` 하나 |
| `app/` (`src` · `tests` · `examples` · `README.md`) | `apps/core` | `premium-discount-app` |
| `app/src-tauri` | `apps/desktop` | `premium-discount-desktop`, `gen/android` 포함 |
| `app/ui` | `web/desktop` | |
| `www/public` | `web/www` | www Worker 의 `assets.directory` |
| `www` | `workers/www` | `src` · `tests` · `package.json` · `wrangler*.json` · `tsconfig.json` · `worker-configuration.d.ts` |
| `cron` | `workers/cron` | |
| `www/migrations` | `workers/migrations` | www 의 `DB` 바인딩 `migrations_dir: "../migrations"`. cron 의 wrangler 는 마이그레이션을 쓰지 않아 그대로 |
| `crates/` · `contracts/` · `test-support/` | 그대로 | |

옛 빌드 폴더 `app/target`(14 GB) · `crates/target`(12 GB)은 지우지 않았습니다(무시되는 폴더). 새 빌드는 루트 `target/` 에 처음부터 생깁니다.

### 26-2. 연결을 고친 곳

- **Cargo**: `[workspace.dependencies]` 의 내부 crate 경로에 `crates/` 를 붙이고 `premium-discount-app = { path = "apps/core" }` 를 더했습니다. 코어의 내부 crate
  의존 5개와 셸의 코어 의존은 `workspace = true` 로 바꿨고(같은 경로), 바깥 의존성의 판 · 기능은 각 매니페스트에 그대로 둡니다. 멤버는 글롭(`crates/*`) 대신
  하나씩 적습니다: `crates/target` 처럼 `Cargo.toml` 없는 폴더가 있으면 글롭이 작업 공간을 깨뜨립니다.
- **`[patch.crates-io]`**: 세 매니페스트의 candle 포크 줄(`ebaf93d`, candle-core · candle-nn)이 같은 값이라 루트 하나로 합쳤습니다.
- **프로필**: 키가 겹치지 않아 합집합입니다. `[profile.dev] debug = 0` 과 `gemm-common` · `gemm-f16` opt 3(옛 app · src-tauri)이 이제 crates 의 디버그 빌드에도,
  `candle-core` · `candle-nn` · `gemm` · `k256` · `elliptic-curve` · `crypto-bigint` · `tiny-keccak` opt 3(옛 crates)이 이제 앱의 디버그 빌드에도 걸립니다.
- **Tauri**: `frontendDist` = `../../web/desktop/dist`. Tauri CLI 는 앞 명령(`beforeBuildCommand`)을 화면 폴더에서 돌리고, `apps/desktop` 아래에 `package.json` 이
  없으면 그 폴더는 `tauri.conf.json` 폴더의 부모(`apps/`)라서 명령을 `npm --prefix ../web/desktop …` · `node desktop/sidecars.mjs` 로 적었습니다(전에는 `app/` 기준
  `npm --prefix ui …` · `node src-tauri/sidecars.mjs`). `cargo tauri …` 는 전처럼 셸 폴더(`apps/desktop`)에서 부릅니다.
- **사이드카(`sidecars.mjs`)**: 두 사이드카를 루트 `Cargo.toml` 로 빌드하고 루트 `target/[삼중항/]release` 에서 찾습니다(전에는 `app/target` · `crates/target`).
  `sidecars.test.mjs` 는 가짜 빌드 자리를 작업 공간 하나로 바꾸고, 넘긴 매니페스트가 루트 `Cargo.toml` 인지도 봅니다.
- **wrangler**: `assets.directory` = `../../web/www/`, `DB` 의 `migrations_dir` = `../migrations` (`wrangler.json` · `wrangler.test.json`).
- **컴파일 시점 경로**: 코어의 `include_str!`(명령 계약 · 캐시 행 픽스처 → `../../../web/desktop/fixtures/…`), `premium-discount-schema` 계약 시험
  (`../../../web/desktop/fixtures/command_contract.json`).
- **Node 시험 · 스크립트**: `workers/{www,cron}` 시험의 `test-support` · `contracts` 경로(한 단계 더 위), `tracker.test.ts` 의 수집 스크립트(`web/www`),
  `test-support/fakes.ts` 의 마이그레이션(`workers/migrations`), `gen_stage_cases.mjs` 의 `stage.ts`, 계약 생성기의 `workers/www/node_modules`,
  화면 개발 브리지의 기본 실행 파일(`../../target/release`), E2E 스크립트의 `dev-server.mjs` · `jsQR` · `mock_server.py` 의 `contracts` 경로.
- **문서 · 주석**: 저장소 루트 기준 경로(`app/…` · `www/…` · `cron/…`)를 새 경로로 바꿨습니다. 이 README 의 2 ~ 25절과 `apps/core/tests/SCENARIOS.md` 의 옛 기록은 두었습니다.
- **바꾸지 않은 것**: `contracts/*.json`(raon-zena 가 `verdict_v1.json` 의 해시를 봅니다. `analytic_pug_v1.json` 의 note 는 옛 경로 `www/public/…` 그대로),
  이미 적용된 `workers/migrations/*.sql` 의 주석, `web/desktop/fixtures/*.json`(Rust · 화면 시험이 함께 읽는 픽스처).

### 26-3. Cargo.lock 합치기

옛 세 잠금 파일의 항목을 합쳐 씨앗으로 두고 `cargo metadata` 로 정리했습니다. 새로 고른 판은 없고(모든 항목이 옛 잠금 셋 중 하나에 있음), 한 작업 공간에는
semver 호환 판이 하나뿐이라 26개 이름이 한 판으로 모였습니다. 대부분 높은 쪽(옛 crates 쪽)이고, `crypto-common 0.1.7` 이 `generic-array =0.14.7` 을 고정해
`generic-array` 만 낮은 쪽입니다.

| 이름 | 옛 crates | 옛 app · src-tauri | 지금 |
| --- | --- | --- | --- |
| candle-kernels | 0.11.0 | 0.11.1 | 0.11.1 |
| libloading (0.9) | 0.9.0 | app 0.9.0 · src-tauri 0.9.1 | 0.9.1 |
| crypto-common | 0.1.6 | 0.1.7 | 0.1.7 |
| generic-array | 0.14.9 | 0.14.7 | 0.14.7 |
| cc · crc32fast · equivalent · errno · find-msvc-tools · flate2 · itoa · libc · log · memmap2 · rustls · rustls-pki-types · rustls-webpki · smallvec · syn(3) · typed-path · unicode-ident · unicode-width(0.2) · utf8_iter · webpki-roots(1) · zerocopy · zerocopy-derive | 높은 판 | 낮은 판 (예: cc 1.8.0, syn 3.0.7) | 옛 crates 의 판 (cc 1.11.2, syn 3.0.10 …) |

그대로인 것: candle-core · candle-nn(포크 `ebaf93d`), candle-metal-kernels 0.11.1, lancedb 0.41.0, lance 14.0.2, arrow 58.4.1, datafusion 54.1.0, tauri 2.12.5,
wry 0.57.2, tokenizers 0.22.2, safetensors 0.8.0 · 0.4.5, half 2.7.1, tokio 1.53.2, reqwest 0.12.28 · 0.13.5, onnxruntime · onnxruntime-sys 0.0.14(경로).

### 26-4. 검증

모두 옮기기 전(옛 배치)과 뒤(새 배치)에 같은 PC 에서 돌려 견줬습니다.

| 묶음 | 옮기기 전 | 옮긴 뒤 |
| --- | --- | --- |
| www (`node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/www/tests/*.test.ts`) | 98 통과 | 98 통과 (시험 이름 같음) |
| cron (같은 명령, `workers/cron/test/*.test.ts`) | 41 통과 | 41 통과 |
| 화면 (`cd web/desktop && npm test`) | 71 통과 | 71 통과 |
| 사이드카 (`node --test apps/desktop/sidecars.test.mjs`) | 4 통과 | 4 통과 |
| 라이브러리 crate (`cargo test --workspace --exclude premium-discount-app --exclude premium-discount-desktop -j 2`, 전에는 `cd crates && cargo test --workspace -j 2`) | 279 통과 · 3 무시 (시험 실행 파일 49) | 279 통과 · 3 무시, 시험마다 결과 같음 |
| `cargo metadata` | — | 성공, 멤버 21 |
| `cargo check --workspace --all-targets --keep-going -j 2` | — | 실패 2건, 둘 다 옮기기와 무관한 기존 결함: `onnxruntime-sys` 의 lib test(`[lib] test = false` 인데 `--all-targets` 가 강제로 묶음, bindgen 레이아웃 시험의 `deref_nullptr`) · `apps/core/examples/quant_probe.rs`(`LoadOptions` 에 `tower_bits` 가 빠짐, 파일은 내용 그대로 옮김). 나머지 대상은 모두 통과 |
| `cargo check -p premium-discount-desktop --all-targets -j 2` | — | 통과 (`frontendDist` 확인). `TAURI_CONFIG` 로 사이드카 설정을 넣어도 통과하고, tauri-build 가 `apps/desktop/binaries/*` 를 루트 `target/debug` 로 복사 |
| `tsc` (workers/www · workers/cron) | www 96 오류(레거시 `index.ts` 등) · cron 0 | 같음 (경로만 다름) |
| `wrangler deploy --dry-run` (www · www 시험 설정 · cron) | 정적 파일 15개, 묶음 1043.27 KiB | 정적 파일 15개(`web/www`), 묶인 `index.js` 세 개 모두 바이트 같음 |
| `wrangler d1 migrations list DB --local` | 0001 ~ 0009 | 0001 ~ 0009 (`workers/migrations`) |
| 생성기 (`gen_wallet_v1.mjs` · `gen_analytic_blob_v1.mjs` · `gen_stage_cases.mjs`) | — | 계약 파일 그대로, `stage_cases.json` 은 note 의 경로 한 줄만 다름 |

앱 코어 단위 시험(`cargo test -p premium-discount-app --lib`)은 이번에 돌리지 않았습니다. 디스크 여유가 5 GB 이고 앱 디버그 시험 빌드는 7 GB 남짓이라서입니다
(옛 `app/target` 14 GB · `crates/target` 12 GB 는 지우지 않음). 코어의 lib · 시험 · 예제(quant_probe 빼고)는 위 `cargo check` 로 컴파일을 확인했습니다.

## 27. cron Worker 를 Rust(workers-rs)로 (2026-11-19)

`workers/cron` 을 workers-rs 0.8.7 로 다시 썼습니다(logis-center 와 같은 방식). `workers/` 는 자기 Cargo 작업 공간(wasm32, 루트의 `exclude`)이고,
공용 층 `workers/common`(premium-discount-worker-common)은 logis-center `workers/common`(370d67a3)에서 옮겨 와 이제 이 저장소가 주인입니다.
판정은 `premium-discount-verdict` crate 를 그대로 씁니다(초월 함수 고리 `Math` · `evidence_in` · `judge_in` 를 더해 cron 은 JS 의 `Math` 를 넘김).
원본 TS(0538044)와 Miniflare 차분 시험(`scripts/workers-diff`, `node cron.mjs`)으로 47개 시나리오가 같고, 원본 버그 둘
(기다리지 않은 `return reject` · 프로토타입을 거치는 가중치 조회)을 고쳤습니다. www 는 아직 TypeScript 입니다.
구조 · 빌드 · 시험대 · 고친 버그 · 알려진 차이는 [`docs/workers-rs.md`](docs/workers-rs.md).
