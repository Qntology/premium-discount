// 개발용 브리지: dist/를 서빙하고, 브라우저의 POST /invoke를 실제 실행 파일(src/main.rs)의 표준 입출력으로 전달한다.
// Tauri 웹뷰에서는 필요 없다 (Tauri의 invoke가 같은 명령을 부른다). 개발과 검증 전용이다: 127.0.0.1에서만 받는다.
//
//   PD_MODEL_DIR=… PD_DATA_DIR=… PD_SERVER_URL=… node dev-server.mjs        (환경 변수는 실행 파일의 --help 참고)
//   PD_BIN       실행 파일 경로 (기본 ../../target/release/premium-discount-app, 루트 target/)
//   PORT         기본 5173
import { createServer } from 'node:http'
import { spawn } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'
import { extname, join, normalize } from 'node:path'

const here = (p) => fileURLToPath(new URL(p, import.meta.url))
const EXE = process.platform === 'win32' ? '.exe' : ''
const BIN = process.env.PD_BIN ?? here(`../../target/release/premium-discount-app${EXE}`)
const PORT = Number(process.env.PORT ?? 5173)
const DIST = here('dist')
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.map': 'application/json' }

const child = spawn(BIN, [], { stdio: ['pipe', 'pipe', 'inherit'], env: process.env })
child.on('exit', (code) => {
	console.error(`app exited with ${code}`)
	process.exit(code ?? 1)
})

// 요청은 차례로 하나씩 보낸다 (실행 파일은 한 줄 요청에 한 줄로 답한다)
const lines = createInterface({ input: child.stdout })
const waiting = []
lines.on('line', (line) => waiting.shift()?.(line))
let chain = Promise.resolve()
function call(cmd, args) {
	const run = () =>
		new Promise((resolve) => {
			waiting.push(resolve)
			child.stdin.write(JSON.stringify({ id: 1, cmd, args }) + '\n')
		})
	const result = chain.then(run)
	chain = result.catch(() => {})
	return result
}

function sameHost(req) {
	return /^(127\.0\.0\.1|localhost)(:\d+)?$/.test(req.headers.host ?? '')
}

const server = createServer(async (req, res) => {
	if (!sameHost(req)) return void res.writeHead(403).end()
	if (req.method === 'POST' && req.url === '/invoke') {
		// 표지 헤더가 없으면 거부한다: 다른 사이트의 폼이나 단순 요청이 명령을 부르지 못하게
		if (req.headers['x-pd-ui'] !== '1') return void res.writeHead(403).end()
		let raw = ''
		for await (const chunk of req) raw += chunk
		let reply
		try {
			const { cmd, args } = JSON.parse(raw)
			reply = await call(String(cmd), args ?? null)
		} catch (e) {
			reply = JSON.stringify({ ok: false, error: { kind: 'invalid_input', message: `bad bridge request: ${e.message}` } })
		}
		return void res.writeHead(200, { 'content-type': 'application/json' }).end(reply)
	}
	if (req.method !== 'GET') return void res.writeHead(405).end()
	const path = new URL(req.url, 'http://x').pathname
	const file = normalize(join(DIST, path === '/' ? 'index.html' : path))
	if (!file.startsWith(DIST)) return void res.writeHead(403).end()
	try {
		const body = await readFile(file)
		res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store' }).end(body)
	} catch {
		res.writeHead(404).end('not found')
	}
})
server.listen(PORT, '127.0.0.1', () => console.log(`ui: http://127.0.0.1:${PORT}`))
