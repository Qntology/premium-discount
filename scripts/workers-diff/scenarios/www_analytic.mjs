// 2단계: analytic.ts — /analytic/events · /analytic/sites. 옮긴 node 시험: analytic.test.ts (사이트 첫 화면은 시나리오의 fetch 로 답한다),
// 수집 스크립트(web/www/static/js/tracker.js)가 실제로 보내는 요청(text/plain 의 { events: [...] }, 사이트의 Origin, 떠날 때 4건씩),
// 그리고 가장자리(본문 형식 · gzip · 크기 · 요청 제한 · CORS · 메서드 · 주인 인증 · 목록 커서 · D1 장애)와 고친 버그(fixes 칸).
// 시나리오 모양은 www_common.mjs 머리말. 저장된 data(gzip) · 응답의 base64 gzip 은 www.mjs 가 풀어서 견준다.
import { ethers } from "ethers";
import pako from "pako";
import zlib from "node:zlib";
import { T0, SECRETS, HASH, TOKEN, app, creds, s3json, walletHeaders, WALLET_PHRASES } from "./www_common.mjs";

const M = "analytic";
const SHOP = "https://shop.example.com";
const VISITOR_IP = "203.0.113.9";
const enc = new TextEncoder();
const WALLET = ethers.Wallet.fromPhrase(WALLET_PHRASES[0]).address.toLowerCase();
const STRANGER = WALLET_PHRASES[1];
const hashId = (text) => ethers.computeAddress(ethers.hashMessage(text)).toLowerCase();
const meta = (content) => `<html><head><meta name="premium-discount-site" content="${content}"></head></html>`;
const html = (body, status = 200) => ({ status, body, headers: { "Content-Type": "text/html" } });

// analytic.test.ts 의 click(over)
function click(over = {}) {
  return {
    type: "click",
    href: SHOP + "/checkout?coupon=A1&email=a@b.co#top",
    sid: "s-1",
    seq: 3,
    t_ms: 9500,
    target: { chain: [{ tag: "button", id: "easy-pay", classes: ["btn"] }], pug: "button | 간편결제" },
    related: [{ chain: [{ tag: "button", id: "coupon" }], pug: "button | 쿠폰 적용 010-1234-5678" }],
    ...over,
  };
}

/** 수집 요청 (analytic.test.ts 의 collect): text/plain, 사이트의 Origin, 방문자 IP. headers 의 null 값은 그 머리글을 뺀다 */
function collect(body, headers = {}, extra = {}) {
  const h = { "Content-Type": "text/plain;charset=UTF-8", Origin: SHOP, "cf-connecting-ip": VISITOR_IP, ...headers };
  for (const k of Object.keys(h)) if (h[k] === null) delete h[k];
  const raw = typeof body === "string" || body instanceof Uint8Array ? body : JSON.stringify(body);
  return { method: "POST", path: "/analytic/events", headers: h, body: raw, ...extra };
}

/** 지갑으로 서명한 주인 요청 (analytic.test.ts 의 owner). 서명 글의 path 는 쿼리 없는 경로다.
 *  서명 시각은 부를 때마다 1 ms 씩 늘린다: 실제 앱처럼 요청마다 새로 서명한다 (같은 서명을 다시 쓰면 fix-wallet-replay 가 401 로 막는다) */
let signedAt = T0;
async function owner(method, path, { body, query = "", phrase = WALLET_PHRASES[0], headers = {} } = {}) {
  const text = body === undefined ? "" : typeof body === "string" ? body : JSON.stringify(body);
  const sig = await walletHeaders(phrase, { time: signedAt++, method, path, body: text });
  return { method, path: path + query, headers: { ...app({ "Content-Type": "application/json" }), ...sig, ...headers }, ...(text ? { body: text } : {}) };
}

/** 같은 순서로 부르는 바깥 fetch 의 답 (ts · rs 두 판이 차례로 같은 함수를 쓰므로 한 판의 호출 수로 나눈 나머지) */
function sequence(...replies) {
  let n = 0;
  return () => replies[n++ % replies.length];
}

const last = (prev) => prev[prev.length - 1];

// ---- gzip 모양들 (pako 의 ungzip 이 받는 것 · 거절하는 것) ----
const gz = (text, opts) => pako.gzip(enc.encode(text), opts);
const cat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};
const le32 = (n) => new Uint8Array([n & 255, (n >>> 8) & 255, (n >>> 16) & 255, (n >>> 24) & 255]);
/** 머리에 FEXTRA · FNAME · FCOMMENT · FHCRC 를 둔 gzip (pako 는 머리 CRC 를 확인한다) */
function gzipWithHeader(text, { flags = 0x1e, badHcrc = false } = {}) {
  const data = enc.encode(text);
  const head = [0x1f, 0x8b, 8, flags, 0, 0, 0, 0, 0, 3];
  if (flags & 4) head.push(4, 0, ...enc.encode("abcd"));
  if (flags & 8) head.push(...enc.encode("events.json"), 0);
  if (flags & 16) head.push(...enc.encode("c"), 0);
  if (flags & 2) {
    const c = (zlib.crc32(Uint8Array.from(head)) & 0xffff) ^ (badHcrc ? 1 : 0);
    head.push(c & 255, c >>> 8);
  }
  return cat(Uint8Array.from(head), zlib.deflateRawSync(data), le32(zlib.crc32(data)), le32(data.length));
}
const CLICK1 = JSON.stringify([click()]);
const gzipped = gz(CLICK1);
const flip = (b, fromEnd) => {
  const c = b.slice();
  c[c.length - fromEnd] ^= 1;
  return c;
};
// { events: [...] } 를 두 멤버로 나눈 gzip (이어 풀면 하나의 JSON)
const half = CLICK1.length >> 1;
const twoMembers = cat(gz(CLICK1.slice(0, half)), gz(CLICK1.slice(half)));
const zlibFdict = pako.deflate(enc.encode(CLICK1), { dictionary: "abc" });

// 상한(1 MiB) 언저리의 gzip: 공백으로 늘린 [click()]
const CAP = 1024 * 1024;
// 길이는 UTF-8 바이트로 (click() 에 한글이 있다)
const CLICK1_BYTES = enc.encode(CLICK1).length;
const padded = (n) => CLICK1.slice(0, -1) + " ".repeat(n - CLICK1_BYTES) + "]";
const atCap = gz(padded(CAP));
const overCap = gz(padded(CAP + 1));

// tracker.js 가 만드는 요소 (domChain: tag · nth_of_type · id · classes · attrs 차례, toPug: 4칸 들여쓰기, scrubText 를 거친 글)
const linkChain = [
  { tag: "a", nth_of_type: 1, attrs: [["data-testid", "buy-2"]] },
  { tag: "li", nth_of_type: 2, classes: ["card", "active"] },
  { tag: "ul", nth_of_type: 1, id: "products" },
  { tag: "main", nth_of_type: 1 },
];
const card = (i) => ({ chain: [{ tag: "li", nth_of_type: i, classes: ["card"] }, ...linkChain.slice(2)], pug: `li\n    a | 상품 ${i}\n` });
const APP = "https://app.example.com";
const SID = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
const tracked = (type, seq, extra = {}) => ({ type, href: APP + "/#/orders/12", sid: SID, seq, t_ms: seq * 750, ...extra });
const trackerBatch = [
  tracked("enter", 1),
  tracked("click", 2, { target: { chain: linkChain, pug: "a | B\n" }, item: { chain: linkChain.slice(1), pug: "li\n    a | B\n" }, related: [card(1), card(3)] }),
  tracked("click", 3, { target: { chain: [{ tag: "button", nth_of_type: 1, id: "pay", attrs: [["type", "submit"]] }], pug: "button | 결제하기\n" }, rage: true }),
  tracked("hover", 4, { target: { chain: [{ tag: "div", nth_of_type: 1, classes: ["addr"] }], pug: "div | 홍길동 # #\n" }, dwell_ms: 1820 }),
  tracked("scroll", 5, { flips: 3 }),
  tracked("change", 6, { target: { chain: [{ tag: "input", nth_of_type: 2, attrs: [["name", "when"], ["type", "datetime-local"]] }], pug: "input\n" }, field: "datetime-local" }),
  tracked("change", 7, { target: { chain: [{ tag: "select", nth_of_type: 1, attrs: [["name", "size"]] }], pug: "select\n    option | M\n" }, field: "select" }),
  tracked("submit", 8, { target: { chain: [{ tag: "form", nth_of_type: 1, id: "checkout" }], pug: "form\n    button | 결제하기\n" } }),
  tracked("back", 9),
];
const leavingBatches = [
  [tracked("enter", 10), tracked("click", 11, { target: { chain: [{ tag: "a", nth_of_type: 3 }], pug: "a | 장바구니 3\n" } }), tracked("change", 12, { field: "text" }), tracked("hover", 13, { dwell_ms: 500 })],
  [tracked("leave", 14, { dirty: 2 })],
];

// sid 의 64 번째 글자 · pug 의 4000 번째 글자 · id 의 80 번째 글자에서 갈리는 서로게이트 쌍
const EMOJI = "😀";
const lonePieces = click({
  sid: "s".repeat(62) + EMOJI, // 64 글자에 딱 맞음
  target: {
    chain: [{ tag: "button", id: "i".repeat(79) + EMOJI, classes: ["c".repeat(80), "d".repeat(81), ...Array.from({ length: 9 }, (_, i) => "k" + i)], attrs: [["data-x", "v".repeat(79) + EMOJI], ["k".repeat(39) + EMOJI, "v"]] }],
    pug: "p | " + "a".repeat(3995) + EMOJI,
  },
});

// 앞 응답의 next 로 다음 쪽 (서명 글의 path 는 쿼리 없는 경로라 미리 서명해 둔다. 쪽마다 새 서명)
const nextPage = async () => ({
  ...(await owner("GET", "/analytic/events")),
  path: (prev) => `/analytic/events?host=shop.example.com&limit=2&since=${last(prev).body.next.since}&after_id=${last(prev).body.next.after_id}`,
});

const sessionEnv = { ...SECRETS };
const sessionHeaders = { ...creds(HASH, TOKEN), "Content-Type": "application/json", "cf-connecting-ip": VISITOR_IP };
const OWNER_EMAIL = "owner@shop.example.com";

export default [
  {
    name: "www_analytic_collect_store", module: M,
    covers: [
      "analytic.test.ts: 수집: 사이트마다 쌓고, sid 원문 · 쿼리 · 개인정보 낱말은 남기지 않는다 (stored 2, Allow-Origin *, 제한기 키 1, cc · session · updated_at · created_at · data)",
      "analytic.test.ts: 사건 모양을 검사한다 (모르는 칸 · rage 'yes' 는 버림, related 150 → 100)",
    ],
    limits: { ANALYTIC_WRITE_LIMITER: true },
    requests: [
      collect({ events: [click(), click({ type: "hover", seq: 4, dwell_ms: 820, related: [] })] }),
      collect([click({ seq: 5, related: Array.from({ length: 150 }, (_, i) => ({ pug: `li | 상품 ${i}` })), value: "비밀", rage: "yes" })]),
      collect([click({ seq: 6, target: { chain: [{ tag: "b<script>" }], pug: "" } })]),
    ],
  },
  {
    name: "www_analytic_collect_formats", module: M,
    covers: ["analytic.test.ts: 수집: gzip 본문 · 배열 본문도 받는다, 다른 사이트의 사건 · 모양이 틀린 사건 · 너무 많은 사건은 받지 않는다, Origin 이 없으면 주소만 본다"],
    requests: [
      collect(gzipped, { "Content-Encoding": "gzip" }),
      collect([click({ seq: 5 })]),
      collect([click({ href: "https://other.example.com/cart" })]),
      collect({ events: [click(), { type: "click" }] }),
      collect({ events: Array.from({ length: 21 }, () => click()) }),
      collect("not json"),
      collect([click({ href: "https://other.example.com/" })], { Origin: "" }),
      collect([click({ href: "https://third.example.com/" })], { Origin: null }),
    ],
  },
  {
    name: "www_analytic_limit_and_preflight", module: M,
    covers: ["analytic.test.ts: 수집: 요청 제한(429, Retry-After 60)과 사전 요청(204, Allow-Origin *)"],
    limits: { ANALYTIC_WRITE_LIMITER: false },
    requests: [
      collect([click()]),
      { method: "OPTIONS", path: "/analytic/events", headers: { Origin: SHOP, "Access-Control-Request-Method": "POST" } },
    ],
  },
  {
    name: "www_analytic_limit_throws_or_no_ip", module: M,
    covers: ["ratelimit: 수집 제한기가 던지면 막지 않는다 · IP 가 없으면 제한기를 부르지 않는다 (SCENARIOS RL-05)"],
    limits: { ANALYTIC_WRITE_LIMITER: "throw" },
    requests: [collect([click()]), collect([click({ seq: 9 })], { "cf-connecting-ip": null })],
  },
  {
    name: "www_analytic_limit_fallback", module: M,
    covers: ["analytic.ts: ANALYTIC_WRITE_LIMITER 가 없으면 EVAL_WRITE_LIMITER 로 센다"],
    limits: { EVAL_WRITE_LIMITER: (n) => n !== 2, EVAL_READ_LIMITER: true },
    requests: [collect([click()]), collect([click({ seq: 4 })]), collect([click({ seq: 5 })])],
  },
  {
    name: "www_analytic_no_limiters", module: M,
    covers: ["ratelimit: 바인딩이 없으면 제한하지 않는다"],
    requests: [collect([click()])],
  },
  {
    name: "www_analytic_owner_limits", module: M, fixes: ["fix-wallet-replay"],
    covers: ["analytic.ts: 주인 쪽은 GET 이 읽기 제한기, POST · DELETE 가 쓰기 제한기 (429 는 주인 CORS)"],
    limits: { EVAL_READ_LIMITER: false, EVAL_WRITE_LIMITER: (n) => n === 1 },
    requests: [
      await owner("GET", "/analytic/sites"),
      await owner("POST", "/analytic/sites", { body: { host: "shop.example.com" } }),
      await owner("DELETE", "/analytic/sites", { query: "?host=shop.example.com" }),
    ],
    fetch: () => html(meta(WALLET)),
  },
  {
    name: "www_analytic_sites_wallet", module: M, fixes: ["fix-wallet-replay"],
    covers: [
      "analytic.test.ts: 사이트 주인 확인: 첫 화면의 meta 가 지갑 주소를 말해야 한다 (틀린 meta 403 + expected, 가져온 주소, 503 → 403, 'not a host' 400, 경로 붙은 주소 200 · cc, 행, GET 목록, 서명 없으면 401)",
    ],
    requests: [
      await owner("POST", "/analytic/sites", { body: { host: "Shop.Example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "shop.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "not a host" } }),
      await owner("POST", "/analytic/sites", { body: { host: "https://shop.example.com/any/path" } }),
      await owner("GET", "/analytic/sites"),
      { method: "POST", path: "/analytic/sites", body: '{"host":"shop.example.com"}' },
      // 다시 확인하면 verified_at 만 바뀐다 (ON CONFLICT)
      await owner("POST", "/analytic/sites", { body: { host: "shop.example.com" } }),
    ],
    fetch: sequence(html(`<meta name="premium-discount-site" content="0x${"b".repeat(40)}">`), html("down", 503), html(meta(WALLET)), html(meta(WALLET))),
  },
  {
    name: "www_analytic_events_read", module: M, fixes: ["fix-wallet-replay"],
    covers: ["analytic.test.ts: 사건 읽기: 확인된 주인만(확인 전 403 · 다른 지갑 403), (시각, id) 커서로 빠짐없이, base64 gzip 으로, 주인은 오래된 사건을 지운다(5건)"],
    requests: [
      collect({ events: [1, 2, 3, 4, 5].map((seq) => click({ seq })) }),
      collect([click({ seq: 6 })]),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com" }),
      await owner("POST", "/analytic/sites", { body: { host: "shop.example.com" } }),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com", phrase: STRANGER }),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com&limit=2" }),
      await nextPage(),
      await nextPage(),
      await nextPage(),
      await owner("DELETE", "/analytic/events", { query: `?host=shop.example.com&before=${T0 + 1}` }),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com" }),
    ],
    fetch: () => html(meta(WALLET)),
  },
  {
    name: "www_analytic_session_owner", module: M,
    covers: ["analytic.test.ts: 사이트 주인 확인: QR 로그인 세션이면 이메일의 hashId 가 주인 주소다 (등록 200 · owner, 수집, 읽기 rows 1, S3 가 다른 토큰을 말하면 401, analytic_sites 1행)"],
    bindings: sessionEnv,
    s3: ({ n }) => s3json({ token: n <= 2 ? TOKEN : "ef".repeat(32), email: OWNER_EMAIL }),
    requests: [
      { method: "POST", path: "/analytic/sites", headers: sessionHeaders, body: JSON.stringify({ host: "shop.example.com" }) },
      collect([click()]),
      { method: "GET", path: "/analytic/events?host=shop.example.com", headers: sessionHeaders },
      { method: "GET", path: "/analytic/events?host=shop.example.com", headers: sessionHeaders },
    ],
    fetch: () => html(meta(hashId(OWNER_EMAIL))),
  },
  {
    name: "www_analytic_tracker_requests", module: M,
    covers: [
      "tracker.js send(): POST text/plain;charset=UTF-8 의 { events: [...] }, 사이트 Origin, 쿠키 없음 — enter · click(target · item · related · rage) · hover(dwell_ms) · scroll(flips) · change(field) · submit · back",
      "tracker.js flush(true): 떠날 때 4건씩(keepalive) · leave(dirty), 해시 라우팅 주소(#/orders/12)",
      "tracker.test.ts: domChain 의 모양(tag · nth_of_type · id · classes · attrs)이 서버에서 tag · id · classes · attrs · nth_of_type 차례로 저장된다",
    ],
    limits: { ANALYTIC_WRITE_LIMITER: true },
    requests: [
      collect({ events: trackerBatch }, { Origin: APP }),
      ...leavingBatches.map((b) => collect({ events: b }, { Origin: APP })),
      // sendBeacon 도 같은 글 본문이다 (Content-Type 은 브라우저가 붙임)
      collect(JSON.stringify({ events: [tracked("leave", 15)] }), { Origin: APP }),
    ],
  },
  {
    name: "www_analytic_collect_edges", module: M,
    covers: ["analytic.ts sanitizeEvent · collect 의 가장자리: 빈 · 객체 · null 본문, 수 다듬기(−0 · 1e400 · 소수 · 상한), 주소(대소문자 · 포트 · 배열 · 수), BOM, 같은 열쇠, 깊은 중첩, 20건, 서로게이트가 갈리는 sid · id · attrs · pug, classes · chain 개수"],
    requests: [
      collect("[]"),
      collect("{}"),
      collect('{"events":[]}'),
      collect("null"),
      collect('"x"'),
      collect({ events: Array.from({ length: 20 }, (_, i) => click({ seq: i })) }),
      collect('[{"type":"click","href":"https://shop.example.com/a","sid":"z","seq":-0,"t_ms":1e400,"dwell_ms":-1,"flips":2.9,"dirty":5000,"field":"Text","rage":true,"type":"hover"}]'),
      collect([click({ seq: 1.7, t_ms: 9e9, field: "datetime-local", target: { chain: Array.from({ length: 7 }, (_, i) => ({ tag: "DIV", nth_of_type: i === 0 ? 1e300 : i + 0.5 })), pug: 7 } })]),
      collect([click({ href: " HTTPS://SHOP.example.com:443/x?y#z " })], { Origin: "https://shop.example.com:8443" }),
      collect([click({ href: ["https://shop.example.com/a"] }), click({ href: 42 })]),
      collect("﻿" + JSON.stringify([click({ seq: 8 })])),
      collect('[{"type":"click","href":"https://shop.example.com/","sid":"a","__proto__":{"x":1},"sid":"b","seq":1}]'),
      collect("[".repeat(100000) + "]".repeat(100000)),
      collect([lonePieces]),
      collect([click({ href: "https://shop.example.com/" })], { Origin: "null" }),
      collect([click({ type: "leave", target: null, item: [], related: "x" })]),
    ],
  },
  {
    name: "www_analytic_body_size", module: M,
    covers: ["analytic.ts collect: 본문 256 KB 상한(413, Content-Length 와 실제 길이) · 상한 안쪽은 받는다"],
    requests: [
      collect(CLICK1 + " ".repeat(256 * 1024 - CLICK1_BYTES)),
      collect(CLICK1 + " ".repeat(256 * 1024 - CLICK1_BYTES + 1)),
      collect(new Uint8Array(256 * 1024 + 10), { "Content-Encoding": "gzip" }),
    ],
  },
  {
    name: "www_analytic_gzip_formats", module: M,
    covers: ["analytic.ts readBody: pako ungzip 이 받는 것(gzip · x-gzip · 대문자 · zlib · 멤버 이어 붙이기 · 0 뒤의 찌꺼기 · 머리 FEXTRA/FNAME/FCOMMENT/FHCRC)과 거절하는 것(raw deflate · 찌꺼기 · CRC · 길이 · 잘림 · 빈 본문 · 평문 · 머리 CRC · 예약 플래그 · FDICT)"],
    requests: [
      collect(gzipped, { "Content-Encoding": "x-gzip" }),
      collect(gzipped, { "Content-Encoding": "GZIP, br" }),
      collect(pako.deflate(enc.encode(CLICK1)), { "Content-Encoding": "gzip" }),
      collect(twoMembers, { "Content-Encoding": "gzip" }),
      collect(cat(gzipped, new Uint8Array([0, 7, 7])), { "Content-Encoding": "gzip" }),
      collect(cat(pako.deflate(enc.encode(CLICK1)), gzipped), { "Content-Encoding": "gzip" }),
      collect(gzipWithHeader(CLICK1), { "Content-Encoding": "gzip" }),
      collect(pako.deflateRaw(enc.encode(CLICK1)), { "Content-Encoding": "gzip" }),
      collect(cat(gzipped, new Uint8Array([7, 7, 7])), { "Content-Encoding": "gzip" }),
      collect(flip(gzipped, 5), { "Content-Encoding": "gzip" }),
      collect(flip(gzipped, 1), { "Content-Encoding": "gzip" }),
      collect(gzipped.slice(0, gzipped.length - 3), { "Content-Encoding": "gzip" }),
      collect(new Uint8Array(0), { "Content-Encoding": "gzip" }),
      collect(CLICK1, { "Content-Encoding": "gzip" }),
      collect(gzipped),
      collect(gzipWithHeader(CLICK1, { badHcrc: true }), { "Content-Encoding": "gzip" }),
      collect(gzipWithHeader(CLICK1, { flags: 0x20 }), { "Content-Encoding": "gzip" }),
      collect(zlibFdict, { "Content-Encoding": "gzip" }),
      collect(cat(gzipped, gz("")), { "Content-Encoding": "gzip" }),
    ],
  },
  {
    name: "www_analytic_gzip_at_cap", module: M,
    covers: ["fix-analytic-gzip-cap 의 경계: 푼 크기가 딱 1 MiB 면 받는다 (원본과 같음)"],
    requests: [collect(atCap, { "Content-Encoding": "gzip" })],
  },
  {
    name: "www_analytic_preflight", module: M,
    covers: ["analytic.ts: 사전 요청 — 수집(/analytic/events 의 POST)은 Allow-Origin * · Content-Encoding, 주인(GET · DELETE · 메서드 없음)은 Origin 을 돌려주고 지갑 · 세션 머리글"],
    requests: [
      { method: "OPTIONS", path: "/analytic/events", headers: { Origin: "tauri://localhost", "Access-Control-Request-Method": "GET" } },
      { method: "OPTIONS", path: "/analytic/events", headers: { Origin: "tauri://localhost" } },
      { method: "OPTIONS", path: "/analytic/events" },
      { method: "OPTIONS", path: "/analytic/sites", headers: { Origin: "tauri://localhost", "Access-Control-Request-Method": "DELETE" } },
    ],
  },
  {
    name: "www_analytic_methods_and_paths", module: M,
    covers: ["analytic.ts: GET · POST · DELETE 밖은 405(주인 CORS, 모르는 메서드 · 소문자 포함), 인증 없는 읽기 401, 비슷한 경로는 레거시로"],
    requests: [
      { method: "PUT", path: "/analytic/events", body: "{}" },
      { method: "PATCH", path: "/analytic/sites", body: "{}" },
      { method: "FOO", path: "/analytic/sites" },
      { method: "PROPFIND", path: "/analytic/sites", headers: app() },
      { method: "PURGE", path: "/analytic/events", headers: app() },
      { method: "patch", path: "/analytic/events" },
      { method: "GET", path: "/analytic/events?host=shop.example.com", headers: app() },
      { method: "DELETE", path: "/analytic/sites?host=shop.example.com", headers: app() },
      { method: "POST", path: "/analytic/events/", headers: app(), body: "[]" },
      { method: "GET", path: "/Analytic/sites", headers: app() },
    ],
  },
  {
    name: "www_analytic_owner_edges", module: M, fixes: ["fix-wallet-replay"],
    covers: ["analytic.ts 주인 쪽 가장자리: 지갑 머리글이 하나라도 있으면 세션을 보지 않음, 4 KB 본문 413, 등록 본문(null · 수 · 깨진 JSON · 'localhost'), 가져오기 실패, meta 의 여러 모양, 512 KB 넘어 있는 meta, 지우기 · 목록 인자"],
    bindings: sessionEnv,
    s3: () => s3json({ token: TOKEN, email: OWNER_EMAIL }),
    requests: [
      { method: "GET", path: "/analytic/sites", headers: { ...sessionHeaders, "x-pd-address": WALLET } },
      await owner("POST", "/analytic/sites", { body: JSON.stringify({ host: "shop.example.com", pad: "x".repeat(5000) }) }),
      await owner("POST", "/analytic/sites", { body: "null" }),
      await owner("POST", "/analytic/sites", { body: "5" }),
      await owner("POST", "/analytic/sites", { body: "{bad" }),
      await owner("POST", "/analytic/sites", { body: { host: "localhost" } }),
      await owner("POST", "/analytic/sites", { body: { host: "fail.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "upper.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "quoted.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "data.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "far.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "near.example.com" } }),
      await owner("POST", "/analytic/sites", { body: { host: "bom.example.com" } }),
      await owner("GET", "/analytic/sites"),
      await owner("DELETE", "/analytic/sites", { query: "?host=upper.example.com" }),
      await owner("DELETE", "/analytic/sites"),
      await owner("DELETE", "/analytic/events", { query: "?host=near.example.com" }),
      await owner("DELETE", "/analytic/events", { query: "?host=near.example.com&before=abc" }),
      await owner("DELETE", "/analytic/events", { query: "?host=near.example.com&before=-1" }),
      await owner("GET", "/analytic/events"),
      await owner("GET", "/analytic/events", { query: "?host=near.example.com&limit=abc&since=-5&after_id=zz" }),
      await owner("GET", "/analytic/events", { query: "?host=near.example.com&limit=0&since=abc" }),
      await owner("GET", "/analytic/events", { query: "?host=near.example.com&limit=-5" }),
      await owner("GET", "/analytic/events", { query: "?host=NEAR.example.com&limit=1e9&since=1e3" }),
    ],
    fetch: ({ url }) => {
      const host = new URL(url).hostname;
      if (host === "fail.example.com") return { throw: "connection refused" };
      if (host === "upper.example.com") return html(`<meta name="PREMIUM-DISCOUNT-SITE" content="  ${WALLET.toUpperCase().replace("0X", "0x")} ,junk">`);
      if (host === "quoted.example.com") return html(`<meta content='0x${"1".repeat(40)},${WALLET}' name='premium-discount-site'>`);
      if (host === "data.example.com") return html(`<meta data-name=premium-discount-site content=${WALLET}>`);
      if (host === "far.example.com") return html(" ".repeat(512 * 1024) + meta(WALLET));
      if (host === "near.example.com") return html(meta(WALLET) + " ".repeat(600 * 1024));
      if (host === "bom.example.com") return html("﻿" + meta(WALLET));
      return html("<html></html>");
    },
  },
  {
    name: "www_analytic_d1_faults", module: M, fixes: ["fix-wallet-replay"],
    covers: ["analytic.ts: try 안의 D1 오류는 console 에 'analytic error' 와 함께 찍고 500 (수집은 수집 CORS, 주인은 주인 CORS)"],
    faults: [{ match: "INSERT INTO web_events", op: "batch" }, { match: "INSERT INTO analytic_sites" }, { match: "SELECT 1 AS ok", nth: 1 }],
    requests: [
      collect([click()]),
      await owner("POST", "/analytic/sites", { body: { host: "shop.example.com" } }),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com" }),
    ],
    fetch: () => html(meta(WALLET)),
  },
  {
    name: "www_analytic_since_infinity", module: M, fixes: ["fix-wallet-replay"],
    covers: ["analytic.ts events: since=Infinity · 아주 큰 수를 그대로 묶는다"],
    sql: [[`INSERT INTO analytic_sites ("cc", "host", "owner", "verified_at") VALUES (?1, ?2, ?3, ?4)`, hashId("shop.example.com"), "shop.example.com", WALLET, 1]],
    requests: [
      collect([click()]),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com&since=Infinity" }),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com&since=1e400&limit=Infinity" }),
      await owner("DELETE", "/analytic/events", { query: "?host=shop.example.com&before=Infinity" }),
    ],
  },
  // ---- 고친 버그 ----
  {
    name: "www_analytic_fix_gzip_cap", module: M, fixes: ["fix-analytic-gzip-cap"],
    covers: ["fix-analytic-gzip-cap: 1 MiB + 1 바이트로 풀리는 gzip 은 413 (원본은 끝까지 풀어 저장)"],
    requests: [collect(overCap, { "Content-Encoding": "gzip" })],
  },
  {
    name: "www_analytic_fix_gzip_bomb", module: M, fixes: ["fix-analytic-gzip-cap"],
    covers: ["fix-analytic-gzip-cap: 256 KB 안의 gzip 폭탄(0 바이트 약 250 MB)은 413 (원본은 다 풀고 JSON 이 아니라 400)"],
    requests: [collect(new Uint8Array(zlib.gzipSync(Buffer.alloc(250 * 1024 * 1024), { level: 9 })), { "Content-Encoding": "gzip" })],
  },
  {
    name: "www_analytic_fix_sites_preflight", module: M, fixes: ["fix-analytic-sites-preflight"],
    covers: ["fix-analytic-sites-preflight: POST /analytic/sites 의 사전 요청은 주인 CORS (원본은 수집 CORS)"],
    requests: [{ method: "OPTIONS", path: "/analytic/sites", headers: { Origin: "tauri://localhost", "Access-Control-Request-Method": "POST" } }],
  },
  {
    name: "www_analytic_fix_empty_host", module: M, fixes: ["fix-analytic-empty-host"],
    covers: ["fix-analytic-empty-host: 호스트 없는 주소는 400 (원본은 Origin 이 없으면 500, 있으면 403)"],
    requests: [
      collect([click({ href: "https://a@/x" })], { Origin: null }),
      collect([click({ href: "https://:80/" })]),
    ],
  },
  {
    name: "www_analytic_fix_sid_surrogate", module: M, fixes: ["fix-analytic-sid-surrogate"],
    covers: ["fix-analytic-sid-surrogate: 64 글자 자리에서 서로게이트 쌍이 갈리는 sid 는 400 (원본은 hashId 가 던져 500)"],
    requests: [collect([click({ sid: "s".repeat(63) + EMOJI })])],
  },
  {
    name: "www_analytic_fix_list_limit", module: M, fixes: ["fix-analytic-list-limit", "fix-wallet-replay"],
    covers: ["fix-analytic-list-limit: limit=2.5 는 2 로 (원본은 LIMIT 2.5 를 SQLite 가 거절해 500)"],
    sql: [[`INSERT INTO analytic_sites ("cc", "host", "owner", "verified_at") VALUES (?1, ?2, ?3, ?4)`, hashId("shop.example.com"), "shop.example.com", WALLET, 1]],
    requests: [
      collect({ events: [1, 2, 3].map((seq) => click({ seq })) }),
      await owner("GET", "/analytic/events", { query: "?host=shop.example.com&limit=2.5" }),
    ],
  },
];
