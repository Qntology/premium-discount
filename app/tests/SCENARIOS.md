# app 테스트 시나리오 (소스 로직 기준)

> 이 문서는 **실제로 실행하지 않았습니다.** 소스를 읽고 각 시나리오의 기대 결과를 코드 기준으로 적었습니다.
> 실제 검증(`cargo test`)은 별도로 진행합니다. 이번 문서는 "무엇을 검사해야 하는가"의 목록입니다.

## 읽는 법

- **상태**
  - `기존:<테스트명>`: 이미 `cargo test --lib`에 있는 테스트가 커버합니다.
  - `신규`: 이번에 정의한 시나리오입니다. 자동화가 필요합니다.
  - `갭`: 현재 구현이 없거나 알려진 문제입니다. 기대 결과는 "목표"이고, 현재 코드는 그 결과를 내지 못합니다.
- **근거**: 파일과 줄 번호입니다. `pipeline.rs:112`처럼 적었습니다.
- 대상 파일: `src/pipeline.rs`(Evaluator), `src/queue.rs`(오프라인 큐), `src/api.rs`(HTTP 어댑터), `src/embedding.rs`(계약·정규화), `src/embeddinggemma.rs`(모델 예산), `src/filter.rs`(욕설 필터 trait)

---

## A. 입력 검증 (`pipeline.rs` `prepare`)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| A-01 | 텍스트가 공백만 있음 | `text = "   "` | `InvalidInput`, 임베딩·저장 호출 없음 | pipeline.rs:112, :186 | 기존:empty_text_is_rejected |
| A-02 | cc 대소문자·공백 | `cc = "  Example.COM "` | `example.com`으로 정규화되어 저장 | pipeline.rs:195 | 기존:clean_text_is_saved_as_vector_with_normalized_cc_and_no_text |
| A-03 | cc 형식 오류 | `localhost`, `example..com`, `-bad.com`, 빈 문자열 | `InvalidInput` | pipeline.rs:195 | 기존:invalid_cc_is_rejected |
| A-04 | cc 64자 초과 | 65자 도메인 | `InvalidInput` | pipeline.rs:203 | 신규 |
| A-05 | TLD가 1자 | `a.b` | `InvalidInput` (tld_ok 조건) | pipeline.rs:202 | 신규 |
| A-06 | from이 0x 없이 40자 hex | `from = "aa…aa"` (42자 아님) | `InvalidInput "from must be an address"` | pipeline.rs:212-219 | 신규 |
| A-07 | from이 제로 주소 | `from = 0x000…0` | 앱은 통과시켜 전송. 서버가 게스트 주소로 바꿈 (서버 시나리오 V-13) | pipeline.rs:212 | 신규 |
| A-08 | to 대문자 주소 | `to = 0xABC…` | 소문자로 정규화 | pipeline.rs:212 | 신규 |
| A-09 | ref가 UUID 아님 | `ref = "not-a-uuid"` | `InvalidInput` | pipeline.rs:130, validate_uuid | 기존:invalid_ref_is_rejected |
| A-10 | ref 대문자 UUID | `ref = "0190F0A0-…"` | 소문자로 전달 | pipeline.rs:130 | 기존:ref_is_passed_through_for_replies |
| A-11 | ref와 local_parent 동시 지정 | 둘 다 `Some` | `InvalidInput "ref and local_parent cannot both be set"` | pipeline.rs:124-129 | 기존:plain_submit_rejects_local_parent_and_ref_together |
| A-12 | local_parent가 빈 문자열 | `local_parent = Some("")` | `InvalidInput` (non_empty) | pipeline.rs:131 | 신규 |
| A-13 | 한글 텍스트 | `"좋은 평가"` | 정상 저장, 원문 미전송 (JSON에 text 키 없음) | pipeline.rs:131 | 기존:clean_text_is_saved_as_vector_with_normalized_cc_and_no_text |
| A-14 | 텍스트 앞뒤 공백 | `"  좋음  "` | trim 후 임베딩 | pipeline.rs:112 | 신규 |

## B. 욕설 필터 분기

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| B-01 | 욕설 판정 (온라인) | `Verdict::Profane` | `SubmitOutcome::Blocked`, 임베딩·전송 없음 | pipeline.rs:135-137 | 기존:profane_text_is_blocked_before_embedding_and_saving |
| B-02 | 욕설 판정 (큐 경로) | `Profane` + `submit_queued` | Blocked, 큐에 아무것도 들어가지 않음 | pipeline.rs:91 | 기존:profane_text_is_blocked_and_never_queued |
| B-03 | 검색은 필터를 거치지 않음 | `Profane` 텍스트로 `search` | 검색 요청이 나감 | pipeline.rs:154-176 | 기존:search_does_not_run_profanity_filter |
| B-04 | 필터 자체가 에러 | `check()`가 `AppError::Filter` 반환 | `submit`이 그 에러를 그대로 반환, 저장 없음 | pipeline.rs:135 | 신규 |
| B-05 | 욕설 필터 구현체 | 실제 `Evaluator` 조립 | `LocalFilter`(사전 기반)로 조립된다. 모델 기반 필터는 `ProfanityFilter`를 구현해 바꿔 끼운다 | filter.rs | 기존:filter::tests::*, commands::tests::profane_input_leaves_no_trace_locally |

## C. 임베딩 계약 (`embedding.rs`, `embeddinggemma.rs`)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| C-01 | 임베더 출력이 정규화되지 않음 | 모든 원소 2.0 | 저장되는 벡터는 단위 길이 | pipeline.rs:179, embedding.rs:40 | 기존:unnormalized_embedder_output_is_saved_as_unit_vector |
| C-02 | 검색 쿼리도 정규화 | 같은 임베더 | 검색 벡터가 단위 길이 | pipeline.rs:154-176 | 기존:unnormalized_embedder_output_is_searched_as_unit_vector |
| C-03 | 0벡터 | 모든 원소 0 | 거부, 저장 없음 | embedding.rs:40-46 | 기존:zero_vector_from_embedder_is_rejected_without_saving |
| C-04 | NaN·Infinity 포함 | 원소에 NaN | 거부 | embedding.rs:41-43 | 기존:normalize_rejects_zero_and_non_finite |
| C-05 | 차원이 768이 아님 | 길이 10 | 거부 (`expected 768 dims`) | embedding.rs:20-27 | 기존:wrong_dimension_from_embedder_is_rejected_without_saving |
| C-06 | 구버전 클라이언트 (384차원) | 길이 384 벡터 | 거부 | embedding.rs:20-27 | 신규 |
| C-07 | 임베더가 에러 반환 | `Embed` 에러 | 에러 전파, 저장 없음 | pipeline.rs:179 | 신규 |
| C-08 | 모델 디렉터리가 없음 | `EmbeddingGemma::load(없는 경로)` | `Embed` 에러로 로드 실패 | embeddinggemma.rs (load) | 신규:load_reports_missing_model_dir |
| C-09 | 토큰 예산 초과 입력 | 오디오 330초 (8,250토큰) | `check_budget`이 거부 | embeddinggemma.rs:52-60 | 기존:budget_rejects_total_over_limit |
| C-10 | 토큰 예산 검사가 파이프라인에 연결됨 | 예산 초과 입력을 `submit`·`search`에 전달 | 필터·모델·서버 호출 전에 `InvalidInput` (`Embedder::token_count` → `check_budget`). 이미지·오디오·비디오 조각의 사전 검사는 연결하지 않았다 (Evaluator는 텍스트 경로만 쓴다) | pipeline.rs `check_token_budget` | 기존:budget_bcc_and_merge_tests::over_budget_* |
| C-11 | 텍스트 임베딩이 Python 참조와 일치 | fixture 6건 (질의·문서·유사도, 558토큰 포함) | 토큰 id 같음, 코사인 ≥ 0.999 | tests/embeddinggemma_reference.rs | 신규 (`--ignored`, `EMBEDDINGGEMMA_DIR` 필요) |

## D. 온라인 저장 (`submit`, pipeline.rs:63)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| D-01 | 정상 저장 | 임베더·서버 정상 | `Saved(response)`, 응답의 id·root·ref·from 반환 | pipeline.rs:63-80 | 기존:clean_text_is_saved_… |
| D-02 | 서버 5xx | 저장 실패 | 에러 반환, 큐에는 남지 않음 | pipeline.rs:76 | 신규 |
| D-03 | 응답 유실 후 사용자가 다시 누름 | 서버는 저장, 클라이언트는 실패로 봄 | 새 request_id로 중복 저장 (멱등 보장 없음) | pipeline.rs:142 | 갭 (알려진 문제) |
| D-04 | 답글에 local_parent 사용 | `submit`에 `local_parent` | `InvalidInput "local_parent needs submit_queued"` | pipeline.rs:70-73 | 기존:plain_submit_rejects_… |

## E. 큐 저장 (`submit_queued`, pipeline.rs:85, queue.rs)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| E-01 | 온라인에서 큐 경로 | 서버 정상 | `Saved`, 큐 비어 있음 | pipeline.rs:85-103 | 기존:submit_queued_saves_immediately_when_online |
| E-02 | 오프라인 | `Offline` | `Queued{local_id}`, 큐 1건 | queue.rs:195-198 | 기존:submit_queued_keeps_item_while_offline_and_flushes_later |
| E-03 | 연결 복구 후 flush | 큐 1건 | 전송 후 큐 비움 | queue.rs:159-215 | 기존:offline_items_wait_and_go_out_in_order |
| E-04 | 부모가 큐에 대기 중인 답글 | 부모 `Local`, 자식 답글 | 부모가 먼저 나가고 자식은 서버 id를 씀 | queue.rs:227-237 | 기존:reply_is_sent_with_parent_server_id_after_parent |
| E-05 | 부모가 이미 전송된 답글 | `enqueue` 시점 | 서버 id로 바로 바뀜 | queue.rs:130-140 | 기존:reply_to_already_sent_parent_uses_server_id_at_enqueue |
| E-06 | 알 수 없는 local_parent | 큐에 없는 로컬 id | `InvalidInput "unknown local parent"` | queue.rs:139 | 기존:enqueue_rejects_unknown_parent_and_ref_field |
| E-07 | 응답 유실 후 재전송 | 서버 저장 완료, 응답 유실 | 같은 request_id로 재전송, 서버에는 1건 | queue.rs:159 | 기존:lost_response_is_not_stored_twice |
| E-08 | 앞 항목 때문에 새 항목이 대기 | 앞 항목 503, 연결은 됨 | 새 항목은 `Queued` (앞 항목이 막고 있음) | queue.rs:200-206 | 신규 |
| E-09 | 5xx 5회 연속 | 앞 항목 503 × 5 | 앞 항목 폐기, 뒤 항목 정상 전송 | queue.rs:199-209 | 기존:server_errors_retry_then_reject_without_blocking_later_items |
| E-10 | 400 응답 | 영구 거부 | 즉시 폐기, 그 답글도 함께 폐기 | queue.rs:98-107, :209 | 기존:permanent_rejection_drops_item_and_its_replies |
| E-11 | 401·403·404 응답 | 라우팅·설정 오류 | 영구 거부로 분류되어 즉시 폐기 | queue.rs:98-107 | 신규 (위험: 설정 오류가 데이터 손실로 이어짐) |
| E-12 | 408·429 응답 | 일시 오류 | 재시도 (5회 한도) | queue.rs:102 | 신규 |
| E-13 | 2xx인데 응답 본문을 읽지 못함 | 서버는 저장 완료 | 본문이 끊기거나 JSON이 잘렸으면 `Offline`(횟수를 세지 않고 같은 request_id로 다시 보냄, 서버 replay로 회수). 끝까지 닿았는데 계약과 다르면 `BadResponse`(재시도 한도까지) | api.rs `read_response` | 기존:response_recovery_tests::* |
| E-14 | 연결 실패·타임아웃 | `Offline` | 횟수를 올리지 않고 멈춤 | queue.rs:195-198 | 신규 |
| E-15 | 거부된 부모에 새 답글 추가 | `enqueue` | `Rejected` 에러 | queue.rs:133-135 | 기존:permanent_rejection_drops_item_and_its_replies (후반부) |
| E-16 | 거부 사실을 사용자에게 알림 | `FlushReport.rejected` | 앱 UI에 표시 | (UI 없음) | 갭 |
| E-17 | 오래 오프라인 | 큐가 계속 쌓임 | 상한 없음. 항목 하나는 768 × f32 ≈ 3KB 원시 크기 | queue.rs (상한 없음) | 갭 |

## F. 영속성 (`to_json` / `from_json`)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| F-01 | JSON 왕복 후 id 매핑 유지 | 부모 전송 후 직렬화 | 복원 후 답글이 서버 id를 찾음 | queue.rs:218-224 | 기존:queue_survives_json_round_trip_with_id_mapping |
| F-02 | 앱 재시작 | 큐에 대기 항목 있음 | 디스크 저장이 없으므로 **전부 소실** (목표: 복원) | queue.rs:11 | 갭 |
| F-03 | 손상된 JSON 로드 | 잘린 문자열 | `from_json` 에러, 빈 큐로 시작하지 않도록 처리 필요 | queue.rs:222 | 신규 |

## G. HTTP 어댑터 (`api.rs`)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| G-01 | 저장 요청 JSON 키 | `ref`가 None | `ref`·`from`·`to` 키가 생략됨 | api.rs:11-24 | 기존:save_request_uses_ref_key_and_omits_none |
| G-02 | 저장 요청에 request_id 포함 | — | `request_id` 키 전송 | api.rs:14 | 기존:save_request_sends_request_id |
| G-03 | 검색 요청 키 | — | `topK`로 전송 (camelCase) | api.rs:39-40 | 기존:search_request_uses_top_k_camel_case |
| G-04 | 기본 헤더 | 세션 없음 | 익명 자리값 헤더 2개 | api.rs:88-90, :164-165 | 기존:sends_anonymous_headers_by_default |
| G-05 | 세션 지정 | `with_session("ab","cd")` | 헤더가 `ab`, `cd`로 전송 | api.rs:148-154 | 기존:sends_session_headers_when_set |
| G-06 | 연결 거부 | 포트 1 | `Offline` | api.rs:170-171 | 기존:connection_refused_is_reported_as_offline |
| G-07 | 타임아웃 15초 | 응답 지연 | `Offline` | api.rs:138, :170 | 신규 (실제 지연 주입 필요) |
| G-08 | 2xx인데 본문 형식 불일치 | `{"id":1}` 등 | `AppError::BadResponse` (재시도 대상). E-13과 연결 | api.rs `read_response` | 기존:response_recovery_tests::complete_body_that_breaks_the_contract_is_a_retryable_bad_response |
| G-09 | 4xx·5xx 응답 | 본문 텍스트 | `Server{status, message}`로 변환 | api.rs:177-182 | 신규 |
| G-10 | 응답의 ref가 null, to 없음 | 공개 검색 응답 | 파싱 성공, to는 None | api.rs:58-67 | 기존:parses_public_search_response_without_to |

## H. 검색 (`search`, pipeline.rs:154)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| H-01 | top_k 0 또는 51 | 범위 밖 | `InvalidInput` | pipeline.rs:156-160 | 기존:search_rejects_top_k_out_of_range |
| H-02 | top_k 1과 50 | 경계값 | 허용 | pipeline.rs:156 | 기존:search_rejects_top_k_out_of_range (후반부) |
| H-03 | cc 정규화 | `"Example.COM"` | `example.com`으로 전송 | pipeline.rs:161 | 기존:search_normalizes_filters_and_passes_top_k |
| H-04 | root·ref가 UUID 아님 | `"abc"` | `InvalidInput` | pipeline.rs:162-163 | 신규 |
| H-05 | to가 잘못된 주소 | `"0x12"` | `InvalidInput` | pipeline.rs:164 | 신규 |
| H-06 | to가 제로 주소 | `0x000…0` | 앱은 통과시킴. 서버가 400으로 거부 (서버 시나리오 S-04) | pipeline.rs:164 | 신규 |
| H-07 | 결과가 0건 | 매칭 없음 | `matches: []` | api.rs:51-53 | 신규 |
| H-08 | 결과에 원문이 없음 | 검색 응답 구조 | `Match`에는 id, score, cc, root, ref, to만 있음 | api.rs:58-67 | 신규 |
| H-09 | 방금 저장한 항목이 검색에 안 나옴 | 서버 반영 지연 | `search_merged`가 로컬 검색과 서버 검색을 한 번의 임베딩으로 합쳐서 방금 쓴 글도 보인다 (서버 id로 중복 제거, 서버 실패 시 로컬 결과만) | pipeline.rs `search_merged`, `merge_matches` | 기존:commands::tests::merged_search_* |

## I. 세션 (`api.rs` Session)

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| I-01 | hash 또는 token 한쪽이 0 | 익명 자리값 혼합 | 익명으로 취급 | api.rs:108-117 | 기존:one_zero_side_still_counts_as_anonymous |
| I-02 | 실제 세션 | 0이 아닌 값 | 익명이 아님 | api.rs:108-117 | 기존:with_session_replaces_anonymous |
| I-03 | 기본값 | `HttpBackend::new` | 익명 | api.rs:136-145 | 기존:anonymous_session_is_default |
| I-04 | 세션을 줘도 서버에서는 게스트로 처리 | 세션 헤더 있음 | 서버 검증 미연결이라 from은 그대로 (서버 A-05 참고) | api.rs:148 | 갭 (검증 미구현) |

## J. 조립과 실행 진입점

| ID | 상황 | 입력·전제 | 기대 결과 | 근거 | 상태 |
|---|---|---|---|---|---|
| J-01 | EmbeddingGemma + HttpBackend + 필터로 조립 | `LocalFilter` | 조립된다 (`src/main.rs`의 `build_core`) | filter.rs, main.rs | 해결 (모델 파일이 있는 실행은 확인하지 못함) |
| J-02 | 실행 진입점 (바이너리·Tauri 커맨드) | — | `src/main.rs`가 UI 명령 6개(`cmd_*`)를 표준 입출력 JSON으로 연다. `src/commands.rs`는 Tauri 커맨드가 그대로 부르는 계층이다. Tauri 셸(창·설정 파일)은 추가하지 않았다 | commands.rs, main.rs, ui/commands.ts | 부분 해결 (Tauri 셸 제외) |
| J-03 | 루트의 옛 `app/embedding.rs` | 크레이트에 포함되지 않음 | 이 브랜치에는 파일이 없다 (이미 삭제됨) | `git ls-files app` | 해결 |

---

## 자동화 메모

- 위 시나리오 중 `기존`은 이미 `cargo test --lib`에서 돌아갑니다. `신규`는 `app/tests/*.rs`로 옮기면 됩니다. 네트워크가 필요한 G-07, E-13은 fake 백엔드나 로컬 리스너로 재현합니다.
- `갭`은 테스트가 실패하도록 두지 말고, 구현 전까지 `#[ignore = "갭: ..."]`로 표시하는 방식을 추천합니다.
