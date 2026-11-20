// 앱 웹뷰 화면: 3단계 오버레이(알약 → 미니 피드 → 서랍). 서랍에는 평가 쓰기, 내 평가 상태 목록, 검색, 계정 · 지갑 · 모델이 있다.
//
// 화면은 평가를 직접 만들거나 고치지 않는다 (localdb.ts의 규칙). 쓰기는 항상 Rust 명령을 거치고,
// 목록은 Rust가 내보낸 행을 Dexie 캐시에 반영(syncCacheFromRust)한 뒤 거기서 읽는다.
// 미니 피드와 알약도 같은 행(내 평가)을 쓴다. 앱에는 따로 피드 명령이 없다.
//
// 흐름
//   등록: cmd_submit_evaluation → (결과 안내) → 캐시 동기화 → 목록 갱신. 1단계의 빠른 입력도 같은 명령을 현재 도메인(cc)으로 부른다
//   확인: cmd_flush_outbox(못 보낸 글 전송, 접수된 글의 최종 결과 확인) → 캐시 동기화 → 판정(cmd_verdicts) → 목록 갱신
//         시작할 때, "지금 확인"을 누를 때, 연결이 돌아왔을 때, 보낼 것이 남아 있으면 10초마다
//   검색: cmd_search_merged (내 글 + 서버 결과를 한 번의 임베딩으로), 추천 / 비추천은 cmd_vote
//   로그인: cmd_login_start(QR와 메일 주소) → 3초마다 cmd_login_status → 메일이 도착하면 로그인 (세션은 Rust가 저장한다)
//   단계 창: 단계 규칙은 stage.ts, 창 크기 · 위치는 아래 "단계 창"이 Tauri 창 API로 바꾼다 (브라우저에서는 CSS만)

import Dexie from 'dexie'

import { coalesced } from './coalesce.ts'
import { makeCommands, syncCacheFromRust, toCommandFailure, type FetchProgress, type MatchVerdict, type MergedMatch, type ModelVariant, type SubmitResult } from './commands.ts'
import { openLocalDb, type DexieLike } from './localdb.ts'
import { openExternal, pickInvoke } from './transport.ts'
import { renderQr } from './qr.ts'
import {
	FILTER_CHIPS,
	STAGE_SIZE,
	classifyPointer,
	effectiveStage,
	effectiveTier,
	feedLine,
	feedOpacity,
	feedRows,
	growsTo,
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
	itemIcon,
	walletLine,
	type FeedFilter,
	type LayoutMode,
	type Stage,
	type StageEvent,
} from './stage.ts'
import {
	accountView,
	byteCounter,
	canVote,
	describeError,
	describeVote,
	describeLoginError,
	describeLoginPhase,
	describeDownload,
	describeDraftModel,
	describeFilter,
	formatDraft,
	describeStartup,
	describeSubmit,
	isModelMissing,
	modelChoices,
	nextVote,
	loginMode,
	mailtoLink,
	normalizePlatform,
	formatAgo,
	previewText,
	reasonLabel,
	scorePercent,
	sourceView,
	stateView,
	tierView,
	type LoginPhase,
	type Notice,
	type Platform,
} from './view.ts'

const invoke = pickInvoke()
const commands = makeCommands(invoke)
const db = openLocalDb(Dexie as unknown as new (name: string) => DexieLike)

const RECHECK_MS = 10_000
const LOGIN_POLL_MS = 3_000
const LOGIN_MAX_ERRORS = 5
const LAST_CC_KEY = 'pd.lastCc'
const STATUS_TICK_MS = 30_000

// ---- DOM ----
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T
const shell = $('shell')
const form = $<HTMLFormElement>('form')
const ccInput = $<HTMLInputElement>('cc')
const textInput = $<HTMLTextAreaElement>('text')
const bytesEl = $('bytes')
const submitBtn = $<HTMLButtonElement>('submit')
const noticeEl = $('notice')
const replyBox = $('replyTo')
const replyLabel = $('replyLabel')
const composeBox = $<HTMLDetailsElement>('composeBox')
const listEl = $('list')
const mineCount = $('mineCount')
const netEl = $('net')
const searchForm = $<HTMLFormElement>('searchForm')
const queryInput = $<HTMLInputElement>('q')
const onlyCc = $<HTMLInputElement>('onlyCc')
const ccChipLabel = $('ccChipLabel')
const chipsEl = $('chips')
const searchNotice = $('searchNotice')
const resultsBox = $('resultsBox')
const resultsCount = $('resultsCount')
const resultsEl = $('results')
const acctLabel = $('acctLabel')
const loginBtn = $<HTMLButtonElement>('loginBtn')
const logoutBtn = $<HTMLButtonElement>('logoutBtn')
const accountWarn = $('accountWarn')
const authorEl = $('author')
const walletAddr = $('walletAddr')
const walletNote = $('walletNote')
const loginModal = $('loginModal')
const qrBox = $('qrBox')
const mailAddr = $('mailAddr')
const copyAddr = $<HTMLButtonElement>('copyAddr')
const loginStatusEl = $('loginStatus')
const loginRetry = $<HTMLButtonElement>('loginRetry')
const mailLink = $<HTMLAnchorElement>('mailLink')
const loginIntro = $('loginIntro')
const loginCard = loginModal.querySelector<HTMLElement>('.modal-card')!
const pillBtn = $<HTMLButtonElement>('pill')
const feedEl = $('feed')
const quickForm = $<HTMLFormElement>('quickForm')
const quickText = $<HTMLInputElement>('quickText')
const quickSubmit = $<HTMLButtonElement>('quickSubmit')
const quickNotice = $('quickNotice')

// 앱이 실행 중인 플랫폼. 앱(Rust)이 알려 주고(cmd_app_status), 모르면 데스크톱으로 본다.
// 개발용 브라우저에서 모바일 화면을 미리 볼 수 있도록 ?platform=android 를 허용한다 (레이아웃만 바뀐다).
// 앱이 답하기 전의 첫 화면은 사용자 에이전트로 짐작한다 (Android 웹뷰에서 알약이 잠깐 보이지 않게)
const params = new URLSearchParams(location.search)
let platform: Platform = normalizePlatform(params.get('platform') ?? platformFromUserAgent(navigator.userAgent))
let mode: LayoutMode = layoutMode(platform)
let stage: Stage = effectiveStage(mode, parseStage(params.get('stage')) ?? 0)
/** 앱(Rust 코어)이 시작했는지. 시작하지 못했으면 알약을 눌렀을 때 바로 시작 안내(2단계)로 간다 */
let appReady = true

function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: { class?: string; text?: string; tone?: string; type?: string } = {}, ...children: (Node | string)[]): HTMLElementTagNameMap[K] {
	const node = document.createElement(tag)
	if (props.class) node.className = props.class
	if (props.text !== undefined) node.textContent = props.text
	if (props.tone) node.dataset.tone = props.tone
	if (props.type && node instanceof HTMLButtonElement) node.type = props.type as 'button'
	for (const c of children) node.append(c)
	return node
}

// 알약에 보여 줄 최근 알림 (등록 · 투표 · 오류). 1분이 지나면 최근 글 한 줄로 돌아간다 (stage.ts pillLine)
let lastNotice: { title: string; at: number } | null = null

// 사용자가 쓴 글과 서버가 준 값은 모두 textContent로만 넣는다 (HTML로 해석하지 않는다)
function showNotice(target: HTMLElement, n: Notice | null) {
	target.replaceChildren()
	target.hidden = n === null
	if (!n) return
	target.dataset.tone = n.tone
	target.append(el('strong', { text: n.title }))
	if (n.detail) target.append(el('span', { text: n.detail }))
	if (target !== loginStatusEl) rememberNotice(n)
}

function rememberNotice(n: Notice) {
	lastNotice = { title: n.title, at: Date.now() }
	renderStatus()
}

// 1단계의 한 줄 안내 (자세한 말은 마우스를 올리면)
function showQuick(n: Notice | null) {
	quickNotice.hidden = n === null
	if (!n) return
	quickNotice.dataset.tone = n.tone
	quickNotice.textContent = n.title
	quickNotice.title = n.detail ?? ''
	rememberNotice(n)
}

// ---- 온라인 표시 ----
// 서버와 실제로 주고받은 결과가 있을 때만 보여 준다 (보낼 것이 없을 때의 "연결됨"은 확인된 것이 아니다)
let netState: 'ok' | 'down' | null = null
function setNet(state: 'ok' | 'down' | null) {
	netState = state
	netEl.hidden = state === null
	renderStatus()
	if (state === null) return
	netEl.dataset.tone = state === 'ok' ? 'ok' : 'bad'
	netEl.textContent = state === 'ok' ? '서버 연결됨' : '오프라인 · 연결되면 자동으로 보냅니다'
}

// ---- 단계 창 ----
// Tauri 창 API (app.withGlobalTauri). 브라우저(dev-server)와 이전 실행 파일에는 없으므로 없으면 CSS만 바꾼다
type PhysPoint = { x: number; y: number }
type PhysSize = { width: number; height: number }
type TauriMonitor = { scaleFactor: number; position: PhysPoint; size: PhysSize; workArea?: { position: PhysPoint; size: PhysSize } }
type TauriWindow = {
	setSize(size: unknown): Promise<void>
	setPosition(position: unknown): Promise<void>
	outerPosition(): Promise<PhysPoint>
	outerSize(): Promise<PhysSize>
	startDragging(): Promise<void>
	close(): Promise<void>
}
type TauriWindowApi = {
	getCurrentWindow(): TauriWindow
	currentMonitor(): Promise<TauriMonitor | null>
	LogicalSize: new (width: number, height: number) => unknown
	PhysicalPosition: new (x: number, y: number) => unknown
}

function windowApi(): TauriWindowApi | null {
	const w = (globalThis as { __TAURI__?: { window?: Partial<TauriWindowApi> } }).__TAURI__?.window
	return w?.getCurrentWindow && w.currentMonitor && w.LogicalSize && w.PhysicalPosition ? (w as TauriWindowApi) : null
}

function applyMode() {
	const root = document.documentElement
	root.dataset.mode = mode
	if (windowApi()) root.dataset.tauri = ''
}

/** 창이 마지막으로 맞춰진 단계 (null이면 아직 한 번도 맞추지 않았다: 처음에는 오른쪽 아래에 붙인다) */
let fittedStage: Stage | null = null
let windowQueue: Promise<void> = Promise.resolve()

function setStage(next: Stage) {
	stage = effectiveStage(mode, next)
	shell.dataset.stage = String(stage)
	if (stage === 1) {
		renderFeed()
		quickText.focus({ preventScroll: true })
	}
	// 단계를 빠르게 여러 번 바꿔도 창 조작은 하나씩, 마지막 단계에 맞춘다
	windowQueue = windowQueue.then(() => fitWindow(stage))
}

function go(event: StageEvent) {
	setStage(nextStage(stage, event, { ready: appReady }))
}

async function fitWindow(to: Stage) {
	const api = windowApi()
	if (mode !== 'overlay' || !api || fittedStage === to) return
	try {
		const win = api.getCurrentWindow()
		const monitor = await api.currentMonitor()
		// 끌어 옮긴 창이면 지금의 오른쪽 아래 모서리를 지키고, 처음이면 작업 영역 오른쪽 아래에 붙인다
		let anchor: { right: number; bottom: number } | null = null
		if (fittedStage !== null) {
			const [p, s] = await Promise.all([win.outerPosition(), win.outerSize()])
			anchor = { right: p.x + s.width, bottom: p.y + s.height }
		}
		const size = STAGE_SIZE[to]
		// 작업 영역(작업 표시줄을 뺀 곳)이 없으면 모니터 전체로
		const box = monitor ? (monitor.workArea ?? { position: monitor.position, size: monitor.size }) : null
		const area = box && { x: box.position.x, y: box.position.y, width: box.size.width, height: box.size.height }
		const pos = monitor && area ? placeWindow(size, area, monitor.scaleFactor, anchor) : null
		const move = () => (pos ? win.setPosition(new api.PhysicalPosition(pos.x, pos.y)) : Promise.resolve())
		const resize = () => win.setSize(new api.LogicalSize(size.width, size.height))
		// 커질 때는 먼저 옮기고, 작아질 때는 먼저 줄인다 (잠깐이라도 화면 밖으로 나가지 않게)
		if (fittedStage === null || growsTo(fittedStage, to)) {
			await move()
			await resize()
		} else {
			await resize()
			await move()
		}
		fittedStage = to
	} catch (e) {
		// 권한이 없거나(capabilities) 창 API가 다른 이전 실행 파일: 화면은 CSS만으로도 동작한다
		console.warn('창 크기를 바꾸지 못했습니다', e)
	}
}

// 누르기와 끌기: 5px 안에서 떼면 누르기, 넘게 움직이면 창을 끈다 (stage.ts classifyPointer).
// 끌기는 운영체제가 맡으므로 그 뒤의 click이 올 수도, 안 올 수도 있다. 온 click은 무시하고, 다음 pointerdown에서 다시 센다
function dragSurface(target: HTMLElement, opts: { onClick?: () => void } = {}) {
	let start: { x: number; y: number } | null = null
	let dragged = false
	target.addEventListener('pointerdown', (ev) => {
		dragged = false
		start = null
		if (ev.button !== 0 || mode !== 'overlay') return
		// 머리줄에서는 버튼 · 입력을 누른 것이면 끌지 않는다
		if (!opts.onClick && (ev.target as HTMLElement).closest('button, a, input, label, summary, textarea')) return
		start = { x: ev.clientX, y: ev.clientY }
	})
	target.addEventListener('pointermove', (ev) => {
		if (!start || classifyPointer(ev.clientX - start.x, ev.clientY - start.y) === 'click') return
		start = null
		dragged = true
		void windowApi()?.getCurrentWindow().startDragging().catch(() => {})
	})
	target.addEventListener('pointerup', () => (start = null))
	target.addEventListener('keydown', () => (dragged = false))
	if (opts.onClick) {
		target.addEventListener('click', () => {
			if (dragged) {
				dragged = false
				return
			}
			opts.onClick!()
		})
	}
}

dragSurface(pillBtn, { onClick: () => go('pill') })
for (const area of document.querySelectorAll<HTMLElement>('.drag-area')) dragSurface(area)
$('toDrawer').addEventListener('click', () => go('expand'))
$('collapse1').addEventListener('click', () => go('collapse'))
$('collapse2').addEventListener('click', () => go('collapse'))
$('closeApp').addEventListener('click', () => void windowApi()?.getCurrentWindow().close().catch(() => {}))
// 1단계에서 창 바깥을 누르면(창이 포커스를 잃으면) 알약으로 접는다. 빠른 입력에 쓰던 글이 있으면 그대로 둔다
window.addEventListener('blur', () => {
	if (mode === 'overlay' && stage === 1 && !quickText.value.trim() && windowApi()) go('collapse')
})

// ---- 답글 대상 ----
type ReplyTarget = { cc: string; ref?: string; localParent?: string; preview: string }
let reply: ReplyTarget | null = null

function setReply(t: ReplyTarget | null) {
	reply = t
	replyBox.hidden = t === null
	ccInput.readOnly = t !== null
	if (t) {
		showNotice(noticeEl, null)
		ccInput.value = t.cc
		replyLabel.textContent = `답글 대상: ${t.preview}`
		composeBox.open = true
		if (stage !== 2) setStage(2)
		textInput.focus()
		renderStatus()
	}
}

// ---- 내 평가 목록 ----
type Row = {
	request_id: string
	id: string
	cc: string
	state: string
	created_at: number
	data: { local_id: string; text: string; reason: string }
}

/** 마지막으로 읽은 내 평가 (최신순 50개). 미니 피드와 알약도 이것을 쓴다 */
let rowsCache: Row[] = []
/** 내 글(서버 id)의 전역 판정. cmd_verdicts가 10분 캐시를 두므로 자주 불러도 서버에 매번 묻지 않는다 */
const myVerdicts = new Map<string, MatchVerdict>()
let filter: FeedFilter = 'all'

async function loadRows(): Promise<Row[]> {
	// Dexie의 인덱스 정렬을 그대로 쓴다 (localdb.ts의 최소 인터페이스에는 없는 조회라서 직접 부른다)
	const table = (db as unknown as { table(n: string): { orderBy(i: string): { reverse(): { limit(n: number): { toArray(): Promise<Row[]> } } } } }).table('evaluations')
	return table.orderBy('created_at').reverse().limit(50).toArray()
}

async function loadVerdicts(rows: Row[]) {
	const ids = rows.filter((r) => r.state === 'stored' && r.id).map((r) => r.id)
	if (ids.length === 0) return
	try {
		const res = await commands.verdicts(ids)
		for (const v of res.verdicts) myVerdicts.set(v.id, v)
	} catch {
		/* 판정 명령을 모르는 이전 실행 파일 · 브리지이거나 서버에 못 물었다: 판정 없이 보여 준다 */
	}
}

function rowTier(r: Row) {
	return r.id ? effectiveTier(myVerdicts.get(r.id)) : 'unrated'
}

/** 현재 도메인만 보기를 켰으면 그 도메인 */
function ccFilter(): string | null {
	const cc = ccInput.value.trim()
	return onlyCc.checked && cc ? cc : null
}

async function renderList() {
	rowsCache = await loadRows()
	renderRows()
	renderFeed()
	renderStatus()
}

function renderRows() {
	const rows = rowsCache
	const shown = rows.filter((r) => matchesFilter({ state: r.state, tier: rowTier(r), cc: r.cc }, filter, ccFilter()))
	mineCount.textContent = rows.length ? (shown.length === rows.length ? `${rows.length}건` : `${shown.length} / ${rows.length}건`) : ''
	listEl.replaceChildren()
	if (rows.length === 0) {
		listEl.append(el('li', { class: 'empty', text: '아직 쓴 글이 없습니다. 위 "평가 쓰기"나 미니 피드의 빠른 입력으로 첫 평가를 등록하세요.' }))
		return
	}
	if (shown.length === 0) {
		listEl.append(el('li', { class: 'empty', text: '이 조건에 맞는 내 평가가 없습니다.' }))
		return
	}
	const now = Date.now()
	for (const r of shown) {
		const sv = stateView(r.state)
		const tier = rowTier(r)
		const meta = el('div', { class: 'meta' }, el('span', { class: 'ic', text: itemIcon(r.state, tier) }), el('span', { class: 'pill', tone: sv.tone, text: sv.label }))
		// 등록된 내 글에 판정이 났으면 함께 보여 준다 (판정 전이면 배지를 두지 않는다)
		if (r.state === 'stored' && tier !== 'unrated') {
			const tv = tierView(myVerdicts.get(r.id))
			meta.append(el('span', { class: 'pill', tone: tv.tone, text: tv.label }))
		}
		meta.append(el('span', { text: r.cc }), el('span', { class: 'num', text: formatAgo(r.created_at, now) }))
		const li = el('li', { class: 'row-item' }, meta, el('p', { class: 'body', text: previewText(r.data.text, 140) }))
		li.dataset.state = r.state
		if ((r.state === 'rejected' || r.state === 'failed') && r.data.reason) {
			li.append(el('p', { class: 'why', text: `사유: ${reasonLabel(r.data.reason)}` }))
		}
		if (r.state === 'missing') li.append(el('p', { class: 'hint', text: '서버에서 결과를 찾을 수 없습니다. 거부·실패 기록은 7일 뒤 서버에서 지워집니다.' }))
		if (r.state === 'rejected') li.append(el('p', { class: 'hint', text: '원문과 벡터는 서버에서 지워졌습니다. 이 기기에는 남아 있습니다.' }))
		// 답글은 서버가 받은 글(id가 있음)이나 아직 보내지 못한 글(로컬 id)에 달 수 있다
		const canReply = r.state === 'stored' || r.state === 'pending' || r.state === 'queued'
		if (canReply && (r.id || r.data.local_id)) {
			const btn = el('button', { class: 'link', type: 'button', text: '답글' })
			btn.addEventListener('click', () => setReply({ cc: r.cc, ref: r.id || undefined, localParent: r.id ? undefined : r.data.local_id, preview: previewText(r.data.text, 40) }))
			li.append(btn)
		}
		listEl.append(li)
	}
}

// ---- 1단계: 미니 피드 ----
function renderFeed() {
	const rows = feedRows(rowsCache)
	feedEl.replaceChildren()
	if (rows.length === 0) {
		feedEl.append(el('li', { class: 'empty', text: appReady ? '아직 쓴 글이 없습니다. 아래에 첫 메모를 남기세요.' : '앱이 아직 시작하지 못했습니다.' }))
		return
	}
	rows.forEach((r, i) => {
		const fromBottom = rows.length - 1 - i
		const tier = rowTier(r)
		const line = feedLine({ cc: r.cc, state: r.state, text: r.data.text }, tier)
		const tone = r.state === 'stored' && tier === 'premium' ? 'ok' : r.state === 'stored' && tier === 'discount' ? 'bad' : stateView(r.state).tone
		const li = el('li', { class: 'feed-row', tone }, el('span', { class: 'tag' }, el('span', { class: 'ic', text: line.icon }), line.tag), el('span', { class: 'text', text: line.preview }))
		li.style.opacity = String(feedOpacity(fromBottom))
		li.title = previewText(r.data.text, 300)
		// 맨 위 줄(세 번째)은 흐려지며 사라지고, 누르면 서랍(2단계)으로 간다
		if (fromBottom === 2) {
			li.classList.add('feed-top')
			li.tabIndex = 0
			li.setAttribute('role', 'button')
			li.setAttribute('aria-label', `전체 보기: ${line.tag}`)
			li.addEventListener('click', () => go('feed-top'))
			li.addEventListener('keydown', (ev) => (ev.key === 'Enter' || ev.key === ' ') && (ev.preventDefault(), go('feed-top')))
		}
		feedEl.append(li)
	})
}

// ---- 알약 · 미니 피드 머리줄의 상태 ----
function renderStatus() {
	const dot = syncDot(rowsCache, netState, appReady)
	for (const id of ['pillDot', 'miniDot']) $(id).dataset.tone = dot.tone
	for (const id of ['pillSync', 'miniSync']) $(id).textContent = dot.label
	const latest = feedRows(rowsCache, 1)[0]
	$('pillLine').textContent = appReady
		? pillLine(lastNotice, latest ? { cc: latest.cc, state: latest.state, text: latest.data.text, created_at: latest.created_at } : null, Date.now())
		: '눌러서 시작 안내 보기'
	const cc = ccInput.value.trim()
	$('miniCc').textContent = cc ? `cc: ${cc}` : ''
	quickText.placeholder = quickPlaceholder(cc)
	ccChipLabel.textContent = cc ? `현재 도메인만 (${cc})` : '현재 도메인만'
}

// ---- 동기화 ----
// 시작할 때의 확인, "지금 확인", 10초 확인, 등록 직후의 목록 갱신이 겹칠 수 있다. 진행 중이면 끝난 뒤 한 번 더 한다 (coalesce.ts)
const refresh = coalesced(async (opts) => {
	try {
		if (opts.flush) {
			try {
				const s = await commands.flushOutbox()
				if (s.stopped || s.sync_stopped) setNet('down')
				else if (s.sent > 0 || s.stored > 0 || s.still_pending > 0) setNet('ok')
			} catch {
				setNet('down')
			}
		}
		await syncCacheFromRust(db, invoke)
		await loadVerdicts(await loadRows())
		await renderList()
	} catch (e) {
		showNotice(noticeEl, describeError(toCommandFailure(e)))
	}
})

async function hasWork(): Promise<boolean> {
	const rows = await loadRows()
	return rows.some((r) => r.state === 'queued' || r.state === 'pending')
}

// ---- 쓰기 ----
function updateBytes() {
	const c = byteCounter(textInput.value)
	bytesEl.textContent = c.label
	bytesEl.style.color = c.over ? 'var(--bad)' : ''
}
textInput.addEventListener('input', () => {
	updateBytes()
	// 새로 쓰기 시작하면 이전 등록 결과 안내는 지운다 (오래된 안내가 남아 있으면 지금 상태로 오해한다)
	showNotice(noticeEl, null)
})
ccInput.addEventListener('input', () => {
	renderStatus()
	if (onlyCc.checked) renderAll()
})

/** 등록 결과 공통 처리: 연결 표시와 마지막 도메인 기억 */
function afterSubmit(result: SubmitResult, cc: string) {
	if (result.status === 'queued') setNet('down')
	else if (result.status !== 'blocked') setNet('ok')
	try {
		localStorage.setItem(LAST_CC_KEY, cc.trim())
	} catch {
		/* 저장소를 못 써도 화면은 동작한다 */
	}
}

form.addEventListener('submit', async (ev) => {
	ev.preventDefault()
	submitBtn.disabled = true
	submitBtn.textContent = '등록 중…'
	showNotice(noticeEl, null)
	try {
		const result = await commands.submitEvaluation({
			text: textInput.value,
			cc: ccInput.value,
			...(reply?.ref ? { ref: reply.ref } : {}),
			...(reply?.localParent ? { local_parent: reply.localParent } : {}),
		})
		const notice = describeSubmit(result)
		showNotice(noticeEl, notice)
		afterSubmit(result, ccInput.value)
		if (!notice.keepText) {
			textInput.value = ''
			updateBytes()
			setReply(null)
		}
		await refresh({ flush: false })
	} catch (e) {
		showNotice(noticeEl, describeError(toCommandFailure(e)))
	} finally {
		submitBtn.disabled = false
		submitBtn.textContent = '등록'
	}
})
$('clearReply').addEventListener('click', () => setReply(null))
$('recheck').addEventListener('click', () => void refresh({ flush: true }))

// 1단계 빠른 입력: 현재 도메인(cc)에 같은 등록 명령으로 남긴다. 도메인이 없으면 서랍의 평가 쓰기로 보낸다
quickText.addEventListener('input', () => (quickNotice.hidden = true))
quickForm.addEventListener('submit', async (ev) => {
	ev.preventDefault()
	const cc = ccInput.value.trim()
	if (!cc) {
		composeBox.open = true
		setStage(2)
		ccInput.focus()
		showNotice(noticeEl, { tone: 'warn', title: '도메인을 먼저 정하세요', detail: '평가 쓰기의 도메인 칸에 적으면 빠른 입력이 그 도메인으로 남깁니다.', keepText: true })
		return
	}
	if (!quickText.value.trim()) return
	quickSubmit.disabled = true
	showQuick({ tone: 'busy', title: '등록 중…', keepText: true })
	try {
		const result = await commands.submitEvaluation({ text: quickText.value, cc })
		const notice = describeSubmit(result)
		showQuick(notice)
		afterSubmit(result, cc)
		if (!notice.keepText) quickText.value = ''
		await refresh({ flush: false })
	} catch (e) {
		showQuick(describeError(toCommandFailure(e)))
	} finally {
		quickSubmit.disabled = false
	}
})

// ---- 걸러 보기 ----
function renderChips() {
	chipsEl.replaceChildren(
		...FILTER_CHIPS.map((c) => {
			const b = el('button', { class: 'chip', type: 'button', text: c.label })
			b.setAttribute('aria-pressed', String(filter === c.id))
			b.addEventListener('click', () => {
				filter = c.id
				renderChips()
				renderAll()
			})
			return b
		}),
	)
}

function renderAll() {
	renderRows()
	renderResults()
}

onlyCc.addEventListener('change', renderAll)

// ---- 검색 ----
/** 마지막 검색 결과 (없으면 검색하지 않은 상태). 걸러 보기를 바꾸면 이것을 다시 그린다 */
let lastMatches: MergedMatch[] | null = null

function matchTier(m: MergedMatch) {
	// 서버에 있는 글만 판정이 있다 (내 글이 아직 로컬에만 있으면 판정 전)
	return m.source === 'local' ? 'unrated' : effectiveTier(m.verdict)
}

function renderResults() {
	resultsBox.hidden = lastMatches === null
	resultsEl.replaceChildren()
	if (!lastMatches) return
	const shown = lastMatches.filter((m) => matchesFilter({ state: m.state, tier: matchTier(m), cc: m.cc }, filter, ccFilter()))
	resultsCount.textContent = lastMatches.length ? (shown.length === lastMatches.length ? `${shown.length}건` : `${shown.length} / ${lastMatches.length}건`) : ''
	if (lastMatches.length === 0) resultsEl.append(el('li', { class: 'empty', text: '비슷한 평가를 찾지 못했습니다.' }))
	else if (shown.length === 0) resultsEl.append(el('li', { class: 'empty', text: '이 조건에 맞는 결과가 없습니다.' }))
	for (const m of shown) resultsEl.append(renderMatch(m))
}

function renderMatch(m: MergedMatch): HTMLElement {
	const src = sourceView(m)
	const li = el('li', { class: src.mine ? 'mine' : '' })
	const meta = el('div', { class: 'meta' }, el('span', { class: 'ic', text: itemIcon(m.state, matchTier(m)) }), el('span', { class: 'pill', tone: src.mine ? 'ok' : undefined, text: src.label }))
	if (m.state) {
		const sv = stateView(m.state)
		meta.append(el('span', { class: 'pill', tone: sv.tone, text: sv.label }))
	}
	// 서버에 있는 글만 판정이 있다 (내 글이 아직 로컬에만 있으면 배지를 두지 않는다)
	if (m.source !== 'local') {
		const tv = tierView(m.verdict)
		meta.append(el('span', { class: 'pill', tone: tv.tone, text: tv.label }))
	}
	if (m.cc) meta.append(el('span', { text: m.cc }))
	meta.append(el('span', { class: 'score num', text: `유사도 ${scorePercent(m.score)}` }))
	li.append(meta)
	if (m.text) li.append(el('p', { class: 'body', text: previewText(m.text, 160) }))
	else li.append(el('p', { class: 'hint', text: '원문은 서버에 보관하지 않아 보여 줄 수 없습니다.' }))
	if (canVote(m)) li.append(renderVote(m))
	return li
}

// 추천 / 비추천 버튼. 같은 버튼을 다시 누르면 거둔다. 오프라인이면 이 기기에 저장되고 나중에 보내진다
function renderVote(m: MergedMatch): HTMLElement {
	const box = el('div', { class: 'vote' })
	const up = el('button', { type: 'button', text: '▲ 추천' })
	const down = el('button', { type: 'button', text: '▼ 비추천' })
	const paint = () => {
		up.setAttribute('aria-pressed', String(m.my_vote === 1))
		down.setAttribute('aria-pressed', String(m.my_vote === -1))
	}
	const press = async (pressed: 1 | -1) => {
		const value = nextVote(m.my_vote, pressed)
		up.disabled = down.disabled = true
		try {
			const r = await commands.vote({ id: m.id!, value })
			m.my_vote = value === 0 ? null : value
			paint()
			showNotice(searchNotice, describeVote(r, value))
		} catch (e) {
			showNotice(searchNotice, describeError(toCommandFailure(e)))
		} finally {
			up.disabled = down.disabled = false
		}
	}
	up.addEventListener('click', () => void press(1))
	down.addEventListener('click', () => void press(-1))
	paint()
	box.append(up, down)
	return box
}

searchForm.addEventListener('submit', async (ev) => {
	ev.preventDefault()
	showNotice(searchNotice, null)
	lastMatches = []
	renderResults()
	resultsCount.textContent = '찾는 중…'
	try {
		const cc = ccFilter()
		const res = await commands.searchMerged({ query: queryInput.value, top_k: 8, ...(cc ? { cc } : {}) })
		if (res.remote_error) {
			setNet('down')
			showNotice(searchNotice, { tone: 'warn', title: '서버에 연결할 수 없어 내 글만 보여 줍니다', keepText: true })
		} else setNet('ok')
		lastMatches = res.matches
		renderResults()
	} catch (e) {
		resultsCount.textContent = ''
		showNotice(searchNotice, describeError(toCommandFailure(e)))
	}
})
$('clearSearch').addEventListener('click', () => {
	lastMatches = null
	queryInput.value = ''
	showNotice(searchNotice, null)
	renderResults()
})

// ---- 계정과 QR 로그인 ----
function renderAccount(info: { state: 'anonymous' | 'signed_in'; address: string | null; warning: string | null }) {
	const v = accountView(info)
	document.querySelector<HTMLElement>('.acct')!.hidden = false // 처음 상태를 알기 전에는 "게스트"를 보여 주지 않는다
	acctLabel.textContent = v.label
	acctLabel.title = info.address ?? ''
	loginBtn.hidden = v.signedIn
	logoutBtn.hidden = !v.signedIn
	authorEl.textContent = v.author
	accountWarn.hidden = !info.warning
	accountWarn.textContent = info.warning ?? ''
	// 지갑: 앱에는 지갑 명령이 없어 세션의 계정 주소만 보여 준다 (stage.ts walletLine)
	const w = walletLine(info)
	walletAddr.hidden = !w.address
	walletAddr.textContent = w.address
	walletNote.textContent = w.note
}

async function refreshAccount(verify: boolean) {
	try {
		renderAccount(await commands.session(verify))
	} catch {
		// 상태를 못 읽으면 게스트로 보여 준다 (처음 그릴 때만. 이미 그려진 상태는 그대로 둔다)
		if (document.querySelector<HTMLElement>('.acct')!.hidden) renderAccount({ state: 'anonymous', address: null, warning: null })
	}
}

let loginSeq = 0 // 닫거나 다시 시작하면 올라간다. 늦게 도착한 응답이 새 상태를 덮지 않게 한다
let pollTimer: number | undefined
let tickTimer: number | undefined
let remainingSec = 0
let loginErrors = 0

function stopLoginTimers() {
	window.clearInterval(pollTimer)
	window.clearInterval(tickTimer)
	pollTimer = tickTimer = undefined
}

function showLoginPhase(phase: LoginPhase, opts: { address?: string | null; error?: string } = {}) {
	showNotice(loginStatusEl, describeLoginPhase(phase, { remainingSec, ...opts }))
	loginRetry.hidden = !(phase === 'expired' || phase === 'error')
}

async function pollLogin(seq: number) {
	try {
		const st = await commands.loginStatus()
		if (seq !== loginSeq) return
		loginErrors = 0
		if (st.state === 'signed_in') {
			stopLoginTimers()
			showLoginPhase('done', { address: st.address })
			await refreshAccount(false)
			void refresh({ flush: true }) // 못 보낸 글은 이제 로그인한 계정으로 나간다
			window.setTimeout(() => seq === loginSeq && closeLogin(), 1200)
		} else if (st.state === 'pending') {
			remainingSec = st.remaining_sec ?? remainingSec
			showLoginPhase('waiting')
		} else {
			// expired(시간이 지났거나 서버가 세션을 모름) 또는 none
			stopLoginTimers()
			showLoginPhase('expired')
		}
	} catch (e) {
		if (seq !== loginSeq) return
		// 연결이 잠깐 끊기는 것은 흔하다. 여러 번 연달아 실패하면 멈추고 다시 시작하게 한다
		if (++loginErrors >= LOGIN_MAX_ERRORS) {
			stopLoginTimers()
			showLoginPhase('error', { error: describeLoginError(toCommandFailure(e)) })
		}
	}
}

async function beginLogin() {
	const seq = ++loginSeq
	stopLoginTimers()
	loginErrors = 0
	qrBox.replaceChildren(el('span', { class: 'muted', text: 'QR을 만드는 중…' }))
	mailAddr.textContent = ''
	copyAddr.disabled = true
	showLoginPhase('starting')
	try {
		const started = await commands.loginStart()
		if (seq !== loginSeq) return
		// 모바일은 이 기기의 메일 앱으로 바로 간다. 데스크톱은 폰으로 QR을 스캔한다 (링크도 보조로 있다)
		mailLink.href = mailtoLink(started.address)
		mailLink.hidden = false
		if (loginMode(platform) === 'qr') {
			renderQr(qrBox, started.mailto, '로그인 QR. 휴대폰 카메라로 스캔해 메일을 보내세요')
			qrBox.dataset.mailto = started.mailto
		}
		mailAddr.textContent = started.address
		copyAddr.disabled = false
		remainingSec = started.expires_in_sec
		showLoginPhase('waiting')
		pollTimer = window.setInterval(() => void pollLogin(seq), LOGIN_POLL_MS)
		tickTimer = window.setInterval(() => {
			remainingSec = Math.max(0, remainingSec - 1)
			if (seq === loginSeq && !loginStatusEl.hidden && loginStatusEl.dataset.tone === 'busy' && pollTimer !== undefined) showLoginPhase('waiting')
		}, 1000)
	} catch (e) {
		if (seq !== loginSeq) return
		qrBox.replaceChildren(el('span', { class: 'muted', text: 'QR을 만들지 못했습니다' }))
		showLoginPhase('error', { error: describeLoginError(toCommandFailure(e)) })
	}
}

function applyLoginMode() {
	const mode = loginMode(platform)
	loginCard.dataset.mode = mode
	mailLink.classList.toggle('primary', mode === 'link')
	mailLink.textContent = mode === 'link' ? '메일 앱으로 로그인' : '이 기기의 메일 앱으로 열기'
	loginIntro.textContent =
		mode === 'link'
			? '비밀번호 없이 로그인합니다. 메일을 한 통 보내면 이 앱이 로그인됩니다.'
			: '비밀번호 없이 로그인합니다. 휴대폰으로 QR을 스캔해 메일을 한 통 보내면 이 앱이 로그인됩니다.'
}

function openLogin() {
	applyLoginMode()
	mailLink.hidden = true
	loginModal.hidden = false
	loginRetry.focus()
	void beginLogin()
}

function closeLogin() {
	loginSeq++
	stopLoginTimers()
	loginModal.hidden = true
	loginBtn.focus()
}

async function copyAddress() {
	const address = mailAddr.textContent ?? ''
	if (!address) return
	try {
		await navigator.clipboard.writeText(address)
		copyAddr.textContent = '복사했습니다'
	} catch {
		// 클립보드를 쓸 수 없는 환경: 주소를 선택해 두면 직접 복사할 수 있다
		const range = document.createRange()
		range.selectNodeContents(mailAddr)
		const sel = window.getSelection()
		sel?.removeAllRanges()
		sel?.addRange(range)
		copyAddr.textContent = '선택했습니다. 복사하세요'
	}
	window.setTimeout(() => (copyAddr.textContent = '주소 복사'), 2000)
}

loginBtn.addEventListener('click', openLogin)
mailLink.addEventListener('click', async (ev) => {
	ev.preventDefault() // 메일 앱은 opener(Tauri) 또는 브라우저가 연다
	try {
		await openExternal(mailLink.href)
	} catch {
		showLoginPhase('error', { error: '메일 앱을 열지 못했습니다. 아래 주소로 직접 메일을 보내세요.' })
	}
})
// 메일 앱에서 돌아오면 기다리지 않고 바로 확인한다 (모바일에서는 앱이 백그라운드에 있는 동안 타이머가 멈출 수 있다)
document.addEventListener('visibilitychange', () => {
	if (document.visibilityState === 'visible' && !loginModal.hidden && pollTimer !== undefined) void pollLogin(loginSeq)
})
$('loginClose').addEventListener('click', closeLogin)
loginRetry.addEventListener('click', () => void beginLogin())
copyAddr.addEventListener('click', () => void copyAddress())
loginModal.addEventListener('click', (ev) => ev.target === loginModal && closeLogin()) // 바깥을 누르면 닫는다
// Esc: 로그인 창이 열려 있으면 그것만 닫고, 아니면 한 단계 접는다 (2 → 1 → 0)
window.addEventListener('keydown', (ev) => {
	if (ev.key !== 'Escape') return
	if (!loginModal.hidden) return closeLogin()
	if (mode === 'overlay') go('escape')
})
logoutBtn.addEventListener('click', async () => {
	try {
		renderAccount(await commands.logout())
	} catch (e) {
		showNotice(noticeEl, describeError(toCommandFailure(e)))
	}
})

// ---- 시작 ----
function showStartupFailure(status: { error: string | null; model_dir: string; data_dir: string; server_url: string; version?: string }) {
	const d = describeStartup(status)
	appReady = false
	$('layout').hidden = true
	$('setup').hidden = false
	$('setupTitle').textContent = d.title
	$('setupError').textContent = d.error
	$('setupError').dataset.tone = d.tone
	// 모델만 없으면 내려받기가 주된 동작이다
	$('setupRetry').classList.toggle('primary', d.tone !== 'info')
	$('setupHints').replaceChildren(...d.hints.map((h) => el('li', { text: h })))
	$('setupVersion').textContent = status.version ? `버전 ${status.version}` : ''
	$('acct')?.setAttribute('hidden', '')
	// 시작 안내는 서랍(2단계) 크기로 보여 준다. 접으면 알약이 "설정 필요"를 보여 주고, 누르면 바로 여기로 온다
	renderList().catch(() => renderStatus())
	setStage(2)
	if (isModelMissing(status.error)) void showModelFetch()
}
$('setupRetry').addEventListener('click', () => location.reload())

// ---- 모델 내려받기: 모델이 없을 때 시작 안내 화면에서 ----
let downloadTimer: ReturnType<typeof setTimeout> | undefined

async function showModelFetch() {
	let status
	try {
		status = await commands.modelStatus()
	} catch {
		return // 모델 명령을 모르는 이전 실행 파일
	}
	$('modelFetch').hidden = false
	$('dlChoices').replaceChildren(
		...modelChoices(status).map((c) => {
			const b = el('button', { type: 'button', class: c.recommended ? 'primary' : '' }, el('strong', { text: c.recommended ? `${c.label} (권장)` : c.label }), el('span', { text: c.detail }))
			b.dataset.variant = c.variant
			b.addEventListener('click', () => void startDownload(c.variant))
			return b
		}),
	)
	// 이전 화면에서 끝낸 설치인데 여전히 시작하지 못했다면 '설치를 마쳤습니다'를 다시 보이지 않는다
	renderDownload(status.download.state === 'done' ? { ...status.download, state: 'idle' } : status.download)
	if (status.download.state === 'running') pollDownload()
}

function renderDownload(p: FetchProgress) {
	const d = describeDownload(p)
	const bar = $<HTMLProgressElement>('dlProgress')
	bar.hidden = d.percent === null
	if (d.percent !== null) bar.value = d.percent
	const statusEl = $('dlStatus')
	statusEl.textContent = d.text
	statusEl.dataset.tone = d.tone
	$('dlCancel').hidden = !d.busy
	for (const b of $('dlChoices').querySelectorAll('button')) b.disabled = d.busy || p.state === 'done'
}

async function startDownload(variant: ModelVariant) {
	try {
		renderDownload(await commands.modelDownload(variant))
		pollDownload()
	} catch (e) {
		const statusEl = $('dlStatus')
		statusEl.textContent = toCommandFailure(e).message
		statusEl.dataset.tone = 'bad'
	}
}

function pollDownload() {
	clearTimeout(downloadTimer)
	downloadTimer = setTimeout(async () => {
		try {
			const s = await commands.modelStatus()
			renderDownload(s.download)
			if (s.download.state === 'running') pollDownload()
			// 설치가 끝나면 다시 시작한다: 셸이 새 모델로 코어를 만든다
			else if (s.download.state === 'done') setTimeout(() => location.reload(), 600)
		} catch {
			pollDownload()
		}
	}, 1000)
}

$('dlCancel').addEventListener('click', async () => {
	try {
		renderDownload(await commands.modelDownloadCancel())
	} catch {
		/* 다음 폴링이 상태를 보여 준다 */
	}
})

async function boot() {
	applyMode()
	shell.dataset.stage = String(stage)
	renderChips()
	// 앱(Rust 코어)이 준비되지 않았으면 다른 명령을 부르지 않고 원인을 보여 준다
	try {
		const status = await commands.appStatus()
		if (!params.get('platform')) {
			platform = normalizePlatform(status.platform)
			mode = layoutMode(platform)
			applyMode()
		}
		if (!status.ready) return showStartupFailure(status)
	} catch (e) {
		const f = toCommandFailure(e)
		// 코어가 없는 상태에서는 모든 명령이 not_ready 오류를 돌려준다
		if (f.kind === 'not_ready') return showStartupFailure({ error: f.message, model_dir: '', data_dir: '', server_url: '' })
		// 이 상태 명령을 모르는 이전 실행 파일이면 그냥 이어간다
	}
	// 첫 단계로 창을 맞춘다 (데스크톱: 작업 영역 오른쪽 아래의 알약, 모바일: 전체 화면 서랍)
	setStage(stage)
	try {
		ccInput.value = localStorage.getItem(LAST_CC_KEY) ?? ''
	} catch {
		/* 무시 */
	}
	updateBytes()
	renderStatus()
	// 계정은 이 기기에 저장된 상태로 먼저 즉시 그리고, 서버에 아직 유효한지 확인하는 것은 뒤에서 한다
	void refreshAccount(false).then(() => refreshAccount(true))
	void refresh({ flush: true })
	setInterval(() => {
		void hasWork().then((work) => {
			if (work) void refresh({ flush: true })
		})
	}, RECHECK_MS)
	// "방금 · n분 전"과 알림 1분을 알약에 맞춘다
	setInterval(renderStatus, STATUS_TICK_MS)
	window.addEventListener('online', () => void refresh({ flush: true }))
	void refreshModelLine()
	void refreshFilter()
	void refreshDraft()
}

// ---- 모델 한 줄 (서랍 아래 접이식) ----
async function refreshModelLine() {
	try {
		$('modelLine').textContent = modelSummary(await commands.modelStatus())
	} catch {
		$('modelLine').textContent = '모델 상태를 읽지 못했습니다 (이전 실행 파일).'
	}
}

// ---- items 초안 (Qwen3.5-0.8B, 이 기기) ----
let draftTimer: ReturnType<typeof setTimeout> | undefined
async function refreshDraft() {
	let status
	try {
		status = await commands.modelStatus()
	} catch {
		return
	}
	if (!status.draft) return
	const d = describeDraftModel(status.draft)
	$('draftCard').hidden = false
	$('draftModelText').textContent = d.text
	const get = $<HTMLButtonElement>('draftGet')
	get.hidden = d.action === null
	get.textContent = d.action ?? ''
	$('draftForm').hidden = !d.ready
	clearTimeout(draftTimer)
	if (d.busy) draftTimer = setTimeout(() => void refreshDraft(), 1000)
}
$('draftGet').addEventListener('click', async () => {
	try {
		await commands.modelDownload('draft')
	} catch (e) {
		$('draftModelText').textContent = toCommandFailure(e).message
	}
	void refreshDraft()
})
$('draftForm').addEventListener('submit', async (ev) => {
	ev.preventDefault()
	const btn = $<HTMLButtonElement>('draftBtn')
	const out = $('draftOut')
	btn.disabled = true
	$('draftStatus').textContent = '만드는 중… (처음에는 모델을 올리느라 더 걸립니다)'
	try {
		const draft = await commands.itemDraft({ text: $<HTMLTextAreaElement>('pageText').value, link: $<HTMLInputElement>('pageLink').value })
		out.textContent = formatDraft(draft)
		out.hidden = false
		out.dataset.type = String(draft.type)
		$('draftStatus').textContent = ''
	} catch (e) {
		$('draftStatus').textContent = toCommandFailure(e).message
	} finally {
		btn.disabled = false
	}
})

// ---- 욕설 필터 모델 (laya-multilingual): 상태 표시와 내려받기 ----
let filterTimer: ReturnType<typeof setTimeout> | undefined
async function refreshFilter() {
	let status
	try {
		status = await commands.modelStatus()
	} catch {
		return // 모델 명령을 모르는 이전 실행 파일
	}
	if (!status.filter) return
	const d = describeFilter(status.filter)
	$('filterInfo').hidden = false
	$('filterText').textContent = d.text
	const get = $<HTMLButtonElement>('filterGet')
	get.hidden = d.action === null
	get.textContent = d.action ?? ''
	clearTimeout(filterTimer)
	// 받는 중이거나 다 받고 바꾸는 중이면 1초마다 다시 본다
	if (d.busy || (status.filter.installed && status.filter.active !== 'laya' && status.filter.download.state === 'done')) {
		filterTimer = setTimeout(() => void refreshFilter(), 1000)
	}
}
$('filterGet').addEventListener('click', async () => {
	try {
		await commands.modelDownload('filter')
	} catch (e) {
		$('filterText').textContent = toCommandFailure(e).message
	}
	void refreshFilter()
})
void boot()
