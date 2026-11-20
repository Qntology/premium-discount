// session.ts (POST /session/start · GET /session) 와 그것이 쓰는 ratelimit.ts. session.test.ts 의 단언을 모두 옮겼다
// (마지막 단언 "로그인한 앱이 저장하면 from 이…" 은 POST /evaluations 라 www_evaluations.mjs 의 몫).
import { SECRETS, S3, HASH, TOKEN, IP, ORIGIN, BOTH_LIMITERS, app, creds, issued, s3json } from "./www_common.mjs";

const M = "session";
const start = (extra = {}) => ({ method: "POST", path: "/session/start", headers: app(extra) });
const status = (headers) => ({ path: "/session", headers });

export default [
  {
    name: "session_issue", module: M,
    covers: ["session.test.ts: 발급: hash 40자리, token 은 HMAC, 웹 QR 과 같은 모양의 mailto · 발급할 때마다 다르다", "session.test.ts: 발급 응답은 캐시되지 않고 CORS 헤더가 붙는다"],
    bindings: SECRETS, limits: BOTH_LIMITERS,
    requests: [start(), start()],
  },
  {
    name: "session_mail_host", module: M,
    covers: ["session.test.ts: 메일 호스트는 설정으로 바꿀 수 있다"],
    bindings: { ...SECRETS, SESSION_MAIL_HOST: "example.test" },
    requests: [start()],
  },
  {
    name: "session_mail_host_empty", module: M,
    // env.SESSION_MAIL_HOST || 'premium.discount' — 빈 글이면 기본값
    bindings: { ...SECRETS, SESSION_MAIL_HOST: "" },
    requests: [start()],
  },
  {
    name: "session_no_secret", module: M,
    covers: ["session.test.ts: SESSION_SECRET 이 없으면 503 이다 (발급하지 않는다)"],
    bindings: { ...SECRETS, SESSION_SECRET: undefined }, limits: BOTH_LIMITERS,
    requests: [start(), { ...start(), headers: app() }],
  },
  {
    name: "session_anonymous", module: M,
    covers: ["session.test.ts: 헤더가 없거나 익명 자리값이면 anonymous 이고 S3 를 읽지 않는다"],
    bindings: SECRETS,
    s3: () => s3json({ email: "x@e.com" }),
    requests: [status(app()), status(app(creds("0x" + "0".repeat(40), "0x" + "0".repeat(64))))],
  },
  {
    name: "session_pending", module: M,
    covers: ["session.test.ts: 발급받았지만 메일이 아직 없으면 pending (S3 의 404 도 403 도 '아직 없음') · email 없는 객체도 pending · S3 주소"],
    bindings: SECRETS,
    s3: ({ n }) => (n === 1 ? { status: 404 } : n === 2 ? { status: 403 } : s3json({})),
    requests: [start(), status(issued(0)), status(issued(0)), status(issued(0))],
  },
  {
    name: "session_signed_in", module: M,
    covers: ["session.test.ts: 메일이 도착해 S3 에 email 이 생기면 signed_in 이고 주소는 이메일에서 만든다"],
    bindings: SECRETS, limits: BOTH_LIMITERS,
    s3: () => s3json({ email: "User@Example.com" }),
    requests: [start(), status(issued(0))],
  },
  {
    name: "session_app_ignores_ip", module: M,
    covers: ["session.test.ts: 앱 세션은 ip 를 비교하지 않는다 (메일은 다른 기기에서 보낸다)"],
    bindings: SECRETS,
    s3: () => s3json({ email: "u@e.com", ip: "198.51.100.1" }),
    requests: [start(), status(issued(0))],
  },
  {
    name: "session_app_email_trimmed", module: M,
    // 앱 세션은 trim 한 email 로 주소를 만든다 (웹 세션과 견줄 것: session_web_email_padded)
    bindings: SECRETS,
    s3: () => s3json({ email: "  u@e.com  " }),
    requests: [start(), status(issued(0))],
  },
  {
    name: "session_token_mismatch", module: M,
    covers: ["session.test.ts: token 이 한 글자라도 다르면 S3 에 email 이 있어도 anonymous 이다 · 다른 비밀로 발급된 것처럼 보이는 token 도"],
    bindings: SECRETS,
    s3: () => s3json({ email: "u@e.com" }),
    requests: [
      start(),
      status((prev) => {
        const t = prev[0].body.token;
        return app(creds(prev[0].body.hash, t.slice(0, -1) + (t.endsWith("0") ? "1" : "0")));
      }),
      status((prev) => app(creds(prev[0].body.hash, "ab".repeat(32)))),
    ],
  },
  {
    name: "session_unissued_hash", module: M,
    covers: ["session.test.ts: 발급한 적 없는 hash 는 S3 에 email 이 있어도 로그인이 아니다"],
    bindings: SECRETS,
    s3: () => s3json({ email: "victim@e.com" }),
    requests: [status(app(creds("cd".repeat(20), "ef".repeat(32))))],
  },
  {
    name: "session_web_session", module: M,
    covers: ["session.test.ts: 웹 세션(S3 의 token 일치)은 그대로 로그인된다 · token 이 다르면 anonymous"],
    bindings: SECRETS,
    s3: ({ n }) => s3json({ token: n === 1 ? "cd".repeat(32) : "ee".repeat(32), email: "web@e.com" }),
    requests: [status(app(creds("ab".repeat(20), "cd".repeat(32)))), status(app(creds("ab".repeat(20), "cd".repeat(32))))],
  },
  {
    name: "session_web_email_padded", module: M,
    // 웹 세션: 확인은 trim 한 email 로 하지만 원본은 obj.email 그대로 주소를 만든다 (fix-web-session-email-trim)
    fixes: ["fix-web-session-email-trim"],
    bindings: SECRETS,
    s3: () => s3json({ token: TOKEN, email: " Web@E.com\n" }),
    requests: [status(app(creds(HASH, TOKEN)))],
  },
  {
    name: "session_s3_errors", module: M,
    covers: ["session.test.ts: S3 오류나 설정 누락은 로그인으로 보지 않고 503 unavailable 이다 (500 · 네트워크 오류)"],
    // S3 500: aws4fetch 가 10번 더 시도한다(50ms × 2^i × 난수만큼 기다림 — 가상 시계 · 씨앗 난수로 두 판이 같아야 한다)
    bindings: SECRETS,
    s3: ({ n }) => (n <= 11 ? { status: 500 } : { throw: "network" }),
    requests: [start(), status(issued(0)), status(issued(0))],
  },
  {
    name: "session_s3_not_configured", module: M,
    covers: ["session.test.ts: S3 설정 누락은 503"],
    bindings: { ...SECRETS, SESSION_S3_BUCKET: undefined },
    s3: () => s3json({ email: "u@e.com" }),
    requests: [start(), status(issued(0))],
  },
  {
    name: "session_s3_throttled_then_ok", module: M,
    // 429 · 503 은 aws4fetch 가 다시 시도한다: 세 번째에 객체가 오면 signed_in
    bindings: SECRETS,
    s3: ({ n }) => (n === 1 ? { status: 429 } : n === 2 ? { status: 503 } : s3json({ email: "late@e.com" })),
    requests: [start(), status(issued(0))],
  },
  {
    name: "session_s3_odd_objects", module: M,
    // 200 인데 JSON 이 아니면 res.json() 이 던져 unavailable(503), JSON null · 수 · 글은 pending, 배열은 email 이 없어 pending
    bindings: SECRETS,
    s3: ({ n }) => [{ body: "<html>" }, { body: "null" }, { body: "42" }, { body: '"x"' }, { body: "[1,2]" }, { body: "" }][n - 1],
    requests: [start(), ...Array.from({ length: 6 }, () => status(issued(0)))],
  },
  {
    name: "session_rate_limits", module: M,
    covers: ["session.test.ts: 발급은 저장 제한기, 상태는 읽기 제한기를 쓰고 걸리면 429 이다 · Retry-After 60 · 키는 IP · 제한에 걸리면 S3 를 읽지 않는다"],
    bindings: SECRETS,
    limits: { EVAL_WRITE_LIMITER: false, EVAL_READ_LIMITER: false },
    s3: () => s3json({ token: TOKEN, email: "u@e.com" }),
    requests: [start(), status(app(creds(HASH, TOKEN)))],
  },
  {
    name: "session_limiter_quirks", module: M,
    // 제한기가 던지면 막지 않는다(로그만) · IP 가 없으면 부르지 않는다 · 앞뒤 공백 IP 는 trim · success 가 없으면 막힌다
    bindings: SECRETS,
    limits: { EVAL_WRITE_LIMITER: "throw", EVAL_READ_LIMITER: (n) => (n === 1 ? {} : { success: true }) },
    requests: [
      start(),
      { method: "POST", path: "/session/start", headers: { Origin: ORIGIN } },
      { method: "POST", path: "/session/start", headers: { Origin: ORIGIN, "cf-connecting-ip": "  " + IP + "  " } },
      status(app()),
      status(app()),
    ],
  },
  {
    name: "session_preflight_and_methods", module: M,
    covers: ["session.test.ts: 사전 요청(204, Allow-Headers 에 x-oauth-hash) · GET /session/start 405 · POST /session 405 · 다른 경로는 넘긴다"],
    bindings: SECRETS, limits: BOTH_LIMITERS,
    requests: [
      { method: "OPTIONS", path: "/session", headers: { Origin: ORIGIN } },
      { method: "OPTIONS", path: "/session/start", headers: { Origin: "https://evil.example" } },
      { path: "/session/start", headers: app() },
      { method: "POST", path: "/session", headers: app() },
      { method: "PUT", path: "/session", headers: app() },
      { method: "DELETE", path: "/session/start", headers: app() },
      { path: "/sessions", headers: app() },
      { path: "/session/", headers: app() },
      { path: "/session?x=1", headers: app() },
    ],
  },
  {
    name: "session_origins", module: M,
    // CORS: 앱 웹뷰 두 출처만 Allow-Origin 을 받는다
    bindings: SECRETS,
    requests: [start({ Origin: "http://tauri.localhost" }), start({ Origin: "https://evil.example" }), { method: "POST", path: "/session/start", headers: { "cf-connecting-ip": IP } }],
  },
  {
    name: "session_cookie_credentials", module: M,
    // 브라우저 쿠키(hash · token)로 상태를 묻는 웹 세션
    bindings: SECRETS,
    s3: () => s3json({ token: TOKEN, email: "cookie@e.com", ip: IP }),
    requests: [{ path: "/session", headers: app(), cookies: { a: "1", hash: HASH, token: TOKEN } }],
  },
  {
    name: "session_s3_signature", module: M,
    // S3 요청의 서명(aws4fetch): 가짜 S3 가 같은 x-amz-date 로 다시 계산해 맞는지 본다. 시각을 바꿔 두 번
    bindings: SECRETS,
    s3: () => s3json({ token: TOKEN, email: "sig@e.com" }),
    requests: [{ ...status(app(creds(HASH, TOKEN))), at: 1760000000000 }, { ...status(app(creds(HASH, TOKEN))), at: 1799999999999 }],
  },
];
