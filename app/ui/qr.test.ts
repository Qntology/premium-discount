// 실행: node --experimental-strip-types --test app/ui/qr.test.ts  (premium-discount 폴더에서)
// jsQR(스캐너 라이브러리)로 매트릭스를 실제로 읽어서, QR이 담고 있는 문자열을 확인한다.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import jsQR from 'jsqr'
import { QUIET_ZONE, qrModules, qrPath } from './qr.ts'

/** 매트릭스를 픽셀로 그려 스캐너에 넣는다 (흰 바탕, 여백 포함) */
function scan(modules: boolean[][], scale = 6): string | null {
	const n = modules.length + QUIET_ZONE * 2
	const w = n * scale
	const data = new Uint8ClampedArray(w * w * 4).fill(255)
	modules.forEach((row, r) =>
		row.forEach((dark, c) => {
			if (!dark) return
			for (let y = 0; y < scale; y++)
				for (let x = 0; x < scale; x++) {
					const i = (((r + QUIET_ZONE) * scale + y) * w + (c + QUIET_ZONE) * scale + x) * 4
					data[i] = data[i + 1] = data[i + 2] = 0
				}
		})
	)
	return jsQR(data, w, w)?.data ?? null
}

const HASH = 'ab12'.repeat(10)
const ADDRESS = `${HASH}.premium.discount@oauth.email`
const MAILTO = 'mailto:' + encodeURIComponent(ADDRESS) // 서버(/session/start)가 돌려주는 모양, 웹의 QR과 같다

test('QR을 스캐너로 읽으면 mailto 링크가 그대로 나온다', () => {
	assert.equal(scan(qrModules(MAILTO)), MAILTO)
	assert.equal(decodeURIComponent(MAILTO.slice('mailto:'.length)), ADDRESS, '메일 앱이 받는 사람 주소로 풀어 쓴다')
})

test('다른 문자열은 다른 QR이고, 짧은 문자열과 긴 문자열 모두 읽힌다', () => {
	assert.notDeepEqual(qrModules(MAILTO), qrModules(MAILTO.replace('ab12', 'ab13')))
	for (const text of ['mailto:a%40b.c', MAILTO, 'mailto:' + encodeURIComponent('x'.repeat(40) + '.premium.discount@oauth.email')]) {
		assert.equal(scan(qrModules(text)), text)
	}
})

test('매트릭스는 정사각형이고 세 모서리에 위치 표지(7×7 테두리)가 있다', () => {
	const m = qrModules(MAILTO)
	const n = m.length
	assert.ok(m.every((row) => row.length === n))
	assert.equal((n - 17) % 4, 0, 'QR 규격의 크기(17 + 4×버전)')
	for (const [r0, c0] of [[0, 0], [0, n - 7], [n - 7, 0]]) {
		for (let i = 0; i < 7; i++) {
			assert.ok(m[r0][c0 + i] && m[r0 + 6][c0 + i] && m[r0 + i][c0] && m[r0 + i][c0 + 6], `위치 표지 테두리 (${r0},${c0})`)
		}
		assert.ok(!m[r0 + 1][c0 + 1] && m[r0 + 3][c0 + 3], '안쪽은 비고 가운데는 채워진다')
	}
})

test('SVG 경로는 어두운 모듈을 빠짐없이, 겹치지 않게 덮는다', () => {
	const m = qrModules(MAILTO)
	const dark = m.flat().filter(Boolean).length
	const runs = [...qrPath(m).matchAll(/M(\d+) (\d+)h(\d+)v1h-\d+z/g)]
	assert.equal(runs.reduce((s, r) => s + Number(r[3]), 0), dark)
	const covered = new Set<string>()
	for (const [, x, y, len] of runs) for (let i = 0; i < Number(len); i++) assert.ok(!covered.has(`${Number(x) + i},${y}`) && covered.add(`${Number(x) + i},${y}`), '겹침 없음')
	for (const key of covered) {
		const [x, y] = key.split(',').map(Number)
		assert.ok(m[y][x], '어두운 모듈만 그린다')
	}
})
