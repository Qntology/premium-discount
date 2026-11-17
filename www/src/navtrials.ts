// 접근성 내비게이션 타임트라이얼(trial/1). 보관 규칙은 cron/src/navtrials.ts(TRIAL_POLICY), 표는 migrations/0007_nav_trials.sql,
// 기기 쪽은 crates/premium-discount-nav trial.rs 다.
//
// POST   /nav/trials              본문 { "runs": [trial/1, ...] } (최대 10개). 런마다 결과를 보낸 순서대로 돌려준다
// GET    /nav/trials?app=0x…      그 앱 · 웹의 내 콜렉션(인증했을 때)과 다른 사용자의 공개 코스(고스트)
// DELETE /nav/trials[?app=0x…]    내 콜렉션을 지운다(앱 하나 또는 전부). 계정 없는 합계 · 개수 · 대표 벡터는 남는다
//
// 인증은 두 갈래 중 하나다 (auth.ts). 지갑 헤더(x-pd-address · x-pd-time · x-pd-signature)가 하나라도 있으면 지갑으로만,
// 없으면 세션(QR 메일 로그인)으로 확인한다. POST · DELETE 는 인증이 있어야 하고, GET 은 인증이 없으면 다른 사용자의 기록만 준다.
//
// 런마다의 결과(status):
// - stored: 새 코스를 콜렉션에 넣었다
// - repeat: 내 콜렉션에 이미 있던 코스를 다시 달렸다. 내 기록(내가 달린 횟수 · 최고 · 마지막)만 고치고, best 면 최고 기록을 갈았다
// - replaced: 새 코스가 11번째라 가장 오래 달리지 않은 코스(evicted)를 덮어썼다
// - expired: 지갑 런인데 날짜가 30일 기한 밖이다
// - invalid: 형식이 틀렸다 (error 에 이유)
// created 는 그 코스 id 가 서버에 처음 생겼는지다. 같은 길은 늘 같은 코스 id 라 서버에 코스가 두 번 생기지 않고, 체크포인트가
// 하나라도 다르면 새 코스가 생긴다.
// duplicate 는 이 사용자가 이 코스를 이미 등록했는지다. 사용자 코스 id = hashId(<계정 키> + <코스 id>) (웹의 itemId =
// hashId(team + cc + link) 와 같은 방식)를 nav_registrations 에 두고, 있으면 등록하지 않는다. 그래서 한 사용자는 같은 코스를
// 한 번만 등록한다: 공개 합계(등록한 사용자 수 · premium · discount · 평균)와 판정(nav_counts · 고스트 따라가기)에는 처음 한 번만
// 들어가고, 다시 달린 런은 내 콜렉션의 내 기록만 고친다. 콜렉션에서 밀려나거나 지운 코스를 다시 달려도 마찬가지다.
//
// id 정의 (crates/premium-discount-nav trial.rs 와 같다). 웹의 hashId(cc + pathname) 처럼 앞의 id 에 다음 조각을 이어 붙여 해싱한다(구분자 없음)
// - 체크포인트 id = hashId(<종류> + <해시> + <제스처>)
// - 코스 id 체인: 첫 체크포인트에서 hashId(<앱 id> + <체크포인트 id>), 다음부터 hashId(<이전 코스 id> + <체크포인트 id>).
//   결승의 값이 그 코스의 id 다. 저장하는 steps 에는 체크포인트마다 id 와 그 지점까지의 코스 id(course)가 붙는다
//
// 고스트 따라가기 판정(route): 런이 ghost(따라간 다른 사용자의 공개 코스)를 밝히면, 시작 · 결승이 그 코스와 같을 때 그 코스의
// 중간 체크포인트 중 런이 지난 것은 premium, 지나지 않은(다른 길로 간) 것은 discount 로 nav_counts 에 센다(체크포인트 id).
// 공개되지 않은 코스 · 다른 앱의 코스 · 결승이 다른 런은 판정하지 않는다(route: null).
// 기기 쪽 같은 규칙은 crates/premium-discount-nav trial.rs 의 route_marks 다.
//
// 체크포인트 종류: 앱은 screen(UIA 구조 지문) · region(영역), 웹은 page(링크값 = URL 틀) · element(DOM 셀렉터 틀).
// 스와이프 · 스크롤로 지난 체크포인트에는 gesture 가 붙고(swipe-left … scroll-down), 같은 해시라도 다른 체크포인트다.
// 누름(press: 클릭 · 탭)은 기본 동작이라 붙이지 않는다(붙어 오면 지운다). 서버는 해시와 제스처 이름만 받는다(좌표 · 시각 없음).

import { ethers } from 'ethers'

import { readSessionCredentials, verifiedAddress, verifyWalletRequest } from './auth'
import { corsHeaders, type EvalEnv } from './evaluations'
import { isRateLimited, RETRY_AFTER_SECONDS } from './ratelimit'
import { NAV_FIRST_DAY, NAV_RECIPE, TRIAL_POLICY, TRIAL_SCHEMA, dayOf, tier, walletOldestDay } from '../../cron/src/navtrials'

export const NAV_TRIALS_PATH = '/nav/trials'
// 한 요청에 담는 런 수 (콜렉션 하나 크기)
const MAX_RUNS = TRIAL_POLICY.maxCourses
// 본문 크기 상한. 런 하나는 체크포인트 10개와 벡터들을 담는다
const MAX_BODY_BYTES = 4 * 1024 * 1024
const MAX_VECTORS = 4
const MAX_DIMS = 2048
const HASH_RE = /^0x[0-9a-f]{40}$/
const SPACE_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/
// 기기는 L2 정규화해 보낸다. float32 반올림만큼만 허용한다
const NORM_TOLERANCE = 1e-3
// IN 바인딩 한 번의 수 (D1 문장당 바인딩 100 이하)
const IN_CHUNK = 90
// 다른 사용자의 공개 코스를 앱마다 몇 개까지 보여 주는가 (많이 달린 순)
const OTHERS_LIMIT = 10
const WALLET_HEADERS = ['x-pd-address', 'x-pd-time', 'x-pd-signature']

export type AccountKind = 'session' | 'wallet'
export interface NavAccount {
	kind: AccountKind
	// accountKey 의 값. 주소 · 이메일은 표에 남지 않는다
	key: string
}
const STEP_KINDS = ['screen', 'region', 'page', 'element'] as const
type StepKind = (typeof STEP_KINDS)[number]
// crates/premium-discount-nav gesture.rs 의 Gesture 이름. press 는 체크포인트에 남기지 않는다
const GESTURES = ['swipe-left', 'swipe-right', 'swipe-up', 'swipe-down', 'scroll-up', 'scroll-down', 'scroll-left', 'scroll-right'] as const
type Gesture = (typeof GESTURES)[number]
interface StepRef {
	kind: StepKind
	hash: string
	gesture?: Gesture
	// 체크포인트 id (checkpointId)와 시작부터 이 체크포인트까지의 코스 id (courseChain). 저장하는 steps 에는 늘 들어 있다
	id?: string
	course?: string
}

// 체크포인트 키: <종류>:<해시> 또는 <종류>:<해시>@<제스처> (crates/premium-discount-nav StepRef::key)
export function stepKey(s: StepRef): string {
	return s.gesture ? `${s.kind}:${s.hash}@${s.gesture}` : `${s.kind}:${s.hash}`
}
// hashId 는 하나에 0.4 ms 남짓 든다. 같은 체크포인트 · 같은 앞부분이 런 · 요청마다 되풀이되므로 isolate 안에 담아 둔다
const HASH_IDS = new Map<string, string>()
const HASH_IDS_MAX = 20_000

function cachedHashId(text: string): string {
	let id = HASH_IDS.get(text)
	if (!id) {
		id = hashId(text)
		if (HASH_IDS.size >= HASH_IDS_MAX) HASH_IDS.clear()
		HASH_IDS.set(text, id)
	}
	return id
}

// 체크포인트 id = hashId(<종류> + <해시> + <제스처>) (crates/premium-discount-nav checkpoint_id). 해시가 0x + 40자로 길이가 정해져 있어
// 구분자 없이 이어도 겹치지 않는다
export function checkpointId(s: StepRef): string {
	return cachedHashId(`${s.kind}${s.hash}${s.gesture ?? ''}`)
}

// 코스 id 체인 (crates/premium-discount-nav course_chain): 첫 값은 hashId(<앱 id> + <체크포인트 id>), 다음부터 hashId(<이전 코스 id> + <체크포인트 id>).
// 앞부분이 같은 코스는 그 지점까지 같은 id 를 갖고, 다른 체크포인트부터 갈라진다
export function courseChain(app: string, steps: StepRef[]): string[] {
	let previous = app
	return steps.map((s) => (previous = cachedHashId(`${previous}${checkpointId(s)}`)))
}
interface ParsedRun {
	app: string
	course: string
	steps: StepRef[]
	splits: number[]
	total: number
	// 1 premium · -1 discount · 0 결정 없음
	type: 1 | -1 | 0
	day: number
	vectors: { hash: string; space: string; vector: Float32Array }[]
	// 따라간 다른 사용자의 코스 (없으면 null)
	ghost: string | null
}
export type RunStatus = 'stored' | 'repeat' | 'replaced' | 'expired' | 'invalid'
export interface RunResult {
	course: string | null
	status: RunStatus
	// 그 코스 id 가 서버에 처음 생겼는가
	created?: boolean
	// 이 사용자가 이 코스를 이미 등록했는가 (그러면 공개 합계 · 판정에 들어가지 않는다)
	duplicate?: boolean
	best?: boolean
	evicted?: string
	// 고스트 따라가기 판정: 따라간 체크포인트 수(premium) · 비켜 간 체크포인트 수(discount). 판정하지 않았으면 null
	route?: { ghost: string; premium: number; discount: number } | null
	error?: string
}

class RunInvalid extends Error {}

// premium.discount 의 hashId: ethers.computeAddress(ethers.hashMessage(text)) (crates/premium-discount-identity hash_id 와 같다)
export function hashId(text: string): string {
	return ethers.computeAddress(ethers.hashMessage(text)).toLowerCase()
}

// 코스 id: 코스 id 체인의 마지막 값 (crates/premium-discount-nav course_hash)
export function courseHash(app: string, steps: StepRef[]): string {
	return courseChain(app, steps).at(-1) ?? ''
}

// 계정 키 = HMAC-SHA256(AUTHOR_KEY_SECRET, "nav-account|<종류>|<주소>"). 공개 작성자 키(projection.ts authorKey: 주소만 넣는다)와
// 넣는 글이 달라 두 키를 서로 맞춰 볼 수 없고, 비밀을 모르면 키에서 주소를 되찾을 수 없다
export async function accountKey(secret: string, kind: AccountKind, address: string): Promise<string> {
	if (!secret) throw new Error('AUTHOR_KEY_SECRET is not configured')
	const enc = new TextEncoder()
	const key = await crypto.subtle.importKey('raw', enc.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign'])
	const sig = await crypto.subtle.sign('HMAC', key, enc.encode(`nav-account|${kind}|${address.toLowerCase()}`))
	return Array.from(new Uint8Array(sig), (b) => b.toString(16).padStart(2, '0')).join('')
}

function parseHash(value: unknown, field: string): string {
	const hash = typeof value === 'string' ? value.trim().toLowerCase() : ''
	if (!HASH_RE.test(hash)) throw new RunInvalid(`${field} must be 0x + 40 hex`)
	return hash
}

function parseVectors(value: unknown, hash: string): ParsedRun['vectors'] {
	if (value === undefined) return []
	if (!Array.isArray(value) || value.length > MAX_VECTORS) throw new RunInvalid(`vectors must hold up to ${MAX_VECTORS} entries`)
	const out: ParsedRun['vectors'] = []
	for (const entry of value as unknown[]) {
		const v = (entry && typeof entry === 'object' ? entry : {}) as Record<string, unknown>
		const space = typeof v.space === 'string' ? v.space : ''
		if (!SPACE_RE.test(space) || out.some((x) => x.space === space)) throw new RunInvalid('spaces must be named and distinct')
		const raw = v.vector
		if (!Array.isArray(raw) || raw.length === 0 || raw.length > MAX_DIMS || !raw.every((x) => typeof x === 'number' && Number.isFinite(x))) {
			throw new RunInvalid(`vector must hold 1 to ${MAX_DIMS} finite numbers`)
		}
		const vector = Float32Array.from(raw as number[])
		let norm = 0
		for (const x of vector) norm += x * x
		if (Math.abs(Math.sqrt(norm) - 1) > NORM_TOLERANCE) throw new RunInvalid('vector must be L2-normalized')
		out.push({ hash, space, vector })
	}
	return out
}

// trial/1 런 하나를 확인한다 (crates/premium-discount-nav trial.rs 의 Run 직렬화와 같은 모양). 코스 해시는 체크포인트로 다시 계산해 맞춰 본다
export function parseRun(value: unknown, today: number): ParsedRun {
	if (!value || typeof value !== 'object' || Array.isArray(value)) throw new RunInvalid('run must be an object')
	const r = value as Record<string, unknown>
	if (r.schema !== TRIAL_SCHEMA) throw new RunInvalid(`schema must be ${TRIAL_SCHEMA}`)
	if (r.recipe !== NAV_RECIPE) throw new RunInvalid(`recipe must be ${NAV_RECIPE}`)
	const app = parseHash(r.app, 'app')
	const day = r.day
	if (typeof day !== 'number' || !Number.isSafeInteger(day) || day < NAV_FIRST_DAY || day > today + 1) {
		throw new RunInvalid('day must be a UTC day number between 2026-01-01 and tomorrow')
	}
	const type = r.type === 'premium' ? 1 : r.type === 'discount' ? -1 : r.type === null ? 0 : null
	if (type === null) throw new RunInvalid('type must be premium, discount or null')
	if (!Array.isArray(r.steps) || r.steps.length < 2 || r.steps.length > TRIAL_POLICY.maxSteps) {
		throw new RunInvalid(`steps must hold 2 to ${TRIAL_POLICY.maxSteps} checkpoints (the device keeps the last ${TRIAL_POLICY.maxSteps})`)
	}
	const steps: StepRef[] = []
	const splits: number[] = []
	const vectors: ParsedRun['vectors'] = []
	// 기기가 함께 보낸 체크포인트마다의 코스 id (있으면 다시 계산해 맞춰 본다)
	const sentCourses: unknown[] = []
	for (const entry of r.steps as unknown[]) {
		const s = (entry && typeof entry === 'object' ? entry : {}) as Record<string, unknown>
		const kind = STEP_KINDS.find((k) => k === s.kind)
		if (!kind) throw new RunInvalid(`step kind must be one of ${STEP_KINDS.join(', ')}`)
		const hash = parseHash(s.hash, 'step hash')
		let gesture: Gesture | undefined
		if (s.gesture !== undefined && s.gesture !== null && s.gesture !== 'press') {
			gesture = GESTURES.find((g) => g === s.gesture)
			if (!gesture) throw new RunInvalid(`gesture must be press or one of ${GESTURES.join(', ')}`)
		}
		const bare: StepRef = gesture ? { kind, hash, gesture } : { kind, hash }
		const step: StepRef = { ...bare, id: checkpointId(bare) }
		if (s.id !== undefined && s.id !== step.id) throw new RunInvalid('step id does not match the checkpoint')
		const split = s.split_ms
		if (typeof split !== 'number' || !Number.isSafeInteger(split) || split < 0 || split % TRIAL_POLICY.splitUnitMs !== 0) {
			throw new RunInvalid(`split_ms must be a multiple of ${TRIAL_POLICY.splitUnitMs}`)
		}
		const prev = steps.at(-1)
		if (prev && stepKey(prev) === stepKey(step)) throw new RunInvalid('the same checkpoint must not repeat in a row')
		if (splits.length === 0 ? split !== 0 : split < splits[splits.length - 1]) {
			throw new RunInvalid('splits must start at 0 and never go down')
		}
		steps.push(step)
		splits.push(split)
		sentCourses.push(s.course)
		vectors.push(...parseVectors(s.vectors, hash))
	}
	const total = splits[splits.length - 1]
	if (r.total_ms !== total) throw new RunInvalid('total_ms must equal the last split')
	if (total > TRIAL_POLICY.maxRunMs) throw new RunInvalid('a run must finish within an hour')
	const course = parseHash(r.course, 'course')
	const chain = courseChain(app, steps)
	if (course !== chain.at(-1)) throw new RunInvalid('course does not match the checkpoints')
	steps.forEach((step, i) => {
		if (sentCourses[i] !== undefined && sentCourses[i] !== chain[i]) throw new RunInvalid('step course does not match the checkpoints so far')
		step.course = chain[i]
	})
	const ghost = r.ghost === undefined || r.ghost === null ? null : parseHash(r.ghost, 'ghost')
	return { app, course, steps, splits, total, type, day, vectors, ghost }
}

const sameStep = (a: StepRef, b: StepRef) => stepKey(a) === stepKey(b)

// 고스트 따라가기 판정 (crates/premium-discount-nav route_marks 와 같은 규칙). 시작과 결승이 고스트와 같을 때만, 고스트의 중간 체크포인트마다
// 런이 지났으면 premium(1), 아니면 discount(-1). 런에만 있는 체크포인트는 판정하지 않는다. 시작 · 결승이 다르면 null
export function routeMarks(run: StepRef[], ghost: StepRef[]): { step: StepRef; type: 1 | -1 }[] | null {
	if (run.length === 0 || ghost.length === 0) return null
	if (!sameStep(run[0], ghost[0]) || !sameStep(run[run.length - 1], ghost[ghost.length - 1])) return null
	const out: { step: StepRef; type: 1 | -1 }[] = []
	for (const step of ghost.slice(1, -1)) {
		if (out.some((m) => sameStep(m.step, step))) continue
		out.push({ step, type: run.some((s) => sameStep(s, step)) ? 1 : -1 })
	}
	return out
}

interface CollectionRow {
	course: string
	steps: string
	runs: number
	premium: number
	discount: number
	best: number[]
	last: number[]
	last_day: number
	touched: number
}
interface CourseRow {
	course: string
	app: string
	steps: string
	runs: number
	premium: number
	discount: number
	record: number[]
	sums: number[]
	last_day: number
}
interface CollectionRecord {
	course: string
	steps: string
	runs: number
	premium: number
	discount: number
	best: string
	last: string
	last_day: number
	touched: number
}
interface CourseRecord {
	course: string
	app: string
	steps: string
	runs: number
	premium: number
	discount: number
	record: string
	sums: string
	last_day: number
}

function chunks<T>(xs: T[], n: number): T[][] {
	const out: T[][] = []
	for (let i = 0; i < xs.length; i += n) out.push(xs.slice(i, i + n))
	return out
}

async function readCourses(db: D1Database, courses: string[]): Promise<Map<string, CourseRow>> {
	const out = new Map<string, CourseRow>()
	for (const part of chunks(courses, IN_CHUNK)) {
		const marks = part.map((_, i) => `?${i + 1}`).join(', ')
		const rows = await db.prepare(`SELECT * FROM nav_courses WHERE "course" IN (${marks})`).bind(...part).all<CourseRecord>()
		for (const r of rows.results) {
			out.set(r.course, { ...r, record: JSON.parse(r.record), sums: JSON.parse(r.sums) })
		}
	}
	return out
}

// 런들을 계정의 콜렉션에 넣는다. 순서: 확인 → (지갑) 기한 지난 내 코스 지우기 → 앱마다 콜렉션 읽기 → 보낸 순서대로 적용 →
// 계정 없는 합계 · 개수 · 대표 벡터와 함께 한 번에 쓰기
export async function storeRuns(db: D1Database, account: NavAccount, items: unknown[], nowMs: number): Promise<RunResult[]> {
	const today = dayOf(nowMs)
	const oldest = account.kind === 'wallet' ? walletOldestDay(nowMs) : NAV_FIRST_DAY
	const results: RunResult[] = []
	const runs: (ParsedRun | null)[] = []
	for (const item of items) {
		try {
			const run = parseRun(item, today)
			const expired = run.day < oldest
			runs.push(expired ? null : run)
			results.push({ course: run.course, status: expired ? 'expired' : 'stored' })
		} catch (err) {
			if (!(err instanceof RunInvalid)) throw err
			runs.push(null)
			results.push({ course: null, status: 'invalid', error: err.message })
		}
	}
	if (runs.every((r) => r === null)) return results

	// 지갑: 기한이 지난 내 코스부터 지운다 (모든 지갑은 cron 도 1분마다 지운다)
	if (account.kind === 'wallet') {
		await db.prepare(`DELETE FROM nav_trials WHERE "account" = ?1 AND "last_day" < ?2`).bind(account.key, oldest).run()
	}

	// 앱마다 내 콜렉션 (최대 10행)
	const collections = new Map<string, Map<string, CollectionRow>>()
	for (const app of new Set(runs.flatMap((r) => (r ? [r.app] : [])))) {
		const rows = await db.prepare(`SELECT * FROM nav_trials WHERE "account" = ?1 AND "app" = ?2`).bind(account.key, app).all<CollectionRecord>()
		collections.set(app, new Map(rows.results.map((r) => [r.course, { ...r, best: JSON.parse(r.best), last: JSON.parse(r.last) }])))
	}

	// 보낸 순서대로 적용한다. touched 는 받은 시각(ms) × 1000 + 요청 안의 순서라, 같은 요청 안에서도 뒤의 런이 더 최근이다
	const dirty = new Map<string, { app: string; row: CollectionRow }>()
	const evicted: { app: string; course: string }[] = []
	runs.forEach((run, i) => {
		if (!run) return
		const collection = collections.get(run.app)!
		const touched = nowMs * 1000 + i
		const premium = run.type === 1 ? 1 : 0
		const discount = run.type === -1 ? 1 : 0
		const row = collection.get(run.course)
		if (row) {
			const best = run.total < row.best[row.best.length - 1]
			row.runs += 1
			row.premium += premium
			row.discount += discount
			if (best) row.best = run.splits
			row.last = run.splits
			row.last_day = Math.max(row.last_day, run.day)
			row.touched = touched
			results[i] = { course: run.course, status: 'repeat', best }
			dirty.set(`${run.app}|${run.course}`, { app: run.app, row })
		} else {
			let status: RunStatus = 'stored'
			let gone: string | undefined
			if (collection.size >= TRIAL_POLICY.maxCourses) {
				// 가장 오래 달리지 않은 코스 자리를 덮어쓴다
				const oldestRow = [...collection.values()].reduce((a, b) => (b.touched < a.touched ? b : a))
				collection.delete(oldestRow.course)
				dirty.delete(`${run.app}|${oldestRow.course}`)
				evicted.push({ app: run.app, course: oldestRow.course })
				status = 'replaced'
				gone = oldestRow.course
			}
			const fresh: CollectionRow = {
				course: run.course,
				steps: JSON.stringify(run.steps),
				runs: 1,
				premium,
				discount,
				best: run.splits,
				last: run.splits,
				last_day: run.day,
				touched,
			}
			collection.set(run.course, fresh)
			dirty.set(`${run.app}|${run.course}`, { app: run.app, row: fresh })
			results[i] = { course: run.course, status, ...(gone ? { evicted: gone } : {}) }
		}
	})

	// 사용자마다 코스 등록은 한 번: 사용자 코스 id = hashId(<계정 키> + <코스 id>)
	const valid = runs.filter((r): r is ParsedRun => r !== null)
	const registrationOf = (run: ParsedRun) => cachedHashId(`${account.key}${run.course}`)
	const registered = new Set<string>()
	for (const part of chunks([...new Set(valid.map(registrationOf))], IN_CHUNK)) {
		const marks = part.map((_, i) => `?${i + 1}`).join(', ')
		const rows = await db.prepare(`SELECT "id" FROM nav_registrations WHERE "id" IN (${marks})`).bind(...part).all<{ id: string }>()
		for (const r of rows.results) registered.add(r.id)
	}
	// 이번에 처음 등록하는 런 (같은 요청 안에서 같은 코스를 두 번 보내면 앞의 것만)
	const fresh = new Set<ParsedRun>()
	runs.forEach((run, i) => {
		if (!run) return
		const id = registrationOf(run)
		results[i].duplicate = registered.has(id)
		if (!registered.has(id)) {
			registered.add(id)
			fresh.add(run)
		}
	})

	// 계정 없는 코스 합계: 코스 id 가 없으면 새로 만들고(created), 있으면 처음 등록하는 런만 더한다(등록한 사용자 수 · 결정 수 · 구간 합)
	const totals = await readCourses(db, [...new Set(valid.map((r) => r.course))])
	const changed = new Set<string>()
	runs.forEach((run, i) => {
		if (!run) return
		const prev = totals.get(run.course)
		results[i].created = !prev
		if (!fresh.has(run)) return
		if (!prev) {
			totals.set(run.course, {
				course: run.course,
				app: run.app,
				steps: JSON.stringify(run.steps),
				runs: 1,
				premium: run.type === 1 ? 1 : 0,
				discount: run.type === -1 ? 1 : 0,
				record: run.splits,
				sums: run.splits,
				last_day: run.day,
			})
			changed.add(run.course)
			return
		}
		if (run.total < prev.record[prev.record.length - 1]) prev.record = run.splits
		prev.runs += 1
		prev.premium += run.type === 1 ? 1 : 0
		prev.discount += run.type === -1 ? 1 : 0
		prev.sums = prev.sums.map((s, k) => s + (run.splits[k] ?? 0))
		prev.last_day = Math.max(prev.last_day, run.day)
		changed.add(run.course)
	})

	const statements: D1PreparedStatement[] = []
	for (const { app, course } of evicted) {
		// 같은 요청에서 다시 들어온 코스는 dirty 에 있으니 아래 upsert 가 되살린다
		statements.push(db.prepare(`DELETE FROM nav_trials WHERE "account" = ?1 AND "app" = ?2 AND "course" = ?3`).bind(account.key, app, course))
	}
	for (const { app, row } of dirty.values()) {
		statements.push(
			db
				.prepare(
					`INSERT INTO nav_trials ("account", "kind", "app", "course", "steps", "runs", "premium", "discount", "best", "last", "last_day", "touched")
					 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
					 ON CONFLICT ("account", "app", "course") DO UPDATE SET "runs" = excluded."runs", "premium" = excluded."premium",
					   "discount" = excluded."discount", "best" = excluded."best", "last" = excluded."last", "last_day" = excluded."last_day",
					   "touched" = excluded."touched"`
				)
				.bind(account.key, account.kind, app, row.course, row.steps, row.runs, row.premium, row.discount, JSON.stringify(row.best), JSON.stringify(row.last), row.last_day, row.touched)
		)
	}
	for (const t of totals.values()) {
		if (!changed.has(t.course)) continue
		statements.push(
			db
				.prepare(
					`INSERT INTO nav_courses ("course", "app", "steps", "runs", "premium", "discount", "record", "sums", "last_day")
					 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
					 ON CONFLICT ("course") DO UPDATE SET "runs" = excluded."runs", "premium" = excluded."premium", "discount" = excluded."discount",
					   "record" = excluded."record", "sums" = excluded."sums", "last_day" = excluded."last_day"`
				)
				.bind(t.course, t.app, t.steps, t.runs, t.premium, t.discount, JSON.stringify(t.record), JSON.stringify(t.sums), t.last_day)
		)
	}
	const countUp = (hash: string, day: number, type: 1 | -1) =>
		db
			.prepare(
				`INSERT INTO nav_counts ("hash", "day", "type", "recipe", "n") VALUES (?1, ?2, ?3, ?4, 1)
				 ON CONFLICT ("hash", "day", "type", "recipe") DO UPDATE SET "n" = "n" + 1`
			)
			.bind(hash, day, type, NAV_RECIPE)
	for (const run of fresh) {
		statements.push(db.prepare(`INSERT INTO nav_registrations ("id", "day") VALUES (?1, ?2) ON CONFLICT DO NOTHING`).bind(registrationOf(run), run.day))
		if (run.type !== 0) statements.push(countUp(run.course, run.day, run.type))
	}

	// 고스트 따라가기 판정. 고스트가 공개됐는지는 이 요청을 쓰기 전의 합계로 본다
	const ghosts = await readCourses(db, [...new Set(runs.flatMap((r) => (r?.ghost ? [r.ghost] : [])))])
	runs.forEach((run, i) => {
		if (!run?.ghost) return
		const ghost = ghosts.get(run.ghost)
		const marks =
			fresh.has(run) && ghost && ghost.app === run.app && ghost.runs >= TRIAL_POLICY.publishMinRuns
				? routeMarks(run.steps, JSON.parse(ghost.steps) as StepRef[])
				: null
		if (!marks) {
			results[i].route = null
			return
		}
		for (const m of marks) statements.push(countUp(checkpointId(m.step), run.day, m.type))
		results[i].route = { ghost: run.ghost, premium: marks.filter((m) => m.type === 1).length, discount: marks.filter((m) => m.type === -1).length }
	})
	for (const run of runs) {
		for (const v of run?.vectors ?? []) {
			statements.push(
				db
					.prepare(`INSERT INTO nav_vectors ("hash", "space", "dims", "vector", "day") VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING`)
					.bind(v.hash, v.space, v.vector.length, v.vector.buffer, run!.day)
			)
		}
	}
	if (statements.length > 0) await db.batch(statements)
	return results
}

// 앱의 내 콜렉션 (최근에 달린 순)
async function readMine(db: D1Database, account: NavAccount, app: string) {
	const rows = await db
		.prepare(`SELECT * FROM nav_trials WHERE "account" = ?1 AND "app" = ?2 ORDER BY "touched" DESC`)
		.bind(account.key, app)
		.all<CollectionRecord>()
	return rows.results.map((r) => ({
		course: r.course,
		steps: JSON.parse(r.steps) as StepRef[],
		runs: r.runs,
		premium: r.premium,
		discount: r.discount,
		tier: tier(r.premium, r.discount),
		best: JSON.parse(r.best) as number[],
		last: JSON.parse(r.last) as number[],
		last_day: r.last_day,
	}))
}

// 다른 사용자의 공개 코스: 많이 달린 순으로 앞의 10개와, 내 콜렉션에 있는 코스 중 공개된 것
async function readOthers(db: D1Database, app: string, mine: string[]) {
	const min = TRIAL_POLICY.publishMinRuns
	const top = await db
		.prepare(`SELECT * FROM nav_courses WHERE "app" = ?1 AND "runs" >= ?2 ORDER BY "runs" DESC, "course" LIMIT ?3`)
		.bind(app, min, OTHERS_LIMIT)
		.all<CourseRecord>()
	const rows = new Map(top.results.map((r) => [r.course, r]))
	const missing = mine.filter((c) => !rows.has(c))
	for (const [course, row] of await readCourses(db, missing)) {
		if (row.app === app && row.runs >= min) rows.set(course, { ...row, record: JSON.stringify(row.record), sums: JSON.stringify(row.sums) })
	}
	// 체크포인트마다의 따라가기 판정 수 (nav_counts, 모든 날짜)
	const tallies = new Map<string, { premium: number; discount: number }>()
	const stepHashes = [...new Set([...rows.values()].flatMap((r) => (JSON.parse(r.steps) as StepRef[]).map((s) => s.id ?? checkpointId(s))))]
	for (const part of chunks(stepHashes, IN_CHUNK)) {
		const marks = part.map((_, i) => `?${i + 2}`).join(', ')
		const counts = await db
			.prepare(`SELECT "hash", "type", SUM("n") AS "n" FROM nav_counts WHERE "recipe" = ?1 AND "hash" IN (${marks}) GROUP BY "hash", "type"`)
			.bind(NAV_RECIPE, ...part)
			.all<{ hash: string; type: number; n: number }>()
		for (const c of counts.results) {
			const t = tallies.get(c.hash) ?? { premium: 0, discount: 0 }
			if (c.type === 1) t.premium += Number(c.n)
			else t.discount += Number(c.n)
			tallies.set(c.hash, t)
		}
	}
	const unit = TRIAL_POLICY.splitUnitMs
	return [...rows.values()].map((r) => {
		const sums = JSON.parse(r.sums) as number[]
		const steps = JSON.parse(r.steps) as StepRef[]
		return {
			course: r.course,
			steps: JSON.parse(r.steps) as StepRef[],
			runs: r.runs,
			premium: r.premium,
			discount: r.discount,
			tier: tier(r.premium, r.discount),
			record: JSON.parse(r.record) as number[],
			average: sums.map((s) => Math.round(s / r.runs / unit) * unit),
			// steps 와 같은 순서: 다른 사용자들이 그 체크포인트를 따라갔는가(premium) · 비켜 갔는가(discount)
			checkpoints: steps.map((s) => {
				const t = tallies.get(s.id ?? checkpointId(s)) ?? { premium: 0, discount: 0 }
				return { ...t, tier: tier(t.premium, t.discount) }
			}),
		}
	})
}

function navCors(origin: string): Record<string, string> {
	return {
		...corsHeaders(origin),
		'Access-Control-Allow-Methods': 'GET, POST, DELETE, OPTIONS',
		'Access-Control-Allow-Headers': 'Content-Type, x-oauth-hash, x-oauth-token, ' + WALLET_HEADERS.join(', '),
	}
}

function reply(body: unknown, status: number, origin: string, extra: Record<string, string> = {}): Response {
	return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...navCors(origin), ...extra } })
}

const hasWalletHeaders = (request: Request) => WALLET_HEADERS.some((h) => request.headers.has(h))

// 지갑 헤더가 있으면 지갑으로만, 없으면 세션으로 확인한다. 어느 쪽도 아니면 null
async function authenticate(request: Request, body: Uint8Array, env: EvalEnv, nowMs: number): Promise<NavAccount | null> {
	if (hasWalletHeaders(request)) {
		const address = await verifyWalletRequest(request, body, nowMs)
		return address ? { kind: 'wallet', key: await accountKey(env.AUTHOR_KEY_SECRET, 'wallet', address) } : null
	}
	const address = await verifiedAddress(request, env)
	return address ? { kind: 'session', key: await accountKey(env.AUTHOR_KEY_SECRET, 'session', address) } : null
}

export async function handleNavTrials(request: Request, env: EvalEnv, nowMs: number = Date.now()): Promise<Response | null> {
	const url = new URL(request.url)
	if (url.pathname !== NAV_TRIALS_PATH) return null
	const origin = request.headers.get('Origin') ?? ''
	const method = request.method
	if (method === 'OPTIONS') return new Response(null, { status: 204, headers: navCors(origin) })
	if (method !== 'GET' && method !== 'POST' && method !== 'DELETE') return reply({ error: 'method not allowed' }, 405, origin)
	// IP 마다의 요청 제한 (평가 API 와 함께 센다). 계정마다의 저장 수 제한은 없고, 콜렉션 크기(앱마다 코스 10개)로만 묶는다
	if (await isRateLimited(request, method === 'GET' ? env.EVAL_READ_LIMITER : env.EVAL_WRITE_LIMITER)) {
		return reply({ error: 'rate limit exceeded' }, 429, origin, { 'Retry-After': String(RETRY_AFTER_SECONDS) })
	}
	if (Number(request.headers.get('Content-Length') ?? 0) > MAX_BODY_BYTES) return reply({ error: 'body too large' }, 413, origin)

	try {
		// 지갑 서명은 본문 바이트 그대로에 대한 것이라 바이트로 읽는다
		const body = new Uint8Array(await request.arrayBuffer())
		if (body.length > MAX_BODY_BYTES) return reply({ error: 'body too large' }, 413, origin)
		const appParam = url.searchParams.get('app')
		const app = appParam === null ? null : appParam.trim().toLowerCase()
		if (app !== null && !HASH_RE.test(app)) return reply({ error: 'app must be 0x + 40 hex' }, 400, origin)

		if (method === 'GET') {
			if (app === null) return reply({ error: 'app is required' }, 400, origin)
			// 인증이 없으면 게스트: 다른 사용자의 공개 코스만. 인증을 보냈는데 틀리면 401
			const signedIn = hasWalletHeaders(request) || readSessionCredentials(request) !== null
			const account = signedIn ? await authenticate(request, body, env, nowMs) : null
			if (signedIn && !account) return reply({ error: 'sign in, or sign the request with a wallet' }, 401, origin)
			const mine = account ? await readMine(env.DB, account, app) : null
			const others = await readOthers(env.DB, app, (mine ?? []).map((m) => m.course))
			return reply(
				{
					app,
					account: account?.kind ?? null,
					mine,
					others,
					policy: { max_steps: TRIAL_POLICY.maxSteps, max_courses: TRIAL_POLICY.maxCourses, publish_min_runs: TRIAL_POLICY.publishMinRuns },
				},
				200,
				origin
			)
		}

		const account = await authenticate(request, body, env, nowMs)
		if (!account) return reply({ error: 'sign in, or sign the request with a wallet' }, 401, origin)

		if (method === 'DELETE') {
			const res = app
				? await env.DB.prepare(`DELETE FROM nav_trials WHERE "account" = ?1 AND "app" = ?2`).bind(account.key, app).run()
				: await env.DB.prepare(`DELETE FROM nav_trials WHERE "account" = ?1`).bind(account.key).run()
			return reply({ account: account.kind, deleted: Number(res.meta?.changes ?? 0) }, 200, origin)
		}

		let parsed: unknown
		try {
			parsed = JSON.parse(new TextDecoder().decode(body))
		} catch {
			return reply({ error: 'invalid JSON body' }, 400, origin)
		}
		const runs = parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? (parsed as { runs?: unknown }).runs : undefined
		if (!Array.isArray(runs) || runs.length === 0 || runs.length > MAX_RUNS) {
			return reply({ error: `runs must hold 1 to ${MAX_RUNS} records` }, 400, origin)
		}
		const results = await storeRuns(env.DB, account, runs, nowMs)
		return reply({ account: account.kind, results }, 200, origin)
	} catch (err) {
		console.log('nav trials error', err)
		return reply({ error: 'internal error' }, 500, origin)
	}
}
