// cron 대기열 처리 테스트. D1은 실제 SQLite(node:sqlite)이고 마이그레이션 0002~0005를 그대로 적용한다.
// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/cron/test/*.test.ts  (premium-discount 폴더에서)
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { processQueue, MASKED_HASH } from '../src/index.ts'
import { FakeD1, FakeVectorize, seedQueue, PARENT_ID, REPLY_ID } from '../../../test-support/fakes.ts'

const T0 = 1_000_000
const MIN = 60_000

type Scores = { profanity?: number; nsfw?: number; illegal?: number }
let fetchCalls: { text: string; questions: string[] }[]
// 텍스트별 판정. 함수가 throw하면 API 오류다
let judge: (text: string) => Scores | 'down' | 'malformed'
const realFetch = globalThis.fetch

beforeEach(() => {
	fetchCalls = []
	judge = () => ({})
	globalThis.fetch = (async (_url: string, init: RequestInit) => {
		const body = JSON.parse(String(init.body))
		fetchCalls.push({ text: body.state, questions: Object.keys(body.questions) })
		const r = judge(body.state)
		if (r === 'down') return new Response('', { status: 503 })
		if (r === 'malformed') return new Response(JSON.stringify({ answers: { profanity: { type: 'noul', noul: 0 } } }), { status: 200 })
		const answers: Record<string, unknown> = {}
		for (const k of ['profanity', 'nsfw', 'illegal'] as const) answers[k] = { type: 'noul', noul: r[k] ?? 0 }
		return new Response(JSON.stringify({ answers }), { status: 200 })
	}) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup() {
	const db = new FakeD1()
	const vec = new FakeVectorize()
	const env = { DB: db, EVAL_VECTORIZE: vec, DASHSCOPE_API_KEY: 'k', DECISION_ENDPOINT: 'https://decision.example' }
	return { db, vec, env: env as unknown as Parameters<typeof processQueue>[0] }
}

const queueRow = (db: FakeD1, id: string) => db.rows(`SELECT * FROM eval_queue WHERE "id" = ?1`, id)[0]

test('깨끗한 글은 evaluations와 Vectorize에 저장되고, 대기열 행(원문·해시 포함)은 지워진다', async () => {
	const { db, vec, env } = setup()
	await seedQueue(db, { id: PARENT_ID, text: '좋은 평가입니다' })
	const res = await processQueue(env, T0)
	assert.equal(res.processed, 1)

	const stored = db.rows(`SELECT * FROM evaluations`)
	assert.equal(stored.length, 1)
	assert.equal(stored[0].id, PARENT_ID)
	assert.equal(stored[0].bcc, '0x' + 'b'.repeat(40))
	// evaluations에는 원문과 해시를 담는 컬럼이 아예 없다
	assert.ok(!('text' in stored[0]) && !('text_hash' in stored[0]))
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 0)

	const v = vec.store.get(PARENT_ID)!
	assert.equal(v.values.length, 768)
	assert.deepEqual(v.metadata, { cc: 'example.com', root: PARENT_ID })
})

test('욕설: rejected가 되고 원문·벡터·text_hash·bcc가 모두 지워진다', async () => {
	const { db, vec, env } = setup()
	judge = () => ({ profanity: 0.97 })
	await seedQueue(db, { id: PARENT_ID, text: '나쁜 말' })
	await processQueue(env, T0)

	const row = queueRow(db, PARENT_ID)
	assert.equal(row.state, 'rejected')
	assert.equal(row.last_error, 'profanity')
	assert.equal(row.text, null)
	assert.equal(row.vector, null)
	assert.equal(row.bcc, null)
	assert.equal(row.text_hash, MASKED_HASH)
	assert.notEqual(row.text_hash, 'a'.repeat(64), '원래 해시가 남아 있으면 안 된다')
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 0)
	assert.equal(vec.store.size, 0)
})

test('음란·불법 질문도 거부 사유가 되고, 걸린 질문 이름이 모두 기록된다', async () => {
	const { db, env } = setup()
	judge = (t) => (t === 'a' ? { nsfw: 0.8 } : t === 'b' ? { illegal: 0.6 } : { nsfw: 0.5, illegal: 0.9 })
	await seedQueue(db, { id: '0190f0a0-0000-7000-8000-00000000000a', text: 'a', created_at: 1 })
	await seedQueue(db, { id: '0190f0a0-0000-7000-8000-00000000000b', text: 'b', created_at: 2 })
	await seedQueue(db, { id: '0190f0a0-0000-7000-8000-00000000000c', text: 'c', created_at: 3 })
	await processQueue(env, T0)
	const reasons = db.rows(`SELECT "last_error" FROM eval_queue ORDER BY "created_at"`).map((r) => r.last_error)
	assert.deepEqual(reasons, ['nsfw', 'illegal', 'nsfw,illegal'])
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 0)
})

test('글 한 건에 결정 API는 한 번만 호출하고 세 질문을 함께 보낸다', async () => {
	const { db, env } = setup()
	await seedQueue(db, { id: PARENT_ID, text: '하나' })
	await processQueue(env, T0)
	assert.equal(fetchCalls.length, 1)
	assert.deepEqual(fetchCalls[0].questions.sort(), ['illegal', 'nsfw', 'profanity'])
})

test('응답이 일부 질문만 담고 있으면 통과시키지 않고 재시도로 남긴다 (fail-closed)', async () => {
	const { db, vec, env } = setup()
	judge = () => 'malformed'
	await seedQueue(db, { id: PARENT_ID, text: '글' })
	await processQueue(env, T0)
	const row = queueRow(db, PARENT_ID)
	assert.equal(row.state, 'pending')
	assert.equal(row.attempts, 1)
	assert.match(String(row.last_error), /unrecognized/)
	assert.equal(row.text, '글', '재시도할 수 있게 원문은 아직 남아 있다')
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 0)
	assert.equal(vec.store.size, 0)
})

test('재시도 한도(10회)에 닿으면 failed가 되고 원문·벡터·text_hash·bcc가 지워진다', async () => {
	const { db, env } = setup()
	judge = () => 'down'
	await seedQueue(db, { id: PARENT_ID, text: '글' })
	for (let i = 0; i < 10; i++) await processQueue(env, T0 + i * MIN)
	const row = queueRow(db, PARENT_ID)
	assert.equal(row.state, 'failed')
	assert.equal(row.attempts, 10)
	assert.equal(row.text, null)
	assert.equal(row.vector, null)
	assert.equal(row.bcc, null)
	assert.equal(row.text_hash, MASKED_HASH)
	assert.match(String(row.last_error), /503/)
	assert.ok(!String(row.last_error).includes('글'), '오류 메시지에 원문이 들어가지 않는다')
})

test('부모가 아직 대기열에 있으면 답글은 저장하지 않고 횟수도 세지 않는다. 부모가 끝난 뒤 순서대로 저장된다', async () => {
	const { db, vec, env } = setup()
	await seedQueue(db, { id: PARENT_ID, text: '부모 글', created_at: 1 })
	await seedQueue(db, { id: REPLY_ID, text: '답글', ref: PARENT_ID, root: PARENT_ID, created_at: 2 })

	// 1분차: 부모의 결정 API가 일시 오류다 → 부모는 재시도, 답글은 부모를 기다린다
	judge = (t) => (t === '부모 글' ? 'down' : {})
	await processQueue(env, T0)
	assert.equal(queueRow(db, PARENT_ID).state, 'pending')
	assert.equal(queueRow(db, PARENT_ID).attempts, 1)
	const waiting = queueRow(db, REPLY_ID)
	assert.equal(waiting.state, 'pending')
	assert.equal(waiting.attempts, 0, '기다리는 동안은 시도로 세지 않는다')
	assert.equal(waiting.text, '답글')
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 0)
	assert.equal(fetchCalls.filter((c) => c.text === '답글').length, 0, '부모가 끝나기 전에는 답글을 검사하지도 않는다')

	// 2분차: 복구 → 같은 실행 안에서 부모가 먼저, 그 다음 답글이 저장된다
	judge = () => ({})
	await processQueue(env, T0 + MIN)
	assert.deepEqual(
		db.rows(`SELECT "id", "ref", "root" FROM evaluations ORDER BY "rowid"`).map((r) => [r.id, r.ref, r.root]),
		[
			[PARENT_ID, null, PARENT_ID],
			[REPLY_ID, PARENT_ID, PARENT_ID],
		]
	)
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 0)
	assert.deepEqual([...vec.store.keys()].sort(), [PARENT_ID, REPLY_ID])
	assert.equal(vec.store.get(REPLY_ID)!.metadata!.ref, PARENT_ID)
})

test('부모가 거부되면 답글도 거부되고 해시까지 지워진다', async () => {
	const { db, vec, env } = setup()
	judge = (t) => (t === '부모 글' ? { profanity: 0.9 } : {})
	await seedQueue(db, { id: PARENT_ID, text: '부모 글', created_at: 1 })
	await seedQueue(db, { id: REPLY_ID, text: '답글', ref: PARENT_ID, root: PARENT_ID, created_at: 2 })
	await processQueue(env, T0)
	const reply = queueRow(db, REPLY_ID)
	assert.equal(reply.state, 'rejected')
	assert.equal(reply.last_error, 'parent rejected')
	assert.equal(reply.text, null)
	assert.equal(reply.text_hash, MASKED_HASH)
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 0)
	assert.equal(vec.store.size, 0)
})

test('저장된 부모가 없는 답글은 거부된다', async () => {
	const { db, env } = setup()
	await seedQueue(db, { id: REPLY_ID, text: '고아 답글', ref: PARENT_ID, root: PARENT_ID })
	await processQueue(env, T0)
	assert.equal(queueRow(db, REPLY_ID).last_error, 'parent missing')
	assert.equal(queueRow(db, REPLY_ID).state, 'rejected')
})

test('Vectorize 장애로 저장이 끊겨도 다시 실행하면 한 번만 저장된다', async () => {
	const { db, vec, env } = setup()
	await seedQueue(db, { id: PARENT_ID, text: '글' })
	vec.failNextUpsert = 1
	await processQueue(env, T0)
	assert.equal(queueRow(db, PARENT_ID).state, 'pending')
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 1, 'D1에는 먼저 들어갔다')
	assert.equal(vec.store.size, 0)

	await processQueue(env, T0 + MIN)
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 1)
	assert.equal(vec.store.size, 1)
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 0)
})

test('lease가 만료된 processing 행은 다시 집고, 만료 전에는 건드리지 않는다', async () => {
	const { db, env } = setup()
	await seedQueue(db, { id: PARENT_ID, text: '죽은 실행이 남긴 행', state: 'processing', attempts: 1, lease_until: T0 + 10 * MIN })
	assert.equal((await processQueue(env, T0)).processed, 0)
	assert.equal(queueRow(db, PARENT_ID).state, 'processing')
	assert.equal((await processQueue(env, T0 + 11 * MIN)).processed, 1)
	assert.equal(db.rows(`SELECT * FROM evaluations`).length, 1)
})

test('rejected·failed 행은 7일 뒤에 지워진다', async () => {
	const { db, env } = setup()
	judge = () => ({ profanity: 1 })
	await seedQueue(db, { id: PARENT_ID, text: '글' })
	await processQueue(env, T0)
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 1)
	await processQueue(env, T0 + 6 * 24 * 60 * MIN)
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 1)
	await processQueue(env, T0 + 8 * 24 * 60 * MIN)
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 0)
})

test('한 번에 20건까지만 처리한다', async () => {
	const { db, env } = setup()
	for (let i = 0; i < 25; i++) {
		await seedQueue(db, { id: `0190f0a0-0000-7000-8000-${String(i).padStart(12, '0')}`, text: `글 ${i}`, created_at: i })
	}
	assert.equal((await processQueue(env, T0)).processed, 20)
	assert.equal(db.rows(`SELECT * FROM eval_queue`).length, 5)
})
