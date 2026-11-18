// 앱 화면을 실제 Chromium으로 쓰는 E2E: 입력 폼에 글을 쓰고 등록 → 상태 변화 → 검색 → 오프라인 → 복구까지.
// 실제 모델(EmbeddingGemma)이 연결된 실행 파일을 dev-server.mjs 브리지로 붙이고, 서버만 가짜(mock_server.py)다.
// 다른 사용자의 글은 두 번째 앱 인스턴스(다른 데이터 폴더)로 같은 가짜 서버에 올려서 만든다.
//
//   cd web/desktop && npm install && npm run build                       # 화면 묶기
//   cargo build --release -p premium-discount-app --bin premium-discount-app      # 실행 파일 (premium-discount 폴더에서, 루트 target/)
//   node tests/e2e/ui_e2e.mjs ../../target/release/premium-discount-app <EMBEDDINGGEMMA_DIR> <스크린샷 폴더>
//   (playwright가 전역으로 설치되어 있어야 한다: require('playwright'))
import { createRequire } from 'node:module'
import { spawn } from 'node:child_process'
import { mkdirSync, readFileSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'

const require = createRequire(import.meta.url)
const { chromium } = require('playwright')
const [BIN_ARG, MODEL, OUT] = process.argv.slice(2)
const here = (p) => fileURLToPath(new URL(p, import.meta.url))
const BIN = fileURLToPath(new URL(BIN_ARG, `file://${process.cwd()}/`))
const SERVER_PORT = 18790, UI_PORT = 18791
const LOG = '/tmp/pd-ui-e2e-server.log', STATE = '/tmp/pd-ui-e2e-server-state.json', DATA_ME = '/tmp/pd-ui-e2e-me', DATA_OTHER = '/tmp/pd-ui-e2e-other'
for (const p of [LOG, STATE, DATA_ME, DATA_OTHER]) rmSync(p, { recursive: true, force: true })
mkdirSync(OUT, { recursive: true })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ---- 가짜 서버 ----
let mock = null
async function startMock() {
	mock = spawn('python3', [here('mock_server.py'), String(SERVER_PORT), LOG, STATE], { stdio: 'inherit' })
	for (let i = 0; i < 50; i++) { try { await fetch(`http://127.0.0.1:${SERVER_PORT}/__control`, { method: 'POST', body: '{}' }); return } catch { await sleep(100) } }
	throw new Error('mock server did not start')
}
const stopMock = async () => { mock.kill('SIGTERM'); await new Promise((r) => mock.once('exit', r)); mock = null }
const control = (done) => fetch(`http://127.0.0.1:${SERVER_PORT}/__control`, { method: 'POST', body: JSON.stringify({ done }) })
const serverLog = () => readFileSync(LOG, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l))

const appEnv = (data) => ({ ...process.env, PD_MODEL_DIR: MODEL, PD_DATA_DIR: data, PD_SERVER_URL: `http://127.0.0.1:${SERVER_PORT}` })

// ---- 다른 사용자: 같은 서버에 글을 올린다 (두 번째 앱 인스턴스) ----
async function seedOtherUser(texts) {
	const app = spawn(BIN, [], { env: appEnv(DATA_OTHER), stdio: ['pipe', 'pipe', 'pipe'] })
	let buf = ''; const waiters = []
	app.stdout.on('data', (d) => { buf += d; let i; while ((i = buf.indexOf('\n')) >= 0) { waiters.shift()?.(buf.slice(0, i)); buf = buf.slice(i + 1) } })
	await new Promise((r) => app.stderr.once('data', r)) // ready
	for (const text of texts) {
		const reply = new Promise((r) => waiters.push(r))
		app.stdin.write(JSON.stringify({ id: 1, cmd: 'cmd_submit_evaluation', args: { text, cc: 'example.com' } }) + '\n')
		assert.equal(JSON.parse(await reply).result.status, 'accepted')
	}
	app.stdin.end(); await new Promise((r) => app.once('exit', r))
}

// ---- 화면 앱 (브리지) ----
const ui = spawn('node', [fileURLToPath(new URL('../../../../web/desktop/dev-server.mjs', import.meta.url))], { env: { ...appEnv(DATA_ME), PD_BIN: BIN, PORT: String(UI_PORT) }, stdio: ['ignore', 'pipe', 'inherit'] })
// 어떤 이유로 끝나든(검증 실패 포함) 자식 프로세스를 남기지 않는다
process.on('exit', () => { ui.kill(); mock?.kill() })
await new Promise((r) => ui.stdout.on('data', (d) => String(d).includes('http://') && r()))
await startMock()
await seedOtherUser(['배터리 지속시간이 길고 화면이 선명해서 좋아요', '고객센터 응대가 느려서 아쉬웠습니다', '포장이 꼼꼼해서 안전하게 도착했어요'])
console.log('seeded other user: 3 posts')

const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width: 1100, height: 1000 }, deviceScaleFactor: 1.5, colorScheme: 'light' })
const page = await ctx.newPage()
page.on('pageerror', (e) => { throw e })
// 캡처: 안내 막대의 페이드인(0.18초)이 끝난 뒤, 맨 위에서 찍는다 (폼과 목록이 함께 보이도록)
const shot = async (name) => { await page.waitForTimeout(300); await page.evaluate(() => scrollTo(0, 0)); await page.screenshot({ path: `${OUT}/${name}.png` }) }
const rowTexts = () => page.locator('#list li.row-item').evaluateAll((els) => els.map((e) => ({ state: e.dataset.state, text: e.querySelector('.body')?.textContent, pill: e.querySelector('.pill')?.textContent })))
const submit = async (cc, text) => { await page.fill('#cc', cc); await page.fill('#text', text); await page.click('#submit') }
let step = 0; const ok = (m) => console.log(`OK ${++step}: ${m}`)

await page.goto(`http://127.0.0.1:${UI_PORT}/`)
await page.waitForSelector('#list .empty')
await shot('01-empty'); ok('빈 상태: 첫 평가 안내')

// 입력 오류: 도메인 형식, 너무 긴 글 (Rust가 돌려준 오류가 한국어 안내로 바뀐다)
await submit('localhost', '도메인이 틀린 글')
await page.waitForSelector('#notice[data-tone="bad"]')
assert.match(await page.textContent('#notice'), /도메인 형식이 올바르지 않습니다/)
assert.equal(await page.inputValue('#text'), '도메인이 틀린 글', '오류일 때 입력한 글을 지우지 않는다')
await shot('02-error-domain'); ok('도메인 오류 안내, 글 유지')
await page.fill('#text', '가'.repeat(3000))
assert.match(await page.textContent('#bytes'), /9,000 \/ 8,192/)
await page.fill('#cc', 'example.com'); await page.click('#submit')
await page.waitForFunction(() => document.querySelector('#notice')?.textContent.includes('너무 깁니다'))
ok('9,000바이트 입력 → 바이트 카운터와 "너무 깁니다" 안내')

// 욕설: 로컬에서 막힌다 (서버로 가지 않고 이 기기에도 남지 않는다)
const before = serverLog().length
await submit('example.com', '시.발 이게 뭐야')
await page.waitForSelector('#notice[data-tone="warn"]')
assert.match(await page.textContent('#notice'), /욕설이 포함되어 등록하지 않았습니다/)
assert.equal(await page.inputValue('#text'), '시.발 이게 뭐야', '고쳐 쓸 수 있게 글을 남긴다')
assert.equal((await rowTexts()).length, 0, '목록에 남지 않는다')
assert.equal(serverLog().length, before, '서버로 가지 않았다')
await shot('03-blocked'); ok('욕설 차단: 서버 전송 없음, 목록 없음')

// 정상 등록 ×3: 서버가 접수만 한 상태(검수 중)
await control(false)
const mine = ['배송이 빨라서 만족합니다. 다시 구매할게요.', 'The battery life is excellent and the screen is sharp.', '가격 대비 성능이 훌륭하다']
for (const t of mine) { await submit('example.com', t); await page.waitForFunction((n) => document.querySelectorAll('#list li.row-item').length >= n, mine.indexOf(t) + 1) }
await page.waitForSelector('#notice[data-tone="busy"]')
assert.match(await page.textContent('#notice'), /서버가 받았습니다\. 검수 중입니다/)
assert.equal(await page.inputValue('#text'), '', '등록되면 입력칸을 비운다')
let rows = await rowTexts()
assert.deepEqual(rows.map((r) => r.pill), ['검수 중', '검수 중', '검수 중'])
assert.equal(rows[2].text, mine[0], '최신 글이 위에 오므로 첫 글이 맨 아래')
assert.match(await page.textContent('#net'), /서버 연결됨/)
await shot('04-submitted'); ok('3건 등록 → 검수 중 3건, 서버 연결됨')

// 서버(cron)가 검수를 마치면 "지금 확인"으로 등록됨이 된다. 거부 사례는 가짜 서버가 시뮬레이션한다
await submit('example.com', '[검수거부시뮬레이션] 문제 있는 글')
await page.waitForFunction(() => document.querySelectorAll('#list li.row-item').length >= 4)
await control(true)
await page.click('#recheck')
await page.waitForFunction(() => [...document.querySelectorAll('#list li.row-item .pill')].every((p) => p.textContent !== '검수 중'))
rows = await rowTexts()
assert.deepEqual(rows.map((r) => r.pill), ['거부됨', '등록됨', '등록됨', '등록됨'])
assert.match(await page.textContent('#list li[data-state="rejected"]'), /사유: 음란·폭력, 불법·개인정보/)
await shot('05-checked'); ok('지금 확인 → 등록됨 3건, 거부됨 1건(사유 표시)')

// 답글: 서버가 받은 글에 단다 (서버 id가 ref로 간다)
await page.locator('#list li[data-state="stored"]').last().getByRole('button', { name: '답글' }).click()
assert.equal(await page.inputValue('#cc'), 'example.com')
assert.equal(await page.locator('#cc').evaluate((e) => e.readOnly), true, '답글은 부모와 같은 도메인')
await page.fill('#text', '저도 같은 생각이에요. 포장도 좋았습니다.')
await shot('06-reply')
await page.click('#submit')
await page.waitForFunction(() => document.querySelectorAll('#list li.row-item').length >= 5)
const posts = serverLog()
const parent = posts.find((p) => p.text === mine[0]), child = posts.find((p) => p.text.startsWith('저도 같은'))
assert.equal(child.ref, parent.id, '서버가 받은 ref가 부모 글의 서버 id와 같다')
assert.equal(await page.locator('#replyTo').isHidden(), true)
ok('답글 → 서버에 ref=부모 id로 전달, 답글 모드 해제')

// 검색: 한국어 질의로 영어 글과 다른 사용자의 글을 함께 찾는다
await page.fill('#q', '배터리가 오래가요'); await page.click('#searchForm button')
await page.waitForSelector('#results li .pill')
const found = await page.locator('#results li').evaluateAll((els) => els.map((e) => ({ text: e.textContent, mine: e.classList.contains('mine') })))
// 1위가 다른 사용자의 배터리 글이면 원문이 없어서 글자로는 확인할 수 없다. 대신 순위로 확인한다:
// 내 영어 배터리 글(한국어 질의와 의미가 같다)이 무관한 내 글(배송, 가격)보다 위에 있다
const rank = (needle) => { const i = found.findIndex((f) => f.text.includes(needle)); return i < 0 ? Infinity : i }
assert.ok(rank('The battery life') < rank('배송이 빨라서'), '의미가 비슷한 글이 위로: ' + JSON.stringify(found.map((f) => f.text.slice(0, 30))))
assert.ok(rank('The battery life') < rank('가격 대비'), '의미가 비슷한 글이 위로')
assert.ok(found.filter((f) => !f.mine).length >= 1, '다른 사용자의 글도 함께')
assert.ok(found.some((f) => !f.mine && f.text.includes('다른 사용자') && f.text.includes('원문은 서버에 보관하지 않아')), '다른 사용자의 글은 원문 없이')
assert.ok(found.some((f) => f.mine && f.text.includes('The battery life is excellent')), '내 글은 원문과 함께')
await shot('07-search'); ok('검색: 내 글(원문)과 다른 사용자 글(원문 없음)이 함께, 유사도 순')

// 오프라인: 서버가 꺼지면 이 기기에 저장해 두고, 돌아오면 보낸다
await stopMock()
await submit('example.com', '오프라인에서 쓴 글입니다.')
await page.waitForFunction(() => document.querySelector('#notice')?.textContent.includes('오프라인이라 이 기기에 저장했습니다'))
await page.waitForFunction(() => document.querySelector('#list li.row-item .pill')?.textContent === '전송 대기')
assert.match(await page.textContent('#net'), /오프라인/)
await page.fill('#q', '오프라인'); await page.click('#searchForm button')
await page.waitForSelector('#searchNotice[data-tone="warn"]')
assert.match(await page.textContent('#searchNotice'), /내 글만 보여 줍니다/)
const offlineHits = await page.locator('#results li').allTextContents()
assert.ok(offlineHits.length >= 1 && offlineHits.every((t) => !t.includes('서버 반영 전')), '서버 검색이 실패해도 등록된 글을 "서버 반영 전"이라고 하지 않는다')
await shot('08-offline'); ok('오프라인: 전송 대기로 저장, 검색은 내 글만')
await startMock()
await page.click('#recheck')
await page.waitForFunction(() => document.querySelector('#list li.row-item .pill')?.textContent !== '전송 대기', null, { timeout: 15000 })
assert.ok(serverLog().some((p) => p.text === '오프라인에서 쓴 글입니다.'), '복구 후 서버가 받았다')
await page.waitForFunction(() => document.querySelector('#net')?.textContent.includes('서버 연결됨'))
await shot('09-recovered'); ok('연결 복구 → 자동 전송, 서버 연결됨')

// 새로고침해도 목록이 남는다 (실제 IndexedDB에 Dexie로 저장한 캐시)
await page.reload(); await page.waitForSelector('#list li.row-item')
assert.ok((await rowTexts()).length >= 6)
assert.ok((await rowTexts()).every((r) => r.pill !== '확인 불가'), '서버를 재시작해도 기록이 유지되어 확인 불가가 없다')
ok(`새로고침 후에도 목록 ${(await rowTexts()).length}건 유지 (실제 IndexedDB)`)
await page.emulateMedia({ colorScheme: 'dark' }); await shot('10-dark'); ok('다크 테마')

await browser.close(); ui.kill(); await stopMock()
console.log('UI E2E OK')
