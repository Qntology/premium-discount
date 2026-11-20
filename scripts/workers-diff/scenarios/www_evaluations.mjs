// 2단계: evaluations.ts — /evaluations · /evaluations/{search,status,votes,supersedes,verdicts} · /public/evaluations/search.
// 옮긴 node 시험: evaluations.test.ts · votes.test.ts · supersedes.test.ts(www 쪽, cron 판정은 cron 시험대의 verdict_superseded) ·
// session.test.ts 의 마지막 단언 · SCENARIOS.md 의 R · V · GA · ID · PR · SR · RL · SS-09 (지금 설계에 남은 것).
// 시나리오 모양은 www_common.mjs 머리말. 이름은 www_evaluations_ 로 시작한다.
import { createHash } from "node:crypto";
import { ethers } from "ethers";
import { SECRETS, S3, HASH, TOKEN, IP, ORIGIN, BOTH_LIMITERS, app, creds, issued, s3json, evaluation, unitVector } from "./www_common.mjs";

const M = "evaluations";
const N = (s) => "www_evaluations_" + s;

const CLIENT_FROM = "0x1111111111111111111111111111111111111111";
const ZERO = "0x" + "0".repeat(40);
const EVAL = "0190f0a0-0000-7000-8000-0000000000a1";
const OTHER = "0190f0a0-0000-7000-8000-0000000000a2";
const PARENT = "0190f0a0-0000-7000-8000-000000000001";
const loggedIn = creds(HASH, TOKEN);
const sha = (t) => createHash("sha256").update(t, "utf8").digest("hex");
const addressFromEmail = (email) => ethers.computeAddress(ethers.hashMessage(email)).toLowerCase();
/** 웹 세션 (S3 의 token 이 일치): email 이 null 이면 객체 없음(404) */
const web = (email) => () => (email ? s3json({ token: TOKEN, email }) : { status: 404 });
const NO_S3 = { ...SECRETS, ...Object.fromEntries(Object.keys(S3).map((k) => [k, undefined])) };

const TEXT = "좋은 평가입니다";
const postBody = (over = {}) => ({ request_id: "req-0123456789abcdef", cc: "example.com", text: TEXT, text_hash: sha(TEXT), vector: unitVector(), ...over });
const post = (path, body, headers = {}) => ({ method: "POST", path, headers: { "Content-Type": "application/json", ...app(headers) }, body });
const get = (path, headers = {}) => ({ path, headers: app(headers) });
/** cf-connecting-ip 없이 */
const postNoIp = (path, body, headers = {}) => ({ method: "POST", path, headers: { "Content-Type": "application/json", Origin: ORIGIN, ...headers }, body });

/** seedQueue (fakes.ts) — 대기열 행 (벡터는 NULL: www 는 읽지 않는다). text_hash 기본 "a"×64, bcc 기본 0xbb…(칸을 주면 null 도 그대로) */
function queue(s) {
  const created = s.created_at ?? 1_000;
  return [
    `INSERT INTO eval_queue ("id", "request_id", "from", "to", "cc", "ref", "root", "text", "text_hash", "vector", "state", "attempts", "lease_until", "created_at", "updated_at", "bcc", "last_error")
     VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, NULL, ?9, 0, NULL, ?10, ?10, ?11, ?12)`,
    s.id, s.request_id ?? "req-" + s.id, s.from ?? "0x00000000000000000000000000000000000000ab", s.cc ?? "example.com", s.ref ?? null, s.root ?? s.ref ?? s.id,
    s.text ?? "대기 중인 글", s.text_hash ?? "a".repeat(64), s.state ?? "pending", created, "bcc" in s ? s.bcc : "0x" + "b".repeat(40), s.last_error ?? null,
  ];
}
/** seedVerdict (fakes.ts) — 판정 행. reasonsRaw 는 reasons 칸의 글 그대로 */
function verdict(v) {
  const at = v.updated_at ?? 1_000;
  return [
    `INSERT INTO eval_verdicts ("eval_id", "cc", "to", "evidence", "value", "up", "down", "level", "rank", "tier", "grade", "tier_since", "reasons", "recipe", "updated_at", "judged_at")
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?12, ?12)`,
    v.eval_id, v.cc ?? "example.com", v.to ?? null, v.evidence ?? 0, v.value ?? 0, v.up ?? 0, v.down ?? 0, v.level ?? null, v.rank ?? null, v.tier, v.grade ?? null, at,
    v.reasonsRaw ?? JSON.stringify(v.reasons ?? []), v.recipe ?? "pd-verdict/1",
  ];
}
/** seedVote (fakes.ts) — 표 행 */
const vote = (v) => [
  `INSERT INTO eval_votes ("id", "request_id", "eval_id", "voter", "voter_kind", "kind", "value", "created_at") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`,
  v.id, v.request_id, v.eval_id, v.voter, v.voter_kind ?? "session", v.kind ?? "vote", v.value, v.created_at ?? 1_000,
];

// ---- 검색 (votes.test.ts seedSearch) ----
const A = "0190f0a0-0000-7000-8000-00000000000a";
const B = "0190f0a0-0000-7000-8000-00000000000b";
const C = "0190f0a0-0000-7000-8000-00000000000c";
const D = "0190f0a0-0000-7000-8000-00000000000d";
const X = "0190f0a0-0000-7000-8000-0000000000ff";
// 검색 결과의 평가 행 (fix-graph-similar-phantom 뒤로 D1 에 행이 없는 결과는 빠지므로 fakes.ts 와 달리 평가 행도 심는다 — X 는 판정 행 없음)
const SEARCH_SQL = [
  ...[D, X, A, C, B].map((id, i) => evaluation({ id, to: "0x" + "2".repeat(40), created_at: 1_000 + i })),
  verdict({ eval_id: A, tier: "premium", grade: 1, rank: 0.7, evidence: 3 }),
  verdict({ eval_id: B, tier: "discount", grade: -2, rank: -0.9, evidence: 4 }),
  verdict({ eval_id: C, tier: "neutral", grade: 0, rank: 0.1, evidence: 2.5 }),
  verdict({ eval_id: D, tier: "unrated", evidence: 1, reasons: ["low_evidence", "no_baseline"] }),
];
const SCORES = [0.9, 0.8, 0.7, 0.6, 0.5];
const SEARCH_MATCHES = [D, X, A, C, B].map((id, i) => ({ id, score: SCORES[i], metadata: { cc: "example.com", root: id, to: "0x" + "2".repeat(40) } }));
/** FakeVectorize.query 와 같이: 받은 topK 만큼 (기본 5). 필터는 흉내 내지 않는다 */
const fakeQuery = (matches) => ({ query: (body) => {
  const m = matches.slice(0, body.options?.topK ?? 5);
  return { matches: m, count: m.length };
} });
const search = (path, over = {}, headers = {}) => post(path, { vector: unitVector(), ...over }, headers);

// ---- 투표 ----
const ballot = (over = {}) => ({ request_id: "vote-0123456789abcdef", id: EVAL, value: 1, ...over });
const castVote = (body, headers = {}) => post("/evaluations/votes", body, headers);
const verdicts = (ids) => get("/evaluations/verdicts?ids=" + ids);

// ---- 대체 (supersedes.test.ts seedPair) ----
const OLD = "0190f0a0-0000-7000-8000-0000000000b1";
const NEW = "0190f0a0-0000-7000-8000-0000000000b2";
const ELSEWHERE = "0190f0a0-0000-7000-8000-0000000000b3";
const OTHER_TARGET = "0190f0a0-0000-7000-8000-0000000000b4";
const B5 = "0190f0a0-0000-7000-8000-0000000000b5";
const AUTHOR = addressFromEmail("author@example.com");
const TO = "0x" + "c".repeat(40);
const PAIR_SQL = [
  evaluation({ id: OLD, from: AUTHOR, to: TO, created_at: 1_000 }),
  evaluation({ id: NEW, from: addressFromEmail("other@example.com"), to: TO, created_at: 2_000 }),
  evaluation({ id: ELSEWHERE, cc: "elsewhere.com", to: TO, created_at: 3_000 }),
  evaluation({ id: OTHER_TARGET, to: null, created_at: 3_000 }),
];
const report = (over = {}) => ({ request_id: "supersede-0123456789", old: OLD, new: NEW, ...over });
const send = (body, headers = loggedIn) => post("/evaluations/supersedes", body, headers);

const many = (n) => Array.from({ length: n }, (_, i) => `0190f0a0-0000-7000-8000-${String(i).padStart(12, "0")}`).join(",");

export default [
  // ───────────── 경로 · CORS · 메서드 ─────────────
  {
    name: N("preflight"), module: M,
    covers: ["evaluations.test.ts: 사전 요청(OPTIONS)은 x-oauth-hash·x-oauth-token 헤더와 GET을 허용한다 (제한 대상이 아니다)", "SCENARIOS R-03"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: false, EVAL_READ_LIMITER: false },
    requests: ["/evaluations", "/evaluations/search", "/public/evaluations/search", "/evaluations/status", "/evaluations/votes", "/evaluations/supersedes", "/evaluations/verdicts"].map((path) => ({
      method: "OPTIONS", path, headers: app({ "Access-Control-Request-Headers": "content-type, x-oauth-hash, x-oauth-token" }),
    })),
  },
  {
    name: N("preflight_origins"), module: M,
    covers: ["evaluations.test.ts: 허용되지 않은 출처에는 Allow-Origin을 주지 않는다", "SCENARIOS R-04"],
    requests: [
      { method: "OPTIONS", path: "/evaluations", headers: { Origin: "https://evil.example" } },
      { method: "OPTIONS", path: "/evaluations", headers: { Origin: "http://tauri.localhost" } },
      { method: "OPTIONS", path: "/evaluations" },
    ],
  },
  {
    name: N("methods_and_paths"), module: M,
    covers: ["votes.test.ts: 투표 경로의 메서드와 사전 요청 (GET 405 · OPTIONS 204 · 다른 경로는 넘긴다)", "votes.test.ts: 판정 조회는 GET만 받는다 (POST 405)", "SCENARIOS R-02 · R-09"],
    bindings: SECRETS, limits: BOTH_LIMITERS,
    requests: [
      get("/evaluations"),
      get("/evaluations/votes"),
      { method: "OPTIONS", path: "/evaluations/votes", headers: { Origin: ORIGIN } },
      get("/evaluations/vote"),
      post("/evaluations/verdicts", {}),
      post("/evaluations/status", {}),
      get("/evaluations/search"),
      get("/public/evaluations/search"),
      { method: "PUT", path: "/evaluations/supersedes", headers: app(), body: "{}" },
      { method: "DELETE", path: "/evaluations", headers: app() },
      get("/evaluations/xyz"),
      get("/evaluations/"),
      get("/EVALUATIONS"),
      get("/public/evaluations"),
      { method: "HEAD", path: "/evaluations/status?request_id=req-0123456789abcdef", headers: app() },
      { method: "PATCH", path: "/evaluations", headers: app(), body: "{}" },
    ],
  },

  // ───────────── 저장 (POST /evaluations) ─────────────
  {
    name: N("save_guest_bcc"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: 저장: 202로 접수하고 bcc를 대기열에 남긴다 (세션이 없으면 클라이언트 값을 쓴다)", "SCENARIOS V-20 · V-21(지금은 202 pending)"],
    bindings: SECRETS, limits: BOTH_LIMITERS,
    requests: [post("/evaluations", postBody({ bcc: "0x" + "AB".repeat(20), from: CLIENT_FROM }))],
  },
  {
    name: N("save_session_ignores_from"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: 검증된 세션이면 클라이언트가 보낸 from을 무시하고 세션 주소로 저장한다", "SCENARIOS AU-06"],
    bindings: SECRETS, s3: web("user@example.com"),
    requests: [post("/evaluations", postBody({ from: CLIENT_FROM }), loggedIn)],
  },
  {
    name: N("save_session_token_mismatch"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: 세션 검증에 실패하면(token 불일치) 세션 주소를 쓰지 않는다"],
    bindings: SECRETS, s3: () => s3json({ token: "ee".repeat(32), email: "user@example.com" }),
    requests: [post("/evaluations", postBody({ from: CLIENT_FROM }), loggedIn)],
  },
  {
    name: N("save_session_ignores_invalid_from"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // 세션이 확인되면 body.from 은 검사하지도 않는다 (?? 의 단락) — 잘못된 from 도 400 이 아니다
    bindings: SECRETS, s3: web("user@example.com"),
    requests: [post("/evaluations", postBody({ from: "0x12" }), loggedIn), post("/evaluations", postBody({ request_id: "req-invalid-from-0001", from: "0x12" }))],
  },
  {
    name: N("save_no_s3_guest"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: S3 설정이 없으면 세션 헤더가 있어도 기존처럼 동작한다 (S3 호출 없음, 게스트 주소)"],
    bindings: NO_S3, s3: web("user@example.com"),
    requests: [post("/evaluations", postBody(), loggedIn)],
  },
  {
    name: N("save_app_session_login"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["session.test.ts: 로그인한 앱이 저장하면 from이 로그인한 이메일에서 만든 주소다 (클라이언트 값은 무시)", "SCENARIOS SS-09"],
    bindings: SECRETS,
    s3: () => s3json({ email: "app@e.com" }),
    requests: [
      { method: "POST", path: "/session/start", headers: app() },
      {
        method: "POST", path: "/evaluations",
        headers: (prev) => ({ "Content-Type": "application/json", ...issued(0)(prev) }),
        body: { request_id: "req-login-0123456789", cc: "example.com", text: "로그인한 앱의 글", text_hash: sha("로그인한 앱의 글"), vector: unitVector(), from: CLIENT_FROM },
      },
    ],
  },
  {
    name: N("save_rate_limited"), module: M,
    covers: ["evaluations.test.ts: 저장이 제한에 걸리면 429와 Retry-After를 주고, 대기열에 아무것도 쓰지 않는다 (IP 를 키로, 읽기 제한기는 쓰지 않는다)", "SCENARIOS RL-01"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: false, EVAL_READ_LIMITER: true },
    requests: [post("/evaluations", postBody())],
  },
  {
    name: N("search_uses_read_limiter"), module: M,
    covers: ["evaluations.test.ts: 검색은 읽기 제한기를 쓰고, 저장 제한기와 서로 영향을 주지 않는다", "SCENARIOS RL-02"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: false, EVAL_READ_LIMITER: (n) => n <= 2 },
    requests: [search("/evaluations/search"), search("/public/evaluations/search"), search("/evaluations/search")],
  },
  {
    name: N("status_not_limited"), module: M,
    covers: ["evaluations.test.ts: status 조회는 제한하지 않는다 (제한이 아니라 \"없음\")", "SCENARIOS RL-03"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: false, EVAL_READ_LIMITER: false },
    requests: [{ path: "/evaluations/status?request_id=req-0123456789abcdef", headers: { "cf-connecting-ip": IP } }],
  },
  {
    name: N("limiter_absent"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: 제한기가 없거나 … 요청은 처리된다 (제한기 없음)", "SCENARIOS RL-05"],
    bindings: SECRETS,
    requests: [post("/evaluations", postBody())],
  },
  {
    name: N("limiter_no_ip"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: 제한기가 없거나, IP를 알 수 없거나 … 요청은 처리된다 (IP 없음: 제한기를 부르지 않는다)"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: false },
    requests: [postNoIp("/evaluations", postBody({ from: CLIENT_FROM }))],
  },
  {
    name: N("limiter_broken"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["evaluations.test.ts: … 제한기가 고장 나도 요청은 처리된다"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: "throw", EVAL_READ_LIMITER: { success: 0 } },
    requests: [post("/evaluations", postBody()), search("/evaluations/search")],
  },
  {
    name: N("save_validation"), module: M,
    covers: ["SCENARIOS V-01 · V-02 · V-03 · V-04 · V-05 · V-07 · V-08 · V-09 · V-11 · V-12 · V-13(지금은 필수) · V-14 · V-17 · V-19 · V-22", "evaluations.ts: 해시가 원문과 맞지 않으면 422"],
    bindings: NO_S3, limits: BOTH_LIMITERS,
    requests: [
      post("/evaluations", "{not json"),
      post("/evaluations", ""),
      post("/evaluations", "[]"),
      post("/evaluations", '"x"'),
      post("/evaluations", "null"),
      post("/evaluations", "0"),
      post("/evaluations", "true"),
      post("/evaluations", postBody({ request_id: undefined })),
      post("/evaluations", postBody({ request_id: "a".repeat(15) })),
      post("/evaluations", postBody({ request_id: "a".repeat(65) })),
      post("/evaluations", postBody({ request_id: "bad id!0123456789" })),
      post("/evaluations", postBody({ request_id: 1234567890123456 })),
      post("/evaluations", postBody({ cc: undefined })),
      post("/evaluations", postBody({ cc: "localhost" })),
      post("/evaluations", postBody({ cc: "a..b" })),
      post("/evaluations", postBody({ cc: "a".repeat(61) + ".com" })),
      post("/evaluations", postBody({ cc: 7 })),
      post("/evaluations", postBody({ text: undefined })),
      post("/evaluations", postBody({ text: " \n\t " })),
      post("/evaluations", postBody({ text: "가".repeat(2731) })),
      post("/evaluations", postBody({ text: 5 })),
      post("/evaluations", postBody({ text_hash: undefined })),
      post("/evaluations", postBody({ text_hash: sha(TEXT).toUpperCase() })),
      post("/evaluations", postBody({ text_hash: sha(TEXT).slice(1) })),
      post("/evaluations", postBody({ text_hash: sha("다른 글") })),
      post("/evaluations", postBody({ text_hash: sha(TEXT + " ") })),
      post("/evaluations", postBody({ from: "0x12" })),
      post("/evaluations", postBody({ from: 12 })),
      post("/evaluations", postBody({ from: "0x" + "aB".repeat(20) })),
      post("/evaluations", postBody({ to: "x" })),
      post("/evaluations", postBody({ bcc: "0X" + "ab".repeat(20) })),
      post("/evaluations", postBody({ vector: unitVector().slice(0, 767) })),
      post("/evaluations", postBody({ vector: [...unitVector(), 0] })),
      post("/evaluations", postBody({ vector: [] })),
      post("/evaluations", postBody({ vector: unitVector().slice(0, 384) })),
      post("/evaluations", postBody({ vector: Object.fromEntries(unitVector().map((x, i) => [i, x])) })),
      post("/evaluations", postBody({ vector: unitVector().map((x, i) => (i === 3 ? String(x) : x)) })),
      post("/evaluations", postBody({ vector: unitVector().map((x, i) => (i === 700 ? null : x)) })),
      post("/evaluations", postBody({ vector: unitVector().map((x, i) => (i === 0 ? [x] : x)) })),
      post("/evaluations", postBody({ vector: unitVector().map((x) => x * 1.02) })),
      post("/evaluations", postBody({ vector: unitVector().map((x) => x * 0.98) })),
      post("/evaluations", postBody({ vector: unitVector().map((x, i) => (i === 0 ? 1e308 : x)) })),
      post("/evaluations", postBody({ vector: "1,2,3" })),
      post("/evaluations", postBody({ ref: "abc" })),
      post("/evaluations", postBody({ ref: "" })),
      post("/evaluations", postBody({ ref: 5 })),
      post("/evaluations", postBody({ cc: undefined, vector: [1] })),
      post("/evaluations", postBody({ text_hash: "x", vector: [1] })),
    ],
  },
  {
    name: N("save_accepts_edges"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS V-06 (cc 대문자 · 공백 → 소문자) · V-10 (노름 1.005 통과) · V-16 (from 대문자 → 소문자) · V-15 (from 제로 → 게스트) · V-18 (to 제로 → NULL)"],
    bindings: NO_S3,
    requests: [
      post("/evaluations", postBody({ request_id: "req-edge-000000000001", cc: "  Example.COM ", from: "0x" + "AB".repeat(20), to: ZERO, bcc: "" })),
      post("/evaluations", postBody({ request_id: "req-edge-000000000002", vector: unitVector().map((x) => x * 1.005), from: ZERO, to: null, bcc: null })),
      post("/evaluations", postBody({ request_id: "req-edge-000000000003", text: "a".repeat(8192), text_hash: sha("a".repeat(8192)), from: ethers.getAddress("0x" + "ab".repeat(20)) })),
      post("/evaluations", postBody({ request_id: "a".repeat(64), text: "  앞뒤 공백 그대로  ", text_hash: sha("  앞뒤 공백 그대로  "), ref: null })),
      post("/evaluations", postBody({ request_id: "A-z_0123456789-_ok", topK: 3, extra: { nested: true } })),
      // 중복 키는 뒤의 것 (JSON.parse), BOM 은 request.json() 이 버린다
      post("/evaluations", `{"request_id":"req-dup-keys-00000001","cc":"wrong.com","cc":"example.com","text":${JSON.stringify(TEXT)},"text_hash":"${sha(TEXT)}","vector":${JSON.stringify(unitVector())}}`),
      post("/evaluations", "﻿" + JSON.stringify(postBody({ request_id: "req-bom-000000000001" }))),
    ],
  },
  {
    name: N("save_address_forms"), module: M,
    fixes: ["fix-eval-address-normalize"],
    // ethers.isAddress 가 받는 다른 꼴: 0x 없는 40자리 · ICAP · 0x 없는 ZeroAddress. 원본은 그 글을 소문자로만 바꿔 저장했다
    bindings: NO_S3,
    requests: [
      post("/evaluations", postBody({ request_id: "req-addr-000000000001", from: "AB".repeat(20), to: "cd".repeat(20), bcc: "XE51K1WQMAMNR3M45C495YWONVZHZJG9TWR" })),
      post("/evaluations", postBody({ request_id: "req-addr-000000000002", from: "0".repeat(40), to: "0".repeat(40) })),
      post("/evaluations", postBody({ request_id: "req-addr-000000000003", from: ethers.getAddress("0x" + "ab".repeat(20)).slice(2) })),
    ],
  },
  {
    name: N("search_to_forms"), module: M,
    fixes: ["fix-eval-address-normalize"],
    // 검색의 to 조건도 같은 꼴로 (0x 없는 주소 · ICAP · 0x 없는 ZeroAddress 는 'non-zero' 400)
    bindings: SECRETS, vec: fakeQuery([]),
    requests: [
      search("/evaluations/search", { to: "2".repeat(40) }),
      search("/evaluations/search", { to: "XE51K1WQMAMNR3M45C495YWONVZHZJG9TWR" }),
      search("/evaluations/search", { to: "0".repeat(40) }),
    ],
  },
  {
    name: N("save_invalid_utf8_body"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // 잘못된 UTF-8 바이트는 U+FFFD 로 풀린다 (request.json()). 해시도 그 글로 맞춘다
    bindings: NO_S3,
    requests: [
      post("/evaluations", new Uint8Array([...Buffer.from(JSON.stringify(postBody({ request_id: "req-utf8-000000000001", text: "AB", text_hash: sha("A�B") })).replace('"AB"', '"A\u0001B"'))].map((b) => (b === 1 ? 0xff : b)))),
    ],
  },
  {
    name: N("save_replay"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS ID-01 (같은 request_id · 같은 cc · ref → 200 같은 id, 행 1개) · ID-02 (cc 다름 409) · ID-03 (ref 다름 409)", "evaluations.ts findReplay: 저장된 평가면 status stored"],
    bindings: NO_S3,
    sql: [evaluation({ id: EVAL }), evaluation({ id: OTHER, ref: EVAL, root: EVAL })],
    requests: [
      post("/evaluations", postBody()),
      post("/evaluations", postBody()),
      post("/evaluations", postBody({ cc: "other.com" })),
      post("/evaluations", postBody({ ref: EVAL })),
      post("/evaluations", postBody({ request_id: "req-" + EVAL })),
      post("/evaluations", postBody({ request_id: "req-" + EVAL, cc: "other.com" })),
      post("/evaluations", postBody({ request_id: "req-" + OTHER, ref: EVAL.toUpperCase() })),
      post("/evaluations", postBody({ request_id: "req-" + OTHER })),
    ],
  },
  {
    name: N("save_replay_queue_states"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // 대기열에서 거부 · 실패한 항목도 같은 request_id 면 그 상태로 답한다
    bindings: NO_S3,
    // processing 항목은 같은 내용(text_hash · bcc)의 다시 보내기다 — 다른 내용은 save_replay_content (fix-eval-replay-mismatch)
    sql: [
      queue({ id: PARENT, request_id: "req-rejected-00000001", state: "rejected", last_error: "profanity" }),
      queue({ id: OTHER, request_id: "req-processing-000001", state: "processing", text: TEXT, text_hash: sha(TEXT), bcc: null }),
    ],
    requests: [post("/evaluations", postBody({ request_id: "req-rejected-00000001" })), post("/evaluations", postBody({ request_id: "req-processing-000001" }))],
  },
  {
    name: N("save_reply_rules"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS PR-01 (부모 없음 400) · PR-02 (cc 다름 400) · PR-03 (답글 root = 부모 root) · PR-04 (답글의 답글) · PR-05 (대문자 ref → 소문자)", "evaluations.ts: 대기열에서 거부되었거나 실패한 부모에는 답글을 달 수 없다"],
    bindings: NO_S3,
    sql: [
      evaluation({ id: EVAL }),
      evaluation({ id: OTHER, ref: EVAL, root: EVAL }),
      queue({ id: PARENT, state: "pending", root: PARENT }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000002", state: "rejected" }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000003", state: "failed" }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000004", state: "processing", ref: EVAL, root: EVAL }),
    ],
    requests: [
      post("/evaluations", postBody({ request_id: "req-reply-00000000001", ref: X })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000002", ref: EVAL, cc: "other.com" })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000003", ref: "0190f0a0-0000-7000-8000-000000000002" })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000004", ref: "0190f0a0-0000-7000-8000-000000000003" })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000005", ref: EVAL.toUpperCase() })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000006", ref: OTHER })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000007", ref: PARENT })),
      post("/evaluations", postBody({ request_id: "req-reply-00000000008", ref: "0190f0a0-0000-7000-8000-000000000004" })),
    ],
  },
  {
    name: N("save_guest_address"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS GA-01 (IP 없음 · from 없음 → 500, 로그) · GA-03 (같은 IP 같은 주소) · GA-04 (다른 IP 다른 주소) · GA-05 (from 이 있으면 IP 없이도)", "evaluations.ts guestAddress: cf-connecting-ip ?? X-Real-IP, trim"],
    bindings: NO_S3,
    requests: [
      postNoIp("/evaluations", postBody({ request_id: "req-guest-00000000001" })),
      postNoIp("/evaluations", postBody({ request_id: "req-guest-00000000002", from: CLIENT_FROM })),
      post("/evaluations", postBody({ request_id: "req-guest-00000000003" })),
      post("/evaluations", postBody({ request_id: "req-guest-00000000004" })),
      post("/evaluations", postBody({ request_id: "req-guest-00000000005" }), { "cf-connecting-ip": "198.51.100.4" }),
      postNoIp("/evaluations", postBody({ request_id: "req-guest-00000000006" }), { "X-Real-IP": " 198.51.100.5 " }),
      postNoIp("/evaluations", postBody({ request_id: "req-guest-00000000007" }), { "cf-connecting-ip": "  ", "X-Real-IP": "198.51.100.5" }),
      post("/evaluations", postBody({ request_id: "req-guest-00000000008" }), { "cf-connecting-ip": " " + IP + " " }),
    ],
  },
  {
    name: N("save_no_guest_salt"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS GA-02 · CF-05 (GUEST_SALT 미설정 → 500, from 이 있으면 저장)"],
    bindings: { ...NO_S3, GUEST_SALT: undefined },
    requests: [post("/evaluations", postBody()), post("/evaluations", postBody({ request_id: "req-salt-000000000002", from: CLIENT_FROM }))],
  },
  {
    name: N("save_empty_guest_salt"), module: M,
    bindings: { ...NO_S3, GUEST_SALT: "" },
    requests: [post("/evaluations", postBody())],
  },
  {
    name: N("save_insert_fails"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS RB-01 (D1 INSERT 실패 → 다시 찾아도 없으면 500)"],
    bindings: NO_S3,
    faults: [{ match: "INSERT INTO eval_queue" }],
    requests: [post("/evaluations", postBody())],
  },
  {
    name: N("save_lookup_fails"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // findReplay · findParent 의 D1 장애는 500 (console 에 오류)
    bindings: NO_S3,
    sql: [evaluation({ id: EVAL })],
    faults: [{ match: `FROM evaluations WHERE "request_id"`, nth: 1 }, { match: `FROM eval_queue WHERE "id"`, nth: 1 }],
    requests: [post("/evaluations", postBody()), post("/evaluations", postBody({ ref: X }))],
  },
  {
    name: N("save_race_same_request"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS ID-07 (같은 request_id 가 동시에: INSERT 가 유니크 제약에 걸리면 먼저 들어간 항목으로 200)"],
    bindings: NO_S3,
    // 먼저 들어간 항목은 같은 내용(같은 글의 해시 · bcc 없음)이다
    race: { match: `FROM eval_queue WHERE "request_id"`, op: "first", after: [{ sql: queue({ id: PARENT })[0], args: queue({ id: PARENT, request_id: "req-0123456789abcdef", text: TEXT, text_hash: sha(TEXT), bcc: null }).slice(1) }] },
    requests: [post("/evaluations", postBody())],
  },
  {
    name: N("save_race_conflict"), module: M,
    fixes: ["fix-eval-replay-mismatch"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    // 경합으로 들어간 항목의 cc 가 다르면 catch 안의 findReplay 가 409
    bindings: NO_S3,
    race: { match: `FROM eval_queue WHERE "request_id"`, op: "first", after: [{ sql: queue({ id: PARENT })[0], args: queue({ id: PARENT, request_id: "req-0123456789abcdef", cc: "other.com" }).slice(1) }] },
    requests: [post("/evaluations", postBody())],
  },
  {
    name: N("save_replay_content"), module: M,
    fixes: ["fix-eval-replay-mismatch"],
    covers: ["evaluations.ts findReplay 주석: 같은 request_id 로 다른 내용이면 409 — 원본은 cc · ref 만 견줘 글이 다른 평가도 처음 항목으로 200 이었다"],
    // 대기 중(pending) 항목: 같은 내용 200 · 글 · to · bcc 가 다르거나 bcc 를 빼면 409 · from 만 다르면 200(처음 항목의 from).
    // 저장된 평가(원문 · 해시 없음): bcc 가 같으면 글이 달라도 200(알 수 없음) · bcc 가 다르면 409.
    // 거부된 항목(cron 이 해시를 덮고 bcc 를 지움): 글이 달라도 원본처럼 그 상태로 200. 응답 본문의 모양은 처음 409 들과 같다
    bindings: NO_S3,
    sql: [
      evaluation({ id: EVAL }),
      [`UPDATE evaluations SET "bcc" = ?1 WHERE "id" = ?2`, "0x" + "ee".repeat(20), EVAL],
      queue({ id: PARENT, request_id: "req-masked-0000000001", state: "rejected", text_hash: "0".repeat(64), bcc: null, last_error: "profanity" }),
    ],
    requests: [
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: TO, bcc: "0x" + "dd".repeat(20) })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: TO, bcc: "0x" + "dd".repeat(20) })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: TO, bcc: "0x" + "dd".repeat(20), text: "다른 글", text_hash: sha("다른 글") })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: "0x" + "c1".repeat(20), bcc: "0x" + "dd".repeat(20) })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", bcc: "0x" + "dd".repeat(20) })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: TO, bcc: "0x" + "d1".repeat(20) })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: TO })),
      post("/evaluations", postBody({ request_id: "req-content-00000001", to: TO.toUpperCase().replace("0X", "0x"), bcc: "0x" + "DD".repeat(20), from: CLIENT_FROM })),
      post("/evaluations", postBody({ request_id: "req-" + EVAL, bcc: "0x" + "ee".repeat(20), text: "다른 글", text_hash: sha("다른 글") })),
      post("/evaluations", postBody({ request_id: "req-" + EVAL })),
      post("/evaluations", postBody({ request_id: "req-masked-0000000001", text: "다른 글", text_hash: sha("다른 글") })),
      get("/evaluations/status?request_id=req-content-00000001"),
    ],
  },
  {
    name: N("save_race_different_text"), module: M,
    fixes: ["fix-eval-replay-mismatch"],
    // 같은 request_id 가 거의 동시에 다른 글로 들어와 먼저 자리를 잡았다: catch 안의 findReplay 가 409 (원본은 cc · ref 가 같아 200 · 남의 항목)
    bindings: NO_S3,
    race: { match: `FROM eval_queue WHERE "request_id"`, op: "first", after: [{ sql: queue({ id: PARENT })[0], args: queue({ id: PARENT, request_id: "req-0123456789abcdef", text: "먼저 온 글", text_hash: sha("먼저 온 글"), bcc: null }).slice(1) }] },
    requests: [post("/evaluations", postBody())],
  },

  // ───────────── 검색 ─────────────
  {
    name: N("search_verdicts"), module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["votes.test.ts: 검색 결과마다 verdict(tier, grade, rank, evidence)가 붙고, 판정 행이 없으면 null이다 (앱 검색은 to 를 그대로, 등급 조건이 없으면 topK 그대로)", "SCENARIOS SR-01 · SR-06 · SR-14"],
    bindings: SECRETS, sql: SEARCH_SQL, vec: fakeQuery(SEARCH_MATCHES),
    requests: [search("/evaluations/search")],
  },
  {
    name: N("search_tier"), module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["votes.test.ts: 검색의 tier 조건: 세 배(최대 50)를 받아 와 D1 판정으로 거르고 topK로 자른다"],
    bindings: SECRETS, sql: SEARCH_SQL, vec: fakeQuery(SEARCH_MATCHES),
    requests: [
      search("/evaluations/search", { tier: "premium", topK: 1 }),
      search("/evaluations/search", { tier: "discount", topK: 2 }),
      search("/evaluations/search", { tier: "rated", topK: 2 }),
      search("/evaluations/search", { tier: "rated", topK: 20 }),
      search("/evaluations/search", { tier: "rated", topK: 17 }),
    ],
  },
  {
    name: N("search_public"), module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["votes.test.ts: 공개 검색도 verdict와 tier 조건을 받고, to는 내보내지 않는다", "SCENARIOS SR-02"],
    bindings: SECRETS, sql: SEARCH_SQL, vec: fakeQuery(SEARCH_MATCHES),
    requests: [search("/public/evaluations/search", { tier: "premium" }), search("/public/evaluations/search")],
  },
  {
    name: N("search_tier_invalid"), module: M,
    covers: ["votes.test.ts: tier가 premium·discount·rated가 아니면 400 (앱·공개 모두), 검증에 걸리면 Vectorize를 부르지 않는다"],
    bindings: SECRETS, sql: SEARCH_SQL, vec: fakeQuery(SEARCH_MATCHES),
    requests: ["/evaluations/search", "/public/evaluations/search"].flatMap((path) =>
      ["neutral", "unrated", "PREMIUM", "", null, 1, ["premium"], "constructor"].map((tier) => search(path, { tier }))),
  },
  {
    name: N("search_validation"), module: M,
    covers: ["SCENARIOS SR-03 (공개 검색의 to 400) · SR-04 (제로 주소 to 400) · SR-05 (topK 0 · 51 · 1.5 · \"5\") · SR-11 (384 벡터)"],
    bindings: SECRETS, vec: fakeQuery(SEARCH_MATCHES),
    requests: [
      post("/evaluations/search", "{bad"),
      post("/evaluations/search", "[]"),
      search("/evaluations/search", { vector: unitVector().slice(0, 384) }),
      search("/evaluations/search", { vector: undefined }),
      ...[0, 51, 1.5, "5", null, -1, true, 1e400].map((topK) => search("/evaluations/search", { topK })),
      search("/evaluations/search", { cc: "bad" }),
      search("/evaluations/search", { cc: null }),
      search("/evaluations/search", { root: "abc" }),
      search("/evaluations/search", { ref: null }),
      search("/public/evaluations/search", { to: "0x" + "2".repeat(40) }),
      search("/public/evaluations/search", { to: null }),
      search("/evaluations/search", { to: ZERO }),
      search("/evaluations/search", { to: "" }),
      search("/evaluations/search", { to: null }),
      search("/evaluations/search", { to: "nope" }),
      search("/evaluations/search", { tier: "premium", cc: "bad" }),
    ],
  },
  {
    name: N("search_filters"), module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    covers: ["SCENARIOS SR-07 · SR-08 · SR-09 (cc · root · ref · to 조건이 Vectorize filter 로, 그 차례) · SR-12 (결과 0건)"],
    bindings: SECRETS, sql: SEARCH_SQL,
    vec: { query: (body) => (body.options.topK === 7 ? { matches: [], count: 0 } : { matches: SEARCH_MATCHES.slice(0, 2), count: 2 }) },
    requests: [
      search("/evaluations/search", { to: "0x" + "2".repeat(40), ref: A.toUpperCase(), root: A, cc: " Example.com" }),
      search("/evaluations/search", { topK: 7, cc: "example.com" }),
      search("/public/evaluations/search", { topK: 50, root: B, tier: "rated" }),
      search("/evaluations/search", { topK: 3.0, to: "0x" + "AB".repeat(20) }),
    ],
  },
  {
    name: N("search_metadata_shapes"), module: M,
    // 메타데이터가 없거나 모양이 다른 결과 (md.x ?? null), 같은 id 가 두 번, 판정 행이 두 번 읽혀도 하나
    bindings: SECRETS, sql: SEARCH_SQL,
    vec: { query: () => ({ matches: [
      { id: A, score: 0.9 },
      { id: B, score: 0.8, metadata: null },
      { id: C, score: 0.7, metadata: "text" },
      { id: D, metadata: { cc: null, root: 0, ref: "", to: false, extra: 1 } },
      { id: A, score: 0.5, metadata: { cc: "example.com" } },
      { id: 12, score: 0.4 },
      "plain",
    ], count: 7 }) },
    requests: [search("/evaluations/search"), search("/public/evaluations/search", { tier: "rated" })],
  },
  {
    name: N("search_match_without_id"), module: M,
    // id 가 없는 결과는 undefined 를 묶어 D1 이 거절한다 → 500 (원본 그대로)
    bindings: SECRETS, sql: SEARCH_SQL,
    vec: { query: () => ({ matches: [{ id: A, score: 0.9 }, { score: 0.3 }], count: 2 }) },
    requests: [search("/evaluations/search")],
  },
  {
    name: N("search_vectorize_errors"), module: M,
    // Vectorize 장애 · 모양이 다른 답은 500 (console 에 오류 글 — TypeError 글까지 같게)
    bindings: SECRETS,
    vec: { query: (body) => {
      const k = body.options.topK;
      if (k === 1) return { fail: "vectorize unavailable" };
      if (k === 2) return {};
      if (k === 3) return { matches: null };
      if (k === 4) return { matches: { length: 1 } };
      if (k === 5) return { matches: [null] };
      if (k === 6) return { matches: "abc" };
      return { matches: [] };
    } },
    requests: [1, 2, 3, 4, 5, 6, 7].map((topK) => search("/evaluations/search", { topK })),
  },
  {
    name: N("search_verdicts_fail"), module: M,
    fixes: ["fix-graph-similar-phantom"],  // ORIGINAL=raw 에서는 D1 문장 글만 다르다 (응답 · 끝 상태는 같다)
    bindings: SECRETS, sql: SEARCH_SQL, vec: fakeQuery(SEARCH_MATCHES),
    // 판정 요약 문장 (원본 · fix-graph-similar-phantom 뒤 모두에 있는 글)
    faults: [{ match: `"grade", "rank", "evidence" FROM ` }],
    requests: [search("/evaluations/search")],
  },
  {
    name: N("search_phantom"), module: M,
    fixes: ["fix-graph-similar-phantom"],
    // D1 에 평가 행이 없는 결과(유령) 둘이 끼어 있다 — 하나는 판정 행만 남은 고아. 원본은 그대로 냈다(고아는 판정까지 붙여서).
    // 고친 판: 등급 조건보다 먼저 빼고 남은 것의 차례 · score · verdict 는 그대로. topK 로 받은 뒤에 빼므로 topK 보다 적을 수 있다
    bindings: SECRETS, sql: [...SEARCH_SQL, verdict({ eval_id: "0190f0a0-0000-7000-8000-0000000000e2", tier: "premium", grade: 2, rank: 0.99, evidence: 9 })],
    vec: fakeQuery([D, "0190f0a0-0000-7000-8000-0000000000e1", X, A, "0190f0a0-0000-7000-8000-0000000000e2", C, B].map((id, i) => ({ id, score: 1 - i / 10, metadata: { cc: "example.com", root: id } }))),
    requests: [
      search("/evaluations/search"),
      search("/evaluations/search", { topK: 7 }),
      search("/evaluations/search", { tier: "premium", topK: 1 }),
      search("/evaluations/search", { tier: "rated", topK: 2 }),
      search("/public/evaluations/search", { topK: 3 }),
    ],
  },

  // ───────────── 상태 ─────────────
  {
    name: N("status"), module: M,
    covers: ["evaluations.ts readStatus: 저장됨 · 대기 상태(pending · processing → reason null, rejected · failed → last_error) · 없음 404 · request_id 검사 400"],
    bindings: SECRETS,
    sql: [
      evaluation({ id: EVAL }),
      evaluation({ id: OTHER, ref: EVAL, root: EVAL }),
      queue({ id: PARENT, request_id: "req-pending-000000001", state: "pending", last_error: "old error" }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000002", request_id: "req-rejected-00000001", state: "rejected", last_error: "profanity" }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000003", request_id: "req-failed-0000000001", state: "failed", last_error: "vectorize: timeout" }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000004", request_id: "req-processing-000001", state: "processing", ref: EVAL, root: EVAL }),
      queue({ id: "0190f0a0-0000-7000-8000-000000000005", request_id: "req-failed-noreason01", state: "failed" }),
    ],
    requests: [
      get("/evaluations/status?request_id=req-" + EVAL),
      get("/evaluations/status?request_id=req-" + OTHER),
      get("/evaluations/status?request_id=req-pending-000000001"),
      get("/evaluations/status?request_id=req-rejected-00000001"),
      get("/evaluations/status?request_id=req-failed-0000000001"),
      get("/evaluations/status?request_id=req-processing-000001"),
      get("/evaluations/status?request_id=req-failed-noreason01"),
      get("/evaluations/status?request_id=req-missing-000000001"),
      get("/evaluations/status"),
      get("/evaluations/status?request_id="),
      get("/evaluations/status?request_id=short"),
      get("/evaluations/status?request_id=req%2Dpending%2D000000001"),
      get("/evaluations/status?x=1&request_id=req-pending-000000001&request_id=req-missing-000000001"),
      get("/evaluations/status?request_id=req+pending+000000001"),
      get("/evaluations/status?request_id=req-%ZZ-0000000000001"),
    ],
  },
  {
    name: N("status_d1_fails"), module: M,
    bindings: SECRETS,
    faults: [{ match: "FROM eval_queue" }],
    requests: [get("/evaluations/status?request_id=req-missing-000000001")],
  },

  // ───────────── 투표 ─────────────
  {
    name: N("vote_validation"), module: M,
    covers: ["votes.test.ts: 투표 입력 검증: 형식이 틀리면 400이고 아무것도 쓰지 않는다 (14가지)"],
    bindings: SECRETS, s3: web(null),
    sql: [evaluation({ id: EVAL })],
    requests: [
      ballot({ request_id: undefined }), ballot({ request_id: "short" }), ballot({ id: "abc" }), ballot({ id: undefined }),
      ballot({ value: 2 }), ballot({ value: 0.5 }), ballot({ value: "1" }), ballot({ value: true }), ballot({ value: null }), ballot({ value: undefined }),
      ballot({ kind: "like" }), ballot({ kind: 1 }), "{not json", [ballot()],
    ].map((b) => castVote(b)),
  },
  {
    name: N("vote_not_stored"), module: M,
    covers: ["votes.test.ts: 저장되지 않은 평가(없거나 대기열에만 있음)에는 404 \"evaluation not found\" (CORS 머리글, 아무것도 쓰지 않는다)"],
    bindings: SECRETS,
    sql: [queue({ id: PARENT, text: "대기 중인 글" })],
    requests: [castVote(ballot({ id: EVAL })), castVote(ballot({ id: PARENT }))],
  },
  {
    name: N("vote_guest"), module: M,
    covers: ["votes.test.ts: 게스트 투표: 202, counted false, 투표자는 게스트 주소(클라이언트 from 무시), 판정 표시가 생긴다 (대문자 id → 소문자, UUID v7)"],
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    requests: [castVote(ballot({ id: EVAL.toUpperCase(), from: CLIENT_FROM }))],
  },
  {
    name: N("vote_session"), module: M,
    covers: ["votes.test.ts: 확인된 로그인 투표는 counted true이고 투표자는 세션 주소다. vote_outcome과 비추천도 받는다"],
    bindings: SECRETS, s3: web("voter@example.com"),
    sql: [evaluation({ id: EVAL })],
    requests: [castVote(ballot({ value: -1, kind: "vote_outcome", from: CLIENT_FROM }), loggedIn)],
  },
  {
    name: N("vote_session_fails"), module: M,
    covers: ["votes.test.ts: 세션 확인에 실패하면(S3에 객체 없음) 게스트 표가 된다"],
    bindings: SECRETS, s3: web(null),
    sql: [evaluation({ id: EVAL })],
    requests: [castVote(ballot(), loggedIn)],
  },
  {
    name: N("vote_author_self"), module: M,
    covers: ["votes.test.ts: 작성자가 자기 평가에 투표하면 기록은 하지만 counted false"],
    bindings: SECRETS, s3: web("author@example.com"),
    sql: [evaluation({ id: EVAL, from: addressFromEmail("author@example.com") })],
    requests: [castVote(ballot(), loggedIn)],
  },
  {
    name: N("vote_retry_marks_again"), module: M,
    covers: ["votes.test.ts: 재시도: 같은 request_id와 같은 내용이면 200으로 처음 본문을 그대로 돌려주고, 판정 표시를 다시 남긴다 (처음 요청이 표시에 실패한 경우 — 표시 뒤에 지워 흉내)"],
    bindings: SECRETS, s3: web("voter@example.com"),
    sql: [evaluation({ id: EVAL })],
    race: { match: "INSERT OR REPLACE INTO verdict_dirty", after: [{ sql: `DELETE FROM verdict_dirty`, args: [] }] },
    requests: [castVote(ballot(), loggedIn), castVote(ballot(), loggedIn)],
  },
  {
    name: N("vote_conflicts"), module: M,
    covers: ["votes.test.ts: 같은 request_id로 내용(값·종류·평가)이 다르면 409이고 표는 그대로다 · 재시도 사이에 세션 확인이 실패해 게스트로 떨어져도 같은 요청이다 (counted 는 처음 저장한 투표자 기준)"],
    bindings: SECRETS, s3: web("voter@example.com"),
    sql: [evaluation({ id: EVAL }), evaluation({ id: OTHER })],
    requests: [
      castVote(ballot(), loggedIn),
      castVote(ballot({ value: -1 }), loggedIn),
      castVote(ballot({ value: 0 }), loggedIn),
      castVote(ballot({ kind: "vote_outcome" }), loggedIn),
      castVote(ballot({ id: OTHER }), loggedIn),
      castVote(ballot(), {}),
      castVote(ballot({ kind: null }), {}),
    ],
  },
  {
    name: N("vote_stacking"), module: M,
    covers: ["votes.test.ts: 표는 덮어쓰지 않고 쌓인다: 다른 request_id로 바꾸거나 철회(0)하면 행이 늘고 표시 시각이 늦춰진다"],
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    requests: [castVote(ballot()), castVote(ballot({ request_id: "vote-withdraw-0123456789", value: 0 })), castVote(ballot({ request_id: "vote-down-000000000001", value: -1, kind: "vote_outcome" }))],
  },
  {
    name: N("vote_rate_limit"), module: M,
    covers: ["votes.test.ts: 투표는 저장 제한기를 쓴다: 걸리면 429이고 아무것도 쓰지 않는다. 읽기 제한기와는 따로다"],
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    limits: { EVAL_WRITE_LIMITER: (n) => n > 1, EVAL_READ_LIMITER: false },
    requests: [castVote(ballot()), castVote(ballot())],
  },
  {
    name: N("vote_negative_zero"), module: M,
    // -0 === 0 이라 받는다. 응답의 value 는 JSON.stringify 로 0, 재시도도 같은 표
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    requests: [
      castVote(`{"request_id":"vote-negzero-0123456789","id":"${EVAL}","value":-0}`),
      castVote(`{"request_id":"vote-negzero-0123456789","id":"${EVAL}","value":0}`),
      castVote(`{"request_id":"vote-negzero-0123456789","id":"${EVAL}","value":-0.0,"kind":"vote"}`),
    ],
  },
  {
    name: N("vote_guest_errors"), module: M,
    // 게스트 주소를 만들 수 없으면 500 (평가 확인 뒤에). 없는 평가면 404 가 먼저다
    bindings: { ...SECRETS, GUEST_SALT: undefined }, s3: web(null),
    sql: [evaluation({ id: EVAL })],
    requests: [castVote(ballot()), castVote(ballot({ id: X })), { ...castVote(ballot()), headers: { "Content-Type": "application/json", Origin: ORIGIN } }],
  },
  {
    name: N("vote_no_ip"), module: M,
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    requests: [postNoIp("/evaluations/votes", ballot())],
  },
  {
    name: N("vote_race_same_request"), module: M,
    // 같은 request_id 의 표가 그 사이 들어오면 INSERT 가 유니크 제약에 걸리고, 먼저 들어간 표(그 투표자)로 200
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    race: { match: `FROM eval_votes WHERE "request_id"`, op: "first", after: [{ sql: vote({})[0], args: vote({ id: "vote-race-1", request_id: "vote-0123456789abcdef", eval_id: EVAL, voter: CLIENT_FROM, value: 1 }).slice(1) }] },
    requests: [castVote(ballot())],
  },
  {
    name: N("vote_race_conflict"), module: M,
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    race: { match: `FROM eval_votes WHERE "request_id"`, op: "first", after: [{ sql: vote({})[0], args: vote({ id: "vote-race-1", request_id: "vote-0123456789abcdef", eval_id: EVAL, voter: CLIENT_FROM, value: -1 }).slice(1) }] },
    requests: [castVote(ballot())],
  },
  {
    name: N("vote_d1_faults"), module: M,
    // INSERT 실패(다시 찾아도 없음) → 500, 표시 실패 → 표는 남고 500, 평가 조회 실패 → 500
    bindings: SECRETS,
    sql: [evaluation({ id: EVAL })],
    faults: [{ match: "INSERT INTO eval_votes", nth: 1 }, { match: "INSERT OR REPLACE INTO verdict_dirty", nth: 1 }, { match: `SELECT "from" FROM evaluations`, nth: 4 }],
    requests: [castVote(ballot()), castVote(ballot()), castVote(ballot()), castVote(ballot())],
  },

  // ───────────── 판정 조회 ─────────────
  {
    name: N("verdicts_order"), module: M,
    covers: ["votes.test.ts: 판정 조회: 요청 순서대로, 중복은 한 번만, 판정 행이 없으면 unrated 기본값, reasons는 배열 (RECIPE = pd-verdict/1)"],
    bindings: SECRETS,
    sql: [
      verdict({ eval_id: A, tier: "premium", grade: 2, rank: 0.9, evidence: 3, value: 0.6, up: 3, level: "secondary", reasons: ["rank_high", "level:secondary"], updated_at: 5_000 }),
      verdict({ eval_id: B, tier: "discount", grade: -1, rank: -0.6, evidence: 2.5, value: -0.4, down: 2, up: 1, level: "primary", reasons: ["rank_low", "level:primary"] }),
    ],
    requests: [verdicts([B, X.toUpperCase(), A, B].join(",")), verdicts(` ${A} , ${A.toUpperCase()},${X}`)],
  },
  {
    name: N("verdicts_validation"), module: M,
    covers: ["votes.test.ts: 판정 조회 검증: id가 없거나, 50개를 넘거나, UUID가 아니면 400. 50개까지는 된다 (ids 매개변수 없음도 400)"],
    bindings: SECRETS,
    requests: [
      ...["", ",", `${A},`, `${A},abc`, many(51), `${A},${A},` + many(49)].map(verdicts),
      get("/evaluations/verdicts"),
      verdicts(many(50)),
      verdicts(`${A},` + many(49).split(",").slice(0, 48).join(",") + `,${A}`),
    ],
  },
  {
    name: N("verdicts_read_limiter"), module: M,
    covers: ["votes.test.ts: 판정 조회는 읽기 제한기를 쓰고 GET만 받는다"],
    bindings: SECRETS, limits: { EVAL_WRITE_LIMITER: true, EVAL_READ_LIMITER: false },
    requests: [verdicts(A), post("/evaluations/verdicts", {})],
  },
  {
    name: N("verdicts_reasons_shapes"), module: M,
    // reasons 칸은 JSON.parse — 깨진 행 하나가 응답 전체를 500 으로 (V8 의 SyntaxError 글 그대로)
    bindings: SECRETS,
    sql: [
      verdict({ eval_id: A, tier: "premium", reasonsRaw: "null" }),
      verdict({ eval_id: B, tier: "neutral", reasonsRaw: "  [\"x\"]  ", recipe: "pd-verdict/2" }),
      verdict({ eval_id: C, tier: "discount", reasonsRaw: "{\"a\":1}" }),
      verdict({ eval_id: D, tier: "unrated", reasonsRaw: "[rank_high" }),
    ],
    requests: [verdicts([A, B, C].join(",")), verdicts([A, D].join(",")), verdicts(D)],
  },
  {
    name: N("verdicts_d1_fails"), module: M,
    bindings: SECRETS,
    faults: [{ match: "FROM eval_verdicts" }],
    requests: [verdicts(A)],
  },

  // ───────────── 신구 대체 ─────────────
  {
    name: N("supersede_validation"), module: M,
    covers: ["supersedes.test.ts: 검증: 형식 · 장소 · 대상 · 시각이 틀리면 400, 없는 평가는 404, 아무것도 쓰지 않는다"],
    bindings: SECRETS, s3: web("author@example.com"), sql: PAIR_SQL,
    requests: [
      send(report({ request_id: undefined })),
      send(report({ old: "abc" })),
      send(report({ new: OLD })),
      send(report({ new: OLD.toUpperCase() })),
      send(report({ new: ELSEWHERE })),
      send(report({ new: OTHER_TARGET })),
      send(report({ old: NEW, new: OLD })),
      send("{nope"),
      send(report({ old: "0190f0a0-0000-7000-8000-0000000000ff" })),
      send(report({ new: "0190f0a0-0000-7000-8000-0000000000ff" })),
      send(report({ new: undefined })),
    ],
  },
  {
    name: N("supersede_reporter"), module: M,
    covers: ["supersedes.test.ts: 보고자는 확인된 로그인이고 두 평가 중 하나의 작성자여야 한다 (게스트 401, 남 403)"],
    bindings: SECRETS, s3: web("stranger@example.com"), sql: PAIR_SQL,
    requests: [send(report(), {}), send(report())],
  },
  {
    name: N("supersede_no_s3"), module: M,
    // S3 설정이 없으면 세션을 확인할 수 없어 401
    bindings: NO_S3, sql: PAIR_SQL,
    requests: [send(report())],
  },
  {
    name: N("supersede_flow"), module: M,
    covers: ["supersedes.test.ts: 작성자의 보고는 기록되고 옛 평가가 다시 판정 대기에 오른다. 같은 request_id 는 같은 답(200), 순서를 뒤집으면 400(재시도 확인 전), 다른 쌍이면 409", "supersedes.test.ts: cron 단언의 www 쪽 (보고 202 · 표가 없는 옛 평가도 보고만으로 표시) — 판정은 cron 시험대 verdict_superseded"],
    bindings: SECRETS, s3: web("author@example.com"),
    sql: [
      ...PAIR_SQL,
      evaluation({ id: B5, from: AUTHOR, to: TO, created_at: 4_000 }),
      evaluation({ id: "0190f0a0-0000-7000-8000-0000000000c1", from: AUTHOR, cc: "lone.com", created_at: 1_000 }),
      evaluation({ id: "0190f0a0-0000-7000-8000-0000000000c2", from: AUTHOR, cc: "lone.com", created_at: 2_000 }),
    ],
    requests: [
      send(report()),
      send(report()),
      send(report({ new: OLD, old: NEW })),
      send(report({ new: B5 })),
      send(report({ request_id: "supersede-lone-0000", old: "0190f0a0-0000-7000-8000-0000000000c1", new: "0190f0a0-0000-7000-8000-0000000000c2" })),
    ],
  },
  {
    name: N("supersede_chain"), module: M,
    // 사슬: OLD → NEW, NEW → B5 (새 평가의 작성자가 보고), 같은 쌍을 다른 request_id 로 다시 → 기록은 그대로 두고 표시만 다시 (202)
    bindings: SECRETS,
    s3: ({ n }) => s3json({ token: TOKEN, email: n === 2 ? "other@example.com" : "author@example.com" }),
    sql: [...PAIR_SQL, evaluation({ id: B5, from: AUTHOR, to: TO, created_at: 4_000 })],
    requests: [
      send(report()),
      send(report({ request_id: "supersede-chain-00002", old: NEW, new: B5 })),
      send(report({ request_id: "supersede-again-00003" })),
      send(report({ request_id: "supersede-again-00003" })),
      send(report({ request_id: "supersede-skip-000004", old: OLD, new: B5 })),
    ],
  },
  {
    name: N("supersede_equal_times"), module: M,
    // 시각이 같으면 400 (new 가 더 늦어야 한다), to 가 둘 다 NULL 이면 같은 대상
    bindings: SECRETS, s3: web("author@example.com"),
    sql: [
      evaluation({ id: OLD, from: AUTHOR, created_at: 1_000 }),
      evaluation({ id: NEW, from: AUTHOR, created_at: 1_000 }),
      evaluation({ id: B5, from: AUTHOR, created_at: 1_001 }),
    ],
    requests: [send(report()), send(report({ new: B5 }))],
  },
  {
    name: N("supersede_race_same_request"), module: M,
    // 같은 request_id 의 다른 쌍이 그 사이 들어오면 INSERT 가 유니크 제약에 걸리고 catch 의 findSupersede 가 409
    bindings: SECRETS, s3: web("author@example.com"), sql: [...PAIR_SQL, evaluation({ id: B5, from: AUTHOR, to: TO, created_at: 4_000 })],
    race: {
      match: `FROM eval_supersedes WHERE "request_id"`, op: "first",
      after: [{ sql: `INSERT INTO eval_supersedes ("old_id", "new_id", "reporter", "request_id", "created_at") VALUES (?1, ?2, ?3, ?4, ?5)`, args: [OLD, B5, AUTHOR, "supersede-0123456789", 500] }],
    },
    requests: [send(report())],
  },
  {
    name: N("supersede_race_same_pair"), module: M,
    // 같은 request_id · 같은 쌍이 그 사이 들어오면: ON CONFLICT 가 받는지 · 유니크 오류 뒤 재시도로 200 인지 (D1 이 정한다)
    bindings: SECRETS, s3: web("author@example.com"), sql: PAIR_SQL,
    race: {
      match: `FROM eval_supersedes WHERE "request_id"`, op: "first",
      after: [{ sql: `INSERT INTO eval_supersedes ("old_id", "new_id", "reporter", "request_id", "created_at") VALUES (?1, ?2, ?3, ?4, ?5)`, args: [OLD, NEW, AUTHOR, "supersede-0123456789", 500] }],
    },
    requests: [send(report())],
  },
  {
    name: N("supersede_d1_faults"), module: M,
    bindings: SECRETS, s3: web("author@example.com"), sql: PAIR_SQL,
    faults: [{ match: "INSERT INTO eval_supersedes", nth: 1 }, { match: "INSERT OR REPLACE INTO verdict_dirty", nth: 1 }, { match: `SELECT "from", "cc", "to", "created_at"`, nth: 6 }],
    requests: [send(report()), send(report()), send(report()), send(report({ request_id: "supersede-head-fails1" }))],
  },
  {
    name: N("supersede_rate_limit"), module: M,
    bindings: SECRETS, s3: web("author@example.com"), sql: PAIR_SQL,
    limits: { EVAL_WRITE_LIMITER: false, EVAL_READ_LIMITER: true },
    requests: [send(report())],
  },
];
