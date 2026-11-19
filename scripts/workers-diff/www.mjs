// www 차분 시험: 원본 TS(35781c0 의 workers/www/src · 그것이 가져다 쓰는 workers/cron/src 의 verdict.ts · navtrials.ts)와
// Rust(wasm, workers/www/build)를 같은 D1(workers/migrations 적용) · 같은 씨앗 행 · 같은 정적 파일(web/www) · 같은 가상 시계 ·
// 같은 난수 · 같은 가짜 바인딩(요청 제한 · Vectorize · S3 · 바깥 fetch)으로 돌려 요청 차례를 보내고 견준다:
// 응답(상태 · 정렬한 머리글 · Set-Cookie · 본문(JSON 이면 값으로)), 요청마다의 console 기록 · D1 결과 · 밖으로 샌 예외,
// 바깥 호출 차례(D1 문장 · Vectorize · 요청 제한 · S3(서명을 같은 시각으로 다시 계산해 확인) · 그 밖의 fetch), D1 끝 상태, Vectorize 끝 상태.
//
// usage: node www.mjs [이름[,이름…]]              (ONLY=a,b 도 됨)
//        MODULE=session node www.mjs              scenarios/www_session.mjs 의 것만 (쉼표로 여럿)
//        ORIGINAL=raw node www.mjs                고치지 않은 원본과 견준다. 고친 버그가 닿는 시나리오(fixes 칸)만 달라야 한다
//
// 무작위 값: 두 판의 crypto.getRandomValues · randomUUID 는 같은 씨앗을 쓴다(www_mocks_src.mjs) — 같은 차례로 같은 수의 바이트를
// 꺼내면 앱 세션의 hash · token 도 두 판이 같아 그대로 견준다. 그래서 정규화는 기본으로 끈다.
// 씨앗으로 맞출 수 없는 무작위가 남는 시나리오만 `random: true`(또는 NORMALIZE=1)로 켠다: 한쪽에만 나오는 40 · 64 자리 16진 값을
// 나온 순서대로 <random#n> 으로 바꾼다. 이 정규화는 그 값에서 계산한 것끼리의 차이(주소 · 해시 하나만 다른 것)를 가린다 —
// 고치지 않은 원본(ORIGINAL=raw)과 견줄 때 해시만 다른 고침이 "같음"으로 보인다(fix-web-session-email-trim 이 그 예: 정규화를 켜면
// raw 에서도 같게 나온다). 그래서 바꾼 값이 있으면 줄 끝에 normalized=N 으로 알린다. 앱 세션 발급은 정규화와 상관없이 각 쪽에서
// 따로 확인해 `check` 칸에 남긴다 (token = HMAC(SESSION_SECRET, "session|" + hash) · address · mailto).
import { Miniflare } from "miniflare";
import * as esbuild from "esbuild";
import fs from "node:fs";
import path from "node:path";
import { createHmac } from "node:crypto";
process.env.TZ = "UTC";
import { execFileSync } from "node:child_process";
import { AwsV4Signer } from "aws4fetch";
import { patchOriginal } from "./fixes.mjs";
import { wwwMocksSource } from "./www_mocks_src.mjs";
import { SCENARIOS } from "./www_scenarios.mjs";
import { BASE_URL, T0, DEFAULT_CF, DEFAULT_BINDINGS } from "./scenarios/www_common.mjs";

const HERE = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
const REPO = path.resolve(HERE, "..", "..");
const WORKERS = path.join(REPO, "workers");
const OUT = path.join(HERE, "out");
const ONLY = (process.env.ONLY || process.argv.slice(2).find((a) => !a.startsWith("-")) || "").split(",").filter(Boolean);
const MODULES = (process.env.MODULE || "").split(",").filter(Boolean);
const RAW = process.env.ORIGINAL === "raw" || process.argv.includes("--raw");
const NL = String.fromCharCode(10);

// 원본 TypeScript 는 저장소 이력에서 꺼낸다 (workers-rs 로 바꾸기 직전 커밋). esbuild 가 상대 경로를 따라가도록 같은 나무 모양으로 쓴다
const ORIGINAL_COMMIT = "35781c0";
const FILES = [
  ...["index", "session", "auth", "ratelimit", "blob", "projection", "renderHtml", "evaluations", "navtrials", "graph", "analytic"].map((f) => `workers/www/src/${f}.ts`),
  "workers/cron/src/verdict.ts",
  "workers/cron/src/navtrials.ts",
];
async function original() {
  const dir = path.join(OUT, "www-src");
  fs.rmSync(dir, { recursive: true, force: true });
  const raw = Object.fromEntries(FILES.map((f) => [f, execFileSync("git", ["-C", REPO, "show", `${ORIGINAL_COMMIT}:${f}`]).toString("utf8")]));
  const { files, applied } = RAW ? { files: raw, applied: [] } : await patchOriginal("www", raw);
  console.log(RAW ? "[www] original as is (raw)" : `[www] fixes applied to the original: ${applied.join(", ") || "(none)"}`);
  for (const [f, s] of Object.entries(files)) {
    fs.mkdirSync(path.join(dir, path.dirname(f)), { recursive: true });
    fs.writeFileSync(path.join(dir, f), s);
  }
  return path.join(dir, "workers/www/src/index.ts");
}

const ENTRY_TS = ['import "./prelude.mjs";', 'import W from "./www-ts.mjs";', 'import { wrapWwwEnv, harnessFetch } from "./mocks.mjs";',
  'export default { fetch(req, env, ctx) { return harnessFetch(req, (r) => W.fetch(r, wrapWwwEnv(env), ctx)); } };', ""].join(NL);
const ENTRY_RS = ['import "./prelude.mjs";', 'import W from "./index.js";', 'import { wrapWwwEnv, harnessFetch } from "./mocks.mjs";',
  'export default { fetch(req, env, ctx) { return harnessFetch(req, (r) => new W(ctx, wrapWwwEnv(env)).fetch(r)); } };', ""].join(NL);

fs.mkdirSync(OUT, { recursive: true });
const TS = path.join(OUT, "www-ts");
fs.mkdirSync(TS, { recursive: true });
await esbuild.build({
  entryPoints: [await original()],
  bundle: true, format: "esm", platform: "browser", mainFields: ["browser", "module", "main"], conditions: ["workerd", "worker", "browser"],
  outfile: path.join(TS, "www-ts.mjs"), logLevel: "error",
});
fs.copyFileSync(path.join(HERE, "prelude.mjs"), path.join(TS, "prelude.mjs"));
fs.writeFileSync(path.join(TS, "entry.mjs"), ENTRY_TS);
fs.writeFileSync(path.join(TS, "mocks.mjs"), wwwMocksSource());

// Rust 빌드 산출물 (worker-build) 을 out/www-rs 로
const RS = path.join(OUT, "www-rs");
fs.mkdirSync(RS, { recursive: true });
const BUILD = path.join(WORKERS, "www", "build");
if (!fs.existsSync(path.join(BUILD, "index_bg.wasm"))) throw new Error("workers/www/build 가 없다: (cd workers/www && worker-build --release)");
fs.copyFileSync(path.join(BUILD, "index_bg.wasm"), path.join(RS, "index_bg.wasm"));
{
  // Miniflare 의 모듈 해석기는 `import source X from "*.wasm"`(소스 단계 가져오기)를 못 읽는다: 시험 사본에서만 기본 가져오기로
  const js = fs.readFileSync(path.join(BUILD, "index.js"), "utf8");
  fs.writeFileSync(path.join(RS, "index.js"), js.replace(/import source (\w+) from"\.\/index_bg\.wasm"/, 'import $1 from"./index_bg.wasm"'));
}
fs.copyFileSync(path.join(HERE, "prelude.mjs"), path.join(RS, "prelude.mjs"));
fs.writeFileSync(path.join(RS, "entry.mjs"), ENTRY_RS);
fs.writeFileSync(path.join(RS, "mocks.mjs"), wwwMocksSource());

// D1 스키마: workers/migrations 를 이름순으로 모두 (wrangler d1 migrations apply 와 같은 차례). 주석을 빼고 ; 로 나눈다.
// apac1_premium_discount 는 migrations_dir 이 없어(wrangler.json) 빈 DB 로 둔다 (원본 코드는 이 바인딩을 쓰지 않는다)
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
const sortHeaders = (h) => [...h].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));

// 가짜 S3: aws4fetch 가 보낸 서명을 같은 x-amz-date 로 다시 계산해 맞는지 본다(logis-center commerce.mjs 와 같은 방식).
// 답은 시나리오의 s3({ method, path, n }) — { status, body } (body 가 객체면 JSON) · Response · 없으면 404
async function s3Mock(req, url, scenario, bindings, events, counter) {
  const p = decodeURIComponent(url.pathname.slice(1));
  const auth = req.headers.get("authorization") || "";
  let sig = "none";
  const m = auth.match(/SignedHeaders=([^,]+), Signature=([0-9a-f]+)/);
  if (m) {
    const headers = {};
    for (const h of m[1].split(";")) if (h !== "host") headers[h] = req.headers.get(h);
    const signer = new AwsV4Signer({
      url: req.url, method: req.method, headers, accessKeyId: bindings.SESSION_S3_ACCESS_KEY_ID, secretAccessKey: bindings.SESSION_S3_SECRET_ACCESS_KEY,
      service: "s3", region: bindings.SESSION_S3_REGION, datetime: req.headers.get("x-amz-date"),
    });
    const signed = await signer.sign();
    sig = signed.headers.get("authorization") === auth ? "ok" : "bad";
  }
  const n = (counter.s3 = (counter.s3 || 0) + 1);
  events.push({ kind: "s3", method: req.method, url: req.url, sig, headers: sortHeaders(req.headers) });
  const reply = scenario.s3 ? await scenario.s3({ method: req.method, path: p, n }) : null;
  if (reply instanceof Response) return reply;
  if (!reply) return new Response("", { status: 404 });
  if (reply.throw) throw new Error(reply.throw);
  const body = typeof reply.body === "string" || reply.body === undefined ? reply.body ?? "" : JSON.stringify(reply.body);
  return new Response(body, { status: reply.status ?? 200 });
}

async function run(variant, scenario) {
  const events = [];
  const ends = [];
  const counter = {};
  const vectors = new Map(Object.entries(scenario.vectors || {}));
  const bindings = { ...DEFAULT_BINDINGS, ...(scenario.bindings || {}) };
  for (const [k, v] of Object.entries(bindings)) if (v === undefined) delete bindings[k];
  const limits = scenario.limits || {};
  let raceFired = false;
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
          if (!body.sql.includes(f.match) || (f.op && f.op !== body.op)) continue;
          const key = JSON.stringify([f.match, f.op]);
          const n = (counter[key] || 0) + 1;
          counter[key] = n;
          if (f.nth === undefined || f.nth === n) reply.fail = f.message || "D1_ERROR: injected failure";
        }
        // race: 맞는 문장을 처음 실행한 뒤 같은 DB 에 끼워 넣을 문장 (다른 요청이 그 사이 같은 request_id 로 넣은 경합 — cron.mjs 와 같은 꼴)
        const race = scenario.race;
        if (race && !raceFired && body.sql.includes(race.match) && (!race.op || race.op === body.op)) {
          raceFired = true;
          reply.after = race.after;
        }
        return Response.json(reply);
      }
      if (kind === "rl") {
        events.push({ kind: "rl", name: body.name, opts: body.opts });
        // limits[이름]: true(통과) · false(막힘) · "throw" · (n) => 값 — 바인딩이 있는지는 __HARNESS_LIMITERS 가 정한다
        let mode = limits[body.name];
        if (typeof mode === "function") mode = mode((counter[body.name] = (counter[body.name] || 0) + 1));
        if (mode === "throw") return Response.json({ fail: "rate limiter unavailable" });
        if (mode && typeof mode === "object") return Response.json({ result: mode });
        return Response.json({ result: { success: mode !== false } });
      }
      if (kind === "vec") {
        const { op } = body;
        events.push({ kind: "vec", ...body });
        if (scenario.vec && scenario.vec[op]) {
          const r = await scenario.vec[op](body, vectors);
          return Response.json(r && r.fail ? r : { result: r });
        }
        if (op === "upsert") {
          for (const v of body.vectors) vectors.set(v.id, { values: v.values, metadata: v.metadata });
          return Response.json({ result: { mutationId: "m" + vectors.size } });
        }
        if (op === "getByIds") return Response.json({ result: body.ids.filter((id) => vectors.has(id)).map((id) => ({ id, metadata: vectors.get(id).metadata })) });
        if (op === "deleteByIds") {
          for (const id of body.ids) vectors.delete(id);
          return Response.json({ result: { mutationId: "d" } });
        }
        return Response.json({ result: { matches: [], count: 0 } });
      }
      throw new Error("unknown harness call " + kind);
    }
    if (/\.s3\.[^.]+\.amazonaws\.com$/.test(url.hostname)) return s3Mock(req, url, scenario, bindings, events, counter);
    // 그 밖의 바깥 요청 (analytic 의 사이트 첫 화면 등): 시나리오의 fetch({ url, method, headers, body }) — 없으면 404
    const text = await req.text();
    let body;
    try { body = JSON.parse(text); } catch { body = text; }
    events.push({ kind: "fetch", url: req.url, method: req.method, headers: sortHeaders(req.headers), body });
    const reply = scenario.fetch ? await scenario.fetch({ url: req.url, method: req.method, headers: Object.fromEntries(req.headers), body }) : null;
    if (reply instanceof Response) return reply;
    if (!reply) return new Response("", { status: 404 });
    if (reply.throw) throw new Error(reply.throw);
    return new Response(reply.body ?? "", { status: reply.status ?? 200, headers: reply.headers || {} });
  };

  const mf = new Miniflare({
    modules: true,
    scriptPath: path.join(variant === "ts" ? TS : RS, "entry.mjs"),
    modulesRules: [{ type: "CompiledWasm", include: ["**/*.wasm"], fallthrough: true }, { type: "ESModule", include: ["**/*.js", "**/*.mjs"] }],
    compatibilityDate: "2026-04-01",
    d1Databases: { DB: `pd-www-${variant}-${scenario.name}`, apac1_premium_discount: `pd-www-apac1-${variant}-${scenario.name}` },
    d1Persist: false,
    bindings: { ...bindings, __HARNESS_LIMITERS: Object.keys(limits).join(",") },
    assets: { directory: path.join(REPO, "web", "www"), binding: "ASSETS", routerConfig: { has_user_worker: true } },
    outboundService: outbound,
  });
  try {
    const db = await mf.getD1Database("DB");
    for (const stmt of MIGRATIONS) await db.prepare(stmt).run();
    for (const [sql, ...args] of scenario.sql || []) await db.prepare(sql).bind(...args).run();
    const responses = [];
    const jar = {};
    for (let i = 0; i < scenario.requests.length; i++) {
      const r = scenario.requests[i];
      const prev = responses;
      const headers = { ...(typeof r.headers === "function" ? r.headers(prev) : r.headers || {}) };
      const cookies = { ...(r.jar ? jar : {}), ...(r.cookies || {}) };
      if (Object.keys(cookies).length) headers.Cookie = Object.entries(cookies).map(([k, v]) => `${k}=${v}`).join("; ");
      headers["x-harness-clock"] = String(r.at ?? T0 + i * 1000);
      const body = typeof r.body === "function" ? r.body(prev) : r.body;
      const pathText = typeof r.path === "function" ? r.path(prev) : r.path || "/";
      const before = ends.length;
      let out;
      try {
        const res = await mf.dispatchFetch(BASE_URL + pathText, {
          method: r.method || "GET", headers, body: body === undefined ? undefined : typeof body === "string" || body instanceof Uint8Array ? body : JSON.stringify(body),
          cf: r.cf === null ? undefined : { ...DEFAULT_CF, ...(r.cf || {}) },
        });
        const setCookie = res.headers.getSetCookie();
        for (const c of setCookie) {
          const [kv] = c.split(";");
          const k = kv.indexOf("=");
          jar[kv.slice(0, k).trim()] = kv.slice(k + 1).trim();
        }
        const text = await res.text();
        let parsed;
        try { parsed = JSON.parse(text); } catch { parsed = text; }
        const hs = sortHeaders([...res.headers].filter(([k]) => k !== "set-cookie"));
        out = { status: res.status, headers: Object.fromEntries(hs), setCookie, body: parsed };
        // 무작위 값에서 계산한 값은 정규화가 가리므로 여기서 따로 확인한다 (앱 세션 발급)
        if (parsed && typeof parsed === "object" && typeof parsed.hash === "string" && typeof parsed.token === "string") {
          const host = bindings.SESSION_MAIL_HOST || "premium.discount";
          const address = `${parsed.hash}.${host}@oauth.email`;
          out.check = {
            token_is_hmac: bindings.SESSION_SECRET !== undefined && parsed.token === createHmac("sha256", bindings.SESSION_SECRET).update("session|" + parsed.hash).digest("hex"),
            address: parsed.address === address,
            mailto: parsed.mailto === "mailto:" + encodeURIComponent(address),
          };
        }
      } catch (e) {
        out = { error: String(e) };
      }
      out.end = ends.slice(before);
      responses.push(out);
    }
    const state = {};
    const tables = (await db.prepare(`SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' AND name NOT LIKE 'd1_%' ORDER BY name`).all()).results;
    for (const { name } of tables) {
      const { results } = await db.prepare(`SELECT * FROM "${name}" ORDER BY rowid`).all();
      if (results.length) state[name] = results.map(shrink);
    }
    return { responses, events, state, vectors: [...vectors].map(([id, v]) => ({ id, ...v })) };
  } finally {
    await mf.dispose();
  }
}

// 무작위 값(앱 세션 hash · token 과 그것이 든 S3 주소 · 서명)은 실행마다 다르다:
// 한쪽에만 있는 40/64자리 16진 값은 나온 순서대로 자리표로 바꾼다 (양쪽에 다 있는 값 — 결정적 해시 — 은 그대로)
const RANDOM = /(?:0x)?(?:[0-9a-fA-F]{64}|[0-9a-fA-F]{40})(?![0-9a-fA-F])/g;
function normalize(a, b) {
  const inA = new Set((a.match(RANDOM) || []).map((x) => x.toLowerCase().replace(/^0x/, "")));
  const inB = new Set((b.match(RANDOM) || []).map((x) => x.toLowerCase().replace(/^0x/, "")));
  let replaced = 0;
  const norm = (s, other) => {
    const seen = new Map();
    const out = s.replace(RANDOM, (x) => {
      const key = x.toLowerCase().replace(/^0x/, "");
      if (other.has(key)) return x;
      if (!seen.has(key)) seen.set(key, `<random#${seen.size}>`);
      return seen.get(key);
    });
    replaced += seen.size;
    return out;
  };
  return [norm(a, inB), norm(b, inA), replaced];
}

let fail = 0;
let unexpected = 0;
const picked = SCENARIOS.filter((s) => (!ONLY.length || ONLY.includes(s.name)) && (!MODULES.length || MODULES.includes(s.module)));
if (!picked.length) {
  console.log(`no scenario (ONLY=${ONLY.join(",")} MODULE=${MODULES.join(",")})`);
  process.exit(ONLY.length ? 1 : 0);
}
for (const scenario of picked) {
  const ts = await run("ts", scenario);
  const rs = await run("rs", scenario);
  const plain = [JSON.stringify(ts, null, 1), JSON.stringify(rs, null, 1), 0];
  const [a, b, replaced] = scenario.random || process.env.NORMALIZE === "1" ? normalize(plain[0], plain[1]) : plain;
  const tag = RAW ? "raw" : "fixed";
  fs.writeFileSync(path.join(OUT, `www-${scenario.name}-${tag}-ts.json`), a);
  fs.writeFileSync(path.join(OUT, `www-${scenario.name}-${tag}-rs.json`), b);
  const same = a === b;
  if (!same) fail++;
  const expectDiff = RAW && (scenario.fixes || []).length > 0;
  const note = RAW ? (same === !expectDiff ? (expectDiff ? `  (expected: ${scenario.fixes.join(", ")})` : "") : "  UNEXPECTED") : "";
  if (RAW && same === expectDiff) unexpected++;
  const st = (x) => x.responses.map((r) => r.status ?? "E").join(",");
  const kinds = (x) => {
    const c = {};
    for (const e of x.events) c[e.kind] = (c[e.kind] || 0) + 1;
    return Object.entries(c).map(([k, n]) => `${k}${n}`).join(" ") || "-";
  };
  // 정규화가 바꾼 값이 있으면 알린다 (그 값에서 계산한 것끼리의 차이는 보이지 않는다)
  const rnd = replaced ? `  normalized=${replaced}` : "";
  console.log(`${same ? "SAME" : "DIFF"}  [${scenario.module}] ${scenario.name}  status=${st(ts)}/${st(rs)}  ${kinds(ts)} / ${kinds(rs)}${rnd}${note}`);
}
if (RAW) {
  console.log(unexpected ? `${unexpected} scenario(s) not as expected` : `raw: ${fail} of ${picked.length} scenario(s) differ, each only where a fix applies`);
  process.exit(unexpected ? 1 : 0);
}
console.log(fail ? `${fail} of ${picked.length} scenario(s) differ` : `all ${picked.length} scenarios identical`);
process.exit(fail ? 1 : 0);
