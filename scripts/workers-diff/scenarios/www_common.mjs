// www 차분 시나리오의 공용 값 · 도구. 모듈마다 scenarios/www_<모듈>.mjs 가 `export default [ …시나리오 ]` 를 내고,
// www_scenarios.mjs 가 모은다. 2단계 모듈(evaluations · navtrials · graph · analytic)은 자기 파일만 채우면 된다.
//
// 시나리오 하나:
//   {
//     name, module,                       이름(고유) · 모듈(MODULE= 거르기, 파일 이름의 www_<모듈>)
//     covers: ["session.test.ts: …"],     옮긴 node 시험의 단언 (없으면 가장자리)
//     fixes: ["fix-…"],                   닿는 버그 수정 id (docs/workers-rs.md) — ORIGINAL=raw 에서 이 시나리오만 달라야 한다
//     random: true,                       씨앗 있는 crypto 로도 맞출 수 없는 무작위가 남을 때만: 한쪽에만 있는 16진 값을 정규화 (www.mjs 머리말)
//     bindings: { 이름: 값 },              변수 · 비밀 (DEFAULT_BINDINGS 위에 덮음, undefined 면 빠짐)
//     limits: { EVAL_WRITE_LIMITER: true | false | "throw" | {…} | (n) => … },  있는 요청 제한 바인딩과 그 답 (키가 없으면 바인딩도 없음)
//     sql: [[sql, ...args], …],           요청 전에 DB 에 넣을 씨앗 행
//     vectors: { id: { values, metadata } }, vec: { query: (body, store) => result, … }   가짜 Vectorize 의 처음 상태 · 답
//     faults: [{ match, op?, nth?, message? }]  D1 문장 장애
//     race: { match, op?, after: [{ sql, args }] }  match 가 든 문장을 처음 실행한 뒤 끼워 넣을 문장 (실행마다 한 번, 경합 흉내)
//     s3: ({ method, path, n }) => { status, body } | { throw } | Response   가짜 S3 의 답 (n: 이 시나리오에서 몇 번째 S3 호출)
//     fetch: ({ url, method, headers, body }) => { status, body, headers } | { throw }   그 밖의 바깥 요청의 답
//     requests: [{ method, path, headers, cookies, jar, body, cf, at }],  차례로 보낼 요청.
//        headers · body · path 는 함수(prev) 일 수 있다: prev = 이 쪽(ts · rs)에서 앞서 받은 응답들 ({ status, headers, body, … })
//        at = 가상 시계(ms, 기본 T0 + 차례 × 1000). cf = request.cf 에 덮을 값 (null 이면 기본값 없이)
//   }
import { ethers } from "ethers";

export const BASE_URL = "https://premium.discount";
// contracts/wallet_v1.json 의 요청 시각 (2025-11-17T12:00:00Z)
export const T0 = 1763380800000;
export const IP = "203.0.113.7";
export const ORIGIN = "tauri://localhost";
export const DEFAULT_CF = { country: "KR", city: "Seoul", continent: "AS", timezone: "Asia/Seoul", region: "Seoul", regionCode: "11", postalCode: "04524", latitude: "37.56", longitude: "126.97", asOrganization: "Test ISP" };
// wrangler.json 의 vars
export const DEFAULT_BINDINGS = { SESSION_MAIL_HOST: "premium.discount" };

// 시험의 비밀 · S3 설정 (session.test.ts · auth.test.ts 와 같은 값)
export const SECRET = "test-session-secret";
export const S3 = {
  SESSION_S3_BUCKET: "oauth-sessions",
  SESSION_S3_REGION: "ap-northeast-2",
  SESSION_S3_ACCESS_KEY_ID: "AKIATESTTESTTESTTEST",
  SESSION_S3_SECRET_ACCESS_KEY: "test-secret",
};
export const SECRETS = { SESSION_SECRET: SECRET, GUEST_SALT: "salt", AUTHOR_KEY_SECRET: "k", ...S3 };
export const HASH = "ab".repeat(20);
export const TOKEN = "cd".repeat(32);
export const BOTH_LIMITERS = { EVAL_WRITE_LIMITER: true, EVAL_READ_LIMITER: true };

/** 앱 · 브라우저 요청의 머리글 (Origin · cf-connecting-ip) */
export const app = (extra = {}) => ({ Origin: ORIGIN, "cf-connecting-ip": IP, ...extra });
/** x-oauth-hash · x-oauth-token */
export const creds = (hash, token) => ({ "x-oauth-hash": hash, "x-oauth-token": token });
/** 앞서 받은 응답 i 의 앱 세션 자격 증명 (POST /session/start 의 본문) */
export const issued = (i) => (prev) => app(creds(prev[i].body.hash, prev[i].body.token));
/** S3 객체 답 */
export const s3json = (obj, status = 200) => ({ status, body: JSON.stringify(obj) });

// ---- 2단계 모듈이 쓸 씨앗 · 서명 도구 (test-support/fakes.ts · cron_scenarios.mjs 와 같은 기본값) ----

export const PARENT_ID = "0190f0a0-0000-7000-8000-000000000001";
export const AUTHOR = "0x00000000000000000000000000000000000000ab";

/** test-support/fakes.ts 의 unitVector(seed): sin(seed·(i+1)) 를 정규화한 Float32 × 768 의 바이트 (hex) */
export function unitVectorHex(seed = 1, bytes = 3072) {
  const v = new Float32Array(768);
  for (let i = 0; i < v.length; i++) v[i] = Math.sin(seed * (i + 1));
  const norm = Math.sqrt(v.reduce((s, x) => s + x * x, 0));
  const u = new Uint8Array(v.map((x) => x / norm).buffer);
  const out = new Uint8Array(bytes);
  out.set(u.subarray(0, Math.min(bytes, u.length)));
  return Buffer.from(out).toString("hex");
}
/** unitVector(seed) 의 숫자 배열 (요청 본문의 vector) */
export function unitVector(seed = 1) {
  const v = new Float32Array(768);
  for (let i = 0; i < v.length; i++) v[i] = Math.sin(seed * (i + 1));
  const norm = Math.sqrt(v.reduce((s, x) => s + x * x, 0));
  return Array.from(v.map((x) => x / norm));
}

/** seedEvaluation (fakes.ts) — evaluations 행 */
export function evaluation(s) {
  const created = s.created_at ?? 1_000;
  return [
    `INSERT INTO evaluations ("id", "from", "to", "cc", "bcc", "ref", "root", "created_at", "updated_at", "request_id")
     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?7, ?8)`,
    s.id, s.from ?? AUTHOR, s.to ?? null, s.cc ?? "example.com", s.ref ?? null, s.root ?? s.ref ?? s.id, created, "req-" + s.id,
  ];
}

/** 계약 지갑(contracts/wallet_v1.json)의 시드 구문 */
export const WALLET_PHRASES = [
  "test test test test test test test test test test test junk",
  "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
];

/** 지갑 서명 머리글 (auth.ts walletRequestMessage 와 같은 글에 EIP-191 서명). body 는 보낼 글 그대로 */
export async function walletHeaders(phrase, { time = T0, method = "POST", path = "/nav/trials", body = "" } = {}) {
  const wallet = ethers.Wallet.fromPhrase(phrase);
  const address = wallet.address.toLowerCase();
  const sha = ethers.sha256(ethers.toUtf8Bytes(body)).slice(2);
  const message = ["premium.discount request v1", `address: ${address}`, `time: ${time}`, `method: ${method.toUpperCase()}`, `path: ${path}`, `body-sha256: ${sha}`].join("\n");
  return { "x-pd-address": address, "x-pd-time": String(time), "x-pd-signature": await wallet.signMessage(message) };
}
