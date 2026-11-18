// 실행: node --experimental-strip-types --test web/desktop/commands.test.ts  (premium-discount 폴더에서)
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

import { CommandFailure, makeCommands, syncCacheFromRust, toCommandFailure, type Invoke } from './commands.ts'
import { EVALUATIONS, META, type DbLike } from './localdb.ts'

const contract: Record<string, { args: unknown }> = JSON.parse(readFileSync(new URL('./fixtures/command_contract.json', import.meta.url), 'utf8'))
const cacheRows: any[] = JSON.parse(readFileSync(new URL('./fixtures/cache_rows.json', import.meta.url), 'utf8'))

function recorder(reply: unknown = null) {
	const calls: { cmd: string; args: Record<string, unknown> | undefined }[] = []
	const invoke: Invoke = async (cmd, args) => {
		calls.push({ cmd, args })
		return reply
	}
	return { calls, invoke }
}

test('명령 이름과 인자 모양이 Rust와 맺은 계약(command_contract.json)과 같다', async () => {
	const { calls, invoke } = recorder([])
	const c = makeCommands(invoke)
	const sample = (cmd: string) => contract[cmd].args as any
	await c.submitEvaluation(sample('cmd_submit_evaluation'))
	await c.syncCache(sample('cmd_sync_cache').since)
	await c.searchLocal(sample('cmd_search_local'))
	await c.searchRemote(sample('cmd_search_remote'))
	await c.searchMerged(sample('cmd_search_merged'))
	await c.flushOutbox()
	await c.vote(sample('cmd_vote'))
	await c.supersede(sample('cmd_supersede'))
	await c.verdicts(sample('cmd_verdicts').ids, sample('cmd_verdicts').refresh)
	await c.session(sample('cmd_session').verify)
	await c.loginStart()
	await c.loginStatus()
	await c.logout()
	await c.appStatus()
	await c.modelStatus()
	await c.modelDownload(sample('cmd_model_download').variant)
	await c.modelDownloadCancel()
	await c.itemDraft(sample('cmd_item_draft'))
	await c.analyticSite(sample('cmd_analytic_site'))
	await c.analyticSync(sample('cmd_analytic_sync'))
	await c.analyticReport(sample('cmd_analytic_report'))
	await c.analyticForget(sample('cmd_analytic_forget'))

	const names = Object.keys(contract).filter((k) => !k.startsWith('_'))
	assert.deepEqual(calls.map((x) => x.cmd), names)
	for (const call of calls) {
		const expected = contract[call.cmd].args
		// 인자가 없는 명령은 args 자체를 보내지 않고, 나머지는 { args } 하나로만 보낸다
		assert.deepEqual(call.args, expected === null ? undefined : { args: expected }, call.cmd)
	}
})

test('결과는 invoke가 돌려준 값 그대로다', async () => {
	const { invoke } = recorder({ status: 'accepted', id: 'a', root: 'a', ref: null, from: '0x1' })
	const res = await makeCommands(invoke).submitEvaluation({ text: 't', cc: 'example.com' })
	assert.equal(res.status, 'accepted')
})

test('Rust 오류({kind, message}), 문자열, Error를 모두 CommandFailure로 맞춘다', async () => {
	const rustError: Invoke = async () => {
		throw { kind: 'offline', message: 'offline: no network' }
	}
	await assert.rejects(makeCommands(rustError).flushOutbox(), (e: unknown) => e instanceof CommandFailure && e.kind === 'offline' && /no network/.test(e.message))

	assert.equal(toCommandFailure('boom').kind, 'unknown')
	assert.equal(toCommandFailure('boom').message, 'boom')
	assert.equal(toCommandFailure(new Error('x')).message, 'x')
	const already = new CommandFailure('storage', 's')
	assert.equal(toCommandFailure(already), already)
})

test('syncCacheFromRust: cmd_sync_cache로 받은 행을 캐시에 반영하고 커서를 옮긴다', async () => {
	const tables = new Map<string, Map<unknown, any>>()
	const key: Record<string, string> = { [EVALUATIONS]: 'request_id', [META]: 'key' }
	const table = (name: string) => {
		if (!tables.has(name)) tables.set(name, new Map())
		const rows = tables.get(name)!
		return {
			bulkPut: async (rs: any[]) => rs.forEach((r) => rows.set(r[key[name]], r)),
			put: async (r: any) => void rows.set(r[key[name]], r),
			get: async (k: unknown) => rows.get(k),
			where: () => ({ equals: () => ({ toArray: async () => [...rows.values()] }) }),
		}
	}
	const db: DbLike = { table }

	const seen: (number | undefined)[] = []
	const invoke: Invoke = async (cmd, args) => {
		assert.equal(cmd, 'cmd_sync_cache')
		const since = (args as any).args.since as number
		seen.push(since)
		return cacheRows.filter((r) => r.updated_at > since)
	}
	const first = await syncCacheFromRust(db, invoke)
	assert.equal(first.applied, cacheRows.length)
	assert.equal(tables.get(EVALUATIONS)!.size, cacheRows.length)
	assert.equal(seen[0], 0, '처음에는 since=0')

	const second = await syncCacheFromRust(db, invoke)
	assert.equal(seen[1], first.cursor - 1, '한 칸 겹쳐서 요청한다 (같은 밀리초에 바뀐 행을 놓치지 않으려고)')
	assert.ok(second.cursor >= first.cursor)
})
