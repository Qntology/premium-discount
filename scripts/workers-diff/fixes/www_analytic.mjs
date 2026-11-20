// www analytic.ts(35781c0)에 덧대는 수정 — Rust 판(workers/www/src/analytic.rs · analytic/)의 같은 수정과 1:1. id 는 docs/workers-rs.md 의 "고친 버그" 표와 같다.
const F = "workers/www/src/analytic.ts";

export default [
  // gzip 본문을 상한 없이 풀었다: 256 KB 의 gzip 이 약 264 MB 로 풀린다(메모리 한도 128 MB). 푼 크기 1 MiB 를 넘으면 413.
  // pako Inflate 의 출력 버퍼를 상한 + 1 바이트로 두고, 버퍼를 내보낼 때(onData) 상한을 넘으면 던진다 — Rust inflate.rs 와 같은 경계
  {
    id: "fix-analytic-gzip-cap", worker: "www", file: F,
    find: "import { ungzip } from 'pako'",
    replace: "import { Inflate, ungzip } from 'pako'",
  },
  {
    id: "fix-analytic-gzip-cap", worker: "www", file: F,
    find: "function readBody(bytes: Uint8Array, encoding: string | null): unknown {\n\tconst text = encoding && encoding.toLowerCase().includes('gzip') ? new TextDecoder().decode(ungzip(bytes)) : new TextDecoder().decode(bytes)",
    replace: [
      "const MAX_INFLATED_BYTES = 1024 * 1024",
      "const BODY_TOO_LARGE = new Error('body too large')",
      "function inflateLimited(bytes: Uint8Array): Uint8Array {",
      "\tconst inflator = new Inflate({ chunkSize: MAX_INFLATED_BYTES + 1 })",
      "\tconst chunks: Uint8Array[] = []",
      "\tlet total = 0",
      "\tinflator.onData = (chunk: Uint8Array) => {",
      "\t\ttotal += chunk.length",
      "\t\tif (total > MAX_INFLATED_BYTES) throw BODY_TOO_LARGE",
      "\t\tchunks.push(chunk)",
      "\t}",
      "\tinflator.push(bytes, true)",
      "\tif (inflator.err) throw inflator.msg || 'inflate error'",
      "\tconst out = new Uint8Array(total)",
      "\tlet at = 0",
      "\tfor (const c of chunks) {",
      "\t\tout.set(c, at)",
      "\t\tat += c.length",
      "\t}",
      "\treturn out",
      "}",
      "function readBody(bytes: Uint8Array, encoding: string | null): unknown {",
      "\tconst text = encoding && encoding.toLowerCase().includes('gzip') ? new TextDecoder().decode(inflateLimited(bytes)) : new TextDecoder().decode(bytes)",
    ].join("\n"),
  },
  {
    id: "fix-analytic-gzip-cap", worker: "www", file: F,
    find: "} catch {\n\t\treturn reply({ error: 'invalid JSON body' }, 400, origin, false)\n\t}",
    replace: "} catch (err) {\n\t\tif (err === BODY_TOO_LARGE) return reply({ error: 'body too large' }, 413, origin, false)\n\t\treturn reply({ error: 'invalid JSON body' }, 400, origin, false)\n\t}",
  },
  // POST /analytic/sites 의 사전 요청(Access-Control-Request-Method: POST)이 수집용 CORS 를 받아(Allow-Origin *, Allow-Headers 에
  // x-pd-* · x-oauth-* 없음) 브라우저에서 주인 등록이 막혔다. 수집용은 /analytic/events 의 POST 사전 요청만
  {
    id: "fix-analytic-sites-preflight", worker: "www", file: F,
    find: "cors(origin, ownerCall && request.headers.get('Access-Control-Request-Method') !== 'POST')",
    replace: "cors(origin, url.pathname === ANALYTIC_SITES_PATH || request.headers.get('Access-Control-Request-Method') !== 'POST')",
  },
  // 호스트가 빈 주소("https://a@/" · "https://:80/")는 cleanHref 를 지나 hostOf 가 null — Origin 이 없으면 hashId(null) 이 TypeError 로 500.
  // 호스트 없는 주소의 사건은 모양이 틀린 것(400)
  {
    id: "fix-analytic-empty-host", worker: "www", file: F,
    find: "if (!href || !sid) return null",
    replace: "if (!href || !sid || !hostOf(href)) return null",
  },
  // sid 를 64 글자로 자른 곳이 서로게이트 쌍의 가운데면 짝 없는 서로게이트가 hashId(cc + sid)(ethers toUtf8Bytes)에서 TypeError → 500.
  // 그런 sid 는 모양이 틀린 것(400)
  {
    id: "fix-analytic-sid-surrogate", worker: "www", file: F,
    find: "if (!href || !sid ||",
    replace: "if (!href || !sid || !sid.isWellFormed() ||",
  },
  // 읽기의 limit 이 정수가 아니면(limit=2.5) 그대로 LIMIT ?4 에 묶여 SQLite 가 거절 → 500. 내림한다
  {
    id: "fix-analytic-list-limit", worker: "www", file: F,
    find: "Math.max(1, Number(url.searchParams.get('limit') ?? DEFAULT_LIST) || DEFAULT_LIST)",
    replace: "Math.max(1, Math.floor(Number(url.searchParams.get('limit') ?? DEFAULT_LIST) || DEFAULT_LIST))",
  },
];
