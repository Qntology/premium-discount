// navtrials.ts — /nav/trials (세션 또는 지갑 서명). workers/www/tests/navtrials.test.ts 의 단언을 요청 차례로 옮겼고(covers 칸),
// 가장자리(게스트 · 세션 · 지갑, 콜렉션 10 · 덮어쓰기, 고스트 · 중복 카운트, 지갑 기한 · 등급, 틀린 본문, 요청 제한, CORS, D1 장애,
// 저장된 값이 이상한 행)를 더했다. 시나리오 모양은 www_common.mjs 머리말.
//
// 시험의 무작위 지갑(ethers.Wallet.createRandom)은 정해진 개인 키(keccak("pd-nav-<n>"))로 바꿨다 — 두 판이 같은 서명 · 같은 계정 키를 보도록.
// 시계: T0(2025-11-17)은 받는 날짜의 하한(2026-01-01)보다 앞이라, 시험과 같은 NOW(2026-11-17T03:00Z)로 요청마다 at 을 준다.
// 순수 함수 단언(체크포인트 id · 코스 id 기준값, tier 표, routeMarks 표, accountKey)은 workers/www/src/navtrials.rs 의 단위 시험이 맡고,
// 시험대에서는 그 값이 D1 · 응답에 그대로 나오는 것으로 함께 견준다. expireWalletTrials(cron)는 cron 시험대(cron_wallet_*)가 맡는다.
import { createHmac } from "node:crypto";
import { ethers } from "ethers";
import { SECRETS, HASH, TOKEN, ORIGIN, IP, BOTH_LIMITERS, app, creds, s3json } from "./www_common.mjs";

const M = "navtrials";
const NOW = Date.UTC(2026, 10, 17, 3, 0, 0);
const DAY_MS = 86_400_000;
const TODAY = Math.floor(NOW / DAY_MS);
const MINUTE = 60_000;
const EMAIL = "nav@example.com";
const NL = String.fromCharCode(10);

const hashId = (text) => ethers.computeAddress(ethers.hashMessage(text)).toLowerCase();
const APP = "0x39206c971297059b89e4e6fc35844ee87feb0407";
const S1 = "0x" + "1".repeat(40);
const R2 = "0x" + "2".repeat(40);
const S3_HASH = "0x" + "3".repeat(40);
const APP2 = "0x" + "9".repeat(40);
const h = (n) => "0x" + n.toString(16).padStart(40, "0");
const fill = (c) => "0x" + c.repeat(40);
const checkpointId = (s) => hashId(`${s.kind}${s.hash}${s.gesture ?? ""}`);
function courseChain(appId, steps) {
  let previous = appId;
  return steps.map((s) => (previous = hashId(`${previous}${checkpointId(s)}`)));
}
const courseHash = (appId, steps) => courseChain(appId, steps).at(-1) ?? "";
const COURSE = courseHash(APP, [{ kind: "screen", hash: S1 }, { kind: "region", hash: R2 }, { kind: "screen", hash: S3_HASH }]);
const CP_R2 = checkpointId({ kind: "region", hash: R2 });

/** trial/1 런 (navtrials.test.ts 의 run). points = [종류, 해시, 구간 시간, 제스처?] */
function run(points, over = {}, appId = APP) {
  const steps = points.map(([kind, hash, split_ms, gesture]) => (gesture ? { kind, hash, split_ms, gesture } : { kind, hash, split_ms }));
  return {
    schema: "trial/1",
    app: appId,
    course: courseHash(appId, steps.map((s) => (s.gesture && s.gesture !== "press" ? { kind: s.kind, hash: s.hash, gesture: s.gesture } : { kind: s.kind, hash: s.hash }))),
    steps,
    total_ms: steps[steps.length - 1].split_ms,
    type: null,
    day: TODAY,
    recipe: "pd-verdict/2",
    ...over,
  };
}
const lap = (r2, s3, over = {}) => run([["screen", S1, 0], ["region", R2, r2], ["screen", S3_HASH, s3]], over);
const courseOf = (i, over = {}, appId = APP) => run([["screen", h(100 + i), 0], ["screen", S3_HASH, 1000 + 100 * i]], over, appId);
const webRun = (pressGesture, over = {}) =>
  run([
    ["page", fill("a"), 0],
    pressGesture ? ["element", fill("b"), 400, pressGesture] : ["element", fill("b"), 400],
    ["page", fill("a"), 900, "scroll-down"],
    ["element", fill("c"), 1500, "swipe-left"],
    ["page", fill("d"), 2100],
  ], over);
const WEB_COURSE = webRun(null).course;

// ---- 요청 ----
const session = creds(HASH, TOKEN);
const enc = (b) => (b === undefined ? undefined : typeof b === "string" || b instanceof Uint8Array ? b : JSON.stringify(b));
/** 세션(앱 헤더) 요청 */
const sess = (method, body, at = NOW, query = "", extra = {}) => ({ method, path: "/nav/trials" + query, headers: app({ "Content-Type": "application/json", ...session, ...extra }), body: enc(body), at });
/** 인증 없는 요청 */
const plain = (method, body, at = NOW, query = "", extra = {}) => ({ method, path: "/nav/trials" + query, headers: app({ "Content-Type": "application/json", ...extra }), body: enc(body), at });
const postSession = (runs, at = NOW) => sess("POST", { runs }, at);
const getSession = (appId = APP, at = NOW) => sess("GET", undefined, at, `?app=${appId}`);
const getGuest = (appId = APP, at = NOW) => plain("GET", undefined, at, `?app=${appId}`);

/** 정해진 지갑 n (시험의 createRandom 대신) */
const W = (n) => new ethers.Wallet(ethers.id(`pd-nav-${n}`));
/** 지갑 서명 요청 (auth.ts walletRequestMessage 와 같은 글). time 은 서명 시각(기본 at), sendBody 는 서명과 다른 본문을 보낼 때 */
async function signed(w, method, body, at = NOW, { query = "", time = at, sendBody } = {}) {
  const text = body === undefined ? "" : JSON.stringify(body);
  const sha = ethers.sha256(ethers.toUtf8Bytes(text)).slice(2);
  const message = ["premium.discount request v1", `address: ${w.address.toLowerCase()}`, `time: ${time}`, `method: ${method}`, "path: /nav/trials", `body-sha256: ${sha}`].join(NL);
  const headers = app({ "Content-Type": "application/json", "x-pd-address": w.address, "x-pd-time": String(time), "x-pd-signature": await w.signMessage(message) });
  const sent = sendBody !== undefined ? sendBody : text;
  return { method, path: "/nav/trials" + query, headers, body: sent === "" ? undefined : sent, at };
}
const postWallet = (w, runs, at = NOW) => signed(w, "POST", { runs }, at);
const getWallet = (w, appId = APP, at = NOW) => signed(w, "GET", undefined, at, { query: `?app=${appId}` });

// 세션 계정 키 (씨앗 행용): HMAC(AUTHOR_KEY_SECRET, "nav-account|session|" + addressFromEmail(EMAIL))
const SESSION_KEY = createHmac("sha256", SECRETS.AUTHOR_KEY_SECRET).update(`nav-account|session|${hashId(EMAIL)}`).digest("hex");
const seedTrial = (r) => [
  `INSERT INTO nav_trials ("account", "kind", "app", "course", "steps", "runs", "premium", "discount", "best", "last", "last_day", "touched") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)`,
  r.account ?? SESSION_KEY, r.kind ?? "session", r.app ?? APP, r.course, r.steps ?? "[]", r.runs ?? 1, r.premium ?? 0, r.discount ?? 0, r.best ?? "[0,1000]", r.last ?? "[0,1000]", r.last_day ?? TODAY, r.touched ?? NOW * 1000,
];
const seedCourse = (r) => [
  `INSERT INTO nav_courses ("course", "app", "steps", "runs", "premium", "discount", "record", "sums", "last_day") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)`,
  r.course, r.app ?? APP, r.steps ?? "[]", r.runs ?? 11, r.premium ?? 0, r.discount ?? 0, r.record ?? "[0,1000]", r.sums ?? "[0,11000]", r.last_day ?? TODAY,
];
const seedCount = (hash, type, n, day = TODAY, recipe = "pd-verdict/2") => [`INSERT INTO nav_counts ("hash", "day", "type", "recipe", "n") VALUES (?1, ?2, ?3, ?4, ?5)`, hash, day, type, recipe, n];
const stepsJson = (appId, steps) => {
  const chain = courseChain(appId, steps);
  return JSON.stringify(steps.map((s, i) => ({ ...s, id: checkpointId(s), course: chain[i] })));
};
const LAP_STEPS = stepsJson(APP, [{ kind: "screen", hash: S1 }, { kind: "region", hash: R2 }, { kind: "screen", hash: S3_HASH }]);

const base = (name, extra) => ({ name: `www_navtrials_${name}`, module: M, bindings: SECRETS, s3: () => s3json({ token: TOKEN, email: EMAIL }), ...extra });

// ---- 미리 서명해 둔 요청 (서명은 결정적이다: RFC 6979) ----
const detour = (over = {}) => run([["screen", S1, 0], ["region", h(0x77), 900], ["screen", S3_HASH, 2000]], { day: TODAY + 1, ...over });
const ghostDetour = (over = {}) => run([["screen", S1, 0], ["region", h(0x55), 800], ["screen", S3_HASH, 2000]], over);
const elsewhere = run([["screen", S1, 0], ["region", R2, 900], ["screen", h(0x66), 2000]], { ghost: COURSE });
const fork = run([["screen", S1, 0], ["region", R2, 900], ["screen", h(0x66), 2000]]);
const wrongCourse = lap(1000, 3000);
wrongCourse.steps[1].course = courseChain(APP, [{ kind: "screen", hash: S1 }])[0];
const rightCourse = lap(1000, 3000);
{
  const chain = courseChain(APP, [{ kind: "screen", hash: S1 }, { kind: "region", hash: R2 }, { kind: "screen", hash: S3_HASH }]);
  rightCourse.steps.forEach((s, i) => (s.course = chain[i]));
}
const tapper = run([["page", fill("a"), 0], ["element", fill("b"), 400], ["page", fill("a"), 900, "scroll-down"], ["element", fill("c"), 1500], ["page", fill("d"), 2100]], { ghost: WEB_COURSE });

const W0 = W(0);
const idsNewRepeat = [
  await postWallet(W0, [lap(2000, 4000, { type: "discount" })]),
  await postWallet(W0, [lap(1000, 2500, { type: "premium" })], NOW + MINUTE),
  await postWallet(W0, [lap(3000, 6000, { day: TODAY + 1 })], NOW + DAY_MS),
  await postWallet(W0, [detour()], NOW + DAY_MS),
  await getWallet(W0, APP, NOW + DAY_MS),
];

const publishWallets = Array.from({ length: 11 }, (_, i) => W(100 + i));
const typeOf = (i) => (i < 8 ? "premium" : i < 10 ? "discount" : null);
const publishReqs = [
  ...(await Promise.all(Array.from({ length: 10 }, (_, i) => postWallet(publishWallets[i], [lap(1000 + 100 * i, 3000 + 200 * i, { type: typeOf(i) })])))),
  getGuest(),
  await getWallet(publishWallets[0]),
  await postWallet(publishWallets[10], [lap(2000, 5000, { type: typeOf(10) })]),
  getGuest(),
  await getWallet(publishWallets[3]),
  getGuest(APP2),
];

const expiryWallet = W(200);
const expiryReqs = [
  await postWallet(expiryWallet, [courseOf(0), courseOf(1, { day: TODAY - 29 }), courseOf(2, { day: TODAY - 30 })]),
  await postWallet(expiryWallet, [courseOf(3, { day: TODAY + 1 })], NOW + DAY_MS),
  postSession([courseOf(5, { day: TODAY - 200 })], NOW + DAY_MS),
  // 지갑 날짜의 가장자리: 오늘 − 29 는 남고 − 30 은 기한 밖, 내일은 받고 모레는 형식 오류
  await postWallet(expiryWallet, [courseOf(6, { day: TODAY + 1 - 29 }), courseOf(7, { day: TODAY + 1 - 30 }), courseOf(8, { day: TODAY + 2 }), courseOf(9, { day: TODAY + 3 })], NOW + DAY_MS + 1000),
  await getWallet(expiryWallet, APP, NOW + DAY_MS + 2000),
];

const delA = W(300);
const delB = W(301);
const deleteReqs = [
  await postWallet(delA, [courseOf(0), courseOf(1), courseOf(0, {}, APP2)]),
  await postWallet(delB, [courseOf(0)]),
  await signed(delA, "DELETE", undefined, NOW, { query: `?app=${APP}` }),
  // 앞 요청과 같은 글이 되지 않게 1 ms 뒤에 새로 서명한다 (같은 서명으로 쿼리만 바꾸면 fix-wallet-replay 가 막는다 — www_wallet.mjs)
  await signed(delA, "DELETE", undefined, NOW, { time: NOW + 1 }),
  await postWallet(delA, [courseOf(0)], NOW + MINUTE),
  postSession([courseOf(4)]),
  sess("DELETE", undefined),
  // 지운 뒤 다시 지우면 0
  sess("DELETE", undefined, NOW + 1000, `?app=${APP}`),
];

const authWallet = W(400);
const authReqs = [
  plain("POST", { runs: [lap(1000, 3000)] }),
  // 다른 본문에 대한 서명 · 5분 지난 서명
  await signed(authWallet, "POST", { runs: [lap(1000, 3000)] }, NOW, { sendBody: JSON.stringify({ runs: [lap(1000, 2000)] }) }),
  await signed(authWallet, "POST", { runs: [lap(1000, 3000)] }, NOW + 5 * MINUTE + 1, { time: NOW }),
  // 5분 안의 앞뒤(경계 포함)는 받는다
  await signed(authWallet, "POST", { runs: [lap(1000, 3000)] }, NOW + 5 * MINUTE, { time: NOW }),
  await signed(authWallet, "POST", { runs: [lap(1000, 3100)] }, NOW, { time: NOW + 5 * MINUTE }),
  // 세션이 맞아도 지갑 헤더가 틀리면 받지 않는다 (GET 도)
  sess("POST", { runs: [lap(1000, 3000)] }, NOW, "", { "x-pd-address": authWallet.address }),
  plain("GET", undefined, NOW, `?app=${APP}`, { "x-pd-address": authWallet.address }),
  // 빈 지갑 머리글도 "있음"이다
  plain("GET", undefined, NOW, `?app=${APP}`, { "x-pd-signature": "" }),
  sess("GET", undefined),
  sess("GET", undefined, NOW, "?app=shop.example.com"),
  postSession([]),
  postSession(Array.from({ length: 11 }, (_, i) => courseOf(i))),
  sess("POST", '{"runs":'),
  sess("PUT", undefined),
  sess("HEAD", undefined),
  sess("PATCH", "{}"),
  plain("OPTIONS", undefined),
  { method: "POST", path: "/nav/marks", headers: app() },
  { method: "GET", path: "/nav/trials/", headers: app() },
];

const routeWalkers = [0, 1, 2].map((i) => W(500 + i));
const routeQuiet = W(510);
const routeReqs = [
  ...(await Promise.all(Array.from({ length: 11 }, (_, i) => postWallet(W(520 + i), [lap(1000, 3000)])))),
  ...(await Promise.all(routeWalkers.map((w) => postWallet(w, [ghostDetour({ ghost: COURSE })])))),
  await postWallet(W(511), [lap(1100, 2900, { ghost: COURSE })]),
  await postWallet(routeWalkers[0], [ghostDetour({ ghost: COURSE })], NOW + MINUTE),
  await postWallet(routeQuiet, [elsewhere, courseOf(1, { ghost: courseOf(0).course }), courseOf(2, { ghost: COURSE }, APP2)]),
  await postWallet(routeQuiet, [courseOf(3)]),
  await postWallet(routeQuiet, [courseOf(4, { ghost: "nope" })]),
  getGuest(),
];

const gestureReqs = [
  ...(await Promise.all(Array.from({ length: 11 }, (_, i) => postWallet(W(600 + i), [webRun(null)])))),
  await postWallet(W(611), [tapper]),
  getGuest(),
];

const chainA = W(700);
const chainB = W(701);
const chainReqs = [
  await postWallet(chainA, [lap(1000, 3000)]),
  await postWallet(chainB, [lap(1200, 2800)]),
  await postWallet(chainA, [fork]),
  await getWallet(chainA),
  await postWallet(chainA, [wrongCourse]),
  await postWallet(chainA, [rightCourse]),
];

// 섞인 요청 하나: 형식 오류 · 기한 밖 · 같은 코스 두 번 · 벡터(같은 해시 · 다른 체크포인트) · 고스트(공개 전) · 다른 앱
const mixWallet = W(800);
const vecRun = lap(1000, 3000, { type: "premium" });
vecRun.steps[0].vectors = [{ space: "granite-97m-r2-cls-v1", vector: [0.6, 0.8] }, { space: "siglip2.patch_v1", vector: [-0, 1] }];
vecRun.steps[2].vectors = [{ space: "granite-97m-r2-cls-v1", vector: [1] }];
const sameHashRun = run([["page", fill("a"), 0], ["page", fill("a"), 500, "scroll-down"], ["page", fill("e"), 900]]);
sameHashRun.steps[0].vectors = [{ space: "a", vector: [1] }];
sameHashRun.steps[1].vectors = [{ space: "a", vector: [0, -1] }];
const mixReqs = [
  await postWallet(mixWallet, [
    "not a run",
    vecRun,
    lap(900, 2000, { type: "discount" }),
    courseOf(1, { day: TODAY - 40 }),
    sameHashRun,
    courseOf(2, { ghost: COURSE, type: "premium" }),
    courseOf(3, {}, APP2),
    lap(800, 1500),
  ]),
  await getWallet(mixWallet),
  // 앞 GET 과 같은 시각이면 서명 글이 같다(쿼리는 글에 없다): 1 ms 뒤에 새로 서명한다
  await getWallet(mixWallet, APP2, NOW + 1),
];

// 벡터 · 구간 가장자리
const vec = (vectors, over = {}) => {
  const r = lap(1000, 3000, over);
  r.steps[1].vectors = vectors;
  return r;
};
const vectorEdges = [
  vec(null),
  vec({}),
  vec([1, 2]),
  vec(Array.from({ length: 5 }, (_, i) => ({ space: `s${i}`, vector: [1] }))),
  vec([{ space: "Granite", vector: [1] }]),
  vec([{ space: "a".repeat(65), vector: [1] }]),
  vec([{ space: "-a", vector: [1] }]),
  vec([{ space: "a", vector: [1] }, { space: "a", vector: [1] }]),
  vec([{ space: "a", vector: [] }]),
];
const vectorEdges2 = [
  vec([{ space: "a", vector: Array.from({ length: 2049 }, (_, i) => (i === 0 ? 1 : 0)) }]),
  vec([{ space: "a", vector: [1, "0"] }]),
  vec([{ space: "a", vector: [1e39] }]),
  vec([{ space: "a", vector: [1.0009] }]),
  vec([{ space: "a", vector: [1.0011] }]),
  vec([{ space: "a".repeat(64), vector: Array.from({ length: 2048 }, (_, i) => (i === 7 ? -1 : 0)) }]),
  run([["screen", S1, -0], ["screen", S3_HASH, 0]], { total_ms: -0 }),
  run([["screen", S1, 0], ["screen", S3_HASH, 3_600_000]]),
  run([["screen", S1, 0], ["screen", S3_HASH, 1e21]]),
  run([["screen", S1, 0], ["screen", S3_HASH, 150]]),
];
const formEdges = [
  lap(1000, 3000, { recipe: "pd-verdict/1" }),
  lap(1000, 3000, { app: "0x" + "A".repeat(40) }),
  lap(1000, 3000, { app: " 0X" + APP.slice(2).toUpperCase() + NL }),
  lap(1000, 3000, { day: TODAY + 0.5 }),
  lap(1000, 3000, { day: String(TODAY) }),
  lap(1000, 3000, { day: 20453 }),
  lap(1000, 3000, { type: "premium", ghost: null }),
  lap(1000, 3000, { steps: "abc" }),
  lap(1000, 3000, { course: COURSE.toUpperCase().replace("0X", "0x") }),
  lap(1000, 3000, { total_ms: "3000" }),
];
const stepEdges = (() => {
  const s = (mut) => {
    const r = lap(1000, 3000);
    mut(r.steps);
    return r;
  };
  return [
    s((st) => (st[1] = null)),
    s((st) => (st[1] = [])),
    s((st) => (st[1].hash = 7)),
    s((st) => (st[1].gesture = null)),
    s((st) => (st[1].gesture = "press")),
    s((st) => (st[1].id = null)),
    s((st) => (st[1].id = CP_R2.toUpperCase())),
    s((st) => (st[1].course = null)),
    s((st) => (st[1].split_ms = 1000.5)),
    s((st) => (st[1].split_ms = "1000")),
  ];
})();

// 아주 깊은 본문 (원본은 그 깊이에서 객체인지만 본다)
const deep = (n, leaf) => "[".repeat(n) + leaf + "]".repeat(n);
const deepId = (n) => {
  const r = JSON.stringify(lap(1000, 3000));
  return r.replace(`"split_ms":1000`, `"split_ms":1000,"id":${deep(n, "1")}`);
};

const tierRows = [
  seedTrial({ course: h(0x901), premium: 2, discount: 0, touched: NOW * 1000 + 1 }),
  seedTrial({ course: h(0x902), premium: 2, discount: 1, touched: NOW * 1000 + 2 }),
  seedTrial({ course: h(0x903), premium: 3, discount: 2, touched: NOW * 1000 + 3 }),
  seedTrial({ course: h(0x904), premium: 0, discount: 3, touched: NOW * 1000 + 4, best: "[0,1e400]", last: '["0","x"]' }),
  // 공개 코스 셋 (많이 달린 순 · 같은 수는 course 순) 과 공개 안 된 것, 다른 앱
  seedCourse({ course: h(0xa01), runs: 12, premium: 8, discount: 1, steps: LAP_STEPS, record: "[0,900,2900]", sums: "[0,13000,40000]" }),
  seedCourse({ course: h(0xa02), runs: 11, premium: 3, discount: 3, steps: stepsJson(APP, [{ kind: "page", hash: fill("a") }, { kind: "page", hash: fill("a"), gesture: "scroll-down" }]), sums: "[0,5555]" }),
  seedCourse({ course: h(0xa00), runs: 11, premium: 0, discount: 9, steps: JSON.stringify([{ kind: "screen", hash: S1 }, { kind: "region", hash: R2, gesture: "swipe-up" }]), sums: '[0,"7",-1,null,1e400]' }),
  seedCourse({ course: h(0xa03), runs: 10, steps: LAP_STEPS }),
  seedCourse({ course: h(0xa04), app: APP2, runs: 50, steps: LAP_STEPS }),
  // 체크포인트 판정 수: 따라감 · 비켜 감 (다른 규칙의 수는 세지 않는다)
  seedCount(CP_R2, 1, 2),
  seedCount(CP_R2, 1, 1, TODAY - 3),
  seedCount(CP_R2, -1, 1),
  seedCount(checkpointId({ kind: "screen", hash: S1 }), -1, 4),
  seedCount(checkpointId({ kind: "screen", hash: S1 }), 1, 9, TODAY, "pd-verdict/1"),
  seedCount(checkpointId({ kind: "region", hash: R2, gesture: "swipe-up" }), 1, 5),
];

// 내 콜렉션의 공개 코스(많이 달린 10개 밖)는 others 에 더한다
const manyPublished = Array.from({ length: 11 }, (_, i) => seedCourse({ course: h(0xb00 + i), runs: 30 - i, steps: LAP_STEPS, record: "[0,1000,3000]", sums: `[0,${1000 * (30 - i)},1e400]` }));

// 덮어쓰기: 10개가 찬 콜렉션 (touched 는 미래 — 같은 요청에서 고친 행이 가장 오래된 것이 되어 밀려나는 가장자리)
const fullCollection = (touched) => Array.from({ length: 10 }, (_, i) => seedTrial({ course: courseOf(i).course, steps: stepsJson(APP, [{ kind: "screen", hash: h(100 + i) }, { kind: "screen", hash: S3_HASH }]), touched: touched(i), best: `[0,${1000 + 100 * i}]`, last: `[0,${1000 + 100 * i}]` }));

// ---- fix-nav-lost-update: 동시에 들어온 쓰기 (race 의 before 가 다른 요청이 그 사이 쓴 것을 끼워 넣는다) ----
const asRace = ([sql, ...args]) => ({ sql, args });
const REGISTER_SQL = `INSERT INTO nav_registrations ("id", "day") VALUES (?1, ?2)`;
const SESSION_REG = hashId(SESSION_KEY + COURSE);
/** 합계 행을 바꾸는 다른 요청: runs 하나 더하고 sums 글을 바꾼다(공백을 붙여도 JSON 이다) — 조건 건 갱신이 늘 진다 */
const bumpCourse = { sql: `UPDATE nav_courses SET "runs" = "runs" + 1, "sums" = "sums" || ' ' WHERE "course" = ?1`, args: [COURSE] };

const raceScenarios = [
  base("race_first_registration", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 같은 새 코스의 첫 등록 둘 (다른 사용자): 다른 사용자의 첫 등록이 우리 batch 바로 앞에 들어온다.
    // 원본은 upsert 의 절대값(runs 1)으로 덮어 하나를 잃는다. 고친 판은 INSERT 가 지고(created → false) 다시 읽어 더한다(runs 2)
    race: {
      match: "nav_courses", op: "batch",
      before: [
        asRace(seedCourse({ course: COURSE, steps: LAP_STEPS, runs: 1, premium: 1, record: "[0,1200,2800]", sums: "[0,1200,2800]" })),
        { sql: REGISTER_SQL, args: [hashId("other-account" + COURSE), TODAY] },
        asRace(seedCount(COURSE, 1, 1)),
      ],
    },
    requests: [postSession([lap(1000, 3000, { type: "premium" })]), getSession(APP, NOW + 1000)],
  }),
  base("race_existing_course_update", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 있는 코스(runs 5)를 다른 사용자가 그 사이 고쳤다(runs 6 · sums). 원본은 읽은 값 + 1 = 6 으로 덮는다. 고친 판은 조건(record · sums 글)이
    // 맞지 않아 다시 읽고 다시 계산한다: runs 7, sums 는 둘 다 더한 값, record 는 더 빠른 우리 런
    sql: [seedCourse({ course: COURSE, steps: LAP_STEPS, runs: 5, record: "[0,1000,3000]", sums: "[0,5000,15000]" })],
    race: { match: "nav_courses", op: "batch", before: [{ sql: `UPDATE nav_courses SET "runs" = 6, "sums" = '[0,6200,18100]' WHERE "course" = ?1`, args: [COURSE] }] },
    requests: [postSession([lap(900, 2500)])],
  }),
  base("race_same_user_identical_runs", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 같은 사용자의 같은 요청 둘: 다른 하나가 등록 · 콜렉션 · 합계 · 판정 수를 모두 쓴 뒤 우리가 등록을 잡는다.
    // 원본은 등록을 SELECT 로 미리 봐 둘 다 처음 등록으로 세고(nav_counts 2) 콜렉션 runs 를 1 로 덮는다.
    // 고친 판은 등록을 잡지 못해 duplicate(합계 · 판정 수에 다시 넣지 않음), 콜렉션은 INSERT 가 져 다시 읽어 더한다(runs 2 — 내 기록은 런마다)
    race: {
      match: "nav_registrations", op: "batch",
      before: [
        { sql: REGISTER_SQL, args: [SESSION_REG, TODAY] },
        asRace(seedTrial({ course: COURSE, steps: LAP_STEPS, runs: 1, premium: 1, best: "[0,1000,3000]", last: "[0,1000,3000]", touched: NOW * 1000 })),
        asRace(seedCourse({ course: COURSE, steps: LAP_STEPS, runs: 1, premium: 1, record: "[0,1000,3000]", sums: "[0,1000,3000]" })),
        asRace(seedCount(COURSE, 1, 1)),
      ],
    },
    requests: [postSession([lap(1000, 3000, { type: "premium" })]), getSession(APP, NOW + 1000)],
  }),
  base("race_retry_exhausted", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 합계 행이 batch 앞에서도, 다시 읽을 때마다도 바뀐다: 3번 다시 시도한 뒤 던져 500 (console 에 같은 글). 그때 batch 의 나머지
    // (콜렉션 · 등록 · 판정 수)는 이미 쓰였고 합계에는 이 런이 없다. 원본은 200 이지만 끼어든 수 넷을 덮어 잃는다
    sql: [seedCourse({ course: COURSE, steps: LAP_STEPS, runs: 5, record: "[0,1000,3000]", sums: "[0,5000,15000]" })],
    race: [
      { match: "nav_courses", op: "batch", before: [bumpCourse] },
      { match: `FROM nav_courses WHERE "course" = ?1`, op: "first", times: 3, after: [bumpCourse] },
    ],
    requests: [postSession([lap(900, 2500, { type: "discount" })]), getSession(APP, NOW + 1000)],
  }),
  base("race_trial_row_update", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 같은 사용자가 이미 달린 코스를 두 요청이 동시에 다시 달린다: 다른 하나가 그 사이 runs 4 · best 2800 으로 고쳤다.
    // 원본은 읽은 runs 3 + 1 = 4 로 덮어 하나를 잃는다. 고친 판은 best 글이 달라 다시 읽고, runs 5 · best 는 더 빠른 우리 런(2500)
    sql: [
      seedTrial({ course: COURSE, steps: LAP_STEPS, runs: 3, best: "[0,1000,3000]", last: "[0,1000,3000]", touched: NOW * 1000 - 5000 }),
      [REGISTER_SQL, SESSION_REG, TODAY],
    ],
    race: { match: "nav_trials", op: "batch", before: [{ sql: `UPDATE nav_trials SET "runs" = 4, "best" = '[0,950,2800]', "last" = '[0,950,2800]', "touched" = ?1 WHERE "course" = ?2`, args: [NOW * 1000 - 1, COURSE] }] },
    requests: [postSession([lap(900, 2500)]), getSession(APP, NOW + 1000)],
  }),
  base("race_eviction_retouched", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 콜렉션 10개가 찬 사용자의 새 코스가 가장 오래된 코스(courseOf(0))를 밀어내려는 사이, 다른 요청이 그 코스를 다시 달렸다.
    // 원본은 방금 달린 코스를 지운다. 고친 판은 지우기가 지고(touched 가 다름) 다시 읽어 그다음 오래된 코스(courseOf(1))를 밀어낸다(evicted)
    sql: fullCollection((i) => NOW * 1000 - 1000 + i),
    race: { match: "nav_trials", op: "batch", before: [{ sql: `UPDATE nav_trials SET "runs" = "runs" + 1, "touched" = ?1 WHERE "course" = ?2`, args: [NOW * 1000 + 500, courseOf(0).course] }] },
    requests: [postSession([courseOf(50)]), getSession(APP, NOW + 1000)],
  }),
  base("batch_fault_releases_registration", {
    covers: [],
    fixes: ["fix-nav-lost-update"],
    // 쓰기 batch 가 실패하면(500) 잡은 등록을 되돌려, 같은 런을 다시 보내면 처음 등록으로 센다 (원본은 등록도 같은 batch 라 같은 결과)
    faults: [{ match: "INSERT INTO nav_trials", op: "batch", nth: 1 }],
    requests: [postSession([lap(1000, 3000, { type: "premium" })]), postSession([lap(1000, 3000, { type: "premium" })], NOW + 1000)],
  }),
];

export default [
  base("ids_new_and_repeat", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: [
      "navtrials.test.ts: 체크포인트 id · 코스 id 는 기기(crates/premium-discount-nav)와 같은 기준값이다 (코스 id · steps 의 id · course 가 응답 · D1 에)",
      "navtrials.test.ts: 코스 id 가 없으면 새 코스를 만들고, 같은 사용자가 같은 코스를 다시 달리면 등록하지 않는다 (내 기록만 고친다)",
      "navtrials.test.ts: 계정 키는 갈래마다 다르고 공개 작성자 키와도 맞춰 볼 수 없다 (nav_trials.account)",
    ],
    requests: idsNewRepeat,
  }),
  base("collection_limit", {
    fixes: ["fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 앱 · 웹마다 코스 10개: 11번째 새 코스는 가장 오래 달리지 않은 코스를 덮어쓴다 (첫 환경)"],
    requests: [
      postSession(Array.from({ length: 10 }, (_, i) => courseOf(i))),
      postSession([courseOf(0)], NOW + MINUTE),
      postSession([courseOf(10)], NOW + 2 * MINUTE),
      getSession(APP, NOW + 2 * MINUTE),
      postSession([courseOf(0, {}, APP2)], NOW + 2 * MINUTE + 1000),
      getSession(APP, NOW + 2 * MINUTE + 2000),
      getSession(APP2, NOW + 2 * MINUTE + 2000),
      postSession([courseOf(1)], NOW + 3 * MINUTE),
    ],
  }),
  base("collection_burst", {
    fixes: ["fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 앱 · 웹마다 코스 10개 — 한 요청 안에서 넘쳐도 같다: 다음 요청의 새 코스가 앞 요청의 첫 코스를 밀어낸다 (두 번째 환경)"],
    requests: [postSession(Array.from({ length: 10 }, (_, i) => courseOf(20 + i))), postSession([courseOf(30)], NOW + MINUTE)],
  }),
  base("collection_evicts_touched_in_request", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    // 시계가 뒤로 간 경우: 이 요청에서 다시 달린 코스(touched 가 작아짐)가 가장 오래된 것이 되어 같은 요청의 새 코스에 밀려난다 (dirty 에서 빠짐)
    sql: fullCollection((i) => (NOW + DAY_MS) * 1000 + i),
    requests: [postSession([courseOf(5), courseOf(40), courseOf(41)]), getSession()],
  }),
  base("collection_readd_evicted", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    // 밀려난 코스를 같은 요청에서 다시 달린다: DELETE 뒤 upsert 가 새 행(runs 1)으로 되살린다
    sql: fullCollection((i) => NOW * 1000 - 1000 + i),
    requests: [postSession([courseOf(50), courseOf(0), courseOf(50)]), getSession()],
  }),
  base("no_receive_limit", {
    fixes: ["fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 받는 수 제한은 없다: 같은 분에 런 150개를 보내도 모두 받지만, 한 사용자는 같은 코스를 한 번만 등록한다"],
    requests: [...Array.from({ length: 15 }, (_, k) => postSession(Array.from({ length: 10 }, (_, i) => courseOf(i)), NOW + k)), getSession()],
  }),
  base("invalid_runs", {
    fixes: ["fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 플로우는 체크포인트 10개까지이고, 형식이 틀린 런은 invalid 다"],
    requests: (() => {
      const eleven = run(Array.from({ length: 11 }, (_, i) => ["screen", h(i + 1), i * 100]));
      const tenPoints = run(Array.from({ length: 10 }, (_, i) => ["screen", h(i + 1), i * 100]));
      const bad = [
        eleven,
        lap(1000, 3000, { course: h(7) }),
        lap(1050, 3000),
        run([["screen", S1, 100], ["screen", S3_HASH, 300]]),
        lap(3000, 2000),
        run([["screen", S1, 0], ["screen", S1, 100], ["screen", S3_HASH, 300]]),
        lap(1000, 3000, { total_ms: 2000 }),
        lap(1000, 3000, { type: "neutral" }),
        lap(1000, 3000, { type: undefined }),
        lap(1000, 3000, { schema: "trial/2" }),
        lap(1000, 3000, { day: TODAY + 2 }),
        lap(1000, 3_600_100),
        run([["screen", S1, 0]]),
        "not a run",
      ];
      const withVectors = lap(1000, 3000);
      withVectors.steps[1].vectors = [{ space: "granite-97m-r2-cls-v1", vector: [0.6, 0.8] }];
      const notUnit = lap(1000, 3000);
      notUnit.steps[1].vectors = [{ space: "granite-97m-r2-cls-v1", vector: [3, 4] }];
      return [postSession(bad.slice(0, 7)), postSession(bad.slice(7)), postSession([eleven]), postSession([notUnit, tenPoints, withVectors])];
    })(),
  }),
  base("run_form_edges", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    requests: [postSession(formEdges), postSession(stepEdges, NOW + 1000), postSession(vectorEdges, NOW + 2000), postSession(vectorEdges2, NOW + 3000), getSession(APP, NOW + 4000)],
  }),
  base("publish_ghosts", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 다른 사용자의 타임트라이얼: 11번 넘게 달린 코스만 보이고, 중복 카운트 · 등급 · 최고 기록 · 평균 고스트를 준다"],
    requests: publishReqs,
  }),
  base("wallet_expiry", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 지갑 코스는 마지막으로 달린 날 기준 30일까지, 세션 코스는 기한 없이 둔다 (계정 없는 합계는 남는다) — www 쪽 (cron 의 expireWalletTrials 는 cron 시험대)"],
    requests: expiryReqs,
  }),
  base("delete", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: ["navtrials.test.ts: DELETE 는 내 콜렉션(앱 하나 또는 전부)만 지우고 계정 없는 합계는 남긴다"],
    requests: deleteReqs,
  }),
  base("auth_and_format", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 인증 · 요청 형식"],
    requests: authReqs,
  }),
  base("ghost_route", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: [
      "navtrials.test.ts: 다른 사용자의 코스를 따라 달리면 체크포인트에 premium, 다른 길로 가면 고르지 않은 체크포인트에 discount 가 쌓이고 고스트에 보인다",
      "navtrials.test.ts: 고스트 따라가기 판정: 따라간 체크포인트는 premium, 고르지 않은 체크포인트는 discount (Rust route_marks 와 같은 표 — 표 자체는 단위 시험)",
    ],
    requests: routeReqs,
  }),
  base("web_course", {
    fixes: ["fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 웹 코스는 페이지 · 요소와 제스처로 정하고, 코스 해시는 기기(Rust)와 같다"],
    requests: (() => {
      const pressed = webRun("press");
      pressed.course = WEB_COURSE;
      const bad = [
        run([["page", fill("a"), 0], ["link", fill("b"), 100]]),
        run([["page", fill("a"), 0], ["element", fill("b"), 100, "pinch"]]),
        run([["page", fill("a"), 0, "scroll-down"], ["page", fill("a"), 100, "scroll-down"]]),
      ];
      const wrongId = webRun(null);
      wrongId.steps[1].id = CP_R2;
      const rightId = webRun(null);
      rightId.steps[1].id = checkpointId({ kind: "element", hash: fill("b") });
      return [postSession([webRun(null), pressed]), getSession(), postSession(bad), postSession([wrongId]), postSession([rightId])];
    })(),
  }),
  base("ghost_gesture", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 고스트 따라가기 판정은 제스처까지 본다: 스와이프할 곳을 눌러 지나면 그 체크포인트는 비켜 간 것이다"],
    requests: gestureReqs,
  }),
  base("same_course_chain", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: ["navtrials.test.ts: 같은 길은 누가 달려도 같은 코스 id 라 두 번 등록되지 않고, 다른 체크포인트가 나오면 이전 코스 id 에 이어 새 코스가 생긴다"],
    requests: chainReqs,
  }),
  base("mixed_request", {
    fixes: ["fix-wallet-replay", "fix-nav-lost-update"],
    covers: [],
    requests: mixReqs,
  }),
  base("tiers_and_tallies", {
    covers: [],
    // 내 콜렉션의 등급(unrated · premium · neutral · discount), 공개 코스의 등급 · 평균(반올림 · 글 · null · Infinity) · 체크포인트 판정 수(id 없는 저장 steps 는 checkpointId 로)
    sql: tierRows,
    requests: [getSession(), getGuest(), getGuest(APP2)],
  }),
  base("others_include_mine", {
    covers: [],
    // 많이 달린 10개 밖의 공개 코스라도 내 콜렉션에 있으면 others 에 들어온다 (JSON.parse → stringify → parse 를 거친 값)
    sql: [...manyPublished, seedTrial({ course: h(0xb0a), steps: LAP_STEPS }), seedTrial({ course: h(0xb00), steps: LAP_STEPS, touched: NOW * 1000 - 5 }), seedTrial({ course: h(0xa03), steps: LAP_STEPS, touched: NOW * 1000 - 9 })],
    requests: [getSession(), getGuest()],
  }),
  base("ghost_route_stored_shapes", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    // 저장된 고스트 steps 의 모양: [] (판정 없음 = 빈 판정) · 글(시작이 다름) · 공개 하한 경계 · 다른 앱 · 같은 코스를 고스트로
    sql: [
      seedCourse({ course: h(0xc01), steps: "[]" }),
      seedCourse({ course: h(0xc02), steps: '"abc"' }),
      seedCourse({ course: h(0xc03), steps: LAP_STEPS, runs: 10 }),
      seedCourse({ course: h(0xc04), steps: LAP_STEPS, runs: "11" }),
      seedCourse({ course: h(0xc05), steps: LAP_STEPS, app: APP2 }),
      seedCourse({ course: COURSE, steps: LAP_STEPS, runs: 11, record: "[0,1000,3000]", sums: "[0,11000,33000]" }),
      seedCourse({ course: h(0xc06), steps: JSON.stringify([{ kind: "screen", hash: S1 }, { kind: "region", hash: R2 }, { kind: "region", hash: R2 }, { kind: "screen", hash: S3_HASH }]) }),
    ],
    requests: [
      postSession([
        courseOf(1, { ghost: h(0xc01) }),
        courseOf(2, { ghost: h(0xc02) }),
        courseOf(3, { ghost: h(0xc03) }),
        lap(1000, 3000, { ghost: h(0xc04) }),
        lap(900, 2000, { ghost: h(0xc05) }),
        courseOf(4, { ghost: h(0xcff) }),
        lap(800, 1800, { ghost: h(0xc06) }),
      ]),
      postSession([ghostDetour({ ghost: COURSE, type: "discount" }), run([["screen", S1, 0], ["screen", S3_HASH, 500]], { ghost: COURSE })], NOW + 1000),
      getGuest(APP, NOW + 2000),
    ],
  }),
  base("ghost_route_bad_steps", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    // 저장된 고스트 steps 가 객체면 원본은 stepKey(undefined) 에서 TypeError → 500 (console 기록까지 같아야 한다)
    sql: [seedCourse({ course: h(0xc10), steps: "{}" })],
    requests: [postSession([courseOf(1, { ghost: h(0xc10) })])],
  }),
  base("ghost_route_bad_json", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    sql: [seedCourse({ course: h(0xc11), steps: "[{" })],
    requests: [postSession([courseOf(1, { ghost: h(0xc11) })]), postSession([courseOf(2)], NOW + 1000)],
  }),
  base("stored_rows_odd", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    // 내 콜렉션의 best 가 null 이면 다시 달릴 때 TypeError → 500, 글 runs 는 JS 처럼 이어 붙고, 합계의 sums 가 배열이 아니면 TypeError
    sql: [
      seedTrial({ course: courseOf(0).course, steps: "[]", best: "null" }),
      seedTrial({ course: courseOf(1).course, steps: "[]", runs: "2", premium: "x", best: '"9"', last_day: TODAY - 400 }),
      seedCourse({ course: courseOf(2).course, sums: "5", runs: 3 }),
      seedCourse({ course: courseOf(3).course, record: "[]", sums: '["1","2"]', runs: 3, steps: "[]" }),
    ],
    requests: [
      postSession([courseOf(0)]),
      postSession([courseOf(1, { type: "premium" })], NOW + 1000),
      postSession([courseOf(2)], NOW + 2000),
      postSession([courseOf(3, { type: "discount" })], NOW + 3000),
      getSession(APP, NOW + 4000),
    ],
  }),
  base("stored_rows_bad_json_get", {
    covers: [],
    sql: [seedTrial({ course: h(0xd01), steps: "[1,]" })],
    requests: [getSession(), getGuest()],
  }),
  base("guest_session_wallet_get", {
    fixes: ["fix-wallet-replay"],
    covers: [],
    sql: [seedCourse({ course: COURSE, steps: LAP_STEPS, runs: 11 }), seedTrial({ course: COURSE, steps: LAP_STEPS }), seedTrial({ course: h(0xe01), account: "someone-else", steps: LAP_STEPS })],
    requests: [getGuest(), getSession(), await getWallet(W(900)), { ...getSession(), headers: app({ Cookie: `hash=${HASH}; token=${TOKEN}` }) }],
  }),
  base("session_states", {
    covers: [],
    // 마지막 요청은 앞뒤 공백이 붙은 이메일 — 웹 세션의 trim (1단계 fix-web-session-email-trim)이 계정 키를 바꾼다
    fixes: ["fix-web-session-email-trim", "fix-nav-lost-update"],
    // 세션 확인이 실패하는 갈래: 토큰 다름 · 이메일 없음 · S3 오류(재시도) · 익명 자리값(게스트)
    s3: ({ n }) => (n === 1 ? s3json({ token: "ef".repeat(32), email: EMAIL }) : n === 2 ? s3json({ token: TOKEN }) : n <= 4 ? { status: 403, body: "" } : s3json({ token: TOKEN, email: " " + EMAIL })),
    requests: [
      postSession([courseOf(0)]),
      getSession(),
      postSession([courseOf(0)], NOW + 1000),
      getSession(APP, NOW + 1000),
      sess("GET", undefined, NOW + 2000, `?app=${APP}`, creds("0x" + "0".repeat(40), "0x" + "0".repeat(64))),
      postSession([courseOf(0)], NOW + 3000),
    ],
  }),
  base("session_s3_unavailable", {
    covers: [],
    bindings: { ...SECRETS, SESSION_S3_BUCKET: undefined },
    requests: [postSession([courseOf(0)]), getSession(), getGuest()],
  }),
  base("secret_missing", {
    fixes: ["fix-wallet-replay"],
    covers: [],
    // AUTHOR_KEY_SECRET 이 없으면 계정 키에서 던진다 → try 안이라 500 · console 기록. 게스트 조회는 그대로
    bindings: { ...SECRETS, AUTHOR_KEY_SECRET: undefined },
    requests: [await postWallet(W(910), [courseOf(0)]), postSession([courseOf(0)]), getSession(), getGuest(), sess("DELETE", undefined)],
  }),
  base("secret_empty", {
    fixes: ["fix-wallet-replay"],
    covers: [],
    bindings: { ...SECRETS, AUTHOR_KEY_SECRET: "" },
    requests: [await getWallet(W(911)), getGuest()],
  }),
  base("malformed_bodies", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    requests: [
      sess("POST", "[]"),
      sess("POST", "null", NOW + 1),
      sess("POST", "5", NOW + 2),
      sess("POST", '"runs"', NOW + 3),
      sess("POST", '{"runs":{}}', NOW + 4),
      sess("POST", '{"runs":"abc"}', NOW + 5),
      sess("POST", "", NOW + 6),
      sess("POST", "{} x", NOW + 7),
      sess("POST", '{"runs":[1e400, -0, true, null, [], {}]}', NOW + 8),
      sess("POST", String.fromCharCode(0xfeff) + JSON.stringify({ runs: [courseOf(0)] }), NOW + 9),
      sess("POST", '{"runs":[1],"runs":[2,3]}', NOW + 10),
      sess("POST", '{"__proto__":{"runs":[1]},"x":1}', NOW + 11),
      sess("POST", new Uint8Array([0x7b, 0x22, 0x72, 0x75, 0x6e, 0x73, 0x22, 0x3a, 0x5b, 0x22, 0xff, 0xfe, 0x22, 0x5d, 0x7d]), NOW + 12),
      sess("POST", `{"runs":[${deep(3000, "1")}]}`, NOW + 13),
      sess("POST", `{"runs":[${deepId(3000)}]}`, NOW + 14),
      sess("POST", `{"runs":[${JSON.stringify(lap(1000, 3000)).replace('"day":', '"day":1e400,"x":')}]}`, NOW + 15),
      sess("POST", '  {"runs": [ ]}  ', NOW + 16),
      sess("DELETE", "not json", NOW + 17),
      // JSON 의 -0 (구간 시간 0 과 같다, 저장하면 0)
      sess("POST", '{"runs":[' + JSON.stringify(courseOf(0)).replace('"split_ms":0', '"split_ms":-0') + "]}", NOW + 18),
    ],
  }),
  base("body_too_large", {
    covers: [],
    requests: [sess("POST", "x".repeat(4 * 1024 * 1024 + 1)), sess("POST", " ".repeat(4 * 1024 * 1024 - 20) + '{"runs":[1]}', NOW + 1000)],
  }),
  base("query_variants", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    requests: [
      sess("GET", undefined, NOW, `?app=%20${APP.toUpperCase().replace("0X", "0x")}%20`),
      sess("GET", undefined, NOW, `?app=+0X${APP.slice(2)}+`),
      sess("GET", undefined, NOW, "?app="),
      sess("GET", undefined, NOW, `?app=bad&app=${APP}`),
      sess("GET", undefined, NOW, `?x=1&%61pp=${APP}`),
      sess("DELETE", undefined, NOW, "?app=bad"),
      sess("POST", { runs: [courseOf(0)] }, NOW, "?app=bad"),
      sess("POST", { runs: [courseOf(0)] }, NOW, `?app=${APP2}`),
      getSession(APP, NOW + 1000),
    ],
  }),
  base("cors_preflight", {
    covers: ["navtrials.test.ts: 인증 · 요청 형식 — OPTIONS 는 204 와 GET, POST, DELETE · x-pd-signature 를 허락한다"],
    requests: [
      { method: "OPTIONS", path: "/nav/trials", headers: { Origin: ORIGIN } },
      { method: "OPTIONS", path: "/nav/trials", headers: { Origin: "http://tauri.localhost" } },
      { method: "OPTIONS", path: "/nav/trials", headers: { Origin: "https://evil.example" } },
      { method: "OPTIONS", path: "/nav/trials?app=bad", headers: {} },
      { method: "GET", path: `/nav/trials?app=${APP}`, headers: { Origin: "https://evil.example", "cf-connecting-ip": IP } },
      { method: "DELETE", path: "/nav/trials", headers: { Origin: "http://tauri.localhost" } },
    ],
  }),
  base("rate_limited", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    // GET 은 읽기 제한기, POST · DELETE 는 저장 제한기. 막히면 429 + Retry-After, 던지면 기록하고 통과, IP 가 없으면 세지 않는다
    limits: { EVAL_READ_LIMITER: (n) => (n === 1 ? false : n === 2 ? "throw" : true), EVAL_WRITE_LIMITER: (n) => (n <= 2 ? false : n === 3 ? "throw" : true) },
    requests: [
      getGuest(),
      getGuest(APP, NOW + 1),
      postSession([courseOf(0)], NOW + 2),
      sess("DELETE", undefined, NOW + 3),
      postSession([courseOf(0)], NOW + 4),
      { ...postSession([courseOf(1)], NOW + 5), headers: { "Content-Type": "application/json", Origin: ORIGIN, ...session } },
      { method: "OPTIONS", path: "/nav/trials", headers: app(), at: NOW + 6 },
      sess("PUT", undefined, NOW + 7),
      getGuest(APP, NOW + 8),
    ],
  }),
  base("rate_limit_bindings_absent", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    limits: {},
    requests: [getGuest(), postSession([courseOf(0)])],
  }),
  base("both_limiters_pass", {
    fixes: ["fix-nav-lost-update"],
    covers: [],
    limits: BOTH_LIMITERS,
    requests: [postSession([courseOf(0)]), getSession()],
  }),
  base("d1_faults", {
    covers: [],
    // D1 이 던지면 try 안이라 500 (console 에 같은 오류 글). batch 가 실패하면 아무것도 쓰지 않는다.
    // 등록 단계의 장애: 원본은 등록 SELECT(all), fix-nav-lost-update 뒤에는 등록 잡기 batch — 그래서 op 없이 처음 닿는 문장에 건다
    // (고친 판은 두 번째 요청의 batch 장애에서 잡은 등록을 되돌린다)
    fixes: ["fix-nav-lost-update"],
    faults: [
      { match: "nav_registrations", nth: 1 },
      { match: "INSERT INTO nav_trials", op: "batch", nth: 1 },
      { match: "ORDER BY \"runs\" DESC", op: "all", nth: 1 },
      { match: "SELECT * FROM nav_trials WHERE \"account\" = ?1 AND \"app\" = ?2 ORDER BY", op: "all", nth: 1 },
      { match: "DELETE FROM nav_trials WHERE \"account\" = ?1", op: "run", nth: 1 },
    ],
    requests: [postSession([courseOf(0)]), postSession([courseOf(0)], NOW + 1000), postSession([courseOf(0)], NOW + 2000), getGuest(APP, NOW + 3000), getSession(APP, NOW + 4000), getSession(APP, NOW + 5000), sess("DELETE", undefined, NOW + 6000), sess("DELETE", undefined, NOW + 7000)],
  }),
  base("d1_wallet_expire_fault", {
    fixes: ["fix-wallet-replay"],
    covers: [],
    faults: [{ match: "\"last_day\" < ?2", op: "run" }],
    requests: [await postWallet(W(920), [courseOf(0)]), await postWallet(W(920), ["bad"], NOW + 1000)],
  }),
  ...raceScenarios,
];
