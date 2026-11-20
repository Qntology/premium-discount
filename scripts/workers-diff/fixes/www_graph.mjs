// www graph.ts 에 덧대는 수정 — 형식은 fixes/www.mjs 와 같다 ({ id, worker: "www", file, find, replace, count? }).
// 2단계(graph)에서는 고친 것이 없다: 원본의 모든 갈래를 그대로 옮겼고(scenarios/www_graph.mjs 28개가 원본과 같음),
// 찾은 의심 자리 둘은 동작 결정이 필요해 보고에 남겼다가 그 뒤 고쳤다 — 고침은 fixes/www_behaviour.mjs 에 있다:
//   · fix-graph-local-window: /graph/local 이 스레드의 오래된 200건(ASC)만 줘 200건 넘는 스레드의 새 평가를 고르면 그 평가가 응답에 없었다
//   · fix-graph-similar-phantom: /graph/similar(와 검색)가 D1 을 확인하지 않아 행 없는 벡터(유령)도 노드로 나왔다 (SCENARIOS GR-13)
export default [];
