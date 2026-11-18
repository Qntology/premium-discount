# 실제 EmbeddingGemma 모델로 실행 파일(main)을 끝까지 돌리는 검증. 모델 파일(약 1.5GB)이 필요해서 cargo test에는 넣지 않았다.
#
#   cargo build --release --bin premium-discount-app          # app 폴더에서
#   python3 tests/e2e/mock_server.py 18787 /tmp/server.log &   # 가짜 서버 (www의 검증 규칙과 멱등 request_id를 흉내)
#   python3 tests/e2e/e2e_main.py target/release/premium-discount-app <EMBEDDINGGEMMA_DIR> /tmp/pd-data 18787 /tmp/server.log
#
# 실제 EmbeddingGemma로 main(표준 입출력)을 끝까지 돌린다: 저장 → 로컬 검색 → 병합 검색 → 캐시 동기화 → flush
import json, subprocess, sys, time, os, math
BIN, MODEL, DATA, PORT, LOG = sys.argv[1:6]
env = dict(os.environ, PD_MODEL_DIR=MODEL, PD_DATA_DIR=DATA, PD_SERVER_URL=f'http://127.0.0.1:{PORT}')
t0 = time.time()
p = subprocess.Popen([BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True, bufsize=1)
# 준비 신호(표준 오류의 'ready') 기다리기
ready = p.stderr.readline().strip()
print(f'startup: {ready!r} in {time.time()-t0:.1f}s')
def call(i, cmd, args=None):
    t = time.time(); p.stdin.write(json.dumps({'id': i, 'cmd': cmd, 'args': args}) + '\n'); p.stdin.flush()
    r = json.loads(p.stdout.readline()); print(f'  [{cmd}] {time.time()-t:.2f}s ok={r["ok"]}'); return r
BCC = '0x' + 'ab'*20
texts = ['배송이 빨라서 만족합니다. 다시 구매할게요.', 'The battery life is excellent and the screen is sharp.', '가격 대비 성능이 훌륭하다']
for i, t in enumerate(texts, 1):
    r = call(i, 'cmd_submit_evaluation', {'text': t, 'cc': 'example.com', 'bcc': BCC})
    assert r['ok'] and r['result']['status'] == 'accepted', r
r = call(10, 'cmd_submit_evaluation', {'text': '시.발 이게 뭐야', 'cc': 'example.com'}); assert r['ok'] and r['result']['status'] == 'blocked', r
print('  profane -> blocked (no embed, no send)')
r = call(11, 'cmd_search_local', {'query': '배송이 빨라서 만족합니다. 다시 구매할게요.', 'top_k': 3}); assert r['ok']
m = r['result']; print('  local top:', [(x['text'][:14], round(x['score'], 3)) for x in m])
# 질의·문서는 접두어가 달라 같은 문장도 1.0이 아니다. 1등이 같은 문장이고 2등과 간격이 충분하면 정상
assert m[0]['text'].startswith('배송이') and m[0]['score'] > 0.9 and m[0]['score'] - m[1]['score'] > 0.15, m
r = call(12, 'cmd_search_local', {'query': '배터리 오래가고 화면이 선명해요', 'top_k': 3}); print('  local semantic (ko query → en text):', [(x['text'][:20], round(x['score'], 3)) for x in r['result']])
assert r['result'][0]['text'].startswith('The battery'), '의미 검색 순위'
r = call(13, 'cmd_search_merged', {'query': '가격 대비 성능', 'top_k': 5}); print('  merged:', [(x['source'], (x['text'] or '')[:12], round(x['score'], 3)) for x in r['result']['matches']], r['result']['remote_error'])
r = call(14, 'cmd_sync_cache', {'since': 0}); rows = r['result']; print('  cache rows:', len(rows), {x['state'] for x in rows}, {x['bcc'] for x in rows})
r = call(15, 'cmd_flush_outbox'); print('  flush:', {k: v for k, v in r['result'].items() if v not in (0, None, [])})
r = call(16, 'cmd_sync_cache', {'since': 0}); print('  states after flush:', sorted({x['state'] for x in r['result']}))
p.stdin.close(); p.wait(timeout=30); print('exit', p.returncode)
srv = [json.loads(l) for l in open(LOG)]
print('server got', len(srv), 'saves; vector_len', {s['vector_len'] for s in srv}, 'norm', [round(s['norm'], 4) for s in srv], 'bcc', {s['bcc'] for s in srv})
assert all('시.발' not in s['text'] for s in srv), '욕설이 서버로 갔다'
print('E2E OK')
