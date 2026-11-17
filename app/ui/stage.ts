// 3단계 오버레이 창의 규칙: 단계 크기, 단계 옮기기, 누르기와 끌기 구분, 창 자리, 미니 피드, 동기화 점, 걸러 보기.
// DOM과 Tauri를 건드리지 않는 순수 함수만 둔다 (stage.test.ts). 창을 실제로 움직이는 것은 main.ts의 "단계 창"이다.
//
//   0단계 알약(360x56)   동기화 점과 최근 한 줄. 누르면 1단계, 끌면 창을 옮긴다
//   1단계 미니 피드(360x260)   최근 내 평가 3개와 빠른 입력. 맨 위 줄을 누르면 2단계, Esc나 접기는 0단계
//   2단계 서랍(400x580)   검색, 걸러 보기, 타임라인, 계정 · 지갑 · 모델. Esc는 1단계
//
// 모바일(Android · iOS)은 창 크기를 바꿀 수 없으므로 2단계를 전체 화면으로만 쓴다 (mode 'full').
import type { MatchVerdict, Tier } from './commands.ts'
import { formatAgo, previewText, stateView, type Platform } from './view.ts'

export type Stage = 0 | 1 | 2
export type LayoutMode = 'overlay' | 'full'

/** 단계마다 창 크기 (논리 픽셀). style.css의 --stage-w / --stage-h와 같은 값이어야 한다 (stage.test.ts가 확인한다) */
export const STAGE_SIZE: Record<Stage, { width: number; height: number }> = {
	0: { width: 360, height: 56 },
	1: { width: 360, height: 260 },
	2: { width: 400, height: 580 },
}

/** 화면 가장자리(작업 영역)와의 여백, 논리 픽셀 */
export const DOCK_MARGIN = 16
/** 이만큼(논리 픽셀) 움직이기 전에 손을 떼면 누르기, 넘으면 끌기 */
export const DRAG_THRESHOLD = 5
/** 알약에 알림을 보여 주는 시간. 지나면 최근 글 한 줄로 돌아간다 */
export const NOTICE_TTL_MS = 60_000

/** 개발용 브라우저에서 첫 화면을 고를 때만 쓴다. 앱은 cmd_app_status의 platform이 정답이다 */
export function platformFromUserAgent(ua: string | null | undefined): Platform | null {
	if (!ua) return null
	if (/Android/i.test(ua)) return 'android'
	if (/iPhone|iPad|iPod/i.test(ua)) return 'ios'
	return null
}

/** 개발용 미리보기 ?stage=0|1|2. 그 밖의 값은 null */
export function parseStage(value: string | null | undefined): Stage | null {
	return value === '0' ? 0 : value === '1' ? 1 : value === '2' ? 2 : null
}

/** 데스크톱은 오버레이(단계마다 창 크기를 바꾼다), 모바일은 전체 화면 */
export function layoutMode(platform: Platform): LayoutMode {
	return platform === 'desktop' ? 'overlay' : 'full'
}

/** 전체 화면에서는 늘 2단계다 */
export function effectiveStage(mode: LayoutMode, stage: Stage): Stage {
	return mode === 'full' ? 2 : stage
}

export type StageEvent = 'pill' | 'feed-top' | 'expand' | 'escape' | 'collapse'

/** 단계 옮기기. ready가 false면(앱이 아직 시작하지 못했으면) 알약을 눌렀을 때 바로 시작 안내가 있는 2단계로 간다 */
export function nextStage(current: Stage, event: StageEvent, opts: { ready?: boolean } = {}): Stage {
	const ready = opts.ready ?? true
	switch (event) {
		case 'pill':
			return current === 0 ? (ready ? 1 : 2) : current
		case 'feed-top':
		case 'expand':
			return 2
		case 'escape':
			return current === 2 ? (ready ? 1 : 0) : 0
		case 'collapse':
			return 0
	}
}

/** 누른 점에서 움직인 거리로 누르기와 끌기를 가른다 */
export function classifyPointer(dx: number, dy: number, threshold = DRAG_THRESHOLD): 'click' | 'drag' {
	return Math.hypot(dx, dy) >= threshold ? 'drag' : 'click'
}

/** 모니터의 작업 영역 (작업 표시줄을 뺀 곳). 물리 픽셀 */
export interface WorkArea {
	x: number
	y: number
	width: number
	height: number
}

/** 창을 커지거나 작아지게 할 때 오른쪽 아래 모서리를 고정한다.
 *  anchor가 없으면 작업 영역 오른쪽 아래에 여백을 두고 붙이고, 있으면(사용자가 끌어 옮긴 창) 그 모서리를 지킨다.
 *  어느 쪽이든 창 전체가 작업 영역 안에 들어오게 자른다. 크기는 논리, 위치는 물리 픽셀이다 */
export function placeWindow(
	size: { width: number; height: number },
	area: WorkArea,
	scale: number,
	anchor: { right: number; bottom: number } | null = null,
	margin = DOCK_MARGIN,
): { x: number; y: number } {
	const s = Number.isFinite(scale) && scale > 0 ? scale : 1
	const w = Math.round(size.width * s)
	const h = Math.round(size.height * s)
	const m = Math.round(margin * s)
	const right = anchor ? anchor.right : area.x + area.width - m
	const bottom = anchor ? anchor.bottom : area.y + area.height - m
	const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(v, Math.max(lo, hi)))
	return {
		x: clamp(right - w, area.x, area.x + area.width - w),
		y: clamp(bottom - h, area.y, area.y + area.height - h),
	}
}

/** 커지는 쪽이면 먼저 옮기고 키운다 (키운 뒤 옮기면 잠깐 화면 밖으로 나간다). 작아지면 먼저 줄이고 옮긴다 */
export function growsTo(from: Stage, to: Stage): boolean {
	const a = STAGE_SIZE[from]
	const b = STAGE_SIZE[to]
	return b.width * b.height > a.width * a.height
}

// ---- 미니 피드 ----

/** 최근 n개. 위에서 아래로 오래된 것 → 새것 (새 글이 맨 아래에 붙는다) */
export function feedRows<T extends { created_at: number }>(rows: readonly T[], n = 3): T[] {
	return [...rows].sort((a, b) => b.created_at - a.created_at).slice(0, n).reverse()
}

/** 피드 줄의 불투명도. 맨 아래(가장 새것)부터 1, 0.8, 0.55 */
export function feedOpacity(fromBottom: number): number {
	return [1, 0.8, 0.55][fromBottom] ?? 0.4
}

/** 판정이 실제로 난 등급. 판정 전(grade 없음)이나 없으면 unrated */
export function effectiveTier(v: Pick<MatchVerdict, 'tier' | 'grade'> | null | undefined): Tier {
	if (!v || v.grade === null || v.grade === undefined) return 'unrated'
	return v.tier
}

/** 줄 앞의 아이콘. 등록된 글은 판정으로, 나머지는 상태로 */
export function itemIcon(state: string | null | undefined, tier: Tier = 'unrated'): string {
	if (!state || state === 'stored') {
		if (tier === 'premium') return '⭐'
		if (tier === 'discount') return '🔻'
		return state ? '✓' : '·'
	}
	switch (state) {
		case 'queued':
			return '📤'
		case 'pending':
			return '⏳'
		case 'rejected':
			return '⛔'
		case 'failed':
			return '⚠'
		default:
			return '?'
	}
}

/** 피드 한 줄: `[cc · 상태] 미리보기`. 판정이 난 글은 상태 대신 판정 이름 */
export function feedLine(row: { cc: string; state: string; text: string }, tier: Tier = 'unrated'): { icon: string; tag: string; preview: string } {
	const label = row.state === 'stored' && tier === 'premium' ? '추천' : row.state === 'stored' && tier === 'discount' ? '비추천' : stateView(row.state).label
	return { icon: itemIcon(row.state, tier), tag: `${row.cc || '도메인 없음'} · ${label}`, preview: previewText(row.text, 60) }
}

// ---- 동기화 점 (알약) ----

export type SyncDot = { tone: 'ok' | 'wait' | 'busy' | 'bad' | 'muted'; label: string }

/** 알약의 점과 짧은 말. 오프라인 > 전송 대기 > 검수 중 > 등록됨 순서로 본다.
 *  net은 서버와 실제로 주고받은 결과(main.ts setNet)이고, 모르면 null이다 */
export function syncDot(rows: readonly { state: string }[], net: 'ok' | 'down' | null, ready = true): SyncDot {
	if (!ready) return { tone: 'bad', label: '설정 필요' }
	const queued = rows.filter((r) => r.state === 'queued').length
	const pending = rows.filter((r) => r.state === 'pending').length
	if (net === 'down') return { tone: 'bad', label: queued ? `오프라인 · 대기 ${queued}건` : '오프라인' }
	if (queued) return { tone: 'wait', label: `전송 대기 ${queued}건` }
	if (pending) return { tone: 'busy', label: `검수 중 ${pending}건` }
	if (rows.length === 0) return { tone: 'muted', label: '쓴 글 없음' }
	return { tone: 'ok', label: '등록됨' }
}

/** 알약의 한 줄. 1분 안의 알림이 있으면 그것, 아니면 가장 최근 글 */
export function pillLine(
	notice: { title: string; at: number } | null,
	latest: { cc: string; state: string; text: string; created_at: number } | null,
	now: number,
	ttl = NOTICE_TTL_MS,
): string {
	if (notice && now - notice.at < ttl) return notice.title
	if (latest) return `${latest.cc || '도메인 없음'} · ${previewText(latest.text, 40)} · ${formatAgo(latest.created_at, now)}`
	return '아직 쓴 글이 없습니다'
}

/** 빠른 입력의 안내 문구 */
export function quickPlaceholder(cc: string | null | undefined): string {
	const c = (cc ?? '').trim()
	return c ? `${c}에 빠른 메모/평가 남기기...` : '도메인을 먼저 정하세요 (전체 화면의 평가 쓰기)'
}

// ---- 걸러 보기 (2단계) ----

export type FeedFilter = 'all' | 'premium' | 'discount' | 'waiting'

export const FILTER_CHIPS: readonly { id: FeedFilter; label: string }[] = [
	{ id: 'all', label: '전체' },
	{ id: 'premium', label: '⭐ 추천' },
	{ id: 'discount', label: '🔻 비추천' },
	{ id: 'waiting', label: '⏳ 전송 대기/검수 중' },
]

/** 걸러 보기. tier는 effectiveTier로 맞춘 값, cc가 있으면 그 도메인만 (대소문자 · 앞뒤 공백 무시) */
export function matchesFilter(item: { state?: string | null; tier?: Tier | null; cc?: string | null }, filter: FeedFilter, cc: string | null = null): boolean {
	const want = (cc ?? '').trim().toLowerCase()
	if (want && (item.cc ?? '').trim().toLowerCase() !== want) return false
	switch (filter) {
		case 'all':
			return true
		case 'premium':
			return item.tier === 'premium'
		case 'discount':
			return item.tier === 'discount'
		case 'waiting':
			return item.state === 'queued' || item.state === 'pending'
	}
}

// ---- 계정 · 지갑 · 모델 (2단계 아래 접이식) ----

/** 지갑 칸. 앱에는 아직 지갑 명령이 없다 (시드 지갑 서명은 crates/pd-identity에만 있다). 세션의 계정 주소만 보여 준다 */
export function walletLine(info: { state: 'anonymous' | 'signed_in'; address: string | null }): { address: string; note: string } {
	const note = '지갑(시드 구문) 명령은 아직 앱에 없습니다.'
	if (info.state === 'signed_in' && info.address) return { address: info.address, note: `로그인한 계정 주소입니다. ${note}` }
	return { address: '', note: `로그인하면 계정 주소가 여기 보입니다. ${note}` }
}

/** 임베딩 모델 설치 상태 한 줄 */
export function modelSummary(status: { installed: 'none' | 'text' | 'full'; revision: string }): string {
	const rev = status.revision ? ` · 판 ${status.revision.slice(0, 7)}` : ''
	if (status.installed === 'full') return `임베딩 모델: 전체(이미지 · 오디오 포함) 설치됨${rev}`
	if (status.installed === 'text') return `임베딩 모델: 텍스트 전용 설치됨${rev}`
	return '임베딩 모델: 설치되지 않음'
}
