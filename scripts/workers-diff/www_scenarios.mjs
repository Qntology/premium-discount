// www 차분 시험의 시나리오 모음. 모듈마다 scenarios/www_<모듈>.mjs 가 자기 목록을 내고(시나리오 모양은 scenarios/www_common.mjs),
// 여기서 모듈 이름을 붙여 모은다. 2단계(evaluations · navtrials · graph · analytic)는 자기 파일만 채우면 되고 이 파일은 그대로 둔다.
import session from "./scenarios/www_session.mjs";
import auth from "./scenarios/www_auth.mjs";
import legacy from "./scenarios/www_legacy.mjs";
import evaluations from "./scenarios/www_evaluations.mjs";
import navtrials from "./scenarios/www_navtrials.mjs";
import graph from "./scenarios/www_graph.mjs";
import analytic from "./scenarios/www_analytic.mjs";

const BY_MODULE = { session, auth, legacy, evaluations, navtrials, graph, analytic };

export const SCENARIOS = Object.entries(BY_MODULE).flatMap(([module, list]) => list.map((s) => ({ module, ...s })));

const names = new Set();
for (const s of SCENARIOS) {
  if (names.has(s.name)) throw new Error(`duplicate scenario name ${s.name}`);
  names.add(s.name);
}
