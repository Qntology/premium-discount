// 화면에 보여 줄 문구와 값을 만드는 순수 함수. DOM을 건드리지 않아서 Node에서 테스트한다 (view.test.ts).
import type { CommandFailure, MatchVerdict, MergedMatch, SessionInfo, SubmitResult, VoteResult } from './commands.ts'

/** 서버·Rust와 같은 값 (app/src/pipeline.rs MAX_TEXT_BYTES) */
export const MAX_TEXT_BYTES = 8192

/** 로컬 상태 → 사용자에게 보이는 말과 색 계열 (localdb.ts STATES와 같은 목록) */
export const STATE_VIEW: Record<string, { label: string; tone: 'wait' | 'busy' | 'ok' | 'bad' | 'muted' }> = {
	queued: { label: '전송 대기', tone: 'wait' },
	pending: { label: '검수 중', tone: 'busy' },
	stored: { label: '등록됨', tone: 'ok' },
	rejected: { label: '거부됨', tone: 'bad' },
	failed: { label: '등록 실패', tone: 'bad' },
	missing: { label: '확인 불가', tone: 'muted' },
}

export function stateView(state: string) {
	return STATE_VIEW[state] ?? { label: state || '알 수 없음', tone: 'muted' as const }
}

/** 거부 사유를 사람이 읽는 말로. Rust가 로컬 행에 남기는 형식 (app/src/queue.rs)
 *  - `server status rejected: nsfw,illegal`   서버가 사유와 함께 거부 (사유는 cron의 질문 이름, 쉼표로 이어짐)
 *  - `server status rejected` / `server status failed`   사유 없이 거부, 또는 재시도 한도를 넘겨 실패
 *  - `parent local-1 was rejected`   답글을 단 글이 거부되어 함께 버려짐
 *  - `server returned 400: …`   서버가 요청 자체를 받아들이지 않음 */
const REASONS: Record<string, string> = {
	profanity: '욕설',
	nsfw: '음란·폭력',
	illegal: '불법·개인정보',
	'parent rejected': '답글을 단 글이 거부됨',
	'parent missing': '답글을 단 글을 찾을 수 없음',
}
export function reasonLabel(reason: string): string {
	let r = reason.trim()
	if (!r) return ''
	const status = r.match(/^server status (rejected|failed)(?::\s*(.*))?$/)
	if (status) {
		if (!status[2]) return status[1] === 'failed' ? '서버가 처리하지 못했습니다 (재시도 한도 초과)' : '서버가 거부했습니다'
		r = status[2]
	}
	if (/^parent .+ was rejected$/.test(r)) return '답글을 단 글이 거부됨'
	const http = r.match(/^server returned (\d{3})/)
	if (http) return `서버가 요청을 받아들이지 않았습니다 (${http[1]})`
	return r
		.split(',')
		.map((x) => REASONS[x.trim()] ?? x.trim())
		.join(', ')
}

/** UTF-8 바이트 수 (서버의 8KB 제한과 같은 기준) */
export function utf8Length(s: string): number {
	return new TextEncoder().encode(s).length
}

export function byteCounter(text: string) {
	const used = utf8Length(text)
	return { used, over: used > MAX_TEXT_BYTES, label: `${used.toLocaleString('ko-KR')} / ${MAX_TEXT_BYTES.toLocaleString('ko-KR')} 바이트` }
}

export function previewText(text: string, max = 80): string {
	const t = text.replace(/\s+/g, ' ').trim()
	return t.length > max ? t.slice(0, max) + '…' : t
}

/** 코사인 유사도 → 퍼센트 표시 (음수는 0) */
export function scorePercent(score: number): string {
	return `${Math.round(Math.max(0, Math.min(1, score)) * 100)}%`
}

/** 밀리초 시각 → "방금", "5분 전", "3시간 전", 그 밖에는 날짜 */
export function formatAgo(ms: number, now: number): string {
	const sec = Math.max(0, Math.round((now - ms) / 1000))
	if (sec < 45) return '방금'
	if (sec < 3600) return `${Math.round(sec / 60)}분 전`
	if (sec < 86400) return `${Math.round(sec / 3600)}시간 전`
	const d = new Date(ms)
	return `${d.getFullYear()}.${String(d.getMonth() + 1).padStart(2, '0')}.${String(d.getDate()).padStart(2, '0')}`
}

export type Notice = { tone: 'ok' | 'busy' | 'warn' | 'bad'; title: string; detail?: string; keepText: boolean }

/** 저장 결과 안내. keepText가 true면 입력한 글을 지우지 않는다 (고쳐서 다시 낼 수 있게) */
export function describeSubmit(r: SubmitResult): Notice {
	switch (r.status) {
		case 'blocked':
			return { tone: 'warn', title: '욕설이 포함되어 등록하지 않았습니다', detail: '이 기기에도 저장하지 않았고 서버로도 보내지 않았습니다. 고쳐서 다시 등록하세요.', keepText: true }
		case 'queued':
			return { tone: 'warn', title: '오프라인이라 이 기기에 저장했습니다', detail: '연결되면 자동으로 서버에 보냅니다.', keepText: false }
		case 'accepted':
			return { tone: 'busy', title: '서버가 받았습니다. 검수 중입니다', detail: '보통 1분 안에 끝납니다. 결과는 아래 목록에서 확인하세요.', keepText: false }
		case 'saved':
			return { tone: 'ok', title: '등록했습니다', keepText: false }
	}
}

/** 명령 오류 → 사용자 안내. Rust의 오류 문구(영어)를 알려진 것은 한국어로 바꾼다 */
export function describeError(e: Pick<CommandFailure, 'kind' | 'message'>): Notice {
	const m = e.message
	const bad = (title: string, detail?: string): Notice => ({ tone: 'bad', title, detail, keepText: true })
	switch (e.kind) {
		case 'invalid_input':
			if (m.includes('cc must be')) return bad('도메인 형식이 올바르지 않습니다', '예: example.com')
			if (m.includes('text must be at most')) return bad(`글이 너무 깁니다 (최대 ${MAX_TEXT_BYTES.toLocaleString('ko-KR')}바이트)`)
			if (m.includes('limit is')) return bad('글이 너무 길어 처리할 수 없습니다', '조금 줄여서 다시 시도하세요.')
			if (m.includes('must not be empty')) return bad('내용을 입력하세요')
			return bad('입력을 확인하세요', m)
		case 'offline':
			return bad('서버에 연결할 수 없습니다')
		case 'embed':
			return bad('글을 벡터로 바꾸지 못했습니다', '모델 파일 설정을 확인하세요.')
		case 'storage':
			return bad('이 기기에 저장하지 못했습니다', m)
		case 'queue_full':
			return bad('보내지 못한 글이 너무 많습니다', '연결을 확인한 뒤 다시 시도하세요.')
		case 'rejected':
			return bad('서버가 거부했습니다', m)
		default:
			return bad('요청을 처리하지 못했습니다', m)
	}
}

/** 병합 검색 결과 한 건의 출처 표시. 내 글의 진행 상태(전송 대기, 검수 중, 등록됨)는 상태 표시가 따로 보여 준다.
 *  source가 local인 이유는 여럿이다 (아직 서버에 없음, 서버 검색 실패, 서버 결과 순위 밖). 그래서 라벨로 "서버 반영 전"을 단정하지 않는다. */
export function sourceView(m: Pick<MergedMatch, 'source'>): { label: string; mine: boolean } {
	switch (m.source) {
		case 'local':
		case 'both':
			return { label: '내 글', mine: true }
		default:
			return { label: '다른 사용자', mine: false }
	}
}

// ---- 추천 / 비추천 ----

type Tone = 'ok' | 'busy' | 'wait' | 'bad' | 'muted'

/** grade(−2..2) → 배지. 판정이 없거나(unrated) 서버 결과가 아니면 '판정 전' */
export function tierView(v: MatchVerdict | null | undefined): { label: string; tone: Tone } {
	if (!v || v.tier === 'unrated' || v.grade === null) return { label: '판정 전', tone: 'muted' }
	if (v.tier === 'premium') return v.grade >= 2 ? { label: '강력 추천', tone: 'ok' } : { label: '추천', tone: 'ok' }
	if (v.tier === 'discount') return v.grade <= -2 ? { label: '강력 비추천', tone: 'bad' } : { label: '비추천', tone: 'bad' }
	return { label: '보통', tone: 'muted' }
}

/** 투표할 수 있는 결과: 서버에 있는 남의 글. 내 글(both, local)은 서버가 세지 않으므로 버튼을 두지 않는다 */
export function canVote(m: Pick<MergedMatch, 'id' | 'source'>): boolean {
	return m.source === 'remote' && !!m.id
}

/** 같은 버튼을 다시 누르면 철회(0)다 */
export function nextVote(current: 1 | -1 | null | undefined, pressed: 1 | -1): 1 | -1 | 0 {
	return current === pressed ? 0 : pressed
}

/** 투표 결과 안내 */
export function describeVote(r: VoteResult, value: 1 | -1 | 0): Notice {
	const what = value === 0 ? '투표를 거뒀습니다' : value === 1 ? '추천했습니다' : '비추천했습니다'
	if (r.status === 'queued') return { tone: 'warn', title: `${what} (이 기기에 저장)`, detail: '연결되면 자동으로 서버에 보냅니다.', keepText: true }
	if (!r.counted) return { tone: 'busy', title: what, detail: '로그인하면 판정에 반영됩니다. 로그인 전 표는 저장만 됩니다.', keepText: true }
	return { tone: 'ok', title: what, detail: '판정은 1분 안에 다시 계산됩니다.', keepText: true }
}

// ---- 로그인 ----

/** 주소를 짧게: 0x1234…abcd. 비어 있으면 빈 문자열 */
export function shortAddress(address: string | null | undefined): string {
	if (!address) return ''
	return address.length > 14 ? `${address.slice(0, 6)}…${address.slice(-4)}` : address
}

/** 남은 시간 m:ss */
export function formatCountdown(sec: number): string {
	const s = Math.max(0, Math.floor(sec))
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}

/** 머리글의 계정 표시와, 등록 폼 아래의 작성자 안내 */
export function accountView(info: Pick<SessionInfo, 'state' | 'address'>): { signedIn: boolean; label: string; author: string } {
	if (info.state === 'signed_in') {
		const short = shortAddress(info.address)
		return { signedIn: true, label: short || '로그인됨', author: short ? `등록 주소: ${short}` : '로그인한 계정으로 등록합니다.' }
	}
	return { signedIn: false, label: '게스트', author: '게스트로 등록합니다. 로그인하면 내 계정 주소로 등록됩니다.' }
}

/** 로그인 중 생긴 오류를 사용자 문구로. Rust가 돌려주는 실제 문구("server returned 503: …")를 알아본다 */
export function describeLoginError(e: Pick<CommandFailure, 'kind' | 'message'>): string {
	const m = e.message
	if (e.kind === 'offline') return '서버에 연결할 수 없습니다. 연결을 확인한 뒤 다시 시도하세요.'
	if (e.kind === 'rejected' && m.includes('not supported')) return '이 서버는 앱 로그인을 지원하지 않습니다.'
	if (e.kind === 'server') {
		if (m.includes('503') && m.includes('not configured')) return '서버에서 로그인이 아직 설정되지 않았습니다. 운영자에게 문의하세요.'
		if (m.includes('503')) return '로그인 서버가 잠시 응답하지 않습니다. 조금 뒤에 다시 시도하세요.'
		if (m.includes('429')) return '요청이 너무 많습니다. 잠시 뒤에 다시 시도하세요.'
	}
	return `로그인을 진행하지 못했습니다. ${m}`
}

export type Platform = 'android' | 'ios' | 'desktop'

export function normalizePlatform(value: string | null | undefined): Platform {
	return value === 'android' || value === 'ios' ? value : 'desktop'
}

/** 로그인 방식. 모바일은 이 기기의 메일 앱으로 바로 가는 링크, 데스크톱은 폰으로 스캔하는 QR(링크는 보조) */
export function loginMode(platform: Platform): 'link' | 'qr' {
	return platform === 'desktop' ? 'qr' : 'link'
}

export const MAIL_SUBJECT = 'premium.discount 로그인'
export const MAIL_BODY = '이 메일을 그대로 보내면 로그인됩니다. 내용은 바꾸지 마세요.'

/** 메일 앱을 여는 링크. 받는 사람, 제목, 본문을 채워 두어 사용자는 "보내기"만 누르면 된다.
 *  (메일 앱은 보안상 사용자가 직접 보내기를 눌러야 하므로 자동 전송은 할 수 없다) */
export function mailtoLink(address: string): string {
	return `mailto:${address}?subject=${encodeURIComponent(MAIL_SUBJECT)}&body=${encodeURIComponent(MAIL_BODY)}`
}

export type LoginPhase = 'starting' | 'waiting' | 'done' | 'expired' | 'error'

/** 로그인 창의 상태 줄 */
export function describeLoginPhase(phase: LoginPhase, opts: { remainingSec?: number; address?: string | null; error?: string } = {}): Notice {
	switch (phase) {
		case 'starting':
			return { tone: 'busy', title: 'QR을 만드는 중입니다…', keepText: true }
		case 'waiting':
			return { tone: 'busy', title: '메일을 기다리는 중입니다', detail: opts.remainingSec === undefined ? undefined : `남은 시간 ${formatCountdown(opts.remainingSec)}`, keepText: true }
		case 'done':
			return { tone: 'ok', title: '로그인했습니다', detail: shortAddress(opts.address) || undefined, keepText: true }
		case 'expired':
			return { tone: 'warn', title: '시간이 지났습니다', detail: '새 QR을 만들어 다시 시도하세요.', keepText: true }
		case 'error':
			return { tone: 'bad', title: '로그인을 이어갈 수 없습니다', detail: opts.error, keepText: true }
	}
}

// ---- 시작 실패 ----

/** 시작 실패가 모델 파일이 없어서인지 (그렇다면 화면이 내려받기를 보여 준다) */
export function isModelMissing(error: string | null): boolean {
	return /토크나이저|가중치 파일|model\.safetensors|No such file|not found|모델/i.test(error ?? '')
}

// ---- 모델 내려받기 ----

/** 바이트를 MB·GB로 (소수 첫째 자리) */
export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes < 0) return '-'
	if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(2)}GB`
	return `${(bytes / 1e6).toFixed(1)}MB`
}

export interface ModelChoice {
	variant: 'text' | 'full'
	label: string
	detail: string
	recommended: boolean
}

/** 내려받기 선택지. 이 앱의 화면은 텍스트만 쓰므로 텍스트 전용을 권한다 (모바일은 메모리도 적다) */
export function modelChoices(sizes: { text_bytes: number; full_bytes: number }): ModelChoice[] {
	return [
		{ variant: 'text', label: `텍스트 전용 · ${formatBytes(sizes.text_bytes)}`, detail: '평가 쓰기·검색에 필요한 전부. 메모리 약 0.7GB', recommended: true },
		{ variant: 'full', label: `전체 · ${formatBytes(sizes.full_bytes)}`, detail: '이미지·오디오 입력까지 (그 입력이 올 때만 메모리에 올림)', recommended: false },
	]
}

/** 욕설 필터 상태 문구와, 받을 수 있으면 그 버튼 문구 */
export function describeFilter(info: {
	installed: boolean
	active: 'dictionary' | 'laya'
	download: { state: 'idle' | 'running' | 'done' | 'failed' | 'cancelled'; done_bytes: number; total_bytes: number; error: string | null }
	bytes: number
}): { text: string; action: string | null; busy: boolean } {
	if (info.active === 'laya') return { text: '욕설 필터: laya-multilingual 모델 (이 기기에서 판정)', action: null, busy: false }
	const d = info.download
	if (d.state === 'running') {
		const pct = d.total_bytes > 0 ? Math.floor((d.done_bytes / d.total_bytes) * 100) : 0
		return { text: `욕설 필터 모델 받는 중 ${formatBytes(d.done_bytes)} / ${formatBytes(d.total_bytes)} (${pct}%)`, action: null, busy: true }
	}
	if (info.installed) return { text: '욕설 필터: 기본 사전 (모델이 설치되어 있지만 설정 filter=dictionary로 꺼져 있습니다)', action: null, busy: false }
	const failed = d.state === 'failed' ? ` 받지 못했습니다: ${d.error ?? '알 수 없는 오류'}.` : ''
	return { text: `욕설 필터: 기본 사전.${failed}`, action: `더 정확한 모델 받기 (${formatBytes(info.bytes)})`, busy: false }
}

/** 초안 모델 상태 문구와, 받을 수 있으면 그 버튼 문구 */
export function describeDraftModel(info: {
	installed: boolean
	download: { state: 'idle' | 'running' | 'done' | 'failed' | 'cancelled'; done_bytes: number; total_bytes: number; error: string | null }
	bytes: number
}): { text: string; action: string | null; busy: boolean; ready: boolean } {
	const d = info.download
	if (d.state === 'running') {
		const pct = d.total_bytes > 0 ? Math.floor((d.done_bytes / d.total_bytes) * 100) : 0
		return { text: `초안 모델 받는 중 ${formatBytes(d.done_bytes)} / ${formatBytes(d.total_bytes)} (${pct}%)`, action: null, busy: true, ready: false }
	}
	if (info.installed) return { text: '초안 모델: Qwen3.5-0.8B (이 기기)', action: null, busy: false, ready: true }
	const failed = d.state === 'failed' ? ` 받지 못했습니다: ${d.error ?? '알 수 없는 오류'}.` : ''
	return { text: `초안 모델이 없습니다.${failed}`, action: `초안 모델 받기 (${formatBytes(info.bytes)})`, busy: false, ready: false }
}

/** 초안을 사람이 읽기 좋게 */
export function formatDraft(d: { type: number; head: string; body: string; link: string; digest: string }): string {
	const kind = d.type === 1 ? '1 premium' : '2 discount'
	return `type: ${kind}\nhead: ${d.head}\nbody: ${d.body}\nlink: ${d.link}\ndigest: ${d.digest}`
}

/** 진행 상황 문구와 표시 */
export function describeDownload(p: {
	state: 'idle' | 'running' | 'done' | 'failed' | 'cancelled'
	done_bytes: number
	total_bytes: number
	error: string | null
}): { text: string; tone: 'info' | 'ok' | 'warn' | 'bad'; percent: number | null; busy: boolean; resumable: boolean } {
	const percent = p.total_bytes > 0 ? Math.min(100, Math.floor((p.done_bytes / p.total_bytes) * 100)) : null
	const amount = `${formatBytes(p.done_bytes)} / ${formatBytes(p.total_bytes)}`
	switch (p.state) {
		case 'running':
			return { text: `내려받는 중 ${amount} (${percent ?? 0}%)`, tone: 'info', percent, busy: true, resumable: false }
		case 'done':
			return { text: '설치를 마쳤습니다. 앱을 시작합니다…', tone: 'ok', percent: 100, busy: false, resumable: false }
		case 'cancelled':
			return { text: `멈췄습니다 (${amount}). 다시 누르면 이어받습니다.`, tone: 'warn', percent, busy: false, resumable: true }
		case 'failed':
			return { text: `내려받지 못했습니다: ${p.error ?? '알 수 없는 오류'}`, tone: 'bad', percent, busy: false, resumable: true }
		default:
			return { text: '', tone: 'info', percent: null, busy: false, resumable: false }
	}
}

/** 앱이 시작하지 못했을 때 화면에 보여 줄 안내 (모델 폴더가 없는 경우가 가장 흔하다) */
export function describeStartup(status: { error: string | null; model_dir: string; data_dir: string; server_url: string }): {
	title: string
	error: string
	hints: string[]
	/** info: 처음 실행처럼 모델만 없으면 고장이 아니므로 오류처럼 보이지 않게 한다 */
	tone: 'info' | 'bad'
} {
	const error = status.error ?? '알 수 없는 오류'
	const hints: string[] = []
	const missing = isModelMissing(error)
	if (missing) {
		hints.push(`임베딩 모델(google/embeddinggemma-2)이 이 폴더에 있어야 합니다: ${status.model_dir}`)
		hints.push('아래에서 바로 내려받거나, 이미 받은 폴더(model.safetensors 또는 model-text.safetensors와 tokenizer.json)를 설정 파일의 model_dir 또는 PD_MODEL_DIR 환경 변수로 지정하세요.')
	}
	hints.push(`이 기기의 데이터 폴더: ${status.data_dir}`)
	hints.push(`서버: ${status.server_url}`)
	return { title: missing ? '모델이 필요합니다' : '앱을 시작하지 못했습니다', error, hints, tone: missing ? 'info' : 'bad' }
}
