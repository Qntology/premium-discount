// cron 차분 시험의 시나리오. 각 시나리오는 단계(steps)의 목록이다:
//   { sql: [[sql, ...args], …] }  D1 에 바로 쓰기 (www 가 하는 일: 대기열 · 평가 · 표 · 표시)
//   { vec: { failUpsert: n, failGet: msg, failDelete: msg } }  가짜 Vectorize 장애
//   { run: ms }                    scheduled 한 번 (가상 시계 = ms)
// 그 밖의 칸: decide(text, runIndex) — 가짜 결정 API 의 판정 ({profanity, nsfw, illegal} 점수 | "down" | "malformed" | "badjson" | "neterr" | {status, body}),
//   faults — D1 문장 장애 주입 [{ match, op?, nth?, run?, message? }], race — 문장 실행 뒤 끼워 넣을 문장 { match, op?, after: [{ sql, args }] },
//   vectors — Vectorize 의 처음 상태, bindings — 변수 · 비밀(값이 undefined 면 빠짐),
//   covers — 이 시나리오가 옮긴 node 시험(workers/cron/test/*.test.ts)의 단언, fixes — 닿는 버그 수정 id (docs/workers-rs.md).

const MIN = 60_000;
const DAY = 86_400_000;
const PARENT_ID = "0190f0a0-0000-7000-8000-000000000001";
const REPLY_ID = "0190f0a0-0000-7000-8000-000000000002";
const AUTHOR = "0x00000000000000000000000000000000000000ab";
const TO = "0x" + "c".repeat(40);

// test-support/fakes.ts 의 unitVector(seed): sin(seed·(i+1)) 를 정규화한 Float32 × 768
function unitVectorHex(seed = 1, bytes = 3072) {
  const v = new Float32Array(768);
  for (let i = 0; i < v.length; i++) v[i] = Math.sin(seed * (i + 1));
  const norm = Math.sqrt(v.reduce((s, x) => s + x * x, 0));
  const u = new Uint8Array(v.map((x) => x / norm).buffer);
  const out = new Uint8Array(bytes);
  out.set(u.subarray(0, Math.min(bytes, u.length)));
  return Buffer.from(out).toString("hex");
}
const VEC = unitVectorHex();

// seedQueue (test-support/fakes.ts 와 같은 기본값). vector: 기본 BLOB, "null" | "text" | 바이트 수
function queue(s) {
  const created = s.created_at ?? 1_000;
  const vector = s.vector === "null" ? "NULL" : s.vector === "text" ? "'not a blob'" : `X'${typeof s.vector === "number" ? unitVectorHex(1, s.vector) : VEC}'`;
  return [
    `INSERT INTO eval_queue ("id", "request_id", "from", "to", "cc", "ref", "root", "text", "text_hash", "vector", "state", "attempts", "lease_until", "created_at", "updated_at", "bcc")
     VALUES (?1, ?2, ?3, ?4, 'example.com', ?5, ?6, ?7, ?8, ${vector}, ?9, ?10, ?11, ?12, ?12, ?13)`,
    s.id, s.request_id ?? "req-" + s.id, AUTHOR, s.to ?? null, s.ref ?? null, s.root ?? s.ref ?? s.id, s.text === undefined ? "글" : s.text,
    "a".repeat(64), s.state ?? "pending", s.attempts ?? 0, s.lease_until ?? null, created, s.bcc === undefined ? "0x" + "b".repeat(40) : s.bcc,
  ];
}
// seedEvaluation
function evaluation(s) {
  const created = s.created_at ?? 1_000;
  return [
    `INSERT INTO evaluations ("id", "from", "to", "cc", "bcc", "ref", "root", "created_at", "updated_at", "request_id")
     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?7, ?8)`,
    s.id, s.from ?? AUTHOR, s.to ?? null, s.cc ?? "example.com", s.ref ?? null, s.root ?? s.ref ?? s.id, created, "req-" + s.id,
  ];
}
// seedVote: 표 한 건 + verdict_dirty 표시. 시나리오마다 번호를 새로 센다
function voter() {
  let n = 0x100;
  return () => "0x" + (n++).toString(16).padStart(40, "0");
}
function votes() {
  let seq = 0;
  const vote = (v) => {
    const n = ++seq;
    const out = [[
      `INSERT INTO eval_votes ("id", "request_id", "eval_id", "voter", "voter_kind", "kind", "value", "created_at") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`,
      v.id ?? `vote-${String(n).padStart(8, "0")}`, `req-vote-${String(n).padStart(8, "0")}`, v.eval_id, v.voter, v.voter_kind ?? "session", v.kind ?? "vote", v.value, v.created_at,
    ]];
    if (v.dirty !== false) out.push([`INSERT OR REPLACE INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)`, v.eval_id, v.created_at]);
    return out;
  };
  return vote;
}
const evalId = (n) => `0190f0a0-0000-7000-8000-${n.toString(16).padStart(12, "0")}`;

// verdicts.test.ts 의 seedScope: 같은 cc+to 의 평가 6건. E1 추천 3, E6 비추천 3, E2~E5 추천·비추천 2씩
function scope(T0) {
  const vote = votes();
  const next = voter();
  const E = [0, 1, 2, 3, 4, 5, 6].map((n) => evalId(0xe0 + n));
  const sql = [];
  for (let i = 1; i <= 6; i++) sql.push(evaluation({ id: E[i], to: TO }));
  const cast = (id, values, at) => values.flatMap((value) => vote({ eval_id: id, voter: next(), value, created_at: at }));
  sql.push(...cast(E[1], [1, 1, 1], T0));
  for (const i of [2, 3, 4, 5]) sql.push(...cast(E[i], [1, -1, 1, -1], T0));
  sql.push(...cast(E[6], [-1, -1, -1], T0));
  return { E, sql, cast };
}

const Q = 1_000_000; // queue.test.ts 의 T0
const V0 = 1_763_251_200_000; // verdicts.test.ts 의 T0

export const SCENARIOS = [
  // ---- 대기열 (queue.test.ts) ----
  { name: "empty", covers: "빈 대기열 · 빈 판정: 아무것도 하지 않는다", steps: [{ run: Q }] },
  {
    name: "clean_saved",
    covers: "깨끗한 글은 evaluations 와 Vectorize 에 저장되고 대기열 행은 지워진다 · 글 한 건에 결정 API 한 번, 세 질문",
    steps: [{ sql: [queue({ id: PARENT_ID, text: "좋은 평가입니다" })] }, { run: Q }],
  },
  {
    name: "profanity_rejected",
    covers: "욕설: rejected, 원문 · 벡터 · text_hash · bcc 지움",
    decide: () => ({ profanity: 0.97 }),
    steps: [{ sql: [queue({ id: PARENT_ID, text: "나쁜 말" })] }, { run: Q }],
  },
  {
    name: "nsfw_illegal_reasons",
    covers: "음란 · 불법 질문도 거부 사유, 걸린 질문 이름이 모두 기록된다",
    decide: (t) => (t === "a" ? { nsfw: 0.8 } : t === "b" ? { illegal: 0.6 } : { nsfw: 0.5, illegal: 0.9 }),
    steps: [{ sql: [queue({ id: evalId(0xa), text: "a", created_at: 1 }), queue({ id: evalId(0xb), text: "b", created_at: 2 }), queue({ id: evalId(0xc), text: "c", created_at: 3 })] }, { run: Q }],
  },
  {
    name: "malformed_fail_closed",
    covers: "응답이 일부 질문만 담으면 통과시키지 않고 재시도로 남긴다 (fail-closed)",
    decide: () => "malformed",
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "retry_limit_failed",
    covers: "재시도 한도(10회)에 닿으면 failed, 원문 · 벡터 · text_hash · bcc 지움, 오류에 원문 없음 (decision api responded 503)",
    decide: () => "down",
    steps: [{ sql: [queue({ id: PARENT_ID })] }, ...Array.from({ length: 10 }, (_, i) => ({ run: Q + i * MIN }))],
  },
  {
    name: "reply_waits_for_parent",
    covers: "부모가 대기열에 있으면 답글은 기다리고 횟수도 세지 않는다. 부모가 끝난 뒤 같은 실행에서 순서대로 저장",
    decide: (t, run) => (run === 0 && t === "부모 글" ? "down" : {}),
    steps: [
      { sql: [queue({ id: PARENT_ID, text: "부모 글", created_at: 1 }), queue({ id: REPLY_ID, text: "답글", ref: PARENT_ID, root: PARENT_ID, created_at: 2 })] },
      { run: Q },
      { run: Q + MIN },
    ],
  },
  {
    name: "parent_rejected",
    covers: "부모가 거부되면 답글도 거부되고 해시까지 지워진다",
    decide: (t) => (t === "부모 글" ? { profanity: 0.9 } : {}),
    steps: [{ sql: [queue({ id: PARENT_ID, text: "부모 글", created_at: 1 }), queue({ id: REPLY_ID, text: "답글", ref: PARENT_ID, root: PARENT_ID, created_at: 2 })] }, { run: Q }],
  },
  {
    name: "parent_missing",
    covers: "저장된 부모가 없는 답글은 거부된다",
    steps: [{ sql: [queue({ id: REPLY_ID, text: "고아 답글", ref: PARENT_ID, root: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "vectorize_outage_idempotent",
    covers: "Vectorize 장애로 저장이 끊겨도 다시 실행하면 한 번만 저장된다",
    vec: { failUpsert: 1 },
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }, { run: Q + MIN }],
  },
  {
    name: "lease",
    covers: "lease 가 만료된 processing 행은 다시 집고, 만료 전에는 건드리지 않는다",
    steps: [{ sql: [queue({ id: PARENT_ID, text: "죽은 실행이 남긴 행", state: "processing", attempts: 1, lease_until: Q + 10 * MIN })] }, { run: Q }, { run: Q + 11 * MIN }],
  },
  {
    name: "retention_7days",
    covers: "rejected · failed 행은 7일 뒤에 지워진다",
    decide: () => ({ profanity: 1 }),
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }, { run: Q + 6 * DAY }, { run: Q + 8 * DAY }],
  },
  {
    name: "batch_of_20",
    covers: "한 번에 20건까지만 처리한다",
    steps: [{ sql: Array.from({ length: 25 }, (_, i) => queue({ id: `0190f0a0-0000-7000-8000-${String(i).padStart(12, "0")}`, text: `글 ${i}`, created_at: i })) }, { run: Q }],
  },
  // ---- 대기열 가장자리 ----
  {
    name: "to_ref_metadata",
    covers: "to · ref 가 있으면 Vectorize 메타데이터에 들어간다 (cc · root 는 늘), bcc NULL 은 NULL 로, 빈 ref 는 최상위로",
    steps: [
      { sql: [evaluation({ id: PARENT_ID })] },
      { sql: [queue({ id: REPLY_ID, text: "답", to: TO, ref: PARENT_ID, root: PARENT_ID, bcc: null, created_at: 1 }), queue({ id: evalId(0x33), text: "빈 ref", ref: "", root: evalId(0x33), created_at: 2 })] },
      { run: Q },
    ],
  },
  {
    name: "vector_wrong_length",
    covers: "vector.test: 길이가 다른 벡터는 거부 (D1 에 흔적 없이 재시도)",
    steps: [{ sql: [queue({ id: PARENT_ID, vector: 100 })] }, { run: Q }],
  },
  {
    name: "vector_text_and_null",
    covers: "vector.test: 지원하지 않는 형태(TEXT)는 거부, 원문 · 벡터가 없는 행은 예외로 재시도",
    steps: [{ sql: [queue({ id: evalId(1), vector: "text", created_at: 1 }), queue({ id: evalId(2), vector: "null", created_at: 2 }), queue({ id: evalId(3), text: null, created_at: 3 })] }, { run: Q }],
  },
  {
    name: "decision_bad_json",
    covers: "결정 API 가 JSON 이 아닌 본문을 주면 SyntaxError 메시지로 재시도",
    decide: () => "badjson",
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "decision_server_throws",
    covers: "결정 API 쪽이 무너지면(가짜 서버가 던짐 → Miniflare 가 500) 상태 코드만 남기고 재시도",
    decide: () => "neterr",
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "decision_fetch_rejects",
    covers: "fetch 자체가 거부되면(http 가 아닌 주소) 그 TypeError 메시지로 재시도",
    bindings: { DECISION_ENDPOINT: "ftp://decision.example/systemone" },
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "decision_odd_shapes",
    covers: "moderation.test: 형태가 다른 응답(answers null · 글 · 형식 다른 답 · 숫자 아닌 noul)은 예외",
    decide: (t) => ({ a: { status: 200, body: "null" }, b: { status: 200, body: '{"answers":"x"}' }, c: { status: 200, body: '{"answers":{"profanity":{"type":"noul","noul":"high"},"nsfw":{"type":"noul","noul":0},"illegal":{"type":"noul","noul":0}}}' }, d: { status: 200, body: '{"answers":{"profanity":{"type":"text","noul":0},"nsfw":{"type":"noul","noul":0},"illegal":{"type":"noul","noul":0}}}' }, e: { status: 200, body: '{"answers":[1,2,3]}' }, f: { status: 429, body: "slow down" } })[t],
    steps: [{ sql: ["a", "b", "c", "d", "e", "f"].map((t, i) => queue({ id: evalId(0x10 + i), text: t, created_at: i })) }, { run: Q }],
  },
  {
    name: "no_api_key",
    covers: "비밀 DASHSCOPE_API_KEY 가 없으면 원본처럼 'Bearer undefined' 를 보낸다",
    bindings: { DASHSCOPE_API_KEY: undefined },
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "no_endpoint",
    covers: "DECISION_ENDPOINT 가 없으면 fetch(undefined) 의 TypeError 로 재시도",
    bindings: { DECISION_ENDPOINT: undefined },
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "claim_lost",
    covers: "다른 실행이 먼저 집은 행(조건부 UPDATE 의 changes 0)은 건너뛴다",
    race: { match: 'SELECT "id" FROM eval_queue', op: "all", after: [{ sql: `UPDATE eval_queue SET "state" = 'processing', "lease_until" = ?1 WHERE "id" = ?2`, args: [Q + 5 * MIN, evalId(1)] }] },
    steps: [{ sql: [queue({ id: evalId(1), created_at: 1 }), queue({ id: evalId(2), created_at: 2 })] }, { run: Q }],
  },
  {
    name: "limit_stored",
    covers: "재시도 한도에서 D1 · Vectorize 에 모두 있으면(삭제만 실패했던 경우) 대기열 행만 지운다",
    decide: () => "down",
    vectors: { [PARENT_ID]: { values: { tag: "seed" }, metadata: { cc: "example.com", root: PARENT_ID } } },
    steps: [{ sql: [evaluation({ id: PARENT_ID }), queue({ id: PARENT_ID, attempts: 9 })] }, { run: Q }],
  },
  {
    name: "limit_partial_rollback",
    covers: "재시도 한도에서 D1 만 있으면 되돌리고 failed",
    decide: () => "down",
    steps: [{ sql: [evaluation({ id: PARENT_ID }), queue({ id: PARENT_ID, attempts: 9 })] }, { run: Q }],
  },
  {
    name: "limit_vector_only",
    covers: "재시도 한도에서 벡터만 있으면 partial: 벡터를 지우고 failed",
    decide: () => "down",
    vectors: { [PARENT_ID]: { values: { tag: "seed" }, metadata: { cc: "example.com", root: PARENT_ID } } },
    steps: [{ sql: [queue({ id: PARENT_ID, attempts: 9 })] }, { run: Q }],
  },
  {
    name: "limit_inspect_failed",
    covers: "재시도 한도에서 저장 여부를 확인할 수 없으면 pending 으로 남기고 'inspect failed: …'",
    decide: () => "down",
    vec: { failGet: "vectorize getByIds unavailable" },
    steps: [{ sql: [queue({ id: PARENT_ID, attempts: 9 })] }, { run: Q }],
  },
  {
    name: "limit_rollback_failed",
    covers: "재시도 한도에서 되돌리기가 실패하면 pending 으로 남기고 'rollback failed: …'",
    decide: () => "down",
    vec: { failDelete: "vectorize deleteByIds unavailable" },
    steps: [{ sql: [evaluation({ id: PARENT_ID }), queue({ id: PARENT_ID, attempts: 9 })] }, { run: Q }],
  },
  {
    name: "d1_down",
    covers: "대기열을 읽는 첫 문장이 실패하면 예외가 scheduled 밖으로 샌다 (판정 · 지갑 정리도 하지 않음)",
    faults: [{ match: 'SELECT "id" FROM eval_queue', op: "all", message: "D1_ERROR: database unavailable" }],
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  {
    name: "save_insert_fails",
    covers: "evaluations INSERT 가 실패하면 catch 가 잡아 재시도로 남긴다",
    faults: [{ match: "INSERT OR IGNORE INTO evaluations", op: "run", message: "D1_ERROR: disk full" }],
    steps: [{ sql: [queue({ id: PARENT_ID })] }, { run: Q }],
  },
  // ---- 고친 버그가 닿는 곳 ----
  {
    name: "reject_update_fails",
    covers: "거부 UPDATE 가 실패해도 그 행만 재시도로 남고 실행은 이어진다 (원본: 예외가 새어 다음 행 · 판정 · 지갑 정리가 멈춤)",
    fixes: ["fix-return-await"],
    decide: (t) => (t === "나쁜 말" ? { profanity: 0.9 } : {}),
    faults: [{ match: `SET "state" = 'rejected'`, op: "run", nth: 1, message: "D1_ERROR: write conflict" }],
    steps: (() => {
      const vote = votes();
      const next = voter();
      return [
        { sql: [queue({ id: evalId(1), text: "나쁜 말", created_at: 1 }), queue({ id: evalId(2), text: "좋은 말", created_at: 2 }), evaluation({ id: evalId(9) }), ...vote({ eval_id: evalId(9), voter: next(), value: 1, created_at: Q })] },
        { run: Q },
        { run: Q + MIN },
      ];
    })(),
  },
  {
    name: "release_update_fails",
    covers: "답글을 되돌리는 UPDATE 가 실패해도 그 행만 재시도로 남고 실행은 이어진다",
    fixes: ["fix-return-await"],
    decide: (t, run) => (run === 0 && t === "부모 글" ? "down" : {}),
    faults: [{ match: `"attempts" = CASE WHEN`, op: "run", nth: 1, message: "D1_ERROR: write conflict" }],
    steps: [
      { sql: [queue({ id: PARENT_ID, text: "부모 글", created_at: 1 }), queue({ id: REPLY_ID, text: "답글", ref: PARENT_ID, root: PARENT_ID, created_at: 2 }), queue({ id: evalId(3), text: "따로", created_at: 3 })] },
      { run: Q },
      { run: Q + MIN },
    ],
  },
  {
    name: "verdict_prototype_kind",
    covers: "www 를 거치지 않은 표의 종류가 Object.prototype 이름(constructor · toString)이면 모르는 종류(가중치 0)로 본다 (원본: NaN → NOT NULL 로 매 실행이 멈춤)",
    fixes: ["fix-weight-own"],
    steps: (() => {
      const vote = votes();
      const next = voter();
      const today = Math.floor(V0 / DAY);
      return [
        {
          sql: [
            evaluation({ id: evalId(0x900) }), evaluation({ id: evalId(0x901) }),
            ...vote({ eval_id: evalId(0x900), voter: next(), value: 1, created_at: V0 }),
            ...vote({ eval_id: evalId(0x900), voter: next(), kind: "constructor", value: 1, created_at: V0 }),
            ...vote({ eval_id: evalId(0x900), voter: next(), kind: "toString", value: -1, created_at: V0 }),
            ...[1, 1, 1].flatMap((value) => vote({ eval_id: evalId(0x901), voter: next(), value, created_at: V0 })),
            [`INSERT INTO nav_trials ("account", "kind", "app", "course", "steps", "runs", "premium", "discount", "best", "last", "last_day", "touched") VALUES ('w', 'wallet', 'app', 'old', '[]', 1, 0, 0, '[]', '[]', ?1, ?1)`, today - 60],
          ],
        },
        { run: V0 },
        { run: V0 + MIN },
      ];
    })(),
  },
  // ---- 판정 (verdict.test.ts · verdict_v2.test.ts 는 crate 의 계약 시험, verdicts.test.ts 는 아래) ----
  {
    name: "verdict_scope_tiers",
    covers: "범위의 기준선으로 등급: premium · discount · neutral, 기준선 세 범위, 표시 지움",
    steps: [{ sql: scope(V0).sql }, { run: V0 }],
  },
  {
    name: "verdict_hysteresis",
    covers: "머무르기: enter 아래 exit 위면 premium 유지 · tier_since 그대로, 값이 0 아래면 neutral · tier_since 바뀜",
    steps: (() => {
      const s = scope(V0);
      const T1 = V0 + DAY;
      return [
        { sql: s.sql },
        { run: V0 },
        { sql: [...s.cast(s.E[1], [-1, -1], T1), ...[2, 3, 4].flatMap((i) => s.cast(s.E[i], [1], T1))] },
        { run: T1 },
        { sql: s.cast(s.E[1], [-1], T1 + MIN) },
        { run: T1 + MIN },
      ];
    })(),
  },
  {
    name: "verdict_guest_and_author",
    covers: "게스트의 표와 작성자 자신의 표는 기록되지만 가중치 0, 다른 로그인의 비추천 1표만 센다",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const id = evalId(1);
      return [
        { sql: [evaluation({ id }), ...[0, 1, 2].flatMap(() => vote({ eval_id: id, voter: next(), voter_kind: "guest", value: 1, created_at: V0 })), ...vote({ eval_id: id, voter: AUTHOR, value: 1, created_at: V0 })] },
        { run: V0 },
        { sql: vote({ eval_id: id, voter: next(), value: -1, created_at: V0 + MIN }) },
        { run: V0 + MIN },
      ];
    })(),
  },
  {
    name: "verdict_latest_vote",
    covers: "(투표자, 종류)마다 가장 최근 표만: 바꾼 표 · 철회 · 종류가 다른 표 · 같은 시각이면 id 순 · 넣은 순서가 아니라 created_at",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const id = evalId(2);
      const [a, b, c, d] = [next(), next(), next(), next()];
      const H = 3_600_000;
      return [
        {
          sql: [
            evaluation({ id }),
            ...vote({ eval_id: id, voter: a, value: 1, created_at: V0 - 3 * H }),
            ...vote({ eval_id: id, voter: a, value: -1, created_at: V0 - H }),
            ...vote({ eval_id: id, voter: b, value: 1, created_at: V0 - 2 * H }),
            ...vote({ eval_id: id, voter: b, value: 0, created_at: V0 - H / 2 }),
            ...vote({ eval_id: id, voter: c, value: 1, created_at: V0 - 2 * H }),
            ...vote({ eval_id: id, voter: c, kind: "vote_outcome", value: -1, created_at: V0 - H }),
            ...vote({ eval_id: id, voter: d, value: -1, created_at: V0 - H, id: "vote-zz" }),
            ...vote({ eval_id: id, voter: d, value: 1, created_at: V0 - H, id: "vote-aa" }),
            ...vote({ eval_id: id, voter: a, value: 1, created_at: V0 - 4 * H }),
          ],
        },
        { run: V0 },
      ];
    })(),
  },
  {
    name: "verdict_race_new_vote",
    covers: "계산하는 사이에 새 표가 들어오면 표시가 남고 다음 실행이 센다",
    race: {
      match: "FROM eval_votes", op: "all",
      after: [
        { sql: `INSERT INTO eval_votes ("id", "request_id", "eval_id", "voter", "voter_kind", "kind", "value", "created_at") VALUES (?1, ?2, ?3, ?4, 'session', 'vote', 1, ?5)`, args: ["vote-late", "req-vote-late", evalId(3), "0x" + "9".repeat(40), V0 + 30_000] },
        { sql: `INSERT OR REPLACE INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)`, args: [evalId(3), V0 + 30_000] },
      ],
    },
    steps: (() => {
      const vote = votes();
      const next = voter();
      const id = evalId(3);
      return [{ sql: [evaluation({ id }), ...vote({ eval_id: id, voter: next(), value: 1, created_at: V0 }), ...vote({ eval_id: id, voter: next(), value: 1, created_at: V0 })] }, { run: V0 + MIN }, { run: V0 + 2 * MIN }];
    })(),
  },
  {
    name: "verdict_sweep_decay",
    covers: "sweep: 새 표가 없어도 가장 오래전에 계산한 판정을 다시 계산해 감쇠를 반영 (반감기 뒤 unrated)",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const id = evalId(4);
      return [{ sql: [evaluation({ id }), ...[1, 1, 1].flatMap((value) => vote({ eval_id: id, voter: next(), value, created_at: V0 }))] }, { run: V0 }, { run: V0 + 90 * DAY }];
    })(),
  },
  {
    name: "verdict_sweep_peers",
    covers: "sweep 은 25건씩 오래된 것부터, 건드린 범위의 피어는 다시 판정만, global 만 같은 평가는 건드리지 않음",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const peers = Array.from({ length: 30 }, (_, i) => evalId(0x200 + i));
      const other = evalId(0x300);
      const sql = [];
      for (const id of peers) sql.push(evaluation({ id }), ...[1, -1, 1].flatMap((value) => vote({ eval_id: id, voter: next(), value, created_at: V0 })));
      sql.push(evaluation({ id: other, cc: "other.example" }), ...[1, 1, 1].flatMap((value) => vote({ eval_id: other, voter: next(), value, created_at: V0 })));
      return [{ sql }, { run: V0 }, { sql: [[`UPDATE eval_verdicts SET "updated_at" = ?1 WHERE "eval_id" = ?2`, V0 + 1, other]] }, { run: V0 + MIN }, { run: V0 + 2 * MIN }];
    })(),
  },
  {
    name: "verdict_dropped",
    covers: "평가가 없는 표시는 지우고 건너뜀, 평가가 사라진 판정 행은 지우고 기준선에서도 뺀다",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const [ghost, kept, gone] = [evalId(5), evalId(6), evalId(7)];
      return [
        { sql: [[`INSERT INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)`, ghost, V0]] },
        { run: V0 },
        { sql: [evaluation({ id: kept }), evaluation({ id: gone }), ...[1, 1, 1].flatMap((value) => vote({ eval_id: kept, voter: next(), value, created_at: V0 })), ...[-1, -1, -1].flatMap((value) => vote({ eval_id: gone, voter: next(), value, created_at: V0 }))] },
        { run: V0 + 30_000 },
        { sql: [[`DELETE FROM evaluations WHERE "id" = ?1`, gone]] },
        { run: V0 + MIN },
      ];
    })(),
  },
  {
    name: "verdict_low_evidence",
    covers: "E < min_evidence 인 평가는 기준선에 들어가지 않고, to 가 없으면 secondary 범위도 없다",
    steps: (() => {
      const vote = votes();
      const next = voter();
      return [
        { sql: [evaluation({ id: evalId(8) }), evaluation({ id: evalId(9) }), ...[1, 1].flatMap((value) => vote({ eval_id: evalId(8), voter: next(), value, created_at: V0 })), ...vote({ eval_id: evalId(9), voter: next(), value: -1, created_at: V0 })] },
        { run: V0 },
      ];
    })(),
  },
  {
    name: "verdict_scheduled_after_queue",
    covers: "예약 실행은 대기열 다음에 판정을 처리한다 (같은 실행에서 저장된 평가는 다음 표부터)",
    steps: (() => {
      const vote = votes();
      const next = voter();
      return [{ sql: [queue({ id: PARENT_ID }), evaluation({ id: evalId(10) }), ...vote({ eval_id: evalId(10), voter: next(), value: 1, created_at: V0 })] }, { run: V0 }];
    })(),
  },
  // ---- 판정 가장자리: 문턱 · 단계 · 대체 · 감쇠의 비트 ----
  {
    name: "verdict_thresholds",
    covers: "문턱: premium grade 1 · 2, discount −1 · −2, floor(순위는 높지만 값이 작음), 단계 대체(secondary 5 미만 → primary 10 → global 30)",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const sql = [];
      // cc a.example + to: 4건(secondary n 4 < 5) · cc a.example to 없음 6건 → primary n 10
      const patterns = [[1, 1, 1, 1], [1, 1, 1, -1], [1, -1], [-1, -1, -1], [1, 1, 1, 1, 1, 1, -1], [-1, -1, -1, -1, 1], [1, 1, -1, -1], [1, 1, 1], [-1, -1], [1, 1, 1, 1, 1, 1, 1, 1]];
      patterns.forEach((p, i) => {
        const id = evalId(0x400 + i);
        sql.push(evaluation({ id, cc: "a.example", to: i < 4 ? TO : null }));
        for (const value of p) sql.push(...vote({ eval_id: id, voter: next(), value, created_at: V0 - i * 3_600_000 }));
      });
      // cc b.example: 5건 → secondary 단계, 값이 고르게 퍼진다
      [[1, 1, 1], [1, 1, -1, 1], [-1, 1, -1], [1, 1], [-1, -1, -1, -1]].forEach((p, i) => {
        const id = evalId(0x500 + i);
        sql.push(evaluation({ id, cc: "b.example", to: TO }));
        for (const value of p) sql.push(...vote({ eval_id: id, voter: next(), value, created_at: V0 - (i + 1) * 7_200_000 }));
      });
      // global 30 을 넘기도록 c.example 에 평가 20건
      for (let i = 0; i < 20; i++) {
        const id = evalId(0x600 + i);
        sql.push(evaluation({ id, cc: "c.example" }));
        for (let k = 0; k <= i % 5; k++) sql.push(...vote({ eval_id: id, voter: next(), value: (i + k) % 3 === 0 ? -1 : 1, created_at: V0 - (i * 5 + k) * 1_800_000 }));
      }
      return [{ sql }, { run: V0 }, { run: V0 + 3 * DAY }];
    })(),
  },
  {
    name: "verdict_superseded",
    covers: "대체된 평가(eval_supersedes)는 근거와 상관없이 discount −2, 이유 superseded",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const [old, neu] = [evalId(0x700), evalId(0x701)];
      return [
        {
          sql: [
            evaluation({ id: old, to: TO, created_at: 1 }), evaluation({ id: neu, to: TO, created_at: 2 }),
            [`INSERT INTO eval_supersedes ("old_id", "new_id", "reporter", "request_id", "created_at") VALUES (?1, ?2, ?3, ?4, ?5)`, old, neu, AUTHOR, "req-sup", V0],
            ...[1, 1, 1, 1].flatMap((value) => vote({ eval_id: old, voter: next(), value, created_at: V0 })),
            ...[1].flatMap((value) => vote({ eval_id: neu, voter: next(), value, created_at: V0 })),
          ],
        },
        { run: V0 },
      ];
    })(),
  },
  {
    name: "verdict_decay_bits",
    covers: "감쇠 2^x 와 순위 tanh 의 마지막 자리까지: 시각이 제각각인 표 400여 건, 기준선이 있는 범위 (JS Math 를 쓰는지 확인)",
    steps: (() => {
      const vote = votes();
      const next = voter();
      const sql = [];
      let s = 99;
      const rnd = () => { s = (Math.imul(s, 1103515245) + 12345) >>> 0; return s / 4294967296; };
      for (let i = 0; i < 40; i++) {
        const id = evalId(0x800 + i);
        sql.push(evaluation({ id, cc: "d.example", to: i % 2 ? TO : null }));
        const n = 3 + Math.floor(rnd() * 15);
        for (let k = 0; k < n; k++) {
          const value = rnd() < 0.55 ? 1 : -1;
          sql.push(...vote({ eval_id: id, voter: next(), kind: rnd() < 0.2 ? "vote_outcome" : "vote", value, created_at: V0 - Math.floor(rnd() * 200 * DAY) }));
        }
      }
      return [{ sql }, { run: V0 }, { run: V0 + 17 * DAY + 12_345 }];
    })(),
  },
  // ---- 지갑 타임트라이얼 정리 (navtrials.ts) ----
  {
    name: "nav_wallet_expiry",
    covers: "마지막으로 달린 날이 30일(오늘 포함)보다 앞선 지갑 코스만 지우고 기록을 남긴다. 세션 코스는 두고, 지운 게 없으면 기록하지 않는다",
    steps: (() => {
      const today = Math.floor(V0 / DAY);
      const trial = (account, kind, course, lastDay) => [
        `INSERT INTO nav_trials ("account", "kind", "app", "course", "steps", "runs", "premium", "discount", "best", "last", "last_day", "touched") VALUES (?1, ?2, 'app', ?3, '[]', 1, 0, 0, '[]', '[]', ?4, ?4)`,
        account, kind, course, lastDay,
      ];
      return [
        { sql: [trial("w1", "wallet", "c1", today - 30), trial("w1", "wallet", "c2", today - 29), trial("w2", "wallet", "c3", today), trial("s1", "session", "c4", today - 400)] },
        { run: V0 },
        { run: V0 + MIN },
      ];
    })(),
  },
];
