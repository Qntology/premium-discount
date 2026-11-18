// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
// 앱 로그인: 발급(POST /session/start), 상태(GET /session), 검증된 주소가 평가 저장의 from이 되는 것까지.
// S3는 globalThis.fetch를 바꿔서 흉내 낸다 (서명은 실제로 계산된다).
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { createHmac } from 'node:crypto'
import { handleSession } from '../src/session.ts'
import { handleEvaluations } from '../src/evaluations.ts'
import { addressFromEmail } from '../src/auth.ts'
import { FakeD1, FakeVectorize, FakeLimiter, unitVector } from '../../test-support/fakes.ts'

const SECRET = 'test-session-secret'
const IP = '203.0.113.7'
const ORIGIN = 'tauri://localhost'
const S3 = {
	SESSION_S3_BUCKET: 'oauth-sessions',
	SESSION_S3_REGION: 'ap-northeast-2',
	SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST',
	SESSION_S3_SECRET_ACCESS_KEY: 'test-secret',
}
const realFetch = globalThis.fetch
let s3Reply: (url: string) => Response
let s3Urls: string[]

beforeEach(() => {
	s3Urls = []
	s3Reply = () => new Response('', { status: 404 })
	globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
		const url = new Request(input as RequestInfo, init).url
		s3Urls.push(url)
		return s3Reply(url)
	}) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup(extra: Record<string, unknown> = {}) {
	const write = new FakeLimiter()
	const read = new FakeLimiter()
	const env = {
		DB: new FakeD1(), EVAL_VECTORIZE: new FakeVectorize(), GUEST_SALT: 'salt', AUTHOR_KEY_SECRET: 'k',
		SESSION_SECRET: SECRET, ...S3, EVAL_WRITE_LIMITER: write, EVAL_READ_LIMITER: read, ...extra,
	}
	return { write, read, env: env as unknown as Parameters<typeof handleSession>[1] }
}

const hmac = (hash: string) => createHmac('sha256', SECRET).update('session|' + hash).digest('hex')
const post = (path: string, headers: Record<string, string> = {}) => new Request('https://premium.discount' + path, { method: 'POST', headers: { Origin: ORIGIN, 'cf-connecting-ip': IP, ...headers } })
const get = (path: string, headers: Record<string, string> = {}) => new Request('https://premium.discount' + path, { headers: { Origin: ORIGIN, 'cf-connecting-ip': IP, ...headers } })

async function start(env: ReturnType<typeof setup>['env']) {
	const res = (await handleSession(post('/session/start'), env))!
	assert.equal(res.status, 201)
	return (await res.json()) as { hash: string; token: string; address: string; mailto: string }
}
const creds = (s: { hash: string; token: string }) => ({ 'x-oauth-hash': s.hash, 'x-oauth-token': s.token })
const status = async (env: ReturnType<typeof setup>['env'], headers: Record<string, string>) => {
	const res = (await handleSession(get('/session', headers), env))!
	return { code: res.status, body: await res.json() }
}

// ---- 발급 ----

test('발급: hash 40자리, token은 HMAC(비밀, "session|"+hash), 웹 QR과 같은 모양의 mailto', async () => {
	const { env } = setup()
	const s = await start(env)
	assert.match(s.hash, /^[0-9a-f]{40}$/)
	assert.match(s.token, /^[0-9a-f]{64}$/)
	assert.equal(s.token, hmac(s.hash), '서버 밖에서 따로 계산한 HMAC과 같다')
	assert.equal(s.address, `${s.hash}.premium.discount@oauth.email`)
	assert.equal(s.mailto, 'mailto:' + encodeURIComponent(s.address), 'auth.js의 QR 문자열과 같은 규칙')
	assert.notEqual((await start(env)).hash, s.hash, '발급할 때마다 다르다')
})

test('발급 응답은 캐시되지 않고(token이 들어 있다), CORS 헤더가 붙는다', async () => {
	const { env } = setup()
	const res = (await handleSession(post('/session/start'), env))!
	assert.equal(res.headers.get('Cache-Control'), 'no-store')
	assert.equal(res.headers.get('Access-Control-Allow-Origin'), ORIGIN)
})

test('메일 호스트는 설정으로 바꿀 수 있다', async () => {
	const { env } = setup({ SESSION_MAIL_HOST: 'example.test' })
	assert.match((await start(env)).address, /\.example\.test@oauth\.email$/)
})

test('SESSION_SECRET이 없으면 503이다 (발급하지 않는다)', async () => {
	const { env } = setup({ SESSION_SECRET: undefined })
	const res = (await handleSession(post('/session/start'), env))!
	assert.equal(res.status, 503)
	assert.deepEqual(await res.json(), { error: 'login is not configured' })
})

// ---- 상태 ----

test('헤더가 없거나 익명 자리값이면 anonymous이고 S3를 읽지 않는다', async () => {
	const { env } = setup()
	assert.deepEqual(await status(env, {}), { code: 200, body: { state: 'anonymous', address: null } })
	const zero = { 'x-oauth-hash': '0x' + '0'.repeat(40), 'x-oauth-token': '0x' + '0'.repeat(64) }
	assert.equal((await status(env, zero)).body.state, 'anonymous')
	assert.equal(s3Urls.length, 0)
})

test('발급받았지만 메일이 아직 없으면 pending (S3의 404도 403도 "아직 없음")', async () => {
	const { env } = setup()
	const s = await start(env)
	for (const code of [404, 403]) {
		s3Reply = () => new Response('', { status: code })
		assert.deepEqual(await status(env, creds(s)), { code: 200, body: { state: 'pending', address: null } }, String(code))
	}
	s3Reply = () => new Response(JSON.stringify({}), { status: 200 })
	assert.equal((await status(env, creds(s))).body.state, 'pending', 'email 없는 객체도 pending')
	assert.equal(s3Urls[0], `https://oauth-sessions.s3.ap-northeast-2.amazonaws.com/hash/${s.hash}`)
})

test('메일이 도착해 S3에 email이 생기면 signed_in이고 주소는 이메일에서 만든다', async () => {
	const { env } = setup()
	const s = await start(env)
	s3Reply = () => new Response(JSON.stringify({ email: 'User@Example.com' }), { status: 200 })
	const r = await status(env, creds(s))
	assert.equal(r.code, 200)
	assert.deepEqual(r.body, { state: 'signed_in', address: addressFromEmail('User@Example.com') })
})

test('앱 세션은 ip를 비교하지 않는다 (메일은 다른 기기에서 보낸다)', async () => {
	const { env } = setup()
	const s = await start(env)
	s3Reply = () => new Response(JSON.stringify({ email: 'u@e.com', ip: '198.51.100.1' }), { status: 200 })
	assert.equal((await status(env, creds(s))).body.state, 'signed_in')
})

test('token이 한 글자라도 다르면 S3에 email이 있어도 anonymous이다', async () => {
	const { env } = setup()
	const s = await start(env)
	s3Reply = () => new Response(JSON.stringify({ email: 'u@e.com' }), { status: 200 })
	const bad = { 'x-oauth-hash': s.hash, 'x-oauth-token': s.token.slice(0, -1) + (s.token.endsWith('0') ? '1' : '0') }
	assert.equal((await status(env, bad)).body.state, 'anonymous')
	// 다른 비밀로 발급된 것처럼 보이는 token도 마찬가지다
	assert.equal((await status(env, { 'x-oauth-hash': s.hash, 'x-oauth-token': 'ab'.repeat(32) })).body.state, 'anonymous')
})

test('발급한 적 없는 hash는 S3에 email이 있어도 로그인이 아니다 (hash만 알아서는 안 된다)', async () => {
	const { env } = setup()
	s3Reply = () => new Response(JSON.stringify({ email: 'victim@e.com' }), { status: 200 })
	const hash = 'cd'.repeat(20)
	assert.equal((await status(env, { 'x-oauth-hash': hash, 'x-oauth-token': 'ef'.repeat(32) })).body.state, 'anonymous')
})

test('웹 세션(S3의 token 일치)은 그대로 로그인된다', async () => {
	const { env } = setup()
	const web = { 'x-oauth-hash': 'ab'.repeat(20), 'x-oauth-token': 'cd'.repeat(32) }
	s3Reply = () => new Response(JSON.stringify({ token: 'cd'.repeat(32), email: 'web@e.com' }), { status: 200 })
	assert.deepEqual((await status(env, web)).body, { state: 'signed_in', address: addressFromEmail('web@e.com') })
	s3Reply = () => new Response(JSON.stringify({ token: 'ee'.repeat(32), email: 'web@e.com' }), { status: 200 })
	assert.equal((await status(env, web)).body.state, 'anonymous')
})

test('S3 오류나 설정 누락은 로그인으로 보지 않고 503 unavailable이다', async () => {
	const { env } = setup()
	const s = await start(env)
	s3Reply = () => new Response('', { status: 500 })
	assert.deepEqual(await status(env, creds(s)), { code: 503, body: { state: 'unavailable', address: null } })
	globalThis.fetch = (async () => { throw new Error('network') }) as typeof fetch
	assert.equal((await status(env, creds(s))).code, 503)
	const noS3 = setup({ SESSION_S3_BUCKET: undefined })
	assert.equal((await status(noS3.env, creds(s))).code, 503)
})

// ---- 제한과 CORS ----

test('발급은 저장 제한기, 상태는 읽기 제한기를 쓰고 걸리면 429이다', async () => {
	const { env, write, read } = setup()
	write.allow = false
	const blocked = (await handleSession(post('/session/start'), env))!
	assert.equal(blocked.status, 429)
	assert.equal(blocked.headers.get('Retry-After'), '60')
	assert.deepEqual(write.keys, [IP])
	read.allow = false
	assert.equal((await handleSession(get('/session', { 'x-oauth-hash': 'ab'.repeat(20), 'x-oauth-token': 'cd'.repeat(32) }), env))!.status, 429)
	assert.equal(s3Urls.length, 0, '제한에 걸리면 S3를 읽지 않는다')
})

test('사전 요청과 메서드 검사', async () => {
	const { env } = setup()
	const pre = (await handleSession(new Request('https://premium.discount/session', { method: 'OPTIONS', headers: { Origin: ORIGIN } }), env))!
	assert.equal(pre.status, 204)
	assert.match(pre.headers.get('Access-Control-Allow-Headers')!.toLowerCase(), /x-oauth-hash/)
	assert.equal((await handleSession(get('/session/start'), env))!.status, 405)
	assert.equal((await handleSession(post('/session'), env))!.status, 405)
	assert.equal(await handleSession(get('/sessions'), env), null, '다른 경로는 넘긴다')
})

// ---- 평가 저장과 연결 ----

test('로그인한 앱이 저장하면 from이 로그인한 이메일에서 만든 주소다 (클라이언트 값은 무시)', async () => {
	const { env } = setup()
	const s = await start(env)
	s3Reply = () => new Response(JSON.stringify({ email: 'app@e.com' }), { status: 200 })
	const text = '로그인한 앱의 글'
	const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))
	const text_hash = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('')
	const res = (await handleEvaluations(
		new Request('https://premium.discount/evaluations', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', Origin: ORIGIN, 'cf-connecting-ip': IP, ...creds(s) },
			body: JSON.stringify({ request_id: 'req-login-0123456789', cc: 'example.com', text, text_hash, vector: Array.from(unitVector()), from: '0x1111111111111111111111111111111111111111' }),
		}),
		env
	))!
	assert.equal(res.status, 202)
	assert.equal((await res.json()).from, addressFromEmail('app@e.com'))
})
