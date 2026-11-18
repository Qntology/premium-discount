// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
// 인증의 지갑 갈래(익명 계정): 요청 서명 확인. 기준은 contracts/wallet_v1.json(ethers 로 뽑은 값)이고,
// 기기 쪽 Rust(crates/premium-discount-identity)도 같은 계약을 시험한다.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { ethers } from 'ethers'
import { verifyWalletRequest, walletRequestMessage, WALLET_MAX_SKEW_MS } from '../src/auth.ts'

const contract = JSON.parse(readFileSync(fileURLToPath(new URL('../../contracts/wallet_v1.json', import.meta.url)), 'utf8'))
const enc = new TextEncoder()

// 계약의 요청 r 을 Request 로. overrides 로 본문 · 경로 · 헤더를 바꿔 위조를 만든다
function signed(r, overrides = {}) {
	const headers = { 'x-pd-address': r.address, 'x-pd-time': String(r.time_ms), 'x-pd-signature': r.signature, ...(overrides.headers ?? {}) }
	const method = r.method.toUpperCase()
	const body = overrides.body ?? r.body
	const init = { method, headers }
	if (method !== 'GET' && method !== 'HEAD') init.body = body
	return { request: new Request(`https://premium.discount${overrides.path ?? r.path}`, init), body: enc.encode(body) }
}

test('계약과 같은 서명 글을 만든다', async () => {
	assert.equal(contract.max_skew_ms, WALLET_MAX_SKEW_MS)
	for (const r of contract.requests) {
		assert.equal(await walletRequestMessage(r.address, r.time_ms, r.method, r.path, enc.encode(r.body)), r.message)
	}
})

test('계약의 요청 서명을 받고 주소를 돌려준다', async () => {
	for (const r of contract.requests) {
		const { request, body } = signed(r)
		assert.equal(await verifyWalletRequest(request, body, r.time_ms + WALLET_MAX_SKEW_MS), r.address, r.path)
	}
})

test('본문 · 경로 · 주소 · 형식 · 시각이 다르면 받지 않는다', async () => {
	const r = contract.requests[0]
	const other = ethers.Wallet.fromPhrase(contract.wallets[1].phrase).address.toLowerCase()
	const forged = [
		['본문', signed(r, { body: '{"tampered":true}' })],
		['경로', signed(r, { path: '/nav/other' })],
		['다른 주소', signed(r, { headers: { 'x-pd-address': other } })],
		['서명 형식', signed(r, { headers: { 'x-pd-signature': '0x1234' } })],
		['시각 형식', signed(r, { headers: { 'x-pd-time': 'soon' } })],
		['주소 없음', signed(r, { headers: { 'x-pd-address': '' } })],
	]
	for (const [what, { request, body }] of forged) {
		assert.equal(await verifyWalletRequest(request, body, r.time_ms), null, what)
	}
	const { request, body } = signed(r)
	assert.equal(await verifyWalletRequest(request, body, r.time_ms + WALLET_MAX_SKEW_MS + 1), null, '5분이 넘으면')
	assert.equal(await verifyWalletRequest(request, body, r.time_ms - WALLET_MAX_SKEW_MS - 1), null, '5분 앞서도')
})

test('새로 만든 ethers 지갑의 서명도 받는다 (대소문자 섞인 주소 헤더도)', async () => {
	const wallet = ethers.Wallet.createRandom()
	const time = 1763380800000
	const body = '{"runs":[]}'
	const message = await walletRequestMessage(wallet.address, time, 'post', '/nav/trials', enc.encode(body))
	const signature = await wallet.signMessage(message)
	const request = new Request('https://premium.discount/nav/trials?debug=1', {
		method: 'POST',
		headers: { 'x-pd-address': wallet.address, 'x-pd-time': String(time), 'x-pd-signature': signature },
		body,
	})
	assert.equal(await verifyWalletRequest(request, enc.encode(body), time), wallet.address.toLowerCase(), '쿼리는 서명 글에 들어가지 않는다')
})
