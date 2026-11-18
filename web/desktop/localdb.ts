// 화면(Tauri 웹뷰)의 로컬 캐시: Dexie(IndexedDB).
//
// 구조 결정 (README 13절): Rust(LanceDB)가 원본이고 Dexie는 캐시다. logis-center와 같은 구조다.
// - 이 캐시는 Rust에서만 채운다. 화면이 직접 평가를 만들거나 고치지 않는다 (쓰기는 항상 Rust 먼저).
// - 지워져도 Rust에서 다시 채울 수 있다 (`syncFromRust`의 커서를 0으로 되돌리면 된다).
// - 벡터는 넣지 않는다. 내 글 검색은 Rust에서 하므로 768개 숫자를 IPC로 넘길 이유가 없다.
//
// 컬럼 규칙 (README 13-2, 13-3)
// - "없음"은 빈 문자열이다. IndexedDB는 null을 키로 쓸 수 없어 null인 행이 인덱스에서 빠지기 때문이다.
//   서버(D1, Vectorize)의 NULL과 Rust(LanceDB)의 NULL은 Rust가 내보낼 때 `""`로 바꿔 준다 (`none_to_empty`).
// - 시간은 밀리초(UTC epoch) 정수다.
// - logis-center의 봉투 규칙(id, from, to, cc, bcc, ref ... 이름과 `data.*`에 나머지)을 따르되 두 가지가 다르다.
//   1) 기본 키는 `request_id`다. `id`는 서버가 접수할 때 정하므로 처음에는 `""`이고, 기본 키로 쓸 수 없다.
//   2) `root`와 `state`는 인덱스로 쓰므로 `data` 안이 아니라 최상위에 둔다.
//
// 이 파일은 Dexie를 직접 import하지 않는다. 화면이 가진 Dexie 생성자를 `openLocalDb`에 넘긴다
// (logis-center가 `window.Dexie`를 쓰는 것과 같다). 그래서 Node에서도 테스트할 수 있다.
//
// 행 모양은 Rust `LocalRecord::to_cache_row`와 같아야 한다. `fixtures/cache_rows.json`이 두 쪽의 계약이고,
// 어느 한쪽을 바꾸면 이 픽스처와 양쪽 테스트가 깨진다.

export const DB_NAME = 'PremiumDiscountLocal'
export const DB_VERSION = 1
export const EVALUATIONS = 'evaluations'
export const META = 'meta'

export const STATES = ['queued', 'pending', 'stored', 'rejected', 'failed', 'missing'] as const
export type LocalState = (typeof STATES)[number]

export interface LocalRow {
	request_id: string
	id: string
	from: string
	to: string
	cc: string
	bcc: string
	ref: string
	root: string
	state: LocalState
	created_at: number
	updated_at: number
	data: {
		local_id: string
		parent_local: string
		text: string
		text_hash: string
		attempts: number
		reason: string
	}
}

// 첫 항목이 기본 키, 나머지는 인덱스. `[a+b]`는 복합 인덱스.
// - [cc+state]: 도메인별 상태 목록
// - [root+created_at]: 스레드를 시간순으로
// - [state+created_at]: 보내기 대기 목록 등 상태별 시간순
export const EVALUATIONS_STORE =
	'request_id, id, root, ref, cc, from, to, state, created_at, updated_at, [cc+state], [root+created_at], [state+created_at]'

// Dexie 중 우리가 쓰는 부분만 적은 최소 인터페이스 (실제 Dexie와 테스트용 가짜가 모두 맞는다)
export interface TableLike {
	bulkPut(rows: unknown[]): Promise<unknown>
	put(row: unknown): Promise<unknown>
	get(key: unknown): Promise<any>
	where(index: string): { equals(value: unknown): { toArray(): Promise<any[]> } }
}
export interface DbLike {
	table(name: string): TableLike
}
export interface DexieLike extends DbLike {
	version(n: number): { stores(definition: Record<string, string>): unknown }
}

/** 화면이 가진 Dexie 생성자로 캐시 DB를 연다. 예: `openLocalDb((window as any).Dexie)` */
export function openLocalDb(DexieCtor: new (name: string) => DexieLike): DexieLike {
	const db = new DexieCtor(DB_NAME)
	db.version(DB_VERSION).stores({
		[EVALUATIONS]: EVALUATIONS_STORE,
		[META]: 'key',
	})
	return db
}

/** "없음"을 빈 문자열로 (Rust `none_to_empty`와 같은 규칙) */
export function noneToEmpty(v: string | null | undefined): string {
	return v === null || v === undefined ? '' : String(v)
}

/** 빈 문자열을 "없음"으로. 서버로 보낼 때 필드를 생략하는 데 쓴다 (Rust `empty_to_none`과 같은 규칙) */
export function emptyToNone(v: string | null | undefined): string | undefined {
	const t = noneToEmpty(v).trim()
	return t === '' ? undefined : t
}

function toInt(value: unknown, field: string): number {
	if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isInteger(value) || value < 0) {
		throw new Error(`cache row field ${field} must be a non-negative integer`)
	}
	return value
}

/** Rust가 보낸 행을 검사하고 캐시 행 모양으로 맞춘다. 빠진 값과 null은 빈 문자열이 된다. 상태와 시간이 틀리면 오류다. */
export function normalizeRow(raw: unknown): LocalRow {
	if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
		throw new Error('cache row must be an object')
	}
	const r = raw as Record<string, any>
	const request_id = noneToEmpty(r.request_id)
	if (request_id === '') throw new Error('cache row has no request_id')
	const state = noneToEmpty(r.state)
	if (!(STATES as readonly string[]).includes(state)) throw new Error(`unknown state: ${state}`)
	const d = r.data && typeof r.data === 'object' && !Array.isArray(r.data) ? r.data : {}
	return {
		request_id,
		id: noneToEmpty(r.id),
		from: noneToEmpty(r.from),
		to: noneToEmpty(r.to),
		cc: noneToEmpty(r.cc),
		bcc: noneToEmpty(r.bcc),
		ref: noneToEmpty(r.ref),
		root: noneToEmpty(r.root),
		state: state as LocalState,
		created_at: toInt(r.created_at, 'created_at'),
		updated_at: toInt(r.updated_at, 'updated_at'),
		data: {
			local_id: noneToEmpty(d.local_id),
			parent_local: noneToEmpty(d.parent_local),
			text: noneToEmpty(d.text),
			text_hash: noneToEmpty(d.text_hash),
			attempts: d.attempts === undefined || d.attempts === null ? 0 : toInt(d.attempts, 'data.attempts'),
			reason: noneToEmpty(d.reason),
		},
	}
}

export interface SyncResult {
	applied: number
	cursor: number
}

/**
 * Rust에서 바뀐 행을 받아 캐시에 반영한다.
 * `fetchRows(since)`는 화면 셸이 Rust의 `Outbox::cache_rows_since(since)`를 부르는 함수다 (Tauri 명령 이름은 셸이 정한다).
 * - 커서는 마지막으로 반영한 `updated_at`이다. Rust는 `updated_at > since`인 행을 주므로, 같은 밀리초에 나중에 바뀐 행을
 *   놓치지 않도록 한 칸 겹쳐서(`cursor - 1`) 요청한다. `bulkPut`은 같은 행을 다시 써도 결과가 같다.
 * - 받은 행을 모두 검사한 뒤에 쓴다. 한 행이라도 틀리면 아무것도 쓰지 않고 오류를 던진다 (커서도 그대로다).
 * - 행을 쓴 다음에 커서를 쓴다. 그 사이에 멈추면 다음에 같은 행을 다시 받을 뿐이다.
 */
export async function syncFromRust(
	db: DbLike,
	fetchRows: (since: number) => Promise<unknown[]>,
): Promise<SyncResult> {
	const meta = db.table(META)
	const saved = await meta.get('cursor')
	const cursor = saved && Number.isFinite(saved.value) ? Number(saved.value) : 0

	const raw = await fetchRows(Math.max(cursor - 1, 0))
	const rows = raw.map(normalizeRow)
	if (rows.length === 0) return { applied: 0, cursor }

	await db.table(EVALUATIONS).bulkPut(rows)
	const next = rows.reduce((max, r) => Math.max(max, r.updated_at), cursor)
	await meta.put({ key: 'cursor', value: next })
	return { applied: rows.length, cursor: next }
}

/** 상태별 목록 (예: 보내기를 기다리는 글) */
export function byState(db: DbLike, state: LocalState): Promise<LocalRow[]> {
	return db.table(EVALUATIONS).where('state').equals(state).toArray()
}

/** 한 스레드 전체 (같은 root) */
export async function thread(db: DbLike, root: string): Promise<LocalRow[]> {
	const rows: LocalRow[] = await db.table(EVALUATIONS).where('root').equals(root).toArray()
	return rows.sort((a, b) => a.created_at - b.created_at)
}
