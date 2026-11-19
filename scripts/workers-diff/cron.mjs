// cron 차분 시험: 원본 TS(0538044 의 workers/cron/src)와 Rust(wasm, workers/cron/build)를 같은 D1(workers/migrations 적용) ·
// 같은 씨앗 행 · 같은 가상 시계 · 같은 가짜 결정 API · 같은 가짜 Vectorize 로 돌려 견준다:
// D1 호출(순서 · SQL · 묶은 값 · 방법 · 결과 크기), Vectorize 호출, 바깥 fetch(주소 · 머리글 · 본문), console 기록,
// 밖으로 샌 예외, D1 끝 상태(모든 표), Vectorize 끝 상태.
//
// usage: node cron.mjs [시나리오 이름]        (ONLY=이름 도 됨)
//        ORIGINAL=raw node cron.mjs           고치지 않은 원본과 견준다. 고친 버그가 닿는 시나리오(fixes 칸)만 달라야 한다
import { Miniflare } from "miniflare";
import * as esbuild from "esbuild";
import fs from "node:fs";
import path from "node:path";
process.env.TZ = "UTC";
import { execFileSync } from "node:child_process";
import { patchOriginal } from "./fixes.mjs";
import { mocksSource } from "./mocks_src.mjs";
import { SCENARIOS } from "./cron_scenarios.mjs";

const HERE = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
const REPO = path.resolve(HERE, "..", "..");
const WORKERS = path.join(REPO, "workers");
const OUT = path.join(HERE, "out");
const ONLY = process.env.ONLY || process.argv.slice(2).find((a) => !a.startsWith("-"));
const RAW = process.env.ORIGINAL === "raw" || process.argv.includes("--raw");
const NL = String.fromCharCode(10);

// 원본 TypeScript 는 저장소 이력에서 꺼낸다 (workers-rs 로 바꾸기 직전 커밋)
const ORIGINAL_COMMIT = "0538044";
const FILES = ["index.ts", "moderation.ts", "vector.ts", "verdict.ts", "navtrials.ts"];
async function original() {
  const dir = path.join(OUT, "cron-src");
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  const raw = Object.fromEntries(
    FILES.map((f) => [f, execFileSync("git", ["-C", REPO, "show", `${ORIGINAL_COMMIT}:workers/cron/src/${f}`]).toString("utf8")])
  );
  const { files, applied } = RAW ? { files: raw, applied: [] } : await patchOriginal("cron", raw);
  console.log(RAW ? "[cron] original as is (raw)" : `[cron] fixes applied to the original: ${applied.join(", ") || "(none)"}`);
  for (const [f, s] of Object.entries(files)) fs.writeFileSync(path.join(dir, f), s);
  return path.join(dir, "index.ts");
}

const ENTRY_TS = ['import "./prelude.mjs";', 'import W from "./cron-ts.mjs";', 'import { wrapEnv, harness } from "./mocks.mjs";',
  'export default { scheduled(ctrl, env, ctx) { return harness(ctrl, () => W.scheduled(ctrl, wrapEnv(env), ctx)); } };', ""].join(NL);
const ENTRY_RS = ['import "./prelude.mjs";', 'import W from "./index.js";', 'import { wrapEnv, harness } from "./mocks.mjs";',
  'export default { scheduled(ctrl, env, ctx) { return harness(ctrl, () => new W(ctx, wrapEnv(env)).scheduled(ctrl)); } };', ""].join(NL);

fs.mkdirSync(OUT, { recursive: true });
await esbuild.build({
  entryPoints: [await original()],
  bundle: true, format: "esm", platform: "browser", mainFields: ["browser", "module", "main"], conditions: ["workerd", "worker", "browser"],
  outfile: path.join(OUT, "cron-ts.mjs"), logLevel: "error",
});
fs.copyFileSync(path.join(HERE, "prelude.mjs"), path.join(OUT, "prelude.mjs"));
fs.writeFileSync(path.join(OUT, "cron-ts-entry.mjs"), ENTRY_TS);
fs.writeFileSync(path.join(OUT, "mocks.mjs"), mocksSource());

// Rust 빌드 산출물 (worker-build) 을 out/cron-rs 로
const RS = path.join(OUT, "cron-rs");
fs.mkdirSync(RS, { recursive: true });
const BUILD = path.join(WORKERS, "cron", "build");
if (!fs.existsSync(path.join(BUILD, "index_bg.wasm"))) throw new Error("workers/cron/build 가 없다: (cd workers/cron && worker-build --release)");
fs.copyFileSync(path.join(BUILD, "index_bg.wasm"), path.join(RS, "index_bg.wasm"));
{
  // Miniflare 의 모듈 해석기는 `import source X from "*.wasm"`(소스 단계 가져오기)를 못 읽는다: 시험 사본에서만 기본 가져오기로
  const js = fs.readFileSync(path.join(BUILD, "index.js"), "utf8");
  fs.writeFileSync(path.join(RS, "index.js"), js.replace(/import source (\w+) from"\.\/index_bg\.wasm"/, 'import $1 from"./index_bg.wasm"'));
}
fs.copyFileSync(path.join(HERE, "prelude.mjs"), path.join(RS, "prelude.mjs"));
fs.writeFileSync(path.join(RS, "entry.mjs"), ENTRY_RS);
fs.writeFileSync(path.join(RS, "mocks.mjs"), mocksSource());

// D1 스키마: workers/migrations 를 이름순으로 모두 (wrangler d1 migrations apply 와 같은 차례). 주석을 빼고 ; 로 나눈다
const MIGRATIONS = fs
  .readdirSync(path.join(WORKERS, "migrations"))
  .filter((f) => f.endsWith(".sql"))
  .sort()
  .flatMap((f) =>
    fs.readFileSync(path.join(WORKERS, "migrations", f), "utf8")
      .split(/\r?\n/).map((l) => l.replace(/--.*$/, "")).join(NL)
      .split(";").map((s) => s.trim()).filter(Boolean)
  );

// 끝 상태의 BLOB(숫자 배열)은 길이 · FNV 로 줄인다
function fnv(bytes) {
  let h = 2166136261;
  for (const b of bytes) h = Math.imul(h ^ b, 16777619) >>> 0;
  return h.toString(16).padStart(8, "0");
}
const isBytes = (a) => Array.isArray(a) && a.length > 8 && a.every((x) => Number.isInteger(x) && x >= 0 && x < 256);
const shrink = (row) => Object.fromEntries(Object.entries(row).map(([k, v]) => [k, isBytes(v) ? { blob: v.length, fnv: fnv(v) } : v]));

// 가짜 결정 API 응답
function decisionResponse(verdict) {
  if (verdict === "neterr") throw new Error("decision network unreachable");
  if (verdict === "down") return new Response("", { status: 503 });
  if (verdict === "malformed") return new Response(JSON.stringify({ answers: { profanity: { type: "noul", noul: 0 } } }), { status: 200 });
  if (verdict === "badjson") return new Response("<html>gateway</html>", { status: 200 });
  if (verdict && verdict.status) return new Response(verdict.body ?? "", { status: verdict.status });
  const answers = {};
  for (const k of ["profanity", "nsfw", "illegal"]) answers[k] = { type: "noul", noul: (verdict || {})[k] ?? 0 };
  return new Response(JSON.stringify({ answers }), { status: 200 });
}

async function run(variant, scenario) {
  const events = [];
  const ends = [];
  const vectors = new Map(Object.entries(scenario.vectors || {}).map(([id, v]) => [id, v]));
  const vec = { failUpsert: 0, ...(scenario.vec || {}) };
  const counts = new Map();
  let runIndex = -1;
  const outbound = async (req) => {
    const url = new URL(req.url);
    if (url.hostname === "harness") {
      const body = JSON.parse(await req.text());
      const kind = url.pathname.slice(1);
      if (kind === "end") {
        ends.push(body);
        return Response.json({});
      }
      if (kind === "d1") {
        events.push({ kind: "d1", op: body.op, sql: body.sql, args: body.args });
        const reply = {};
        for (const f of scenario.faults || []) {
          if (f.run !== undefined && f.run !== runIndex) continue;
          if (!body.sql.includes(f.match) || (f.op && f.op !== body.op)) continue;
          const key = JSON.stringify([f.match, f.op, f.run]);
          const n = (counts.get(key) || 0) + 1;
          counts.set(key, n);
          if (f.nth === undefined || f.nth === n) reply.fail = f.message || "D1_ERROR: injected failure";
        }
        const race = scenario.race;
        if (race && body.sql.includes(race.match) && (!race.op || race.op === body.op) && !race.fired) {
          race.fired = true;
          reply.after = race.after;
        }
        return Response.json(reply);
      }
      if (kind === "vec") {
        const { op } = body;
        events.push({ kind: "vec", ...body });
        if (op === "upsert") {
          if (vec.failUpsert > 0) { vec.failUpsert--; return Response.json({ fail: "vectorize unavailable" }); }
          for (const v of body.vectors) vectors.set(v.id, { values: v.values, metadata: v.metadata });
          return Response.json({ result: { mutationId: "m" + vectors.size } });
        }
        if (op === "getByIds") {
          if (vec.failGet) return Response.json({ fail: vec.failGet });
          return Response.json({ result: body.ids.filter((id) => vectors.has(id)).map((id) => ({ id, metadata: vectors.get(id).metadata })) });
        }
        if (op === "deleteByIds") {
          if (vec.failDelete) return Response.json({ fail: vec.failDelete });
          for (const id of body.ids) vectors.delete(id);
          return Response.json({ result: { mutationId: "d" } });
        }
        return Response.json({ result: { matches: [], count: 0 } });
      }
      throw new Error("unknown harness call " + kind);
    }
    // 바깥 요청 (결정 API)
    const text = await req.text();
    let body;
    try { body = JSON.parse(text); } catch { body = text; }
    const headers = [...req.headers].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    events.push({ kind: "fetch", url: req.url, method: req.method, headers, body });
    const decide = scenario.decide || (() => ({}));
    return decisionResponse(decide(body && body.state, runIndex));
  };

  const mf = new Miniflare({
    modules: true,
    scriptPath: variant === "ts" ? path.join(OUT, "cron-ts-entry.mjs") : path.join(RS, "entry.mjs"),
    modulesRules: [{ type: "CompiledWasm", include: ["**/*.wasm"], fallthrough: true }, { type: "ESModule", include: ["**/*.js", "**/*.mjs"] }],
    compatibilityDate: "2026-04-01",
    d1Databases: { DB: `pd-${variant}-${scenario.name}` },
    d1Persist: false,
    bindings: Object.fromEntries(
      Object.entries({ DECISION_ENDPOINT: "https://decision.example/systemone", DASHSCOPE_API_KEY: "test-key", ...(scenario.bindings || {}) }).filter(([, v]) => v !== undefined)
    ),
    outboundService: outbound,
  });
  try {
    const db = await mf.getD1Database("DB");
    for (const stmt of MIGRATIONS) await db.prepare(stmt).run();
    const worker = await mf.getWorker();
    const runs = [];
    if (scenario.race) delete scenario.race.fired;
    for (const step of scenario.steps) {
      if (step.sql) {
        for (const [sql, ...args] of step.sql) await db.prepare(sql).bind(...args).run();
      }
      if (step.vec) Object.assign(vec, step.vec);
      if (step.run !== undefined) {
        runIndex++;
        const before = ends.length;
        const res = await worker.scheduled({ cron: "* * * * *", scheduledTime: new Date(step.run) });
        runs.push({ at: step.run, outcome: res.outcome, end: ends.slice(before) });
      }
    }
    const state = {};
    const tables = (await db.prepare(`SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name`).all()).results;
    for (const { name } of tables) {
      const { results } = await db.prepare(`SELECT * FROM "${name}" ORDER BY rowid`).all();
      if (results.length) state[name] = results.map(shrink);
    }
    return { events, runs, state, vectors: [...vectors].map(([id, v]) => ({ id, ...v })) };
  } finally {
    await mf.dispose();
  }
}

let fail = 0;
let unexpected = 0;
const picked = SCENARIOS.filter((s) => !ONLY || s.name === ONLY);
if (!picked.length) throw new Error(`no scenario ${ONLY}`);
for (const scenario of picked) {
  const ts = await run("ts", scenario);
  const rs = await run("rs", scenario);
  const a = JSON.stringify(ts, null, 1), b = JSON.stringify(rs, null, 1);
  const tag = RAW ? "raw" : "fixed";
  fs.writeFileSync(path.join(OUT, `cron-${scenario.name}-${tag}-ts.json`), a);
  fs.writeFileSync(path.join(OUT, `cron-${scenario.name}-${tag}-rs.json`), b);
  const same = a === b;
  if (!same) fail++;
  // 고치지 않은 원본과는 고친 버그가 닿는 시나리오만 달라야 한다
  const expectDiff = RAW && (scenario.fixes || []).length > 0;
  const note = RAW ? (same === !expectDiff ? (expectDiff ? `  (expected: ${scenario.fixes.join(", ")})` : "") : "  UNEXPECTED") : "";
  if (RAW && same === expectDiff) unexpected++;
  const outc = (x) => x.runs.map((r) => r.outcome).join(",");
  const kinds = (x) => {
    const c = {};
    for (const e of x.events) c[e.kind] = (c[e.kind] || 0) + 1;
    return Object.entries(c).map(([k, n]) => `${k}${n}`).join(" ");
  };
  console.log(`${same ? "SAME" : "DIFF"}  ${scenario.name}  runs=${outc(ts)}/${outc(rs)}  ${kinds(ts)} / ${kinds(rs)}${note}`);
}
if (RAW) {
  console.log(unexpected ? `${unexpected} scenario(s) not as expected` : `raw: ${fail} scenario(s) differ, each only where a fix applies`);
  process.exit(unexpected ? 1 : 0);
}
console.log(fail ? `${fail} scenario(s) differ` : `all ${picked.length} scenarios identical`);
process.exit(fail ? 1 : 0);
