// cron 판정 처리(processVerdicts) 테스트. D1은 실제 SQLite(node:sqlite)이고 마이그레이션 0002~0006을 그대로 적용한다.
// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test cron/test/*.test.ts  (premium-discount 폴더에서)
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import worker, { processVerdicts } from '../src/index.ts'
import { PARAMS, RECIPE, baselineOf, evidence, judge, type Obs } from '../src/verdict.ts'
import { FakeD1, FakeVectorize, AUTHOR, seedEvaluation, seedVote } from '../../test-support/fakes.ts'

const T0 = 1_763_251_200_000
const MIN = 60_000
const DAY = 86_400_000
const TO = '0x' + 'c'.repeat(40)
const realFetch = globalThis.fetch

beforeEach(() => {
	globalThis.fetch = (async () => {
		throw new Error('network must not be used in this test')
	}) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup() {
	const db = new FakeD1()
	const env = { DB: db, EVAL_VECTORIZE: new FakeVectorize(), DASHSCOPE_API_KEY: 'k', DECISION_ENDPOINT: 'https://decision.example' }
	return { db, env: env as unknown as Parameters<typeof processVerdicts>[0] }
}

const evalId = (n: number) => `0190f0a0-0000-7000-8000-${n.toString(16).padStart(12, '0')}`
let voterSeq = 0x100
// 투표자마다 다른 로그인 주소 (작성자 AUTHOR와 겹치지 않는다)
const nextVoter = () => '0x' + (voterSeq++).toString(16).padStart(40, '0')
const verdictOf = (db: FakeD1, id: string) => db.rows(`SELECT * FROM eval_verdicts WHERE "eval_id" = ?1`, id)[0]
const vote = (sign: number, at: number, weight = 1, kind = 'vote'): Obs => ({ kind, sign, weight, credit: 1, at_ms: at })
const close = (a: unknown, b: number, what: string) => assert.ok(Math.abs(Number(a) - b) <= 1e-9, `${what}: ${a} vs ${b}`)

// 서로 다른 로그인 투표자들이 한 표씩 던진다
async function cast(db: FakeD1, id: string, values: number[], at: number) {
	for (const value of values) await seedVote(db, { eval_id: id, voter: nextVoter(), value, created_at: at })
}

// 같은 cc+to 범위의 평가 6건: E1은 추천 3표(v 0.6), E6은 비추천 3표(v −0.6), E2~E5는 추천·비추천 2표씩(E 4, v 0)
const E = [0, 1, 2, 3, 4, 5, 6].map((n) => evalId(0xe0 + n))
async function seedScope(db: FakeD1) {
	for (let i = 1; i <= 6; i++) await seedEvaluation(db, { id: E[i], to: TO })
	await cast(db, E[1], [1, 1, 1], T0)
	for (const i of [2, 3, 4, 5]) await cast(db, E[i], [1, -1, 1, -1], T0)
	await cast(db, E[6], [-1, -1, -1], T0)
}

test('범위의 기준선으로 등급을 매긴다: 하나는 premium, 하나는 discount, 나머지는 neutral', async () => {
	const { db, env } = setup()
	await seedScope(db)
	assert.deepEqual(await processVerdicts(env, T0), { computed: 6, judged: 6 })

	const base = baselineOf([0.6, 0, 0, 0, 0, -0.6])
	for (const key of [`cc:example.com|to:${TO}`, 'cc:example.com', '*']) {
		const s = db.rows(`SELECT * FROM verdict_scopes WHERE "scope_key" = ?1`, key)[0]
		assert.ok(s, key)
		assert.equal(s.n, 6, key)
		close(s.mean, base.mean, `${key} mean`)
		close(s.sd, base.sd, `${key} sd`)
		assert.equal(s.updated_at, T0)
	}

	const top = verdictOf(db, E[1])
	const want = judge({ ...evidence([vote(1, T0), vote(1, T0), vote(1, T0)], T0, 'pd'), baselines: { secondary: base }, prev: { tier: 'unrated', since_ms: T0 }, nowMs: T0, track: 'pd' })
	assert.equal(top.tier, 'premium')
	assert.equal(top.grade, 2)
	assert.equal(top.level, 'secondary')
	assert.deepEqual(JSON.parse(String(top.reasons)), ['rank_high', 'level:secondary'])
	close(top.rank, want.rank!, 'rank')
	close(top.evidence, 3, 'evidence')
	close(top.value, 0.6, 'value')
	assert.equal(top.up, 3)
	assert.equal(top.down, 0)
	assert.equal(top.cc, 'example.com')
	assert.equal(top.to, TO)
	assert.equal(top.recipe, RECIPE)
	assert.equal(top.tier_since, T0)
	assert.equal(top.updated_at, T0)
	assert.equal(top.judged_at, T0)

	const bottom = verdictOf(db, E[6])
	assert.equal(bottom.tier, 'discount')
	assert.equal(bottom.grade, -2)
	assert.deepEqual(JSON.parse(String(bottom.reasons)), ['rank_low', 'level:secondary'])
	assert.equal(bottom.down, 3)
	for (const i of [2, 3, 4, 5]) {
		const mid = verdictOf(db, E[i])
		assert.equal(mid.tier, 'neutral', `E${i}`)
		assert.equal(mid.grade, 0)
		assert.deepEqual(JSON.parse(String(mid.reasons)), ['level:secondary'])
		assert.equal(mid.up, 2)
		assert.equal(mid.down, 2)
	}
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0, '계산한 평가의 표시는 지운다')
})

test('머무르기: 순위가 enter 아래로 내려와도 exit 위면 premium을 유지하고 tier_since도 그대로다. 값이 0 아래로 가면 neutral', async () => {
	const { db, env } = setup()
	await seedScope(db)
	await processVerdicts(env, T0)

	// 하루 뒤: E1에 비추천 2표, E2·E3·E4에 추천 1표씩
	const T1 = T0 + DAY
	await cast(db, E[1], [-1, -1], T1)
	for (const i of [2, 3, 4]) await cast(db, E[i], [1], T1)
	await processVerdicts(env, T1)

	const held = verdictOf(db, E[1])
	assert.equal(held.tier, 'premium')
	assert.equal(held.grade, 1)
	assert.deepEqual(JSON.parse(String(held.reasons)), ['held', 'level:secondary'])
	assert.ok(Number(held.rank) >= PARAMS.exit && Number(held.rank) < PARAMS.enter, `rank ${held.rank}`)
	assert.equal(held.tier_since, T0, '등급이 그대로면 시작 시각도 그대로다')
	assert.equal(held.judged_at, T1)
	close(held.evidence, evidence([vote(1, T0), vote(1, T0), vote(1, T0), vote(-1, T1), vote(-1, T1)], T1, 'pd').evidence, 'evidence')
	// 같은 E·v·기준선이라도 직전 등급이 premium이 아니었다면 neutral이다
	const s = db.rows(`SELECT "n", "mean", "sd" FROM verdict_scopes WHERE "scope_key" = ?1`, `cc:example.com|to:${TO}`)[0] as { n: number; mean: number; sd: number }
	const fresh = judge({ evidence: Number(held.evidence), value: Number(held.value), baselines: { secondary: s }, prev: { tier: 'neutral', since_ms: T0 }, nowMs: T1, track: 'pd' })
	assert.equal(fresh.tier, 'neutral')
	assert.equal(verdictOf(db, E[6]).tier_since, T0, 'discount도 그대로')

	// 1분 뒤 비추천 1표가 더 오면 v < 0 이라 머무를 수 없다
	const T2 = T1 + MIN
	await cast(db, E[1], [-1], T2)
	await processVerdicts(env, T2)
	const dropped = verdictOf(db, E[1])
	assert.ok(Number(dropped.value) < 0)
	assert.equal(dropped.tier, 'neutral')
	assert.equal(dropped.grade, 0)
	assert.equal(dropped.tier_since, T2, '등급이 바뀌면 시작 시각도 바뀐다')
})

test('게스트의 표와 작성자 자신의 표는 기록되지만 가중치가 0이다', async () => {
	const { db, env } = setup()
	const id = evalId(1)
	await seedEvaluation(db, { id })
	for (let i = 0; i < 3; i++) await seedVote(db, { eval_id: id, voter: nextVoter(), voter_kind: 'guest', value: 1, created_at: T0 })
	await seedVote(db, { eval_id: id, voter: AUTHOR, value: 1, created_at: T0 })
	await processVerdicts(env, T0)
	let row = verdictOf(db, id)
	assert.equal(row.evidence, 0)
	assert.equal(row.value, 0)
	assert.equal(row.up, 0)
	assert.equal(row.down, 0)
	assert.equal(row.tier, 'unrated')
	assert.equal(row.grade, null)
	assert.deepEqual(JSON.parse(String(row.reasons)), ['low_evidence', 'no_baseline'])

	// 다른 로그인 투표자의 비추천 1표만 센다
	await seedVote(db, { eval_id: id, voter: nextVoter(), value: -1, created_at: T0 + MIN })
	await processVerdicts(env, T0 + MIN)
	row = verdictOf(db, id)
	const want = evidence([vote(-1, T0 + MIN)], T0 + MIN, 'pd')
	close(row.evidence, want.evidence, 'evidence')
	close(row.value, want.value, 'value')
	assert.equal(row.up, 0)
	assert.equal(row.down, 1)
})

test('(투표자, 종류)마다 가장 최근 표만 센다: 바꾼 표, 철회, 종류가 다른 표, 같은 시각이면 id 순', async () => {
	const { db, env } = setup()
	const id = evalId(2)
	await seedEvaluation(db, { id })
	const [a, b, c, d] = [nextVoter(), nextVoter(), nextVoter(), nextVoter()]
	const H = 3_600_000
	// a: 추천 → 비추천으로 바꿈
	await seedVote(db, { eval_id: id, voter: a, value: 1, created_at: T0 - 3 * H })
	await seedVote(db, { eval_id: id, voter: a, value: -1, created_at: T0 - H })
	// b: 추천했다가 철회
	await seedVote(db, { eval_id: id, voter: b, value: 1, created_at: T0 - 2 * H })
	await seedVote(db, { eval_id: id, voter: b, value: 0, created_at: T0 - H / 2 })
	// c: 표(vote) 추천과 결과 표(vote_outcome) 비추천은 따로 센다
	await seedVote(db, { eval_id: id, voter: c, value: 1, created_at: T0 - 2 * H })
	await seedVote(db, { eval_id: id, voter: c, kind: 'vote_outcome', value: -1, created_at: T0 - H })
	// d: 같은 시각의 두 행은 id가 큰 쪽이 최신이다 (넣은 순서와 반대로 넣는다)
	await seedVote(db, { eval_id: id, voter: d, value: -1, created_at: T0 - H, id: 'vote-zz' })
	await seedVote(db, { eval_id: id, voter: d, value: 1, created_at: T0 - H, id: 'vote-aa' })
	// 넣은 순서가 아니라 created_at이 최신을 정한다: a의 더 이른 시각 추천을 마지막에 넣어도 a의 표는 비추천이다
	await seedVote(db, { eval_id: id, voter: a, value: 1, created_at: T0 - 4 * H })

	await processVerdicts(env, T0)
	const row = verdictOf(db, id)
	const want = evidence(
		[vote(-1, T0 - H), vote(1, T0 - 2 * H), vote(-1, T0 - H, PARAMS.weights.vote_outcome, 'vote_outcome'), vote(-1, T0 - H)],
		T0,
		'pd'
	)
	close(row.evidence, want.evidence, 'evidence')
	close(row.value, want.value, 'value')
	assert.equal(row.up, 1, 'c의 추천')
	assert.equal(row.down, 3, 'a, c의 결과 표, d')
	assert.equal(db.rows(`SELECT * FROM eval_votes WHERE "eval_id" = ?1`, id).length, 9, '표는 덮어쓰지 않고 쌓인다')
})

test('계산하는 사이에 새 표가 들어오면 표시가 남고, 다음 실행이 그 표를 센다', async () => {
	const { db, env } = setup()
	const id = evalId(3)
	await seedEvaluation(db, { id })
	await cast(db, id, [1, 1], T0)
	const late = nextVoter()
	let fired = false
	// cron이 표를 읽은 직후에 www가 새 표를 넣고 표시 시각을 늦춘다
	const racedDb = {
		prepare(sql: string) {
			const stmt = db.prepare(sql)
			if (!fired && sql.includes('FROM eval_votes')) {
				const all = stmt.all.bind(stmt)
				stmt.all = (async () => {
					const rs = await all()
					fired = true
					await seedVote(db, { eval_id: id, voter: late, value: 1, created_at: T0 + 30_000 })
					return rs
				}) as typeof stmt.all
			}
			return stmt
		},
	}
	await processVerdicts({ ...env, DB: racedDb } as unknown as typeof env, T0 + MIN)
	assert.ok(fired)
	assert.deepEqual(db.rows(`SELECT "eval_id", "at" FROM verdict_dirty`).map((r) => [r.eval_id, r.at]), [[id, T0 + 30_000]], '늦게 들어온 표시는 남는다')
	assert.equal(verdictOf(db, id).up, 2, '이번 계산에는 아직 없다')

	await processVerdicts(env, T0 + 2 * MIN)
	assert.equal(verdictOf(db, id).up, 3)
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)
})

test('sweep: 새 표가 없어도 가장 오래전에 계산한 판정을 다시 계산해 감쇠를 반영한다', async () => {
	const { db, env } = setup()
	const id = evalId(4)
	await seedEvaluation(db, { id })
	await cast(db, id, [1, 1, 1], T0)
	await processVerdicts(env, T0)
	let row = verdictOf(db, id)
	assert.equal(row.tier, 'neutral', 'E 3, 기준선 없음')
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)

	// 반감기(90일) 뒤: E가 절반이 되어 min_evidence 아래로 내려간다
	const later = T0 + PARAMS.tracks.pd.half_life_ms
	assert.deepEqual(await processVerdicts(env, later), { computed: 1, judged: 1 })
	row = verdictOf(db, id)
	close(row.evidence, 1.5, 'evidence')
	close(row.value, 1.5 / 3.5, 'value')
	assert.equal(row.updated_at, later)
	assert.equal(row.tier, 'unrated')
	assert.equal(row.tier_since, later)
	assert.deepEqual(JSON.parse(String(row.reasons)), ['low_evidence', 'no_baseline'])
})

test('sweep은 한 번에 25건, 오래된 것부터. 건드린 범위의 피어는 다시 판정만 하고, global만 같은 평가는 건드리지 않는다', async () => {
	const { db, env } = setup()
	const peers = Array.from({ length: 30 }, (_, i) => evalId(0x200 + i))
	for (const id of peers) {
		await seedEvaluation(db, { id })
		await cast(db, id, [1, -1, 1], T0)
	}
	const other = evalId(0x300)
	await seedEvaluation(db, { id: other, cc: 'other.example' })
	await cast(db, other, [1, 1, 1], T0)
	assert.deepEqual(await processVerdicts(env, T0), { computed: 31, judged: 31 })
	// other.example은 가장 최근에 계산한 것으로 만든다 (sweep 순서 밖)
	await db.prepare(`UPDATE eval_verdicts SET "updated_at" = ?1 WHERE "eval_id" = ?2`).bind(T0 + 1, other).run()

	const T1 = T0 + MIN
	assert.deepEqual(await processVerdicts(env, T1), { computed: 25, judged: 30 }, 'sweep 25건 + 같은 cc의 피어 5건')
	const rows = db.rows(`SELECT "eval_id", "updated_at", "judged_at" FROM eval_verdicts`)
	const peerRows = rows.filter((r) => r.eval_id !== other)
	assert.equal(peerRows.filter((r) => r.updated_at === T1).length, 25)
	assert.equal(peerRows.filter((r) => r.updated_at === T0).length, 5, '피어는 다시 계산하지 않는다')
	assert.ok(peerRows.every((r) => r.judged_at === T1), '피어도 다시 판정한다')
	const o = rows.find((r) => r.eval_id === other)!
	assert.equal(o.judged_at, T0, 'global 범위만 같은 평가는 다시 판정하지 않는다')

	// 다음 실행은 남은 5건과 other.example부터 sweep한다
	const T2 = T1 + MIN
	await processVerdicts(env, T2)
	const swept = db.rows(`SELECT "eval_id" FROM eval_verdicts WHERE "updated_at" = ?1`, T2).map((r) => r.eval_id)
	assert.equal(swept.length, 25)
	assert.ok(swept.includes(other))
	for (const r of peerRows.filter((r) => r.updated_at === T0)) assert.ok(swept.includes(r.eval_id))
})

test('평가가 없는 표시는 지우고 건너뛴다. 평가가 사라진 판정 행은 지우고 기준선에서도 뺀다', async () => {
	const { db, env } = setup()
	const ghost = evalId(5)
	await db.prepare(`INSERT INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)`).bind(ghost, T0).run()
	assert.deepEqual(await processVerdicts(env, T0), { computed: 0, judged: 0 })
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)
	assert.equal(db.rows(`SELECT * FROM eval_verdicts`).length, 0)

	// 판정까지 끝난 뒤 평가가 지워진 경우 (cron이 부분 저장을 되돌림)
	const kept = evalId(6)
	const gone = evalId(7)
	await seedEvaluation(db, { id: kept })
	await seedEvaluation(db, { id: gone })
	await cast(db, kept, [1, 1, 1], T0)
	await cast(db, gone, [-1, -1, -1], T0)
	await processVerdicts(env, T0)
	assert.equal(db.rows(`SELECT "n" FROM verdict_scopes WHERE "scope_key" = 'cc:example.com'`)[0].n, 2)
	await db.prepare(`DELETE FROM evaluations WHERE "id" = ?1`).bind(gone).run()

	await processVerdicts(env, T0 + MIN)
	assert.deepEqual(db.rows(`SELECT "eval_id" FROM eval_verdicts`).map((r) => r.eval_id), [kept])
	const s = db.rows(`SELECT "n", "mean" FROM verdict_scopes WHERE "scope_key" = 'cc:example.com'`)[0]
	assert.equal(s.n, 1)
	close(s.mean, Number(verdictOf(db, kept).value), 'mean')
})

test('E < min_evidence인 평가는 기준선에 들어가지 않는다', async () => {
	const { db, env } = setup()
	await seedEvaluation(db, { id: evalId(8) })
	await seedEvaluation(db, { id: evalId(9) })
	await cast(db, evalId(8), [1, 1], T0)
	await cast(db, evalId(9), [-1], T0)
	await processVerdicts(env, T0)
	const s = db.rows(`SELECT "n", "mean", "sd" FROM verdict_scopes WHERE "scope_key" = '*'`)[0]
	assert.equal(s.n, 1)
	close(s.mean, 0.5, 'mean')
	assert.equal(s.sd, 0)
	assert.equal(db.rows(`SELECT * FROM verdict_scopes WHERE "scope_key" LIKE 'cc:%|to:%'`).length, 0, 'to가 없으면 secondary 범위도 없다')
})

test('예약 실행은 대기열 다음에 판정을 처리한다', async () => {
	const { db, env } = setup()
	const id = evalId(10)
	await seedEvaluation(db, { id })
	await cast(db, id, [1], T0)
	await worker.scheduled({} as ScheduledController, env, {} as ExecutionContext)
	assert.equal(verdictOf(db, id).up, 1)
	assert.equal(db.rows(`SELECT * FROM verdict_dirty`).length, 0)
})
