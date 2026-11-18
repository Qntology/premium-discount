// 실행: node --experimental-strip-types --test web/desktop/stage.test.ts  (premium-discount 폴더에서)
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { STATES } from './localdb.ts'
import {
	DOCK_MARGIN,
	DRAG_THRESHOLD,
	FILTER_CHIPS,
	STAGE_SIZE,
	classifyPointer,
	effectiveStage,
	effectiveTier,
	feedLine,
	feedOpacity,
	feedRows,
	growsTo,
	itemIcon,
	layoutMode,
	matchesFilter,
	modelSummary,
	nextStage,
	parseStage,
	pillLine,
	placeWindow,
	platformFromUserAgent,
	quickPlaceholder,
	syncDot,
	walletLine,
	type Stage,
} from './stage.ts'

test('단계 크기는 기획안 값이고 style.css의 --stage-w / --stage-h와 같다', () => {
	assert.deepEqual(STAGE_SIZE[0], { width: 360, height: 56 })
	assert.deepEqual(STAGE_SIZE[1], { width: 360, height: 260 })
	assert.deepEqual(STAGE_SIZE[2], { width: 400, height: 580 })
	const css = readFileSync(new URL('./style.css', import.meta.url), 'utf8')
	for (const s of [0, 1, 2] as Stage[]) {
		const rule = css.match(new RegExp(`\\.shell\\[data-stage="${s}"\\]\\s*\\{([^}]*)\\}`))
		assert.ok(rule, `stage ${s} 규칙이 style.css에 있다`)
		assert.match(rule![1], new RegExp(`--stage-w:\\s*${STAGE_SIZE[s].width}px`), `stage ${s} 너비`)
		assert.match(rule![1], new RegExp(`--stage-h:\\s*${STAGE_SIZE[s].height}px`), `stage ${s} 높이`)
	}
})

test('데스크톱은 오버레이, 모바일은 전체 화면이고 전체 화면은 늘 2단계', () => {
	assert.equal(layoutMode('desktop'), 'overlay')
	assert.equal(layoutMode('android'), 'full')
	assert.equal(layoutMode('ios'), 'full')
	assert.equal(effectiveStage('full', 0), 2)
	assert.equal(effectiveStage('full', 1), 2)
	assert.equal(effectiveStage('overlay', 0), 0)
	assert.equal(effectiveStage('overlay', 1), 1)
})

test('미리보기용 ?stage= 값은 0 · 1 · 2만 받는다', () => {
	assert.equal(parseStage('0'), 0)
	assert.equal(parseStage('1'), 1)
	assert.equal(parseStage('2'), 2)
	assert.equal(parseStage('3'), null)
	assert.equal(parseStage(''), null)
	assert.equal(parseStage(null), null)
})

test('사용자 에이전트로 첫 화면만 고른다 (앱의 답이 오면 그쪽이 맞다)', () => {
	assert.equal(platformFromUserAgent('Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 wv'), 'android')
	assert.equal(platformFromUserAgent('Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)'), 'ios')
	assert.equal(platformFromUserAgent('Mozilla/5.0 (Windows NT 10.0; Win64; x64) Edg/131'), null)
	assert.equal(platformFromUserAgent(''), null)
	assert.equal(platformFromUserAgent(undefined), null)
})

test('단계 옮기기: 알약 → 1, 맨 위 줄 · 펼치기 → 2, Esc는 한 단계씩, 접기는 0', () => {
	assert.equal(nextStage(0, 'pill'), 1)
	assert.equal(nextStage(1, 'pill'), 1, '1단계에서 알약은 없다')
	assert.equal(nextStage(1, 'feed-top'), 2)
	assert.equal(nextStage(1, 'expand'), 2)
	assert.equal(nextStage(0, 'expand'), 2)
	assert.equal(nextStage(2, 'escape'), 1)
	assert.equal(nextStage(1, 'escape'), 0)
	assert.equal(nextStage(0, 'escape'), 0)
	assert.equal(nextStage(2, 'collapse'), 0)
	assert.equal(nextStage(1, 'collapse'), 0)
	// 앱이 시작하지 못했으면 피드가 비어 있으므로 바로 시작 안내(2단계)로 가고, Esc는 알약으로
	assert.equal(nextStage(0, 'pill', { ready: false }), 2)
	assert.equal(nextStage(2, 'escape', { ready: false }), 0)
})

test('5px 전에 떼면 누르기, 넘으면 끌기', () => {
	assert.equal(DRAG_THRESHOLD, 5)
	assert.equal(classifyPointer(0, 0), 'click')
	assert.equal(classifyPointer(3, 3), 'click', '대각선 4.24px')
	assert.equal(classifyPointer(4, 3), 'drag', '정확히 5px')
	assert.equal(classifyPointer(-6, 0), 'drag')
	assert.equal(classifyPointer(0, -2), 'click')
})

test('창 자리: 처음에는 작업 영역 오른쪽 아래에 여백 16px로 붙는다 (배율 반영)', () => {
	assert.equal(DOCK_MARGIN, 16)
	const area = { x: 0, y: 0, width: 1920, height: 1040 } // 작업 표시줄 40px를 뺀 1080p
	assert.deepEqual(placeWindow(STAGE_SIZE[0], area, 1), { x: 1920 - 360 - 16, y: 1040 - 56 - 16 })
	// 150% 배율: 크기와 여백 모두 물리 픽셀로
	const hi = { x: 0, y: 0, width: 2880, height: 1560 }
	assert.deepEqual(placeWindow(STAGE_SIZE[2], hi, 1.5), { x: 2880 - 600 - 24, y: 1560 - 870 - 24 })
	// 두 번째 모니터(왼쪽에 있어 x가 음수)
	const left = { x: -1920, y: 0, width: 1920, height: 1080 }
	assert.deepEqual(placeWindow(STAGE_SIZE[1], left, 1), { x: -360 - 16, y: 1080 - 260 - 16 })
	// 배율 값이 이상하면 1로
	assert.deepEqual(placeWindow(STAGE_SIZE[0], area, 0), placeWindow(STAGE_SIZE[0], area, 1))
})

test('창 자리: 끌어 옮긴 창은 오른쪽 아래 모서리를 지키고, 작업 영역 밖으로 나가지 않는다', () => {
	const area = { x: 0, y: 0, width: 1920, height: 1040 }
	// 알약(360x56)을 화면 가운데쯤으로 옮긴 뒤 2단계로 키운다: 모서리 (1000, 600) 고정
	assert.deepEqual(placeWindow(STAGE_SIZE[2], area, 1, { right: 1000, bottom: 600 }), { x: 600, y: 20 })
	// 왼쪽 위 구석에 둔 알약을 키우면 위 · 왼쪽으로 넘치지 않게 민다
	assert.deepEqual(placeWindow(STAGE_SIZE[2], area, 1, { right: 370, bottom: 70 }), { x: 0, y: 0 })
	// 모서리가 화면 밖이면(모니터를 뺐을 때) 안으로 당긴다
	assert.deepEqual(placeWindow(STAGE_SIZE[0], area, 1, { right: 5000, bottom: 5000 }), { x: 1920 - 360, y: 1040 - 56 })
	// 작업 영역보다 큰 창은 왼쪽 위에 맞춘다
	assert.deepEqual(placeWindow(STAGE_SIZE[2], { x: 0, y: 0, width: 300, height: 400 }, 1), { x: 0, y: 0 })
})

test('커지는 쪽이면 먼저 옮기고, 작아지는 쪽이면 먼저 줄인다', () => {
	assert.equal(growsTo(0, 1), true)
	assert.equal(growsTo(1, 2), true)
	assert.equal(growsTo(0, 2), true)
	assert.equal(growsTo(2, 0), false)
	assert.equal(growsTo(1, 0), false)
	assert.equal(growsTo(1, 1), false)
})

// Rust crate(premium-discount-window)가 같은 표를 읽는다. stage.ts 를 고치면 표를 다시 만들고(아래 명령) Rust 쪽도 맞춘다:
//   node --experimental-strip-types crates/premium-discount-window/tests/gen_stage_cases.mjs  (premium-discount 폴더에서)
test('Rust crate와 같은 표: 창 자리 · 단계 옮기기 · 누르기와 끌기 · 커지는 쪽', () => {
	const cases = JSON.parse(readFileSync(new URL('../../crates/premium-discount-window/tests/stage_cases.json', import.meta.url), 'utf8'))
	assert.deepEqual(cases.constants, { stage_size: [STAGE_SIZE[0], STAGE_SIZE[1], STAGE_SIZE[2]], dock_margin: DOCK_MARGIN, drag_threshold: DRAG_THRESHOLD })
	assert.ok(cases.place_window.length > 0)
	for (const c of cases.place_window) assert.deepEqual(placeWindow(c.size, c.area, c.scale, c.anchor, c.margin), c.expected, c.name)
	assert.equal(cases.next_stage.length, 3 * 5 * 2, '단계 3 x 사건 5 x ready 2')
	for (const c of cases.next_stage) assert.equal(nextStage(c.current, c.event, { ready: c.ready }), c.expected, JSON.stringify(c))
	assert.ok(cases.classify_pointer.length > 0)
	for (const c of cases.classify_pointer) assert.equal(classifyPointer(c.dx, c.dy, c.threshold), c.expected, JSON.stringify(c))
	assert.equal(cases.grows_to.length, 9)
	for (const c of cases.grows_to) assert.equal(growsTo(c.from, c.to), c.expected, JSON.stringify(c))
})

const row = (n: number, state = 'stored', cc = 'example.com') => ({ created_at: n * 1000, state, cc, text: `글 ${n}` })

test('미니 피드: 최근 3개, 오래된 것이 위 · 새것이 맨 아래', () => {
	const rows = [row(5), row(1), row(9), row(7), row(3)] // Dexie는 최신순이지만 순서에 기대지 않는다
	assert.deepEqual(feedRows(rows).map((r) => r.text), ['글 5', '글 7', '글 9'])
	assert.deepEqual(feedRows(rows, 1).map((r) => r.text), ['글 9'])
	assert.deepEqual(feedRows([row(2)]).map((r) => r.text), ['글 2'])
	assert.deepEqual(feedRows([]), [])
	assert.equal(rows[0].text, '글 5', '원래 배열은 그대로')
	assert.equal(feedOpacity(0), 1)
	assert.equal(feedOpacity(1), 0.8)
	assert.ok(feedOpacity(2) < 0.8)
})

test('판정 등급: 판정 전(grade 없음)은 unrated', () => {
	assert.equal(effectiveTier(null), 'unrated')
	assert.equal(effectiveTier(undefined), 'unrated')
	assert.equal(effectiveTier({ tier: 'premium', grade: null }), 'unrated')
	assert.equal(effectiveTier({ tier: 'premium', grade: 1 }), 'premium')
	assert.equal(effectiveTier({ tier: 'discount', grade: -2 }), 'discount')
	assert.equal(effectiveTier({ tier: 'neutral', grade: 0 }), 'neutral')
})

test('아이콘과 피드 한 줄: 등록된 글은 판정, 나머지는 상태', () => {
	for (const s of STATES) assert.notEqual(itemIcon(s), '', s)
	assert.equal(itemIcon('stored', 'premium'), '⭐')
	assert.equal(itemIcon('stored', 'discount'), '🔻')
	assert.equal(itemIcon('stored'), '✓')
	assert.equal(itemIcon('queued'), '📤')
	assert.equal(itemIcon('pending'), '⏳')
	assert.equal(itemIcon('rejected'), '⛔')
	assert.equal(itemIcon('queued', 'premium'), '📤', '보내지 못한 글은 판정보다 상태가 먼저')
	assert.equal(itemIcon(null, 'premium'), '⭐', '서버 결과(상태 없음)는 판정으로')
	assert.equal(itemIcon(null), '·')
	assert.deepEqual(feedLine({ cc: 'shop.kr', state: 'queued', text: '  배송이\n빨라요 ' }), { icon: '📤', tag: 'shop.kr · 전송 대기', preview: '배송이 빨라요' })
	assert.equal(feedLine({ cc: 'shop.kr', state: 'stored', text: 'x' }, 'premium').tag, 'shop.kr · 추천')
	assert.equal(feedLine({ cc: 'shop.kr', state: 'stored', text: 'x' }, 'discount').tag, 'shop.kr · 비추천')
	assert.equal(feedLine({ cc: 'shop.kr', state: 'stored', text: 'x' }).tag, 'shop.kr · 등록됨')
	assert.equal(feedLine({ cc: '', state: 'pending', text: 'x' }).tag, '도메인 없음 · 검수 중')
	assert.equal(feedLine({ cc: 'a', state: 'stored', text: '가'.repeat(80) }).preview, '가'.repeat(60) + '…')
})

test('동기화 점: 오프라인 > 전송 대기 > 검수 중 > 등록됨', () => {
	const q = { state: 'queued' }
	const p = { state: 'pending' }
	const s = { state: 'stored' }
	assert.deepEqual(syncDot([s, q, q, p], 'down'), { tone: 'bad', label: '오프라인 · 대기 2건' })
	assert.deepEqual(syncDot([s], 'down'), { tone: 'bad', label: '오프라인' })
	assert.deepEqual(syncDot([s, q, p], 'ok'), { tone: 'wait', label: '전송 대기 1건' })
	assert.deepEqual(syncDot([s, q], null), { tone: 'wait', label: '전송 대기 1건' }, '연결을 모르면 행 상태로')
	assert.deepEqual(syncDot([s, p, p], 'ok'), { tone: 'busy', label: '검수 중 2건' })
	assert.deepEqual(syncDot([s, { state: 'rejected' }], 'ok'), { tone: 'ok', label: '등록됨' })
	assert.deepEqual(syncDot([], null), { tone: 'muted', label: '쓴 글 없음' })
	assert.deepEqual(syncDot([q], 'down', false), { tone: 'bad', label: '설정 필요' })
})

test('알약 한 줄: 1분 안의 알림, 아니면 최근 글, 없으면 안내', () => {
	const now = 1_000_000_000
	const latest = { cc: 'shop.kr', state: 'stored', text: '좋아요\n정말', created_at: now - 10_000 }
	assert.equal(pillLine({ title: '등록했습니다', at: now - 5_000 }, latest, now), '등록했습니다')
	assert.equal(pillLine({ title: '등록했습니다', at: now - 61_000 }, latest, now), 'shop.kr · 좋아요 정말 · 방금')
	assert.equal(pillLine(null, latest, now), 'shop.kr · 좋아요 정말 · 방금')
	assert.equal(pillLine(null, null, now), '아직 쓴 글이 없습니다')
})

test('빠른 입력 안내는 현재 도메인을 넣고, 없으면 먼저 정하라고 한다', () => {
	assert.equal(quickPlaceholder('example.com'), 'example.com에 빠른 메모/평가 남기기...')
	assert.equal(quickPlaceholder('  '), '도메인을 먼저 정하세요 (전체 화면의 평가 쓰기)')
	assert.equal(quickPlaceholder(null), '도메인을 먼저 정하세요 (전체 화면의 평가 쓰기)')
})

test('걸러 보기: 칩 네 개와 cc', () => {
	assert.deepEqual(FILTER_CHIPS.map((c) => c.id), ['all', 'premium', 'discount', 'waiting'])
	const items = [
		{ state: 'stored', tier: 'premium' as const, cc: 'a.com' },
		{ state: 'stored', tier: 'discount' as const, cc: 'b.com' },
		{ state: 'queued', tier: 'unrated' as const, cc: 'a.com' },
		{ state: 'pending', tier: 'unrated' as const, cc: 'b.com' },
		{ state: null, tier: 'premium' as const, cc: 'B.com ' }, // 서버 결과: 상태 없음
		{ state: 'rejected', tier: 'unrated' as const, cc: null },
	]
	const pick = (f: Parameters<typeof matchesFilter>[1], cc: string | null = null) => items.flatMap((it, i) => (matchesFilter(it, f, cc) ? [i] : []))
	assert.deepEqual(pick('all'), [0, 1, 2, 3, 4, 5])
	assert.deepEqual(pick('premium'), [0, 4])
	assert.deepEqual(pick('discount'), [1])
	assert.deepEqual(pick('waiting'), [2, 3])
	assert.deepEqual(pick('all', 'b.com'), [1, 3, 4], 'cc는 대소문자 · 공백 무시')
	assert.deepEqual(pick('premium', 'b.com'), [4])
	assert.deepEqual(pick('waiting', ' A.COM'), [2])
	assert.deepEqual(pick('all', '  '), [0, 1, 2, 3, 4, 5], '빈 cc는 거르지 않는다')
})

test('지갑 칸은 세션 주소만 보여 주고 지갑 명령이 없다고 말한다', () => {
	const signed = walletLine({ state: 'signed_in', address: '0x1234567890abcdef1234' })
	assert.equal(signed.address, '0x1234567890abcdef1234')
	assert.match(signed.note, /아직 앱에 없습니다/)
	const guest = walletLine({ state: 'anonymous', address: null })
	assert.equal(guest.address, '')
	assert.match(guest.note, /로그인하면/)
	assert.match(guest.note, /아직 앱에 없습니다/)
})

test('모델 한 줄', () => {
	assert.equal(modelSummary({ installed: 'text', revision: 'abcdef1234567' }), '임베딩 모델: 텍스트 전용 설치됨 · 판 abcdef1')
	assert.match(modelSummary({ installed: 'full', revision: '' }), /^임베딩 모델: 전체/)
	assert.equal(modelSummary({ installed: 'none', revision: 'x' }), '임베딩 모델: 설치되지 않음')
})
