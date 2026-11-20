// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/cron/test/*.test.ts  (premium-discount 폴더에서)
// contracts/verdict_v2.json(참조 구현 contracts/gen_verdict_v2.py가 만든 계약)을 TypeScript 구현이 계약의 매개변수로 통과하는지 본다.
// 서버는 아직 v1(PARAMS)로 판정한다. v2(접근성 내비게이션의 implicit 관측 · nav 트랙)로 바꾸는 것은 서버 단계(M5)에서 한다.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { PARAMS, verdict, type Params, type VerdictCase, type Verdict } from '../src/verdict.ts'

const contract = JSON.parse(
	readFileSync(fileURLToPath(new URL('../../../contracts/verdict_v2.json', import.meta.url)), 'utf8')
) as { recipe: string; tolerance: number; params: Params; cases: (VerdictCase & { name: string; expect: Verdict })[] }

test('v2는 v1 매개변수에 nav 트랙과 implicit 가중치만 더한 것이다', () => {
	assert.equal(contract.recipe, 'pd-verdict/2')
	assert.deepEqual(contract.params, {
		...PARAMS,
		tracks: { ...PARAMS.tracks, nav: PARAMS.tracks.web },
		weights: { ...PARAMS.weights, implicit: 0.3 },
	})
})

test('v2 계약 사례를 계약의 매개변수로 모두 통과한다', () => {
	assert.ok(contract.cases.length >= 25, 'cases')
	const close = (a: number | null, b: number | null, what: string) => {
		if (a === null || b === null) return assert.equal(a, b, what)
		assert.ok(Math.abs(a - b) <= contract.tolerance, `${what}: ${a} vs ${b}`)
	}
	for (const c of contract.cases) {
		const got = verdict(c, contract.params)
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
