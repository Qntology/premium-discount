// cron 원본(0538044 workers/cron/src)에 덧대는 수정 — Rust 판(workers/cron)의 같은 수정과 1:1. id 는 docs/workers-rs.md 의 "고친 버그" 표와 같다.
export default [
  // processOne 의 try 안에서 기다리지 않은 `return reject(…)` · `return release(…)`: 그 UPDATE 가 실패하면 catch 를 지나 실행 전체가 멈췄다
  { id: "fix-return-await", worker: "cron", file: "index.ts", count: 3, find: "return reject(env, id,", replace: "return await reject(env, id," },
  { id: "fix-return-await", worker: "cron", file: "index.ts", find: "return release(env, id, now)", replace: "return await release(env, id, now)" },
  // 가중치 조회가 Object.prototype 을 거쳐(`constructor` 등) NaN 이 되던 것: 자기 속성만 (crate 의 Params::weight 와 같다)
  {
    id: "fix-weight-own", worker: "cron", file: "index.ts",
    find: "(PARAMS.weights[v.kind] ?? 0)",
    replace: "(Object.hasOwn(PARAMS.weights, v.kind) ? PARAMS.weights[v.kind] : 0)",
  },
];
