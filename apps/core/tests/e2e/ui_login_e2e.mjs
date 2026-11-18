// QR 로그인 E2E: 실제 모델이 연결된 실행 파일 + Chromium. 서버만 가짜(mock_server.py)이고, "이메일 도착"은 /__control로 흉내 낸다.
// QR은 브라우저에서 실제로 그린 SVG를 래스터로 바꿔 jsQR로 읽어서, 스캐너가 읽는 내용이 올바른지 확인한다.
//
//   cd app/ui && npm install && npm run build
//   node tests/e2e/ui_login_e2e.mjs target/release/premium-discount-app <EMBEDDINGGEMMA_DIR> <스크린샷 폴더>
import { createRequire } from 'node:module'
import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, rmSync, statSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'

const require = createRequire(import.meta.url)
const { chromium } = require('playwright')
const [BIN_ARG, MODEL, OUT] = process.argv.slice(2)
const here = (p) => fileURLToPath(new URL(p, import.meta.url))
const BIN = fileURLToPath(new URL(BIN_ARG, `file://${process.cwd()}/`))
const JSQR = here('../../ui/node_modules/jsqr/dist/jsQR.js')
const SERVER_PORT = 18792, UI_PORT = 18793
const LOG = '/tmp/pd-login-e2e-server.log', DATA = '/tmp/pd-login-e2e-me'
const SESSION_FILE = `${DATA}/session.json`
for (const p of [LOG, DATA]) rmSync(p, { recursive: true, force: true })
mkdirSync(OUT, { recursive: true })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const api = (path, init) => fetch(`http://127.0.0.1:${SERVER_PORT}${path}`, init).then((r) => r.json())
const control = (o) => api('/__control', { method: 'POST', body: JSON.stringify(o) })
const serverLog = () => (existsSync(LOG) ? readFileSync(LOG, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : [])

const mock = spawn('python3', [here('mock_server.py'), String(SERVER_PORT), LOG], { stdio: 'inherit' })
let ui = null
const startUi = async () => {
	ui = spawn('node', [here('../../ui/dev-server.mjs')], { env: { ...process.env, PD_MODEL_DIR: MODEL, PD_DATA_DIR: DATA, PD_SERVER_URL: `http://127.0.0.1:${SERVER_PORT}`, PD_BIN: BIN, PORT: String(UI_PORT) }, stdio: ['ignore', 'pipe', 'inherit'] })
	await new Promise((r) => ui.stdout.on('data', (d) => String(d).includes('http://') && r()))
}
const stopUi = async () => { ui.kill(); await new Promise((r) => ui.once('exit', r)); ui = null }
process.on('exit', () => { ui?.kill(); mock.kill() })
for (let i = 0; i < 50; i++) { try { await control({}); break } catch { await sleep(100) } }
await startUi()

const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width: 1100, height: 900 }, deviceScaleFactor: 1.5, colorScheme: 'light', permissions: ['clipboard-read', 'clipboard-write'] })
const page = await ctx.newPage()
page.on('pageerror', (e) => { throw e })
const shot = async (name) => { await page.waitForTimeout(300); await page.evaluate(() => scrollTo(0, 0)); await page.screenshot({ path: `${OUT}/${name}.png` }) }
let step = 0; const ok = (m) => console.log(`OK ${++step}: ${m}`)
const text = (sel) => page.textContent(sel)

// 브라우저 안에서 SVG QR을 래스터로 바꿔 jsQR로 읽는다
await page.addInitScript({ path: JSQR })
const decodeQr = () => page.evaluate(async () => {
	const svg = document.querySelector('#qrBox svg')
	const url = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(new XMLSerializer().serializeToString(svg))
	const img = new Image(); img.src = url; await img.decode()
	const c = document.createElement('canvas'); c.width = c.height = 700
	const g = c.getContext('2d'); g.fillStyle = '#fff'; g.fillRect(0, 0, 700, 700); g.drawImage(img, 0, 0, 700, 700)
	const d = g.getImageData(0, 0, 700, 700)
	return window.jsQR(d.data, 700, 700)?.data ?? null
})

await page.goto(`http://127.0.0.1:${UI_PORT}/`)
await page.waitForSelector('.acct:not([hidden])')
assert.equal(await text('#acctLabel'), '게스트')
assert.equal(await page.locator('#loginBtn').isVisible(), true)
assert.equal(await page.locator('#logoutBtn').isVisible(), false)
assert.match(await text('#author'), /게스트로 등록합니다\. 로그인하면 내 계정 주소로 등록됩니다/)
await shot('11-guest'); ok('처음에는 게스트: 로그인 버튼, 등록 주소 안내')

// ---- QR 로그인 시작 ----
await page.click('#loginBtn')
await page.waitForSelector('#qrBox svg')
const address = (await text('#mailAddr')).trim()
assert.match(address, /^[0-9a-f]{40}\.premium\.discount@oauth\.email$/)
const decoded = await decodeQr()
assert.equal(decoded, 'mailto:' + encodeURIComponent(address), '스캐너가 읽은 내용이 메일 주소를 담은 mailto 링크다')
assert.equal(await page.getAttribute('#qrBox', 'data-mailto'), decoded)
await page.waitForFunction(() => /남은 시간 1[45]:\d\d/.test(document.getElementById('loginStatus').textContent))
assert.match(await text('#loginStatus'), /메일을 기다리는 중입니다/)
assert.equal(existsSync(SESSION_FILE), false, '메일이 도착하기 전에는 저장하지 않는다')
await shot('12-login-qr'); ok(`QR 표시, 스캐너(jsQR)가 읽은 값 = mailto 링크, 주소 ${address.slice(0, 8)}…, 15분 카운트다운`)

// 화면 어디에도 token이 없다
const issuedToken = (await api('/__sessions'))[address.split('.')[0]]
assert.match(issuedToken, /^[0-9a-f]{64}$/)
const dom = await page.content()
const stored = await page.evaluate(() => JSON.stringify({ ...localStorage }))
assert.ok(!dom.includes(issuedToken) && !stored.includes(issuedToken), 'token은 화면(DOM, localStorage)에 없다')
ok('token은 화면에 전달되지 않는다')

await page.click('#copyAddr')
await page.waitForFunction(() => document.getElementById('copyAddr').textContent === '복사했습니다')
assert.equal(await page.evaluate(() => navigator.clipboard.readText()), address)
ok('주소 복사')

// ---- 메일 도착 → 로그인 ----
await page.waitForTimeout(3500)
assert.ok((await api('/__stats')).session_polls >= 1, '3초마다 서버에 묻는다')
await control({ verified: true })
await page.waitForFunction(() => document.getElementById('loginStatus').textContent.includes('로그인했습니다'), null, { timeout: 10000 })
await shot('13-login-done')
await page.waitForFunction(() => document.getElementById('loginModal').hidden, null, { timeout: 5000 })
assert.equal(await text('#acctLabel'), '0x0000…00cc')
assert.equal(await page.locator('#logoutBtn').isVisible(), true)
assert.equal(await page.locator('#loginBtn').isVisible(), false)
assert.equal(await text('#author'), '등록 주소: 0x0000…00cc')
await shot('14-signed-in')
const saved = JSON.parse(readFileSync(SESSION_FILE, 'utf8'))
assert.equal(saved.hash, address.split('.')[0])
assert.equal(statSync(SESSION_FILE).mode & 0o777, 0o600, '세션 파일은 소유자만 읽는다')
ok('메일 도착 → 로그인, 창이 닫히고 계정 표시, 세션 파일 저장(0600)')

// ---- 로그인한 계정으로 등록 ----
const submit = async (t) => { await page.fill('#cc', 'example.com'); await page.fill('#text', t); await page.click('#submit'); await page.waitForSelector('#notice[data-tone="busy"]') }
await submit('로그인한 상태로 쓰는 글입니다.')
const asUser = serverLog().at(-1)
assert.equal(asUser.session_hash, saved.hash, '등록 요청이 발급받은 세션으로 나갔다')
assert.equal(asUser.from, '0x' + '00'.repeat(19) + 'cc', '서버가 세션으로 알아본 계정이 작성자다')
ok('등록 요청이 로그인한 세션 헤더로 나가고 서버가 내 계정으로 저장')

// ---- 앱을 껐다 켜도 로그인 유지 ----
await stopUi(); await startUi()
await page.reload()
await page.waitForFunction(() => document.getElementById('acctLabel').textContent === '0x0000…00cc')
assert.equal(await page.locator('#logoutBtn').isVisible(), true)
ok('앱을 다시 켜도 로그인 유지 (저장된 세션 복원, 서버 확인)')

// ---- 로그아웃 ----
await page.click('#logoutBtn')
await page.waitForFunction(() => document.getElementById('acctLabel').textContent === '게스트')
assert.equal(existsSync(SESSION_FILE), false, '로그아웃하면 저장된 세션을 지운다')
await submit('로그아웃한 뒤의 글입니다.')
const asGuest = serverLog().at(-1)
assert.match(asGuest.session_hash, /^0+$/, '익명 자리값으로 나간다')
assert.equal(asGuest.from, '0x' + '00'.repeat(19) + 'ab', '게스트 주소로 저장')
ok('로그아웃: 파일 삭제, 이후 등록은 게스트로')

// ---- 오류와 다시 시도 ----
await control({ login_error: true })
await page.click('#loginBtn')
await page.waitForFunction(() => document.getElementById('loginStatus').textContent.includes('로그인을 이어갈 수 없습니다'))
assert.match(await text('#loginStatus'), /로그인이 아직 설정되지 않았습니다/)
assert.equal(await page.locator('#loginRetry').isVisible(), true)
assert.match(await text('#qrBox'), /QR을 만들지 못했습니다/)
await shot('15-login-error')
await control({ login_error: false })
await page.click('#loginRetry')
await page.waitForSelector('#qrBox svg')
const retryAddress = (await text('#mailAddr')).trim()
assert.notEqual(retryAddress, address, '새 QR은 새 세션이다')
ok('서버가 로그인을 설정하지 않았으면 안내하고, 복구 뒤 "새 QR 만들기"로 이어간다')

// ---- 닫으면 서버에 묻지 않는다 ----
await page.keyboard.press('Escape')
assert.equal(await page.locator('#loginModal').isHidden(), true)
const before = (await api('/__stats')).session_polls
await page.waitForTimeout(7000)
assert.ok((await api('/__stats')).session_polls - before <= 1, '창을 닫은 뒤에는 폴링이 멈춘다')
ok('Esc로 닫으면 폴링이 멈춘다')

// ---- 데스크톱에서도 이 기기의 메일 앱으로 열 수 있는 링크가 보조로 있다 ----
await page.click('#loginBtn'); await page.waitForSelector('#qrBox svg')
assert.equal((await text('#mailLink')).trim(), '이 기기의 메일 앱으로 열기')
const deskAddress = (await text('#mailAddr')).trim()
const deskHref = await page.getAttribute('#mailLink', 'href')
assert.ok(deskHref.startsWith(`mailto:${deskAddress}?`), deskHref)
assert.equal(await page.locator('#qrBox').isVisible(), true, '데스크톱은 QR이 중심이다')
await page.keyboard.press('Escape')
ok('데스크톱: QR이 중심이고 메일 링크는 보조로 있다')

// ---- 모바일: QR 대신 링크를 눌러 메일 앱으로 간다 ----
await control({ verified: false })
const mobile = await browser.newContext({ viewport: { width: 390, height: 780 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, colorScheme: 'light' })
// Tauri의 opener 플러그인을 흉내 낸다: 호출된 주소만 기록한다 (진짜 Tauri 창에서는 tauri_e2e.mjs가 권한 범위를 확인한다)
await mobile.addInitScript(() => { window.__TAURI__ = { opener: { openUrl: (u) => { (window.__opened ||= []).push(u); return Promise.resolve() } } } })
const mp = await mobile.newPage()
mp.on('pageerror', (e) => { throw e })
await mp.goto(`http://127.0.0.1:${UI_PORT}/?platform=android`)
await mp.waitForSelector('.acct:not([hidden])')
await mp.click('#loginBtn')
await mp.waitForFunction(() => document.getElementById('mailAddr').textContent.length > 0)
const mAddress = (await mp.textContent('#mailAddr')).trim()
assert.equal((await mp.textContent('#mailLink')).trim(), '메일 앱으로 로그인')
assert.equal(await mp.locator('#mailLink').isVisible(), true)
assert.equal(await mp.locator('#qrBox').isVisible(), false, '모바일에서는 QR을 보여 주지 않는다 (자기 화면의 QR은 스캔할 수 없다)')
assert.equal(await mp.locator('#qrBox svg').count(), 0, 'QR을 그리지도 않는다')
assert.match(await mp.textContent('.modal-card'), /위 버튼을 누르면 메일 앱이 열립니다/)
assert.equal(await mp.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
await mp.waitForTimeout(300); await mp.screenshot({ path: `${OUT}/18-mobile-link.png` })
// 링크를 누르면 메일 앱을 여는 호출이 한 번 나간다. 받는 사람·제목·본문이 채워져 있다
await mp.click('#mailLink')
const opened = await mp.evaluate(() => window.__opened)
assert.equal(opened.length, 1)
const u = new URL(opened[0])
assert.equal(u.protocol, 'mailto:')
assert.equal(decodeURIComponent(u.pathname), mAddress, '받는 사람이 로그인 주소로 채워져 있다')
assert.equal(u.searchParams.get('subject'), 'premium.discount 로그인')
assert.match(u.searchParams.get('body'), /그대로 보내면 로그인됩니다/)
ok('모바일: QR 대신 링크 버튼, 누르면 받는 사람·제목·본문이 채워진 메일 앱 호출')

// 메일 앱에서 돌아오면(앱이 다시 보이면) 3초 타이머를 기다리지 않고 바로 확인한다
const polls0 = (await api('/__stats')).session_polls
for (let i = 0; i < 3; i++) { await mp.evaluate(() => document.dispatchEvent(new Event('visibilitychange'))); await mp.waitForTimeout(700) }
const extra = (await api('/__stats')).session_polls - polls0
assert.ok(extra >= 3, `타이머(3초)만으로는 2.1초 동안 최대 1번이다. 실제 ${extra}번`)
ok(`앱이 다시 보이면 즉시 확인한다 (2.1초 동안 ${extra}번)`)

// 메일이 도착하면 로그인된다
await control({ verified: true })
await mp.waitForFunction(() => document.getElementById('acctLabel').textContent === '0x0000…00cc', null, { timeout: 10000 })
await mp.waitForFunction(() => document.getElementById('loginModal').hidden, null, { timeout: 5000 })
await mp.waitForTimeout(300); await mp.screenshot({ path: `${OUT}/19-mobile-signed-in.png` })
ok('모바일: 메일 도착 → 로그인')
await mobile.close()

// ---- 다크 테마와 좁은 화면 ----
await page.emulateMedia({ colorScheme: 'dark' })
await page.click('#loginBtn'); await page.waitForSelector('#qrBox svg')
assert.equal(await decodeQr(), 'mailto:' + encodeURIComponent((await text('#mailAddr')).trim()), '다크 테마에서도 QR은 읽힌다')
await shot('16-login-dark')
await page.setViewportSize({ width: 390, height: 780 })
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, '좁은 화면에서 가로로 넘치지 않는다')
await shot('17-login-phone')
ok('다크 테마에서도 QR이 읽히고, 좁은 화면(390px)에서 넘치지 않는다')

await browser.close(); await stopUi(); mock.kill()
console.log('UI LOGIN E2E OK')
