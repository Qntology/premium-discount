// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/cron/test/*.test.ts  (premium-discount 폴더에서)
// contracts/verdict_v1.json(참조 구현 contracts/gen_verdict_v1.py가 만든 계약)을 TypeScript 구현이 그대로 통과하는지 본다.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { PARAMS, RECIPE, baselineOf, verdict, type VerdictCase, type Verdict } from '../src/verdict.ts'

const contract = JSON.parse(
	readFileSync(fileURLToPath(new URL('../../../contracts/verdict_v1.json', import.meta.url)), 'utf8')
) as { recipe: string; tolerance: number; params: unknown; cases: (VerdictCase & { name: string; expect: Verdict })[] }

test('recipe와 매개변수가 계약과 같다', () => {
	assert.equal(contract.recipe, RECIPE)
	assert.deepEqual(contract.params, PARAMS)
})

test('계약 사례를 모두 통과한다', () => {
	assert.ok(contract.cases.length >= 15, 'cases')
	const close = (a: number | null, b: number | null, what: string) => {
		if (a === null || b === null) return assert.equal(a, b, what)
		assert.ok(Math.abs(a - b) <= contract.tolerance, `${what}: ${a} vs ${b}`)
	}
	for (const c of contract.cases) {
		const got = verdict(c)
		const want = c.expect
		close(got.evidence, want.evidence, `${c.name} evidence`)
		close(got.value, want.value, `${c.name} value`)
		close(got.rank, want.rank, `${c.name} rank`)
		assert.equal(got.level, want.level, `${c.name} level`)
		assert.equal(got.tier, want.tier, `${c.name} tier`)
		assert.equal(got.grade, want.grade, `${c.name} grade`)
		assert.equal(got.since_ms, want.since_ms, `${c.name} since`)
		assert.deepEqual(got.reasons, want.reasons, `${c.name} reasons`)
	}
})

test('기준선은 모집단 표준편차', () => {
	assert.deepEqual(baselineOf([]), { n: 0, mean: 0, sd: 0 })
	const b = baselineOf([1, -1, 1, -1])
	assert.equal(b.n, 4)
	assert.equal(b.mean, 0)
	assert.equal(b.sd, 1)
})
