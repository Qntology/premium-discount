// 모델 내려받기 E2E: 모델이 없는 상태로 실행 파일을 켜고, 화면에서 텍스트 전용 모델을 받아 앱이 시작되는지 본다.
// Hugging Face 자리는 model_server.py(가짜, 실제 모델 파일을 Range로 답함, 속도 제한), 평가 서버는 mock_server.py.
// 받은 바이트는 고정 해시로 확인하므로 실제 모델 파일이 필요하다.
//
//   cd app/ui && npm install && npm run build
//   node tests/e2e/ui_model_e2e.mjs target/release/premium-discount-app <EMBEDDINGGEMMA_DIR(원본 파일)> <스크린샷 폴더> [<LAYA_DIR>]
//   LAYA_DIR을 주면 욕설 필터 모델(laya-multilingual) 내려받기와 판정도 본다. QWEN_DIR(환경 변수)을 주면 items 초안 모델도 본다
import { createRequire } from 'node:module'
import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, rmSync, statSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import assert from 'node:assert/strict'

const require = createRequire(import.meta.url)
const { chromium } = require('playwright')
const [BIN_ARG, SOURCE, OUT, LAYA] = process.argv.slice(2)
const QWEN = process.env.QWEN_DIR
const here = (p) => fileURLToPath(new URL(p, import.meta.url))
const BIN = fileURLToPath(new URL(BIN_ARG, `file://${process.cwd()}/`))
const SERVER_PORT = 18795, MODEL_PORT = 18796, UI_PORT = 18797
const LOG = '/tmp/pd-model-e2e-server.log', MODEL_LOG = '/tmp/pd-model-e2e-hf.log'
const DATA = '/tmp/pd-model-e2e-data', MODEL = '/tmp/pd-model-e2e-model', FILTER = '/tmp/pd-model-e2e-filter', DRAFT = '/tmp/pd-model-e2e-draft'
for (const p of [LOG, MODEL_LOG, DATA, MODEL, FILTER, DRAFT]) rmSync(p, { recursive: true, force: true })
mkdirSync(OUT, { recursive: true })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const lines = (f) => (existsSync(f) ? readFileSync(f, 'utf8').trim().split('\n').filter(Boolean).map((l) => JSON.parse(l)) : [])
const hf = (o) => fetch(`http://127.0.0.1:${MODEL_PORT}/__control`, { method: 'POST', body: JSON.stringify(o) }).then((r) => r.json())

const mock = spawn('python3', [here('mock_server.py'), String(SERVER_PORT), LOG], { stdio: 'inherit' })
const hfServer = spawn('python3', [here('model_server.py'), String(MODEL_PORT), SOURCE, MODEL_LOG, ...(LAYA ? [`convaiinnovations/laya-multilingual=${LAYA}`] : []), ...(QWEN ? [`Qwen/Qwen3.5-0.8B=${QWEN}`] : [])], { stdio: 'inherit' })
process.on('exit', () => { ui?.kill(); mock.kill(); hfServer.kill() })
for (let i = 0; i < 50; i++) { try { await hf({}); break } catch { await sleep(100) } }
// 진행률과 멈추기를 화면에서 볼 수 있도록 초당 40MB로 늦춘다 (텍스트 전용 574MB ≈ 15초)
await hf({ rate: 40e6 })

const ui = spawn('node', [here('../../ui/dev-server.mjs')], {
	env: { ...process.env, PD_MODEL_DIR: MODEL, PD_FILTER_MODEL_DIR: FILTER, PD_DRAFT_MODEL_DIR: DRAFT, PD_DATA_DIR: DATA, PD_SERVER_URL: `http://127.0.0.1:${SERVER_PORT}`, PD_MODEL_BASE_URL: `http://127.0.0.1:${MODEL_PORT}`, PD_BIN: BIN, PORT: String(UI_PORT) },
	stdio: ['ignore', 'pipe', 'inherit'],
})
await new Promise((r) => ui.stdout.on('data', (d) => String(d).includes('http://') && r()))
const invoke = (cmd, args = null) =>
	fetch(`http://127.0.0.1:${UI_PORT}/invoke`, { method: 'POST', headers: { 'content-type': 'application/json', 'x-pd-ui': '1' }, body: JSON.stringify({ cmd, args }) }).then((r) => r.json())

const browser = await chromium.launch()
const ctx = await browser.newContext({ viewport: { width: 1000, height: 820 }, deviceScaleFactor: 1.5, colorScheme: 'light' })
const page = await ctx.newPage()
page.on('pageerror', (e) => { throw e })
const shot = async (p, name) => { await p.waitForTimeout(250); await p.screenshot({ path: `${OUT}/${name}.png` }) }
let step = 0; const ok = (m) => console.log(`OK ${++step}: ${m}`)
const text = (sel) => page.textContent(sel)
const percent = () => page.evaluate(() => document.getElementById('dlProgress').value)

// ---- 1. 모델이 없으면: 시작 안내 + 내려받기 ----
await page.goto(`http://127.0.0.1:${UI_PORT}/`)
await page.waitForSelector('#modelFetch:not([hidden])')
assert.equal(await page.locator('#layout').isVisible(), false)
assert.match(await text('#setupError'), /토크나이저|가중치 파일/)
const choices = await page.locator('#dlChoices button').allTextContents()
assert.equal(choices.length, 2)
assert.match(choices[0], /텍스트 전용 · 574\.3MB \(권장\)/)
assert.match(choices[1], /전체 · 1\.52GB/)
const status0 = await invoke('cmd_app_status')
assert.equal(status0.ok, true)
assert.equal(status0.result.ready, false)
await shot(page, 'm1-no-model'); ok(`모델이 없으면 시작 안내와 내려받기 선택지: ${choices.map((c) => c.split('·')[0].trim()).join(', ')}`)

// 모바일 폭에서도 선택지가 잘리지 않는다
const phone = await ctx.newPage()
await phone.setViewportSize({ width: 390, height: 844 })
await phone.goto(`http://127.0.0.1:${UI_PORT}/?platform=android`)
await phone.waitForSelector('#modelFetch:not([hidden])')
assert.equal(await phone.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, '가로 스크롤 없음')
await shot(phone, 'm2-no-model-phone'); await phone.close(); ok('390px 폭: 가로 스크롤 없이 선택지 표시')

// ---- 2. 텍스트 전용 받기 → 진행률 → 멈추기 ----
await page.click('#dlChoices button[data-variant="text"]')
await page.waitForFunction(() => document.getElementById('dlProgress').value >= 8, null, { timeout: 60000 })
assert.match(await text('#dlStatus'), /내려받는 중 .*MB \/ 574\.3MB \(\d+%\)/)
assert.equal(await page.locator('#dlCancel').isVisible(), true)
assert.equal(await page.locator('#dlChoices button[data-variant="full"]').isDisabled(), true, '받는 중에는 다른 선택을 막는다')
await shot(page, 'm3-downloading'); ok(`진행률 표시: ${(await text('#dlStatus')).trim()}`)

await page.click('#dlCancel')
await page.waitForFunction(() => /멈췄습니다/.test(document.getElementById('dlStatus').textContent))
const part = `${MODEL}/model-text.safetensors.part`
assert.ok(existsSync(part) && statSync(part).size > 10e6, '받은 부분이 남아 있다')
const stoppedAt = statSync(part).size
assert.match(await text('#dlStatus'), /이어받습니다/)
await shot(page, 'm4-cancelled'); ok(`멈추기: 받은 부분 ${(stoppedAt / 1e6).toFixed(1)}MB가 남고, 이어받기 안내`)

// ---- 3. 다시 누르면 이어받고, 끝나면 앱이 시작된다 ----
await hf({ rate: 400e6 })
await page.click('#dlChoices button[data-variant="text"]')
await page.waitForSelector('.acct:not([hidden])', { timeout: 180000 })
assert.equal(await page.locator('#setup').isVisible(), false)
assert.equal(await page.locator('#layout').isVisible(), true)
const reqs = lines(MODEL_LOG).filter((r) => r.name === 'model.safetensors' && r.status === 206)
const resumed = reqs.filter((r) => r.start > 612_180_776)
assert.ok(resumed.length >= 1, `텍스트 구간 중간부터 이어받았다: ${JSON.stringify(reqs.map((r) => r.range))}`)
assert.ok(reqs.every((r) => r.start < 1_154_186_072), '이미지·오디오 구간(텍스트 구간 밖)은 요청하지 않았다')
assert.equal(lines(MODEL_LOG).filter((r) => r.name === 'model.safetensors' && r.status === 200).length, 0, '원본 전체는 받지 않았다')
assert.ok(existsSync(`${MODEL}/model-text.safetensors`) && existsSync(`${MODEL}/tokenizer.json`))
assert.ok(!existsSync(`${MODEL}/model.safetensors`) && !existsSync(part))
const sentText = reqs.reduce((n, r) => n + r.sent, 0)
ok(`이어받기(${resumed[0].range}) → 설치 → 앱 시작. 받은 모델 바이트 ${(sentText / 1e6).toFixed(1)}MB (원본 1488.9MB 중 텍스트 구간만)`)

// ---- 4. 받은 모델로 실제 등록 ----
await page.fill('#cc', 'example.com')
await page.fill('#text', '내려받은 모델로 쓰는 첫 평가입니다.')
await page.click('#submit')
await page.waitForFunction(() => /서버가 받았습니다/.test(document.getElementById('notice')?.textContent ?? ''), null, { timeout: 60000 })
const posted = lines(LOG).filter((r) => r.path === '/evaluations' || r.vector_len)
assert.equal(posted.at(-1).vector_len, 768)
assert.ok(Math.abs(posted.at(-1).norm - 1) < 0.01)
await shot(page, 'm5-ready-submitted'); ok('받은 모델로 등록: 서버가 768차원 단위 벡터를 받음')

// ---- 5. 상태: 텍스트 전용 설치라 타워가 없다 ----
const status = await invoke('cmd_model_status')
assert.equal(status.result.installed, 'text')
assert.equal(status.result.residency.vision.state, 'not_installed')
assert.equal(status.result.residency.audio.state, 'not_installed')
const app = await invoke('cmd_app_status')
assert.equal(app.result.ready, true)
assert.ok(app.result.model.resident_mb > 0)
ok(`cmd_model_status: installed=text, 비전·오디오 not_installed, 상주 메모리 ${app.result.model.resident_mb}MB`)

// ---- 6. 욕설 필터 모델: 사전이 놓치는 모욕을 laya로 받은 뒤에는 막는다 ----
if (LAYA) {
	await page.waitForSelector('#filterInfo:not([hidden])')
	assert.match(await text('#filterText'), /욕설 필터: 기본 사전/)
	assert.match(await text('#filterGet'), /더 정확한 모델 받기 \(678\.2MB\)/)
	const insult = 'Go to hell, you idiot seller.'
	const before = lines(LOG).filter((r) => r.path === '/evaluations').length
	await page.fill('#text', insult)
	await page.click('#submit')
	await page.waitForFunction(() => /서버가 받았습니다/.test(document.getElementById('notice')?.textContent ?? ''), null, { timeout: 60000 })
	assert.equal(lines(LOG).filter((r) => r.path === '/evaluations').length, before + 1, '사전 필터는 욕 단어가 없는 모욕을 통과시킨다')
	await shot(page, 'm6-filter-dictionary'); ok('사전 필터: 욕 단어 없는 모욕("Go to hell, you idiot seller.")은 통과 → 서버로 감')

	await page.click('#filterGet')
	await page.waitForFunction(() => /laya-multilingual 모델/.test(document.getElementById('filterText').textContent), null, { timeout: 300000 })
	assert.ok(existsSync(`${FILTER}/model.safetensors`) && existsSync(`${FILTER}/tokenizer/tokenizer.json`))
	const st = await invoke('cmd_model_status')
	assert.equal(st.result.filter.active, 'laya')
	assert.equal(st.result.filter.installed, true)
	await shot(page, 'm7-filter-laya'); ok('필터 모델 받기 → 설치 → 앱을 다시 켜지 않고 laya로 바뀜 (cmd_model_status.filter.active=laya)')

	await page.fill('#text', insult)
	await page.click('#submit')
	await page.waitForFunction(() => /욕설이 포함되어 등록하지 않았습니다/.test(document.getElementById('notice')?.textContent ?? ''), null, { timeout: 60000 })
	assert.equal(lines(LOG).filter((r) => r.path === '/evaluations').length, before + 1, 'laya가 막으면 서버로 가지 않는다')
	await page.fill('#text', '포장이 조금 아쉬웠지만 제품은 좋아요.')
	await page.click('#submit')
	await page.waitForFunction(() => /서버가 받았습니다/.test(document.getElementById('notice')?.textContent ?? ''), null, { timeout: 60000 })
	await shot(page, 'm8-filter-blocked'); ok('laya 필터: 같은 모욕은 이 기기에서 막고(서버 전송 없음), 정상 글은 그대로 등록')
}

// ---- 7. items 초안: 초안 모델(텍스트 부분만)을 받아 이 기기에서 초안을 만든다 ----
if (QWEN) {
	await page.waitForSelector('#draftCard:not([hidden])')
	assert.match(await text('#draftModelText'), /초안 모델이 없습니다/)
	assert.match(await text('#draftGet'), /초안 모델 받기 \(1\.52GB\)/)
	assert.equal(await page.locator('#draftForm').isVisible(), false)
	await page.click('#draftGet')
	await page.waitForSelector('#draftForm:not([hidden])', { timeout: 900000 })
	assert.ok(existsSync(`${DRAFT}/model-text.safetensors`) && existsSync(`${DRAFT}/tokenizer.json`))
	const qreqs = lines(MODEL_LOG).filter((r) => r.repo === 'Qwen/Qwen3.5-0.8B' && r.name.endsWith('.safetensors'))
	assert.ok(qreqs.length > 0 && qreqs.every((r) => r.status === 206 && r.start < 1_504_851_080), '비전·MTP 구간과 원본 전체는 받지 않았다')
	ok(`초안 모델 받기 → 텍스트 부분만 설치 (Range 요청 ${qreqs.length}건, 받은 ${(qreqs.reduce((n, r) => n + r.sent, 0) / 1e6).toFixed(1)}MB)`)

	await page.fill('#pageText', '스마트 워치 스트랩 2+1 행사! 이번 주말까지 전 품목 최대 60% 할인, 쿠폰 사용 시 추가 5% 할인. 무료 배송.')
	await page.fill('#pageLink', 'https://shop.example/strap')
	await page.click('#draftBtn')
	await page.waitForSelector('#draftOut:not([hidden])', { timeout: 300000 })
	const out = await text('#draftOut')
	assert.match(out, /^type: 2 discount/m, out)
	assert.match(out, /^head: \S/m)
	assert.match(out, /^link: https:\/\/shop\.example\/strap$/m)
	assert.match(out, /^digest: [0-9a-f]{64}$/m)
	await shot(page, 'm9-item-draft'); ok(`초안: ${out.split('\n').slice(0, 2).join(' / ')}`)
}

await browser.close()
ui.kill()
console.log('MODEL E2E OK')
process.exit(0)
