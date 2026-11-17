// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
// S3는 globalThis.fetch를 바꿔서 흉내 낸다. 서명(aws4fetch)은 실제로 계산된다.
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { addressFromEmail, readSessionCredentials, verifiedAddress } from '../src/auth.ts'

const HASH = 'ab'.repeat(20)
const TOKEN = 'cd'.repeat(32)
const IP = '203.0.113.7'
const S3 = {
	SESSION_S3_BUCKET: 'oauth-sessions',
	SESSION_S3_REGION: 'ap-northeast-2',
	SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST',
	SESSION_S3_SECRET_ACCESS_KEY: 'test-secret',
}

const realFetch = globalThis.fetch
let s3Calls: { url: string; authorization: string | null }[]
let s3Reply: () => Response

beforeEach(() => {
	s3Calls = []
	s3Reply = () => new Response(JSON.stringify({ token: TOKEN, email: 'user@example.com', ip: IP }), { status: 200 })
	globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
		const req = new Request(input as RequestInfo, init)
		s3Calls.push({ url: req.url, authorization: req.headers.get('authorization') })
		return s3Reply()
	}) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function req(headers: Record<string, string>) {
	return new Request('https://premium.discount/evaluations', { method: 'POST', headers: { 'cf-connecting-ip': IP, ...headers } })
}
const session = { 'x-oauth-hash': HASH, 'x-oauth-token': TOKEN }

test('앱 헤더와 웹 쿠키에서 hash/token을 읽고, 0x와 대문자를 정규화한다', () => {
	assert.deepEqual(readSessionCredentials(req(session)), { hash: HASH, token: TOKEN })
	assert.deepEqual(readSessionCredentials(req({ 'x-oauth-hash': '0x' + HASH.toUpperCase(), 'x-oauth-token': '0x' + TOKEN })), { hash: HASH, token: TOKEN })
	assert.deepEqual(readSessionCredentials(req({ Cookie: `a=1; hash=${HASH}; token=${TOKEN}` })), { hash: HASH, token: TOKEN })
})

test('헤더가 하나라도 있으면 쿠키는 보지 않고, 앱의 익명 자리값과 형식 오류는 세션이 아니다', () => {
	assert.equal(readSessionCredentials(req({ 'x-oauth-hash': HASH, Cookie: `hash=${HASH}; token=${TOKEN}` })), null)
	assert.equal(readSessionCredentials(req({ 'x-oauth-hash': '0x' + '0'.repeat(40), 'x-oauth-token': '0x' + '0'.repeat(64) })), null)
	assert.equal(readSessionCredentials(req({ 'x-oauth-hash': 'zz', 'x-oauth-token': TOKEN })), null)
	assert.equal(readSessionCredentials(req({})), null)
})

test('S3 설정이 없으면 S3를 호출하지 않고 null이다 (기존 동작)', async () => {
	assert.equal(await verifiedAddress(req(session)), null)
	assert.equal(await verifiedAddress(req(session), {}), null)
	for (const missing of Object.keys(S3)) {
		const partial: Record<string, string> = { ...S3 }
		delete partial[missing]
		assert.equal(await verifiedAddress(req(session), partial), null, missing)
	}
	assert.equal(s3Calls.length, 0)
})

test('세션 헤더가 없거나 익명이면 S3를 호출하지 않는다', async () => {
	assert.equal(await verifiedAddress(req({}), S3), null)
	assert.equal(await verifiedAddress(req({ 'x-oauth-hash': '0x' + '0'.repeat(40), 'x-oauth-token': '0x' + '0'.repeat(64) }), S3), null)
	assert.equal(s3Calls.length, 0)
})

test('token·email·ip가 맞으면 이메일에서 만든 주소를 돌려주고, 서명된 GET으로 hash/<hash>를 읽는다', async () => {
	const addr = await verifiedAddress(req(session), S3)
	assert.equal(addr, addressFromEmail('user@example.com'))
	assert.match(addr!, /^0x[0-9a-f]{40}$/)
	assert.equal(s3Calls.length, 1)
	assert.equal(s3Calls[0].url, `https://oauth-sessions.s3.ap-northeast-2.amazonaws.com/hash/${HASH}`)
	assert.match(s3Calls[0].authorization ?? '', /^AWS4-HMAC-SHA256 Credential=AKIATESTTESTTESTTEST\//)
})

test('저장된 token이 0x 접두사나 대문자여도 같은 값이면 통과한다', async () => {
	s3Reply = () => new Response(JSON.stringify({ token: '0x' + TOKEN.toUpperCase(), email: 'user@example.com' }), { status: 200 })
	assert.equal(await verifiedAddress(req(session), S3), addressFromEmail('user@example.com'))
})

test('거부해야 하는 경우는 모두 null이다 (fail-closed)', async () => {
	const cases: [string, () => Response | never][] = [
		['token이 한 글자 다름', () => new Response(JSON.stringify({ token: TOKEN.slice(0, -1) + 'e', email: 'u@e.com' }))],
		['token 없음', () => new Response(JSON.stringify({ email: 'u@e.com' }))],
		['email 없음', () => new Response(JSON.stringify({ token: TOKEN }))],
		['email이 공백', () => new Response(JSON.stringify({ token: TOKEN, email: '  ' }))],
		['ip가 다름', () => new Response(JSON.stringify({ token: TOKEN, email: 'u@e.com', ip: '198.51.100.1' }))],
		['객체 없음(404)', () => new Response('NoSuchKey', { status: 404 })],
		['접근 거부(403)', () => new Response('AccessDenied', { status: 403 })],
		['JSON이 아님', () => new Response('<html>')],
		['JSON null', () => new Response('null')],
		['네트워크 오류', () => { throw new Error('boom') }],
	]
	for (const [name, reply] of cases) {
		s3Reply = reply
		assert.equal(await verifiedAddress(req(session), S3), null, name)
	}
})

test('객체에 ip가 없으면 ip는 비교하지 않는다', async () => {
	s3Reply = () => new Response(JSON.stringify({ token: TOKEN, email: 'user@example.com' }))
	assert.equal(await verifiedAddress(req({ ...session, 'cf-connecting-ip': '198.51.100.99' }), S3), addressFromEmail('user@example.com'))
})
