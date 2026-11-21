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

// ---- 시나리오 5 (contracts/web_analytic_v1.json WA-02 · 03 · 04 · 05 · 07 의 수집 쪽) ----
// 새 규칙의 사례는 Rust(crates/premium-discount-analytic tests/web_analytic.rs)와 같은 파일을 읽는다
const s5 = JSON.parse(readFileSync(fileURLToPath(new URL('../../../crates/premium-discount-analytic/tests/data/web_analytic_s5_cases.json', import.meta.url)), 'utf8'))
const bytes = (v: any) => new TextEncoder().encode(JSON.stringify(v)).length

test('시나리오 5 사례: 아이콘 단추 이름표 · 열린 모달의 뿌리 · 가상 주소가 Rust 와 같다 (WA-02 · WA-03)', () => {
	const t = load()
	for (const c of s5.pug) {
		const doc = body(c.html)
		assert.equal(t.toPug(doc.body.firstElementChild), c.pug, c.name)
	}
	for (const c of s5.modal_href) assert.equal(t.modalHref(c.href, c.slug), c.expect, c.href)
	for (const c of s5.slug) assert.equal(t.slugify(c.raw), c.expect, c.raw)
	// 보내는 요소: 입력 칸은 값도 이름표도 없다
	const input = JSON.stringify(t.element(body('<input value="홍길동" aria-label="이름">').querySelector('input')))
	assert.ok(!input.includes('홍길동') && !input.includes('이름'), input)
})

test('모달 감지: 열린 aria-modal 고정 층만 화면 이동이고, 이름은 제목 → 단추 글 → 셀렉터 순이다 (WA-03)', () => {
	const t = load()
	const doc = body(`<div id="banner" style="position:fixed">광고</div><dialog id="closed" style="position:fixed"><button>결제하기</button></dialog>` +
		`<div id="drawer" role="dialog" aria-modal="true" style="position: fixed; right: 0"><h2>장바구니</h2><button>주문하기</button></div>`)
	const found = t.activeModal(doc)
	assert.equal(found && found.id, 'drawer')
	assert.equal(t.modalSlug(found), 'cart')
	// 수집에서 뺀 영역 안 · 뷰포트를 조금만 덮는 층은 아니다
	assert.equal(t.activeModal(body(`<div data-pd-ignore><div role="dialog" aria-modal="true" style="position:fixed"><h2>장바구니</h2></div></div>`)), null)
	const small = load()
	small.viewportArea = () => 0.05
	assert.equal(small.activeModal(doc), null)
	const one = (html: string) => body(html).body.firstElementChild
	assert.equal(t.modalSlug(one('<div role="dialog" aria-label="Checkout"><p>x</p></div>')), 'checkout')
	assert.equal(t.modalSlug(one('<div role="dialog"><p>배송지</p><button>결제하기</button></div>')), 'checkout')
	assert.equal(t.modalSlug(one('<div role="dialog" id="size-guide"><p>치수표</p></div>')), 'size-guide')
	assert.equal(t.modalSlug(one('<div role="dialog" id="modal-12345"><p>x</p></div>')), 'dialog')
})

test('떠날 때: 결말 사건이 첫 묶음(< 8 KB)에 먼저 가고, 합은 keepalive 한도 안이다 (WA-04)', () => {
	const t = load()
	const heavy = (i: number) => ({ chain: [{ tag: 'li', nth_of_type: i + 1, classes: ['card'] }], pug: `li\n    span | 무거운 상품 카드 이름이 아주 긴 ${i} 번째 상품 무료배송 오늘 출발\n` })
	const href = 'https://shop.example.com/list'
	const events: any[] = []
	for (let i = 1; i <= 30; i++) events.push({ type: 'click', href, sid: 's', seq: i, t_ms: i * 700, target: heavy(i), related: Array.from({ length: 30 }, (_, k) => heavy(k)) })
	events.push({ type: 'hover', href, sid: 's', seq: 31, t_ms: 22_000, dwell_ms: 900 })
	events.push({ type: 'scroll', href, sid: 's', seq: 32, t_ms: 23_000, flips: 3 })
	events.push({ type: 'leave', href, sid: 's', seq: 33, t_ms: 30_000, dirty: 1 })
	const packets = t.unloadPackets(events)
	const first = packets[0]
	assert.ok(bytes({ events: first }) < t.limits.FIRST_PACKET_BYTES, String(bytes({ events: first })))
	assert.deepEqual(first.map((e: any) => e.type), ['click', 'leave'], 'the leave and the last click come first')
	assert.ok(first.every((e: any) => !e.related || e.related.length <= t.limits.TERMINAL_RELATED))
	const total = packets.reduce((n: number, p: any[]) => n + bytes({ events: p }), 0)
	assert.ok(total <= t.limits.KEEPALIVE_BYTES, String(total))
	// 사건은 그대로(seq · t_ms)이고 두 번 가지 않으며, 원래 사건의 형제는 건드리지 않는다
	const sent = packets.flat()
	assert.equal(new Set(sent.map((e: any) => e.seq)).size, sent.length)
	for (const e of sent) assert.equal(e.t_ms, events.find((x) => x.seq === e.seq).t_ms)
	assert.equal(events[29].related.length, 30)
	// 한도를 넘는 무거운 누름은 버리지만, 작은 머묾 · 스크롤은 누름 뒤에 들어간다
	assert.ok(sent.length < events.length)
	const later = packets.slice(1).flat().map((e: any) => e.type)
	assert.ok(later.indexOf('hover') > later.lastIndexOf('click') && later.includes('scroll'), later.join(','))
	// 가벼운 큐는 모두 4건씩
	const light = t.unloadPackets([{ type: 'enter', href, seq: 1, t_ms: 0 }, { type: 'click', href, seq: 2, t_ms: 10 }, { type: 'scroll', href, seq: 3, t_ms: 20, flips: 1 }])
	assert.deepEqual(light.map((p: any[]) => p.map((e) => e.seq)), [[2], [1, 3]])
})

test('형제: 뷰포트에 보인 것만, 고른 항목에서 가까운 순으로 고른 것 포함 16개까지 (WA-05)', () => {
	const t = load()
	const doc = body(`<ul id="list">${Array.from({ length: 80 }, (_, i) => `<li class="card"><span>상품 ${i}</span></li>`).join('')}</ul>`)
	const items = Array.from(doc.querySelectorAll('li')) as any[]
	// 뷰포트에는 앞의 8개만 보인다
	t.viewportShare = (el: any) => (items.indexOf(el) < 8 ? 1 : 0)
	const ctx = t.listContext(items[2].querySelector('span'))
	assert.equal(ctx.item, items[2])
	assert.deepEqual(ctx.related.map((r: any) => r.textContent), [0, 1, 3, 4, 5, 6, 7].map((i) => `상품 ${i}`))
	// 배치를 모르면 가까운 15개 (고른 것 포함 16), 문서 순서
	const unknown = load()
	const deep = unknown.listContext(items[40].querySelector('span'))
	assert.deepEqual(deep.related.map((r: any) => items.indexOf(r)), [32, 33, 34, 35, 36, 37, 38, 39, 41, 42, 43, 44, 45, 46, 47])
})

test('탭 묶음: 부모 탭의 sessionStorage 를 물려받은 새 탭의 첫 페이지만 sid 를 새로 만든다 (WA-07)', () => {
	const t = load()
	assert.equal(t.inheritedTab(true, 1, 'navigate'), true)
	assert.equal(t.inheritedTab(true, 1, ''), true)
	assert.equal(t.inheritedTab(true, 1, 'reload'), false, 'a reload keeps the tab session')
	assert.equal(t.inheritedTab(true, 2, 'navigate'), false, 'the next page of the same tab keeps it')
	assert.equal(t.inheritedTab(null, 1, 'navigate'), false, 'a tab the visitor opened has nothing to inherit')
})

// 수집기를 가짜 창(linkedom 문서 · 메모리 sessionStorage · fetch 기록 · 손으로 돌리는 타이머)에서 돌린다
function collector(opts: any = {}) {
	const { document } = parseHTML(`<!doctype html><html><body>${opts.html ?? ''}</body></html>`)
	const docListeners: Record<string, any> = {}
	const rootListeners: Record<string, any> = {}
	;(document as any).addEventListener = (type: string, fn: any) => { docListeners[type] = fn }
	const sent: any[] = []
	const timers: any[] = []
	let observer: any = null
	const storage: Map<string, string> = opts.storage ?? new Map()
	const root: any = {
		document,
		navigator: {},
		location: { origin: 'https://shop.example.com', pathname: '/product/1', hash: '' },
		history: { length: opts.historyLength ?? 2 },
		opener: opts.opener ? {} : null,
		crypto: globalThis.crypto,
		scrollY: 0,
		sessionStorage: {
			getItem: (k: string) => (storage.has(k) ? storage.get(k) : null),
			setItem: (k: string, v: string) => { storage.set(k, String(v)) },
			removeItem: (k: string) => { storage.delete(k) },
		},
		fetch: (_url: string, init: any) => {
			sent.push({ events: JSON.parse(init.body).events, keepalive: !!init.keepalive, bytes: new TextEncoder().encode(init.body).length })
			return Promise.resolve({})
		},
		addEventListener: (type: string, fn: any) => { rootListeners[type] = fn },
		MutationObserver: class {
			constructor(fn: any) {
				observer = fn
			}
			observe() {}
		},
	}
	new Function('window', 'setTimeout', 'clearTimeout', source)(root, (fn: any) => timers.push(fn), () => {})
	root.PDTracker.visible = () => true
	const run = () => {
		while (timers.length) timers.shift()()
	}
	return { document, docListeners, rootListeners, sent, storage, run, mutate: () => observer && observer([]) }
}

test('수집: 드로어가 열리면 가상 주소로 들어오고, 사건에 탭 묶음이 붙고, 떠날 때 떠남이 첫 묶음이다 (WA-03 · WA-04 · WA-07)', () => {
	const c = collector({ html: `<main><button id="add">장바구니 담기</button></main><div id="drawer" style="position: fixed; right: 0"><h2>장바구니</h2><button id="order">주문하기</button></div>` })
	c.docListeners.click({ target: c.document.getElementById('add') })
	// 주소는 /product/1 그대로, 드로어가 aria-modal 로 열린다
	const drawer = c.document.getElementById('drawer')
	drawer.setAttribute('role', 'dialog')
	drawer.setAttribute('aria-modal', 'true')
	c.mutate()
	c.run()
	c.docListeners.click({ target: c.document.getElementById('order') })
	c.rootListeners.pagehide()
	const all = c.sent.flatMap((s: any) => s.events)
	const opened = all.find((e: any) => e.type === 'enter' && e.surface === 'modal_drawer')
	assert.equal(opened.href, 'https://shop.example.com/product/1#/__modal/cart')
	const order = all.find((e: any) => e.type === 'click' && e.target.pug.includes('주문하기'))
	assert.equal(order.href, opened.href)
	assert.equal(order.surface, 'modal_drawer')
	const add = all.find((e: any) => e.type === 'click' && e.target.pug.includes('장바구니 담기'))
	assert.equal(add.href, 'https://shop.example.com/product/1')
	assert.equal(add.surface, undefined)
	const tg = c.storage.get('pd.tracker.tg')
	assert.match(String(tg), /^[0-9a-f]{32}$/)
	assert.ok(all.every((e: any) => e.tab_group === tg && e.sid === c.storage.get('pd.tracker.sid')))
	const leaving = c.sent.filter((s: any) => s.keepalive)
	assert.ok(leaving[0].events.some((e: any) => e.type === 'leave') && leaving[0].bytes < 8000)
	assert.deepEqual(all.map((e: any) => e.seq).sort((a: number, b: number) => a - b), [1, 2, 3, 4, 5])
})

test('수집: 부모 탭의 sessionStorage 를 물려받은 새 탭은 sid 를 새로 만들고 탭 묶음은 이어 쓴다 (WA-07)', () => {
	const storage = new Map([['pd.tracker.sid', 'parent'], ['pd.tracker.start', '1'], ['pd.tracker.seq', '41'], ['pd.tracker.tg', 'tg-parent']])
	const child = collector({ opener: true, historyLength: 1, storage })
	child.rootListeners.pagehide()
	const ev = child.sent.flatMap((s: any) => s.events)
	assert.ok(ev.length > 0 && ev.every((e: any) => e.sid !== 'parent' && e.tab_group === 'tg-parent'))
	assert.equal(Math.min(...ev.map((e: any) => e.seq)), 1, 'a new tab session counts from 1')
	// 같은 탭의 다음 페이지(기록 2)는 새 sid 를 이어 쓴다
	const childSid = storage.get('pd.tracker.sid')
	const next = collector({ opener: true, historyLength: 2, storage })
	next.rootListeners.pagehide()
	assert.ok(next.sent.flatMap((s: any) => s.events).every((e: any) => e.sid === childSid && e.tab_group === 'tg-parent'))
})
