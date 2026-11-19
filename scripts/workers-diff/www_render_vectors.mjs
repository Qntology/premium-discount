// renderHtml 벡터: 원본(35781c0 의 workers/www/src/renderHtml.ts)을 그대로 불러 몇 가지 인자로 돌린 결과를
// workers/www/tests/render_vectors.json 에 쓴다. Rust 판(workers/www/src/render.rs)이 `cargo test` 에서 같은 바이트인지 본다.
// renderHtml 은 레거시 처리기의 실행될 수 없는 GET 갈래에서만 불려 차분 시험대로는 닿지 않는다(docs/workers-rs.md 의 www 절).
//
// usage: node www_render_vectors.mjs   (Node 22, --experimental-strip-types 없이: 함수 하나라 형 주석만 지우고 불러온다)
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const HERE = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
const REPO = path.resolve(HERE, "..", "..");
const src = execFileSync("git", ["-C", REPO, "show", "35781c0:workers/www/src/renderHtml.ts"]).toString("utf8");
// 형 주석이 없는 JS 다 (export function renderHtml(cookies, rows) { return `…` })
const mod = await import("data:text/javascript;base64," + Buffer.from(src).toString("base64"));

const cases = [
  { name: "empty", cookies: {}, rows: [] },
  { name: "signed in", cookies: { address: "0x00000000000000000000000000000000000000ab" }, rows: [] },
  { name: "empty address", cookies: { address: "" }, rows: null },
  { name: "rows undefined", cookies: { team: "t" } },
  {
    name: "rows with data",
    cookies: { address: "0xabc", n: 1e21, f: 0.1 + 0.2, neg: -0, small: 5e-7 },
    rows: [
      { id: "0x1", type: "0", data: { head: "글제목", body: "줄\n바꿈 \"따옴표\" </script>   \u0007", link: "https://a.example/?q=1&b=2" }, created_at: 1760000000000 },
      { id: "0x2", data: [1, 2, { k: null }], nested: { "10": "a", "2": "b", z: true } },
    ],
  },
];
const out = cases.map((c) => ({ name: c.name, cookies: c.cookies, rows: c.rows === undefined ? { $: "undefined" } : c.rows, html: mod.renderHtml(c.cookies, c.rows) }));
const file = path.join(REPO, "workers", "www", "tests", "render_vectors.json");
fs.writeFileSync(file, JSON.stringify({ source: "35781c0:workers/www/src/renderHtml.ts", cases: out }, null, 1) + "\n");
console.log(`wrote ${out.length} cases to ${path.relative(REPO, file)}`);
