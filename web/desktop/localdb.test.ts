// 실행: node --experimental-strip-types --test web/desktop/localdb.test.ts  (premium-discount 폴더에서)
// Dexie는 설치하지 않고 같은 인터페이스의 메모리 가짜를 쓴다. 실제 IndexedDB 동작은 확인하지 못한다 (README 참고).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

import {
	DB_NAME,
	DB_VERSION,
	EVALUATIONS,
	EVALUATIONS_STORE,
	META,
	STATES,
	byState,
	emptyToNone,
	noneToEmpty,
	normalizeRow,
	openLocalDb,
	syncFromRust,
	thread,
	type DbLike,
	type DexieLike,
} from './localdb.ts'

const fixture: any[] = JSON.parse(readFileSync(new URL('./fixtures/cache_rows.json', import.meta.url), 'utf8'))

// ---- 메모리 가짜 Dexie ----
class FakeTable {
	rows = new Map<unknown, any>()
	primaryKey: string
	constructor(primaryKey: string) {
		this.primaryKey = primaryKey
	}
	async bulkPut(rows: any[]) {
		for (const r of rows) this.rows.set(r[this.primaryKey], structuredClone(r))
	}
	async put(row: any) {
		this.rows.set(row[this.primaryKey], structuredClone(row))
	}
	async get(key: unknown) {
		return this.rows.get(key)
	}
	where(index: string) {
		return {
			equals: (value: unknown) => ({
				toArray: async () => [...this.rows.values()].filter((r) => r[index] === value),
			}),
		}
	}
}

class FakeDexie implements DexieLike {
	tables = new Map<string, FakeTable>()
	stores: Record<string, string> | null = null
	versionNumber = 0
	name: string
	constructor(name: string) {
		this.name = name
	}
	version(n: number) {
		this.versionNumber = n
		return {
			stores: (definition: Record<string, string>) => {
				this.stores = definition
				for (const [table, schema] of Object.entries(definition)) {
					this.tables.set(table, new FakeTable(schema.split(',')[0].trim()))
				}
			},
		}
	}
	table(name: string) {
		const t = this.tables.get(name)
		if (!t) throw new Error(`no table ${name}`)
		return t
	}
}

function newDb(): FakeDexie {
	return openLocalDb(FakeDexie as unknown as new (n: string) => DexieLike) as FakeDexie
}

// ---- 변환 규칙 ----
test('없음과 빈 문자열 변환은 Rust와 같은 규칙이다', () => {
	assert.equal(noneToEmpty(null), '')
	assert.equal(noneToEmpty(undefined), '')
	assert.equal(noneToEmpty('0xab'), '0xab')
	assert.equal(emptyToNone(''), undefined)
	assert.equal(emptyToNone('   '), undefined)
	assert.equal(emptyToNone(null), undefined)
	assert.equal(emptyToNone(' 0xab '), '0xab')
})

// ---- 계약 픽스처 (Rust `to_cache_row`와 같은 파일) ----
test('Rust가 내보내는 행(픽스처)은 정규화해도 그대로다', () => {
	assert.equal(fixture.length, 2)
	for (const row of fixture) assert.deepEqual(normalizeRow(row), row)
})

test('픽스처의 "없음"은 모두 빈 문자열이다 (null이 없다)', () => {
	const bare = fixture[1]
	for (const k of ['id', 'from', 'to', 'bcc', 'ref', 'root']) assert.equal(bare[k], '', k)
	assert.equal(bare.data.reason, '')
	assert.ok(!JSON.stringify(fixture).includes('null'))
	assert.ok(!('vector' in bare) && !('vector' in bare.data), '벡터는 캐시로 보내지 않는다')
})

// ---- 정규화 ----
test('null과 빠진 값은 빈 문자열로, 시간은 정수로 검사한다', () => {
	const row = normalizeRow({
		request_id: 'req-x',
		id: null,
		to: undefined,
		cc: 'example.com',
		state: 'queued',
		created_at: 5,
		updated_at: 6,
	})
	assert.equal(row.id, '')
	assert.equal(row.to, '')
	assert.equal(row.ref, '')
	assert.deepEqual(row.data, { local_id: '', parent_local: '', text: '', text_hash: '', attempts: 0, reason: '' })
})

test('잘못된 행은 거부한다', () => {
	const ok = { request_id: 'r', state: 'queued', created_at: 1, updated_at: 1 }
	assert.doesNotThrow(() => normalizeRow(ok))
	assert.throws(() => normalizeRow(null), /object/)
	assert.throws(() => normalizeRow([]), /object/)
	assert.throws(() => normalizeRow({ ...ok, request_id: '' }), /request_id/)
	assert.throws(() => normalizeRow({ ...ok, request_id: null }), /request_id/)
	assert.throws(() => normalizeRow({ ...ok, state: 'weird' }), /unknown state/)
	assert.throws(() => normalizeRow({ ...ok, state: undefined }), /unknown state/)
	for (const bad of [-1, 1.5, NaN, '5', null, undefined]) {
		assert.throws(() => normalizeRow({ ...ok, created_at: bad }), /created_at/, String(bad))
		assert.throws(() => normalizeRow({ ...ok, updated_at: bad }), /updated_at/, String(bad))
	}
	assert.throws(() => normalizeRow({ ...ok, data: { attempts: -1 } }), /attempts/)
})

test('상태 목록은 Rust와 같다', () => {
	assert.deepEqual([...STATES], ['queued', 'pending', 'stored', 'rejected', 'failed', 'missing'])
})

// ---- 스키마 ----
test('Dexie 스키마: 기본 키는 request_id, 벡터는 없고, 복합 인덱스는 존재하는 컬럼만 쓴다', () => {
	const tokens = EVALUATIONS_STORE.split(',').map((t) => t.trim())
	assert.equal(tokens[0], 'request_id', '첫 항목이 기본 키')
	const plain = tokens.filter((t) => !t.startsWith('['))
	for (const need of ['id', 'root', 'ref', 'cc', 'from', 'to', 'state', 'created_at', 'updated_at']) {
		assert.ok(plain.includes(need), `index ${need}`)
	}
	assert.ok(!EVALUATIONS_STORE.includes('vector') && !EVALUATIONS_STORE.includes('bcc'))
	const columns = new Set(Object.keys(fixture[0]).filter((k) => k !== 'data'))
	for (const t of tokens.filter((x) => x.startsWith('['))) {
		for (const part of t.slice(1, -1).split('+')) assert.ok(columns.has(part), `${t} -> ${part}`)
	}
	assert.equal(new Set(tokens).size, tokens.length, '중복 인덱스 없음')
})

test('openLocalDb가 이름, 버전, 두 테이블을 정의한다', () => {
	const db = newDb()
	assert.equal(db.name, DB_NAME)
	assert.equal(db.versionNumber, DB_VERSION)
	assert.deepEqual(db.stores, { [EVALUATIONS]: EVALUATIONS_STORE, [META]: 'key' })
	assert.equal(db.table(EVALUATIONS).primaryKey, 'request_id')
})

// ---- 동기화 ----
test('첫 동기화는 처음부터 받아 쓰고 커서를 가장 큰 updated_at으로 옮긴다', async () => {
	const db = newDb()
	const seen: number[] = []
	const r = await syncFromRust(db, async (since) => {
		seen.push(since)
		return fixture
	})
	assert.deepEqual(seen, [0])
	assert.deepEqual(r, { applied: 2, cursor: 1700000001000 })
	assert.equal(db.table(EVALUATIONS).rows.size, 2)
	assert.equal((await db.table(META).get('cursor')).value, 1700000001000)
})

test('다음 동기화는 한 칸 겹쳐서 요청하고, 같은 행을 다시 받아도 중복되지 않는다', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	const seen: number[] = []
	const r = await syncFromRust(db, async (since) => {
		seen.push(since)
		return [fixture[1]] // 같은 밀리초에 다시 온 행
	})
	assert.deepEqual(seen, [1700000001000 - 1])
	assert.equal(r.applied, 1)
	assert.equal(db.table(EVALUATIONS).rows.size, 2, '기본 키가 같으면 덮어쓴다')
})

test('바뀐 행이 없으면 커서는 그대로다', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	const r = await syncFromRust(db, async () => [])
	assert.deepEqual(r, { applied: 0, cursor: 1700000001000 })
})

test('상태가 바뀐 행은 새 값으로 덮어쓴다', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	const updated = { ...fixture[1], state: 'stored', id: '0190f0a0-0000-7000-8000-000000000003', updated_at: 1700000009000 }
	await syncFromRust(db, async () => [updated])
	const got = await db.table(EVALUATIONS).get('req-contract-0000000002')
	assert.equal(got.state, 'stored')
	assert.equal(got.id, '0190f0a0-0000-7000-8000-000000000003')
	assert.equal((await db.table(META).get('cursor')).value, 1700000009000)
})

test('한 행이라도 틀리면 아무것도 쓰지 않고 커서도 그대로다', async () => {
	const db = newDb()
	await syncFromRust(db, async () => [fixture[0]])
	const before = (await db.table(META).get('cursor')).value
	await assert.rejects(
		syncFromRust(db, async () => [fixture[1], { ...fixture[1], request_id: 'req-bad', state: 'weird' }]),
		/unknown state/,
	)
	assert.equal(db.table(EVALUATIONS).rows.size, 1, '앞의 정상 행도 쓰지 않았다')
	assert.equal((await db.table(META).get('cursor')).value, before)
})

test('Rust 호출이 실패하면 오류를 그대로 올리고 캐시는 변하지 않는다', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	await assert.rejects(
		syncFromRust(db, async () => {
			throw new Error('ipc down')
		}),
		/ipc down/,
	)
	assert.equal(db.table(EVALUATIONS).rows.size, 2)
})

test('커서를 지우면 처음부터 다시 채운다 (캐시는 Rust에서 다시 만들 수 있다)', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	db.tables.set(EVALUATIONS, new FakeTable('request_id')) // 캐시가 지워진 상황
	db.tables.set(META, new FakeTable('key'))
	const seen: number[] = []
	const r = await syncFromRust(db, async (since) => {
		seen.push(since)
		return fixture
	})
	assert.deepEqual(seen, [0])
	assert.equal(r.applied, 2)
	assert.equal(db.table(EVALUATIONS).rows.size, 2)
})

// ---- 조회 ----
test('빈 문자열 컬럼도 조회할 수 있다 (id가 아직 없는 행)', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	const noServerId = await (db as DbLike).table(EVALUATIONS).where('id').equals('').toArray()
	assert.equal(noServerId.length, 1)
	assert.equal(noServerId[0].request_id, 'req-contract-0000000002')
})

test('byState와 thread', async () => {
	const db = newDb()
	await syncFromRust(db, async () => fixture)
	assert.deepEqual((await byState(db, 'queued')).map((r) => r.request_id), ['req-contract-0000000002'])
	assert.deepEqual((await byState(db, 'stored')).map((r) => r.request_id), ['req-contract-0000000001'])
	assert.equal((await thread(db, '0190f0a0-0000-7000-8000-000000000001')).length, 1)
})
