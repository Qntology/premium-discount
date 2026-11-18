// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test cron/test/*.test.ts  (premium-discount 폴더에서)
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { moderate, parseModeration, QUESTIONS, THRESHOLD } from '../src/moderation.ts'

const ENV = { DASHSCOPE_API_KEY: 'test-key', DECISION_ENDPOINT: 'https://decision.example/systemone' }

function answers(p: { profanity?: number; nsfw?: number; illegal?: number }) {
	const out: Record<string, unknown> = {}
	for (const [k, v] of Object.entries({ profanity: 0, nsfw: 0, illegal: 0, ...p })) out[k] = { type: 'noul', noul: v }
	return { answers: out }
}

test('세 질문이 모두 임계값 미만이면 clean', () => {
	assert.deepEqual(parseModeration(answers({})), { verdict: 'clean', flagged: [] })
	assert.equal(parseModeration(answers({ profanity: THRESHOLD - 0.001 })).verdict, 'clean')
})

test('질문 하나라도 임계값 이상이면 profane이고 걸린 질문이 모두 나온다', () => {
	assert.deepEqual(parseModeration(answers({ nsfw: THRESHOLD })), { verdict: 'profane', flagged: ['nsfw'] })
	assert.deepEqual(parseModeration(answers({ illegal: 0.9 })), { verdict: 'profane', flagged: ['illegal'] })
	assert.deepEqual(parseModeration(answers({ profanity: 0.6, illegal: 0.7 })).flagged, ['profanity', 'illegal'])
})

test('보낸 질문의 답이 하나라도 빠지면 통과시키지 않고 예외다 (fail-closed)', () => {
	for (const missing of ['profanity', 'nsfw', 'illegal']) {
		const body = answers({})
		delete (body.answers as Record<string, unknown>)[missing]
		assert.throws(() => parseModeration(body), /unrecognized/, missing)
	}
})

test('형태가 다른 응답은 예외다', () => {
	for (const bad of [null, {}, { answers: null }, { answers: 'x' }, { answers: { profanity: { type: 'noul', noul: 'high' } } }]) {
		assert.throws(() => parseModeration(bad), /unrecognized/)
	}
	const nan = answers({})
	;(nan.answers as Record<string, { noul: number }>).nsfw.noul = NaN
	assert.throws(() => parseModeration(nan), /unrecognized/)
	const wrongType = answers({})
	;(wrongType.answers as Record<string, { type: string }>).illegal.type = 'text'
	assert.throws(() => parseModeration(wrongType), /unrecognized/)
})

test('한 번의 호출에 세 질문을 모두 담아 보낸다', async () => {
	const calls: { url: string; init: RequestInit }[] = []
	const realFetch = globalThis.fetch
	globalThis.fetch = (async (url: string, init: RequestInit) => {
		calls.push({ url, init })
		return new Response(JSON.stringify(answers({})), { status: 200 })
	}) as typeof fetch
	try {
		const result = await moderate(ENV, '안녕하세요')
		assert.equal(result.verdict, 'clean')
	} finally {
		globalThis.fetch = realFetch
	}
	assert.equal(calls.length, 1)
	assert.equal(calls[0].url, ENV.DECISION_ENDPOINT)
	const body = JSON.parse(String(calls[0].init.body))
	assert.equal(body.model, 'decision-model-preview')
	assert.equal(body.state, '안녕하세요')
	assert.deepEqual(Object.keys(body.questions).sort(), ['illegal', 'nsfw', 'profanity'])
	for (const [name, q] of Object.entries(body.questions) as [string, { type: string; instructions: string }][]) {
		assert.equal(q.type, 'noul')
		assert.equal(q.instructions, QUESTIONS[name as keyof typeof QUESTIONS])
	}
	assert.equal((calls[0].init.headers as Record<string, string>).Authorization, 'Bearer test-key')
})

test('HTTP 오류는 상태 코드만 담은 예외다 (원문 미포함)', async () => {
	const realFetch = globalThis.fetch
	globalThis.fetch = (async () => new Response('원문을 되돌려 주는 오류 본문', { status: 503 })) as typeof fetch
	try {
		await assert.rejects(moderate(ENV, '비밀 원문'), (e: Error) => e.message === 'decision api responded 503')
	} finally {
		globalThis.fetch = realFetch
	}
})
