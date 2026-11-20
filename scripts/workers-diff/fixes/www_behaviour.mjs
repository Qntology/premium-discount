// www 의 동작 고침 셋 — Rust 판(workers/www/src/graph.rs · util.rs · evaluations.rs)의 같은 고침과 1:1. id 는 docs/workers-rs.md 의 "고친 버그" 표와 같다.
// 모양은 fixes/www.mjs 와 같다 ({ id, worker: "www", file, find, replace, count? }). SQL 은 Rust 와 글자까지 같아야 한다(시험대가 D1 문장을 견준다).
//
// fix-graph-local-window: /graph/local 이 스레드의 가장 오래된 200건(ASC LIMIT 200)만 줘, 200건 넘는 스레드에서는 고른 평가와 새 답글이 빠졌다.
//   고른 평가 + 그 조상 사슬(재귀 CTE, root 까지)을 늘 넣고, 남은 칸을 가장 새 평가로 채운다. 200건 이하 스레드는 원본과 같은 행 · 차례.
// fix-graph-similar-phantom: /graph/similar 와 검색(/evaluations/search · /public/evaluations/search)이 D1 에 행이 없는 Vectorize 결과(유령)를
//   판정 없는 결과로 냈다. 판정 요약을 읽던 IN 한 번을 evaluations 에서 시작해 eval_verdicts 를 LEFT JOIN 하는 문장으로 바꿔(왕복 그대로),
//   행이 있는 결과만 그 차례대로 남긴다.
// fix-eval-replay-mismatch: 같은 request_id 의 다시 보내기를 cc · ref 로만 견줘, 글이 다른 평가도 처음 항목으로 200 이었다. 찾은 행이 아직 쥐고
//   있는 내용(to 늘, bcc 는 저장된 평가 · 살아 있는 대기열, text_hash 는 pending · processing 대기열)까지 견줘 다르면 409.
const GRAPH = "workers/www/src/graph.ts";
const EVAL = "workers/www/src/evaluations.ts";

// graph.rs local_thread_sql() 과 같은 글 (?1 root · ?2 상한 · ?3 고른 평가)
const LOCAL_SQL =
  'WITH RECURSIVE "chain"("id", "depth") AS (SELECT ?3, 1 UNION SELECT p."id", c."depth" + 1 FROM "chain" c JOIN evaluations x ON x."id" = c."id" JOIN evaluations p ON p."id" = x."ref" AND p."root" = ?1 WHERE c."depth" < ?2), ' +
  '"newest"("id") AS (SELECT "id" FROM evaluations WHERE "root" = ?1 AND "id" NOT IN (SELECT "id" FROM "chain") ORDER BY "created_at" DESC, "id" DESC LIMIT max(0, ?2 - (SELECT COUNT(DISTINCT "id") FROM "chain"))) ' +
  'SELECT ${SELECT_COLS} FROM ${FROM_JOINED} WHERE e."root" = ?1 AND ((SELECT COUNT(*) FROM evaluations WHERE "root" = ?1) <= ?2 OR e."id" IN (SELECT "id" FROM "chain") OR e."id" IN (SELECT "id" FROM "newest")) ORDER BY e."created_at" ASC LIMIT ?2';

// util.rs stored_verdicts_sql(n) 과 같은 글
const STORED_VERDICTS = [
  "export async function storedVerdicts(env: EvalEnv, ids: string[]): Promise<{ stored: Set<string>; verdicts: Map<string, VerdictSummary> }> {",
  "\tconst stored = new Set<string>()",
  "\tconst verdicts = new Map<string, VerdictSummary>()",
  "\tif (ids.length === 0) return { stored, verdicts }",
  "\tconst rs = await env.DB.prepare(",
  '\t\t`SELECT e."id", "eval_id", "tier", "grade", "rank", "evidence" FROM evaluations e LEFT JOIN eval_verdicts ON "eval_id" = e."id" WHERE e."id" IN (${binds(ids.length)})`',
  "\t).bind(...ids).all<any>()",
  "\tfor (const r of rs.results) {",
  "\t\tstored.add(r.id)",
  "\t\tif (r.eval_id != null) verdicts.set(r.eval_id, { tier: r.tier, grade: r.grade, rank: r.rank, evidence: r.evidence })",
  "\t}",
  "\treturn { stored, verdicts }",
  "}",
].join("\n");

const FIND_REPLAY = [
  "async function findReplay(env: EvalEnv, requestId: string, content: { cc: string; ref: string | null; to: string | null; bcc: string | null; textHash: string }): Promise<Replay | null> {",
  "\tconst stored = await env.DB.prepare(",
  '\t\t`SELECT "id", "root", "ref", "from", "cc", "to", "bcc" FROM evaluations WHERE "request_id" = ?1`',
  "\t).bind(requestId).first<any>()",
  "\tconst found: any = stored",
  "\t\t? { ...stored, status: 'stored' }",
  "\t\t: await env.DB.prepare(",
  '\t\t\t`SELECT "id", "root", "ref", "from", "cc", "state" AS "status", "to", "bcc", "text_hash" FROM eval_queue WHERE "request_id" = ?1`',
  "\t\t).bind(requestId).first<any>()",
  "\tif (!found) return null",
  "\tconst liveQueue = found.status === 'pending' || found.status === 'processing'",
  "\tconst holdsBcc = liveQueue || found.status === 'stored'",
  "\tif (found.cc !== content.cc || found.ref !== content.ref || found.to !== content.to",
  "\t\t|| (holdsBcc && found.bcc !== content.bcc) || (liveQueue && found.text_hash !== content.textHash)) {",
  "\t\tthrow new ConflictError('request_id was already used for a different evaluation')",
  "\t}",
  "\treturn found",
  "}",
].join("\n");

export default [
  {
    id: "fix-graph-local-window", worker: "www", file: GRAPH,
    find: '`SELECT ${SELECT_COLS} FROM ${FROM_JOINED} WHERE e."root" = ?1 ORDER BY e."created_at" ASC LIMIT ?2` ).bind(base.root, GRAPH_NODE_LIMIT_TEMP)',
    replace: "`" + LOCAL_SQL + "`\n\t).bind(base.root, GRAPH_NODE_LIMIT_TEMP, id)",
  },
  {
    id: "fix-graph-similar-phantom", worker: "www", file: EVAL,
    find: [
      "export async function verdictSummaries(env: EvalEnv, ids: string[]): Promise<Map<string, VerdictSummary>> {",
      "const out = new Map<string, VerdictSummary>()",
      "if (ids.length === 0) return out",
      "const rs = await env.DB.prepare(",
      '`SELECT "eval_id", "tier", "grade", "rank", "evidence" FROM eval_verdicts WHERE "eval_id" IN (${binds(ids.length)})`',
      ").bind(...ids).all<VerdictSummary & { eval_id: string }>()",
      "for (const r of rs.results) out.set(r.eval_id, { tier: r.tier, grade: r.grade, rank: r.rank, evidence: r.evidence })",
      "return out",
      "}",
    ].join("\n"),
    replace: STORED_VERDICTS,
  },
  {
    id: "fix-graph-similar-phantom", worker: "www", file: EVAL,
    find: [
      "const verdicts = await verdictSummaries(env, result.matches.map((m) => m.id))",
      "const matches = tiers ? result.matches.filter((m) => tiers.includes(verdicts.get(m.id)?.tier ?? 'unrated')) : result.matches",
    ].join("\n"),
    replace: [
      "const { stored, verdicts } = await storedVerdicts(env, result.matches.map((m) => m.id))",
      "\tconst found = result.matches.filter((m) => stored.has(m.id))",
      "\tconst matches = tiers ? found.filter((m) => tiers.includes(verdicts.get(m.id)?.tier ?? 'unrated')) : found",
    ].join("\n"),
  },
  {
    id: "fix-graph-similar-phantom", worker: "www", file: GRAPH,
    find: "import { ValidationError, parseDomain, parseUuid, verdictSummaries } from './evaluations'",
    replace: "import { ValidationError, parseDomain, parseUuid, storedVerdicts } from './evaluations'",
  },
  {
    id: "fix-graph-similar-phantom", worker: "www", file: GRAPH,
    find: [
      "const matches = res.matches.filter((m) => m.id !== id).slice(0, topK)",
      "// 최대 49건이라 IN 한 번으로 읽는다",
      "const verdicts = await verdictSummaries(env, matches.map((m) => m.id))",
    ].join("\n"),
    replace: [
      "const candidates = res.matches.filter((m) => m.id !== id).slice(0, topK)",
      "\t// 최대 49건이라 IN 한 번으로 읽는다. 행이 없는 결과(유령)는 뺀다",
      "\tconst { stored, verdicts } = await storedVerdicts(env, candidates.map((m) => m.id))",
      "\tconst matches = candidates.filter((m) => stored.has(m.id))",
    ].join("\n"),
  },
  {
    id: "fix-eval-replay-mismatch", worker: "www", file: EVAL,
    find: [
      "async function findReplay(env: EvalEnv, requestId: string, cc: string, ref: string | null): Promise<Replay | null> {",
      "const stored = await env.DB.prepare(",
      '`SELECT "id", "root", "ref", "from", "cc" FROM evaluations WHERE "request_id" = ?1`',
      ").bind(requestId).first<Omit<Replay, 'status'>>()",
      "const found = stored",
      "? { ...stored, status: 'stored' }",
      ": await env.DB.prepare(",
      '`SELECT "id", "root", "ref", "from", "cc", "state" AS "status" FROM eval_queue WHERE "request_id" = ?1`',
      ").bind(requestId).first<Replay>()",
      "if (!found) return null",
      "if (found.cc !== cc || found.ref !== ref) {",
      "throw new ConflictError('request_id was already used for a different evaluation')",
      "}",
      "return found",
      "}",
    ].join("\n"),
    replace: FIND_REPLAY,
  },
  {
    id: "fix-eval-replay-mismatch", worker: "www", file: EVAL,
    find: "const replay = await findReplay(env, requestId, cc, ref)",
    replace: "const content = { cc, ref, to, bcc, textHash }\n\tconst replay = await findReplay(env, requestId, content)",
  },
  {
    id: "fix-eval-replay-mismatch", worker: "www", file: EVAL,
    find: "const again = await findReplay(env, requestId, cc, ref)",
    replace: "const again = await findReplay(env, requestId, content)",
  },
];
