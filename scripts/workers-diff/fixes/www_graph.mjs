// www graph.ts 에 덧대는 수정 — 형식은 fixes/www.mjs 와 같다 ({ id, worker: "www", file, find, replace, count? }).
// 2단계(graph)에서는 고친 것이 없다: 원본의 모든 갈래를 그대로 옮겼고(scenarios/www_graph.mjs 28개가 원본과 같음),
// 찾은 의심 자리는 동작 결정이 필요한 것이라 고치지 않고 보고에 남겼다 —
//   · /graph/local 은 스레드의 오래된 200건(ASC)만 주므로 200건 넘는 스레드의 새 평가를 고르면 그 평가가 응답에 없다 (graph_limit_200)
//   · /graph/similar 는 D1 을 확인하지 않아 행 없는 벡터(유령)도 노드로 나온다 (SCENARIOS GR-13, graph_similar_phantom)
export default [];
