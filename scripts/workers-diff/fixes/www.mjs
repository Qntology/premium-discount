// www 원본(35781c0 workers/www/src)에 덧대는 수정 — Rust 판(workers/www)의 같은 수정과 1:1. id 는 docs/workers-rs.md 의 "고친 버그" 표와 같다.
// file 은 저장소 안의 경로다 (www.mjs 가 원본 나무를 그 모양으로 꺼낸다).
export default [
  // 웹 세션이 확인은 trim 한 email 로 하고 돌려주는 것은 obj.email 그대로여서, 공백 붙은 이메일이 앱 세션과 다른 주소가 되던 것
  {
    id: "fix-web-session-email-trim", worker: "www", file: "workers/www/src/auth.ts",
    find: "return { state: 'verified', email: obj.email as string }",
    replace: "return { state: 'verified', email }",
  },
];
