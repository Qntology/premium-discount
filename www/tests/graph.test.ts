// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
import { test } from 'node:test'
import assert from 'node:assert/strict'
import vm from 'node:vm'
import { handleGraph } from '../src/graph.ts'
import { FakeD1, FakeVectorize, FakeLimiter, unitVector, seedEvaluation, seedVerdict } from '../../test-support/fakes.ts'

function setup() {
	const read = new FakeLimiter()
	const db = new FakeD1()
	const vec = new FakeVectorize()
	const env = { DB: db, EVAL_VECTORIZE: vec, GUEST_SALT: 's', AUTHOR_KEY_SECRET: 'k', EVAL_READ_LIMITER: read }
	return { read, db, vec, env: env as unknown as Parameters<typeof handleGraph>[1] }
}
const get = (path: string) => new Request('https://premium.discount' + path, { headers: { 'cf-connecting-ip': '203.0.113.7' } })

test('그래프 JSON API는 읽기 제한에 걸리면 429다 (D1·Vectorize를 조회하지 않는다)', async () => {
	const { env, read } = setup()
	read.allow = false
	for (const path of ['/graph?cc=example.com', '/graph/local?id=0190f0a0-0000-7000-8000-000000000001', '/graph/similar?id=0190f0a0-0000-7000-8000-000000000001']) {
		const res = (await handleGraph(get(path), env))!
		assert.equal(res.status, 429, path)
		assert.equal(res.headers.get('Retry-After'), '60')
	}
	assert.deepEqual(read.keys, ['203.0.113.7', '203.0.113.7', '203.0.113.7'])
})

test('제한 안에서는 그대로 응답한다', async () => {
	const { env } = setup()
	const res = (await handleGraph(get('/graph?cc=example.com'), env))!
	assert.equal(res.status, 200)
	assert.deepEqual(await res.json(), { nodes: [], edges: [], summary: { premium: 0, neutral: 0, discount: 0, unrated: 0 } })
})

// ---- 판정 등급 ----

const ROOT = '0190f0a0-0000-7000-8000-0000000000a0'
const A = '0190f0a0-0000-7000-8000-0000000000a1'
const B = '0190f0a0-0000-7000-8000-0000000000a2'
const C = '0190f0a0-0000-7000-8000-0000000000a3'
const LONE = '0190f0a0-0000-7000-8000-0000000000b0'

// 스레드 하나(ROOT ← A, B, C)와 따로 떨어진 평가 하나. ROOT는 판정 행이 없다
async function seedGraph(db: FakeD1) {
	await seedEvaluation(db, { id: ROOT, created_at: 1 })
	await seedEvaluation(db, { id: A, ref: ROOT, root: ROOT, created_at: 2 })
	await seedEvaluation(db, { id: B, ref: ROOT, root: ROOT, created_at: 3 })
	await seedEvaluation(db, { id: C, ref: ROOT, root: ROOT, created_at: 4 })
	await seedEvaluation(db, { id: LONE, created_at: 5 })
	await seedVerdict(db, { eval_id: A, tier: 'premium', grade: 2 })
	await seedVerdict(db, { eval_id: B, tier: 'discount', grade: -1 })
	await seedVerdict(db, { eval_id: C, tier: 'neutral', grade: 0 })
	await seedVerdict(db, { eval_id: LONE, tier: 'premium', grade: 1 })
}
type GraphNode = { id: string; type: string; tier?: string; grade?: number | null }
const tiers = (nodes: GraphNode[]) => Object.fromEntries(nodes.filter((n) => n.type === 'evaluation').map((n) => [n.id, [n.tier, n.grade]]))

test('/graph: 평가 노드에 tier·grade가 붙고(판정 행이 없으면 unrated, null), summary는 평가 노드의 등급별 개수다', async () => {
	const { env, db } = setup()
	await seedGraph(db)
	const body = await (await handleGraph(get('/graph?cc=example.com'), env))!.json()
	assert.deepEqual(tiers(body.nodes), {
		[ROOT]: ['unrated', null],
		[A]: ['premium', 2],
		[B]: ['discount', -1],
		[C]: ['neutral', 0],
		[LONE]: ['premium', 1],
	})
	assert.deepEqual(body.summary, { premium: 2, neutral: 1, discount: 1, unrated: 1 })
	for (const n of body.nodes as GraphNode[]) {
		if (n.type !== 'evaluation') assert.ok(!('tier' in n) && !('grade' in n), '도메인·작성자 노드에는 등급이 없다')
	}
	assert.equal(body.edges.filter((e: { type: string }) => e.type === 'reply').length, 3, '판정을 붙여도 답글 연결은 그대로다')
})

test('/graph/local: 스레드의 평가만 센다', async () => {
	const { env, db } = setup()
	await seedGraph(db)
	const body = await (await handleGraph(get('/graph/local?id=' + B), env))!.json()
	assert.deepEqual(Object.keys(tiers(body.nodes)).sort(), [ROOT, A, B, C].sort())
	assert.deepEqual(body.summary, { premium: 1, neutral: 1, discount: 1, unrated: 1 })
})

test('/graph/similar: 유사 노드에도 tier·grade가 붙고 summary는 유사 노드만 센다', async () => {
	const { env, db, vec } = setup()
	await seedGraph(db)
	await vec.upsert([{ id: A, values: unitVector(2) }])
	// 자기 자신(A)은 빠진다
	vec.matches = [A, LONE, ROOT, B].map((id, i) => ({ id, score: [1, 0.9, 0.8, 0.7][i] }))
	const body = await (await handleGraph(get('/graph/similar?id=' + A), env))!.json()
	assert.deepEqual(tiers(body.nodes), { [LONE]: ['premium', 1], [ROOT]: ['unrated', null], [B]: ['discount', -1] })
	assert.deepEqual(body.summary, { premium: 1, neutral: 0, discount: 1, unrated: 1 })
	assert.deepEqual(body.edges.map((e: { target: string }) => e.target), [LONE, ROOT, B])
})

test('/graph/view 페이지는 제한 대상이 아니다', async () => {
	const { env, read } = setup()
	read.allow = false
	const res = (await handleGraph(get('/graph/view'), env))!
	assert.equal(res.status, 200)
	assert.match(res.headers.get('Content-Type')!, /text\/html/)
	assert.equal(read.keys.length, 0)
})

test('뷰의 인라인 스크립트는 문법이 맞고, 로컬 원문 오버레이가 들어 있다', async () => {
	const { env } = setup()
	const html = await (await handleGraph(get('/graph/view'), env))!.text()
	const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1])
	assert.equal(scripts.length, 1)
	assert.doesNotThrow(() => new vm.Script(scripts[0]), '템플릿 문자열 안의 스크립트가 깨지지 않았다')
	assert.ok(scripts[0].includes("'PremiumDiscountLocal'"))
	assert.ok(scripts[0].includes('textContent'), '원문은 textContent로만 넣는다')
	assert.ok(!/innerHTML/.test(scripts[0]), '원문을 HTML로 넣지 않는다')
})
