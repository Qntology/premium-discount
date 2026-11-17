# premium-discount 공용 라이브러리 (`crates/`)

앱(`app/`, LanceDB 0.31 · candle)과 raon-zena(LanceDB 0.41 · candle 0.11 포크)가 함께 링크하는 crate 입니다. 앱과 따로 빌드합니다(`cd crates && cargo test`). 이름은 약어 없이 `premium-discount-*` 로 씁니다(2026-11-17 `pd-*` 에서 바꿈).

**원칙**
- LanceDB · Tauri · 비동기 런타임(tokio) · 모델 런타임(candle)에 의존하지 않습니다. 저장과 무거운 계산은 앱이 맡습니다.
- 네트워크는 `premium-discount-mcp` 의 블로킹 `GET /graph*`(ureq) 하나뿐입니다.
- `premium-discount-manifest` 는 내려받기 검증 규칙만 갖고, HTTP 는 부르는 쪽이 고릅니다.

| crate | 역할 | 쓰는 곳 |
| --- | --- | --- |
| `premium-discount-identity` | 시드 구문(BIP-39 12단어) · BIP-32 파생 · EIP-191 서명 · `hashId` · 요청 서명(`x-pd-*` 헤더). 기준 `contracts/wallet_v1.json` | nav, schema, 서버 계약 시험 |
| `premium-discount-verdict` | PD-Verdict 판정(`pd-verdict/1` · `pd-verdict/2`). 기준 `contracts/verdict_v*.json` | nav, mcp, raon-zena `zena-verdict` |
| `premium-discount-nav` | 접근성 내비게이션: 후보 합치기 · 1 ~ 9 번호 · 개인정보 화면 · 행동 에피소드 → SDS 결정 · 타임트라이얼 | mcp(`pd_nav_number`) |
| `premium-discount-schema` | `envelope`(`cc` 정규화) · `address`(`to` · `bcc`) · `contract`(앱 명령 17개의 이름 · 인자 · 결과 · 오류 종류) | app(`commands.rs` 가 다시 내보냄), mcp |
| `premium-discount-filter` | 사전 욕설 필터 `LocalFilter` · `Verdict` | app(`filter.rs` 의 `ProfanityFilter` 가 감쌈), mcp(앱을 띄우기 전 1차 검사) |
| `premium-discount-mcp-protocol` | stdio JSON-RPC 2.0 MCP: `McpClient` · `serve` · `ToolHandler` (스레드만) | mcp, raon-zena `zena-avatar-mcp`(`protocol` 로 다시 내보냄) |
| `premium-discount-mcp` | MCP 서버 바이너리(`pd_*` 도구 9개)와 호스트용 `PdSyncClient` · `SyncLoop`, `find_pd_app` · `find_mcp_server`. [README](premium-discount-mcp/README.md) | raon-zena, 데스크톱 설치본(사이드카) |
| `premium-discount-sds` | `Moments`(Welford: 더하기 · 합치기 · 빼기, 모집단 · 표본 분산)와 스코프 백오프 | nav(마찰 기준선), raon-zena `zena-sds` |
| `premium-discount-manifest` | 고정 파일(이름 · 크기 · SHA-256): `.part` 경로 · 해시 · 확인 뒤 이름 바꾸기 · 안전한 상대 경로 | app(`model_fetch.rs`), raon-zena 아바타 카탈로그 |

**계약 값은 그대로입니다.** crate 이름만 바뀌었고, 판정 recipe(`pd-verdict/1` · `pd-verdict/2`), 지갑 헤더(`x-pd-address` …), MCP 도구 이름(`pd_*`)은 서버 · DB · 다른 클라이언트와 맞물린 값이라 바꾸지 않았습니다.

**raon-zena 에서 쓰기.** raon-zena 작업 공간 `Cargo.toml` 이 9개를 이 저장소의 git 리비전으로 고정합니다. 옆에 둔 체크아웃으로 빌드하려면 그 파일 끝의 `[patch."https://github.com/Qntology/premium-discount"]` 주석을 풉니다. 이 crate 들의 공개 API 를 바꾸면, raon-zena 가 리비전을 올릴 때 함께 고쳐야 합니다.

**데스크톱 설치본의 사이드카.** `app/src-tauri/tauri.sidecar.conf.json` 으로 묶으면 `premium-discount-app`(헤드리스 코어)과 `premium-discount-mcp` 가 창 실행 파일 옆에 설치됩니다(`app/src-tauri/sidecars.mjs`, README 22절).
