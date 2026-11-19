// www 의 가짜 바인딩 (두 진입 모듈이 같이 쓰는 mocks.mjs 의 글). cron 의 mocks_src.mjs(D1 · Vectorize · 시험대 통신)를 그대로 쓰고
// fetch 처리기에 필요한 것을 덧붙인다:
//
// - 요청 제한 바인딩(EVAL_WRITE_LIMITER · EVAL_READ_LIMITER · ANALYTIC_WRITE_LIMITER): limit({ key }) 를 http://harness/rl 로.
//   어느 바인딩이 있는지는 시나리오가 정해 바인딩 `__HARNESS_LIMITERS`(쉼표 목록)로 넘기고, 워커에는 그 이름이 보이지 않는다.
// - D1 은 DB 와 apac1_premium_discount 둘 다 감싼다(문장마다 시험대에 알림).
// - 요청 하나를 감싸(harnessFetch) 시계를 머리글 x-harness-clock 으로 맞추고(그 머리글은 지우고 넘긴다), 끝에 console 기록 ·
//   D1 결과 · 밖으로 샌 예외를 시험대로 보낸다.
import { mocksSource } from "./mocks_src.mjs";

export const wwwMocksSource = () => mocksSource() + `
// 씨앗 있는 crypto.getRandomValues · crypto.randomUUID: 두 판이 같은 차례로 같은 수의 바이트를 꺼내면 같은 "무작위" 값이 나온다
// (앱 세션 hash, 2단계의 uuidv7 · randomUUID). Rust 의 getrandom(wasm_js)도 부를 때마다 globalThis.crypto.getRandomValues 를 찾는다.
// 그래서 정규화(www.mjs)는 꺼내는 차례 · 수가 달라진 경우에만 남는 안전망이다
let rseed = 20261119;
function nextByte() {
  rseed = (Math.imul(rseed, 1664525) + 1013904223) >>> 0;
  return rseed >>> 24;
}
function seededGetRandomValues(arr) {
  const u = new Uint8Array(arr.buffer, arr.byteOffset, arr.byteLength);
  for (let i = 0; i < u.length; i++) u[i] = nextByte();
  return arr;
}
function seededRandomUUID() {
  const b = seededGetRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return h.slice(0, 8) + "-" + h.slice(8, 12) + "-" + h.slice(12, 16) + "-" + h.slice(16, 20) + "-" + h.slice(20);
}
Object.defineProperty(globalThis.crypto, "getRandomValues", { value: seededGetRandomValues, configurable: true, writable: true });
Object.defineProperty(globalThis.crypto, "randomUUID", { value: seededRandomUUID, configurable: true, writable: true });
const LIMITER_NAMES = ["EVAL_WRITE_LIMITER", "EVAL_READ_LIMITER", "ANALYTIC_WRITE_LIMITER"];
function limiter(name) {
  return {
    limit: async (opts) => {
      const r = await ctl("rl", { name, opts });
      if (r.fail) throw new Error(r.fail);
      return r.result;
    },
  };
}
export function wrapWwwEnv(env) {
  const on = new Set(String(Reflect.get(env, "__HARNESS_LIMITERS") || "").split(",").filter(Boolean));
  return new Proxy(env, { get(target, prop) {
    if (prop === "__HARNESS_LIMITERS") return undefined;
    if (prop === "EVAL_VECTORIZE") return vectorize;
    if (prop === "DB" || prop === "apac1_premium_discount") return wrapD1(Reflect.get(target, prop));
    if (LIMITER_NAMES.includes(prop)) return on.has(prop) ? limiter(prop) : undefined;
    return Reflect.get(target, prop);
  } });
}
export async function harnessFetch(req, fn) {
  const at = req.headers.get("x-harness-clock");
  if (at) globalThis.__harness_set_clock(Number(at));
  const headers = new Headers(req.headers);
  headers.delete("x-harness-clock");
  const request = new Request(req, { headers, cf: req.cf });
  globalThis.__harness_logs.length = 0;
  results.length = 0;
  let exception = null;
  try {
    return await fn(request);
  } catch (e) {
    exception = { string: String(e), isError: e instanceof Error };
    throw e;
  } finally {
    await ctl("end", { logs: [...globalThis.__harness_logs], results: [...results], exception });
  }
}
`;
