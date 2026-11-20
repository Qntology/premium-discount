// www evaluations.ts 에 덧대는 수정 — Rust 판(workers/www/src/evaluations.rs)의 같은 수정과 1:1. id 는 docs/workers-rs.md 의 "고친 버그" 표와 같다.
// 모양은 fixes/www.mjs 와 같다 ({ id, worker: "www", file, find, replace, count? }).
export default [
  // isAddress 가 받는 다른 꼴(0x 없는 40자리 · ICAP)이 소문자만 되어 그대로 저장 · 검색되고 ZeroAddress 검사를 빠져나가던 것
  {
    id: "fix-eval-address-normalize", worker: "www", file: "workers/www/src/evaluations.ts",
    find: "const normalized = value.toLowerCase()",
    replace: "const normalized = ethers.getAddress(value).toLowerCase()",
  },
];
