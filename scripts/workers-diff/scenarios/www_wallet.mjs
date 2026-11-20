// fix-wallet-replay — 지갑 서명 한 번만 쓰기 (auth.rs verify_wallet_request_once · fixes/www_security.mjs). 지갑 서명을 받는 경로는
// /nav/trials(GET · POST · DELETE)와 /analytic/sites · /analytic/events 의 주인 쪽이다. 서명 글에는 경로만 있고 쿼리가 없어
// (contracts/wallet_v1.json), 원본은 잡은 서명을 5분 안에 다른 쿼리로 다시 보내도 받았다. 고친 판은 서명한 글의 SHA-256 을
// wallet_nonces 에 서명 시각 + 5분 + 60초(시계 차이 여유)까지 기억해 다시 쓰면 401 (틀린 서명과 같은 답).
// 모든 시나리오가 ORIGINAL=raw 에서 달라야 한다 (원본은 다시 쓴 서명을 받고 wallet_nonces 를 쓰지 않는다). 모양은 www_common.mjs 머리말.
import { createHmac } from "node:crypto";
import { ethers } from "ethers";
import { T0, SECRETS, WALLET_PHRASES, app, walletHeaders, walletMessage, walletNonce, NONCE_INSERT } from "./www_common.mjs";

const M = "wallet";
const FIX = ["fix-wallet-replay"];
const MINUTE = 60_000;
const SKEW = 5 * MINUTE;
// 키를 서명 창 뒤로 더 두는 여유 (auth.rs WALLET_NONCE_GRACE_MS) — 지우기에만 쓴다
const GRACE = MINUTE;
const APP = "0x39206c971297059b89e4e6fc35844ee87feb0407";
const APP2 = "0x" + "9".repeat(40);
const PHRASE = WALLET_PHRASES[0];
const ADDRESS = ethers.Wallet.fromPhrase(PHRASE).address.toLowerCase();
// 지갑 계정 키 (navtrials accountKey): HMAC(AUTHOR_KEY_SECRET, "nav-account|wallet|" + 주소)
const WALLET_KEY = createHmac("sha256", SECRETS.AUTHOR_KEY_SECRET).update(`nav-account|wallet|${ADDRESS}`).digest("hex");

/** 지갑으로 서명한 요청. 서명 글의 path 는 쿼리 없는 경로, time 은 서명 시각(기본 T0), at 은 받는 시각(기본 차례대로) */
async function signed(method, path, { query = "", body, time = T0, at, phrase = PHRASE } = {}) {
  const text = body === undefined ? "" : typeof body === "string" ? body : JSON.stringify(body);
  const sig = await walletHeaders(phrase, { time, method, path, body: text });
  return { method, path: path + query, headers: { ...app({ "Content-Type": "application/json" }), ...sig }, ...(text ? { body: text } : {}), ...(at !== undefined ? { at } : {}) };
}
/** 같은 머리글(서명)을 그대로 두고 경로 · 받는 시각만 바꾼 다시 보내기 */
const replay = (req, over = {}) => ({ ...req, ...over });
/** 그 요청의 한 번만 쓰기 키 */
const nonceOf = (method, path, { body = "", time = T0 } = {}) => walletNonce(walletMessage(ADDRESS, { time, method, path, body }));

const seedTrial = (app_, course) => [
  `INSERT INTO nav_trials ("account", "kind", "app", "course", "steps", "runs", "premium", "discount", "best", "last", "last_day", "touched") VALUES (?1, 'wallet', ?2, ?3, '[]', 1, 0, 0, '[0,1000]', '[0,1000]', 20409, 1)`,
  WALLET_KEY, app_, course,
];

const getMine = await signed("GET", "/nav/trials", { query: `?app=${APP}` });
const postInvalid = await signed("POST", "/nav/trials", { body: { runs: ["not a run"] } });
const sitesList = await signed("GET", "/analytic/sites");
const deleteOneApp = await signed("DELETE", "/nav/trials", { query: `?app=${APP}` });

// 같은 글의 다른 서명 꼴: v 27/28 → 0/1, 16진 대문자 (ethers · verify_message 가 모두 받는다)
const vForm = (sig) => sig.slice(0, -2) + (parseInt(sig.slice(-2), 16) - 27).toString(16).padStart(2, "0");
const upperForm = (sig) => "0x" + sig.slice(2).toUpperCase();

export default [
  {
    name: "www_wallet_replay_identical", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 같은 요청을 그대로 다시: 두 번째는 401 (GET · POST · analytic 주인 읽기). 틀린 서명처럼 D1 에 닿지 않는 401 이 아니라 키를 잡지 못한 401
    requests: [getMine, replay(getMine), postInvalid, replay(postInvalid), sitesList, replay(sitesList)],
  },
  {
    name: "www_wallet_replay_other_query", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 버그의 꼴: 잡은 DELETE /nav/trials?app=X 를 쿼리 없이 다시 보내 콜렉션 전부를 지운다 → 401, 다른 앱의 콜렉션은 남는다.
    // GET 도 쿼리만 바꿔 다른 앱의 내 콜렉션을 읽을 수 없고, analytic 주인 읽기도 같다
    sql: [seedTrial(APP, "0x" + "c".repeat(40)), seedTrial(APP2, "0x" + "d".repeat(40))],
    requests: [
      deleteOneApp,
      replay(deleteOneApp, { path: "/nav/trials" }),
      getMine,
      replay(getMine, { path: `/nav/trials?app=${APP2}` }),
      await signed("GET", "/nav/trials", { query: `?app=${APP2}`, time: T0 + 1 }),
      sitesList,
      replay(sitesList, { path: "/analytic/sites?host=shop.example.com" }),
    ],
  },
  {
    name: "www_wallet_fresh_signature", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 같은 경로 · 같은 본문이라도 새로 서명하면(시각이 다름) 받는다. 같은 시각 · 다른 메서드 · 다른 경로 · 다른 본문도 다른 글이다
    requests: [
      getMine,
      await signed("GET", "/nav/trials", { query: `?app=${APP}`, time: T0 + 1 }),
      await signed("DELETE", "/nav/trials", { query: `?app=${APP}` }),
      postInvalid,
      await signed("POST", "/nav/trials", { body: { runs: ["other"] } }),
      sitesList,
    ],
  },
  {
    name: "www_wallet_replay_after_window", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 서명 시각 + 5분(경계 포함)까지는 키가 남아 다시 쓰기를 막고, 그 뒤로는 시각 검사가 먼저 막는다(D1 에 닿지 않음).
    // 키는 60초 여유(GRACE)까지 남는다: 5분 + 1 ms 의 새 서명은 옛 키를 지우지 못하고, 5분 + 60초 + 1 ms 의 새 서명이 지운다
    // (끝 상태에는 새 키 둘)
    requests: [
      { ...getMine, at: T0 },
      replay(getMine, { at: T0 + SKEW }),
      replay(getMine, { at: T0 + SKEW + 1 }),
      await signed("GET", "/nav/trials", { query: `?app=${APP}`, time: T0 + SKEW + 1, at: T0 + SKEW + 1 }),
      replay(getMine, { at: T0 + SKEW + GRACE }),
      await signed("GET", "/nav/trials", { query: `?app=${APP}`, time: T0 + SKEW + GRACE + 1, at: T0 + SKEW + GRACE + 1 }),
    ],
  },
  {
    name: "www_wallet_signature_forms", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 키는 서명 바이트가 아니라 서명한 글이다: 같은 글의 다른 서명 꼴(v 0 · 1, 16진 대문자)로 다시 보내도 401
    requests: [
      getMine,
      replay(getMine, { headers: { ...getMine.headers, "x-pd-signature": vForm(getMine.headers["x-pd-signature"]) } }),
      replay(getMine, { headers: { ...getMine.headers, "x-pd-signature": upperForm(getMine.headers["x-pd-signature"]) } }),
    ],
  },
  {
    name: "www_wallet_replay_race", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 같은 요청 둘이 동시에: 다른 하나가 우리 키 잡기 바로 앞에 같은 키를 잡는다 → 우리는 바뀐 행 0 → 401. 둘 중 하나만 통과한다 (끝 상태에 키 하나)
    race: { match: "INSERT INTO wallet_nonces", op: "batch", before: [{ sql: NONCE_INSERT, args: [nonceOf("GET", "/nav/trials"), T0 + SKEW + GRACE] }] },
    requests: [getMine],
  },
  {
    name: "www_wallet_nonce_cleanup", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 기한 지난 키 25개 · 남은 키 3개: 받은 지갑 요청마다 지난 키를 10개까지 지운다 (25 → 15 → 5 → 0), 기한 안의 키는 남는다
    sql: [
      ...Array.from({ length: 25 }, (_, k) => [NONCE_INSERT, "e".repeat(60) + String(k).padStart(4, "0"), T0 - 1 - k]),
      ...Array.from({ length: 3 }, (_, k) => [NONCE_INSERT, "f".repeat(60) + String(k).padStart(4, "0"), T0 + MINUTE + k]),
    ],
    requests: await Promise.all([0, 1, 2].map((k) => signed("GET", "/nav/trials", { query: `?app=${APP}`, time: T0 + k, at: T0 + k }))),
  },
  {
    name: "www_wallet_nonce_d1_fault", module: M, fixes: FIX, bindings: SECRETS,
    covers: [],
    // 키 잡기 batch 가 던지면 각 경로의 try 안이라 500 (nav trials error · analytic error). 틀린 서명은 D1 에 닿지 않아 401 그대로
    faults: [{ match: "wallet_nonces", op: "batch" }],
    requests: [getMine, postInvalid, sitesList, replay(getMine, { headers: { ...getMine.headers, "x-pd-time": String(T0 + 1) } })],
  },
];
