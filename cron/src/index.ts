// 평가 대기열 처리 (1분마다). www가 eval_queue에 넣은 항목을 욕설 검사 후 evaluations와 Vectorize로 옮긴다.
//
// 상태 흐름: pending → processing → (삭제: 저장 완료) | rejected (욕설) | failed (재시도 한도 초과)
// - processing은 lease_until까지만 유효하다. 실행이 중간에 죽으면 만료 뒤 다른 실행이 다시 집는다.
// - 저장 순서는 D1(INSERT OR IGNORE) → Vectorize(upsert) → 대기열 삭제. 중간에 끊겨도 다시 실행하면 같은 결과다.
// - rejected와 failed는 원문과 벡터를 지우고, 원문 해시(text_hash)와 콘텐츠 해시 주소(bcc)도 지운다.
//   해시가 남아 있으면 같은 문장을 가진 쪽이 해시를 대조해 거부된 원문을 확인할 수 있다.
//
// 대기열 다음에는 판정(processVerdicts)을 처리한다. 아래 "판정" 절 참고.
// 마지막으로 기한이 지난 지갑의 접근성 내비게이션 타임트라이얼을 지운다 (navtrials.ts).

import { moderate } from './moderation'
import { toVector } from './vector'
import { PARAMS, RECIPE, baselineOf, evidence, judge, type Baseline, type Baselines, type Level, type Obs, type Tier } from './verdict'
import { expireWalletTrials } from './navtrials'

export interface Env {
	DB: D1Database
	EVAL_VECTORIZE: VectorizeIndex
	DASHSCOPE_API_KEY: string
	DECISION_ENDPOINT: string
}

const BATCH_SIZE = 20 // 한 번의 실행에서 처리할 최대 건수 (임시값)
const LEASE_MS = 5 * 60 * 1000 // processing 상태 만료 (한 번의 실행보다 길게)
const MAX_ATTEMPTS = 10 // 이 횟수를 넘으면 failed (임시값)
const RETAIN_MS = 7 * 24 * 60 * 60 * 1000 // rejected·failed 행을 남겨 두는 기간 (원문과 해시는 이미 지워졌다)
// text_hash는 NOT NULL이라 지울 때 이 값으로 덮어쓴다 (SHA-256 hex와 같은 길이의 의미 없는 값)
export const MASKED_HASH = '0'.repeat(64)

interface QueueRow {
	id: string
	request_id: string
	from: string
	to: string | null
	cc: string
	// 콘텐츠 해시 주소 (0005 마이그레이션 이전에 쌓인 행은 NULL)
	bcc: string | null
	ref: string | null
	root: string
	text: string | null
	// D1은 BLOB을 ArrayBuffer가 아니라 바이트 배열(number[])로 돌려준다. toVector가 두 형태를 모두 받는다
	vector: ArrayBuffer | number[] | null
	attempts: number
	created_at: number
}

// 한 번의 실행: 처리 대상을 골라 하나씩 집는다(claim). 다른 실행이 이미 집었으면 건너뛴다
export async function processQueue(env: Env, now: number = Date.now()): Promise<{ processed: number }> {
	const due = await env.DB.prepare(
		`SELECT "id" FROM eval_queue
		 WHERE "state" = 'pending' OR ("state" = 'processing' AND "lease_until" < ?1)
		 ORDER BY "created_at" LIMIT ?2`
	)
		.bind(now, BATCH_SIZE)
		.all<{ id: string }>()

	let processed = 0
	for (const { id } of due.results) {
		if (!(await claim(env, id, now))) continue
		await processOne(env, id, now)
		processed++
	}
	// rejected·failed 행은 원문이 없지만 기록으로 남아 있다. 보관 기간이 지나면 지운다
	await env.DB.prepare(`DELETE FROM eval_queue WHERE "state" IN ('rejected', 'failed') AND "updated_at" < ?1`).bind(now - RETAIN_MS).run()
	return { processed }
}

// 조건부 UPDATE로 집는다. 변경된 행이 1건이면 이 실행이 집은 것이다
async function claim(env: Env, id: string, now: number): Promise<boolean> {
	const res = await env.DB.prepare(
		`UPDATE eval_queue
		 SET "state" = 'processing', "lease_until" = ?1, "attempts" = "attempts" + 1, "updated_at" = ?2
		 WHERE "id" = ?3 AND ("state" = 'pending' OR ("state" = 'processing' AND "lease_until" < ?2))`
	)
		.bind(now + LEASE_MS, now, id)
		.run()
	return res.meta.changes === 1
}

async function processOne(env: Env, id: string, now: number): Promise<void> {
	const row = await env.DB.prepare(
		`SELECT "id", "request_id", "from", "to", "cc", "bcc", "ref", "root", "text", "vector", "attempts", "created_at"
		 FROM eval_queue WHERE "id" = ?1`
	)
		.bind(id)
		.first<QueueRow>()
	if (!row) return

	try {
		// 답글은 부모가 저장된 뒤에만 저장한다. 부모가 거부되었으면 답글도 거부한다
		// 부모의 대기열 행이 남아 있는 동안에는 부모 저장이 끝난 것이 아니다.
		// 대기열 행은 D1과 Vectorize 저장이 끝난 뒤에 지워진다. 부모가 D1에만 있는 중간 상태에서는
		// 답글을 저장하지 않는다 (부모를 되돌릴 때 답글이 고아가 된다)
		if (row.ref) {
			const parent = await env.DB.prepare(`SELECT "state" FROM eval_queue WHERE "id" = ?1`)
				.bind(row.ref)
				.first<{ state: string }>()
			if (parent) {
				if (parent.state === 'rejected' || parent.state === 'failed') {
					return reject(env, id, 'parent rejected', now)
				}
				// 부모가 아직 처리 중이다. 이번 시도는 세지 않고 다음 실행에 다시 본다
				return release(env, id, now)
			}
			const stored = await env.DB.prepare(`SELECT 1 AS ok FROM evaluations WHERE "id" = ?1`)
				.bind(row.ref)
				.first<{ ok: number }>()
			if (!stored) return reject(env, id, 'parent missing', now)
		}

		if (row.text === null || row.vector === null) {
			throw new Error('queue row has no text or vector')
		}

		const result = await moderate(env, row.text)
		if (result.verdict === 'profane') {
			// 사유는 걸린 질문 이름이다 (profanity, nsfw, illegal). 앱의 status 조회가 이 값을 돌려준다
			return reject(env, id, result.flagged.join(','), now)
		}

		await save(env, row)
	} catch (err) {
		// 원문은 오류 메시지에 넣지 않는다. 오류 종류만 남긴다
		await retryOrFail(env, row, now, err instanceof Error ? err.message : 'unknown error')
	}
}

// 저장: D1 → Vectorize → 대기열 삭제 순서. 같은 id를 여러 번 써도 결과가 같다
async function save(env: Env, row: QueueRow): Promise<void> {
	// D1 행을 쓰기 전에 변환한다. 벡터가 잘못된 행은 D1에 흔적을 남기지 않고 실패한다
	const vector = toVector(row.vector)

	await env.DB.prepare(
		`INSERT OR IGNORE INTO evaluations ("id", "from", "to", "cc", "bcc", "ref", "root", "created_at", "updated_at", "request_id")
		 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)`
	)
		.bind(row.id, row.from, row.to, row.cc, row.bcc ?? null, row.ref, row.root, row.created_at, row.request_id)
		.run()

	// 메타데이터 규칙은 www/src/evaluations.ts의 vectorMetadata와 같아야 한다 (cc·root 항상, to·ref는 값이 있을 때만)
	const metadata: Record<string, string> = { cc: row.cc, root: row.root }
	if (row.to) metadata.to = row.to
	if (row.ref) metadata.ref = row.ref
	await env.EVAL_VECTORIZE.upsert([{ id: row.id, values: vector, metadata }])

	await env.DB.prepare(`DELETE FROM eval_queue WHERE "id" = ?1`).bind(row.id).run()
}

async function reject(env: Env, id: string, reason: string, now: number): Promise<void> {
	await env.DB.prepare(
		`UPDATE eval_queue SET "state" = 'rejected', "text" = NULL, "vector" = NULL, "text_hash" = ?1, "bcc" = NULL,
		 "lease_until" = NULL, "last_error" = ?2, "updated_at" = ?3 WHERE "id" = ?4`
	)
		.bind(MASKED_HASH, reason, now, id)
		.run()
}

async function release(env: Env, id: string, now: number): Promise<void> {
	await env.DB.prepare(
		`UPDATE eval_queue SET "state" = 'pending', "lease_until" = NULL,
		 "attempts" = CASE WHEN "attempts" > 0 THEN "attempts" - 1 ELSE 0 END, "updated_at" = ?1 WHERE "id" = ?2`
	)
		.bind(now, id)
		.run()
}

// 재시도 한도에 닿았을 때, 이미 저장이 끝난 행인지 확인한다. 저장(D1 → Vectorize)은 끝났는데 대기열 삭제만 실패한 경우가 있다.
// 그때 failed로 표시하면 저장된 평가가 있는데 상태는 failed가 되는 불일치가 생긴다.
// - 'stored': D1과 Vectorize 모두 있다 → 대기열 행만 지우면 끝
// - 'partial': D1만 있고 벡터가 없다 → D1 행을 되돌리고 failed로 끝낸다 (검색에 안 나오는 유령 행 방지)
// - 'none': 저장된 것이 없다 → failed로 끝낸다
// 확인 자체가 실패하면(Vectorize 장애 등) 데이터를 잃지 않도록 failed로 바꾸지 않고 다음 실행에 다시 본다.
export type StoredState = 'stored' | 'partial' | 'none'

export async function inspectStored(env: Env, id: string): Promise<StoredState> {
	const inD1 = await env.DB.prepare(`SELECT 1 AS ok FROM evaluations WHERE "id" = ?1`).bind(id).first<{ ok: number }>()
	const inVectorize = (await env.EVAL_VECTORIZE.getByIds([id])).length > 0
	if (inD1 && inVectorize) return 'stored'
	if (inD1) return 'partial'
	// D1에 없는데 벡터만 있는 경우는 받아들일 수 없는 상태다. 정리는 finalizeFailed가 한다
	return inVectorize ? 'partial' : 'none'
}

async function retryOrFail(env: Env, row: QueueRow, now: number, error: string): Promise<void> {
	if (row.attempts >= MAX_ATTEMPTS) {
		let state: StoredState
		try {
			state = await inspectStored(env, row.id)
		} catch (err) {
			// 저장 여부를 확인할 수 없다. 판단을 미루고 대기열에 남긴다 (원문과 벡터도 그대로)
			await env.DB.prepare(
				`UPDATE eval_queue SET "state" = 'pending', "lease_until" = NULL, "last_error" = ?1, "updated_at" = ?2 WHERE "id" = ?3`
			)
				.bind(`inspect failed: ${err instanceof Error ? err.message : 'unknown error'}`, now, row.id)
				.run()
			return
		}
		if (state === 'stored') {
			await env.DB.prepare(`DELETE FROM eval_queue WHERE "id" = ?1`).bind(row.id).run()
			return
		}
		if (state === 'partial') {
			// 부분 저장을 되돌린다. 둘 다 없는 값에 대해 지우는 것은 안전하다. 실패하면 위와 같이 다음 실행에 다시 본다
			try {
				await env.EVAL_VECTORIZE.deleteByIds([row.id])
				await env.DB.prepare(`DELETE FROM evaluations WHERE "id" = ?1`).bind(row.id).run()
			} catch (err) {
				await env.DB.prepare(
					`UPDATE eval_queue SET "state" = 'pending', "lease_until" = NULL, "last_error" = ?1, "updated_at" = ?2 WHERE "id" = ?3`
				)
					.bind(`rollback failed: ${err instanceof Error ? err.message : 'unknown error'}`, now, row.id)
					.run()
				return
			}
		}
		await env.DB.prepare(
			`UPDATE eval_queue SET "state" = 'failed', "text" = NULL, "vector" = NULL, "text_hash" = ?1, "bcc" = NULL,
			 "lease_until" = NULL, "last_error" = ?2, "updated_at" = ?3 WHERE "id" = ?4`
		)
			.bind(MASKED_HASH, error, now, row.id)
			.run()
		return
	}
	await env.DB.prepare(
		`UPDATE eval_queue SET "state" = 'pending', "lease_until" = NULL, "last_error" = ?1, "updated_at" = ?2 WHERE "id" = ?3`
	)
		.bind(error, now, row.id)
		.run()
}

// ---- 판정 (PD-Verdict v1, verdict.ts) ----
// www가 투표를 eval_votes에 추가하고 verdict_dirty에 표시한다. 한 번의 실행에서
// 1. 표시된 평가와, 가장 오래전에 계산한 판정(sweep)의 근거량 E와 값 v를 다시 계산한다
// 2. 그 평가들이 속한 범위(secondary: cc+to, primary: cc, global: 전체)의 기준선을 다시 계산한다
// 3. 그 평가들과, 건드린 secondary·primary 범위의 피어를 다시 판정한다 (기준선이 바뀌면 피어의 순위도 바뀐다)

const VERDICT_BATCH = 50 // 한 번의 실행에서 처리할 투표 표시(verdict_dirty) 최대 건수 (임시값)
// 새 표가 없어도 시간이 지나면 감쇠로 v가 바뀐다. 가장 오래전에 계산한 판정을 실행마다 이만큼 다시 계산한다 (임시값)
const VERDICT_SWEEP = 25
const JUDGE_LIMIT = 500 // 건드린 범위마다 다시 판정할 피어 최대 수. 넘치면 가장 오래전에 판정한 행부터 (임시값)
// 판정 결과는 JSON 배열 하나로 묶어 한 문장에 쓴다 (약 200바이트 × 200행). 행마다 UPDATE를 보내면
// 피어가 많은 실행에서 D1의 실행당 쿼리 수 상한에 닿는다
const JUDGE_CHUNK = 200
const TRACK = 'pd'
const GLOBAL_SCOPE = '*'

interface Scope {
	// verdict_scopes.scope_key
	key: string
	level: Level
	cc: string | null
	to: string | null
}

// 평가 하나가 속한 범위. secondary는 to가 있을 때만 있다
function scopesOf(cc: string, to: string | null): Scope[] {
	const scopes: Scope[] = []
	if (to !== null) scopes.push({ key: `cc:${cc}|to:${to}`, level: 'secondary', cc, to })
	scopes.push({ key: `cc:${cc}`, level: 'primary', cc, to: null })
	scopes.push({ key: GLOBAL_SCOPE, level: 'global', cc: null, to: null })
	return scopes
}

interface VoteRow {
	voter: string
	voter_kind: string
	kind: string
	value: number
	created_at: number
}

interface JudgeRow {
	eval_id: string
	cc: string
	to: string | null
	evidence: number
	value: number
	tier: Tier
	tier_since: number
}

const JUDGE_COLS = `"eval_id", "cc", "to", "evidence", "value", "tier", "tier_since"`

interface Judgement {
	id: string
	tier: Tier
	grade: number | null
	rank: number | null
	level: Level | null
	reasons: string[]
	since: number
}

export async function processVerdicts(env: Env, now: number = Date.now()): Promise<{ computed: number; judged: number }> {
	const dirty = await env.DB.prepare(`SELECT "eval_id", "at" FROM verdict_dirty ORDER BY "at" LIMIT ?1`)
		.bind(VERDICT_BATCH)
		.all<{ eval_id: string; at: number }>()
	const swept = await env.DB.prepare(`SELECT "eval_id" FROM eval_verdicts ORDER BY "updated_at" LIMIT ?1`)
		.bind(VERDICT_SWEEP)
		.all<{ eval_id: string }>()
	// 읽어 둔 표시 시각. 계산 뒤에는 이 값 이하인 표시만 지운다 (그 사이 들어온 표의 표시는 남아 다음 실행이 다시 본다)
	const marks = new Map(dirty.results.map((d) => [d.eval_id, d.at]))
	const work = [...new Set([...marks.keys(), ...swept.results.map((s) => s.eval_id)])]

	const touched = new Map<string, Scope>()
	const computed: string[] = []
	for (const id of work) {
		const ev = await env.DB.prepare(`SELECT "cc", "to", "from" FROM evaluations WHERE "id" = ?1`)
			.bind(id)
			.first<{ cc: string; to: string | null; from: string }>()
		if (!ev) {
			await dropVerdict(env, id, touched)
			continue
		}
		await computeVerdict(env, id, ev, now)
		const at = marks.get(id)
		if (at !== undefined) {
			await env.DB.prepare(`DELETE FROM verdict_dirty WHERE "eval_id" = ?1 AND "at" <= ?2`).bind(id, at).run()
		}
		for (const s of scopesOf(ev.cc, ev.to)) touched.set(s.key, s)
		computed.push(id)
	}
	if (touched.size === 0) return { computed: 0, judged: 0 }

	// 건드린 범위의 기준선은 다시 계산해 쓰고, 나머지 범위(피어의 다른 secondary 등)는 저장된 값을 읽는다
	const baselines = new Map<string, Baseline>()
	for (const s of touched.values()) baselines.set(s.key, await refreshScope(env, s, now))

	// 다시 판정할 행: 계산한 평가 + 건드린 secondary·primary 범위의 피어 (범위마다 JUDGE_LIMIT까지).
	// global 범위만 같은 피어는 여기서 다시 판정하지 않는다 (전체를 매 실행 볼 수는 없다). 그런 피어는 sweep 차례가 오면 다시 판정된다
	const rows = new Map<string, JudgeRow>()
	for (const r of await readJudgeRows(env, computed)) rows.set(r.eval_id, r)
	for (const s of touched.values()) {
		if (s.level === 'global') continue
		for (const r of await scopePeers(env, s)) rows.set(r.eval_id, r)
	}
	const missing = new Set<string>()
	for (const r of rows.values()) {
		for (const s of scopesOf(r.cc, r.to)) if (!baselines.has(s.key)) missing.add(s.key)
	}
	for (const [key, b] of await readScopes(env, [...missing])) baselines.set(key, b)

	const judged: Judgement[] = []
	for (const r of rows.values()) {
		const bs: Baselines = {}
		for (const s of scopesOf(r.cc, r.to)) {
			const b = baselines.get(s.key)
			if (b) bs[s.level] = b
		}
		const v = judge({ evidence: r.evidence, value: r.value, baselines: bs, prev: { tier: r.tier, since_ms: r.tier_since }, nowMs: now, track: TRACK })
		judged.push({ id: r.eval_id, tier: v.tier, grade: v.grade, rank: v.rank, level: v.level, reasons: v.reasons, since: v.since_ms })
	}
	await writeJudgements(env, judged, now)
	return { computed: computed.length, judged: judged.length }
}

// E·v를 다시 계산해 저장한다. (투표자, 종류)마다 가장 최근 표만 센다
async function computeVerdict(env: Env, id: string, ev: { cc: string; to: string | null; from: string }, now: number): Promise<void> {
	const votes = await env.DB.prepare(
		`SELECT "voter", "voter_kind", "kind", "value", "created_at" FROM eval_votes WHERE "eval_id" = ?1 ORDER BY "created_at", "id"`
	)
		.bind(id)
		.all<VoteRow>()
	// 시간순으로 읽으므로 같은 (투표자, 종류)는 뒤의 행이 앞의 행을 덮는다
	const latest = new Map<string, VoteRow>()
	for (const v of votes.results) latest.set(`${v.voter}|${v.kind}`, v)

	const obs: Obs[] = []
	let up = 0
	let down = 0
	for (const v of latest.values()) {
		if (v.value === 0) continue // 철회
		// 확인된 로그인의 표만 센다. 게스트 주소는 IP만 바꾸면 늘릴 수 있고, 작성자는 자기 평가를 올릴 수 없어야 한다
		const weight = v.voter_kind === 'session' && v.voter !== ev.from ? (PARAMS.weights[v.kind] ?? 0) : 0
		obs.push({ kind: v.kind, sign: v.value, weight, credit: 1, at_ms: v.created_at })
		if (weight > 0 && v.value === 1) up++
		if (weight > 0 && v.value === -1) down++
	}
	const { evidence: e, value } = evidence(obs, now, TRACK)

	// 새 행은 unrated로 시작하고 같은 실행의 판정 단계에서 등급을 받는다. 있던 행은 E·v와 표 수만 바꾼다
	await env.DB.prepare(
		`INSERT INTO eval_verdicts ("eval_id", "cc", "to", "evidence", "value", "up", "down", "level", "rank", "tier", "grade", "tier_since", "reasons", "recipe", "updated_at", "judged_at")
		 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 'unrated', NULL, ?8, '[]', ?9, ?8, ?8)
		 ON CONFLICT ("eval_id") DO UPDATE SET "evidence" = excluded."evidence", "value" = excluded."value",
		 "up" = excluded."up", "down" = excluded."down", "updated_at" = excluded."updated_at"`
	)
		.bind(id, ev.cc, ev.to, e, value, up, down, now, RECIPE)
		.run()
}

// 평가가 없다 (재시도 한도에서 부분 저장을 되돌린 경우 등). 표시를 지우고, 남은 판정 행도 지운다.
// 판정 행이 남으면 sweep 맨 앞에 계속 걸려 자리를 차지하고, 범위의 기준선에도 섞인다
async function dropVerdict(env: Env, id: string, touched: Map<string, Scope>): Promise<void> {
	await env.DB.prepare(`DELETE FROM verdict_dirty WHERE "eval_id" = ?1`).bind(id).run()
	const stale = await env.DB.prepare(`SELECT "cc", "to" FROM eval_verdicts WHERE "eval_id" = ?1`)
		.bind(id)
		.first<{ cc: string; to: string | null }>()
	if (!stale) return
	await env.DB.prepare(`DELETE FROM eval_verdicts WHERE "eval_id" = ?1`).bind(id).run()
	// 빠진 행이 기준선에서도 빠지도록 그 범위를 다시 계산한다
	for (const s of scopesOf(stale.cc, stale.to)) touched.set(s.key, s)
}

// 범위의 기준선: 판정 가능한(E ≥ min_evidence) 평가들의 v (verdict.ts baselineOf, 모집단 표준편차)
async function refreshScope(env: Env, s: Scope, now: number): Promise<Baseline> {
	const min = PARAMS.min_evidence
	let rs: { results: { value: number }[] }
	if (s.level === 'secondary') {
		rs = await env.DB.prepare(`SELECT "value" FROM eval_verdicts WHERE "cc" = ?1 AND "to" = ?2 AND "evidence" >= ?3`)
			.bind(s.cc, s.to, min)
			.all<{ value: number }>()
	} else if (s.level === 'primary') {
		rs = await env.DB.prepare(`SELECT "value" FROM eval_verdicts WHERE "cc" = ?1 AND "evidence" >= ?2`).bind(s.cc, min).all<{ value: number }>()
	} else {
		rs = await env.DB.prepare(`SELECT "value" FROM eval_verdicts WHERE "evidence" >= ?1`).bind(min).all<{ value: number }>()
	}
	const b = baselineOf(rs.results.map((r) => r.value))
	await env.DB.prepare(`INSERT OR REPLACE INTO verdict_scopes ("scope_key", "n", "mean", "sd", "updated_at") VALUES (?1, ?2, ?3, ?4, ?5)`)
		.bind(s.key, b.n, b.mean, b.sd, now)
		.run()
	return b
}

// 범위의 피어. 가장 오래전에 판정한 행부터 읽으므로 범위가 JUDGE_LIMIT보다 커도 실행이 거듭되면 모두 차례가 온다
async function scopePeers(env: Env, s: Scope): Promise<JudgeRow[]> {
	const rs =
		s.level === 'secondary'
			? await env.DB.prepare(`SELECT ${JUDGE_COLS} FROM eval_verdicts WHERE "cc" = ?1 AND "to" = ?2 ORDER BY "judged_at" LIMIT ?3`)
				.bind(s.cc, s.to, JUDGE_LIMIT)
				.all<JudgeRow>()
			: await env.DB.prepare(`SELECT ${JUDGE_COLS} FROM eval_verdicts WHERE "cc" = ?1 ORDER BY "judged_at" LIMIT ?2`)
				.bind(s.cc, JUDGE_LIMIT)
				.all<JudgeRow>()
	return rs.results
}

// id 목록(최대 VERDICT_BATCH + VERDICT_SWEEP)은 JSON 배열 하나로 넘긴다. D1은 문장당 바인딩이 100개까지다
async function readJudgeRows(env: Env, ids: string[]): Promise<JudgeRow[]> {
	if (ids.length === 0) return []
	const rs = await env.DB.prepare(`SELECT ${JUDGE_COLS} FROM eval_verdicts WHERE "eval_id" IN (SELECT "value" FROM json_each(?1))`)
		.bind(JSON.stringify(ids))
		.all<JudgeRow>()
	return rs.results
}

async function readScopes(env: Env, keys: string[]): Promise<Map<string, Baseline>> {
	const out = new Map<string, Baseline>()
	for (let i = 0; i < keys.length; i += JUDGE_CHUNK) {
		const rs = await env.DB.prepare(
			`SELECT "scope_key", "n", "mean", "sd" FROM verdict_scopes WHERE "scope_key" IN (SELECT "value" FROM json_each(?1))`
		)
			.bind(JSON.stringify(keys.slice(i, i + JUDGE_CHUNK)))
			.all<Baseline & { scope_key: string }>()
		for (const r of rs.results) out.set(r.scope_key, { n: r.n, mean: r.mean, sd: r.sd })
	}
	return out
}

// 판정 결과를 덩이마다 UPDATE 한 문장으로 쓴다. reasons는 JSON 배열 문자열로 저장된다
async function writeJudgements(env: Env, items: Judgement[], now: number): Promise<void> {
	for (let i = 0; i < items.length; i += JUDGE_CHUNK) {
		await env.DB.prepare(
			`UPDATE eval_verdicts SET
			 "tier" = json_extract(j."value", '$.tier'), "grade" = json_extract(j."value", '$.grade'),
			 "rank" = json_extract(j."value", '$.rank'), "level" = json_extract(j."value", '$.level'),
			 "reasons" = json_extract(j."value", '$.reasons'), "tier_since" = json_extract(j."value", '$.since'),
			 "recipe" = ?2, "judged_at" = ?3
			 FROM json_each(?1) AS j
			 WHERE eval_verdicts."eval_id" = json_extract(j."value", '$.id')`
		)
			.bind(JSON.stringify(items.slice(i, i + JUDGE_CHUNK)), RECIPE, now)
			.run()
	}
}

export default {
	async scheduled(_controller: ScheduledController, env: Env, _ctx: ExecutionContext): Promise<void> {
		const result = await processQueue(env)
		console.log('eval queue processed', result.processed)
		// 대기열 다음에 판정을 처리한다
		const verdicts = await processVerdicts(env)
		console.log('verdicts computed', verdicts.computed, 'judged', verdicts.judged)
		// 접근성 내비게이션: 기한(30일)이 지난 지갑 코스를 지운다. 기록을 더 올리지 않는 지갑도 기한을 넘기지 않게
		const expired = await expireWalletTrials(env.DB, Date.now())
		if (expired > 0) console.log('wallet nav trials expired', expired)
	},
}
