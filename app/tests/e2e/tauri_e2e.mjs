// 실제 Tauri 창(WebKitGTK)을 W3C WebDriver(WebKitWebDriver)로 조작하는 E2E. 가상 디스플레이(Xvfb)에서 돌린다.
// 화면 쪽 로직은 ui_e2e.mjs / ui_login_e2e.mjs가 Chromium으로 자세히 확인하므로, 여기서는 "진짜 창에서 Tauri 연결이 되는가"에 집중한다:
// window.__TAURI__, 명령(IPC) 왕복, 실제 모델 임베딩, 로그인 세션 저장과 복원, 시작 실패 안내.
//
//   cd app/ui && npm install && npm run build && cd ../src-tauri && cargo build        # 화면 묶기와 Tauri 빌드
//   Xvfb :99 -screen 0 1280x900x24 &
//   DISPLAY=:99 node tests/e2e/tauri_e2e.mjs target/debug/premium-discount-desktop <EMBEDDINGGEMMA_DIR> <스크린샷 폴더> [A|B]
//   (A: 모델이 없을 때 안내와 창 안에서 내려받기·설치, B: 전체 흐름. 생략하면 둘 다)
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'

const [BIN_ARG, MODEL, OUT, ONLY] = process.argv.slice(2)
const here = (p) => fileURLToPath(new URL(p, import.meta.url))
const BIN = fileURLToPath(new URL(BIN_ARG, `file://${process.cwd()}/`))
const DRIVER_PORT = 4444, SERVER_PORT = 18795
const LOG = '/tmp/pd-tauri-e2e-server.log'
// WebView의 저장소(IndexedDB 등)는 XDG 폴더 아래에 생긴다. 실행마다 비우지 않으면 이전 실행의 Dexie 캐시가 남는다
// 실행 안에서 띄운 문서 포털(xdg-document-portal)이 XDG 폴더 안에 FUSE로 마운트된 채 남을 수 있어
// 폴더를 지우는 대신 시나리오마다 새 폴더를 쓰고, 남은 포털은 끈다
let XDG = '/tmp/pd-tauri-e2e-xdg'
let xdgEnv = {}
function freshXdg() {
	spawnSync('pkill', ['-f', 'xdg-document-portal'])
	XDG = `/tmp/pd-tauri-e2e-xdg-${Date.now()}`
	xdgEnv = { XDG_DATA_HOME: `${XDG}/data`, XDG_CACHE_HOME: `${XDG}/cache`, XDG_CONFIG_HOME: `${XDG}/config` }
}
freshXdg()
mkdirSync(OUT, { recursive: true })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const ELEMENT = 'element-6066-11e4-a52e-4f735466cecf'
let step = 0; const ok = (m) => console.log(`OK ${++step}: ${m}`)

// ---- WebDriver ----
async function startDriver(env) {
	const proc = spawn('dbus-run-session', ['--', 'WebKitWebDriver', '--host=127.0.0.1', `--port=${DRIVER_PORT}`], { env: { ...process.env, ...xdgEnv, ...env, TAURI_WEBVIEW_AUTOMATION: 'true', WEBKIT_DISABLE_COMPOSITING_MODE: '1', WEBKIT_DISABLE_DMABUF_RENDERER: '1' }, stdio: 'ignore', detached: true })
	for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${DRIVER_PORT}/status`); break } catch { await sleep(200) } }
	const call = async (method, path, body) => {
		const res = await fetch(`http://127.0.0.1:${DRIVER_PORT}${path}`, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) })
		const json = await res.json()
		if (json.value?.error) throw new Error(`${method} ${path}: ${json.value.error}: ${json.value.message}`)
		return json.value
	}
	const created = await call('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'wry', 'webkitgtk:browserOptions': { binary: BIN, args: [] } } } })
	const sid = created.sessionId
	const s = (p) => `/session/${sid}${p}`
	const d = {
		exec: (script, args = []) => call('POST', s('/execute/sync'), { script, args }),
		// 비동기 스크립트: 마지막 인자(done)를 부르면 그 값이 결과가 된다
		execAsync: (script, args = []) => call('POST', s('/execute/async'), { script, args }),
		find: async (css) => (await call('POST', s('/element'), { using: 'css selector', value: css }))[ELEMENT],
		click: async (css) => call('POST', s(`/element/${await d.find(css)}/click`), {}),
		async waitFor(fn, label, timeoutMs = 60000) {
			const t0 = Date.now(); let last
			while (Date.now() - t0 < timeoutMs) { try { const v = await fn(); if (v) return v; last = v } catch (e) { last = e.message } await sleep(400) }
			// 시간이 지나도 안 되면 화면 상태를 남겨 원인을 볼 수 있게 한다
			const dump = await d.exec("return JSON.stringify({ acct: document.getElementById('acctLabel')?.textContent, notice: document.getElementById('notice')?.innerText, list: [...document.querySelectorAll('#list li')].map((e) => e.innerText.replace(/\\n/g, ' | ')), net: document.getElementById('net')?.innerText })").catch((e) => `덤프 실패: ${e.message}`)
			console.log('화면 상태:', dump)
			await d.shot('tauri-timeout').catch(() => {})
			throw new Error(`시간 초과: ${label} (마지막 값: ${JSON.stringify(last)})`)
		},
		text: (css) => d.exec('return document.querySelector(arguments[0])?.textContent ?? null', [css]),
		visible: (css) => d.exec('const e = document.querySelector(arguments[0]); return !!e && !e.hidden && e.getClientRects().length > 0', [css]),
		// 진짜 키 입력을 먼저 시도하고, 글자가 틀어지면(한글 합성 등) 값을 넣고 input 이벤트를 보낸다
		async type(css, text) {
			const id = await d.find(css)
			await call('POST', s(`/element/${id}/clear`), {})
			await call('POST', s(`/element/${id}/value`), { text })
			const got = await d.exec('return document.querySelector(arguments[0]).value', [css])
			if (got === text) return 'keys'
			await d.exec("const e = document.querySelector(arguments[0]); e.value = arguments[1]; e.dispatchEvent(new Event('input', { bubbles: true }))", [css, text])
			return 'script'
		},
		async shot(name) {
			await sleep(400)
			await d.exec('scrollTo(0, 0)')
			writeFileSync(`${OUT}/${name}.png`, Buffer.from(await call('GET', s('/screenshot')), 'base64'))
		},
		async close() { try { await call('DELETE', s('')) } catch {} try { process.kill(-proc.pid) } catch {} await sleep(1500) },
	}
	return d
}

// ---- 가짜 서버 ----
let mock = null
const api = (path, init) => fetch(`http://127.0.0.1:${SERVER_PORT}${path}`, init).then((r) => r.json())
const control = (o) => api('/__control', { method: 'POST', body: JSON.stringify(o) })
async function startMock() {
	mock = spawn('python3', [here('mock_server.py'), String(SERVER_PORT), LOG], { stdio: 'ignore' })
	for (let i = 0; i < 50; i++) { try { await control({}); return } catch { await sleep(100) } }
	throw new Error('mock server did not start')
}
const serverLog = () => (existsSync(LOG) ? readFileSync(LOG, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : [])
process.on('exit', () => mock?.kill())

// ---- A: 모델이 없으면 창이 안내와 내려받기를 보여 주고, 진짜 창에서 받아 설치하면 앱이 시작된다 ----
// Hugging Face 자리는 model_server.py (MODEL 폴더의 실제 파일을 Range로 답한다. 해시가 고정이라 진짜 파일이어야 한다)
if (!ONLY || ONLY === 'A') {
	const MODEL_A = '/tmp/pd-tauri-e2e-model', HF_LOG = '/tmp/pd-tauri-e2e-hf.log', MODEL_PORT = 18798
	freshXdg()
	for (const p of [MODEL_A, HF_LOG, '/tmp/pd-tauri-e2e-fail']) rmSync(p, { recursive: true, force: true })
	const hf = spawn('python3', [here('model_server.py'), String(MODEL_PORT), MODEL, HF_LOG], { stdio: 'ignore' })
	for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${MODEL_PORT}/__control`, { method: 'POST', body: '{}' }); break } catch { await sleep(100) } }
	const d = await startDriver({ PD_MODEL_DIR: MODEL_A, PD_DATA_DIR: '/tmp/pd-tauri-e2e-fail', PD_SERVER_URL: `http://127.0.0.1:${SERVER_PORT}`, PD_MODEL_BASE_URL: `http://127.0.0.1:${MODEL_PORT}` })
	try {
		await d.waitFor(() => d.visible('#setup'), '시작 안내', 60000)
		assert.equal(await d.exec('return typeof window.__TAURI__?.core?.invoke'), 'function', 'window.__TAURI__가 주입되었다')
		assert.equal(await d.text('#setupTitle'), '모델이 필요합니다')
		assert.match(await d.text('#setupError'), /토크나이저|No such file/)
		const hints = await d.exec("return [...document.querySelectorAll('#setupHints li')].map((e) => e.textContent)")
		assert.ok(hints.some((h) => h.includes(MODEL_A)), JSON.stringify(hints))
		assert.ok(hints.some((h) => h.includes('PD_MODEL_DIR')))
		assert.equal(await d.visible('#layout'), false, '다른 화면은 숨긴다')
		assert.equal(await d.exec('return document.title'), 'premium.discount')
		await d.waitFor(() => d.visible('#modelFetch'), '내려받기 선택지')
		await d.shot('tauri-01-model-needed')
		ok('모델이 없으면: 창은 뜨고, 원인과 내려받기 선택지를 보여 준다 (not_ready 오류와 cmd_model_status가 화면까지 전달됨)')

		// 진짜 창에서 텍스트 전용을 받는다 → 설치가 끝나면 화면이 다시 시작하고, 셸이 새 모델로 코어를 만든다
		await d.click('#dlChoices button[data-variant="text"]')
		await d.waitFor(() => d.visible('.acct'), '내려받기 → 설치 → 앱 시작', 600000)
		assert.ok(existsSync(`${MODEL_A}/model-text.safetensors`) && existsSync(`${MODEL_A}/tokenizer.json`) && !existsSync(`${MODEL_A}/model.safetensors`))
		const hfLog = readFileSync(HF_LOG, 'utf8').trim().split('\n').map((l) => JSON.parse(l))
		assert.equal(hfLog.filter((r) => r.name === 'model.safetensors' && r.status === 200).length, 0, '원본 전체는 받지 않았다')
		const st = await d.execAsync("const done = arguments[arguments.length - 1]; window.__TAURI__.core.invoke('cmd_model_status').then(done, (e) => done({ error: String(e) }))")
		assert.equal(st.installed, 'text', JSON.stringify(st))
		assert.equal(st.residency?.vision?.state, 'not_installed', JSON.stringify(st))
		await d.shot('tauri-02-installed-ready')
		ok(`진짜 창에서 텍스트 전용 내려받기 → 설치 → 같은 창에서 앱 시작 (cmd_model_status: installed=${st.installed}, 비전 ${st.residency.vision.state}, 상주 ${st.residency.resident_mb}MB)`)
	} finally { await d.close(); hf.kill() }
}

// ---- B: 실제 모델로 전체 흐름 ----
if (!ONLY || ONLY === 'B') {
	const DATA = '/tmp/pd-tauri-e2e-data'
	freshXdg(); rmSync(DATA, { recursive: true, force: true }); rmSync(LOG, { force: true })
	await startMock()
	const env = { PD_MODEL_DIR: MODEL, PD_DATA_DIR: DATA, PD_SERVER_URL: `http://127.0.0.1:${SERVER_PORT}` }
	let d = await startDriver(env)
	try {
		// 디버그 빌드는 모델을 올리는 데 오래 걸린다 (릴리스는 약 3초)
		await d.waitFor(() => d.visible('.acct'), '코어 초기화와 첫 화면', 240000)
		assert.equal(await d.exec('return typeof window.__TAURI__?.core?.invoke'), 'function')
		assert.equal(await d.text('#acctLabel'), '게스트')
		assert.match(await d.text('#author'), /게스트로 등록합니다/)
		assert.equal(await d.visible('#setup'), false)
		assert.ok(existsSync(`${DATA}/evaluations_local.lance`), '로컬 저장소(LanceDB)가 열렸다')
		await d.shot('tauri-02-guest'); ok('진짜 창이 뜨고 IPC로 첫 화면을 그린다: 게스트, 로컬 저장소 열림')

		// 모바일 로그인 준비: Rust가 알려 주는 플랫폼, 그리고 외부 앱 열기 권한은 mailto:만 허용
		const st = await d.execAsync("const done = arguments[arguments.length - 1]; window.__TAURI__.core.invoke('cmd_app_status').then(done, (e) => done({ error: String(e) }))")
		assert.equal(st.platform, 'desktop', JSON.stringify(st))
		const tryOpen = (url) => d.execAsync("const done = arguments[arguments.length - 1]; window.__TAURI__.core.invoke('plugin:opener|open_url', { url: arguments[0] }).then(() => done({ ok: true }), (e) => done({ ok: false, err: String(e) }))", [url])
		const web = await tryOpen('https://example.com/')
		assert.equal(web.ok, false, 'https 링크는 열 수 없어야 한다')
		assert.match(web.err, /not allowed/i, web.err)
		const mail = await tryOpen('mailto:test@example.com?subject=x')
		// 허용된 링크는 권한 오류가 아니다 (이 가상 환경에는 메일 앱이 없어 열기 자체는 실패할 수 있다)
		assert.doesNotMatch(mail.err ?? '', /not allowed/i, mail.err)
		ok(`platform=${st.platform}, 외부 열기: https는 거부(${web.err.slice(0, 40)}…), mailto는 권한 통과(${mail.ok ? '열림' : '열기 자체는 실패: ' + (mail.err ?? '').slice(0, 40)})`)

		// 등록: 진짜 입력 → Rust 명령 → 실제 모델 임베딩 → 서버
		const m1 = await d.type('#cc', 'example.com'); const m2 = await d.type('#text', '배송이 빨라서 만족합니다. 다시 구매할게요.')
		await d.click('#submit')
		await d.waitFor(async () => /서버가 받았습니다/.test((await d.text('#notice')) ?? ''), '등록 결과 안내', 180000)
		const posted = serverLog()
		assert.equal(posted.length, 1)
		assert.equal(posted[0].text, '배송이 빨라서 만족합니다. 다시 구매할게요.')
		assert.equal(posted[0].vector_len, 768)
		assert.ok(Math.abs(posted[0].norm - 1) < 0.01)
		await d.waitFor(() => d.exec("return document.querySelectorAll('#list li.row-item').length === 1"), '목록에 한 건')
		await d.shot('tauri-03-submitted'); ok(`등록: 실제 모델이 768차원 단위 벡터로 만들어 서버가 받음 (입력 방식: 이름 ${m1}, 글 ${m2})`)

		// 욕설은 로컬에서 막힌다
		await d.type('#text', '시.발 이게 뭐야'); await d.click('#submit')
		await d.waitFor(async () => /욕설이 포함되어 등록하지 않았습니다/.test((await d.text('#notice')) ?? ''), '차단 안내')
		assert.equal(serverLog().length, 1, '서버로 가지 않았다')
		ok('욕설 입력은 서버로 가지 않고 막힘')

		// QR 로그인
		await d.click('#loginBtn')
		await d.waitFor(() => d.exec("return !!document.querySelector('#qrBox svg')"), 'QR 표시')
		const address = (await d.text('#mailAddr')).trim()
		assert.match(address, /^[0-9a-f]{40}\.premium\.discount@oauth\.email$/)
		assert.equal(existsSync(`${DATA}/session.json`), false)
		await d.shot('tauri-04-login-qr'); ok(`QR 로그인 창: 주소 ${address.slice(0, 8)}…`)
		await control({ verified: true })
		await d.waitFor(async () => (await d.text('#acctLabel')) === '0x0000…00cc', '로그인 완료', 30000)
		// 로그인이 끝나면 1.2초 뒤에 창이 닫힌다
		await d.waitFor(async () => !(await d.visible('#loginModal')), '로그인 창 닫힘', 10000)
		const saved = JSON.parse(readFileSync(`${DATA}/session.json`, 'utf8'))
		assert.equal(saved.hash, address.split('.')[0])
		assert.equal(statSync(`${DATA}/session.json`).mode & 0o777, 0o600)
		await d.shot('tauri-05-signed-in'); ok('메일 도착 → 로그인, 세션 파일 저장(0600)')

		await d.type('#text', '로그인한 상태로 쓰는 글입니다.'); await d.click('#submit')
		await d.waitFor(() => serverLog().length === 2, '로그인 상태의 등록', 120000)
		assert.equal(serverLog()[1].session_hash, saved.hash)
		assert.equal(serverLog()[1].from, '0x' + '00'.repeat(19) + 'cc')
		ok('로그인한 세션 헤더로 등록되고 서버가 내 계정으로 저장')
	} finally { await d.close() }

	// 앱을 껐다 켜도: 로그인과 목록이 유지된다 (진짜 IndexedDB와 세션 파일)
	d = await startDriver(env)
	try {
		await d.waitFor(async () => (await d.text('#acctLabel')) === '0x0000…00cc', '재시작 뒤 로그인 복원', 240000)
		await d.waitFor(() => d.exec("return document.querySelectorAll('#list li.row-item').length >= 2"), '재시작 뒤 목록')
		await d.shot('tauri-06-restarted'); ok('앱을 다시 켜도 로그인과 내 평가 목록이 유지됨')
		await d.click('#logoutBtn')
		await d.waitFor(async () => (await d.text('#acctLabel')) === '게스트', '로그아웃')
		assert.equal(existsSync(`${DATA}/session.json`), false)
		ok('로그아웃: 저장된 세션 삭제')
	} finally { await d.close() }
	mock.kill()
}
console.log('TAURI E2E OK')
