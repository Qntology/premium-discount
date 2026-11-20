// auth.ts 의 세션 갈래 — auth.test.ts 의 단언을 GET /session 으로 옮겼다 (1단계에서 auth 의 세션 확인에 닿는 경로는 /session 뿐:
// verifiedAddress 를 부르는 evaluations · navtrials · analytic 은 2단계). verifiedAddress 는 readSessionCredentials + checkSession +
// addressFromEmail 이고, /session 은 같은 checkSession 의 결과를 state 로 내보낸다 (verified → signed_in, 그 밖은 로그인 아님).
// 지갑 갈래(verifyWalletRequest · walletRequestMessage — wallet.test.ts)는 2단계 경로에서만 쓰여 workers/www/tests/contracts.rs 가 맡는다.
import { SECRETS, S3, HASH, TOKEN, IP, app, creds, s3json } from "./www_common.mjs";

const M = "auth";
const status = (headers, extra = {}) => ({ path: "/session", headers, ...extra });
const session = creds(HASH, TOKEN);
// auth.test.ts 의 기본 S3 객체 (token · email · ip)
const okObject = () => s3json({ token: TOKEN, email: "user@example.com", ip: IP });

export default [
  {
    name: "auth_read_credentials", module: M,
    covers: ["auth.test.ts: 앱 헤더와 웹 쿠키에서 hash/token 을 읽고, 0x 와 대문자를 정규화한다"],
    bindings: SECRETS,
    s3: okObject,
    requests: [
      status(app(session)),
      status(app(creds("0x" + HASH.toUpperCase(), "0x" + TOKEN))),
      status(app(), { cookies: { a: "1", hash: HASH, token: TOKEN } }),
      status(app(creds(" 0X" + HASH + " ", TOKEN.toUpperCase()))),
    ],
  },
  {
    name: "auth_header_wins_and_bad_forms", module: M,
    covers: ["auth.test.ts: 헤더가 하나라도 있으면 쿠키는 보지 않고, 앱의 익명 자리값과 형식 오류는 세션이 아니다"],
    bindings: SECRETS,
    s3: okObject,
    requests: [
      status(app({ "x-oauth-hash": HASH }), { cookies: { hash: HASH, token: TOKEN } }),
      status(app({ "x-oauth-token": TOKEN, "x-oauth-hash": "" }), { cookies: { hash: HASH, token: TOKEN } }),
      status(app(creds("0x" + "0".repeat(40), "0x" + "0".repeat(64)))),
      status(app(creds("zz", TOKEN))),
      status(app(creds(HASH, TOKEN + "0"))),
      status(app(creds(HASH.slice(1), TOKEN))),
      status(app()),
      // 쿠키: 공백 · 같은 이름(뒤의 것) · = 없는 조각 · 값 안의 =
      status(app(), { cookies: { hash: "zz", " hash ": HASH, token: TOKEN, junk: "a=b" } }),
      { path: "/session", headers: app({ Cookie: `hash=${HASH}; flag; token=${TOKEN}=` }) },
    ],
  },
  {
    name: "auth_s3_not_configured", module: M,
    covers: ["auth.test.ts: S3 설정이 없으면 S3 를 호출하지 않고 null 이다 (각 설정이 하나씩 빠짐)"],
    bindings: { SESSION_SECRET: SECRETS.SESSION_SECRET },
    s3: okObject,
    requests: [status(app(session))],
  },
  ...Object.keys(S3).map((missing) => ({
    name: `auth_s3_missing_${missing.toLowerCase()}`, module: M,
    covers: [`auth.test.ts: S3 설정이 없으면 null (${missing})`],
    bindings: { ...SECRETS, [missing]: undefined },
    s3: okObject,
    requests: [status(app(session))],
  })),
  {
    name: "auth_s3_empty_setting", module: M,
    // 빈 글도 설정 없음 (!bucket)
    bindings: { ...SECRETS, SESSION_S3_REGION: "" },
    s3: okObject,
    requests: [status(app(session))],
  },
  {
    name: "auth_no_session_no_s3", module: M,
    covers: ["auth.test.ts: 세션 헤더가 없거나 익명이면 S3 를 호출하지 않는다"],
    bindings: SECRETS,
    s3: okObject,
    requests: [status(app()), status(app(creds("0x" + "0".repeat(40), "0x" + "0".repeat(64))))],
  },
  {
    name: "auth_verified", module: M,
    covers: ["auth.test.ts: token·email·ip 가 맞으면 이메일에서 만든 주소를 돌려주고, 서명된 GET 으로 hash/<hash> 를 읽는다"],
    bindings: SECRETS,
    s3: okObject,
    requests: [status(app(session))],
  },
  {
    name: "auth_stored_token_forms", module: M,
    covers: ["auth.test.ts: 저장된 token 이 0x 접두사나 대문자여도 같은 값이면 통과한다"],
    bindings: SECRETS,
    s3: ({ n }) => s3json({ token: n === 1 ? "0x" + TOKEN.toUpperCase() : "  " + TOKEN + "  ", email: "user@example.com" }),
    requests: [status(app(session)), status(app(session))],
  },
  {
    name: "auth_fail_closed", module: M,
    covers: ["auth.test.ts: 거부해야 하는 경우는 모두 null 이다 (token 한 글자 · token 없음 · email 없음 · email 공백 · ip 다름 · 404 · 403 · JSON 아님 · JSON null · 네트워크 오류)"],
    bindings: SECRETS,
    s3: ({ n }) =>
      [
        s3json({ token: TOKEN.slice(0, -1) + "e", email: "u@e.com" }),
        s3json({ email: "u@e.com" }),
        s3json({ token: TOKEN }),
        s3json({ token: TOKEN, email: "  " }),
        s3json({ token: TOKEN, email: "u@e.com", ip: "198.51.100.1" }),
        { status: 404, body: "NoSuchKey" },
        { status: 403, body: "AccessDenied" },
        { body: "<html>" },
        { body: "null" },
        { throw: "boom" },
        // 형이 다른 값
        s3json({ token: 1, email: "u@e.com" }),
        s3json({ token: TOKEN, email: ["u@e.com"] }),
        s3json({ token: TOKEN, email: "u@e.com", ip: 7 }),
      ][n - 1],
    requests: Array.from({ length: 13 }, () => status(app(session))),
  },
  {
    name: "auth_ip_absent_or_empty", module: M,
    covers: ["auth.test.ts: 객체에 ip 가 없으면 ip 는 비교하지 않는다"],
    bindings: SECRETS,
    s3: ({ n }) => s3json(n === 1 ? { token: TOKEN, email: "user@example.com" } : { token: TOKEN, email: "user@example.com", ip: "" }),
    requests: [status(app({ ...session, "cf-connecting-ip": "198.51.100.99" })), status(app({ ...session, "cf-connecting-ip": "198.51.100.99" }))],
  },
  {
    name: "auth_ip_compare_trimmed", module: M,
    // 요청의 IP 는 trim, 객체의 ip 는 그대로 비교한다
    bindings: SECRETS,
    s3: ({ n }) => s3json({ token: TOKEN, email: "u@e.com", ip: n === 1 ? IP : " " + IP }),
    requests: [status(app({ ...session, "cf-connecting-ip": " " + IP + " " })), status(app(session))],
  },
];
