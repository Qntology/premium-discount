// 앱 웹뷰 화면: 평가 쓰기, 내 평가 상태 목록, 검색.
//
// 화면은 평가를 직접 만들거나 고치지 않는다 (localdb.ts의 규칙). 쓰기는 항상 Rust 명령을 거치고,
// 목록은 Rust가 내보낸 행을 Dexie 캐시에 반영(syncCacheFromRust)한 뒤 거기서 읽는다.
//
// 흐름
//   등록: cmd_submit_evaluation → (결과 안내) → 캐시 동기화 → 목록 갱신
//   확인: cmd_flush_outbox(못 보낸 글 전송, 접수된 글의 최종 결과 확인) → 캐시 동기화 → 목록 갱신
//         시작할 때, "지금 확인"을 누를 때, 연결이 돌아왔을 때, 보낼 것이 남아 있으면 10초마다
//   검색: cmd_search_merged (내 글 + 서버 결과를 한 번의 임베딩으로)
//   로그인: cmd_login_start(QR와 메일 주소) → 3초마다 cmd_login_status → 메일이 도착하면 로그인 (세션은 Rust가 저장한다)

import Dexie from 'dexie'

import { coalesced } from './coalesce.ts'
import { makeCommands, syncCacheFromRust, toCommandFailure, type FetchProgress, type MergedMatch, type ModelVariant } from './commands.ts'
import { openLocalDb, type DexieLike } from './localdb.ts'
import { openExternal, pickInvoke } from './transport.ts'
import { renderQr } from './qr.ts'
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

// ---- DOM ----
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T
const form = $<HTMLFormElement>('form')
const ccInput = $<HTMLInputElement>('cc')
const textInput = $<HTMLTextAreaElement>('text')
const bytesEl = $('bytes')
const submitBtn = $<HTMLButtonElement>('submit')
const noticeEl = $('notice')
const replyBox = $('replyTo')
const replyLabel = $('replyLabel')
const listEl = $('list')
const mineCount = $('mineCount')
const netEl = $('net')
const searchForm = $<HTMLFormElement>('searchForm')
const queryInput = $<HTMLInputElement>('q')
const onlyCc = $<HTMLInputElement>('onlyCc')
const searchNotice = $('searchNotice')
const resultsEl = $('results')
const acctLabel = $('acctLabel')
const loginBtn = $<HTMLButtonElement>('loginBtn')
const logoutBtn = $<HTMLButtonElement>('logoutBtn')
const accountWarn = $('accountWarn')
const authorEl = $('author')
const loginModal = $('loginModal')
const qrBox = $('qrBox')
const mailAddr = $('mailAddr')
const copyAddr = $<HTMLButtonElement>('copyAddr')
const loginStatusEl = $('loginStatus')
const loginRetry = $<HTMLButtonElement>('loginRetry')
const mailLink = $<HTMLAnchorElement>('mailLink')
const loginIntro = $('loginIntro')
const loginCard = loginModal.querySelector<HTMLElement>('.modal-card')!

// 앱이 실행 중인 플랫폼. 앱(Rust)이 알려 주고(cmd_app_status), 모르면 데스크톱으로 본다.
// 개발용 브라우저에서 모바일 화면을 미리 볼 수 있도록 ?platform=android 를 허용한다 (레이아웃만 바뀐다)
let platform: Platform = normalizePlatform(new URLSearchParams(location.search).get('platform'))

function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: { class?: string; text?: string; tone?: string; type?: string } = {}, ...children: (Node | string)[]): HTMLElementTagNameMap[K] {
	const node = document.createElement(tag)
	if (props.class) node.className = props.class
	if (props.text !== undefined) node.textContent = props.text
	if (props.tone) node.dataset.tone = props.tone
	if (props.type && node instanceof HTMLButtonElement) node.type = props.type as 'button'
	for (const c of children) node.append(c)
	return node
}

// 사용자가 쓴 글과 서버가 준 값은 모두 textContent로만 넣는다 (HTML로 해석하지 않는다)
function showNotice(target: HTMLElement, n: Notice | null) {
	target.replaceChildren()
	target.hidden = n === null
	if (!n) return
	target.dataset.tone = n.tone
	target.append(el('strong', { text: n.title }))
	if (n.detail) target.append(el('span', { text: n.detail }))
}

// ---- 온라인 표시 ----
// 서버와 실제로 주고받은 결과가 있을 때만 보여 준다 (보낼 것이 없을 때의 "연결됨"은 확인된 것이 아니다)
function setNet(state: 'ok' | 'down' | null) {
	netEl.hidden = state === null
	if (state === null) return
	netEl.dataset.tone = state === 'ok' ? 'ok' : 'bad'
	netEl.textContent = state === 'ok' ? '서버 연결됨' : '오프라인 · 연결되면 자동으로 보냅니다'
}

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
		textInput.focus()
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

async function loadRows(): Promise<Row[]> {
	// Dexie의 인덱스 정렬을 그대로 쓴다 (localdb.ts의 최소 인터페이스에는 없는 조회라서 직접 부른다)
	const table = (db as unknown as { table(n: string): { orderBy(i: string): { reverse(): { limit(n: number): { toArray(): Promise<Row[]> } } } } }).table('evaluations')
	return table.orderBy('created_at').reverse().limit(50).toArray()
}

async function renderList() {
	const rows = await loadRows()
	mineCount.textContent = rows.length ? `${rows.length}건` : ''
	listEl.replaceChildren()
	if (rows.length === 0) {
		listEl.append(el('li', { class: 'empty', text: '아직 쓴 글이 없습니다. 위 폼에서 첫 평가를 등록하세요.' }))
		return
	}
	const now = Date.now()
	for (const r of rows) {
		const sv = stateView(r.state)
		const meta = el('div', { class: 'meta' }, el('span', { class: 'pill', tone: sv.tone, text: sv.label }), el('span', { text: r.cc }), el('span', { class: 'num', text: formatAgo(r.created_at, now) }))
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
		if (result.status === 'queued') setNet('down')
		else if (result.status !== 'blocked') setNet('ok')
		try {
			localStorage.setItem(LAST_CC_KEY, ccInput.value.trim())
		} catch {
			/* 저장소를 못 써도 화면은 동작한다 */
		}
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

// ---- 검색 ----
function renderMatch(m: MergedMatch): HTMLElement {
	const src = sourceView(m)
	const li = el('li', { class: src.mine ? 'mine' : '' })
	const meta = el('div', { class: 'meta' }, el('span', { class: 'pill', tone: src.mine ? 'ok' : undefined, text: src.label }))
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
	resultsEl.replaceChildren()
	try {
		const res = await commands.searchMerged({ query: queryInput.value, top_k: 8, ...(onlyCc.checked && ccInput.value.trim() ? { cc: ccInput.value } : {}) })
		if (res.remote_error) {
			setNet('down')
			showNotice(searchNotice, { tone: 'warn', title: '서버에 연결할 수 없어 내 글만 보여 줍니다', keepText: true })
		} else setNet('ok')
		if (res.matches.length === 0) resultsEl.append(el('li', { class: 'empty', text: '비슷한 평가를 찾지 못했습니다.' }))
		for (const m of res.matches) resultsEl.append(renderMatch(m))
	} catch (e) {
		showNotice(searchNotice, describeError(toCommandFailure(e)))
	}
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
window.addEventListener('keydown', (ev) => ev.key === 'Escape' && !loginModal.hidden && closeLogin())
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
	// 앱(Rust 코어)이 준비되지 않았으면 다른 명령을 부르지 않고 원인을 보여 준다
	try {
		const status = await commands.appStatus()
		if (!status.ready) return showStartupFailure(status)
		if (!new URLSearchParams(location.search).get('platform')) platform = normalizePlatform(status.platform)
	} catch (e) {
		const f = toCommandFailure(e)
		// 코어가 없는 상태에서는 모든 명령이 not_ready 오류를 돌려준다
		if (f.kind === 'not_ready') return showStartupFailure({ error: f.message, model_dir: '', data_dir: '', server_url: '' })
		// 이 상태 명령을 모르는 이전 실행 파일이면 그냥 이어간다
	}
	try {
		ccInput.value = localStorage.getItem(LAST_CC_KEY) ?? ''
	} catch {
		/* 무시 */
	}
	updateBytes()
	// 계정은 이 기기에 저장된 상태로 먼저 즉시 그리고, 서버에 아직 유효한지 확인하는 것은 뒤에서 한다
	void refreshAccount(false).then(() => refreshAccount(true))
	void refresh({ flush: true })
	setInterval(() => {
		void hasWork().then((work) => {
			if (work) void refresh({ flush: true })
		})
	}, RECHECK_MS)
	window.addEventListener('online', () => void refresh({ flush: true }))
	void refreshFilter()
	void refreshDraft()
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
