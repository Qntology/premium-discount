// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/www/tests/*.test.ts  (premium-discount 폴더에서)
// 웹 행동 분석 수집 (/analytic/events, /analytic/sites): tracker-logis-center 의 Worker 를 옮긴 것
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { ethers } from 'ethers'
import { gzip } from 'pako'
import { cleanHref, handleAnalytic, hashId, hostOf, sanitizeEvent, scrubText, siteMetaOwners, type AnalyticEnv, type Fetcher } from '../src/analytic.ts'
import { decodeBlob, decodeBytes, encodeBlob, bytesToBase64 } from '../src/blob.ts'
import { walletRequestMessage } from '../src/auth.ts'
import { FakeD1, FakeLimiter } from '../../../test-support/fakes.ts'

const NOW = Date.UTC(2026, 10, 18, 3, 0, 0)
const SHOP = 'https://shop.example.com'
const enc = new TextEncoder()

function setup() {
	const db = new FakeD1()
	const limiter = new FakeLimiter()
	const env = { DB: db, GUEST_SALT: 'salt', AUTHOR_KEY_SECRET: 'secret', ANALYTIC_WRITE_LIMITER: limiter }
	return { db, limiter, env: env as unknown as AnalyticEnv }
}

function click(over: Record<string, unknown> = {}) {
	return {
		type: 'click',
		href: SHOP + '/checkout?coupon=A1&email=a@b.co#top',
		sid: 's-1',
		seq: 3,
		t_ms: 9500,
		target: { chain: [{ tag: 'button', id: 'easy-pay', classes: ['btn'] }], pug: 'button | 간편결제' },
		related: [{ chain: [{ tag: 'button', id: 'coupon' }], pug: 'button | 쿠폰 적용 010-1234-5678' }],
		...over,
	}
}

function collect(body: unknown, headers: Record<string, string> = {}) {
	const raw = typeof body === 'string' || body instanceof Uint8Array ? body : JSON.stringify(body)
	return new Request('https://premium.discount/analytic/events', {
		method: 'POST',
		headers: { 'Content-Type': 'text/plain;charset=UTF-8', Origin: SHOP, 'cf-connecting-ip': '203.0.113.9', ...headers },
		body: raw as BodyInit,
	})
}

async function send(env: AnalyticEnv, request: Request, fetcher?: Fetcher, nowMs = NOW) {
	const res = (await handleAnalytic(request, env, nowMs, fetcher))!
	return { status: res.status, body: (res.status === 204 ? null : await res.json()) as any, res }
}

async function owner(wallet: ethers.HDNodeWallet, method: string, path: string, body?: unknown, query = '', nowMs = NOW) {
	const text = body === undefined ? '' : JSON.stringify(body)
	const signature = await wallet.signMessage(await walletRequestMessage(wallet.address, nowMs, method, path, enc.encode(text)))
	const headers = { 'Content-Type': 'application/json', Origin: 'tauri://localhost', 'x-pd-address': wallet.address, 'x-pd-time': String(nowMs), 'x-pd-signature': signature }
	return new Request('https://premium.discount' + path + query, text ? { method, headers, body: text } : { method, headers })
}

// 사이트 첫 화면을 흉내 내는 fetch
function siteFetcher(html: string, status = 200): Fetcher & { calls: string[] } {
	const calls: string[] = []
	const f = (async (url: string) => {
		calls.push(url)
		return new Response(html, { status, headers: { 'Content-Type': 'text/html' } })
	}) as Fetcher & { calls: string[] }
	f.calls = calls
	return f
}

test('주소와 글을 다듬는다: 쿼리 · 문서 안 위치 빼기, 해시 라우팅 남기기, 이메일 · 긴 숫자 지우기 (Rust 와 같은 규칙)', () => {
	assert.equal(cleanHref('HTTPS://Shop.example.com/cart?coupon=A1&email=x@y.z#top'), 'https://Shop.example.com/cart')
	assert.equal(cleanHref('https://app.example.com/#/orders/12?tab=2'), 'https://app.example.com/#/orders/12')
	assert.equal(cleanHref('https://app.example.com/#!/cart'), 'https://app.example.com/#!/cart')
	assert.equal(cleanHref('javascript:alert(1)'), null)
	assert.equal(cleanHref('https:///nohost'), null)
	assert.equal(hostOf('https://user:pw@Shop.Example.com:8443/a'), 'shop.example.com')
	assert.equal(scrubText('div\n  span | 홍길동 010-1234-5678 hong@example.com\n  button | 결제하기 2026'), 'div\n  span | 홍길동 # #\n  button | 결제하기 #')
})

test('사건 모양을 검사한다', () => {
	assert.equal(sanitizeEvent({ ...click(), type: 'keylog' }), null)
	assert.equal(sanitizeEvent({ ...click(), href: 'file:///etc/passwd' }), null)
	assert.equal(sanitizeEvent({ ...click(), sid: '' }), null)
	const e = sanitizeEvent({ ...click(), related: Array.from({ length: 150 }, (_, i) => ({ pug: `li | 상품 ${i}` })), value: '비밀', rage: 'yes' })!
	assert.equal(e.related!.length, 100)
	assert.equal(e.href, SHOP + '/checkout')
	assert.equal((e as any).value, undefined, '모르는 칸(입력값 등)은 버린다')
	assert.equal(e.rage, undefined, 'rage 는 true 일 때만')
	const badTag = sanitizeEvent({ ...click(), target: { chain: [{ tag: 'b<script>' }], pug: '' } })!
	assert.equal(badTag.target, undefined)
})

test('수집: 사이트마다 쌓고, sid 원문 · 쿼리 · 개인정보 낱말은 남기지 않는다', async () => {
	const { db, env, limiter } = setup()
	const r = await send(env, collect({ events: [click(), click({ type: 'hover', seq: 4, dwell_ms: 820, related: [] })] }))
	assert.equal(r.status, 200)
	assert.equal(r.body.stored, 2)
	assert.equal(r.res.headers.get('Access-Control-Allow-Origin'), '*')
	assert.equal(limiter.keys.length, 1)
	const rows = db.rows(`SELECT * FROM web_events ORDER BY "seq"`)
	assert.equal(rows.length, 2)
	const cc = hashId('shop.example.com')
	assert.equal(rows[0].cc, cc)
	assert.equal(rows[0].session, hashId(cc + 's-1'))
	assert.equal(rows[0].updated_at, 0)
	assert.equal(rows[0].created_at, NOW)
	const data = decodeBlob(rows[0].data)
	assert.equal(data.schema, 'web-event/1')
	assert.equal(data.href, SHOP + '/checkout')
	assert.equal(data.sid, undefined)
	assert.equal((data.related as any)[0].pug, 'button | 쿠폰 적용 #')
	assert.ok(!JSON.stringify(data).includes('a@b.co'))
	assert.equal(decodeBlob(rows[1].data).dwell_ms, 820)
})

test('수집: gzip 본문 · 배열 본문도 받는다, 다른 사이트의 사건 · 모양이 틀린 사건 · 너무 많은 사건은 받지 않는다', async () => {
	const { db, env } = setup()
	const gz = gzip(enc.encode(JSON.stringify([click()])))
	assert.equal((await send(env, collect(gz, { 'Content-Encoding': 'gzip' }))).status, 200)
	assert.equal((await send(env, collect([click({ seq: 5 })]))).status, 200)
	const foreign = await send(env, collect([click({ href: 'https://other.example.com/cart' })]))
	assert.equal(foreign.status, 403, '페이지의 Origin 과 사건 주소가 다르다')
	assert.equal((await send(env, collect({ events: [click(), { type: 'click' }] }))).status, 400)
	assert.equal((await send(env, collect({ events: Array.from({ length: 21 }, () => click()) }))).status, 400)
	assert.equal((await send(env, collect('not json'))).status, 400)
	assert.equal(db.rows(`SELECT COUNT(*) AS n FROM web_events`)[0].n, 2, '거절한 요청은 하나도 남기지 않는다')
	// 앱 · 서버 사이 호출처럼 Origin 이 없으면 주소만 본다
	assert.equal((await send(env, collect([click({ href: 'https://other.example.com/' })], { Origin: '' }))).status, 200)
})

test('수집: 요청 제한과 사전 요청', async () => {
	const { env, limiter } = setup()
	limiter.allow = false
	const r = await send(env, collect([click()]))
	assert.equal(r.status, 429)
	assert.equal(r.res.headers.get('Retry-After'), '60')
	const pre = await send(env, new Request('https://premium.discount/analytic/events', { method: 'OPTIONS', headers: { Origin: SHOP, 'Access-Control-Request-Method': 'POST' } }))
	assert.equal(pre.status, 204)
	assert.equal(pre.res.headers.get('Access-Control-Allow-Origin'), '*')
})

test('사이트 주인 확인: 첫 화면의 meta 가 지갑 주소를 말해야 한다', async () => {
	const { db, env } = setup()
	const wallet = ethers.Wallet.createRandom()
	const address = wallet.address.toLowerCase()
	assert.deepEqual(siteMetaOwners(`<head><META content='${address}, 0x${'a'.repeat(40)}' Name="premium-discount-site"><meta name="x" content="${address}"></head>`), [address, '0x' + 'a'.repeat(40)])

	const wrong = siteFetcher(`<meta name="premium-discount-site" content="0x${'b'.repeat(40)}">`)
	const denied = await send(env, await owner(wallet, 'POST', '/analytic/sites', { host: 'Shop.Example.com' }), wrong)
	assert.equal(denied.status, 403)
	assert.equal(denied.body.expected, `<meta name="premium-discount-site" content="${address}">`)
	assert.deepEqual(wrong.calls, ['https://shop.example.com/'])
	assert.equal((await send(env, await owner(wallet, 'POST', '/analytic/sites', { host: 'shop.example.com' }), siteFetcher('down', 503))).status, 403)
	assert.equal((await send(env, await owner(wallet, 'POST', '/analytic/sites', { host: 'not a host' }), wrong)).status, 400)

	const ok = await send(env, await owner(wallet, 'POST', '/analytic/sites', { host: 'https://shop.example.com/any/path' }), siteFetcher(`<html><head><meta name="premium-discount-site" content="${address}"></head></html>`))
	assert.equal(ok.status, 200)
	assert.equal(ok.body.cc, hashId('shop.example.com'))
	assert.deepEqual(db.rows(`SELECT "host", "owner" FROM analytic_sites`), [{ host: 'shop.example.com', owner: address }])
	const mine = await send(env, await owner(wallet, 'GET', '/analytic/sites'))
	assert.deepEqual(mine.body.sites.map((s: any) => s.host), ['shop.example.com'])
	// 서명이 없으면 401
	const unsigned = await send(env, new Request('https://premium.discount/analytic/sites', { method: 'POST', body: '{"host":"shop.example.com"}' }), wrong)
	assert.equal(unsigned.status, 401)
})

test('사건 읽기: 확인된 주인만, (시각, id) 커서로 빠짐없이, base64 gzip 으로', async () => {
	const { db, env } = setup()
	const wallet = ethers.Wallet.createRandom()
	const stranger = ethers.Wallet.createRandom()
	// 같은 시각에 5건, 그 뒤에 1건
	await send(env, collect({ events: [1, 2, 3, 4, 5].map((seq) => click({ seq })) }))
	await send(env, collect([click({ seq: 6 })]), undefined, NOW + 1000)
	const before = await send(env, await owner(wallet, 'GET', '/analytic/events', undefined, '?host=shop.example.com'))
	assert.equal(before.status, 403, '사이트를 확인하기 전')
	await send(env, await owner(wallet, 'POST', '/analytic/sites', { host: 'shop.example.com' }), siteFetcher(`<meta name="premium-discount-site" content="${wallet.address}">`))
	assert.equal((await send(env, await owner(stranger, 'GET', '/analytic/events', undefined, '?host=shop.example.com'))).status, 403, '다른 지갑')

	const seen: number[] = []
	let query = '?host=shop.example.com&limit=2'
	for (let page = 0; page < 5; page++) {
		const r = await send(env, await owner(wallet, 'GET', '/analytic/events', undefined, query))
		assert.equal(r.status, 200)
		for (const row of r.body.rows) {
			const e = decodeBlob(row.data)
			seen.push(e.seq as number)
			assert.equal(row.updated_at, 0)
		}
		if (!r.body.more) break
		query = `?host=shop.example.com&limit=2&since=${r.body.next.since}&after_id=${r.body.next.after_id}`
	}
	assert.deepEqual(seen.sort(), [1, 2, 3, 4, 5, 6])

	// 주인은 오래된 사건을 지울 수 있다
	const del = await send(env, await owner(wallet, 'DELETE', '/analytic/events', undefined, `?host=shop.example.com&before=${NOW + 1}`))
	assert.equal(del.body.deleted, 5)
	assert.equal(db.rows(`SELECT COUNT(*) AS n FROM web_events`)[0].n, 1)
})

test('사이트 주인 확인: QR 로그인 세션이면 이메일의 hashId 가 주인 주소다 (앱이 쓰는 갈래)', async () => {
	const { db, env } = setup()
	const email = 'owner@shop.example.com'
	const sessionEnv = { ...env, SESSION_S3_BUCKET: 'oauth-sessions', SESSION_S3_REGION: 'ap-northeast-2', SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST', SESSION_S3_SECRET_ACCESS_KEY: 'test-secret' } as AnalyticEnv
	const headers = { 'Content-Type': 'application/json', 'x-oauth-hash': 'ab'.repeat(20), 'x-oauth-token': 'cd'.repeat(32), 'cf-connecting-ip': '203.0.113.9' }
	const address = ethers.computeAddress(ethers.hashMessage(email)).toLowerCase()
	const realFetch = globalThis.fetch
	// 세션 확인(S3 의 hash/<hash>)은 전역 fetch, 사이트 첫 화면은 넘긴 fetcher 로
	globalThis.fetch = (async () => new Response(JSON.stringify({ token: 'cd'.repeat(32), email }))) as typeof fetch
	try {
		const site = siteFetcher(`<meta name="premium-discount-site" content="${address}">`)
		const reg = await send(sessionEnv, new Request('https://premium.discount/analytic/sites', { method: 'POST', headers, body: JSON.stringify({ host: 'shop.example.com' }) }), site)
		assert.equal(reg.status, 200, JSON.stringify(reg.body))
		assert.equal(reg.body.owner, address)
		await send(sessionEnv, collect([click()]))
		const read = await send(sessionEnv, new Request('https://premium.discount/analytic/events?host=shop.example.com', { headers }))
		assert.equal(read.status, 200)
		assert.equal(read.body.rows.length, 1)
		// 세션 헤더가 틀리면 (S3 가 다른 토큰을 말하면) 401
		globalThis.fetch = (async () => new Response(JSON.stringify({ token: 'ef'.repeat(32), email }))) as typeof fetch
		assert.equal((await send(sessionEnv, new Request('https://premium.discount/analytic/events?host=shop.example.com', { headers }))).status, 401)
	} finally {
		globalThis.fetch = realFetch
	}
	assert.equal(db.rows(`SELECT COUNT(*) AS n FROM analytic_sites`)[0].n, 1)
})

// contracts/analytic_blob_v1.json: Rust(crates/premium-discount-analytic) 와 함께 통과하는 사례
const blobCases = JSON.parse(readFileSync(fileURLToPath(new URL('../../../contracts/analytic_blob_v1.json', import.meta.url)), 'utf8'))

test('BLOB 코덱: 계약의 사례 (Rust 와 같다)', () => {
	assert.ok(blobCases.cases.length >= 10)
	for (const c of blobCases.cases) assert.deepEqual(decodeBlob(c.input), c.expect, c.name)
})

test('BLOB 코덱: logis-center 의 BUG-RT-2 · 3 · 4 가 고쳐졌다', () => {
	// BUG-RT-2: 50 바이트 이하의 base64 gzip
	const small = encodeBlob({ a: 1 })
	assert.ok(small.length <= 50)
	assert.deepEqual(decodeBlob(bytesToBase64(small)), { a: 1 })
	// BUG-RT-3: 조각(subarray)은 그 범위만
	const view = new TextEncoder().encode('xx{"a":1}').subarray(2)
	assert.deepEqual(decodeBlob(view), { a: 1 })
	assert.deepEqual(decodeBlob(new DataView(new TextEncoder().encode('zz{"b":2}').buffer, 2)), { b: 2 })
	// BUG-RT-4: 원시값은 사건이 아니다
	assert.deepEqual(decodeBlob('42'), {})
	assert.deepEqual(decodeBlob('null'), {})
	assert.deepEqual(decodeBytes(enc.encode('"x"')), {})
})
