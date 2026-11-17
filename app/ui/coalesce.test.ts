// 실행: node --experimental-strip-types --test app/ui/coalesce.test.ts  (premium-discount 폴더에서)
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { coalesced, type RefreshOptions } from './coalesce.ts'

/** 끝나는 시점을 테스트가 정하는 가짜 갱신 */
function harness() {
	const started: RefreshOptions[] = []
	const finishers: (() => void)[] = []
	let active = 0
	let maxActive = 0
	const refresh = coalesced((opts) => {
		started.push(opts)
		active++
		maxActive = Math.max(maxActive, active)
		return new Promise<void>((resolve) => {
			finishers.push(() => {
				active--
				resolve()
			})
		})
	})
	return { refresh, started, finish: () => finishers.shift()!(), maxActive: () => maxActive }
}
const tick = () => new Promise((r) => setTimeout(r, 0))

test('혼자 불리면 바로 한 번 실행한다', async () => {
	const h = harness()
	const p = h.refresh({ flush: true })
	assert.deepEqual(h.started, [{ flush: true }])
	h.finish()
	await p
})

test('진행 중에 불리면 버리지 않고, 끝난 뒤 한 번만 더 실행한다 (등록 직후 목록 갱신이 사라지던 문제)', async () => {
	const h = harness()
	const first = h.refresh({ flush: true }) // 시작할 때의 확인
	const afterSubmit = h.refresh({ flush: false }) // 등록 직후의 목록 갱신
	assert.equal(h.started.length, 1, '아직 겹쳐 실행하지 않는다')
	let done = false
	void afterSubmit.then(() => (done = true))
	h.finish()
	await first
	await tick()
	assert.equal(h.started.length, 2, '끝나자 이어서 한 번 더')
	assert.equal(done, false, '등록 직후의 갱신은 더 실행한 것이 끝나야 이행된다')
	h.finish()
	await afterSubmit
	assert.equal(done, true)
	assert.equal(h.maxActive(), 1, '동시에 둘 이상 돌지 않는다')
})

test('진행 중에 여러 번 불려도 더 실행하는 것은 한 번이고, flush가 하나라도 있으면 flush한다', async () => {
	const h = harness()
	const first = h.refresh({ flush: false })
	const a = h.refresh({ flush: false })
	const b = h.refresh({ flush: true })
	const c = h.refresh({ flush: false })
	h.finish()
	await first
	await tick()
	assert.deepEqual(h.started, [{ flush: false }, { flush: true }])
	h.finish()
	await Promise.all([a, b, c])
	await tick()
	assert.equal(h.started.length, 2, '세 번 불렸어도 더는 없다')
	assert.equal(h.maxActive(), 1)
})

test('더 실행하는 동안 또 불리면 그 뒤에 다시 한 번 실행한다', async () => {
	const h = harness()
	const first = h.refresh({ flush: false })
	const second = h.refresh({ flush: false })
	h.finish()
	await first
	await tick() // 두 번째가 돌기 시작했다
	const third = h.refresh({ flush: true })
	h.finish()
	await second
	await tick()
	assert.equal(h.started.length, 3)
	assert.equal(h.started[2].flush, true)
	h.finish()
	await third
})

test('갱신이 실패해도 다음 갱신은 막히지 않는다', async () => {
	let calls = 0
	const refresh = coalesced(async () => {
		calls++
		if (calls === 1) throw new Error('boom')
	})
	await refresh({ flush: false })
	await refresh({ flush: false })
	assert.equal(calls, 2)
})
