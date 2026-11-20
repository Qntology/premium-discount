# apps/core (평가 클라이언트 로직, `premium-discount-app`)

평가 저장과 확인의 Rust 로직입니다. 루트 작업 공간(`../../Cargo.toml`)의 멤버이고, 산출물은 루트 `target/` 에 생깁니다 (2026-11-19 까지는 `app/`, 화면은 `app/ui`, Tauri 창은 `app/src-tauri` 였습니다). 루트 `README.md`의 8~11번 섹션이 서버 계약과 진행 상황을 설명합니다.

## 구조
- `src/pipeline.rs`: `Evaluator::submit`(온라인), `submit_queued`(대기열 경유), `search`, `status_of`
  - 입력 검사 → 로컬 욕설 필터 → 임베딩 → L2 정규화 → 계약 검사 → 원문 해시 계산 → 전송
  - 원문은 8KB 이하여야 합니다 (`MAX_TEXT_BYTES`, 서버와 같은 값)
- `src/queue.rs`: 오프라인 대기열. 원문·해시·벡터를 함께 보관하고, 서버 status가 거부·실패면 버립니다
- `src/api.rs`: 서버 DTO와 `HttpBackend`. `text`, `text_hash`를 보내고, 202와 200 응답의 `status`를 읽습니다
- `src/embedding.rs`: `Embedder` trait, L2 정규화, 서버 계약 검사(768차원, norm ≈ 1)
- `src/embeddinggemma.rs`: google/embeddinggemma-2 설정(config.json 기준 레이어 구성, 768차원), 역할 접두어(`with_prompt`), 입력 토큰 예산 검사, `EmbeddingGemma::load`(토크나이저와 가중치를 읽고, `embed_parts`로 텍스트·이미지·오디오·비디오 조각을 임베딩)
- `src/embeddinggemma_text.rs`: 텍스트 백본 (candle, Gemma 구조). `language_model.*` 텐서만 읽습니다
- `src/embeddinggemma_vision.rs`: 이미지·비디오 프레임 경로 (크기 조정, 패치, 비전 타워, 투영)
- `src/embeddinggemma_audio.rs`: 오디오 경로 (log-mel, 오디오 타워, 투영)
- `src/filter.rs`: `ProfanityFilter` trait과 `LocalFilter`(사전 기반 로컬 1차 필터, 모델·네트워크 불필요). 정밀도 우선입니다: 명백한 욕설만 막고, 애매한 표현은 서버(cron)의 결정 모델이 다시 판정합니다. 모델 기반 필터는 같은 trait을 구현해 바꿔 끼웁니다
- `src/commands.rs`: UI 명령 계층 `AppCore` (`cmd_submit_evaluation`, `cmd_sync_cache`, `cmd_search_local`, `cmd_search_remote`, `cmd_search_merged`, `cmd_flush_outbox`). 프레임워크에 묶이지 않습니다
- `src/main.rs`: 실행 파일. 위 명령을 표준 입출력(JSON 한 줄씩)으로 엽니다. 환경 변수는 `--help` 참고
- `web/desktop/index.html`, `style.css`, `main.ts`, `view.ts`, `transport.ts`: 웹뷰 화면. 평가 입력 폼, 내 평가 상태 목록(전송 대기/검수 중/등록됨/거부됨), 검색(내 글 + 서버 결과). 문구와 상태 변환은 `view.ts`(순수 함수, `view.test.ts`)
- `web/desktop/dev-server.mjs`, `web/desktop/build.mjs`: 개발용 브리지(브라우저의 `/invoke`를 실행 파일의 표준 입출력으로 전달)와 esbuild 묶기. Tauri에서는 `window.__TAURI__`의 `invoke`를 쓰므로 브리지가 필요 없다
- `src/session.rs`: 로그인 세션 저장소 (`PD_DATA_DIR/session.json`, 소유자만 읽는 0600, 임시 파일 후 이름 변경)
- `apps/desktop/`: Tauri v2 창. 코어(이 크레이트)와 화면(`web/desktop/dist`)을 묶고 명령 11개(`cmd_*`)를 코어에 연결한다. 앱 본체는 `apps/desktop/src/lib.rs`(`run()`), `main.rs`는 그것을 부르는 한 줄이다 (모바일 빌드가 라이브러리를 요구해서 나눴다). 설정은 `apps/desktop/src/settings.rs`
- `web/desktop/qr.ts`, `web/desktop/coalesce.ts`: QR 그리기(SVG)와 겹친 갱신 합치기
- `web/desktop/commands.ts`: 화면 쪽 래퍼 (`makeCommands(invoke)`, `syncCacheFromRust`). `web/desktop/fixtures/command_contract.json`이 Rust와 맺은 명령 계약입니다

## 임베딩 입력 규칙
- 저장하는 평가 원문은 `title: none | text: ` 접두어를 붙여 임베딩합니다 (`Role::Document`).
- 검색 질의는 `task: search result | query: ` 접두어를 붙여 임베딩합니다 (`Role::Query`).
- 서버로 보내는 `text`와 `text_hash`에는 접두어를 붙이지 않습니다. 해시는 사용자가 입력한 원문 기준입니다.

## 저장 응답 처리
| 서버 응답 | 앱 결과 |
|---|---|
| 201/202 `status: pending` | `SubmitOutcome::Accepted` (서버가 받았고 욕설 검사 중) |
| 200/201 `status: stored` 또는 status 없음 | `SubmitOutcome::Saved` |
| `status: rejected` 또는 `failed` | `AppError::Rejected` (대기열이면 버림) |
| 연결 실패 | `Queued` (대기열에 보관) |

`Accepted`는 저장 완료가 아닙니다. 최종 결과는 `Evaluator::status_of(request_id)`로 확인합니다.
(`stored`, `pending`, `processing`, `rejected`, `failed`. 거부되면 `reason`에 이유가 옵니다.)

## 현재 상태
- embeddinggemma-2 텍스트·이미지·오디오·비디오 프레임 추론은 연결했습니다. transformers 5.17 참조와 코사인 0.999 이상으로 일치합니다 (아래 테스트). 비디오·오디오 파일 디코딩과 laya 욕설 필터는 아직 없습니다.
- 테스트는 가짜 임베딩·필터·백엔드로 동작을 검증합니다. 실제 서버와의 통신은 확인하지 않았습니다.
- Tauri 창은 `apps/desktop/`에 있습니다 (아래 "Tauri 창"). Android(arm64) 디버그 APK 빌드까지 확인했습니다 (루트 README 15-6). 설치 파일(deb, AppImage, msi, dmg) 번들은 만들어 보지 않았고, Linux(WebKitGTK) 밖에서는 실행해 보지 못했습니다.
- `LocalFilter`는 사전 기반이라 문맥을 이해하지 못합니다. convaiinnovations/laya-multilingual 같은 모델 필터는 아직 붙이지 않았습니다 (루트 `README.md` 15-4 참고).
- 실제 `EmbeddingGemma`로 `main`을 끝까지 실행해 확인했습니다 (`tests/e2e/`, 가짜 서버 사용). 실제 서버와의 통신은 확인하지 못했습니다.
- 대기열의 디스크 저장은 `OfflineQueue::save_to`/`load_from`으로 구현되어 있지만, 언제 호출할지는 정하지 않았습니다.

## 화면 실행 (개발)
```bash
cargo build --release --bin premium-discount-app     # 이 폴더(apps/core)에서. 실행 파일은 루트 target/release
cd ../../web/desktop && npm install && npm run build  # dist/에 묶는다 (dev-server.mjs 의 기본 PD_BIN 은 ../../target/release)
PD_MODEL_DIR=<embeddinggemma-2 폴더> PD_DATA_DIR=<보관함 폴더> PD_SERVER_URL=<서버 주소> node dev-server.mjs
# → http://127.0.0.1:5173
```
실모델과 Chromium으로 화면을 끝까지 쓰는 검증은 `tests/e2e/ui_e2e.mjs` 머리말에 방법이 있다 (가짜 서버 포함).

## Tauri 창
`apps/desktop/`은 `premium-discount-app`(코어)을 작업 공간 의존성으로 쓰는 별도 크레이트입니다. 코어에는 GUI 의존성이 들어가지 않습니다. 명령마다 인자 하나(`args`)만 받으므로 Tauri의 인자 이름 변환(camelCase)이 끼지 않습니다.

```bash
# 시스템 라이브러리 (Ubuntu): libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev patchelf
cd ../../web/desktop && npm install && npm run build   # 화면 묶기 (apps/desktop/tauri.conf.json의 frontendDist)
cd ../../apps/desktop && cargo build                  # 디버그 빌드는 모델을 올리는 데 오래 걸린다. 배포용은 apps/desktop 에서 cargo tauri build
```

- **모델은 앱에 넣지 않습니다** (약 1.5GB). 폴더를 지정합니다. 우선순위: 환경 변수(`PD_MODEL_DIR`, `PD_DATA_DIR`, `PD_SERVER_URL`) > 설정 파일 > 기본값.
  설정 파일은 앱 설정 폴더의 `config.json`입니다 (Linux: `~/.config/discount.premium.app/config.json`).
  ```json
  { "model_dir": "/home/me/models/embeddinggemma-2", "server_url": "https://premium.discount" }
  ```
  기본 모델 위치는 `<데이터 폴더>/models/embeddinggemma-2`입니다.
- 코어는 **첫 명령이 올 때** 만듭니다(모델 로드). 창은 바로 뜨고, 만들지 못하면(모델 폴더 없음 등) 모든 명령이 `not_ready` 오류를 돌려주며 화면이 원인과 해결 방법을 보여 줍니다.
- 화면은 `window.__TAURI__`(`withGlobalTauri`)의 `invoke`로 명령을 부릅니다. CSP는 `default-src 'self'; connect-src ipc: http://ipc.localhost`입니다.
- 새 명령을 추가하려면 `src/commands.rs`(AppCore), `apps/desktop/src/lib.rs`(`#[tauri::command]` 한 줄과 `generate_handler!`), `web/desktop/commands.ts`, `web/desktop/fixtures/command_contract.json`을 함께 바꿉니다. 계약 파일은 Rust와 화면 테스트가 같이 읽습니다.

진짜 창을 조작하는 검증은 `tests/e2e/tauri_e2e.mjs` 머리말에 방법이 있습니다 (Xvfb + WebKitWebDriver, `TAURI_WEBVIEW_AUTOMATION=true`). WebView의 저장소(IndexedDB)는 XDG 폴더 아래에 생기므로 실행마다 임시 폴더로 격리합니다.

## 모델 내려받기, 텍스트 전용, 양자화 (루트 README 16절)
- 모델이 없으면 앱이 시작 안내 화면에서 바로 받는다: **텍스트 전용 574.3MB**(권장, 원본에서 `language_model.*` 구간만 Range로 받아 `model-text.safetensors`로 조립) 또는 전체 1.52GB. 리비전·SHA-256 고정, 이어받기, 멈추기. 코드는 `src/model_fetch.rs`, 명령은 `cmd_model_status`, `cmd_model_download {variant}`, `cmd_model_download_cancel` (모델이 없어도 답한다).
- 명령줄로 받기: `cargo run --release --example fetch_model -- <모델 폴더> text|full`
- 모델 폴더는 `model.safetensors`(전체) 또는 `model-text.safetensors`(텍스트 전용)와 `tokenizer.json`이면 된다. 둘 다 있으면 전체를 쓴다.
- 입력 임베딩 표(262,144 x 512)는 파일 매핑에 두고 필요한 행만 읽는다. 비전·오디오 타워는 그 입력이 처음 올 때 올린다 (`src/residency.rs`: 실측 비용, 메모리 예산, 타워 간 전환).
- 설정 (환경 변수 > config.json > 기본값):
  - `modalities` / `PD_MODALITIES`: `auto`(기본) | `text`(타워를 올리지 않는다)
  - `quant` / `PD_QUANT`: `auto`(기본: 모바일 Q8_0, 데스크톱 F32) | `off` | `q8_0`. Q8_0은 층 가중치 520MB → 138MB, 참조 대비 코사인 0.9999
  - `model_base_url` / `PD_MODEL_BASE_URL`: 모델을 받는 주소 (기본 Hugging Face)
- 실측 도구: `examples/model_probe.rs`(로드 시간·상주 메모리·코사인), `examples/quant_probe.rs`(양자화 레시피별 코사인·크기·시간, 층·묶음 민감도)

## 욕설 필터(laya)와 items 초안(Qwen3.5) (루트 README 17절)
- **욕설 필터**: 사전 필터(`LocalFilter`)로 시작하고, `laya-multilingual`(678MB)을 받으면 앱을 다시 켜지 않고 사전 + laya로 바뀐다 (`SwitchFilter`).
  화면의 작성 칸 아래 "더 정확한 모델 받기" 또는 `cmd_model_download {variant: "filter"}`. 코드는 `src/laya.rs`.
- **items 초안**: 쇼핑 페이지 글 → `{type(1 premium / 2 discount), head, body, link, digest}` (이 기기의 Qwen3.5-0.8B, `src/qwen35.rs`, `src/items_draft.rs`).
  초안 모델은 원본 1.75GB 중 텍스트 가중치 구간만(1.52GB) 받는다. 명령 `cmd_item_draft {text, link}`, 화면 오른쪽 "페이지 초안" 카드.
  출력 헤드는 16,384개로 자른다 (입력 어휘는 그대로, 요청마다 페이지 토큰을 더한다).
- **양자화 캐시**: Q8_0으로 열면 `<모델 폴더>/.pd-cache/*.gguf`에 결과를 남기고 다음부터 그대로 읽는다 (지문이 다르면 다시 만든다, `src/qcache.rs`).
- 설정 추가 (환경 변수 > config.json > 기본값):
  - `filter` / `PD_FILTER`: `auto`(기본: laya가 있으면 laya) | `dictionary` | `laya`
  - `filter_model_dir` / `PD_FILTER_MODEL_DIR`: 기본 `<model_dir의 부모>/laya-multilingual`
  - `draft_model_dir` / `PD_DRAFT_MODEL_DIR`: 기본 `<model_dir의 부모>/qwen3.5-0.8b`
  - `quant`가 비전·오디오 타워, laya, Qwen에도 적용된다 (`auto`면 모바일만 Q8_0)
- 실측 도구: `examples/filter_eval.rs`(사전·laya 비교), `examples/qwen_probe.rs`(헤드 Trimming·Q8_0·NLL)
- 참조 시험 (모델과 Python 참조 픽스처 필요):
  ```bash
  LAYA_DIR=<laya-multilingual> cargo test --release -j 1 --test laya_reference -- --ignored --nocapture
  QWEN_DIR=<Qwen3.5-0.8B> cargo test --release -j 1 --test qwen35_reference -- --ignored --nocapture
  ```
  픽스처는 `tests/reference/*.py`가 진짜 laya·transformers로 만든다 (torch CPU, 판은 각 스크립트 머리말).

## 로그인 (QR 또는 메일 링크)
Rust가 `cmd_app_status`의 `platform`(`android` | `ios` | `desktop`)으로 실행 환경을 알려 주고, 화면이 그에 맞는 방식을 고릅니다. 로그인 시작·확인·저장은 어느 쪽이든 같습니다 (`cmd_login_start` → `cmd_login_status`).
- **데스크톱**: QR을 보여 주고 폰으로 스캔해 메일을 보냅니다 (아래).
- **모바일(Android·iOS)**: QR 대신 **"메일 앱으로 로그인"** 버튼(링크)을 보여 줍니다. 누르면 기기의 메일 앱이 `mailto:<hash>.premium.discount@oauth.email`로 제목과 본문이 채워진 새 메일을 엽니다. **사용자가 보내기만 하면 됩니다.** 앱이 메일을 대신 보내지는 못합니다 (운영체제가 허용하지 않고, 사용자의 계정으로 메일을 보내는 일이기 때문입니다). 메일 앱에서 돌아오면(화면이 다시 보일 때) 바로 한 번 확인하고, 그 뒤에는 3초마다 확인합니다.
- 링크는 `tauri-plugin-opener`로 엽니다. 권한은 `mailto:`만 허용합니다 (`apps/desktop/capabilities/default.json`). `opener:default`는 http(s)까지 열어서 쓰지 않았습니다. 브라우저 개발 화면에서는 일반 링크로 동작합니다.
- 개발용 브라우저에서 모바일 화면을 보려면 주소에 `?platform=android`를 붙입니다.

## QR 로그인
- 흐름: 화면의 "로그인" → `cmd_login_start`(서버 `POST /session/start`가 세션 발급) → QR(`mailto:<hash>.premium.discount@oauth.email`)과 주소 표시 → 사용자가 폰으로 메일 한 통 전송 → 3초마다 `cmd_login_status`(서버 `GET /session`) → 메일이 도착하면 로그인, 세션을 `session.json`에 저장.
- `token`은 Rust 안에만 있고 화면으로 가지 않습니다. 앱을 다시 켜면 저장된 세션을 복원하고 서버에 아직 유효한지 확인합니다 (연결이 없으면 그대로 둡니다). 로그아웃하면 파일을 지웁니다.
- 대기 시간은 15분입니다. 지나면 새 QR을 만듭니다.
- 세션 파일은 평문(0600)입니다. 운영체제 키체인에는 넣지 않았습니다.

## 테스트
```bash
cargo test -p premium-discount-app
node --experimental-strip-types --test web/desktop/*.test.ts   # premium-discount 폴더에서
```

모델 파일이 있으면 참조 비교 테스트도 실행합니다 (기본 실행에서는 건너뜁니다). 실모델로 실행 파일을 끝까지 돌리는 검증은 `tests/e2e/e2e_main.py` 머리말에 방법이 있습니다.

```bash
EMBEDDINGGEMMA_DIR=/path/to/embeddinggemma-2 cargo test --release -j 1 --test embeddinggemma_reference -- --ignored --nocapture
# 양자화한 텍스트 백본으로 같은 참조와 비교 (계약: 코사인 0.999 이상)
PD_QUANT=q8_0 EMBEDDINGGEMMA_DIR=/path/to/embeddinggemma-2 cargo test --release -j 1 --test embeddinggemma_multimodal -- --ignored --nocapture
```
