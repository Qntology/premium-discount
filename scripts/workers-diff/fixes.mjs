// 원본 TS(0538044)에 버그 수정을 덧대 "고친 원본"을 만든다. Rust 판의 같은 수정과 1:1 로 견주기 위해서다.
// logis-center scripts/workers-diff/fixes.mjs 와 같은 규칙: 각 수정은 { id, worker, file, find, replace, count? } —
// find 의 공백(탭 · 줄바꿈 수)은 무시하고 맞추며, 정확히 한 번(count 를 주면 그 수만큼) 맞아야 한다.
// 설명은 docs/workers-rs.md 의 "고친 버그" 표와 같은 id 를 쓴다. 워커별 수정은 fixes/<워커>.mjs 에 둔다.
import fs from "node:fs";
import path from "node:path";

const HERE = path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1"));
const escape = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const loose = (s) => new RegExp(escape(s.trim()).replace(/\s+/g, "\\s+"), "g");

/** fixes/ 폴더의 묶음: export default [ ...수정 ] */
export async function allFixes() {
  const dir = path.join(HERE, "fixes");
  if (!fs.existsSync(dir)) return [];
  const out = [];
  for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".mjs")).sort()) {
    const mod = await import(new URL(`./fixes/${f}`, import.meta.url));
    out.push(...mod.default);
  }
  return out;
}

/** 원본 파일들({ 이름: 글 })에 수정을 덧댄다. 맞는 수가 count(기본 1)가 아니면 던진다 */
export async function patchOriginal(worker, files) {
  const out = { ...files };
  const applied = [];
  for (const fix of await allFixes()) {
    if (fix.worker !== worker) continue;
    if (!(fix.file in out)) throw new Error(`fix ${fix.id}: no file ${fix.file}`);
    const re = fix.find instanceof RegExp ? fix.find : loose(fix.find);
    const n = (out[fix.file].match(re) || []).length;
    const want = fix.count ?? 1;
    if (n !== want) throw new Error(`fix ${fix.id}: ${n} matches in ${fix.file} (want ${want})`);
    out[fix.file] = out[fix.file].replace(re, () => fix.replace);
    if (!applied.includes(fix.id)) applied.push(fix.id);
  }
  return { files: out, applied };
}
