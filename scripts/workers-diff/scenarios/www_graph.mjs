// graph.ts — /graph · /graph/local · /graph/similar · /graph/view (공개, 읽기 제한기). graph.test.ts 의 단언을 모두 옮겼고(`covers`),
// SCENARIOS.md 의 GR · R-06~08 · RL-04 와 premium-discount-mcp(crates/premium-discount-mcp/src/graph.rs)가 보내는 요청을 그대로 보낸다.
//
// 가짜 Vectorize: 시험대의 기본 getByIds 는 values 를 돌려주지 않고 query 는 늘 빈 결과라 similar 를 시험할 수 없다. 그래서 이 파일의
// `vecStore()` 를 시나리오의 `vec` 칸(시험대가 원래 받는 시나리오별 답)으로 넘긴다: 씨앗 `vectors: { id: { values, metadata } }` 를
// 저장소로 쓰고, getByIds 는 values 까지 돌려주며(운영 Vectorize 처럼 숫자 배열), query 는 받은 벡터의 요약(형 · 길이 · Float64 FNV)과
// 같은 저장 벡터를 찾아 코사인으로 순위를 매긴다(filter 는 metadata 의 같은 값, topK 기본 5). 공용 파일(www_mocks_src.mjs · www.mjs)은
// 바꾸지 않았다 — 다른 시나리오의 Vectorize 동작 · 기록은 그대로다.
import { IP, PARENT_ID, AUTHOR, evaluation, unitVector } from "./www_common.mjs";

const M = "graph";
const READ = { EVAL_READ_LIMITER: true };
const SECRET = { AUTHOR_KEY_SECRET: "k" };
const get = (path, extra = {}) => ({ path, headers: { "cf-connecting-ip": IP, ...extra } });
// premium-discount-mcp 의 GraphClient: ureq 2.12.1(기본 기능)의 GET — 머리글은 ureq 가 붙이는 것뿐이고, cf-connecting-ip 는 Cloudflare 가 붙인다
const mcp = (path) => ({ path, headers: { "User-Agent": "ureq/2.12.1", Accept: "*/*", "accept-encoding": "gzip", "cf-connecting-ip": IP } });

// graph.test.ts 의 id 들: 스레드 하나(ROOT ← A, B, C)와 따로 떨어진 평가 하나. ROOT 는 판정 행이 없다
const ROOT = "0190f0a0-0000-7000-8000-0000000000a0";
const A = "0190f0a0-0000-7000-8000-0000000000a1";
const B = "0190f0a0-0000-7000-8000-0000000000a2";
const C = "0190f0a0-0000-7000-8000-0000000000a3";
const LONE = "0190f0a0-0000-7000-8000-0000000000b0";
const MISSING = "0190f0a0-0000-7000-8000-0000000000ff";

/** seedVerdict (fakes.ts) — eval_verdicts 행 */
function verdict(v) {
  const at = v.updated_at ?? 1_000;
  return [
    `INSERT INTO eval_verdicts ("eval_id", "cc", "to", "evidence", "value", "up", "down", "level", "rank", "tier", "grade", "tier_since", "reasons", "recipe", "updated_at", "judged_at")
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?12, ?12)`,
    v.eval_id, v.cc ?? "example.com", v.to ?? null, v.evidence ?? 0, v.value ?? 0, v.up ?? 0, v.down ?? 0, v.level ?? null, v.rank ?? null,
    v.tier, v.grade ?? null, at, JSON.stringify(v.reasons ?? []), v.recipe ?? "pd-verdict/1",
  ];
}

// graph.test.ts 의 seedGraph
const SEED_GRAPH = [
  evaluation({ id: ROOT, created_at: 1 }),
  evaluation({ id: A, ref: ROOT, root: ROOT, created_at: 2 }),
  evaluation({ id: B, ref: ROOT, root: ROOT, created_at: 3 }),
  evaluation({ id: C, ref: ROOT, root: ROOT, created_at: 4 }),
  evaluation({ id: LONE, created_at: 5 }),
  verdict({ eval_id: A, tier: "premium", grade: 2 }),
  verdict({ eval_id: B, tier: "discount", grade: -1 }),
  verdict({ eval_id: C, tier: "neutral", grade: 0 }),
  verdict({ eval_id: LONE, tier: "premium", grade: 1 }),
];

// ---- 값을 가진 가짜 Vectorize (위 머리말) ----
function fnv(u8) {
  let h = 2166136261;
  for (const b of u8) h = Math.imul(h ^ b, 16777619) >>> 0;
  return h.toString(16).padStart(8, "0");
}
// mocks_src.mjs 의 vectorSummary 가 배열에 쓰는 것과 같은 요약
const summaryFnv = (values) => fnv(new Uint8Array(new Float64Array(values).buffer));
function cosine(a, b) {
  let dot = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) {
    dot += a[i] * b[i];
    na += a[i] * a[i];
    nb += b[i] * b[i];
  }
  return na && nb ? dot / Math.sqrt(na * nb) : 0;
}
/** 저장 벡터의 values 를 돌려주는 getByIds 와 코사인 query. over: { getByIds?, query? } 로 한쪽만 바꿀 수 있다 */
export function vecStore(over = {}) {
  return {
    getByIds: ({ ids }, store) => ids.filter((id) => store.has(id)).map((id) => ({ id, values: store.get(id).values, metadata: store.get(id).metadata })),
    query: ({ vector, options }, store) => {
      const q = vector && vector.tag === "[object Array]"
        ? [...store.values()].find((v) => Array.isArray(v.values) && v.values.length === vector.len && summaryFnv(v.values) === vector.fnv)
        : null;
      if (!q) return { matches: [], count: 0 };
      const filter = (options && options.filter) || {};
      const scored = [...store]
        .filter(([, v]) => Array.isArray(v.values) && v.values.length === q.values.length && Object.entries(filter).every(([k, x]) => (v.metadata || {})[k] === x))
        .map(([id, v]) => ({ id, score: cosine(q.values, v.values) }))
        .sort((a, b) => b.score - a.score || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
      const matches = scored.slice(0, (options && options.topK) ?? 5);
      return { matches, count: matches.length };
    },
    ...over,
  };
}
/** 정해 둔 matches 를 돌려주는 query (fakes.ts 의 vec.matches 와 같다: topK 로 자르고 필터는 흉내 내지 않는다) */
const fixedMatches = (matches) => ({ options }) => {
  const m = matches.slice(0, (options && options.topK) ?? 5);
  return { matches: m, count: m.length };
};
/** 이 실행(저장소)에서 몇 번째 query 인가 (1부터) — 실행마다 시험대가 새 저장소를 만든다. 묻는 topK 로 가르지 않으려고 쓴다
 * (fix-graph-similar-overfetch 가 topK 를 바꾼다) */
const QUERIES = new WeakMap();
const queryCount = (store) => {
  const n = (QUERIES.get(store) ?? 0) + 1;
  QUERIES.set(store, n);
  return n;
};
const firstQuery = (store) => queryCount(store) === 1;
/** 작은 차원의 벡터: 축 i 에 가까운 정도를 다르게 */
const v4 = (...xs) => xs;

// similar 시나리오의 작은 저장소: A 와 가까운 차례 LONE > B > C > ROOT, 다른 cc 의 X 는 A 와 똑같지만 filter 로 빠진다
const OTHER = "0190f0a0-0000-7000-8000-0000000000c0";
const SMALL_VECTORS = {
  [A]: { values: v4(1, 0, 0, 0), metadata: { cc: "example.com", root: ROOT } },
  [LONE]: { values: v4(0.9, 0.1, 0, 0), metadata: { cc: "example.com", root: LONE } },
  [B]: { values: v4(0.7, 0.3, 0.1, 0), metadata: { cc: "example.com", root: ROOT } },
  [C]: { values: v4(0.2, 0.9, 0.1, 0.3), metadata: { cc: "example.com", root: ROOT } },
  [ROOT]: { values: v4(-0.5, 0.5, 0.5, 0.5), metadata: { cc: "example.com", root: ROOT } },
  [OTHER]: { values: v4(1, 0, 0, 0), metadata: { cc: "other.com", root: OTHER } },
};

// MCP 시나리오의 씨앗: app:// 자리(code.exe)의 스레드 둘, 작성자 셋(대소문자만 다른 주소는 같은 작성자 키)
const MCP_IDS = Array.from({ length: 6 }, (_, i) => `0190f0a1-0000-7000-8000-00000000000${i}`);
const MCP_SEED = [
  evaluation({ id: MCP_IDS[0], cc: "code.exe", from: AUTHOR, created_at: 10 }),
  evaluation({ id: MCP_IDS[1], cc: "code.exe", from: AUTHOR.toUpperCase().replace("0X", "0x"), ref: MCP_IDS[0], root: MCP_IDS[0], created_at: 11 }),
  evaluation({ id: MCP_IDS[2], cc: "code.exe", from: "0x00000000000000000000000000000000000000cd", ref: MCP_IDS[1], root: MCP_IDS[0], created_at: 12 }),
  evaluation({ id: MCP_IDS[3], cc: "code.exe", from: "guest:abc", created_at: 13 }),
  evaluation({ id: MCP_IDS[4], cc: "code.exe", from: "0x00000000000000000000000000000000000000cd", ref: MCP_IDS[3], root: MCP_IDS[3], created_at: 14 }),
  evaluation({ id: MCP_IDS[5], cc: "example.com", created_at: 15 }),
  verdict({ eval_id: MCP_IDS[0], cc: "code.exe", tier: "premium", grade: 2, rank: 0.8, evidence: 4.5 }),
  verdict({ eval_id: MCP_IDS[1], cc: "code.exe", tier: "unrated", grade: null }),
  verdict({ eval_id: MCP_IDS[3], cc: "code.exe", tier: "discount", grade: -2 }),
];
const MCP_VECTORS = Object.fromEntries(
  MCP_IDS.slice(0, 5).map((id, i) => [id, { values: unitVector(i + 2).slice(0, 16), metadata: { cc: "code.exe", root: i < 3 ? MCP_IDS[0] : MCP_IDS[3] } }])
);

// 200건 넘는 도메인 · 스레드 (GR-03 · GR-06): BIG_ROOT 를 뿌리로 한 205건, 앞 세 건(가장 오래된 것)이 부모
const BIG_ROOT = "0190f0a2-0000-7000-8000-000000000000";
const bigId = (i) => `0190f0a2-0000-7000-8000-${String(i).padStart(12, "0")}`;
const BIG_SEED = Array.from({ length: 205 }, (_, i) =>
  i === 0 ? evaluation({ id: BIG_ROOT, cc: "big.example", created_at: 100 }) : evaluation({ id: bigId(i), cc: "big.example", ref: bigId(i % 3), root: BIG_ROOT, created_at: 100 + i, from: `0x${String(i % 4).padStart(40, "0")}` })
);

// fix-graph-local-window 의 스레드들
// DEEP: 뿌리 DEEP(0) ← 사슬 DEEP(1..30)(각각 앞 것의 답글, 가장 오래됨) + 뿌리에 단 답글 F(1..220)(더 새것, 두 건씩 같은 시각) = 251건
const deepId = (i) => `0190f0a4-0000-7000-8000-${String(i).padStart(12, "0")}`;
const fillId = (j) => `0190f0a4-0000-7000-8001-${String(j).padStart(12, "0")}`;
const DEEP_SEED = [
  ...Array.from({ length: 31 }, (_, i) =>
    evaluation({ id: deepId(i), cc: "deep.example", ref: i ? deepId(i - 1) : null, root: deepId(0), created_at: 1_000 + i, from: `0x${String(i % 3).padStart(40, "0")}` })),
  ...Array.from({ length: 220 }, (_, k) =>
    evaluation({ id: fillId(k + 1), cc: "deep.example", ref: deepId(0), root: deepId(0), created_at: 2_000 + Math.floor((k + 1) / 2), from: `0x${String(k % 5).padStart(40, "0")}` })),
  verdict({ eval_id: deepId(30), cc: "deep.example", tier: "premium", grade: 2 }),
  verdict({ eval_id: deepId(0), cc: "deep.example", tier: "discount", grade: -1 }),
];
// LINE: 한 줄로 이어진 205건 (LINE(i) 는 LINE(i-1) 의 답글) — 사슬이 200보다 길다
const lineId = (i) => `0190f0a5-0000-7000-8000-${String(i).padStart(12, "0")}`;
const LINE_SEED = Array.from({ length: 205 }, (_, i) =>
  evaluation({ id: lineId(i), cc: "line.example", ref: i ? lineId(i - 1) : null, root: lineId(0), created_at: 5_000 + i }));
// 경계: 200건 스레드와 201건 스레드, 시각이 모두 같다(같은 시각의 차례가 원본과 같은지 — 200건은 원본 그대로여야 한다)
const edgeId = (t, i) => `0190f0a6-0000-7000-800${t}-${String(i).padStart(12, "0")}`;
const EDGE_SEED = [200, 201].flatMap((n, t) =>
  Array.from({ length: n }, (_, i) => evaluation({ id: edgeId(t, i), cc: "edge.example", ref: i ? edgeId(t, (i - 1) % 7) : null, root: edgeId(t, 0), created_at: 9_000 })));

// fix-graph-similar-phantom: D1 에 행이 없는 결과 둘(하나는 판정 행만 남은 고아)
const GHOST = "0190f0a0-0000-7000-8000-0000000000e1";
const GHOST_RATED = "0190f0a0-0000-7000-8000-0000000000e2";
const GHOST2 = "0190f0a0-0000-7000-8000-0000000000e3";
const GHOST3 = "0190f0a0-0000-7000-8000-0000000000e4";

export default [
  // ---- graph.test.ts ----
  {
    name: "graph_rate_limited", module: M,
    covers: ["graph.test.ts: 그래프 JSON API는 읽기 제한에 걸리면 429다 (D1·Vectorize를 조회하지 않는다) — Retry-After 60, 키는 IP", "SCENARIOS RL-04"],
    bindings: SECRET, limits: { EVAL_READ_LIMITER: false }, sql: SEED_GRAPH, vectors: SMALL_VECTORS, vec: vecStore(),
    requests: [get("/graph?cc=example.com"), get(`/graph/local?id=${PARENT_ID}`), get(`/graph/similar?id=${PARENT_ID}`)],
  },
  {
    name: "graph_empty_domain", module: M,
    covers: ["graph.test.ts: 제한 안에서는 그대로 응답한다 — 빈 그래프 { nodes: [], edges: [], summary: 0 · 0 · 0 · 0 }"],
    bindings: SECRET, limits: READ,
    requests: [get("/graph?cc=example.com")],
  },
  {
    name: "graph_domain_tiers", module: M,
    covers: [
      "graph.test.ts: /graph: 평가 노드에 tier·grade가 붙고(판정 행이 없으면 unrated, null), summary는 평가 노드의 등급별 개수다 · 도메인·작성자 노드에는 등급이 없다 · 답글 연결 3",
      "SCENARIOS GR-01 · GR-04 · GR-05 · R-06 · DB-09",
    ],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    requests: [get("/graph?cc=example.com")],
  },
  {
    name: "graph_local_thread", module: M,
    fixes: ["fix-graph-local-window"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["graph.test.ts: /graph/local: 스레드의 평가만 센다", "SCENARIOS GR-07"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    requests: [get(`/graph/local?id=${B}`), get(`/graph/local?id=${LONE}`)],
  },
  {
    name: "graph_similar_tiers", module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["graph.test.ts: /graph/similar: 유사 노드에도 tier·grade가 붙고 summary는 유사 노드만 센다 · 자기 자신(A)은 빠진다 · 엣지 차례 LONE, ROOT, B"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    vectors: { [A]: { values: unitVector(2) } },
    vec: vecStore({ query: fixedMatches([A, LONE, ROOT, B].map((id, i) => ({ id, score: [1, 0.9, 0.8, 0.7][i] }))) }),
    requests: [get(`/graph/similar?id=${A}`)],
  },
  {
    name: "graph_view_not_limited", module: M,
    covers: [
      "graph.test.ts: /graph/view 페이지는 제한 대상이 아니다 (200, text/html, 제한기를 부르지 않음)",
      "graph.test.ts: 뷰의 인라인 스크립트는 문법이 맞고, 로컬 원문 오버레이가 들어 있다 — 본문 바이트가 원본과 같다",
      "SCENARIOS R-08 · GR-15",
    ],
    limits: { EVAL_READ_LIMITER: false },
    requests: [get("/graph/view"), get("/graph/view?cc=example.com")],
  },

  // ---- premium-discount-mcp 의 요청 (pd_graph_inspect) ----
  {
    name: "graph_mcp_domain", module: M,
    covers: ["premium-discount-mcp: query_of(cc) → GET /graph?cc=code.exe (app:// 자리) — 작성자 키 · 스레드 · tier 분포를 summarize 가 읽는다"],
    bindings: SECRET, limits: READ, sql: MCP_SEED,
    requests: [mcp("/graph?cc=code.exe"), mcp("/graph?cc=example.com"), mcp("/graph?cc=store.steampowered.com")],
  },
  {
    name: "graph_mcp_local", module: M,
    fixes: ["fix-graph-local-window"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["premium-discount-mcp: mode local → GET /graph/local?id=<소문자 UUID> (가운데 평가 · 답글의 답글)"],
    bindings: SECRET, limits: READ, sql: MCP_SEED,
    requests: [mcp(`/graph/local?id=${MCP_IDS[2]}`), mcp(`/graph/local?id=${MCP_IDS[3]}`)],
  },
  {
    name: "graph_mcp_similar", module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["premium-discount-mcp: mode similar → GET /graph/similar?id=…&topK=5 · top_k 없으면 10 · 1 · 49 (서버와 같은 검사 범위)"],
    bindings: SECRET, limits: READ, sql: MCP_SEED, vectors: MCP_VECTORS, vec: vecStore(),
    requests: [mcp(`/graph/similar?id=${MCP_IDS[0]}&topK=5`), mcp(`/graph/similar?id=${MCP_IDS[0]}&topK=10`), mcp(`/graph/similar?id=${MCP_IDS[3]}&topK=1`), mcp(`/graph/similar?id=${MCP_IDS[3]}&topK=49`)],
  },
  {
    name: "graph_mcp_errors", module: M,
    covers: ["premium-discount-mcp: 서버 오류는 본문의 error 를 읽는다 — 429 'rate limit exceeded' · 404 'not found' · 404 'vector not found'"],
    bindings: SECRET, limits: { EVAL_READ_LIMITER: (n) => n !== 1 }, sql: MCP_SEED, vectors: {}, vec: vecStore(),
    requests: [mcp(`/graph/local?id=${MCP_IDS[0]}`), mcp(`/graph/local?id=${MISSING}`), mcp(`/graph/similar?id=${MCP_IDS[0]}&topK=10`), mcp(`/graph/similar?id=${MISSING}&topK=10`)],
  },

  // ---- 가장자리 ----
  {
    name: "graph_bad_params", module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS GR-02 (cc 형식 오류 400) · GR-09 (id 형식 오류 400) · GR-11 (topK 0 · 50 → 400, 최대 49)"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH, vectors: SMALL_VECTORS, vec: vecStore(),
    requests: [
      get("/graph"), get("/graph?cc="), get("/graph?cc=bad"), get("/graph?cc=exa_mple.com"), get(`/graph?cc=${"a".repeat(61)}.com`),
      get("/graph/local"), get("/graph/local?id=abc"), get(`/graph/local?id=${A}x`),
      get("/graph/similar"), get("/graph/similar?id=abc&topK=0"),
      ...["0", "50", "1.5", "", "abc", "-1", "Infinity"].map((k) => get(`/graph/similar?id=${A}&topK=${k}`)),
      // 1 · 49 · 49.0 · 0x10 · 1e1 은 통과 (Number(raw) 가 1~49 의 정수)
      ...["1", "49", "49.0", "0x10", "1e1"].map((k) => get(`/graph/similar?id=${A}&topK=${k}`)),
    ],
  },
  {
    name: "graph_query_decoding", module: M,
    fixes: ["fix-graph-local-window", "fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // searchParams.get: + 는 공백 · %xx 풀기 · 처음 것. parseDomain 은 trim · 소문자, parseUuid 는 대문자 UUID 를 소문자로
    bindings: SECRET, limits: READ, sql: SEED_GRAPH, vectors: SMALL_VECTORS, vec: vecStore(),
    requests: [
      get("/graph?cc=+Ex%61mple.COM+"), get("/graph?cc=example.com&cc=bad"), get("/graph?cc=bad&cc=example.com"), get("/graph?CC=example.com"),
      get(`/graph/local?id=${B.toUpperCase()}`), get(`/graph/local?id=%20${B}`), get(`/graph/similar?id=${A.toUpperCase()}&topK=+2+`),
      get(`/graph/similar?topK=3&id=${A}&topK=60`), get(`/graph?cc=%E2%82%AC.com`), get(`/graph?cc=%FF.com`),
    ],
  },
  {
    name: "graph_methods_and_paths", module: M,
    covers: ["SCENARIOS R-07 · GR-16 (POST /graph → 405, 제한기보다 먼저)", "그래프 경로가 아니면 null → 레거시(418)"],
    bindings: SECRET, limits: { EVAL_READ_LIMITER: false }, sql: SEED_GRAPH,
    requests: [
      { ...get("/graph?cc=example.com"), method: "POST", body: "{}" }, { ...get("/graph/view"), method: "PUT", body: "x" },
      { ...get("/graph/local"), method: "DELETE" }, { ...get("/graph/similar"), method: "OPTIONS" }, { ...get("/graph"), method: "HEAD" },
      { ...get("/graph"), method: "PATCH", body: "x" },
      get("/graph/"), get("/graph/view/"), get("/graphs"), get("/GRAPH"), get("/graph/x"), get("/graph/%6Cocal?id=" + A),
    ],
  },
  {
    name: "graph_local_not_found", module: M,
    covers: ["SCENARIOS GR-08 (스레드 그래프 id 없음 → 404)"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    requests: [get(`/graph/local?id=${MISSING}`)],
  },
  {
    name: "graph_similar_cosine", module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS GR-10 (유사 그래프: 자기 자신 제외, 같은 cc) · topK 기본 10 → query topK 11 · topK 2 → 3 · 49 → 50 (fix-graph-similar-overfetch 뒤로 22 · 6 · 100, 결과는 같다)"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH, vectors: SMALL_VECTORS, vec: vecStore(),
    requests: [get(`/graph/similar?id=${A}`), get(`/graph/similar?id=${A}&topK=2`), get(`/graph/similar?id=${A}&topK=49`), get(`/graph/similar?id=${C}&topK=1`)],
  },
  {
    name: "graph_similar_missing", module: M,
    fixes: ["fix-graph-similar-overfetch"],  // ORIGINAL=raw 에서는 Vectorize query 의 topK 만 다르다 (응답 · D1 · 끝 상태는 같다)
    covers: ["SCENARIOS GR-12 (D1 행은 있는데 벡터 없음 → 404 vector not found)", "평가 행이 없으면 Vectorize 를 부르지 않고 404 not found"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    vectors: { [B]: { metadata: { cc: "example.com" } }, [C]: { values: null }, [ROOT]: { values: [] } },
    vec: vecStore(),
    // A: 저장소에 없음 · B: values 없음 · C: values null · ROOT: 빈 배열은 참이라 빈 벡터로 query · MISSING: 평가 행 없음
    requests: [get(`/graph/similar?id=${A}`), get(`/graph/similar?id=${B}`), get(`/graph/similar?id=${C}`), get(`/graph/similar?id=${ROOT}`), get(`/graph/similar?id=${MISSING}`)],
  },
  {
    name: "graph_similar_phantom", module: M,
    fixes: ["fix-graph-similar-phantom"],
    // 원본은 D1 행 없는 벡터(MISSING)를 판정 없는(unrated) 노드로 냈다 — 고친 판은 그 결과를 빼고 남은 둘만
    covers: ["SCENARIOS GR-13 (벡터는 있는데 D1 행이 없음 → 고친 판은 노드에서 뺀다)"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    vectors: { ...SMALL_VECTORS, [MISSING]: { values: v4(0.95, 0.05, 0, 0), metadata: { cc: "example.com" } } },
    vec: vecStore(),
    requests: [get(`/graph/similar?id=${A}&topK=3`)],
  },
  {
    name: "graph_similar_empty", module: M,
    // 자기 자신만 나오면 빈 그래프이고 판정 조회(IN)를 하지 않는다 · 같은 id 가 여러 번 · 대문자 id 는 자기 자신이 아니다
    // (fix-graph-similar-phantom 뒤로 대문자 id 는 D1 에 그 글의 행이 없어 빠진다)
    fixes: ["fix-graph-similar-phantom"],
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    vectors: { [A]: { values: v4(1, 0, 0, 0), metadata: { cc: "example.com" } } },
    // 첫 요청(topK 기본)은 자기 자신만 — 요청 차례로 가른다 (fix-graph-similar-overfetch 가 묻는 topK 를 바꾸므로 topK 로 가르지 않는다)
    vec: vecStore({
      query: (b, store) =>
        firstQuery(store)
          ? { matches: [{ id: A, score: 1 }], count: 1 }
          : { matches: [{ id: A.toUpperCase(), score: 0.99 }, { id: B, score: 0.5 }, { id: B, score: 0.5 }, { id: A, score: 1 }, { id: C }], count: 5 },
    }),
    requests: [get(`/graph/similar?id=${A}`), get(`/graph/similar?id=${A}&topK=4`)],
  },
  {
    name: "graph_secret_missing", module: M,
    fixes: ["fix-graph-local-window", "fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS GR-14 · CF-05 (AUTHOR_KEY_SECRET 없음 → 500 internal error, 행이 없으면 200 · similar 는 비밀을 쓰지 않음)"],
    bindings: { AUTHOR_KEY_SECRET: undefined }, limits: READ, sql: SEED_GRAPH, vectors: SMALL_VECTORS, vec: vecStore(),
    requests: [get("/graph?cc=example.com"), get(`/graph/local?id=${A}`), get("/graph?cc=nobody.example"), get(`/graph/similar?id=${A}&topK=2`)],
  },
  {
    name: "graph_secret_empty", module: M,
    bindings: { AUTHOR_KEY_SECRET: "" }, limits: READ, sql: SEED_GRAPH,
    requests: [get("/graph?cc=example.com")],
  },
  {
    name: "graph_limit_200", module: M,
    // 원본의 스레드는 오래된 200건(ASC)이라 고른 bigId(204) 가 응답에 없었다 — 고친 판은 그것과 뿌리(부모) + 가장 새 198건
    fixes: ["fix-graph-local-window"],
    covers: ["SCENARIOS GR-03 (200건 초과 → 최신 200건만, DESC) · GR-06 (부모가 응답에 없으면 reply 없음) · 스레드는 고른 평가 · 조상 + 가장 새 것"],
    bindings: SECRET, limits: READ, sql: BIG_SEED,
    requests: [get("/graph?cc=big.example"), get(`/graph/local?id=${bigId(204)}`)],
  },
  {
    name: "graph_rate_limiter_variants", module: M,
    covers: ["SCENARIOS RL-05 (제한기 없음 · IP 없음 · 제한기 고장 → 통과)"],
    bindings: SECRET, sql: SEED_GRAPH,
    // 1: 던짐(통과) · 2: success 없음(막힘) · 3: success true · 4: IP 머리글 없음(제한기를 부르지 않음)
    limits: { EVAL_READ_LIMITER: (n) => (n === 1 ? "throw" : n === 2 ? {} : { success: true }) },
    requests: [get("/graph?cc=example.com"), get("/graph?cc=example.com"), get("/graph?cc=example.com"), { path: "/graph?cc=example.com" }],
  },
  {
    name: "graph_no_limiter", module: M,
    bindings: SECRET, sql: SEED_GRAPH,
    requests: [get("/graph?cc=example.com")],
  },
  {
    name: "graph_d1_faults", module: M,
    fixes: ["fix-graph-local-window", "fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // D1 이 던지면 catch 가 잡아 console 'graph error <err>' · 500
    bindings: SECRET, limits: READ, sql: SEED_GRAPH, vectors: SMALL_VECTORS, vec: vecStore(),
    faults: [
      { match: "LEFT JOIN eval_verdicts", nth: 1 },
      { match: `SELECT "root" FROM evaluations`, nth: 1 },
      { match: "LEFT JOIN eval_verdicts", nth: 2, message: "D1_ERROR: thread read failed" },
      { match: `SELECT "cc" FROM evaluations`, nth: 1 },
      // 판정 요약 문장 (원본 · fix-graph-similar-phantom 뒤 모두에 있는 글)
      { match: `"grade", "rank", "evidence" FROM `, nth: 1 },
    ],
    requests: [
      get("/graph?cc=example.com"), get(`/graph/local?id=${B}`), get(`/graph/local?id=${B}`), get(`/graph/similar?id=${A}&topK=2`), get(`/graph/similar?id=${A}&topK=2`),
      get(`/graph/similar?id=${A}&topK=2`),
    ],
  },
  {
    name: "graph_vectorize_faults", module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // getByIds · query 가 던지거나 이상한 모양을 돌려줄 때 — 모두 try 안이라 500 (console 에 원본의 TypeError 글)
    bindings: SECRET, limits: READ, sql: SEED_GRAPH, vectors: SMALL_VECTORS,
    vec: (() => {
      const base = vecStore();
      // 답의 차례는 실행(ts · rs)마다 처음부터: 시험대가 실행마다 새로 만드는 저장소(store)에 매단다
      const seen = new WeakMap();
      const step = (store, k) => {
        const c = seen.get(store) || { g: 0, q: 0 };
        seen.set(store, c);
        return c[k]++;
      };
      const gets = [() => ({ fail: "vectorize unavailable" }), () => null, () => ({}), () => "abc", () => [[0.5, 0.5]], (b, s) => base.getByIds(b, s)];
      // 알려진 차이: matches 의 id 가 글이 아니면(예: [7]) 원본은 그 값을 그대로 묶어 D1 이 던지고 Rust 는 글로 묶는다 — Vectorize 의 id 는 늘 글이라 넣지 않았다
      const queries = [() => ({ fail: "query timeout" }), () => null, () => ({}), () => ({ matches: "x" }), () => ({ matches: [null] }), (b, s) => base.query(b, s)];
      return {
        getByIds: (b, s) => gets[Math.min(step(s, "g"), gets.length - 1)](b, s),
        query: (b, s) => queries[Math.min(step(s, "q"), queries.length - 1)](b, s),
      };
    })(),
    requests: Array.from({ length: 10 }, () => get(`/graph/similar?id=${A}&topK=2`)),
  },
  {
    name: "graph_vector_values_shapes", module: M,
    fixes: ["fix-graph-similar-overfetch"],  // ORIGINAL=raw 에서는 Vectorize query 의 topK 만 다르다 (응답 · D1 · 끝 상태는 같다)
    // values 가 글 · 길이 있는 객체 · 수일 때 Array.from 의 결과가 query 로 간다 (원본 그대로)
    bindings: SECRET, limits: READ, sql: SEED_GRAPH,
    vectors: { [A]: { values: "ab" }, [B]: { values: { length: 2, 0: 0.5 } }, [C]: { values: 3 }, [ROOT]: { values: [0.25, "x", null, true] } },
    vec: vecStore(),
    requests: [get(`/graph/similar?id=${A}`), get(`/graph/similar?id=${B}`), get(`/graph/similar?id=${C}`), get(`/graph/similar?id=${ROOT}`)],
  },
  {
    name: "graph_odd_verdict_tiers", module: M,
    // eval_verdicts.tier 가 네 등급 밖이면(cron 만 쓰므로 생기지 않음) 원본 그대로 summary 에 NaN(null) 칸이 생기고 __proto__ 는 무시된다
    bindings: SECRET, limits: READ,
    sql: [
      ...["gold", "__proto__", "constructor", "1", "toString"].map((t, i) => evaluation({ id: `0190f0a3-0000-7000-8000-00000000000${i}`, cc: "odd.example", created_at: i })),
      ...["gold", "__proto__", "constructor", "1", "toString"].map((t, i) => verdict({ eval_id: `0190f0a3-0000-7000-8000-00000000000${i}`, cc: "odd.example", tier: t, grade: i })),
    ],
    requests: [get("/graph?cc=odd.example")],
  },
  {
    name: "graph_verdict_join", module: M,
    fixes: ["fix-graph-local-window"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // LEFT JOIN: 판정 행이 있으나 grade 가 NULL(unrated) · 판정 행이 다른 cc 의 사본이어도 평가의 cc 로 묶인다 · 같은 작성자의 대소문자
    bindings: SECRET, limits: READ,
    sql: [
      evaluation({ id: ROOT, created_at: 1, from: "0xABCDEF0000000000000000000000000000000001" }),
      evaluation({ id: A, ref: ROOT, root: ROOT, created_at: 2, from: "0xabcdef0000000000000000000000000000000001" }),
      evaluation({ id: B, ref: A, root: ROOT, created_at: 2, from: "guest:Zz" }),
      verdict({ eval_id: ROOT, tier: "unrated", grade: null }),
      verdict({ eval_id: A, cc: "elsewhere.com", tier: "neutral", grade: 0, rank: 0.1 }),
    ],
    requests: [get("/graph?cc=example.com"), get(`/graph/local?id=${A}`)],
  },

  // ---- fix-graph-local-window: 200건 넘는 스레드 ----
  {
    name: "graph_local_window_deep", module: M,
    fixes: ["fix-graph-local-window"],
    // 251건 스레드. DEEP(30): 사슬 31건(가장 오래된 것들)이 모두 들고 남은 169칸은 가장 새 답글 · F(220): 새것 199 + 뿌리 ·
    // F(1)(가장 오래된 답글): 그것 · 뿌리 + 가장 새 198 · 뿌리: 그것 + 가장 새 199. 같은 시각의 두 답글이 경계에 걸리면 id 가 큰 것이 먼저(DESC)
    bindings: SECRET, limits: READ, sql: DEEP_SEED,
    requests: [get(`/graph/local?id=${deepId(30)}`), get(`/graph/local?id=${fillId(220)}`), get(`/graph/local?id=${fillId(1)}`), get(`/graph/local?id=${deepId(0)}`), get(`/graph/local?id=${deepId(15)}`)],
  },
  {
    name: "graph_local_window_long_chain", module: M,
    fixes: ["fix-graph-local-window"],
    // 한 줄 205건: LINE(204) 의 사슬은 205건이라 고른 것에 가까운 200건(LINE(5..204), 뿌리는 빠진다) · LINE(100): 사슬 101 + 가장 새 99 ·
    // LINE(0): 뿌리 + 가장 새 199
    bindings: SECRET, limits: READ, sql: LINE_SEED,
    requests: [get(`/graph/local?id=${lineId(204)}`), get(`/graph/local?id=${lineId(100)}`), get(`/graph/local?id=${lineId(0)}`)],
  },
  {
    name: "graph_local_window_boundary", module: M,
    fixes: ["fix-graph-local-window"],
    // 모든 시각이 같은 200건 · 201건 스레드. 200건은 원본과 같은 행 · 같은 차례(raw 와는 SQL 글만 다르다), 201건은 고른 평가 · 조상 + 가장 새 것(id DESC)
    bindings: SECRET, limits: READ, sql: EDGE_SEED,
    requests: [get(`/graph/local?id=${edgeId(0, 199)}`), get(`/graph/local?id=${edgeId(1, 200)}`), get(`/graph/local?id=${edgeId(1, 3)}`)],
  },

  // ---- fix-graph-similar-phantom ----
  {
    name: "graph_similar_phantom_mixed", module: M,
    fixes: ["fix-graph-similar-phantom"],
    // 유령 둘(하나는 판정 행만 남음)이 저장된 결과 사이에 끼어 있다: 남은 것의 차례 · score · summary 가 맞고, 고아 판정은 되살아나지 않는다.
    // fix-graph-similar-phantom 만으로는 topK 로 자른 뒤에 빼 결과가 topK 보다 적었다 (topK 3 → 둘). fix-graph-similar-overfetch 뒤로는
    // 두 배를 받아 빼고 자르므로 topK 3 → 셋 (LONE · B · C)
    fixes: ["fix-graph-similar-overfetch"],
    bindings: SECRET, limits: READ,
    sql: [...SEED_GRAPH, verdict({ eval_id: GHOST_RATED, tier: "premium", grade: 2 })],
    vectors: { [A]: { values: unitVector(2) } },
    vec: vecStore({ query: fixedMatches([A, GHOST, LONE, GHOST_RATED, B, C, ROOT].map((id, i) => ({ id, score: 1 - i / 10 }))) }),
    requests: [get(`/graph/similar?id=${A}&topK=3`), get(`/graph/similar?id=${A}`), get(`/graph/similar?id=${A}&topK=1`)],
  },

  // ---- fix-graph-similar-overfetch ----
  {
    name: "graph_similar_overfetch", module: M,
    fixes: ["fix-graph-similar-phantom", "fix-graph-similar-overfetch"],
    // 유령 넷이 자기 자신 바로 뒤에 몰려 있다. query topK = min(2 × (topK + 1), 100), 자기 자신 · 유령을 뺀 뒤 topK 로 자른다(차례 그대로):
    // topK 3 → 8건을 받아 LONE · B · C 로 다 찬다 · topK 2 → 6건을 받아 LONE 하나(여유를 넘는 유령: 짧다) · topK 1 → 4건이 자기 자신 + 유령이라 빈 그래프 ·
    // topK 49 → 100건(상한, values · metadata 를 받지 않는 query) 을 받아 저장된 넷 전부. 자기 자신이 가운데에 있어도 빠진다
    bindings: SECRET, limits: READ,
    sql: [...SEED_GRAPH, verdict({ eval_id: GHOST_RATED, tier: "premium", grade: 2 })],
    vectors: { [A]: { values: unitVector(2) }, [B]: { values: unitVector(3) } },
    // 다섯째(B 의 similar)만 다른 차례: 유령 둘 · 자기 자신(B) · LONE · 유령 · C · ROOT — topK 기본 10 → 22건, 자기 자신 · 유령을 빼고 LONE · C · ROOT
    vec: vecStore({
      query: (b, store) =>
        fixedMatches(
          (queryCount(store) < 5 ? [A, GHOST, GHOST_RATED, GHOST2, GHOST3, LONE, B, C, ROOT] : [GHOST, GHOST2, B, LONE, GHOST3, C, ROOT])
            .map((id, i) => ({ id, score: 1 - i / 20 }))
        )(b),
    }),
    requests: [
      get(`/graph/similar?id=${A}&topK=3`),
      get(`/graph/similar?id=${A}&topK=2`),
      get(`/graph/similar?id=${A}&topK=1`),
      get(`/graph/similar?id=${A}&topK=49`),
      get(`/graph/similar?id=${B}`),
    ],
  },
];
