// index.ts 의 처리기 차례와 레거시 처리기(405~1128). 레거시는 어떤 요청에도 418 로 끝나고(legacy.rs 머리말), 밖에서 보이는 것은
// console 기록('req.query.referrer …' · 'err …')과 어디서 던지는지뿐이다. 정적 파일(web/www)은 워커 앞에서 assets 가 답한다.
//
// 1단계에서는 큰 모듈 넷(evaluations · navtrials · analytic · graph)의 경로를 보내지 않는다: Rust 판은 그 자리가 비어 있어
// 레거시로 넘어가고 원본은 그 모듈이 답하므로 다르다. 여기 경로는 그 넷의 어느 경로와도 겹치지 않는다
// (/evaluations · /evaluations/{search,status,votes,supersedes,verdicts} · /public/evaluations/search · /nav/trials ·
//  /analytic/events · /analytic/sites · /graph · /graph/{local,similar,view}).
import { SECRETS, BOTH_LIMITERS, app } from "./www_common.mjs";

const M = "legacy";
const ADDR = "0x00000000000000000000000000000000000000ab";
const HREF = encodeURIComponent("https://Shop.Example.co.kr/Path/Item?x=1");

export default [
  {
    name: "legacy_root", module: M,
    // web/www 에 index.html 이 없어 / 는 워커로 온다: href 가 없으니 new URL('') 에서 TypeError
    bindings: SECRETS, limits: BOTH_LIMITERS,
    requests: [{ path: "/" }, { path: "/?referrer=r1" }, { path: "/anything/else?referrer=" }],
  },
  {
    name: "legacy_methods", module: M,
    // 메서드와 상관없이 같은 길 (PUT · POST · OPTIONS 갈래는 630 뒤라 닿지 않는다)
    bindings: SECRETS,
    requests: [
      { method: "POST", path: "/", headers: { "Content-Type": "application/json" }, body: { action: "update_from_rust", id: "x" } },
      { method: "POST", path: "/?href=" + HREF + "&id=" + ADDR, body: { link: "https://a.example/x" } },
      { method: "PUT", path: "/?href=" + HREF },
      { method: "OPTIONS", path: "/" },
      { method: "DELETE", path: "/x" },
      { method: "HEAD", path: "/" },
      { method: "PATCH", path: "/" },
    ],
  },
  {
    name: "legacy_href_paths", module: M,
    // href 가 올바른 주소면 572 의 `cookies` (ReferenceError), id 가 주소면 630 의 바인딩 없음 (TypeError)
    bindings: SECRETS,
    requests: [
      { path: "/?href=" + HREF + "&referrer=ref" },
      { path: "/?href=" + HREF + "&id=" + ADDR },
      { path: "/?href=" + HREF + "&id=" + ADDR.toUpperCase().replace("0X", "0x") },
      { path: "/?href=" + HREF + "&id=0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed" },
      { path: "/?href=" + HREF + "&id=0x5aaeb6053F3E94C9b9A09f33669435E7Ef1BeAed" },
      { path: "/?href=" + HREF + "&id=XE7338O073KYGTWWZN0F2WZ0R8PX5ZPPZS" },
      { path: "/?href=" + HREF + "&id=not-an-address" },
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent("https://shop.example.co.kr") + "&id=" + ADDR },
    ],
  },
  {
    name: "legacy_decode_errors", module: M,
    // origin 의 decodeURIComponent 가 먼저(로그 전), href 는 로그 뒤 — URIError
    bindings: SECRETS,
    requests: [
      { path: "/?origin=%25E0%25A4%25A&referrer=x" },
      { path: "/?href=%25ZZ&referrer=y" },
      { path: "/?href=%25ED%25A0%2580" },
      { path: "/?origin=%25C0%2580&href=" + HREF },
      { path: "/?href=%25" },
    ],
  },
  {
    name: "legacy_url_forms", module: M,
    // new URL(href) 가 받는 것 · 못 받는 것 (런타임의 URL)
    bindings: SECRETS,
    requests: [
      "javascript:alert(1)", "foo:bar", "http://exa mple.com", "http:/example.com", "//example.com/x", "https://[::1", "https://ün.example/ä",
      "HTTPS://EXAMPLE.COM:443/A", "http://a.b.c.co.uk:8080/p?q#f", "mailto:x@y", "http://%41.com", "http://a^b.com", "  https://trim.example  ",
    ].map((h) => ({ path: "/?href=" + encodeURIComponent(h) })),
  },
  {
    name: "legacy_csp_header", module: M,
    // origin 이 머리글 값으로 틀리면 headers.set('Content-Security-Policy', …) 에서 TypeError (런타임의 Headers)
    bindings: SECRETS,
    requests: [
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent(encodeURIComponent("https://a.example\nx")) },
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent(encodeURIComponent("https://한글.example")) },
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent(encodeURIComponent("a\u0000b")) },
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent(encodeURIComponent("a\rb")) },
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent(encodeURIComponent(" trailing ")) },
      { path: "/?href=" + HREF + "&origin=" + encodeURIComponent(encodeURIComponent("tab\there")) },
    ],
  },
  {
    name: "legacy_query_quirks", module: M,
    // 같은 이름은 뒤의 값 · + 는 공백 · __proto__ 는 무시 · 잘못된 % 는 URLSearchParams 가 그대로 둠
    bindings: SECRETS,
    requests: [
      { path: "/?referrer=a&referrer=b" },
      { path: "/?referrer=a+b%20c" },
      { path: "/?__proto__=x&referrer=p" },
      { path: "/?referrer=%E0%A4%A&href=" },
      { path: "/?referrer=%ED%95%9C%EA%B8%80" },
      { path: "/?referrer" },
      { path: "/?=v&referrer=k" },
      { path: "/?id=" + ADDR + "&href=" + HREF + "&created_at=12abc" },
      { path: "/?href=" + HREF + "&created_at=%2012%20&id=" + ADDR },
    ],
  },
  {
    name: "legacy_cf_and_ip", module: M,
    // request.cf 와 IP 머리글 (630 뒤에서만 쓰이므로 결과는 같다). 국가 코드가 글이 아니면 try 밖에서 던진다
    bindings: SECRETS,
    requests: [
      { path: "/", cf: { country: "US" }, headers: { "cf-connecting-ip": "198.51.100.7", "X-Real-IP": "10.0.0.1" } },
      { path: "/", cf: { country: "" }, headers: { "X-Real-IP": "10.0.0.1" } },
      { path: "/", cf: { country: "constructor" } },
      { path: "/", cf: { country: 5 } },
      { path: "/", cf: { country: true } },
      { path: "/", cf: null },
    ],
  },
  {
    name: "legacy_not_module_paths", module: M,
    // 모듈 처리기의 경로와 비슷하지만 아닌 것: 모두 레거시로
    bindings: SECRETS, limits: BOTH_LIMITERS,
    requests: [
      { path: "/SESSION", headers: app() },
      { path: "/session/start/", method: "POST", headers: app() },
      { path: "/sessions/start", method: "POST", headers: app() },
      { path: "/%73ession", headers: app() },
      { path: "/privacy/" },
    ],
  },
  {
    name: "legacy_assets_first", module: M,
    // 정적 파일은 워커 앞에서 assets 가 답한다 (두 판 모두 워커를 부르지 않는다 — end 기록이 비어야 한다)
    // tracker.test.ts 가 시험하는 web/www/static/js/tracker.js 도 이렇게 나간다
    requests: [{ path: "/static/js/tracker.js" }, { path: "/favicon.ico" }, { path: "/style.css" }, { path: "/static/js/none.js" }],
  },
];
