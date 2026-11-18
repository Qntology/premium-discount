// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
// 신구 대체 보고 (POST /evaluations/supersedes, migrations/0009) → cron 이 옛 평가를 discount(−2, superseded)로 판정.
// 비교(premium-discount-relay)는 보고한 쪽이 한다: 여기서는 서버가 검증하는 조건과 판정까지의 연결만 본다.
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { handleEvaluations } from '../src/evaluations.ts'
import { addressFromEmail } from '../src/auth.ts'
import { processVerdicts } from '../../cron/src/index.ts'
import { FakeD1, FakeVectorize, FakeLimiter, seedEvaluation, seedVote } from '../../test-support/fakes.ts'

const ORIGIN = 'tauri://localhost'
const IP = '203.0.113.7'
const S3 = {
	SESSION_S3_BUCKET: 'oauth-sessions',
	SESSION_S3_REGION: 'ap-northeast-2',
	SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST',
	SESSION_S3_SECRET_ACCESS_KEY: 'test-secret',
}
const loggedIn = { 'x-oauth-hash': 'ab'.repeat(20), 'x-oauth-token': 'cd'.repeat(32) }
const OLD = '0190f0a0-0000-7000-8000-0000000000b1'
const NEW = '0190f0a0-0000-7000-8000-0000000000b2'
const ELSEWHERE = '0190f0a0-0000-7000-8000-0000000000b3'
const OTHER_TARGET = '0190f0a0-0000-7000-8000-0000000000b4'
const AUTHOR = addressFromEmail('author@example.com')
const TO = '0x' + 'c'.repeat(40)
const realFetch = globalThis.fetch
let s3Email: string | null

beforeEach(() => {
	s3Email = null
	globalThis.fetch = (async () =>
		s3Email ? new Response(JSON.stringify({ token: 'cd'.repeat(32), email: s3Email })) : new Response('', { status: 404 })) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup() {
	const db = new FakeD1()
	const env = {
		DB: db,
		EVAL_VECTORIZE: new FakeVectorize(),
		GUEST_SALT: 'salt',
		AUTHOR_KEY_SECRET: 'secret',
		EVAL_WRITE_LIMITER: new FakeLimiter(),
		EVAL_READ_LIMITER: new FakeLimiter(),
		...S3,
	}
	return { db, env: env as unknown as Parameters<typeof handleEvaluations>[1] }
}
type Env = ReturnType<typeof setup>['env']

// 같은 장소 · 대상의 옛 평가(1,000)와 새 평가(2,000): "배송비 3,000원" → "배송비 무료". 다른 장소 · 다른 대상의 평가 하나씩
async function seedPair(db: FakeD1) {
	await seedEvaluation(db, { id: OLD, from: AUTHOR, to: TO, created_at: 1_000 })
	await seedEvaluation(db, { id: NEW, from: addressFromEmail('other@example.com'), to: TO, created_at: 2_000 })
	await seedEvaluation(db, { id: ELSEWHERE, cc: 'elsewhere.com', to: TO, created_at: 3_000 })
	await seedEvaluation(db, { id: OTHER_TARGET, to: null, created_at: 3_000 })
}

const report = (over: Record<string, unknown> = {}) => ({ request_id: 'supersede-0123456789', old: OLD, new: NEW, ...over })
async function send(env: Env, body: unknown, headers: Record<string, string> = loggedIn) {
	const req = new Request('https://premium.discount/evaluations/supersedes', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json', Origin: ORIGIN, 'cf-connecting-ip': IP, ...headers },
		body: typeof body === 'string' ? body : JSON.stringify(body),
	})
	const res = (await handleEvaluations(req, env))!
	return { status: res.status, body: await res.json() }
}

test('검증: 형식 · 장소 · 대상 · 시각이 틀리면 400, 없는 평가는 404, 아무것도 쓰지 않는다', async () => {
	const { env, db } = setup()
	await seedPair(db)
	s3Email = 'author@example.com'
	const bad: [string, unknown][] = [
		['request_id 없음', report({ request_id: undefined })],
		['old 가 UUID 아님', report({ old: 'abc' })],
		['같은 평가', report({ new: OLD })],
		['다른 장소', report({ new: ELSEWHERE })],
		['다른 대상', report({ new: OTHER_TARGET })],
		['새 평가가 더 이르다', report({ old: NEW, new: OLD })],
		['JSON 아님', '{nope'],
	]
	for (const [name, body] of bad) {
		const r = await send(env, body)
		assert.equal(r.status, 400, name)
		assert.equal(typeof r.body.error, 'string', name)
	}
	const missing = await send(env, report({ old: '0190f0a0-0000-7000-8000-0000000000ff' }))
	assert.deepEqual([missing.status, missing.body.error], [404, 'evaluation not found'])
	assert.equal(db.rows(`SELECT * FROM eval_supersedes`).length, 0)
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)
})

test('보고자는 확인된 로그인이고 두 평가 중 하나의 작성자여야 한다 (게스트 401, 남 403)', async () => {
	const { env, db } = setup()
	await seedPair(db)
	assert.equal((await send(env, report(), {})).status, 401)
	s3Email = 'stranger@example.com'
	assert.equal((await send(env, report())).status, 403)
	assert.equal(db.rows(`SELECT * FROM eval_supersedes`).length, 0)
})

test('작성자의 보고는 기록되고 옛 평가가 다시 판정 대기에 오른다. 같은 request_id 는 같은 답, 다른 쌍이면 409', async () => {
	const { env, db } = setup()
	await seedPair(db)
	s3Email = 'author@example.com'
	const first = await send(env, report())
	assert.deepEqual([first.status, first.body], [202, { old: OLD, new: NEW, status: 'accepted' }])
	assert.deepEqual(db.rows(`SELECT "old_id", "new_id", "reporter" FROM eval_supersedes`), [{ old_id: OLD, new_id: NEW, reporter: AUTHOR }])
	assert.deepEqual(db.rows(`SELECT "eval_id" FROM verdict_dirty`), [{ eval_id: OLD }])
	const again = await send(env, report())
	assert.deepEqual([again.status, again.body], [200, { old: OLD, new: NEW, status: 'accepted' }])
	const conflict = await send(env, report({ new: OLD, old: NEW }))
	assert.equal(conflict.status, 400, 'reversed order is rejected before the replay check')
	await seedEvaluation(db, { id: '0190f0a0-0000-7000-8000-0000000000b5', from: AUTHOR, to: TO, created_at: 4_000 })
	const reused = await send(env, report({ new: '0190f0a0-0000-7000-8000-0000000000b5' }))
	assert.equal(reused.status, 409)
	assert.equal(db.rows(`SELECT * FROM eval_supersedes`).length, 1)
})

test('cron: 대체된 평가는 표가 없어도, 추천이 많아도 discount (grade -2, 이유 superseded). 대체한 평가는 그대로', async () => {
	const { env, db } = setup()
	await seedPair(db)
	const now = 10_000_000
	let voter = 0x200
	for (const id of [OLD, NEW]) {
		for (let i = 0; i < 4; i++) {
			await seedVote(db, { eval_id: id, voter: '0x' + (voter++).toString(16).padStart(40, '0'), value: 1, created_at: now - 1_000 })
		}
	}
	s3Email = 'author@example.com'
	assert.equal((await send(env, report())).status, 202)
	await processVerdicts(env as unknown as Parameters<typeof processVerdicts>[0], now)
	const v = (id: string) => db.rows(`SELECT "tier", "grade", "reasons" FROM eval_verdicts WHERE "eval_id" = ?1`, id)[0] as { tier: string; grade: number; reasons: string }
	const old = v(OLD)
	assert.deepEqual([old.tier, old.grade], ['discount', -2])
	assert.equal(JSON.parse(old.reasons)[0], 'superseded')
	const newer = v(NEW)
	assert.notEqual(newer.tier, 'discount')
	assert.ok(!JSON.parse(newer.reasons).includes('superseded'))

	// 표가 하나도 없는 옛 평가도 보고만으로 판정 행이 생겨 discount 가 된다
	const LONE_OLD = '0190f0a0-0000-7000-8000-0000000000c1'
	const LONE_NEW = '0190f0a0-0000-7000-8000-0000000000c2'
	await seedEvaluation(db, { id: LONE_OLD, from: AUTHOR, cc: 'lone.com', created_at: 1_000 })
	await seedEvaluation(db, { id: LONE_NEW, from: AUTHOR, cc: 'lone.com', created_at: 2_000 })
	assert.equal((await send(env, report({ request_id: 'supersede-lone-0000', old: LONE_OLD, new: LONE_NEW }))).status, 202)
	await processVerdicts(env as unknown as Parameters<typeof processVerdicts>[0], now + 1)
	const lone = v(LONE_OLD)
	assert.deepEqual([lone.tier, lone.grade], ['discount', -2])
})
