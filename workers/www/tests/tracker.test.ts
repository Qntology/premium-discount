// 실행: node --experimental-strip-types --import ./test-support/ts-resolve.mjs --test workers/www/tests/*.test.ts  (premium-discount 폴더에서)
// 수집 스크립트(web/www/static/js/tracker.js)의 순수 함수: 속성 없는 PUG(계약 사례 = Rust html_to_pug), 조상 모양, 목록 형제, 크기 어림.
// DOM 은 linkedom 이다(배치 · 계산 스타일이 없으므로 보이는지 판정은 시험에서 바꿔 끼운다).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { parseHTML } from 'linkedom'

const source = readFileSync(fileURLToPath(new URL('../../../web/www/static/js/tracker.js', import.meta.url)), 'utf8')
const pugCases = JSON.parse(readFileSync(fileURLToPath(new URL('../../../contracts/analytic_pug_v1.json', import.meta.url)), 'utf8'))

// 시험 모드로 불러온다: 함수만 내놓고 수집은 시작하지 않는다
function load(): any {
	const g = globalThis as any
	g.PD_TRACKER_TEST = true
	new Function(source)()
	const api = g.PDTracker
	api.visible = () => true
	return api
}

function body(html: string) {
	const { document } = parseHTML(`<!doctype html><html><body>${html}</body></html>`)
	return document
}

test('계약 사례: 브라우저 DOM 의 PUG 가 Rust html_to_pug 와 같다', () => {
	const t = load()
	assert.ok(pugCases.cases.length >= 12)
	for (const c of pugCases.cases) {
		const doc = body(c.html)
		const el = doc.body.firstElementChild
		assert.equal(t.toPug(el), c.pug, c.name)
	}
})

test('시험 모드에서는 수집을 시작하지 않는다', () => {
	const t = load()
	assert.equal(t.flush, undefined)
	assert.equal(typeof t.toPug, 'function')
})

test('조상 모양: 태그 · id · class · 허용 속성 · 같은 태그 중 순서, 글 · 링크 · 입력값 없음', () => {
	const t = load()
	const doc = body(`<main><ul id="products"><li class="card"><a href="/p/1">A</a></li><li class="card active"><a href="/p/2" data-testid="buy-2" title="비밀">B</a></li></ul></main>`)
	const link = doc.querySelectorAll('a')[1]
	const chain = t.domChain(link)
	assert.deepEqual(chain[0], { tag: 'a', nth_of_type: 1, attrs: [['data-testid', 'buy-2']] })
	assert.deepEqual(chain[1], { tag: 'li', nth_of_type: 2, classes: ['card', 'active'] })
	assert.deepEqual(chain[2], { tag: 'ul', nth_of_type: 1, id: 'products' })
	assert.equal(chain[3].tag, 'main')
	assert.equal(chain.length, 4, 'body 에서 멈춘다')
	assert.ok(!JSON.stringify(chain).includes('/p/2') && !JSON.stringify(chain).includes('비밀'))
})

test('목록 형제: ul 의 같은 모양 카드, div 목록, 상태 class 는 보지 않는다', () => {
	const t = load()
	const doc = body(`<ul><li class="card"><span>상품 1</span></li><li class="card on"><span>상품 2</span></li><li class="card"><span>상품 3</span></li><li class="ad"><span>광고</span></li></ul>`)
	const span = doc.querySelectorAll('span')[1]
	const ctx = t.listContext(span)
	assert.equal(ctx.item.tagName.toLowerCase(), 'li')
	assert.deepEqual(ctx.related.map((r: any) => r.textContent), ['상품 1', '상품 3'])

	const grid = body(`<div class="grid"><div class="tile x"><b>A</b></div><div class="tile x"><b>B</b></div><div class="tile x"><b>C</b></div></div>`)
	const b = grid.querySelectorAll('b')[2]
	const g = t.listContext(b)
	assert.equal(g.item.className, 'tile x')
	assert.deepEqual(g.related.map((r: any) => r.textContent), ['A', 'B'])

	// 목록이 아니면 형제 없음
	const lone = body(`<section><button id="pay">결제</button></section>`)
	assert.deepEqual(t.listContext(lone.querySelector('button')).related, [])
})

test('보내는 요소는 이메일 · 긴 숫자를 지운다', () => {
	const t = load()
	const doc = body(`<div class="addr"><span>홍길동</span> <span>010-1234-5678</span> <span>hong@example.com</span></div>`)
	const el = t.element(doc.querySelector('div'))
	assert.equal(el.pug, 'div | 홍길동 # #\n')
	assert.deepEqual(el.chain, [{ tag: 'div', nth_of_type: 1, classes: ['addr'] }])
})

test('크기 어림: 너무 큰 사건은 형제를 반씩 줄이고, 그래도 크면 버린다', () => {
	const t = load()
	const related = Array.from({ length: 100 }, (_, i) => ({ chain: [{ tag: 'li', nth_of_type: i + 1, classes: ['card'] }], pug: `li | 상품 이름이 꽤 긴 카드 ${i}` }))
	const ev = t.fitBudget({ type: 'click', href: 'https://shop.example.com/list', sid: 's', seq: 1, t_ms: 1, target: { chain: [], pug: 'li | 상품' }, related })
	assert.ok(ev && ev.related.length > 0 && ev.related.length < 100, String(ev?.related?.length))
	assert.ok(t.approximateTokenCount(JSON.stringify(ev)) * 5 <= 7000)
	const huge = t.fitBudget({ type: 'click', href: 'x', sid: 's', seq: 1, t_ms: 1, target: { chain: [], pug: 'p | ' + '가'.repeat(5000) } })
	assert.equal(huge, null)
})
