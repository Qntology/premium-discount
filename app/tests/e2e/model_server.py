"""가짜 Hugging Face: /<owner>/<repo>/resolve/<rev>/<file>을 실제 모델 폴더의 파일로 답한다 (Range 지원).

앱의 모델 내려받기(app/src/model_fetch.rs)를 진짜 바이트로 시험하려고 쓴다. 해시가 고정이라 진짜 파일이어야 한다.
요청마다 한 줄씩 로그(JSON)를 남긴다: path, range, status, sent.
속도를 제한할 수 있다 (POST /__control {"rate": 초당 바이트}). 화면에서 진행률과 멈추기를 볼 수 있게.

  python3 model_server.py <port> <모델 폴더> <로그 파일> [<owner/repo>=<폴더> ...]
  (저장소별 폴더를 주면 그 저장소의 요청은 그 폴더에서 답한다. 하위 폴더 경로도 된다: tokenizer/tokenizer.json)
"""
import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT, ROOT, LOG = int(sys.argv[1]), sys.argv[2], sys.argv[3]
REPOS = dict(a.split("=", 1) for a in sys.argv[4:])
state = {"rate": 0}
lock = threading.Lock()


def log(entry):
    with lock, open(LOG, "a") as f:
        f.write(json.dumps(entry) + "\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        if self.path == "/__control":
            body = self.rfile.read(int(self.headers.get("content-length", 0)) or 0)
            state.update(json.loads(body or b"{}"))
            out = json.dumps(state).encode()
            self.send_response(200)
            self.send_header("content-length", str(len(out)))
            self.end_headers()
            self.wfile.write(out)
            return
        self.send_error(404)

    def do_GET(self):
        m = re.match(r"^/([^/]+/[^/]+)/resolve/[^/]+/(.+)$", self.path)
        if m and ".." in m.group(2).split("/"):
            m = None
        path = os.path.join(REPOS.get(m.group(1), ROOT), m.group(2)) if m else None
        if not path or not os.path.isfile(path):
            self.send_response(404)
            self.send_header("content-length", "0")
            self.end_headers()
            log({"path": self.path, "status": 404})
            return
        size = os.path.getsize(path)
        rng = self.headers.get("range")
        start, end, status = 0, size - 1, 200
        if rng:
            a, b = re.match(r"bytes=(\d+)-(\d*)", rng).groups()
            start, end, status = int(a), min(int(b) if b else size - 1, size - 1), 206
        length = end - start + 1
        self.send_response(status)
        self.send_header("content-length", str(length))
        if status == 206:
            self.send_header("content-range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        sent = 0
        try:
            with open(path, "rb") as f:
                f.seek(start)
                while sent < length:
                    chunk = f.read(min(256 * 1024, length - sent))
                    self.wfile.write(chunk)
                    sent += len(chunk)
                    if state["rate"]:
                        time.sleep(len(chunk) / state["rate"])
        except (BrokenPipeError, ConnectionResetError):
            pass
        log({"path": self.path, "repo": m.group(1), "name": m.group(2), "range": rng, "start": start, "status": status, "sent": sent})


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
