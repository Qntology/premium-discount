// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test www/tests/*.test.ts  (premium-discount 폴더에서)
// 접근성 내비게이션 타임트라이얼(POST · GET · DELETE /nav/trials): 플로우는 체크포인트 10개까지, 앱 · 웹마다 코스 10개까지
// (받는 수 제한 없음), 같은 코스는 횟수(중복 카운트)로, 다른 사용자의 기록은 11번 넘게 달린 코스만 고스트로,
// 지갑(익명 계정) 코스는 30일, 세션 코스는 기한 없음, 계정 없는 합계는 줄지 않음.
// D1은 실제 SQLite(node:sqlite)에 migrations 0002~0007을 적용한 가짜다. 세션 확인(S3)은 globalThis.fetch로 흉내 낸다.
import { test, beforeEach, afterEach } from 'node:test'
import assert from 'node:assert/strict'
import { ethers } from 'ethers'
import { handleNavTrials, accountKey, checkpointId, courseChain, courseHash, hashId, routeMarks, stepKey } from '../src/navtrials.ts'
import { walletRequestMessage, addressFromEmail } from '../src/auth.ts'
import { authorKey } from '../src/projection.ts'
import type { EvalEnv } from '../src/evaluations.ts'
import { DAY_MS, TRIAL_POLICY, dayOf, expireWalletTrials, tier } from '../../cron/src/navtrials.ts'
import { FakeD1 } from '../../test-support/fakes.ts'

const ORIGIN = 'tauri://localhost'
const IP = '203.0.113.7'
const HASH = 'ab'.repeat(20)
const TOKEN = 'cd'.repeat(32)
const EMAIL = 'nav@example.com'
const S3 = {
	SESSION_S3_BUCKET: 'oauth-sessions',
	SESSION_S3_REGION: 'ap-northeast-2',
	SESSION_S3_ACCESS_KEY_ID: 'AKIATESTTESTTESTTEST',
	SESSION_S3_SECRET_ACCESS_KEY: 'test-secret',
}
const session = { 'x-oauth-hash': HASH, 'x-oauth-token': TOKEN }
const NOW = Date.UTC(2026, 10, 17, 3, 0, 0)
const TODAY = dayOf(NOW)
const MINUTE = 60_000
// ethers 6.17 로 뽑은 기준값 (crates/pd-nav trial.rs 시험과 같다)
const APP = '0x39206c971297059b89e4e6fc35844ee87feb0407'
// 코스 S1 → R2 → S3 의 코스 id 체인 (마지막이 코스 id)
const CHAIN = ['0x826797e39016c80c7db39259352ad3af843f0acf', '0x517cbe9e479a79a6ef66e403072e9776000d90fc', '0xa9c45e19240962f6cb2dfb7aa8381e3ec3d80340']
const COURSE = CHAIN[2]
// 영역 R2 의 체크포인트 id
const CP_R2 = '0x4e9fa56d3569702eb1ec03106e8bd1896a716af5'
const S1 = '0x' + '1'.repeat(40)
const R2 = '0x' + '2'.repeat(40)
const S3_HASH = '0x' + '3'.repeat(40)
const APP2 = '0x' + '9'.repeat(40)
const enc = new TextEncoder()
const realFetch = globalThis.fetch

beforeEach(() => {
	globalThis.fetch = (async () => new Response(JSON.stringify({ token: TOKEN, email: EMAIL }))) as typeof fetch
})
afterEach(() => {
	globalThis.fetch = realFetch
})

function setup() {
	const db = new FakeD1()
	const env = { DB: db, GUEST_SALT: 'salt', AUTHOR_KEY_SECRET: 'secret', ...S3 }
	return { db, env: env as unknown as EvalEnv }
}

type Kind = 'screen' | 'region' | 'page' | 'element'
const h = (n: number) => '0x' + n.toString(16).padStart(40, '0')
// trial/1 런. points = [종류, 해시, 구간 시간, 제스처?]
function run(points: ([Kind, string, number] | [Kind, string, number, string])[], over: Record<string, unknown> = {}, app = APP) {
	const steps = points.map(([kind, hash, split_ms, gesture]) => (gesture ? { kind, hash, split_ms, gesture } : { kind, hash, split_ms }))
	return {
		schema: 'trial/1',
		app,
		course: courseHash(
			app,
			steps.map((s: any) => (s.gesture && s.gesture !== 'press' ? { kind: s.kind, hash: s.hash, gesture: s.gesture } : { kind: s.kind, hash: s.hash }))
		),
		steps,
		total_ms: steps[steps.length - 1].split_ms,
		type: null,
		day: TODAY,
		recipe: 'pd-verdict/2',
		...over,
	}
}
// 시작 S1 → 영역 R2 → 결승 S3
const lap = (r2: number, s3: number, over: Record<string, unknown> = {}) =>
	run(
		[
			['screen', S1, 0],
			['region', R2, r2],
			['screen', S3_HASH, s3],
		],
		over
	)
// i 번째 코스: 시작 화면만 다르다
const courseOf = (i: number, over: Record<string, unknown> = {}, app = APP) =>
	run(
		[
			['screen', h(100 + i), 0],
			['screen', S3_HASH, 1000 + 100 * i],
		],
		over,
		app
	)

async function walletRequest(wallet: ethers.HDNodeWallet, method: string, body: unknown, nowMs: number, query = '') {
	const text = body === undefined ? '' : JSON.stringify(body)
	const signature = await wallet.signMessage(await walletRequestMessage(wallet.address, nowMs, method, '/nav/trials', enc.encode(text)))
	const headers = { 'Content-Type': 'application/json', Origin: ORIGIN, 'cf-connecting-ip': IP, 'x-pd-address': wallet.address, 'x-pd-time': String(nowMs), 'x-pd-signature': signature }
	return new Request('https://premium.discount/nav/trials' + query, text ? { method, headers, body: text } : { method, headers })
}
function plainRequest(method: string, body: unknown, headers: Record<string, string> = {}, query = '') {
	const init: RequestInit = { method, headers: { 'Content-Type': 'application/json', Origin: ORIGIN, 'cf-connecting-ip': IP, ...headers } }
	if (body !== undefined) init.body = typeof body === 'string' ? body : JSON.stringify(body)
	return new Request('https://premium.discount/nav/trials' + query, init)
}
async function send(env: EvalEnv, request: Request, nowMs = NOW) {
	const res = (await handleNavTrials(request, env, nowMs))!
	return { status: res.status, body: (res.status === 204 ? null : await res.json()) as any, res }
}
const postWallet = async (env: EvalEnv, wallet: ethers.HDNodeWallet, runs: unknown[], nowMs = NOW) => send(env, await walletRequest(wallet, 'POST', { runs }, nowMs), nowMs)
const postSession = (env: EvalEnv, runs: unknown[], nowMs = NOW) => send(env, plainRequest('POST', { runs }, session), nowMs)
const getSession = (env: EvalEnv, app = APP, nowMs = NOW) => send(env, plainRequest('GET', undefined, session, `?app=${app}`), nowMs)
const getGuest = (env: EvalEnv, app = APP) => send(env, plainRequest('GET', undefined, {}, `?app=${app}`))
const statuses = (body: any) => body.results.map((r: any) => r.status)
const course = (db: FakeD1, c: string) => db.rows(`SELECT * FROM nav_courses WHERE "course" = ?1`, c)[0]
// 저장된 체크포인트: 체크포인트 id 가 붙는다
const withIds = (steps: { kind: Kind; hash: string; gesture?: string }[], app = APP) => {
	const chain = courseChain(app, steps as any)
	return steps.map((s, i) => ({ ...s, id: checkpointId(s as any), course: chain[i] }))
}

test('체크포인트 id · 코스 id 는 기기(crates/pd-nav)와 같은 기준값이다', () => {
	assert.equal(hashId('nav-app/1|shop.example.com'), APP)
	const steps = [
		{ kind: 'screen' as const, hash: S1 },
		{ kind: 'region' as const, hash: R2 },
		{ kind: 'screen' as const, hash: S3_HASH },
	]
	// 체크포인트 id = hashId(종류 + 해시 + 제스처)
	assert.equal(checkpointId(steps[1]), CP_R2)
	assert.equal(checkpointId(steps[1]), hashId(`region${R2}`))
	assert.equal(checkpointId({ kind: 'page', hash: R2, gesture: 'scroll-down' }), hashId(`page${R2}scroll-down`))
	// 코스 id 체인: hashId(앱 id + 체크포인트 id) → hashId(이전 코스 id + 체크포인트 id) → …, 마지막이 코스 id
	const ids = steps.map(checkpointId)
	assert.equal(hashId(`${APP}${ids[0]}`), CHAIN[0])
	assert.equal(hashId(`${CHAIN[0]}${ids[1]}`), CHAIN[1])
	assert.equal(hashId(`${CHAIN[1]}${ids[2]}`), CHAIN[2])
	assert.deepEqual(courseChain(APP, steps), CHAIN)
	assert.equal(courseHash(APP, steps), COURSE)
	// 앞부분이 같으면 그 지점까지 같은 id, 다른 체크포인트부터 갈라진다
	const branch = courseChain(APP, [steps[0], steps[1], { kind: 'screen', hash: h(0x66) }])
	assert.deepEqual(branch.slice(0, 2), CHAIN.slice(0, 2))
	assert.notEqual(branch[2], COURSE)
	assert.equal(lap(1000, 3000).course, COURSE)
})

test('코스 등급은 결정이 난 런 3개부터, 2/3 이상이 한쪽이면 그 등급이다 (Rust tier 와 같은 표)', () => {
	assert.deepEqual(
		[tier(2, 0), tier(2, 1), tier(1, 2), tier(3, 2), tier(4, 2), tier(0, 3)],
		['unrated', 'premium', 'discount', 'neutral', 'premium', 'discount']
	)
})

test('코스 id 가 없으면 새 코스를 만들고, 같은 사용자가 같은 코스를 다시 달리면 등록하지 않는다 (내 기록만 고친다)', async () => {
	const { db, env } = setup()
	const wallet = ethers.Wallet.createRandom()
	const first = await postWallet(env, wallet, [lap(2000, 4000, { type: 'discount' })])
	assert.equal(first.status, 200)
	assert.deepEqual(first.body.results, [{ course: COURSE, status: 'stored', created: true, duplicate: false }])
	// 같은 사람이 같은 코스를 다시: 등록하지 않는다(duplicate). 내 최고 기록만 간다
	const second = await postWallet(env, wallet, [lap(1000, 2500, { type: 'premium' })], NOW + MINUTE)
	assert.deepEqual(second.body.results, [{ course: COURSE, status: 'repeat', created: false, duplicate: true, best: true }])
	// 다음 날에도 마찬가지
	const third = await postWallet(env, wallet, [lap(3000, 6000, { day: TODAY + 1 })], NOW + DAY_MS)
	assert.deepEqual(third.body.results, [{ course: COURSE, status: 'repeat', created: false, duplicate: true, best: false }])
	// 체크포인트가 하나라도 다르면 새 코스가 생긴다
	const detour = run(
		[
			['screen', S1, 0],
			['region', h(0x77), 900],
			['screen', S3_HASH, 2000],
		],
		{ day: TODAY + 1 }
	)
	const other = await postWallet(env, wallet, [detour], NOW + DAY_MS)
	assert.equal(other.body.results[0].status, 'stored')
	assert.equal(other.body.results[0].created, true)
	assert.notEqual(other.body.results[0].course, COURSE)

	const all = (await send(env, await walletRequest(wallet, 'GET', undefined, NOW + DAY_MS, `?app=${APP}`), NOW + DAY_MS)).body.mine
	assert.equal(all.length, 2)
	const mine = all.filter((m: any) => m.course === COURSE)
	assert.deepEqual(
		{ runs: mine[0].runs, premium: mine[0].premium, discount: mine[0].discount, tier: mine[0].tier, best: mine[0].best, last: mine[0].last, last_day: mine[0].last_day },
		{ runs: 3, premium: 1, discount: 1, tier: 'unrated', best: [0, 1000, 2500], last: [0, 3000, 6000], last_day: TODAY + 1 }
	)
	assert.deepEqual(
		mine[0].steps,
		withIds([
			{ kind: 'screen', hash: S1 },
			{ kind: 'region', hash: R2 },
			{ kind: 'screen', hash: S3_HASH },
		])
	)
	assert.equal(mine[0].steps[1].id, CP_R2)
	const total = course(db, COURSE)
	assert.deepEqual(
		{ runs: total.runs, premium: total.premium, discount: total.discount, record: JSON.parse(total.record as string), sums: JSON.parse(total.sums as string) },
		{ runs: 1, premium: 0, discount: 1, record: [0, 2000, 4000], sums: [0, 2000, 4000] }
	)
	// 판정 cron 의 입력도 처음 등록한 런 하나만
	assert.deepEqual(db.rows(`SELECT "hash", "day", "type", "n" FROM nav_counts WHERE "hash" = ?1 ORDER BY "type"`, COURSE), [
		{ hash: COURSE, day: TODAY, type: -1, n: 1 },
	])
	// 사용자 코스 id = hashId(계정 키 + 코스 id) 가 하나만 있다
	const key = await accountKey('secret', 'wallet', wallet.address)
	assert.deepEqual(
		db.rows(`SELECT "id" FROM nav_registrations ORDER BY "id"`).map((r) => r.id),
		[hashId(`${key}${COURSE}`), hashId(`${key}${other.body.results[0].course}`)].sort()
	)
	// 표에 지갑 주소는 없다
	const dump = JSON.stringify([db.rows(`SELECT * FROM nav_trials`), db.rows(`SELECT * FROM nav_courses`)])
	assert.ok(!dump.includes(wallet.address.toLowerCase().slice(2)))
	assert.equal(db.rows(`SELECT "account" FROM nav_trials`)[0].account, await accountKey('secret', 'wallet', wallet.address))
})

test('앱 · 웹마다 코스 10개: 11번째 새 코스는 가장 오래 달리지 않은 코스를 덮어쓴다', async () => {
	const { db, env } = setup()
	const ten = await postSession(env, Array.from({ length: 10 }, (_, i) => courseOf(i)))
	assert.deepEqual(statuses(ten.body), new Array(10).fill('stored'))
	// 0번 코스를 다시 달려 가장 최근이 된다 → 다음에 밀려나는 것은 1번
	await postSession(env, [courseOf(0)], NOW + MINUTE)
	const eleventh = await postSession(env, [courseOf(10)], NOW + 2 * MINUTE)
	assert.deepEqual(eleventh.body.results, [{ course: courseOf(10).course, status: 'replaced', created: true, duplicate: false, evicted: courseOf(1).course }])

	const mine = (await getSession(env)).body.mine
	assert.equal(mine.length, TRIAL_POLICY.maxCourses)
	assert.equal(mine[0].course, courseOf(10).course, '최근에 달린 순')
	assert.equal(mine.find((m: any) => m.course === courseOf(0).course).runs, 2)
	assert.ok(!mine.some((m: any) => m.course === courseOf(1).course))
	// 다른 앱 · 웹은 따로 센다. 밀려난 코스의 계정 없는 합계는 남는다
	assert.deepEqual(statuses((await postSession(env, [courseOf(0, {}, APP2)])).body), ['stored'])
	assert.equal((await getSession(env)).body.mine.length, 10)
	assert.equal((await getSession(env, APP2)).body.mine.length, 1)
	assert.equal(course(db, courseOf(1).course).runs, 1)
	// 밀려난 코스를 다시 달리면 콜렉션에는 다시 들어오지만, 이미 등록한 코스라 다시 등록하지 않는다
	const back = (await postSession(env, [courseOf(1)], NOW + 3 * MINUTE)).body.results[0]
	assert.equal(back.status, 'replaced')
	assert.equal(back.duplicate, true)
	assert.equal(course(db, courseOf(1).course).runs, 1)
	// 한 요청 안에서 넘쳐도 같다: 새 코스 11개 → 마지막 것이 그 요청의 첫 코스를 밀어낸다
	const { env: env2 } = setup()
	const burst = await postSession(env2, Array.from({ length: 10 }, (_, i) => courseOf(20 + i)))
	assert.equal(burst.status, 200)
	const more = await postSession(env2, [courseOf(30)], NOW + MINUTE)
	assert.equal(more.body.results[0].evicted, courseOf(20).course)
})

test('받는 수 제한은 없다: 같은 분에 런 150개를 보내도 모두 받지만, 한 사용자는 같은 코스를 한 번만 등록한다', async () => {
	const { db, env } = setup()
	for (let k = 0; k < 15; k++) {
		const res = await postSession(env, Array.from({ length: 10 }, (_, i) => courseOf(i)), NOW + k)
		assert.equal(res.status, 200)
		assert.deepEqual(statuses(res.body), new Array(10).fill(k === 0 ? 'stored' : 'repeat'))
	}
	const mine = (await getSession(env)).body.mine
	assert.equal(mine.reduce((n: number, m: any) => n + m.runs, 0), 150)
	// 공개 합계는 사용자마다 한 번 (코스 10개 × 1명)
	assert.ok(db.rows(`SELECT "runs" FROM nav_courses`).every((r) => r.runs === 1))
	assert.equal(db.rows(`SELECT * FROM nav_registrations`).length, 10)
})

test('플로우는 체크포인트 10개까지이고, 형식이 틀린 런은 invalid 다', async () => {
	const { db, env } = setup()
	const eleven = run(Array.from({ length: 11 }, (_, i) => ['screen', h(i + 1), i * 100] as [Kind, string, number]))
	const tenPoints = run(Array.from({ length: 10 }, (_, i) => ['screen', h(i + 1), i * 100] as [Kind, string, number]))
	const bad = [
		eleven,
		lap(1000, 3000, { course: h(7) }),
		lap(1050, 3000),
		run([
			['screen', S1, 100],
			['screen', S3_HASH, 300],
		]),
		lap(3000, 2000),
		run([
			['screen', S1, 0],
			['screen', S1, 100],
			['screen', S3_HASH, 300],
		]),
		lap(1000, 3000, { total_ms: 2000 }),
		lap(1000, 3000, { type: 'neutral' }),
		lap(1000, 3000, { type: undefined }),
		lap(1000, 3000, { schema: 'trial/2' }),
		lap(1000, 3000, { day: TODAY + 2 }),
		lap(1000, 3_600_100),
		run([['screen', S1, 0]]),
		'not a run',
	]
	const withVectors = lap(1000, 3000)
	;(withVectors.steps[1] as any).vectors = [{ space: 'granite-97m-r2-cls-v1', vector: [0.6, 0.8] }]
	const notUnit = lap(1000, 3000)
	;(notUnit.steps[1] as any).vectors = [{ space: 'granite-97m-r2-cls-v1', vector: [3, 4] }]
	// 한 요청은 런 10개까지라 나눠 보낸다
	for (const part of [bad.slice(0, 7), bad.slice(7)]) {
		const { status, body } = await postSession(env, part)
		assert.equal(status, 200)
		assert.ok(body.results.every((r: any) => r.status === 'invalid' && r.course === null && typeof r.error === 'string'))
	}
	assert.match((await postSession(env, [eleven])).body.results[0].error, /2 to 10 checkpoints/)
	const { body } = await postSession(env, [notUnit, tenPoints, withVectors])
	assert.deepEqual(statuses(body), ['invalid', 'stored', 'stored'])
	assert.deepEqual(db.rows(`SELECT "hash", "space", "dims" FROM nav_vectors`), [{ hash: R2, space: 'granite-97m-r2-cls-v1', dims: 2 }])
})

test('다른 사용자의 타임트라이얼: 11번 넘게 달린 코스만 보이고, 중복 카운트 · 등급 · 최고 기록 · 평균 고스트를 준다', async () => {
	const { env } = setup()
	const wallets = Array.from({ length: 11 }, () => ethers.Wallet.createRandom())
	const typeOf = (i: number) => (i < 8 ? 'premium' : i < 10 ? 'discount' : null)
	for (let i = 0; i < 10; i++) await postWallet(env, wallets[i], [lap(1000 + 100 * i, 3000 + 200 * i, { type: typeOf(i) })])
	// 10번까지는 공개하지 않는다 (게스트에게도, 달린 사람에게도)
	assert.deepEqual((await getGuest(env)).body.others, [])
	assert.deepEqual((await send(env, await walletRequest(wallets[0], 'GET', undefined, NOW, `?app=${APP}`))).body.others, [])

	await postWallet(env, wallets[10], [lap(2000, 5000, { type: typeOf(10) })])
	const guest = await getGuest(env)
	assert.equal(guest.status, 200)
	assert.equal(guest.body.mine, null)
	assert.equal(guest.body.account, null)
	assert.deepEqual(guest.body.others, [
		{
			course: COURSE,
			steps: withIds([
				{ kind: 'screen', hash: S1 },
				{ kind: 'region', hash: R2 },
				{ kind: 'screen', hash: S3_HASH },
			]),
			runs: 11,
			premium: 8,
			discount: 2,
			tier: 'premium',
			record: [0, 1000, 3000],
			average: [0, 1500, 4000],
			checkpoints: [0, 1, 2].map(() => ({ premium: 0, discount: 0, tier: 'unrated' })),
		},
	])
	assert.deepEqual(guest.body.policy, { max_steps: 10, max_courses: 10, publish_min_runs: 11 })
	// 달린 사람은 내 기록과 다른 사용자의 기록을 함께 받는다
	const signed = (await send(env, await walletRequest(wallets[3], 'GET', undefined, NOW, `?app=${APP}`))).body
	assert.equal(signed.account, 'wallet')
	assert.deepEqual(signed.mine[0].best, [0, 1300, 3600])
	assert.equal(signed.others[0].runs, 11)
	// 다른 앱에는 보이지 않는다
	assert.deepEqual((await getGuest(env, APP2)).body.others, [])
})

test('지갑 코스는 마지막으로 달린 날 기준 30일까지, 세션 코스는 기한 없이 둔다 (계정 없는 합계는 남는다)', async () => {
	const { db, env } = setup()
	const wallet = ethers.Wallet.createRandom()
	assert.deepEqual(statuses((await postWallet(env, wallet, [courseOf(0), courseOf(1, { day: TODAY - 29 }), courseOf(2, { day: TODAY - 30 })])).body), [
		'stored',
		'stored',
		'expired',
	])
	// 하루 뒤 새 런을 올리면 기한이 지난 코스(29일 전)가 지워진다
	await postWallet(env, wallet, [courseOf(3, { day: TODAY + 1 })], NOW + DAY_MS)
	assert.deepEqual(
		db.rows(`SELECT "course" FROM nav_trials ORDER BY "last_day"`).map((r) => r.course),
		[courseOf(0).course, courseOf(3).course]
	)
	assert.equal(course(db, courseOf(1).course).runs, 1, '합계는 남는다')

	// 더 올리지 않는 지갑도 cron 이 지운다. 세션 코스는 그대로
	await postSession(env, [courseOf(5, { day: TODAY - 200 })])
	assert.equal(await expireWalletTrials(db as unknown as D1Database, NOW + 29 * DAY_MS), 0)
	assert.equal(await expireWalletTrials(db as unknown as D1Database, NOW + 30 * DAY_MS), 1)
	assert.equal(await expireWalletTrials(db as unknown as D1Database, NOW + 31 * DAY_MS), 1)
	assert.deepEqual(db.rows(`SELECT "kind", "course" FROM nav_trials`), [{ kind: 'session', course: courseOf(5).course }])
	assert.equal(db.rows(`SELECT "account" FROM nav_trials`)[0].account, await accountKey('secret', 'session', addressFromEmail(EMAIL)))
})

test('계정 키는 갈래마다 다르고 공개 작성자 키와도 맞춰 볼 수 없다', async () => {
	const address = addressFromEmail(EMAIL)
	const asSession = await accountKey('secret', 'session', address)
	assert.match(asSession, /^[0-9a-f]{64}$/)
	assert.notEqual(asSession, await accountKey('secret', 'wallet', address))
	assert.ok(!asSession.startsWith(await authorKey(address, 'secret')))
})

test('DELETE 는 내 콜렉션(앱 하나 또는 전부)만 지우고 계정 없는 합계는 남긴다', async () => {
	const { db, env } = setup()
	const a = ethers.Wallet.createRandom()
	const b = ethers.Wallet.createRandom()
	await postWallet(env, a, [courseOf(0), courseOf(1), courseOf(0, {}, APP2)])
	await postWallet(env, b, [courseOf(0)])
	const one = await send(env, await walletRequest(a, 'DELETE', undefined, NOW, `?app=${APP}`))
	assert.deepEqual(one.body, { account: 'wallet', deleted: 2 })
	const all = await send(env, await walletRequest(a, 'DELETE', undefined, NOW))
	assert.deepEqual(all.body, { account: 'wallet', deleted: 1 })
	assert.equal(db.rows(`SELECT * FROM nav_trials`).length, 1, 'b 의 코스는 남는다')
	assert.equal(course(db, courseOf(0).course).runs, 2)
	// 지운 뒤 같은 코스를 다시 달려도 이미 등록한 코스라 다시 등록하지 않는다 (콜렉션에는 다시 들어온다)
	const again = (await postWallet(env, a, [courseOf(0)], NOW + MINUTE)).body.results[0]
	assert.deepEqual([again.status, again.duplicate], ['stored', true])
	assert.equal(course(db, courseOf(0).course).runs, 2)
	await postSession(env, [courseOf(4)])
	assert.deepEqual((await send(env, plainRequest('DELETE', undefined, session))).body, { account: 'session', deleted: 1 })
})

test('인증 · 요청 형식', async () => {
	const { db, env } = setup()
	const wallet = ethers.Wallet.createRandom()
	assert.equal((await send(env, plainRequest('POST', { runs: [lap(1000, 3000)] }))).status, 401)
	// 다른 본문에 대한 서명 · 5분 지난 서명
	const signed = await walletRequest(wallet, 'POST', { runs: [lap(1000, 3000)] }, NOW)
	assert.equal((await send(env, new Request(signed.url, { method: 'POST', headers: signed.headers, body: JSON.stringify({ runs: [lap(1000, 2000)] }) }))).status, 401)
	assert.equal((await send(env, await walletRequest(wallet, 'POST', { runs: [lap(1000, 3000)] }, NOW), NOW + 5 * MINUTE + 1)).status, 401)
	// 세션이 맞아도 지갑 헤더가 틀리면 받지 않는다 (GET 도)
	assert.equal((await send(env, plainRequest('POST', { runs: [lap(1000, 3000)] }, { ...session, 'x-pd-address': wallet.address }))).status, 401)
	assert.equal((await send(env, plainRequest('GET', undefined, { 'x-pd-address': wallet.address }, `?app=${APP}`))).status, 401)
	assert.equal(db.rows(`SELECT * FROM nav_trials`).length, 0)

	assert.equal((await send(env, plainRequest('GET', undefined, session))).status, 400, 'app 이 없다')
	assert.equal((await send(env, plainRequest('GET', undefined, session, '?app=shop.example.com'))).status, 400)
	assert.equal((await postSession(env, [])).status, 400)
	assert.equal((await postSession(env, Array.from({ length: 11 }, (_, i) => courseOf(i)))).status, 400)
	assert.equal((await send(env, plainRequest('POST', '{"runs":', session))).status, 400)
	assert.equal((await send(env, plainRequest('PUT', undefined, session))).status, 405)
	const pre = await send(env, plainRequest('OPTIONS', undefined))
	assert.equal(pre.status, 204)
	assert.match(pre.res.headers.get('Access-Control-Allow-Methods') ?? '', /GET, POST, DELETE/)
	assert.match(pre.res.headers.get('Access-Control-Allow-Headers') ?? '', /x-pd-signature/)
	assert.equal(await handleNavTrials(new Request('https://premium.discount/nav/marks', { method: 'POST' }), env, NOW), null)
})

// crates/pd-nav trial.rs 시험과 같은 사례: 고스트 S1 → A → B → 결승, 런 S1 → A → C → 결승
test('고스트 따라가기 판정: 따라간 체크포인트는 premium, 고르지 않은 체크포인트는 discount (Rust route_marks 와 같은 표)', () => {
	const step = (kind: Kind, hash: string) => ({ kind, hash })
	const [start, a, b, c, finish] = [step('screen', S1), step('region', R2), step('region', h(0x44)), step('region', h(0x55)), step('screen', S3_HASH)]
	const ghost = [start, a, b, finish]
	assert.deepEqual(routeMarks([start, a, c, finish], ghost), [
		{ step: a, type: 1 },
		{ step: b, type: -1 },
	])
	assert.deepEqual(routeMarks(ghost, ghost), [
		{ step: a, type: 1 },
		{ step: b, type: 1 },
	])
	assert.deepEqual(routeMarks([start, b, a, finish], ghost), [
		{ step: a, type: 1 },
		{ step: b, type: 1 },
	])
	assert.deepEqual(routeMarks([start, finish], ghost), [
		{ step: a, type: -1 },
		{ step: b, type: -1 },
	])
	assert.equal(routeMarks([start, a, c], ghost), null, '결승이 다르면 판정하지 않는다')
	assert.equal(routeMarks([c, a, finish], ghost), null, '시작이 다르면 판정하지 않는다')
	assert.equal(routeMarks([], ghost), null)
	assert.deepEqual(routeMarks([start, a, c, finish], [start, finish]), [])
})

test('다른 사용자의 코스를 따라 달리면 체크포인트에 premium, 다른 길로 가면 고르지 않은 체크포인트에 discount 가 쌓이고 고스트에 보인다', async () => {
	const { db, env } = setup()
	// 고스트 코스 S1 → R2 → S3 를 11명이 달려 공개된다
	for (let i = 0; i < 11; i++) await postWallet(env, ethers.Wallet.createRandom(), [lap(1000, 3000)])
	const X = h(0x55)
	const detour = (over: Record<string, unknown> = {}) =>
		run(
			[
				['screen', S1, 0],
				['region', X, 800],
				['screen', S3_HASH, 2000],
			],
			over
		)
	// 셋은 고스트를 보고도 R2 대신 X 로 가서 같은 결승에 닿는다
	const walkers = [0, 1, 2].map(() => ethers.Wallet.createRandom())
	for (const w of walkers) {
		const res = await postWallet(env, w, [detour({ ghost: COURSE })])
		assert.deepEqual(res.body.results[0].route, { ghost: COURSE, premium: 0, discount: 1 })
	}
	// 하나는 고스트를 그대로 따라간다
	const follower = await postWallet(env, ethers.Wallet.createRandom(), [lap(1100, 2900, { ghost: COURSE })])
	assert.deepEqual(follower.body.results[0].route, { ghost: COURSE, premium: 1, discount: 0 })
	const tally = (hash: string) => db.rows(`SELECT "type", SUM("n") AS n FROM nav_counts WHERE "hash" = ?1 GROUP BY "type" ORDER BY "type"`, hash)
	// 판정 수는 체크포인트 id 로 센다
	assert.deepEqual(tally(CP_R2), [
		{ type: -1, n: 3 },
		{ type: 1, n: 1 },
	])
	assert.deepEqual(tally(checkpointId({ kind: 'region', hash: X })), [], '런에만 있는 체크포인트는 판정하지 않는다')
	// 같은 사람이 같은 길을 다시: 이미 등록한 코스라 판정하지 않는다
	const again = await postWallet(env, walkers[0], [detour({ ghost: COURSE })], NOW + MINUTE)
	assert.equal(again.body.results[0].duplicate, true)
	assert.equal(again.body.results[0].route, null)

	// 판정하지 않는 경우: 결승이 다름 · 공개되지 않은 코스 · 다른 앱
	const elsewhere = run(
		[
			['screen', S1, 0],
			['region', R2, 900],
			['screen', h(0x66), 2000],
		],
		{ ghost: COURSE }
	)
	const quiet = ethers.Wallet.createRandom()
	const res = await postWallet(env, quiet, [elsewhere, courseOf(1, { ghost: courseOf(0).course }), courseOf(2, { ghost: COURSE }, APP2)])
	assert.deepEqual(
		res.body.results.map((r: any) => r.route),
		[null, null, null]
	)
	assert.deepEqual(tally(CP_R2), [
		{ type: -1, n: 3 },
		{ type: 1, n: 1 },
	])
	// 고스트 없이 달린 런에는 route 칸이 없다
	assert.equal('route' in (await postWallet(env, quiet, [courseOf(3)])).body.results[0], false)
	// ghost 형식이 틀리면 invalid
	assert.equal((await postWallet(env, quiet, [courseOf(4, { ghost: 'nope' })])).body.results[0].status, 'invalid')

	// 고스트를 보는 사람에게: 체크포인트마다 따라갔는가 · 비켜 갔는가
	const others = (await getGuest(env)).body.others
	const ghost = others.find((c: any) => c.course === COURSE)
	assert.deepEqual(ghost.checkpoints, [
		{ premium: 0, discount: 0, tier: 'unrated' },
		{ premium: 1, discount: 3, tier: 'discount' },
		{ premium: 0, discount: 0, tier: 'unrated' },
	])
})

// 웹 코스: 페이지(링크값) · 요소(DOM 셀렉터)와 제스처. crates/pd-nav trial.rs 와 같은 기준값
const WEB_COURSE = '0xe8c5279f506af6687f43eee19360173c51f44cce'
const fill = (c: string) => '0x' + c.repeat(40)
const webRun = (pressGesture: string | null, over: Record<string, unknown> = {}) =>
	run(
		[
			['page', fill('a'), 0],
			pressGesture ? ['element', fill('b'), 400, pressGesture] : ['element', fill('b'), 400],
			['page', fill('a'), 900, 'scroll-down'],
			['element', fill('c'), 1500, 'swipe-left'],
			['page', fill('d'), 2100],
		],
		over
	)

test('웹 코스는 페이지 · 요소와 제스처로 정하고, 코스 해시는 기기(Rust)와 같다', async () => {
	const { env } = setup()
	assert.equal(webRun(null).course, WEB_COURSE)
	assert.equal(stepKey({ kind: 'page', hash: fill('a'), gesture: 'scroll-down' }), `page:${fill('a')}@scroll-down`)
	// 누름(press)은 붙지 않는다: 마우스 · 손가락으로 누른 사람이 같은 코스
	const pressed = webRun('press')
	pressed.course = WEB_COURSE
	const res = await postSession(env, [webRun(null), pressed])
	assert.deepEqual(statuses(res.body), ['stored', 'repeat'])
	const mine = (await getSession(env)).body.mine
	assert.equal(mine[0].course, WEB_COURSE)
	assert.deepEqual(
		mine[0].steps,
		withIds([
			{ kind: 'page', hash: fill('a') },
			{ kind: 'element', hash: fill('b') },
			{ kind: 'page', hash: fill('a'), gesture: 'scroll-down' },
			{ kind: 'element', hash: fill('c'), gesture: 'swipe-left' },
			{ kind: 'page', hash: fill('d') },
		])
	)
	// 제스처가 다르면 같은 해시라도 다른 체크포인트 id
	assert.notEqual(mine[0].steps[0].id, mine[0].steps[2].id)
	// 같은 요소라도 스와이프로 지나면 다른 코스
	assert.notEqual(webRun('swipe-up').course, WEB_COURSE)

	const bad = [
		run([
			['page', fill('a'), 0],
			['link' as Kind, fill('b'), 100],
		]),
		run([
			['page', fill('a'), 0],
			['element', fill('b'), 100, 'pinch'],
		]),
		run([
			['page', fill('a'), 0, 'scroll-down'],
			['page', fill('a'), 100, 'scroll-down'],
		]),
	]
	const invalid = await postSession(env, bad)
	assert.deepEqual(statuses(invalid.body), ['invalid', 'invalid', 'invalid'])
	assert.match(invalid.body.results[0].error, /screen, region, page, element/)
	assert.match(invalid.body.results[1].error, /gesture/)
	assert.match(invalid.body.results[2].error, /repeat/)
	// 기기가 체크포인트 id 를 함께 보내면 서버가 다시 계산해 맞춰 본다
	const wrongId = webRun(null)
	;(wrongId.steps[1] as any).id = CP_R2
	assert.match((await postSession(env, [wrongId])).body.results[0].error, /step id/)
	const rightId = webRun(null)
	;(rightId.steps[1] as any).id = checkpointId({ kind: 'element', hash: fill('b') })
	assert.equal((await postSession(env, [rightId])).body.results[0].status, 'repeat')
})

test('고스트 따라가기 판정은 제스처까지 본다: 스와이프할 곳을 눌러 지나면 그 체크포인트는 비켜 간 것이다', async () => {
	const { db, env } = setup()
	assert.deepEqual(
		routeMarks(
			[
				{ kind: 'page', hash: fill('a') },
				{ kind: 'element', hash: fill('c') },
				{ kind: 'page', hash: fill('d') },
			],
			[
				{ kind: 'page', hash: fill('a') },
				{ kind: 'element', hash: fill('c'), gesture: 'swipe-left' },
				{ kind: 'page', hash: fill('d') },
			]
		),
		[{ step: { kind: 'element', hash: fill('c'), gesture: 'swipe-left' }, type: -1 }]
	)
	for (let i = 0; i < 11; i++) await postWallet(env, ethers.Wallet.createRandom(), [webRun(null)])
	const tapper = run(
		[
			['page', fill('a'), 0],
			['element', fill('b'), 400],
			['page', fill('a'), 900, 'scroll-down'],
			['element', fill('c'), 1500],
			['page', fill('d'), 2100],
		],
		{ ghost: WEB_COURSE }
	)
	const res = await postWallet(env, ethers.Wallet.createRandom(), [tapper])
	assert.deepEqual(res.body.results[0].route, { ghost: WEB_COURSE, premium: 2, discount: 1 })
	const counts = Object.fromEntries(db.rows(`SELECT "hash", "type", "n" FROM nav_counts`).map((r) => [r.hash, [r.type, r.n]]))
	assert.deepEqual(counts, {
		[checkpointId({ kind: 'page', hash: fill('a'), gesture: 'scroll-down' })]: [1, 1],
		[checkpointId({ kind: 'element', hash: fill('b') })]: [1, 1],
		[checkpointId({ kind: 'element', hash: fill('c'), gesture: 'swipe-left' })]: [-1, 1],
	})
	const ghost = (await getGuest(env)).body.others.find((c: any) => c.course === WEB_COURSE)
	assert.deepEqual(
		ghost.checkpoints.map((c: any) => c.discount),
		[0, 0, 0, 1, 0]
	)
	assert.deepEqual(
		ghost.checkpoints.map((c: any) => c.premium),
		[0, 1, 1, 0, 0]
	)
})

test('같은 길은 누가 달려도 같은 코스 id 라 두 번 등록되지 않고, 다른 체크포인트가 나오면 이전 코스 id 에 이어 새 코스가 생긴다', async () => {
	const { db, env } = setup()
	const a = ethers.Wallet.createRandom()
	const b = ethers.Wallet.createRandom()
	assert.deepEqual((await postWallet(env, a, [lap(1000, 3000)])).body.results[0], { course: COURSE, status: 'stored', created: true, duplicate: false })
	// 다른 사람이 같은 길: 새 코스가 아니라 같은 코스에 그 사람의 등록이 하나 는다
	assert.deepEqual((await postWallet(env, b, [lap(1200, 2800)])).body.results[0], { course: COURSE, status: 'stored', created: false, duplicate: false })
	assert.equal(db.rows(`SELECT * FROM nav_courses`).length, 1)
	assert.equal(course(db, COURSE).runs, 2)
	// 결승 전에서 갈라진 길: 갈라지기 전까지의 코스 id 를 이어받은 새 코스
	const fork = run([
		['screen', S1, 0],
		['region', R2, 900],
		['screen', h(0x66), 2000],
	])
	const forked = (await postWallet(env, a, [fork])).body.results[0]
	assert.equal(forked.created, true)
	const mine = (await send(env, await walletRequest(a, 'GET', undefined, NOW, `?app=${APP}`))).body.mine
	const stored = mine.find((m: any) => m.course === forked.course)
	assert.deepEqual(
		stored.steps.map((s: any) => s.course),
		[CHAIN[0], CHAIN[1], forked.course]
	)
	// 중간에서 끝난 짧은 길(S1 → R2)은 긴 코스의 중간 id 와 같은 코스 id 다
	const short = run([
		['screen', S1, 0],
		['region', R2, 900],
	])
	assert.equal(short.course, CHAIN[1])
	// 기기가 보낸 체크포인트마다의 코스 id 는 다시 계산해 맞춰 본다
	const wrong = lap(1000, 3000)
	;(wrong.steps[1] as any).course = CHAIN[0]
	assert.match((await postWallet(env, a, [wrong])).body.results[0].error, /step course/)
	const right = lap(1000, 3000)
	right.steps.forEach((s: any, i: number) => (s.course = CHAIN[i]))
	assert.equal((await postWallet(env, a, [right])).body.results[0].status, 'repeat')
})
