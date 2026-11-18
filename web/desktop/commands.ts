// Rust 명령(app/src/commands.rs)을 부르는 화면 쪽 래퍼.
//
// `invoke`는 화면 셸이 넘겨 준다. Tauri 웹뷰에서는 `window.__TAURI__.core.invoke`다. 이 파일은 Tauri를 직접 import하지 않으므로
// Node에서도 테스트할 수 있다 (localdb.ts가 Dexie를 직접 import하지 않는 것과 같다).
//
// 모든 명령은 인자 객체 하나를 `args`라는 이름으로 받는다. Tauri는 명령 인자 이름을 camelCase로 바꾸기 때문에,
// 이름 변환이 끼지 않도록 구조체 하나(`args`)로만 주고받는다. 인자 안의 이름은 Rust와 같은 snake_case다.
//
// "없음"은 빈 문자열이어도 된다 (localdb.ts와 같은 규칙). Rust가 빈 문자열을 "없음"으로 읽는다.
//
// 명령 이름과 인자 모양은 fixtures/command_contract.json이 양쪽 테스트의 계약이다.

import { syncFromRust, type DbLike, type SyncResult } from './localdb.ts'

export type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>

export interface SubmitArgs {
	text: string
	cc: string
	from?: string
	to?: string
	/** 콘텐츠 해시 주소 (0x + 40자리) */
	bcc?: string
	ref?: string
	/** 부모가 아직 서버에 없을 때 보관함의 로컬 id */
	local_parent?: string
}

export type SubmitResult =
	| { status: 'blocked' }
	| { status: 'saved' | 'accepted'; id: string; root: string; ref: string | null; from: string }
	| { status: 'queued'; local_id: string }

export interface SearchLocalArgs {
	query: string
	top_k?: number
	cc?: string
}

export interface SearchArgs extends SearchLocalArgs {
	root?: string
	ref?: string
	to?: string
	/** 등급 필터. 판정은 서버에만 있으므로 이 필터가 있으면 내 글(로컬)은 섞이지 않는다 */
	tier?: '' | TierFilter
}

/** 추천(premium) / 보통(neutral) / 비추천(discount) / 판정 전(unrated). 규칙은 contracts/verdict_v1.json */
export type Tier = 'premium' | 'neutral' | 'discount' | 'unrated'
export type TierFilter = 'premium' | 'discount' | 'rated'

/** 검색 결과에 붙는 서버 판정 요약 */
export interface MatchVerdict {
	tier: Tier
	/** -2..2 (unrated면 null) */
	grade: number | null
	rank: number | null
	evidence: number
}

/** 평가 하나의 전역 판정 (cmd_verdicts) */
export interface Verdict extends MatchVerdict {
	id: string
	value: number
	up: number
	down: number
	level: 'secondary' | 'primary' | 'global' | null
	/** 이유 코드 (rank_high, held, low_evidence, level:secondary …) */
	reasons: string[]
	recipe: string
	updated_at: number | null
	/** 앱이 서버에서 받은 시각 (밀리초) */
	fetched_at: number
}

export interface VerdictsResult {
	verdicts: Verdict[]
	/** 한 번도 받은 적 없는 id (오프라인일 때) */
	missing: string[]
	/** 서버에 묻지 못해 캐시 값을 그대로 줬는지 */
	stale: boolean
	remote_error: string | null
}

/** 1 추천 | -1 비추천 | 0 철회 */
export type VoteValue = 1 | -1 | 0

export interface VoteArgs {
	id: string
	value: VoteValue
	kind?: 'vote' | 'vote_outcome'
}

export type VoteResult =
	/** 서버가 받았다. counted가 false면 판정에 들어가지 않는다 (로그인 전이거나 내 글) */
	| { status: 'sent'; request_id: string; counted: boolean }
	/** 연결이 없어 보관함에 남았다. 다음 flushOutbox가 보낸다 */
	| { status: 'queued'; request_id: string }

/** 신구 대체 보고: 새 평가(new)가 옛 평가(old)를 대체한다 (같은 장소 · 대상에서 수치 · 단위 · 형용사만 바뀜) */
export interface SupersedeArgs {
	old: string
	new: string
}

/** 서버가 받았고, 옛 평가의 판정 캐시는 discount(grade -2, 이유 superseded)로 바뀌었다 */
export interface SupersedeResult {
	old: string
	new: string
	request_id: string
}

export interface LocalMatch {
	request_id: string
	local_id: string
	id: string | null
	cc: string
	state: string
	text: string
	score: number
}

export interface RemoteMatch {
	id: string
	score: number
	cc: string | null
	root: string | null
	ref: string | null
	to: string | null
	verdict?: MatchVerdict | null
	/** 이 기기에서 한 표 */
	my_vote?: 1 | -1
}

export interface MergedMatch {
	id: string | null
	request_id: string | null
	score: number
	cc: string | null
	root: string | null
	ref: string | null
	to: string | null
	/** 내 글이면 원문. 서버 응답에는 원문이 없다 */
	text: string | null
	state: string | null
	source: 'local' | 'remote' | 'both'
	/** 서버의 전역 판정 (서버 결과에 있을 때만) */
	verdict: MatchVerdict | null
	/** 이 기기에서 한 표 (1 | -1) */
	my_vote: 1 | -1 | null
}

export interface MergedSearch {
	matches: MergedMatch[]
	/** 서버 검색이 실패했을 때의 이유. 이때 matches는 내 글(로컬)뿐이다 */
	remote_error: string | null
}

export interface RejectedItem {
	local_id: string
	reason: string
}

export interface FlushSummary {
	sent: number
	rejected: RejectedItem[]
	remaining: number
	stopped: string | null
	stored: number
	sync_rejected: RejectedItem[]
	missing: number
	still_pending: number
	sync_stopped: string | null
	persist_error: string | null
	votes_sent: number
	votes_remaining: number
	votes_rejected: { request_id: string; eval_id: string; reason: string }[]
	votes_stopped: string | null
}

export interface SessionInfo {
	state: 'anonymous' | 'signed_in'
	address: string | null
	/** 알려 줄 일이 있을 때의 문구 (저장된 로그인 정보를 읽지 못함, 로그인이 만료됨) */
	warning: string | null
}

/** 앱이 준비되었는지와 설정. ready가 false이면 error에 이유가 있다 (예: 모델 폴더가 없음) */
export interface AppStatus {
	ready: boolean
	error: string | null
	model_dir: string
	data_dir: string
	server_url: string
	version: string
	/** android | ios | desktop. 로그인 방식(메일 링크 또는 QR)을 고르는 데 쓴다 */
	platform: 'android' | 'ios' | 'desktop'
	/** 모델 타워 상주 상태 (코어가 있을 때만) */
	model?: ResidencyReport
}

/** 모델 설치 종류: 텍스트 전용(약 574MB), 전체(이미지·오디오 포함, 약 1.52GB), 욕설 필터 모델(laya-multilingual, 약 678MB) */
export type ModelVariant = 'text' | 'full' | 'filter' | 'draft'

/** 욕설 필터 모델 상태 */
export interface FilterInfo {
	installed: boolean
	/** 지금 쓰는 필터 */
	active: 'dictionary' | 'laya'
	download: FetchProgress
	bytes: number
}

export interface FetchProgress {
	state: 'idle' | 'running' | 'done' | 'failed' | 'cancelled'
	variant: ModelVariant | null
	done_bytes: number
	total_bytes: number
	/** 지금 받는 파일 */
	file: string | null
	error: string | null
}

export interface TowerReport {
	state: 'disabled' | 'not_installed' | 'standby' | 'resident'
	measured_mb: number | null
	budget_mb: number
}

export interface ResidencyReport {
	modalities: 'auto' | 'text'
	vision: TowerReport
	audio: TowerReport
	loads: number
	swaps: number
	refusals: number
	resident_mb: number | null
}

export interface ModelStatus {
	/** 모델 폴더에 설치된 것 */
	installed: 'none' | 'text' | 'full'
	model_dir: string
	download: FetchProgress
	/** 설치별 내려받을 크기 (바이트) */
	text_bytes: number
	full_bytes: number
	revision: string
	residency?: ResidencyReport
	filter?: FilterInfo
	/** items 초안 모델(Qwen3.5-0.8B). active는 '' 또는 'qwen'(메모리에 있음) */
	draft?: { installed: boolean; active: string; download: FetchProgress; bytes: number }
}

/** items 초안 (type 1: premium, 2: discount) */
export interface ItemDraft {
	type: 1 | 2
	head: string
	body: string
	link: string
	digest: string
}

export interface LoginStart {
	/** 메일을 보낼 주소. QR을 못 쓸 때 사용자가 직접 보낸다 */
	address: string
	/** QR에 넣을 mailto: 링크 */
	mailto: string
	expires_in_sec: number
}

export interface LoginStatus {
	/** none(시작한 로그인 없음) | pending(메일 대기) | signed_in | expired */
	state: 'none' | 'pending' | 'signed_in' | 'expired'
	address: string | null
	remaining_sec: number | null
	warning: string | null
}

/** Rust가 돌려주는 오류의 `kind` */
export type CommandErrorKind =
	| 'invalid_input'
	| 'embed'
	| 'filter'
	| 'offline'
	| 'http'
	| 'bad_response'
	| 'server'
	| 'rejected'
	| 'queue_full'
	| 'storage'
	| 'unknown_command'
	// 앱(Rust 코어)이 시작하지 못했다 (Tauri 셸이 돌려준다)
	| 'not_ready'
	| 'internal'

/** 명령이 실패했을 때 던지는 오류. `kind`로 분기한다 (예: 'offline'이면 "연결되면 보냅니다" 안내). */
export class CommandFailure extends Error {
	kind: CommandErrorKind | 'unknown'
	constructor(kind: CommandErrorKind | 'unknown', message: string) {
		super(message)
		this.name = 'CommandFailure'
		this.kind = kind
	}
}

/** invoke가 거부한 값(Rust의 {kind, message}, 문자열, Error)을 CommandFailure로 맞춘다 */
export function toCommandFailure(e: unknown): CommandFailure {
	if (e instanceof CommandFailure) return e
	if (e && typeof e === 'object' && 'kind' in e && 'message' in e) {
		const { kind, message } = e as { kind: unknown; message: unknown }
		return new CommandFailure(String(kind) as CommandErrorKind, String(message))
	}
	if (e instanceof Error) return new CommandFailure('unknown', e.message)
	return new CommandFailure('unknown', String(e))
}

export interface Commands {
	submitEvaluation(args: SubmitArgs): Promise<SubmitResult>
	/** `since` 이후 바뀐 행 (화면 캐시 형태). 보통은 syncCacheFromRust를 쓴다 */
	syncCache(since: number): Promise<unknown[]>
	searchLocal(args: SearchLocalArgs): Promise<LocalMatch[]>
	searchRemote(args: SearchArgs): Promise<{ matches: RemoteMatch[] }>
	/** 내 글(로컬)과 서버 검색을 합쳐서. 방금 저장한 글도 바로 보인다 */
	searchMerged(args: SearchArgs): Promise<MergedSearch>
	/** 못 보낸 항목을 보내고, 접수만 된 항목의 최종 결과를 확인한다. 앱을 켤 때와 연결이 돌아왔을 때 부른다 */
	flushOutbox(): Promise<FlushSummary>
	/** 남의 평가에 추천(1) / 비추천(-1) / 철회(0). 보관함에 먼저 쓰고 바로 보낸다 */
	vote(args: VoteArgs): Promise<VoteResult>
	/** 신구 대체 보고. 연결이 없으면 offline 오류 (같은 보고를 다시 보내도 서버에는 한 번) */
	supersede(args: SupersedeArgs): Promise<SupersedeResult>
	/** 평가들의 전역 판정. 캐시가 10분보다 오래됐거나 refresh면 서버에 묻는다 */
	verdicts(ids: string[], refresh?: boolean): Promise<VerdictsResult>
	/** 로그인 상태. verify가 true면 서버에 물어 아직 유효한지 확인한다 (연결이 없으면 그대로 둔다) */
	session(verify: boolean): Promise<SessionInfo>
	/** QR 로그인 시작. 서버가 세션을 발급하고, 화면에는 주소와 QR 링크만 온다 (token은 Rust 안에만 있다) */
	loginStart(): Promise<LoginStart>
	/** 로그인 진행 상태. 몇 초마다 부른다. 메일이 도착했으면 로그인하고 세션을 저장한다 */
	loginStatus(): Promise<LoginStatus>
	logout(): Promise<SessionInfo>
	appStatus(): Promise<AppStatus>
	/** 모델 설치 상태와 내려받기 진행 상황. 모델이 없어도(코어가 없어도) 답한다 */
	modelStatus(): Promise<ModelStatus>
	/** 모델 내려받기 시작 (이미 받는 중이면 그 진행 상황). 끊긴 곳부터 이어받는다 */
	modelDownload(variant: ModelVariant): Promise<FetchProgress>
	modelDownloadCancel(): Promise<FetchProgress>
	/** 쇼핑 페이지 글 → items 초안 (이 기기의 Qwen3.5-0.8B. 모델이 없으면 not_ready) */
	itemDraft(args: { text: string; link?: string }): Promise<ItemDraft>
	/** 웹 행동 분석: 사이트 확인 (사이트 첫 화면의 <meta name="premium-discount-site" content="<로그인 주소>">) */
	analyticSite(args: { host: string }): Promise<unknown>
	/** 웹 행동 분석: 원시 사건 받기 + 기기 구조화 (DELTA · TOMBSTONE GUARD) */
	analyticSync(args: { host: string; max_pages?: number }): Promise<unknown>
	/** 웹 행동 분석: 자연어 질의 리포트 (기간은 앱이 다시 계산, 퍼널 · 병목 · 선택 대조) */
	analyticReport(args: { host: string; query: string; lang?: string; top_k?: number }): Promise<unknown>
	/** 웹 행동 분석: 사건 지우기 (다시 받지 않는다) */
	analyticForget(args: { ids: string[] }): Promise<unknown>
}

export function makeCommands(invoke: Invoke): Commands {
	async function call<T>(cmd: string, args?: unknown): Promise<T> {
		try {
			return (await invoke(cmd, args === undefined ? undefined : { args })) as T
		} catch (e) {
			throw toCommandFailure(e)
		}
	}
	return {
		submitEvaluation: (args) => call('cmd_submit_evaluation', args),
		syncCache: (since) => call('cmd_sync_cache', { since }),
		searchLocal: (args) => call('cmd_search_local', args),
		searchRemote: (args) => call('cmd_search_remote', args),
		searchMerged: (args) => call('cmd_search_merged', args),
		flushOutbox: () => call('cmd_flush_outbox'),
		vote: (args) => call('cmd_vote', args),
		supersede: (args) => call('cmd_supersede', args),
		verdicts: (ids, refresh = false) => call('cmd_verdicts', { ids, refresh }),
		session: (verify) => call('cmd_session', { verify }),
		loginStart: () => call('cmd_login_start'),
		loginStatus: () => call('cmd_login_status'),
		logout: () => call('cmd_logout'),
		appStatus: () => call('cmd_app_status'),
		modelStatus: () => call('cmd_model_status'),
		modelDownload: (variant) => call('cmd_model_download', { variant }),
		modelDownloadCancel: () => call('cmd_model_download_cancel'),
		itemDraft: (args) => call('cmd_item_draft', args),
		analyticSite: (args) => call('cmd_analytic_site', args),
		analyticSync: (args) => call('cmd_analytic_sync', args),
		analyticReport: (args) => call('cmd_analytic_report', args),
		analyticForget: (args) => call('cmd_analytic_forget', args),
	}
}

/** Rust에서 바뀐 행을 받아 화면 캐시(Dexie)에 반영한다 (`syncFromRust`에 `cmd_sync_cache`를 연결한 것) */
export function syncCacheFromRust(db: DbLike, invoke: Invoke): Promise<SyncResult> {
	const commands = makeCommands(invoke)
	return syncFromRust(db, (since) => commands.syncCache(since))
}
