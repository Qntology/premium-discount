// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
// D1은 실제 SQLite(node:sqlite)에 migrations 0002~0005를 적용한 가짜다.
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { handleEvaluations } from '../src/evaluations.ts'
import { addressFromEmail } from '../src/auth.ts'
import { FakeD1, FakeVectorize, FakeLimiter, unitVector } from '../../test-support/fakes.ts'

const ORIGIN = 'tauri://localhost'
const IP = '203.0.113.7'
const HASH = 'ab'.repeat(20)
const TOKEN = 'cd'.repeat(32)
const CLIENT_FROM = '0x1111111111111111111111111111111111111111'
const realFetch = globalThis.fetch

beforeEach(() => {
	globalThis.fetch = (async () => {
		throw new Error('network must not be used in this test')
	}) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup(extra: Record<string, unknown> = {}) {
	const db = new FakeD1()
	const vec = new FakeVectorize()
	const write = new FakeLimiter()
	const read = new FakeLimiter()
	const env = {
		DB: db,
		EVAL_VECTORIZE: vec,
		GUEST_SALT: 'salt',
		AUTHOR_KEY_SECRET: 'secret',
		EVAL_WRITE_LIMITER: write,
		EVAL_READ_LIMITER: read,
		...extra,
	}
	return { db, vec, write, read, env: env as unknown as Parameters<typeof handleEvaluations>[1] }
}

async function sha256Hex(text: string) {
	const d = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))
	return Array.from(new Uint8Array(d), (b) => b.toString(16).padStart(2, '0')).join('')
}

async function postBody(over: Record<string, unknown> = {}) {
	const text = '좋은 평가입니다'
	return {
		request_id: 'req-0123456789abcdef',
		cc: 'example.com',
		text,
		text_hash: await sha256Hex(text),
		vector: Array.from(unitVector()),
		...over,
	}
}

function post(path: string, body: unknown, headers: Record<string, string> = {}) {
	return new Request('https://premium.discount' + path, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json', Origin: ORIGIN, 'cf-connecting-ip': IP, ...headers },
		body: JSON.stringify(body),
	})
}

// ---- CORS ----

test('사전 요청(OPTIONS)은 x-oauth-hash·x-oauth-token 헤더와 GET을 허용한다', async () => {
	const { env, write, read } = setup()
	const res = (await handleEvaluations(
		new Request('https://premium.discount/evaluations', {
			method: 'OPTIONS',
			headers: { Origin: ORIGIN, 'Access-Control-Request-Headers': 'content-type, x-oauth-hash, x-oauth-token', 'cf-connecting-ip': IP },
		}),
		env
	))!
	assert.equal(res.status, 204)
	assert.equal(res.headers.get('Access-Control-Allow-Origin'), ORIGIN)
	const allowed = res.headers.get('Access-Control-Allow-Headers')!.toLowerCase().split(',').map((s) => s.trim())
	for (const h of ['content-type', 'x-oauth-hash', 'x-oauth-token']) assert.ok(allowed.includes(h), h)
	assert.match(res.headers.get('Access-Control-Allow-Methods')!, /\bGET\b/)
	assert.match(res.headers.get('Access-Control-Allow-Methods')!, /\bPOST\b/)
	assert.equal(write.keys.length + read.keys.length, 0, '사전 요청은 제한 대상이 아니다')
})

test('허용되지 않은 출처에는 Allow-Origin을 주지 않는다', async () => {
	const { env } = setup()
	const res = (await handleEvaluations(new Request('https://premium.discount/evaluations', { method: 'OPTIONS', headers: { Origin: 'https://evil.example' } }), env))!
	assert.equal(res.status, 204)
	assert.equal(res.headers.get('Access-Control-Allow-Origin'), null)
})

// ---- 저장 ----

test('저장: 202로 접수하고 bcc를 대기열에 남긴다', async () => {
	const { env, db } = setup()
	const bcc = '0x' + 'AB'.repeat(20)
	const res = (await handleEvaluations(post('/evaluations', await postBody({ bcc, from: CLIENT_FROM })), env))!
	assert.equal(res.status, 202)
	assert.equal(res.headers.get('Access-Control-Allow-Origin'), ORIGIN)
	const body = await res.json()
	assert.equal(body.status, 'pending')
	const row = db.rows(`SELECT "from", "bcc", "text" FROM eval_queue`)[0]
	assert.equal(row.bcc, bcc.toLowerCase())
	assert.equal(row.from, CLIENT_FROM, '세션이 없으면 클라이언트 값을 쓴다')
})

// ---- 세션 검증 연결 ----

const S3 = {
	SESSION_S3_BUCKET: 'oauth-sessions',
	SESSION_S3_REGION: 'ap-northeast-2',
	SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST',
	SESSION_S3_SECRET_ACCESS_KEY: 'test-secret',
}
const loggedIn = { 'x-oauth-hash': HASH, 'x-oauth-token': TOKEN }

test('검증된 세션이면 클라이언트가 보낸 from을 무시하고 세션 주소로 저장한다', async () => {
	globalThis.fetch = (async () => new Response(JSON.stringify({ token: TOKEN, email: 'user@example.com' }))) as typeof fetch
	const { env, db } = setup(S3)
	const res = (await handleEvaluations(post('/evaluations', await postBody({ from: CLIENT_FROM }), loggedIn), env))!
	assert.equal(res.status, 202)
	const expected = addressFromEmail('user@example.com')
	assert.equal((await res.json()).from, expected)
	assert.equal(db.rows(`SELECT "from" FROM eval_queue`)[0].from, expected)
})

test('세션 검증에 실패하면(token 불일치) 세션 주소를 쓰지 않는다', async () => {
	globalThis.fetch = (async () => new Response(JSON.stringify({ token: 'ee'.repeat(32), email: 'user@example.com' }))) as typeof fetch
	const { env, db } = setup(S3)
	const res = (await handleEvaluations(post('/evaluations', await postBody({ from: CLIENT_FROM }), loggedIn), env))!
	assert.equal(res.status, 202)
	assert.equal(db.rows(`SELECT "from" FROM eval_queue`)[0].from, CLIENT_FROM)
})

test('S3 설정이 없으면 세션 헤더가 있어도 기존처럼 동작한다 (S3 호출 없음)', async () => {
	const { env, db } = setup()
	const res = (await handleEvaluations(post('/evaluations', await postBody(), loggedIn), env))!
	assert.equal(res.status, 202)
	assert.match(String(db.rows(`SELECT "from" FROM eval_queue`)[0].from), /^0x[0-9a-f]{40}$/, '게스트 주소')
})

// ---- 요청 제한 ----

test('저장이 제한에 걸리면 429와 Retry-After를 주고, 대기열에 아무것도 쓰지 않는다', async () => {
	const { env, db, write, read } = setup()
	write.allow = false
	const res = (await handleEvaluations(post('/evaluations', await postBody()), env))!
	assert.equal(res.status, 429)
	assert.equal(res.headers.get('Retry-After'), '60')
	assert.equal(res.headers.get('Access-Control-Allow-Origin'), ORIGIN, '웹뷰가 429를 읽을 수 있어야 한다')
	assert.deepEqual(await res.json(), { error: 'rate limit exceeded' })
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 0)
	assert.deepEqual(write.keys, [IP], 'IP를 키로 쓴다')
	assert.equal(read.keys.length, 0)
})

test('검색은 읽기 제한기를 쓰고, 저장 제한기와 서로 영향을 주지 않는다', async () => {
	const { env, write, read } = setup()
	write.allow = false
	for (const path of ['/evaluations/search', '/public/evaluations/search']) {
		const res = (await handleEvaluations(post(path, { vector: Array.from(unitVector()) }), env))!
		assert.equal(res.status, 200, path)
	}
	assert.equal(read.keys.length, 2)
	assert.equal(write.keys.length, 0)

	read.allow = false
	const limited = (await handleEvaluations(post('/evaluations/search', { vector: Array.from(unitVector()) }), env))!
	assert.equal(limited.status, 429)
})

test('status 조회는 제한하지 않는다', async () => {
	const { env, write, read } = setup()
	write.allow = false
	read.allow = false
	const res = (await handleEvaluations(
		new Request('https://premium.discount/evaluations/status?request_id=req-0123456789abcdef', { headers: { 'cf-connecting-ip': IP } }),
		env
	))!
	assert.equal(res.status, 404, '제한이 아니라 "없음"')
	assert.equal(write.keys.length + read.keys.length, 0)
})

test('제한기가 없거나, IP를 알 수 없거나, 제한기가 고장 나도 요청은 처리된다', async () => {
	const none = setup({ EVAL_WRITE_LIMITER: undefined, EVAL_READ_LIMITER: undefined })
	assert.equal((await handleEvaluations(post('/evaluations', await postBody()), none.env))!.status, 202)

	const noIp = setup()
	noIp.write.allow = false
	const req = new Request('https://premium.discount/evaluations', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json', Origin: ORIGIN },
		body: JSON.stringify(await postBody({ from: CLIENT_FROM })),
	})
	assert.equal((await handleEvaluations(req, noIp.env))!.status, 202)
	assert.equal(noIp.write.keys.length, 0)

	const broken = setup()
	broken.write.fail = true
	assert.equal((await handleEvaluations(post('/evaluations', await postBody()), broken.env))!.status, 202)
})
