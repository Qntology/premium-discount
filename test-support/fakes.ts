// cron 테스트용 가짜 D1과 Vectorize.
// D1은 node:sqlite(실제 SQLite)로 만든다. 그래서 cron/src/index.ts의 SQL이 실제로 실행되고,
// www/migrations의 0002~0009를 그대로 적용해 컬럼과 제약도 운영과 같다.
import { DatabaseSync } from 'node:sqlite'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const MIGRATIONS = ['0002_create_evaluations.sql', '0003_add_request_id.sql', '0004_eval_queue.sql', '0005_eval_bcc.sql', '0006_votes_verdicts.sql', '0007_nav_trials.sql', '0008_web_analytics.sql', '0009_eval_supersedes.sql']

function toSqliteValue(v: unknown): unknown {
	// D1처럼 ArrayBuffer를 BLOB으로 받는다
	if (v instanceof ArrayBuffer) return new Uint8Array(v)
	return v === undefined ? null : v
}

// D1은 bind(a, b)를 ?1, ?2에 묶는다. Node 22.13의 node:sqlite는 이름 있는 매개변수(?NNN 포함)를 위치 인자로 묶지 않아
// "column index out of range"가 나므로, ?NNN이 있는 문장은 { '?1': a, '?2': b }로 넘긴다 (새 Node에서도 같은 결과)
function sqliteArgs(sql: string, args: unknown[]): unknown[] {
	const used = new Set([...sql.matchAll(/\?(\d+)/g)].map((m) => Number(m[1])))
	if (used.size === 0 || args.length === 0) return args
	// 위치 바인딩처럼, 문장의 가장 큰 번호보다 많이 넘기면 오류다
	if (args.length > Math.max(...used)) throw new Error('column index out of range')
	const named: Record<string, unknown> = {}
	args.forEach((v, i) => {
		if (used.has(i + 1)) named['?' + (i + 1)] = v
	})
	return [named]
}

// D1은 BLOB을 ArrayBuffer가 아니라 바이트 숫자 배열로 돌려준다 (cron/src/vector.ts 주석 참고)
function fromSqliteRow(row: Record<string, unknown>): Record<string, unknown> {
	const out: Record<string, unknown> = {}
	for (const [k, v] of Object.entries(row)) out[k] = v instanceof Uint8Array ? Array.from(v) : v
	return out
}

class FakeStatement {
	sql: string
	db: DatabaseSync
	args: unknown[] = []
	constructor(db: DatabaseSync, sql: string) {
		this.db = db
		this.sql = sql
	}
	bind(...args: unknown[]) {
		this.args = args.map(toSqliteValue)
		return this
	}
	async run() {
		const info = this.db.prepare(this.sql).run(...(sqliteArgs(this.sql, this.args) as never[]))
		return { success: true, meta: { changes: Number(info.changes) } }
	}
	async first<T>(): Promise<T | null> {
		const row = this.db.prepare(this.sql).get(...(sqliteArgs(this.sql, this.args) as never[]))
		return row ? (fromSqliteRow(row as Record<string, unknown>) as T) : null
	}
	async all<T>(): Promise<{ results: T[] }> {
		const rows = this.db.prepare(this.sql).all(...(sqliteArgs(this.sql, this.args) as never[]))
		return { results: rows.map((r) => fromSqliteRow(r as Record<string, unknown>) as T) }
	}
}

export class FakeD1 {
	sqlite: DatabaseSync
	constructor() {
		this.sqlite = new DatabaseSync(':memory:')
		for (const name of MIGRATIONS) {
			const path = fileURLToPath(new URL(`../www/migrations/${name}`, import.meta.url))
			this.sqlite.exec(readFileSync(path, 'utf8'))
		}
	}
	prepare(sql: string) {
		return new FakeStatement(this.sqlite, sql)
	}
	// D1 batch: 문장들을 한 트랜잭션으로 차례로 실행한다. 하나라도 실패하면 모두 되돌린다
	async batch(statements: FakeStatement[]) {
		this.sqlite.exec('BEGIN')
		try {
			const out = []
			for (const s of statements) out.push(await s.run())
			this.sqlite.exec('COMMIT')
			return out
		} catch (err) {
			this.sqlite.exec('ROLLBACK')
			throw err
		}
	}
	// 테스트 확인용: 값 그대로 조회한다
	rows(sql: string, ...args: unknown[]): Record<string, unknown>[] {
		return this.sqlite.prepare(sql).all(...(sqliteArgs(sql, args.map(toSqliteValue)) as never[])).map((r) => fromSqliteRow(r as Record<string, unknown>))
	}
}

export class FakeVectorize {
	store = new Map<string, { id: string; values: number[]; metadata?: Record<string, unknown> }>()
	// 다음 upsert를 실패시킨다 (Vectorize 장애 흉내)
	failNextUpsert = 0
	async upsert(vectors: { id: string; values: Float32Array | number[]; metadata?: Record<string, unknown> }[]) {
		if (this.failNextUpsert > 0) {
			this.failNextUpsert--
			throw new Error('vectorize unavailable')
		}
		for (const v of vectors) this.store.set(v.id, { id: v.id, values: Array.from(v.values), metadata: v.metadata })
		return { mutationId: 'm' }
	}
	async getByIds(ids: string[]) {
		return ids.filter((id) => this.store.has(id)).map((id) => this.store.get(id)!)
	}
	async deleteByIds(ids: string[]) {
		for (const id of ids) this.store.delete(id)
		return { mutationId: 'd' }
	}
	// query가 돌려줄 결과 (점수 순으로 넣어 둔다). 기본은 빈 배열이라 기존 테스트는 그대로다. 필터는 흉내 내지 않는다
	matches: { id: string; score: number; metadata?: Record<string, unknown> }[] = []
	// 받은 query 옵션 (topK 확인용)
	queries: { topK?: number; [key: string]: unknown }[] = []
	async query(_vector: number[], opts?: { topK?: number; [key: string]: unknown }) {
		this.queries.push(opts ?? {})
		// Vectorize의 topK 기본값은 5
		const matches = this.matches.slice(0, opts?.topK ?? 5)
		return { matches, count: matches.length }
	}
}

// Workers Rate Limiting 바인딩 흉내. allow가 false면 모든 호출이 제한에 걸린다
export class FakeLimiter {
	keys: string[] = []
	allow = true
	fail = false
	async limit(opts: { key: string }) {
		this.keys.push(opts.key)
		if (this.fail) throw new Error('limiter unavailable')
		return { success: this.allow }
	}
}

export function unitVector(seed = 1): Float32Array {
	const v = new Float32Array(768)
	for (let i = 0; i < v.length; i++) v[i] = Math.sin(seed * (i + 1))
	const norm = Math.sqrt(v.reduce((s, x) => s + x * x, 0))
	return v.map((x) => x / norm)
}

export const PARENT_ID = '0190f0a0-0000-7000-8000-000000000001'
export const REPLY_ID = '0190f0a0-0000-7000-8000-000000000002'

export interface QueueSeed {
	id: string
	request_id?: string
	text: string
	ref?: string | null
	root?: string
	created_at?: number
	state?: string
	attempts?: number
	lease_until?: number | null
	bcc?: string | null
}

// eval_queue에 행 한 건을 넣는다 (www의 enqueueEvaluation이 하는 일)
export async function seedQueue(db: FakeD1, s: QueueSeed) {
	const created = s.created_at ?? 1_000
	await db.prepare(
		`INSERT INTO eval_queue ("id", "request_id", "from", "to", "cc", "ref", "root", "text", "text_hash", "vector", "state", "attempts", "lease_until", "created_at", "updated_at", "bcc")
		 VALUES (?1, ?2, ?3, NULL, 'example.com', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13)`
	)
		.bind(
			s.id,
			s.request_id ?? 'req-' + s.id,
			'0x00000000000000000000000000000000000000ab',
			s.ref ?? null,
			s.root ?? s.ref ?? s.id,
			s.text,
			'a'.repeat(64),
			unitVector().buffer,
			s.state ?? 'pending',
			s.attempts ?? 0,
			s.lease_until ?? null,
			created,
			s.bcc ?? '0x' + 'b'.repeat(40)
		)
		.run()
}

// seedQueue·seedEvaluation의 기본 작성자 주소
export const AUTHOR = '0x00000000000000000000000000000000000000ab'

export interface EvaluationSeed {
	id: string
	from?: string
	to?: string | null
	cc?: string
	ref?: string | null
	root?: string
	created_at?: number
}

// evaluations에 저장이 끝난 평가 한 건을 넣는다 (cron의 save가 하는 일)
export async function seedEvaluation(db: FakeD1, s: EvaluationSeed) {
	const created = s.created_at ?? 1_000
	await db.prepare(
		`INSERT INTO evaluations ("id", "from", "to", "cc", "bcc", "ref", "root", "created_at", "updated_at", "request_id")
		 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?7, ?8)`
	)
		.bind(s.id, s.from ?? AUTHOR, s.to ?? null, s.cc ?? 'example.com', s.ref ?? null, s.root ?? s.ref ?? s.id, created, 'req-' + s.id)
		.run()
}

export interface VoteSeed {
	eval_id: string
	voter: string
	voter_kind?: 'session' | 'guest'
	kind?: 'vote' | 'vote_outcome'
	value: number
	created_at: number
	// 같은 created_at이면 id 순서가 최신을 정한다. 기본값은 넣은 순서대로 커진다
	id?: string
	// false면 verdict_dirty에 표시하지 않는다
	dirty?: boolean
}

let voteSeq = 0

// eval_votes에 표 한 건을 넣고 verdict_dirty에 표시한다 (www의 투표가 하는 일)
export async function seedVote(db: FakeD1, v: VoteSeed) {
	const n = ++voteSeq
	await db.prepare(
		`INSERT INTO eval_votes ("id", "request_id", "eval_id", "voter", "voter_kind", "kind", "value", "created_at")
		 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`
	)
		.bind(v.id ?? `vote-${String(n).padStart(8, '0')}`, `req-vote-${String(n).padStart(8, '0')}`, v.eval_id, v.voter, v.voter_kind ?? 'session', v.kind ?? 'vote', v.value, v.created_at)
		.run()
	if (v.dirty !== false) {
		await db.prepare(`INSERT OR REPLACE INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)`).bind(v.eval_id, v.created_at).run()
	}
}

export interface VerdictSeed {
	eval_id: string
	cc?: string
	to?: string | null
	tier: string
	grade?: number | null
	rank?: number | null
	evidence?: number
	value?: number
	up?: number
	down?: number
	level?: string | null
	reasons?: string[]
	recipe?: string
	updated_at?: number
}

// eval_verdicts에 판정 한 건을 넣는다 (cron의 processVerdicts가 하는 일)
export async function seedVerdict(db: FakeD1, v: VerdictSeed) {
	const at = v.updated_at ?? 1_000
	await db.prepare(
		`INSERT INTO eval_verdicts ("eval_id", "cc", "to", "evidence", "value", "up", "down", "level", "rank", "tier", "grade", "tier_since", "reasons", "recipe", "updated_at", "judged_at")
		 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?12, ?12)`
	)
		.bind(
			v.eval_id,
			v.cc ?? 'example.com',
			v.to ?? null,
			v.evidence ?? 0,
			v.value ?? 0,
			v.up ?? 0,
			v.down ?? 0,
			v.level ?? null,
			v.rank ?? null,
			v.tier,
			v.grade ?? null,
			at,
			JSON.stringify(v.reasons ?? []),
			v.recipe ?? 'pd-verdict/1'
		)
		.run()
}
