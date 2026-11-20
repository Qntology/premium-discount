// 워커 안(두 진입 모듈이 같이 쓰는 mocks.mjs)의 가짜 바인딩. 글(모듈 소스)로 내보낸다: 하네스가 out/ 아래 두 쪽에 쓴다.
// logis-center scripts/workers-diff/mocks_src.mjs 의 방식(진입 모듈이 env 를 Proxy 로 감쌈)을 따르되, 상태와 결정은 시험대(Node)가 쥔다:
//
// - D1: 진짜 Miniflare D1 을 감싼다. 문장을 실행하기 전에 http://harness/d1 에 (sql, 묶은 값, 방법)을 알리고, 시험대가
//   { fail } (그 문장을 던지게) · { after: [{ sql, args }] } (실행 뒤 같은 DB 에 끼워 넣을 문장 — www 가 그 사이 표를 넣는 경합) ·
//   { before: [...] } (실행 바로 전에 끼워 넣을 문장 — 다른 요청이 먼저 쓴 경합, batch 에도)를 돌려준다.
//   그래서 D1 호출의 순서 · 글 · 값이 두 판에서 그대로 견주어진다. 실행 결과(행 수 · 바뀐 행 수)는 실행 끝에 함께 보낸다.
// - batch: 문장 목록을 op "batch" 한 번으로 알린다(sql 은 " ;; " 로 이음, args 는 문장마다). cron 은 batch 를 쓰지 않는다.
// - Vectorize: upsert · getByIds · deleteByIds · query 를 http://harness/vec 로. 저장소와 장애 주입은 시험대에 있다.
//   값(Float32Array)은 형 · 길이 · 비트 FNV 로 줄여 보낸다.
// - 실행 하나(scheduled)를 감싸 시계를 scheduledTime 으로 맞추고, 끝에 console 기록 · 결과 · 밖으로 샌 예외를 보낸다.
export const mocksSource = () => `
const H = "http://harness";
async function ctl(kind, payload) {
  const res = await fetch(H + "/" + kind, { method: "POST", body: JSON.stringify(payload) });
  return res.json();
}
function fnv(u8) { let h = 2166136261; for (const b of u8) h = Math.imul(h ^ b, 16777619) >>> 0; return h.toString(16).padStart(8, "0"); }
function bytesOf(v) {
  if (v instanceof ArrayBuffer) return new Uint8Array(v);
  if (ArrayBuffer.isView(v)) return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
  return null;
}
// 묶은 값: JSON 이 담지 못하는 것(undefined · NaN · ±Infinity · -0 · 바이트)은 꼬리표로
function enc(v) {
  if (v === undefined) return { $: "undefined" };
  if (typeof v === "number" && !Number.isFinite(v)) return { $: String(v) };
  if (typeof v === "number" && Object.is(v, -0)) return { $: "-0" };
  const b = bytesOf(v);
  // gzip 바이트(1f 8b)는 통째로 보낸다: 시험대가 풀어서 견준다 (pako 와 Rust 의 flate2 는 같은 글을 다른 바이트로 압축한다)
  if (b && b.length >= 2 && b[0] === 0x1f && b[1] === 0x8b) return { $bytes: Object.prototype.toString.call(v), gzip: b64(b) };
  if (b) return { $bytes: Object.prototype.toString.call(v), len: b.length, fnv: fnv(b) };
  return v;
}
function b64(u8) { let s = ""; for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode(...u8.subarray(i, i + 0x8000)); return btoa(s); }
function vectorSummary(values) {
  const tag = Object.prototype.toString.call(values);
  const b = bytesOf(values);
  if (b) return { tag, len: values.length, fnv: fnv(b), head: Array.from(values).slice(0, 3) };
  if (Array.isArray(values)) return { tag, len: values.length, fnv: fnv(new Uint8Array(new Float64Array(values).buffer)), head: values.slice(0, 3) };
  return { tag, value: values };
}
const results = [];
function summary(op, out) {
  if (op === "run") return { changes: out && out.meta ? out.meta.changes : undefined, success: out && out.success };
  if (op === "all") return { rows: out && out.results ? out.results.length : undefined };
  if (op === "first") return { found: out !== null && out !== undefined };
  return {};
}
function wrapStmt(db, stmt, sql, args) {
  return new Proxy(stmt, { get(t, prop) {
    if (prop === "bind") return (...a) => wrapStmt(db, t.bind(...a), sql, a);
    if (prop === "__harness") return { raw: t, sql, args };
    if (prop === "first" || prop === "all" || prop === "run" || prop === "raw") return async (...x) => {
      const c = await ctl("d1", { op: prop, sql, args: args.map(enc) });
      if (c.fail) { results.push({ op: prop, threw: c.fail }); throw new Error(c.fail); }
      for (const s of c.before || []) await db.prepare(s.sql).bind(...s.args).run();
      let out;
      try { out = await t[prop](...x); }
      catch (e) { results.push({ op: prop, threw: String(e) }); throw e; }
      results.push({ op: prop, ...summary(prop, out) });
      for (const s of c.after || []) await db.prepare(s.sql).bind(...s.args).run();
      return out;
    };
    const v = Reflect.get(t, prop); return typeof v === "function" ? v.bind(t) : v; } });
}
function wrapD1(db) {
  if (!db || typeof db.prepare !== "function") return db;
  return new Proxy(db, { get(t, prop) {
    if (prop === "prepare") return (sql) => wrapStmt(t, t.prepare(sql), sql, []);
    // batch (www 의 analytic · navtrials): 문장 목록을 한 번에 알리고, 진짜 문장으로 풀어 실행한다
    if (prop === "batch") return async (stmts) => {
      const list = stmts.map((x) => (x && x.__harness) || { raw: x, sql: "?", args: [] });
      const c = await ctl("d1", { op: "batch", sql: list.map((x) => x.sql).join(" ;; "), args: list.map((x) => x.args.map(enc)) });
      if (c.fail) { results.push({ op: "batch", threw: c.fail }); throw new Error(c.fail); }
      for (const s of c.before || []) await t.prepare(s.sql).bind(...s.args).run();
      let out;
      try { out = await t.batch(list.map((x) => x.raw)); }
      catch (e) { results.push({ op: "batch", threw: String(e) }); throw e; }
      results.push({ op: "batch", changes: out.map((r) => (r && r.meta ? r.meta.changes : undefined)) });
      for (const s of c.after || []) await t.prepare(s.sql).bind(...s.args).run();
      return out;
    };
    const v = Reflect.get(t, prop); return typeof v === "function" ? v.bind(t) : v; } });
}
async function vecCall(op, payload) {
  const r = await ctl("vec", { op, ...payload });
  if (r.fail) throw new Error(r.fail);
  return r.result;
}
const vectorize = {
  upsert: (vectors) => vecCall("upsert", { vectors: vectors.map((v) => ({ id: v.id, values: vectorSummary(v.values), metadata: v.metadata })) }),
  getByIds: (ids) => vecCall("getByIds", { ids }),
  deleteByIds: (ids) => vecCall("deleteByIds", { ids }),
  query: (vector, options) => vecCall("query", { vector: vectorSummary(vector), options }),
};
export function wrapEnv(env) {
  return new Proxy(env, { get(target, prop) {
    if (prop === "EVAL_VECTORIZE") return vectorize;
    if (prop === "DB") return wrapD1(Reflect.get(target, prop));
    return Reflect.get(target, prop);
  } });
}
// 실행 하나: 시계를 scheduledTime 으로, 끝에 console · 결과 · 예외를 시험대로
export async function harness(ctrl, fn) {
  globalThis.__harness_set_clock(Number(ctrl.scheduledTime));
  globalThis.__harness_logs.length = 0;
  results.length = 0;
  let exception = null;
  try {
    await fn();
  } catch (e) {
    exception = { string: String(e), isError: e instanceof Error };
    throw e;
  } finally {
    await ctl("end", { logs: [...globalThis.__harness_logs], results: [...results], exception });
  }
}
`;
