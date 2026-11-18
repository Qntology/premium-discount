// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/www/tests/*.test.ts  (premium-discount 폴더에서)
// 투표(POST /evaluations/votes), 판정 조회(GET /evaluations/verdicts), 검색의 등급 조건(tier)과 verdict 필드.
// D1은 실제 SQLite(node:sqlite)에 migrations 0002~0006을 적용한 가짜다. 세션 확인(S3)은 globalThis.fetch로 흉내 낸다.
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { ethers } from 'ethers'
import { handleEvaluations } from '../src/evaluations.ts'
import { addressFromEmail } from '../src/auth.ts'
import { RECIPE } from '../../cron/src/verdict.ts'
import { FakeD1, FakeVectorize, FakeLimiter, unitVector, seedEvaluation, seedQueue, seedVerdict, PARENT_ID } from '../../../test-support/fakes.ts'

const ORIGIN = 'tauri://localhost'
const IP = '203.0.113.7'
const HASH = 'ab'.repeat(20)
const TOKEN = 'cd'.repeat(32)
const CLIENT_FROM = '0x1111111111111111111111111111111111111111'
const EVAL = '0190f0a0-0000-7000-8000-0000000000a1'
const OTHER = '0190f0a0-0000-7000-8000-0000000000a2'
const S3 = {
	SESSION_S3_BUCKET: 'oauth-sessions',
	SESSION_S3_REGION: 'ap-northeast-2',
	SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST',
	SESSION_S3_SECRET_ACCESS_KEY: 'test-secret',
}
const loggedIn = { 'x-oauth-hash': HASH, 'x-oauth-token': TOKEN }
// evaluations.ts guestAddress와 같은 식 (GUEST_SALT 'salt')
const GUEST = ('0x' + ethers.keccak256(ethers.toUtf8Bytes(`guest|salt|${IP}`)).slice(-40)).toLowerCase()
const realFetch = globalThis.fetch
// 웹 세션의 S3 객체에 들어 있는 email. null이면 객체가 없다 (로그인 아님)
let s3Email: string | null

beforeEach(() => {
	s3Email = null
	globalThis.fetch = (async () =>
		s3Email ? new Response(JSON.stringify({ token: TOKEN, email: s3Email })) : new Response('', { status: 404 })) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup() {
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
		...S3,
	}
	return { db, vec, write, read, env: env as unknown as Parameters<typeof handleEvaluations>[1] }
}
type Env = ReturnType<typeof setup>['env']

function post(path: string, body: unknown, headers: Record<string, string> = {}) {
	return new Request('https://premium.discount' + path, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json', Origin: ORIGIN, 'cf-connecting-ip': IP, ...headers },
		body: typeof body === 'string' ? body : JSON.stringify(body),
	})
}
const get = (path: string) => new Request('https://premium.discount' + path, { headers: { Origin: ORIGIN, 'cf-connecting-ip': IP } })

const ballot = (over: Record<string, unknown> = {}) => ({ request_id: 'vote-0123456789abcdef', id: EVAL, value: 1, ...over })
async function vote(env: Env, body: unknown, headers: Record<string, string> = {}) {
	const res = (await handleEvaluations(post('/evaluations/votes', body, headers), env))!
	return { status: res.status, body: await res.json(), res }
}
async function readVerdicts(env: Env, ids: string) {
	const res = (await handleEvaluations(get('/evaluations/verdicts?ids=' + ids), env))!
	return { status: res.status, body: await res.json() }
}

// ---- 투표 ----

test('투표 입력 검증: 형식이 틀리면 400이고 아무것도 쓰지 않는다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	const cases: [string, unknown][] = [
		['request_id 없음', ballot({ request_id: undefined })],
		['request_id 짧음', ballot({ request_id: 'short' })],
		['id가 UUID 아님', ballot({ id: 'abc' })],
		['id 없음', ballot({ id: undefined })],
		['value 2', ballot({ value: 2 })],
		['value 0.5', ballot({ value: 0.5 })],
		['value 문자열', ballot({ value: '1' })],
		['value true', ballot({ value: true })],
		['value null', ballot({ value: null })],
		['value 없음', ballot({ value: undefined })],
		['kind 모름', ballot({ kind: 'like' })],
		['kind 숫자', ballot({ kind: 1 })],
		['JSON 아님', '{not json'],
		['배열', [ballot()]],
	]
	for (const [name, body] of cases) {
		const r = await vote(env, body)
		assert.equal(r.status, 400, name)
		assert.equal(typeof r.body.error, 'string', name)
	}
	assert.equal(db.rows(`SELECT * FROM eval_votes`).length, 0)
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)
})

test('저장되지 않은 평가(없거나 대기열에만 있음)에는 404 "evaluation not found"', async () => {
	const { env, db } = setup()
	await seedQueue(db, { id: PARENT_ID, text: '대기 중인 글' })
	for (const id of [EVAL, PARENT_ID]) {
		const r = await vote(env, ballot({ id }))
		assert.equal(r.status, 404, id)
		assert.deepEqual(r.body, { error: 'evaluation not found' }, '앱은 이 메시지로 영구 거절을 안다')
		assert.equal(r.res.headers.get('Access-Control-Allow-Origin'), ORIGIN)
	}
	assert.equal(db.rows(`SELECT * FROM eval_votes`).length, 0)
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)
})

test('게스트 투표: 202, counted false, 투표자는 게스트 주소(클라이언트 from 무시), 판정 표시가 생긴다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	const r = await vote(env, ballot({ id: EVAL.toUpperCase(), from: CLIENT_FROM }))
	assert.equal(r.status, 202)
	assert.equal(r.res.headers.get('Access-Control-Allow-Origin'), ORIGIN)
	assert.deepEqual(r.body, { id: EVAL, value: 1, kind: 'vote', counted: false, status: 'accepted' })

	const rows = db.rows(`SELECT * FROM eval_votes`)
	assert.equal(rows.length, 1)
	assert.equal(rows[0].eval_id, EVAL)
	assert.equal(rows[0].voter, GUEST)
	assert.notEqual(rows[0].voter, CLIENT_FROM)
	assert.equal(rows[0].voter_kind, 'guest')
	assert.equal(rows[0].kind, 'vote')
	assert.equal(rows[0].value, 1)
	assert.equal(rows[0].request_id, 'vote-0123456789abcdef')
	assert.match(String(rows[0].id), /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/, 'UUID v7')
	assert.deepEqual(db.rows(`SELECT "eval_id", "at" FROM verdict_dirty`), [{ eval_id: EVAL, at: rows[0].created_at }])
})

test('확인된 로그인 투표는 counted true이고 투표자는 세션 주소다. vote_outcome과 비추천도 받는다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	s3Email = 'voter@example.com'
	const r = await vote(env, ballot({ value: -1, kind: 'vote_outcome', from: CLIENT_FROM }), loggedIn)
	assert.equal(r.status, 202)
	assert.deepEqual(r.body, { id: EVAL, value: -1, kind: 'vote_outcome', counted: true, status: 'accepted' })
	const row = db.rows(`SELECT "voter", "voter_kind", "kind", "value" FROM eval_votes`)[0]
	assert.deepEqual(row, { voter: addressFromEmail('voter@example.com'), voter_kind: 'session', kind: 'vote_outcome', value: -1 })
})

test('세션 확인에 실패하면(S3에 객체 없음) 게스트 표가 된다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	const r = await vote(env, ballot(), loggedIn)
	assert.equal(r.status, 202)
	assert.equal(r.body.counted, false)
	assert.deepEqual(db.rows(`SELECT "voter", "voter_kind" FROM eval_votes`)[0], { voter: GUEST, voter_kind: 'guest' })
})

test('작성자가 자기 평가에 투표하면 기록은 하지만 counted false', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL, from: addressFromEmail('author@example.com') })
	s3Email = 'author@example.com'
	const r = await vote(env, ballot(), loggedIn)
	assert.equal(r.status, 202)
	assert.equal(r.body.counted, false)
	assert.equal(db.rows(`SELECT "voter_kind" FROM eval_votes`)[0].voter_kind, 'session')
})

test('재시도: 같은 request_id와 같은 내용이면 200으로 처음 본문을 그대로 돌려주고, 판정 표시를 다시 남긴다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	s3Email = 'voter@example.com'
	const first = await vote(env, ballot(), loggedIn)
	assert.equal(first.status, 202)
	// 처음 요청이 표만 넣고 표시에 실패한 경우를 흉내 낸다
	await db.prepare(`DELETE FROM verdict_dirty`).run()

	const again = await vote(env, ballot(), loggedIn)
	assert.equal(again.status, 200)
	assert.deepEqual(again.body, first.body)
	assert.equal(db.rows(`SELECT * FROM eval_votes`).length, 1)
	assert.equal(db.rows(`SELECT * FROM verdict_dirty WHERE "eval_id" = ?1`, EVAL).length, 1)
})

test('같은 request_id로 내용(값·종류·평가)이 다르면 409이고 표는 그대로다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	await seedEvaluation(db, { id: OTHER })
	s3Email = 'voter@example.com'
	assert.equal((await vote(env, ballot(), loggedIn)).status, 202)
	const conflicts: [string, unknown, Record<string, string>][] = [
		['값', ballot({ value: -1 }), loggedIn],
		['철회', ballot({ value: 0 }), loggedIn],
		['종류', ballot({ kind: 'vote_outcome' }), loggedIn],
		['평가', ballot({ id: OTHER }), loggedIn],
	]
	for (const [name, body, headers] of conflicts) {
		const r = await vote(env, body, headers)
		assert.equal(r.status, 409, name)
		assert.match(r.body.error, /request_id/, name)
	}
	assert.deepEqual(db.rows(`SELECT "eval_id", "value", "kind" FROM eval_votes`), [{ eval_id: EVAL, value: 1, kind: 'vote' }])

	// 재시도 사이에 세션 확인이 실패해 게스트로 떨어져도 같은 요청이다: 처음 저장한 표(로그인 투표자)로 답한다
	const retried = await vote(env, ballot(), {})
	assert.equal(retried.status, 200)
	assert.equal(retried.body.counted, true, 'counted는 처음 저장한 투표자 기준')
	assert.deepEqual(db.rows(`SELECT "voter_kind" FROM eval_votes`), [{ voter_kind: 'session' }])
})

test('표는 덮어쓰지 않고 쌓인다: 다른 request_id로 바꾸거나 철회(0)하면 행이 늘고 표시 시각이 늦춰진다', async () => {
	const { env, db } = setup()
	await seedEvaluation(db, { id: EVAL })
	assert.equal((await vote(env, ballot())).status, 202)
	const firstAt = Number(db.rows(`SELECT "at" FROM verdict_dirty`)[0].at)
	await new Promise((r) => setTimeout(r, 5))
	const r = await vote(env, ballot({ request_id: 'vote-withdraw-0123456789', value: 0 }))
	assert.equal(r.status, 202)
	assert.equal(r.body.value, 0)
	assert.deepEqual(db.rows(`SELECT "value" FROM eval_votes ORDER BY "created_at", "id"`).map((x) => x.value), [1, 0])
	const dirty = db.rows(`SELECT "at" FROM verdict_dirty`)
	assert.equal(dirty.length, 1)
	assert.ok(Number(dirty[0].at) > firstAt)
})

test('투표는 저장 제한기를 쓴다: 걸리면 429이고 아무것도 쓰지 않는다. 읽기 제한기와는 따로다', async () => {
	const { env, db, write, read } = setup()
	await seedEvaluation(db, { id: EVAL })
	write.allow = false
	const limited = await vote(env, ballot())
	assert.equal(limited.status, 429)
	assert.equal(limited.res.headers.get('Retry-After'), '60')
	assert.equal(limited.res.headers.get('Access-Control-Allow-Origin'), ORIGIN)
	assert.deepEqual(write.keys, [IP])
	assert.equal(read.keys.length, 0)
	assert.equal(db.rows(`SELECT * FROM eval_votes`).length, 0)

	write.allow = true
	read.allow = false
	assert.equal((await vote(env, ballot())).status, 202)
})

test('투표 경로의 메서드와 사전 요청', async () => {
	const { env } = setup()
	assert.equal((await handleEvaluations(get('/evaluations/votes'), env))!.status, 405)
	const pre = (await handleEvaluations(new Request('https://premium.discount/evaluations/votes', { method: 'OPTIONS', headers: { Origin: ORIGIN } }), env))!
	assert.equal(pre.status, 204)
	assert.equal(pre.headers.get('Access-Control-Allow-Origin'), ORIGIN)
	assert.equal(await handleEvaluations(get('/evaluations/vote'), env), null, '다른 경로는 넘긴다')
})

// ---- 판정 조회 ----

const A = '0190f0a0-0000-7000-8000-00000000000a'
const B = '0190f0a0-0000-7000-8000-00000000000b'
const C = '0190f0a0-0000-7000-8000-00000000000c'
const D = '0190f0a0-0000-7000-8000-00000000000d'
const X = '0190f0a0-0000-7000-8000-0000000000ff'

test('판정 조회: 요청 순서대로, 중복은 한 번만, 판정 행이 없으면 unrated 기본값, reasons는 배열', async () => {
	const { env, db } = setup()
	await seedVerdict(db, { eval_id: A, tier: 'premium', grade: 2, rank: 0.9, evidence: 3, value: 0.6, up: 3, level: 'secondary', reasons: ['rank_high', 'level:secondary'], updated_at: 5_000 })
	await seedVerdict(db, { eval_id: B, tier: 'discount', grade: -1, rank: -0.6, evidence: 2.5, value: -0.4, down: 2, up: 1, level: 'primary', reasons: ['rank_low', 'level:primary'] })
	const r = await readVerdicts(env, [B, X.toUpperCase(), A, B].join(','))
	assert.equal(r.status, 200)
	assert.deepEqual(r.body, {
		verdicts: [
			{ id: B, tier: 'discount', grade: -1, rank: -0.6, value: -0.4, evidence: 2.5, up: 1, down: 2, level: 'primary', reasons: ['rank_low', 'level:primary'], recipe: RECIPE, updated_at: 1_000 },
			{ id: X, tier: 'unrated', grade: null, rank: null, value: 0, evidence: 0, up: 0, down: 0, level: null, reasons: ['low_evidence', 'no_baseline'], recipe: RECIPE, updated_at: null },
			{ id: A, tier: 'premium', grade: 2, rank: 0.9, value: 0.6, evidence: 3, up: 3, down: 0, level: 'secondary', reasons: ['rank_high', 'level:secondary'], recipe: RECIPE, updated_at: 5_000 },
		],
	})
	assert.equal(RECIPE, 'pd-verdict/1')
})

test('판정 조회 검증: id가 없거나, 50개를 넘거나, UUID가 아니면 400. 50개까지는 된다', async () => {
	const { env } = setup()
	const many = (n: number) => Array.from({ length: n }, (_, i) => `0190f0a0-0000-7000-8000-${String(i).padStart(12, '0')}`).join(',')
	for (const ids of ['', ',', `${A},`, `${A},abc`, many(51)]) {
		const r = await readVerdicts(env, ids)
		assert.equal(r.status, 400, ids.slice(0, 40))
		assert.match(r.body.error, /ids/)
	}
	assert.equal((await handleEvaluations(get('/evaluations/verdicts'), env))!.status, 400, 'ids 매개변수 없음')
	const ok = await readVerdicts(env, many(50))
	assert.equal(ok.status, 200)
	assert.equal(ok.body.verdicts.length, 50)
})

test('판정 조회는 읽기 제한기를 쓰고 GET만 받는다', async () => {
	const { env, write, read } = setup()
	read.allow = false
	const limited = (await handleEvaluations(get('/evaluations/verdicts?ids=' + A), env))!
	assert.equal(limited.status, 429)
	assert.deepEqual(read.keys, [IP])
	assert.equal(write.keys.length, 0)
	assert.equal((await handleEvaluations(post('/evaluations/verdicts', {}), env))!.status, 405)
})

// ---- 검색의 등급 ----

// Vectorize가 돌려줄 순서: D(unrated 행), X(판정 행 없음), A(premium), C(neutral), B(discount)
async function seedSearch(db: FakeD1, vec: FakeVectorize) {
	await seedVerdict(db, { eval_id: A, tier: 'premium', grade: 1, rank: 0.7, evidence: 3 })
	await seedVerdict(db, { eval_id: B, tier: 'discount', grade: -2, rank: -0.9, evidence: 4 })
	await seedVerdict(db, { eval_id: C, tier: 'neutral', grade: 0, rank: 0.1, evidence: 2.5 })
	await seedVerdict(db, { eval_id: D, tier: 'unrated', evidence: 1, reasons: ['low_evidence', 'no_baseline'] })
	const scores = [0.9, 0.8, 0.7, 0.6, 0.5]
	vec.matches = [D, X, A, C, B].map((id, i) => ({ id, score: scores[i], metadata: { cc: 'example.com', root: id, to: '0x' + '2'.repeat(40) } }))
}
const search = async (env: Env, path: string, over: Record<string, unknown> = {}) => {
	const res = (await handleEvaluations(post(path, { vector: Array.from(unitVector()), ...over }), env))!
	return { status: res.status, body: await res.json() }
}

test('검색 결과마다 verdict(tier, grade, rank, evidence)가 붙고, 판정 행이 없으면 null이다', async () => {
	const { env, db, vec } = setup()
	await seedSearch(db, vec)
	const r = await search(env, '/evaluations/search')
	assert.equal(r.status, 200)
	assert.deepEqual(r.body.matches.map((m: { id: string }) => m.id), [D, X, A, C, B])
	const byId = Object.fromEntries(r.body.matches.map((m: { id: string; verdict: unknown }) => [m.id, m.verdict]))
	assert.deepEqual(byId[A], { tier: 'premium', grade: 1, rank: 0.7, evidence: 3 })
	assert.deepEqual(byId[B], { tier: 'discount', grade: -2, rank: -0.9, evidence: 4 })
	assert.deepEqual(byId[D], { tier: 'unrated', grade: null, rank: null, evidence: 1 })
	assert.equal(byId[X], null)
	assert.equal(r.body.matches[0].to, '0x' + '2'.repeat(40), '앱 검색은 to를 그대로 준다')
	assert.equal(vec.queries.at(-1)!.topK, 10, '등급 조건이 없으면 topK 그대로')
})

test('검색의 tier 조건: 세 배(최대 50)를 받아 와 D1 판정으로 거르고 topK로 자른다', async () => {
	const { env, db, vec } = setup()
	await seedSearch(db, vec)
	const ids = (r: { body: { matches: { id: string }[] } }) => r.body.matches.map((m) => m.id)

	const premium = await search(env, '/evaluations/search', { tier: 'premium', topK: 1 })
	assert.equal(vec.queries.at(-1)!.topK, 3)
	assert.deepEqual(ids(premium), [A])
	assert.equal(premium.body.matches[0].verdict.tier, 'premium')

	const discount = await search(env, '/evaluations/search', { tier: 'discount', topK: 2 })
	assert.equal(vec.queries.at(-1)!.topK, 6)
	assert.deepEqual(ids(discount), [B])

	const rated = await search(env, '/evaluations/search', { tier: 'rated', topK: 2 })
	assert.deepEqual(ids(rated), [A, C], '점수 순서를 지키고 topK로 자른다')

	const ratedAll = await search(env, '/evaluations/search', { tier: 'rated', topK: 20 })
	assert.equal(vec.queries.at(-1)!.topK, 50, 'Vectorize 상한 50')
	assert.deepEqual(ids(ratedAll), [A, C, B])
})

test('공개 검색도 verdict와 tier 조건을 받고, to는 내보내지 않는다', async () => {
	const { env, db, vec } = setup()
	await seedSearch(db, vec)
	const r = await search(env, '/public/evaluations/search', { tier: 'premium' })
	assert.equal(r.status, 200)
	assert.deepEqual(r.body.matches, [{ id: A, score: 0.7, cc: 'example.com', root: A, ref: null, verdict: { tier: 'premium', grade: 1, rank: 0.7, evidence: 3 } }])
})

test('tier가 premium·discount·rated가 아니면 400 (앱·공개 모두)', async () => {
	const { env, db, vec } = setup()
	await seedSearch(db, vec)
	for (const path of ['/evaluations/search', '/public/evaluations/search']) {
		for (const tier of ['neutral', 'unrated', 'PREMIUM', '', null, 1, ['premium'], 'constructor']) {
			const r = await search(env, path, { tier })
			assert.equal(r.status, 400, `${path} ${JSON.stringify(tier)}`)
			assert.match(r.body.error, /tier/)
		}
	}
	assert.equal(vec.queries.length, 0, '검증에 걸리면 Vectorize를 부르지 않는다')
})
