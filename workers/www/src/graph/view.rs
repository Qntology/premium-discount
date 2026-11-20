//! 원본 `graph.ts` 의 `VIEW_HTML` (옵시디언식 그래프 뷰, d3 force 레이아웃) — 원본 템플릿 리터럴의 값 그대로(바이트 같음).
//! 원본 핸들러로 뽑아 넣었다. 데이터는 /graph* API 에서만 받는다.

/// `VIEW_HTML`
pub const VIEW_HTML: &str = r#"<!doctype html>
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
"#;
