import { ValidationError, parseDomain, parseUuid, verdictSummaries } from './evaluations'
import type { EvalEnv } from './evaluations'
import type { Tier } from '../../cron/src/verdict'
import { authorKey } from './projection'
import { isRateLimited, RETRY_AFTER_SECONDS } from './ratelimit'

// TODO(결정 보류): 그래프 노드 수 상한. 확정 전까지 임시값이며 변경될 수 있음.
const GRAPH_NODE_LIMIT_TEMP = 200
const SIMILAR_TOP_K_DEFAULT = 10
// 자기 자신을 제외하려고 topK + 1을 요청하므로 최대 49 (Vectorize 상한 50)
const SIMILAR_TOP_K_MAX = 49

// 공개 엔드포인트. 인증은 없고, 요청 제한은 ratelimit.ts의 EVAL_READ_LIMITER(IP 기준)를 쓴다. /graph/view 페이지는 제외한다.
// 응답에는 원문과 주소(from, to 원값)가 들어가지 않는다. 작성자는 표시용 키로만 나온다.
// 평가 노드에는 판정 등급(tier, grade)을 붙이고, 응답에는 평가 노드의 등급별 개수(summary)를 붙인다. 투표자는 나오지 않는다.

// 판정은 eval_verdicts에 있다. 행이 없으면 아직 판정 전이다 (unrated, grade null)
const SELECT_COLS = `e."id", e."from", e."cc", e."ref", e."root", e."created_at", v."tier", v."grade"`
const FROM_JOINED = `evaluations e LEFT JOIN eval_verdicts v ON v."eval_id" = e."id"`
const GRAPH_PATHS = ['/graph', '/graph/local', '/graph/similar', '/graph/view']

type Row = {
	id: string
	from: string
	cc: string
	ref: string | null
	root: string
	created_at: number
	tier: Tier | null
	grade: number | null
}

type Node = {
	id: string
	type: 'evaluation' | 'domain' | 'author'
	label: string
	cc?: string
	root?: string
	created_at?: number
	// 평가 노드만
	tier?: Tier
	grade?: number | null
}

type Edge = {
	source: string
	target: string
	type: 'belongs' | 'authored' | 'reply' | 'similar'
	score?: number
}

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' },
	})
}

// 응답에 들어간 평가 노드의 등급별 개수
function summarize(nodes: Node[]): Record<Tier, number> {
	const summary: Record<Tier, number> = { premium: 0, neutral: 0, discount: 0, unrated: 0 }
	for (const n of nodes) {
		if (n.type === 'evaluation') summary[n.tier ?? 'unrated']++
	}
	return summary
}

function parseTopK(raw: string | null): number {
	if (raw === null) return SIMILAR_TOP_K_DEFAULT
	const n = Number(raw)
	if (!Number.isInteger(n) || n < 1 || n > SIMILAR_TOP_K_MAX) {
		throw new ValidationError(`topK must be an integer between 1 and ${SIMILAR_TOP_K_MAX}`)
	}
	return n
}

// rows -> 노드/엣지. 평가 노드, 도메인 허브, 작성자 허브(표시용 키)를 만든다
async function buildGraph(rows: Row[], env: EvalEnv): Promise<Response> {
	const ids = new Set(rows.map((r) => r.id))
	const keys = await Promise.all(rows.map((r) => authorKey(r.from, env.AUTHOR_KEY_SECRET)))
	const nodes = new Map<string, Node>()
	const edges: Edge[] = []

	rows.forEach((r, i) => {
		const domainId = 'domain:' + r.cc
		const authorId = 'author:' + keys[i]
		nodes.set(r.id, {
			id: r.id,
			type: 'evaluation',
			label: r.id.slice(0, 8),
			cc: r.cc,
			root: r.root,
			created_at: r.created_at,
			tier: r.tier ?? 'unrated',
			grade: r.grade,
		})
		if (!nodes.has(domainId)) nodes.set(domainId, { id: domainId, type: 'domain', label: r.cc })
		if (!nodes.has(authorId)) {
			nodes.set(authorId, { id: authorId, type: 'author', label: '작성자 ' + keys[i].slice(0, 6) })
		}
		edges.push({ source: r.id, target: domainId, type: 'belongs' })
		edges.push({ source: r.id, target: authorId, type: 'authored' })
		// 답글 관계: 부모가 같은 응답 안에 있을 때만 연결
		if (r.ref && ids.has(r.ref)) edges.push({ source: r.id, target: r.ref, type: 'reply' })
	})

	const all = [...nodes.values()]
	return json({ nodes: all, edges, summary: summarize(all) })
}

// GET /graph?cc=example.com : 도메인 범위 그래프
async function domainGraph(url: URL, env: EvalEnv): Promise<Response> {
	const cc = parseDomain(url.searchParams.get('cc'))
	const rs = await env.DB.prepare(
		`SELECT ${SELECT_COLS} FROM ${FROM_JOINED} WHERE e."cc" = ?1 ORDER BY e."created_at" DESC LIMIT ?2`
	).bind(cc, GRAPH_NODE_LIMIT_TEMP).all<Row>()
	return buildGraph(rs.results, env)
}

// GET /graph/local?id=... : 선택한 평가가 속한 스레드(같은 root) 그래프
async function localGraph(url: URL, env: EvalEnv): Promise<Response> {
	const id = parseUuid(url.searchParams.get('id'), 'id')
	const base = await env.DB.prepare(
		`SELECT "root" FROM evaluations WHERE "id" = ?1`
	).bind(id).first<{ root: string }>()
	if (!base) return json({ error: 'not found' }, 404)
	const rs = await env.DB.prepare(
		`SELECT ${SELECT_COLS} FROM ${FROM_JOINED} WHERE e."root" = ?1 ORDER BY e."created_at" ASC LIMIT ?2`
	).bind(base.root, GRAPH_NODE_LIMIT_TEMP).all<Row>()
	return buildGraph(rs.results, env)
}

// GET /graph/similar?id=...&topK=10 : 선택한 평가와 벡터가 비슷한 평가 (같은 cc 안에서)
// 벡터는 서버 안에서만 쓰고 응답에 포함하지 않는다.
async function similarGraph(url: URL, env: EvalEnv): Promise<Response> {
	const id = parseUuid(url.searchParams.get('id'), 'id')
	const topK = parseTopK(url.searchParams.get('topK'))
	const base = await env.DB.prepare(
		`SELECT "cc" FROM evaluations WHERE "id" = ?1`
	).bind(id).first<{ cc: string }>()
	if (!base) return json({ error: 'not found' }, 404)

	const [vec] = await env.EVAL_VECTORIZE.getByIds([id])
	if (!vec || !vec.values) return json({ error: 'vector not found' }, 404)

	const res = await env.EVAL_VECTORIZE.query(Array.from(vec.values), {
		topK: topK + 1,
		filter: { cc: base.cc },
	})
	const matches = res.matches.filter((m) => m.id !== id).slice(0, topK)
	// 최대 49건이라 IN 한 번으로 읽는다
	const verdicts = await verdictSummaries(env, matches.map((m) => m.id))

	const nodes: Node[] = matches.map((m) => ({
		id: m.id,
		type: 'evaluation',
		label: m.id.slice(0, 8),
		cc: base.cc,
		tier: (verdicts.get(m.id)?.tier ?? 'unrated') as Tier,
		grade: verdicts.get(m.id)?.grade ?? null,
	}))
	const edges: Edge[] = matches.map((m) => ({
		source: id,
		target: m.id,
		type: 'similar',
		score: m.score,
	}))
	return json({ nodes, edges, summary: summarize(nodes) })
}

export async function handleGraph(request: Request, env: EvalEnv): Promise<Response | null> {
	const url = new URL(request.url)
	if (!GRAPH_PATHS.includes(url.pathname)) return null
	if (request.method !== 'GET') return json({ error: 'method not allowed' }, 405)

	if (url.pathname === '/graph/view') {
		return new Response(VIEW_HTML, {
			headers: { 'Content-Type': 'text/html; charset=utf-8' },
		})
	}

	// /graph, /graph/local은 D1 조회, /graph/similar는 Vectorize 조회가 따른다
	if (await isRateLimited(request, env.EVAL_READ_LIMITER)) {
		return new Response(JSON.stringify({ error: 'rate limit exceeded' }), {
			status: 429,
			headers: {
				'Content-Type': 'application/json; charset=utf-8',
				'Cache-Control': 'no-store',
				'Retry-After': String(RETRY_AFTER_SECONDS),
			},
		})
	}

	try {
		if (url.pathname === '/graph') return await domainGraph(url, env)
		if (url.pathname === '/graph/local') return await localGraph(url, env)
		return await similarGraph(url, env)
	} catch (err) {
		if (err instanceof ValidationError) return json({ error: err.message }, 400)
		console.log('graph error', err)
		return json({ error: 'internal error' }, 500)
	}
}

// 옵시디언식 그래프 뷰 (d3 force 레이아웃). 데이터는 위 API에서만 받는다.
const VIEW_HTML = `<!doctype html>
<html lang="ko">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>평가 그래프</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; background: #1e1e1e; color: #ddd; font-family: system-ui, sans-serif; overflow: hidden; }
  #bar { position: fixed; top: 0; left: 0; right: 0; padding: 10px 14px; background: #262626; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; z-index: 2; }
  #bar input { background: #111; color: #eee; border: 1px solid #444; padding: 6px 8px; border-radius: 6px; }
  #bar button, #panel button { background: #3a3a3a; color: #eee; border: 0; padding: 6px 10px; border-radius: 6px; cursor: pointer; margin: 4px 4px 0 0; }
  #status { font-size: 12px; color: #999; }
  .legend { font-size: 12px; }
  .legend span { margin-right: 10px; }
  svg { position: fixed; top: 48px; left: 0; width: 100vw; height: calc(100vh - 48px); }
  #panel { position: fixed; right: 0; top: 48px; width: 280px; bottom: 0; background: #242424; padding: 12px; overflow: auto; font-size: 13px; display: none; border-left: 1px solid #333; z-index: 3; }
</style>
</head>
<body>
<div id="bar">
  <input id="cc" placeholder="도메인 (예: example.com)" size="24">
  <button id="load">그래프 불러오기</button>
  <span class="legend">
    <span style="color:#7aa2f7">● 평가</span>
    <span style="color:#ff9e64">● 도메인</span>
    <span style="color:#9ece6a">● 작성자</span>
    <span style="color:#bbbbbb">— 답글</span>
    <span style="color:#e0af68">┄ 유사</span>
  </span>
  <span id="status"></span>
</div>
<svg id="canvas"></svg>
<div id="panel"></div>
<script src="https://cdnjs.cloudflare.com/ajax/libs/d3/7.9.0/d3.min.js"></script>
<script>
(function () {
  var COLORS = { evaluation: '#7aa2f7', domain: '#ff9e64', author: '#9ece6a' };
  var EDGE_COLORS = { belongs: '#555555', authored: '#3b5a3b', reply: '#bbbbbb', similar: '#e0af68' };

  var svg = d3.select('#canvas');
  var g = svg.append('g');
  svg.call(d3.zoom().scaleExtent([0.1, 8]).on('zoom', function (e) { g.attr('transform', e.transform); }));

  var nodes = [];
  var links = [];
  var nodeIds = {};
  var edgeKeys = {};
  var sim = null;
  var statusEl = document.getElementById('status');
  var panel = document.getElementById('panel');

  function setStatus(t) { statusEl.textContent = t; }

  function getJSON(url) {
    return fetch(url).then(function (r) {
      return r.json().then(function (body) {
        if (!r.ok) throw new Error(body.error || ('HTTP ' + r.status));
        return body;
      });
    });
  }

  function addGraph(data) {
    (data.nodes || []).forEach(function (n) {
      if (!nodeIds[n.id]) { nodeIds[n.id] = true; nodes.push(n); }
    });
    (data.edges || []).forEach(function (e) {
      var key = e.source + '|' + e.target + '|' + e.type;
      if (!edgeKeys[key]) { edgeKeys[key] = true; links.push(e); }
    });
  }

  function resetGraph(data) {
    nodes = []; links = []; nodeIds = {}; edgeKeys = {};
    addGraph(data);
    draw();
  }

  function draw() {
    if (sim) sim.stop();
    g.selectAll('*').remove();

    // 시뮬레이션이 원본 객체를 바꾸지 않도록 복사해서 넘긴다
    var simLinks = links.map(function (l) {
      return { source: l.source, target: l.target, type: l.type, score: l.score };
    });

    var linkSel = g.append('g').selectAll('line').data(simLinks).join('line')
      .attr('stroke', function (d) { return EDGE_COLORS[d.type] || '#555555'; })
      .attr('stroke-width', function (d) { return d.type === 'similar' ? 1 + (d.score || 0) * 3 : 1; })
      .attr('stroke-dasharray', function (d) { return d.type === 'similar' ? '4,3' : null; });

    var nodeSel = g.append('g').selectAll('g').data(nodes).join('g')
      .attr('cursor', 'pointer')
      .on('click', function (ev, d) { select(d); });

    nodeSel.append('circle')
      .attr('r', 7)
      .attr('fill', function (d) { return COLORS[d.type] || '#999999'; });
    nodeSel.append('text')
      .attr('x', 10).attr('y', 4).attr('font-size', 11).attr('fill', '#cccccc')
      .text(function (d) { return d.label; });

    nodeSel.call(d3.drag()
      .on('start', function (ev, d) {
        if (!ev.active) sim.alphaTarget(0.3).restart();
        d.fx = d.x; d.fy = d.y;
      })
      .on('drag', function (ev, d) { d.fx = ev.x; d.fy = ev.y; })
      .on('end', function (ev, d) {
        if (!ev.active) sim.alphaTarget(0);
        d.fx = null; d.fy = null;
      }));

    sim = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(simLinks).id(function (d) { return d.id; })
        .distance(function (d) { return d.type === 'similar' ? 40 : 60; })
        .strength(0.3))
      .force('charge', d3.forceManyBody().strength(-150))
      .force('center', d3.forceCenter(window.innerWidth / 2, (window.innerHeight + 48) / 2))
      .force('collide', d3.forceCollide(12))
      .on('tick', function () {
        linkSel
          .attr('x1', function (d) { return d.source.x; })
          .attr('y1', function (d) { return d.source.y; })
          .attr('x2', function (d) { return d.target.x; })
          .attr('y2', function (d) { return d.target.y; });
        nodeSel.attr('transform', function (d) { return 'translate(' + d.x + ',' + d.y + ')'; });
      });
  }

  function makeButton(label, handler) {
    var b = document.createElement('button');
    b.textContent = label;
    b.onclick = handler;
    return b;
  }

  // 내가 쓴 글이면 원문 미리보기를 패널에 덧붙인다 (오버레이).
  // 원문은 서버에 없고, 이 브라우저의 로컬 캐시(IndexedDB 'PremiumDiscountLocal'의 evaluations, 앱 UI의 localdb.ts가 채운다)에만 있다.
  // 캐시는 내 글만 담고 있으므로, 조회되는 글은 곧 내 글이다. 캐시가 없으면(다른 기기, 삭제) 아무것도 덧붙이지 않는다.
  // DB를 새로 만들지 않는다: 없는 DB를 열면 upgradeneeded가 뜨는데, 그때 바로 취소한다.
  var LOCAL_DB = 'PremiumDiscountLocal';
  var PREVIEW_MAX = 500;
  var selectedId = null;

  function findLocalRecord(id) {
    return new Promise(function (resolve) {
      if (!window.indexedDB) { resolve(null); return; }
      var req;
      try { req = indexedDB.open(LOCAL_DB); } catch (e) { resolve(null); return; }
      req.onupgradeneeded = function () { req.transaction.abort(); };
      req.onerror = function () { resolve(null); };
      req.onsuccess = function () {
        var db = req.result;
        try {
          if (!db.objectStoreNames.contains('evaluations')) { db.close(); resolve(null); return; }
          var store = db.transaction('evaluations', 'readonly').objectStore('evaluations');
          if (!store.indexNames.contains('id')) { db.close(); resolve(null); return; }
          var get = store.index('id').getAll(id);
          get.onsuccess = function () { db.close(); resolve(get.result[0] || null); };
          get.onerror = function () { db.close(); resolve(null); };
        } catch (e) {
          try { db.close(); } catch (e2) {}
          resolve(null);
        }
      };
    });
  }

  function showLocalPreview(d) {
    findLocalRecord(d.id).then(function (row) {
      // 조회하는 사이에 다른 노드를 눌렀으면 덧붙이지 않는다
      if (!row || selectedId !== d.id) return;
      var text = row.data && typeof row.data.text === 'string' ? row.data.text : '';
      if (!text) return;
      var box = document.createElement('div');
      box.style.margin = '10px 0';
      box.style.padding = '8px';
      box.style.background = '#1b2a1b';
      box.style.borderRadius = '6px';
      var head = document.createElement('div');
      head.style.fontSize = '11px';
      head.style.color = '#9ece6a';
      head.textContent = '내가 쓴 글 (이 기기에만 저장됨)';
      var body = document.createElement('div');
      body.style.whiteSpace = 'pre-wrap';
      body.style.marginTop = '4px';
      body.textContent = text.length > PREVIEW_MAX ? text.slice(0, PREVIEW_MAX) + '…' : text;
      box.appendChild(head);
      box.appendChild(body);
      panel.appendChild(box);
    });
  }

  function select(d) {
    selectedId = d.id;
    panel.style.display = 'block';
    panel.textContent = '';
    var title = document.createElement('div');
    title.style.fontWeight = '600';
    title.textContent = d.label;
    panel.appendChild(title);
    var info = document.createElement('div');
    info.style.color = '#999999';
    info.style.margin = '6px 0';
    info.textContent = d.type + ' · ' + d.id;
    panel.appendChild(info);

    if (d.type === 'evaluation') {
      showLocalPreview(d);
      panel.appendChild(makeButton('같은 스레드 보기', function () { loadLocal(d.id); }));
      panel.appendChild(makeButton('유사 평가 10개 연결', function () { loadSimilar(d.id); }));
    } else if (d.type === 'domain') {
      panel.appendChild(makeButton('이 도메인 그래프 불러오기', function () { loadDomain(d.label); }));
    }
  }

  function loadDomain(cc) {
    document.getElementById('cc').value = cc;
    setStatus('불러오는 중...');
    getJSON('/graph?cc=' + encodeURIComponent(cc)).then(function (data) {
      resetGraph(data);
      setStatus(nodes.length + '개 노드 · ' + links.length + '개 연결');
    }).catch(function (e) { setStatus('오류: ' + e.message); });
  }

  function loadLocal(id) {
    setStatus('불러오는 중...');
    getJSON('/graph/local?id=' + encodeURIComponent(id)).then(function (data) {
      resetGraph(data);
      setStatus('스레드 그래프 · ' + nodes.length + '개 노드');
    }).catch(function (e) { setStatus('오류: ' + e.message); });
  }

  function loadSimilar(id) {
    setStatus('유사 평가 조회 중...');
    getJSON('/graph/similar?id=' + encodeURIComponent(id) + '&topK=10').then(function (data) {
      addGraph(data);
      draw();
      setStatus('유사 연결 ' + (data.edges || []).length + '개 추가');
    }).catch(function (e) { setStatus('오류: ' + e.message); });
  }

  document.getElementById('load').onclick = function () {
    var cc = document.getElementById('cc').value.trim();
    if (!cc) { setStatus('도메인을 입력하세요'); return; }
    loadDomain(cc);
  };

  var initial = new URLSearchParams(location.search).get('cc');
  if (initial) loadDomain(initial);
})();
</script>
</body>
</html>
`;
