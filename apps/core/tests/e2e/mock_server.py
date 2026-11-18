# 실모델 E2E 검증용 가짜 서버 (app/tests/e2e/e2e_main.py가 쓴다). 사용: python3 mock_server.py <포트> <요청 기록 파일>
# www의 /evaluations 계약(검증 규칙, 멱등 request_id, 202 pending)을 흉내 내는 로컬 가짜 서버. 받은 요청을 파일에 남긴다.
import json, hashlib, math, sys, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[2]
STATE = sys.argv[3] if len(sys.argv) > 3 else None  # 있으면 저장된 글을 이 파일에 남겨 재시작해도 유지한다
store = {}  # request_id -> record
import secrets, urllib.parse, time, os
# 판정은 계약의 참조 구현(contracts/gen_verdict_v1.py)으로 계산한다. 서버 cron과 같은 규칙이다
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', '..', 'contracts'))
import gen_verdict_v1 as verdict_ref
votes = []  # 추가만 하는 투표 기록 (www의 eval_votes와 같다). (평가, 투표자, 종류)마다 마지막 행이 유효하다
sessions = {}  # hash -> token (POST /session/start가 발급한 것)
STATS = {'session_polls': 0}
USER_ADDR = '0x' + '00'*19 + 'cc'  # 로그인한 사용자의 주소 (실제로는 이메일에서 계산한다)
MODE = {'verified': False, 'login_error': False, 'done': True}  # False면 status가 pending (cron이 아직 처리하지 않은 상태). POST /__control {"done": bool}로 바꾼다
REJECT_MARK = '[검수거부시뮬레이션]'  # 이 표지가 든 글은 처리가 끝나면 rejected(음란·불법)로 답한다. 실제 판정은 cron의 결정 모델이 한다

def cos(a, b): return sum(x*y for x, y in zip(a, b))

import os
if STATE and os.path.exists(STATE): store.update(json.load(open(STATE)))
def persist():
    if STATE: json.dump(store, open(STATE, 'w'))

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code); self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def _body(self):
        n = int(self.headers.get('Content-Length', 0)); return json.loads(self.rfile.read(n) or b'{}')
    def _signed_in(self):
        sh = (self.headers.get('x-oauth-hash') or '').lower().removeprefix('0x')
        return bool(sh) and sessions.get(sh) == (self.headers.get('x-oauth-token') or '').lower().removeprefix('0x') and MODE['verified']

    def do_POST(self):
        b = self._body()
        if self.path == '/evaluations/votes':
            rid, eid, value, kind = b.get('request_id'), str(b.get('id', '')).lower(), b.get('value'), b.get('kind') or 'vote'
            if value not in (1, -1, 0) or kind not in ('vote', 'vote_outcome') or not rid:
                return self._send(400, {'error': 'bad vote'})
            author = next((r['from'] for r in store.values() if r['id'] == eid), None)
            if author is None: return self._send(404, {'error': 'evaluation not found'})
            voter = USER_ADDR if self._signed_in() else '0x' + '00'*19 + 'ab'
            counted = self._signed_in() and voter != author
            body = {'id': eid, 'value': value, 'kind': kind, 'counted': counted, 'status': 'accepted'}
            old = next((v for v in votes if v['request_id'] == rid), None)
            if old:
                if (old['eval_id'], old['value'], old['kind']) != (eid, value, kind): return self._send(409, {'error': 'request_id reused'})
                return self._send(200, body)
            votes.append({'request_id': rid, 'eval_id': eid, 'voter': voter, 'counted': counted, 'kind': kind, 'value': value, 'created_at': int(time.time() * 1000)})
            with open(LOG, 'a') as f: f.write(json.dumps({'path': self.path, 'request_id': rid, 'id': eid, 'value': value, 'kind': kind, 'counted': counted}) + '\n')
            return self._send(202, body)
        if self.path == '/__control':
            MODE.update({k: bool(v) for k, v in b.items() if k in MODE}); return self._send(200, MODE)
        if self.path == '/session/start':
            if MODE['login_error']: return self._send(503, {'error': 'login is not configured'})
            h, tok = secrets.token_hex(20), secrets.token_hex(32)
            sessions[h] = tok
            address = f'{h}.premium.discount@oauth.email'
            return self._send(201, {'hash': h, 'token': tok, 'address': address, 'mailto': 'mailto:' + urllib.parse.quote(address, safe='')})
        if self.path == '/evaluations':
            v = b.get('vector', [])
            errs = []
            if len(v) != 768: errs.append('vector length %d' % len(v))
            elif abs(math.sqrt(sum(x*x for x in v)) - 1) > 0.01: errs.append('not normalized')
            if hashlib.sha256(b.get('text', '').encode()).hexdigest() != b.get('text_hash'): errs.append('hash mismatch')
            if errs: return self._send(400, {'error': '; '.join(errs)})
            rid = b['request_id']
            if rid in store:
                r = store[rid]; return self._send(200, {k: r[k] for k in ('id','root','ref','from')} | {'status': 'stored'})
            sh = (self.headers.get('x-oauth-hash') or '').lower().removeprefix('0x')
            signed_in = sessions.get(sh) == (self.headers.get('x-oauth-token') or '').lower().removeprefix('0x') and MODE['verified']
            _id = str(uuid.uuid4()); store[rid] = {'id': _id, 'root': _id, 'ref': b.get('ref'), 'from': USER_ADDR if signed_in else '0x' + '00'*19 + 'ab', 'vector': v, 'cc': b['cc'], 'bcc': b.get('bcc'), 'text': b['text']}
            persist()
            with open(LOG, 'a') as f: f.write(json.dumps({'path': self.path, 'request_id': rid, 'cc': b['cc'], 'bcc': b.get('bcc'), 'ref': b.get('ref'), 'id': _id, 'session_hash': sh, 'from': store[rid]['from'], 'vector_len': len(v), 'norm': math.sqrt(sum(x*x for x in v)), 'text': b['text']}, ensure_ascii=False) + '\n')
            return self._send(202, {k: store[rid][k] for k in ('id','root','ref','from')} | {'status': 'pending'})
        if self.path == '/evaluations/search':
            q = b['vector']
            ms = sorted(({'id': r['id'], 'score': cos(q, r['vector']), 'cc': r['cc'], 'root': r['root'], 'ref': r['ref'], 'to': None} for r in store.values()), key=lambda m: -m['score'])
            for m in ms:
                v = verdict_of(m['id'])
                m['verdict'] = {k: v[k] for k in ('tier', 'grade', 'rank', 'evidence')}
            tier = b.get('tier')
            if tier: ms = [m for m in ms if (m['verdict']['tier'] == tier) or (tier == 'rated' and m['verdict']['tier'] != 'unrated')]
            return self._send(200, {'matches': ms[:b.get('topK', 10)]})
        self._send(404, {'error': 'not found'})
    def do_GET(self):
        if self.path.startswith('/evaluations/verdicts'):
            ids = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get('ids', [''])[0].split(',')
            ids = list(dict.fromkeys(i.lower() for i in ids if i))
            if not 1 <= len(ids) <= 50: return self._send(400, {'error': 'ids must hold 1 to 50 ids'})
            return self._send(200, {'verdicts': [verdict_of(i) for i in ids]})
        if self.path == '/__stats': return self._send(200, STATS)
        if self.path == '/__sessions': return self._send(200, sessions)
        if self.path == '/session':
            STATS['session_polls'] += 1
            h = (self.headers.get('x-oauth-hash') or '').lower().removeprefix('0x')
            tok = (self.headers.get('x-oauth-token') or '').lower().removeprefix('0x')
            if sessions.get(h) != tok or not h: return self._send(200, {'state': 'anonymous', 'address': None})
            return self._send(200, {'state': 'signed_in', 'address': USER_ADDR} if MODE['verified'] else {'state': 'pending', 'address': None})
        if self.path.startswith('/evaluations/status'):
            rid = self.path.split('request_id=')[1]
            r = store.get(rid)
            if not r: return self._send(404, {'error': 'not found'})
            if not MODE['done']: return self._send(200, {'id': r['id'], 'root': r['root'], 'ref': r['ref'], 'status': 'pending', 'reason': None})
            if REJECT_MARK in r['text']: return self._send(200, {'id': r['id'], 'root': r['root'], 'ref': r['ref'], 'status': 'rejected', 'reason': 'nsfw,illegal'})
            return self._send(200, {'id': r['id'], 'root': r['root'], 'ref': r['ref'], 'status': 'stored', 'reason': None})
        self._send(404, {'error': 'not found'})

def verdict_of(eval_id):
    # 서버 cron과 같은 관측: 평가·투표자·종류마다 마지막 표, 세지 않는 표(게스트·자기 글)는 가중치 0
    latest = {}
    for v in votes:
        if v['eval_id'] == eval_id: latest[(v['voter'], v['kind'])] = v
    now = int(time.time() * 1000)
    obs = [{'kind': v['kind'], 'sign': v['value'], 'weight': verdict_ref.PARAMS['weights'][v['kind']] if v['counted'] else 0.0, 'credit': 1.0, 'at_ms': v['created_at']} for v in latest.values()]
    # 가짜 서버는 스코프 기준선을 두지 않는다 (판정은 근거량과 대결 없는 경로만 탄다)
    out = verdict_ref.verdict({'now_ms': now, 'track': 'pd', 'prev': {'tier': 'unrated', 'since_ms': now}, 'obs': obs, 'baseline': {}})
    up = sum(1 for v in latest.values() if v['counted'] and v['value'] == 1)
    down = sum(1 for v in latest.values() if v['counted'] and v['value'] == -1)
    return {'id': eval_id, 'tier': out['tier'], 'grade': out['grade'], 'rank': out['rank'], 'value': out['value'], 'evidence': out['evidence'],
            'up': up, 'down': down, 'level': out['level'], 'reasons': out['reasons'], 'recipe': verdict_ref.RECIPE, 'updated_at': now if latest else None}

ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
