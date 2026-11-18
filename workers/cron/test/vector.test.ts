// 실행: node --experimental-strip-types --test workers/cron/test/vector.test.ts  (premium-discount 폴더에서)
// 외부 의존성 없이 Node 내장 test runner만 쓴다.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { toVector, VECTOR_DIMENSIONS } from '../src/vector.ts'

function sample(): Float32Array {
	const v = new Float32Array(VECTOR_DIMENSIONS)
	for (let i = 0; i < v.length; i++) v[i] = (i - 384) / 768 // 음수, 0, 소수를 모두 포함
	return v
}

test('ArrayBuffer로 오면 그대로 읽는다', () => {
	const v = sample()
	assert.deepEqual(toVector(v.buffer.slice(0)), v)
})

test('D1처럼 바이트 숫자 배열로 와도 같은 벡터를 얻는다', () => {
	const v = sample()
	const asArray = Array.from(new Uint8Array(v.buffer)) // D1이 BLOB을 돌려주는 형태
	assert.equal(asArray.length, 3072)
	const out = toVector(asArray)
	assert.equal(out.length, VECTOR_DIMENSIONS)
	assert.deepEqual(out, v)
})

test('배열을 Float32Array에 바로 넘기던 이전 방식은 3072차원이 된다 (회귀 방지용 설명)', () => {
	const asArray = Array.from(new Uint8Array(sample().buffer))
	assert.equal(new Float32Array(asArray as unknown as ArrayBuffer).length, 3072)
	assert.equal(toVector(asArray).length, VECTOR_DIMENSIONS)
})

test('Uint8Array 같은 뷰도 받고, 오프셋이 있어도 정렬 문제 없이 읽는다', () => {
	const v = sample()
	const padded = new Uint8Array(3 + 3072)
	padded.set(new Uint8Array(v.buffer), 3) // 시작 위치가 4의 배수가 아니다
	assert.deepEqual(toVector(padded.subarray(3)), v)
})

test('길이가 다르면 거부한다', () => {
	assert.throws(() => toVector(new ArrayBuffer(100)), /expected 3072/)
	assert.throws(() => toVector(Array.from({ length: 3071 }, () => 0)), /expected 3072/)
	assert.throws(() => toVector(new Float32Array(1536).buffer), /expected 3072/)
})

test('바이트가 아닌 값이 섞인 배열은 조용히 깎지 않고 거부한다', () => {
	const bad = Array.from({ length: 3072 }, () => 0)
	for (const x of [256, -1, 1.5, NaN, '7' as unknown as number]) {
		const arr = [...bad]
		arr[10] = x
		assert.throws(() => toVector(arr), /non-byte/)
	}
})

test('지원하지 않는 형태는 거부하고, 오류 메시지에 값이 들어가지 않는다', () => {
	for (const x of [null, undefined, 'abc', 42, {}]) {
		assert.throws(() => toVector(x), /unexpected type/)
	}
	try {
		toVector(Array.from({ length: 3072 }, () => 7).concat([300]))
	} catch (e) {
		assert.ok(!String((e as Error).message).includes('300'))
	}
})
