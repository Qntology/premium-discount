# www(Worker) 테스트 시나리오 (소스 로직 기준)

> 이 문서의 대부분은 **실제로 실행하지 않았습니다.** 소스를 읽고 각 시나리오의 기대 결과를 코드 기준으로 적었습니다.
> `자동:`으로 표시한 행(2026-11-14 추가)은 `www/tests/*.test.ts`가 실행합니다. Node 내장 러너를 쓰고, D1은 실제 SQLite(`node:sqlite`)에 `migrations/0002~0005`를 적용한 가짜입니다.
> `premium-discount` 폴더에서: `node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts`

## 읽는 법

- **상태**
  - `신규`: 이번에 정의한 시나리오입니다. 자동화가 필요합니다.
  - `실측`: 이전 세션에서 로컬 또는 원격으로 실제 확인한 결과입니다. 이번 문서에서는 다시 실행하지 않았습니다.
  - `갭`: 현재 구현이 없거나 알려진 문제입니다.
- **근거**: `evaluations.ts:186` 같은 파일과 줄 번호입니다.
- 대상 파일: `src/index.ts`(진입점·레거시), `src/evaluations.ts`(저장·검색), `src/graph.ts`(그래프), `src/auth.ts`(세션), `src/projection.ts`(작성자 키), `migrations/*.sql`(D1), `wrangler.json`·`wrangler.test.json`

---

## R. 요청 라우팅

| ID | 상황 | 요청 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| R-01 | 저장 경로 | `POST /evaluations` | 저장 처리기가 응답 (레거시보다 먼저) | index.ts:373, evaluations.ts:297 | 신규 |
| R-02 | 허용되지 않은 메서드 | `GET /evaluations` | 405 JSON | evaluations.ts:308-310 | 신규 |
| R-03 | CORS 사전 요청 | `OPTIONS /evaluations`, Origin `tauri://localhost` | 204, `Allow-Origin` 설정, `Allow-Headers`에 `x-oauth-hash`·`x-oauth-token`, `Allow-Methods`에 GET(status) | evaluations.ts `corsHeaders` | 자동:evaluations.test.ts |
| R-04 | CORS 허용되지 않은 출처 | Origin `https://evil.example` | 204, `Allow-Origin` 헤더 없음 | evaluations.ts `corsHeaders` | 자동:evaluations.test.ts |
| R-05 | 공개 검색 경로 | `POST /public/evaluations/search` | public 범위로 처리 | evaluations.ts:299-316 | 신규 |
| R-06 | 그래프 API | `GET /graph?cc=example.com` | 200 JSON | graph.ts:145-165 | 신규 |
| R-07 | 그래프에 POST | `POST /graph` | 405 JSON | graph.ts:148 | 신규 |
| R-08 | 그래프 뷰 페이지 | `GET /graph/view` | HTML 반환, d3는 cdnjs에서 로드 | graph.ts:150-152, :202 | 신규 |
| R-09 | 다른 경로 | `GET /evaluations/xyz` | 저장 처리기는 `null`을 돌려주고 레거시로 넘어감 | evaluations.ts:301 | 신규 |
| R-10 | 정적 파일 | `GET /favicon.ico` | 정적 자산이 먼저 응답 (Worker까지 오지 않음) | wrangler.json `assets` | 신규 (배포 후 확인) |

## V. 저장 입력 검증 (`saveEvaluation`, evaluations.ts:186-241)

| ID | 상황 | 본문 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| V-01 | JSON이 아님 | 잘못된 문자열 | 400 "invalid JSON body" | evaluations.ts:52-58 | 신규 |
| V-02 | 객체가 아님 | `[]` 또는 `"x"` | 400 "body must be an object" | evaluations.ts:59-61 | 신규 |
| V-03 | cc 없음 | cc 키 누락 | 400 "cc is required" | evaluations.ts:90 | 신규 |
| V-04 | cc 형식 오류 | `localhost`, `a..b` | 400 "registrable domain" | evaluations.ts:89-96 | 신규 |
| V-05 | cc 65자 | 긴 도메인 | 400 | evaluations.ts:12, :92 | 신규 |
| V-06 | cc 대문자 | `Example.COM` | 소문자로 저장 | evaluations.ts:91 | 신규 |
| V-07 | vector 길이 767·769 | 길이 불일치 | 400 "vector must be an array of length 768" | evaluations.ts:107-109 | 신규 |
| V-08 | vector에 NaN·문자열 | 원소 타입 오류 | 400 "only finite numbers" | evaluations.ts:112-114 | 신규 |
| V-09 | norm 1.02 | 허용 오차(0.01) 초과 | 400 "L2-normalized" | evaluations.ts:117-119 | 신규 |
| V-10 | norm 1.005 | 허용 오차 이내 | 통과 (경계) | evaluations.ts:117 | 신규 |
| V-11 | request_id 15자 또는 65자 | 길이 위반 | 400 "request_id must be 16-64 characters" | evaluations.ts:16, :143-145 | 신규 |
| V-12 | request_id에 허용되지 않은 문자 | `"bad id!"` | 400 | evaluations.ts:16 | 신규 |
| V-13 | request_id 없음 | 키 생략 | 저장됨. 멱등 보장 없음 | evaluations.ts:142 | 신규 |
| V-14 | from이 잘못된 주소 | `"0x12"` | 400 "from must be a valid address" | evaluations.ts:68-69 | 신규 |
| V-15 | from이 제로 주소 | `0x000…0` | 게스트 주소로 대체 (GA 시나리오 참고) | evaluations.ts:72, :192 | 신규 |
| V-16 | from 대문자 | `0xABC…` | 소문자로 저장 | evaluations.ts:71 | 신규 |
| V-17 | to가 잘못된 주소 | `"x"` | 400 "to must be a valid address" | evaluations.ts:193 | 신규 |
| V-18 | to가 제로 주소 | `0x000…0` | null로 저장, 메타데이터에서 생략 | evaluations.ts:72, :165-170 | 신규 |
| V-19 | ref가 UUID 아님 | `"abc"` | 400 "ref must be a UUID" | evaluations.ts:98-103, :195 | 신규 |
| V-20 | ref 없음 (최상위) | ref 키 생략 | ref null, root = 자기 id | evaluations.ts:195, :214-215 | 신규 |
| V-21 | 성공 응답 형식 | 정상 저장 | 201, `{id, root, ref, from}` | evaluations.ts:240 | 신규 |
| V-22 | 본문 누락 필드가 여러 개 | cc 없음 + vector 짧음 | 먼저 검사되는 필드(cc)의 에러 반환 | evaluations.ts:188-195 | 신규 |

## GA. 게스트 주소 (`guestAddress`, evaluations.ts:77-87)

| ID | 상황 | 전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| GA-01 | IP 헤더 없음 | `cf-connecting-ip`, `X-Real-IP` 모두 없음, from도 없음 | 500 "internal error", 저장 없음 (로그: client ip not available) | evaluations.ts:81-84, :317-321 | 신규 |
| GA-02 | GUEST_SALT 미설정 | 시크릿 없음 | 500 (README 9-2에 명시) | evaluations.ts:78-80 | 신규 |
| GA-03 | 같은 IP | 두 번 요청 | 같은 게스트 주소 (결정적) | evaluations.ts:85-86 | 신규 |
| GA-04 | 다른 IP | 두 번 요청 | 다른 게스트 주소 | evaluations.ts:85 | 신규 |
| GA-05 | from을 직접 준 경우 | IP 헤더 없음 | from 값을 쓰므로 IP 없이도 성공 | evaluations.ts:192 | 신규 |
| GA-06 | IP가 바뀜 (모바일 망 전환) | 같은 사용자 | 다른 게스트 주소로 저장됨 (설계상 한계) | evaluations.ts:85 | 갭 (알려진 문제) |
| GA-07 | GUEST_SALT를 교체함 | 운영 중 시크릿 교체 | 이후 모든 게스트 주소가 바뀜. 기존 행은 그대로 | evaluations.ts:85 | 신규 (운영 주의) |

## ID. 멱등 재전송 (`findByRequestId`, `replaySave`, evaluations.ts:158-183, :198-201, :223-227)

| ID | 상황 | 전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| ID-01 | 같은 request_id, 같은 cc·ref | 첫 요청 성공 | 200, 같은 id, D1 행은 1개 | evaluations.ts:198-201, :174-183 | 신규 (README 10-10 통합 테스트 기록은 저장소에 없음) |
| ID-02 | 같은 request_id, cc 다름 | — | 409 "already used for a different evaluation" | evaluations.ts:175-177 | 신규 |
| ID-03 | 같은 request_id, ref 다름 | — | 409 | evaluations.ts:175 | 신규 |
| ID-04 | 재전송 시 벡터가 없음 | D1 행은 있고 Vectorize에 벡터 없음 | 이번 요청의 벡터로 채우고 200 | evaluations.ts:178-181 | 실측 (recovery 시나리오, 384·768 모두 통과) |
| ID-05 | 재전송 시 벡터가 있음 | 정상 상태 | upsert 없이 200 | evaluations.ts:178-181 | 신규 |
| ID-06 | 벡터 조회가 쓰기 직후 비어 있음 | getByIds 지연 | 불필요한 upsert 한 번 (값은 같아서 무해) | evaluations.ts:178 | 실측 (쓰기 직후 getByIds 지연 1회 관찰) |
| ID-07 | 동시에 같은 request_id가 두 번 옴 | INSERT 중 UNIQUE 충돌 | 두 번째 요청이 기존 행을 찾아 replay | evaluations.ts:218-228 | 신규 |
| ID-08 | 동시 중복 + 첫 요청의 벡터 실패·롤백 | 첫 요청이 DELETE까지 수행 | 두 번째가 받은 id의 행은 사라지고 벡터만 남을 수 있음 | evaluations.ts:178-181, :231-237 | 갭 (알려진 문제, 유령 벡터) |
| ID-09 | 재전송 본문의 벡터가 원래와 다름 | 같은 request_id, 다른 벡터 | 기존 벡터를 유지하고 새 벡터는 무시 (벡터가 없을 때만 씀) | evaluations.ts:180 | 신규 (설계 확인 필요) |

## RB. 저장 롤백 (evaluations.ts:218-240)

| ID | 상황 | 전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| RB-01 | D1 INSERT 실패 (unique 이외) | DB 오류 | 500, 벡터 기록 안 함 | evaluations.ts:218-228 | 신규 |
| RB-02 | Vectorize upsert 실패 | 인덱스 오류 | D1 DELETE 후 500, 행 0개 | evaluations.ts:231-237 | 신규 |
| RB-03 | upsert 실패 + DELETE도 실패 | D1 일시 장애 | 500, D1에 벡터 없는 고아 행이 남음. 같은 request_id로 재시도해야 복구 | evaluations.ts:235 | 갭 (재시도 의존) |
| RB-04 | upsert가 실제로는 반영됐는데 에러 반환 | 응답 유실 등 | DELETE로 행은 사라지고 벡터는 남음. 클라이언트 재시도는 새 id로 저장되어 옛 벡터가 유령으로 남음 | evaluations.ts:231-237 | 갭 (확인 필요) |
| RB-05 | 부모 확인 후 부모가 삭제됨 | 경쟁 상태 | 답글 고아. 현재 삭제 경로가 없어 수동 삭제 때만 발생 | evaluations.ts:203-212 | 갭 |

## PR. 답글 규칙 (evaluations.ts:203-212)

| ID | 상황 | 본문 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| PR-01 | 부모가 없음 | ref가 존재하지 않는 UUID | 400 "parent evaluation not found" | evaluations.ts:206-209 | 신규 |
| PR-02 | 부모와 cc가 다름 | 다른 도메인 | 400 "reply must use the same cc as its parent" | evaluations.ts:210 | 신규 |
| PR-03 | 답글 root | 부모 root = R | 답글 root = R | evaluations.ts:211, :214-215 | 신규 |
| PR-04 | 답글의 답글 | 답글을 부모로 | root는 여전히 최상위 R, ref는 직계 부모 | evaluations.ts:211 | 신규 |
| PR-05 | 부모 id가 대문자 | `ref`를 대문자 UUID로 | 소문자로 변환해 조회 | evaluations.ts:98-103, :195 | 신규 |

## SR. 검색 (`searchEvaluations`, evaluations.ts:249-294)

| ID | 상황 | 본문 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| SR-01 | 앱 범위 정상 | `/evaluations/search` | matches: id, score, cc, root, ref, to | evaluations.ts:276-292 | 실측 (smoke 10–13) |
| SR-02 | 공개 범위 정상 | `/public/evaluations/search` | matches에 `to` 키 없음 | evaluations.ts:288 | 실측 (smoke 15) |
| SR-03 | 공개 범위에서 to 필터 | `to` 지정 | 400 "not available in public search" | evaluations.ts:263-264 | 실측 (smoke 14) |
| SR-04 | 앱 범위에서 제로 주소로 to 필터 | `to: 0x000…0` | 400 "to must be a non-zero address" | evaluations.ts:265-267 | 신규 |
| SR-05 | topK 0, 51, 1.5, "5" | 범위·타입 오류 | 400 | evaluations.ts:253-256 | 신규 |
| SR-06 | topK 생략 | — | 10 | evaluations.ts:253 | 신규 |
| SR-07 | cc 필터 | 다른 도메인 | 다른 도메인 결과 없음 | evaluations.ts:259, :270 | 실측 (smoke 10) |
| SR-08 | root·ref 필터 | 부모 id | 해당 스레드 또는 직계 답글만 | evaluations.ts:260-261 | 실측 (smoke 11–12) |
| SR-09 | 필터 조합 | cc + root | 두 조건이 함께 적용 | evaluations.ts:258-269 | 신규 (Vectorize 조합 동작 확인 필요) |
| SR-10 | 메타데이터 인덱스 생성 전 저장된 벡터 | 인덱스 뒤늦게 생성 | 필터에서 빠질 수 있음 | (Vectorize 동작) | 갭 (운영 순서 주의) |
| SR-11 | 벡터 길이 불일치 | 384 벡터 | 400 | evaluations.ts:251 | 신규 |
| SR-12 | 결과 0건 | 매칭 없음 | 200, `matches: []` | evaluations.ts:276 | 신규 |
| SR-13 | 방금 저장한 항목 | 저장 직후 검색 | 반영 지연 중에는 빠질 수 있음 | (Vectorize 비동기) | 실측 (55–171초 관찰) |
| SR-14 | 응답에 원문·from이 없음 | 모든 검색 | 원문 없음, from 없음 | evaluations.ts:276-292 | 신규 |
| SR-15 | 벡터는 있는데 D1 행이 없음 (유령) | 롤백 실패 후 | 검색 결과에 그대로 나옴 (D1을 확인하지 않음) | evaluations.ts:270-294 | 갭 |

## GR. 그래프 (`graph.ts`)

| ID | 상황 | 요청 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| GR-01 | 도메인 그래프 정상 | `/graph?cc=example.com` | nodes: evaluation·domain·author, edges: belongs·authored·reply | graph.ts:59-87, :90-96 | 실측 (smoke 16) |
| GR-02 | cc 형식 오류 | `/graph?cc=bad` | 400 | graph.ts:91, :147-160 | 신규 |
| GR-03 | 200건 초과 | 노드가 많음 | 최신 200건만 | graph.ts:6, :94 | 신규 |
| GR-04 | 응답에 원문 주소 없음 | 모든 그래프 | from·to 원값 없음, 작성자는 키로만 표시 | graph.ts:14, :61, projection.ts:8-23 | 신규 |
| GR-05 | 답글 엣지 | 부모가 같은 응답에 있음 | reply 엣지 생성 | graph.ts:82-83 | 실측 (smoke 16) |
| GR-06 | 부모가 같은 응답에 없음 | 다른 페이지 | reply 엣지 없음 | graph.ts:82-83 | 신규 |
| GR-07 | 스레드 그래프 정상 | `/graph/local?id=B` | 같은 root의 평가 노드 | graph.ts:99-109 | 실측 (smoke 17) |
| GR-08 | 스레드 그래프 id 없음 | 존재하지 않는 id | 404 | graph.ts:104 | 신규 |
| GR-09 | 스레드 그래프 id 형식 오류 | `id=abc` | 400 | graph.ts:100, :98-103 | 신규 |
| GR-10 | 유사 그래프 정상 | `/graph/similar?id=A&topK=5` | 자기 자신 제외, 같은 cc | graph.ts:113-143 | 실측 (smoke 18) |
| GR-11 | topK 0 또는 50 | 범위 밖 | 400 (최대 49) | graph.ts:9, :49-56 | 신규 |
| GR-12 | D1 행은 있는데 벡터 없음 | 쓰기 직후 | 404 "vector not found" | graph.ts:121-122 | 실측 (쓰기 직후 404 1회 관찰, 이후 200) |
| GR-13 | 벡터는 있는데 D1 행이 없음 (유령) | 롤백 실패 후 | 노드로 나옴. D1을 확인하지 않음 | graph.ts:124-135 | 갭 |
| GR-14 | 작성자 키 시크릿 누락 | AUTHOR_KEY_SECRET 없음 | 500 "internal error" | graph.ts:61, projection.ts:9-11, :146-165 | 신규 |
| GR-15 | 그래프 뷰 페이지 | `/graph/view?cc=…` | HTML, 데이터는 `/graph*` API에서 가져옴 | graph.ts:168-240 | 신규 |
| GR-16 | 그래프 API에 잘못된 메서드 | POST | 405 | graph.ts:148 | 신규 |

## AU. 세션 (`auth.ts`)

| ID | 상황 | 입력 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| AU-01 | 헤더가 있으면 쿠키 무시 | 헤더 hash + 쿠키 hash 다름 | 헤더 값 사용 | auth.ts:46-58 | 신규 |
| AU-02 | 0x 접두사 유무 | `0x` 있음·없음 | 둘 다 허용 | auth.ts:36-41 | 신규 |
| AU-03 | 길이 형식 불일치 | hash 39자 | null (익명) | auth.ts:61 | 신규 |
| AU-04 | 한쪽만 0 | hash 0, token 정상 | null (익명) | auth.ts:62 | 신규 |
| AU-05 | 세션 검증 | 정상 세션 + S3 읽기 전용 자격 증명(`SESSION_S3_*`) | 웹 세션: S3 `hash/<hash>`의 token·email·ip를 확인해 이메일에서 만든 주소를 `from`으로 쓴다. 앱 세션: token이 HMAC과 같고 S3에 email이 있으면 로그인(ip 비교 없음). 자격 증명이 없거나 무엇이든 확인 실패면 null(기존 동작). **객체 형식은 가정이며 실제 객체로 확인하지 못했다** | auth.ts `checkSession` | 자동:auth.test.ts, session.test.ts (S3는 가짜) |
| AU-06 | 클라이언트가 임의 from을 보냄 | from = 다른 사람 주소 | 검증된 세션이 있으면 무시하고 세션 주소를 쓴다 (자동 확인). **세션이 없으면 여전히 그대로 저장된다** (위조 가능, 설계상 임시) | evaluations.ts | 부분 해결 |

## PJ. 작성자 키 (`projection.ts`)

| ID | 상황 | 입력 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| PJ-01 | 대소문자 무관 | 같은 주소를 대·소문자로 | 같은 키 | projection.ts:20 | 신규 |
| PJ-02 | 같은 주소·같은 비밀 | 반복 호출 | 항상 같은 12자리 | projection.ts:8-22 | 신규 |
| PJ-03 | 비밀이 빈 문자열 | — | 에러 "AUTHOR_KEY_SECRET is not configured" | projection.ts:9-11 | 신규 |
| PJ-04 | 비밀을 바꿈 | 운영 중 교체 | 모든 작성자 키가 바뀜 (그래프의 작성자 노드가 달라짐) | projection.ts:5-6 | 신규 (운영 주의) |

## LG. 레거시 경로 (`index.ts`, 신규 기능과 독립)

| ID | 상황 | 요청 | 기대 결과 (코드 기준) | 근거 | 상태 |
|---|---|---|---|---|---|
| LG-01 | 저장·그래프 외 모든 경로 | 예: `GET /` | 434행 `var userRegion = userRegion[FLAG]`에서 TypeError. try 블록(442행) 밖이라 500 가능성 높음 | index.ts:434, :442 | 갭 (실행 확인 필요. README 10-3 `/` 확인 기록과 충돌) |
| LG-02 | cf 정보가 없는 요청 | `request.cf` 없음 | 384-407행 구조분해에서 실패 | index.ts:384-407 | 갭 (확인 필요) |
| LG-03 | try 블록 안 에러 | 레거시 POST·GET 내부 예외 | catch 후 418 "I'm a teapot!" | index.ts:1098-1104 | 신규 |
| LG-04 | 레거시 바인딩 이름 | `premium_dicount_${zoneRegion}_items` | zoneRegion이 '' 이므로 바인딩이 없음 → 내부 TypeError → 418 | index.ts:436, :607, :972, :1016 | 갭 |
| LG-05 | 쿼리스트링에 credential | 웹 로그인 스크립트가 `https://premium.discount?hash=…&token=…` 호출 | token이 URL에 노출됨. 로그·기록에 남을 수 있음 | auth.js:645, :678, :890 (public/static/js) | 갭 (보안) |
| LG-06 | SQL 문자열 보간 | `'${cookies.team}'` | 인젝션 가능성. 바인드 파라미터로 바꿔야 함 | index.ts:607, :620, :643 | 갭 (보안) |

## DB. D1 스키마와 마이그레이션 (`migrations/`)

| ID | 상황 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|
| DB-01 | 테이블 목록 | `evaluations`, `items`, `crons`, `console`, `d1_migrations` 존재 | 0001~0003 | 실측 (원격 적용 후 확인) |
| DB-02 | evaluations 컬럼 | id, from, to, cc, bcc, ref, root, created_at, updated_at, request_id (10개) | 0002, 0003 | 실측 |
| DB-03 | request_id 부분 유니크 | NULL은 여러 개 허용, 값은 유일 | 0003 | 신규 |
| DB-04 | bcc는 항상 NULL | INSERT에서 `NULL` 고정 | evaluations.ts:220 | 신규 |
| DB-05 | updated_at = created_at | 저장 시 같은 값 | evaluations.ts:216, :221 | 신규 |
| DB-06 | 인덱스 존재 | ref, root, (cc, created_at), request_id | 0002, 0003 | 신규 |
| DB-07 | 0003 없이 0002만 적용된 DB | request_id 컬럼 없어 INSERT 실패 → 500 | evaluations.ts:220 | 신규 (마이그레이션 순서 의존) |
| DB-08 | 마이그레이션 재실행 | "No migrations to apply" | (실측) | 실측 |
| DB-09 | 조회 기본 정렬 | 그래프는 created_at DESC | graph.ts:94 | 신규 |

## VZ. Vectorize 정합성

| ID | 상황 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|
| VZ-01 | 인덱스 차원과 EVAL_DIMENSIONS가 다름 | upsert 실패 → RB-02로 롤백, 500 | evaluations.ts:7, wrangler | 신규 |
| VZ-02 | 메타데이터 키 구성 | cc, root는 항상. to, ref는 값이 있을 때만 | evaluations.ts:165-170 | 신규 |
| VZ-03 | 메타데이터 인덱스 4개 | cc, to, ref, root (최대 10개) | (실측·문서) | 실측 |
| VZ-04 | 쓰기 시점의 D1 값과 메타데이터가 같음 | 같은 변수로 만듦 | evaluations.ts:230, :180 | 신규 |
| VZ-05 | 로컬 wrangler dev에서 EVAL_VECTORIZE | remote 설정 없으면 "needs to be run remotely" 500 | (실측) | 실측 |
| VZ-06 | 원격 테스트 설정 | `wrangler.test.json`의 `remote: true` | wrangler.test.json | 실측 |
| VZ-07 | 쓰기 반영 지연 | 저장 후 검색·getByIds가 지연될 수 있음 | (실측 55–171초) | 실측 |

## CF. 설정과 배포

| ID | 상황 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|
| CF-01 | wrangler.json의 EVAL_VECTORIZE 인덱스가 계정에 없음 | `wrangler deploy` 실패 | wrangler.json:27-30 (`premium-discount-eval` 없음) | 갭 (운영 인덱스 생성 필요) |
| CF-02 | 레거시 Vectorize 두 개가 계정에 없음 | 배포 시 바인딩 실패 가능 | wrangler.json:18-26 | 갭 |
| CF-03 | D1 바인딩 ID가 계정에 있음 | 정상 | wrangler.json:6-17 (f7ec…, a8a1…) | 실측 |
| CF-04 | predeploy 마이그레이션 | 이미 적용되어 있으면 no-op | package.json predeploy | 실측 |
| CF-05 | 시크릿 GUEST_SALT·AUTHOR_KEY_SECRET 미설정 | 저장(GA-02)·그래프(GR-14) 실패 | evaluations.ts:78, projection.ts:9 | 신규 |
| CF-06 | 테스트 설정의 Worker 이름 | `premium-discount-test`로 운영과 다름 | wrangler.test.json | 실측 |

---

## SS. 앱 로그인 (`session.ts`, 2026-11-14 추가)

| ID | 상황 | 요청 | 기대 결과 | 상태 |
|---|---|---|---|---|
| SS-01 | 발급 | `POST /session/start` | 201 `{hash(40), token(64), address, mailto}`. token은 `HMAC(SESSION_SECRET, "session|"+hash)` (서버 밖에서 따로 계산해 비교). `Cache-Control: no-store` | 자동:session.test.ts |
| SS-02 | 비밀 없음 | 〃 | 503 `login is not configured` | 자동 |
| SS-03 | 메일 도착 전 | `GET /session` (발급받은 세션) | `pending` (S3의 404·403·email 없음 모두) | 자동 |
| SS-04 | 메일 도착 후 | 〃 | `signed_in` + 이메일에서 만든 주소. ip가 달라도 통과 | 자동 |
| SS-05 | 변조·모르는 세션 | token 한 글자 변경, 발급하지 않은 hash(S3에 email이 있어도) | `anonymous` | 자동 (구현을 깨면 3개 실패) |
| SS-06 | 웹 세션 | S3의 token이 일치 | `signed_in`, 불일치면 `anonymous` | 자동 |
| SS-07 | S3 오류·설정 누락 | 5xx, 네트워크 오류, 자격 증명 없음 | 503 `unavailable` (로그인으로 보지 않음) | 자동 |
| SS-08 | 제한 | 발급은 저장 제한기, 상태는 읽기 제한기 | 걸리면 429, S3를 읽지 않음 | 자동 |
| SS-09 | 평가 저장 연결 | 로그인한 앱이 `POST /evaluations` (본문 `from`은 다른 주소) | 응답과 대기열의 `from`이 로그인한 이메일에서 만든 주소 | 자동 |

## RL. 요청 제한 (`ratelimit.ts`, 2026-11-14 추가)

| ID | 상황 | 요청 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| RL-01 | 저장이 제한에 걸림 | `POST /evaluations`, 쓰기 제한기 거부 | 429, `Retry-After: 60`, CORS 헤더 유지, 대기열에 아무것도 쓰지 않음 | evaluations.ts | 자동:evaluations.test.ts |
| RL-02 | 검색은 읽기 제한기 | `POST /evaluations/search`, `/public/evaluations/search` | 읽기 제한기만 쓰고 쓰기 제한기와 서로 영향 없음 | evaluations.ts | 자동 |
| RL-03 | status 조회 | `GET /evaluations/status` | 제한하지 않음 (앱이 대기 항목을 한꺼번에 확인한다) | evaluations.ts | 자동 |
| RL-04 | 그래프 JSON | `/graph`, `/graph/local`, `/graph/similar` | 429. `/graph/view` 페이지는 제외 | graph.ts | 자동:graph.test.ts |
| RL-05 | 제한기 없음·IP 없음·제한기 고장 | 로컬 개발, 헤더 없음, 바인딩 오류 | 요청은 통과한다 (제한 장치 장애가 서비스 장애가 되지 않게) | ratelimit.ts | 자동 |
| RL-06 | 임계값 | 쓰기 IP당 분당 20, 읽기 60 | 임시값. wrangler.json `ratelimits`. 키는 IP (`from`은 위조할 수 있어 쓰지 않는다) | wrangler.json | 결정 필요 |

## GV. 그래프 뷰 로컬 원문 오버레이 (`graph.ts` VIEW_HTML)

| ID | 상황 | 전제 | 기대 결과 | 상태 |
|---|---|---|---|---|
| GV-01 | 내 글 노드 선택 | 이 브라우저의 `PremiumDiscountLocal.evaluations`에 같은 `id`가 있다 | 패널에 "내가 쓴 글 (이 기기에만 저장됨)"과 원문(500자까지)이 나온다. 원문은 `textContent`로만 넣는다 (HTML 비실행) | 실측 (Playwright + Chromium, 네트워크는 가짜) |
| GV-02 | 남의 글 노드 선택 | 로컬 캐시에 없다 | 미리보기 없음. 기존 버튼은 그대로 | 실측 |
| GV-03 | 노드를 빠르게 바꿈 | 조회가 늦게 끝난다 | 이전 노드의 미리보기가 섞이지 않는다 | 실측 |
| GV-04 | 로컬 캐시가 없는 브라우저 | `PremiumDiscountLocal` 없음 | 미리보기 없음, DB를 새로 만들지 않는다 (`upgradeneeded`에서 취소) | 실측 |

## 관찰 요약 (코드 읽기 결과)

1. **저장 경로의 일관성**: D1이 먼저, Vectorize가 나중입니다. 실패 시 행을 지우지만, 지우기 자체가 실패하면 고아가 남습니다(RB-03). 재시도가 복구의 전제입니다.
2. **검색은 Vectorize만 봅니다**: D1을 확인하지 않아서 유령 벡터가 그대로 나옵니다(SR-15, GR-13).
3. **신뢰 경계가 약합니다**: from이 검증되지 않고(AU-05, AU-06), 욕설 필터는 앱에서만 돕니다. 서버는 원문이 없어서 재검사할 수 없습니다.
4. **레거시 경로가 살아 있습니다**: 신규 경로를 제외한 요청은 434행에서 실패할 가능성이 높습니다(LG-01). 이 부분은 실행으로 확인해야 합니다.
5. **운영 순서가 중요합니다**: 인덱스와 메타데이터 인덱스, 시크릿이 배포보다 먼저 있어야 합니다(CF-01, SR-10, CF-05).
