import { ethers } from 'ethers'

import { authorKey } from './projection'
import { verifiedAddress, type SessionEnv } from './auth'
import { isRateLimited, RETRY_AFTER_SECONDS, type RateLimitEnv } from './ratelimit'
// 판정 규칙 이름. 계산은 cron이 하고, 여기서는 판정 행이 없는 평가에 붙일 이름만 쓴다
import { RECIPE } from '../../cron/src/verdict'

// google/embeddinggemma-2 출력 차원 (고정값, 벡터 길이 검사에 사용). Vectorize 인덱스 차원과 같아야 한다
export const EVAL_DIMENSIONS = 768

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
const DOMAIN_RE = /^([a-z0-9-]{1,63}\.)+[a-z]{2,63}$/
// Vectorize 인덱싱 값은 64바이트 제한이므로 cc도 64자 이하로 제한
const DOMAIN_MAX_LEN = 64
// Vectorize: 메타데이터를 함께 반환할 때 topK 최대 50
const TOP_K_MAX = 50
// 멱등 키 형식 (앱은 16바이트 난수를 hex 32자리로 만든다)
const REQUEST_ID_RE = /^[A-Za-z0-9_-]{16,64}$/
// SHA-256 hex (소문자 64자)
const HASH_RE = /^[0-9a-f]{64}$/
// 원문 최대 크기 (UTF-8 바이트). 임시값
const MAX_TEXT_BYTES = 8192
const ALLOWED_ORIGINS = ['tauri://localhost', 'http://tauri.localhost']
// 판정 조회 한 번에 받는 최대 id 수 (IN 바인딩이 D1의 문장당 상한 100을 넘지 않게)
const VERDICT_IDS_MAX = 50
// 검색의 tier: 'rated' 조건. 판정이 난 평가 전부다 (unrated 제외)
const RATED_TIERS = ['premium', 'neutral', 'discount']

export interface EvalEnv extends SessionEnv, RateLimitEnv {
	DB: D1Database
	EVAL_VECTORIZE: VectorizeIndex
	// 게스트 주소용 솔트 (wrangler secret put GUEST_SALT). 필수
	GUEST_SALT: string
	// 공개 표시용 작성자 키 비밀 (wrangler secret put AUTHOR_KEY_SECRET). 필수
	AUTHOR_KEY_SECRET: string
}

export class ValidationError extends Error {}
// 같은 request_id로 다른 내용을 보낸 경우 (409)
export class ConflictError extends Error {}
// 원문과 해시가 맞지 않는 경우 (422). 저장하지 않는다
export class HashMismatchError extends Error {}

export function corsHeaders(origin: string): Record<string, string> {
	const headers: Record<string, string> = {
		// status·verdicts 조회는 GET이다. 앱 웹뷰는 세션을 x-oauth-hash / x-oauth-token 헤더로 보낸다 (auth.ts)
		'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
		'Access-Control-Allow-Headers': 'Content-Type, x-oauth-hash, x-oauth-token',
		'Vary': 'Origin',
	}
	if (ALLOWED_ORIGINS.includes(origin)) {
		headers['Access-Control-Allow-Origin'] = origin
	}
	return headers
}

function json(body: unknown, status: number, origin: string): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json', ...corsHeaders(origin) },
	})
}

function rateLimited(origin: string): Response {
	return new Response(JSON.stringify({ error: 'rate limit exceeded' }), {
		status: 429,
		headers: { 'Content-Type': 'application/json', 'Retry-After': String(RETRY_AFTER_SECONDS), ...corsHeaders(origin) },
	})
}

async function readJson(request: Request): Promise<Record<string, unknown>> {
	let body: unknown
	try {
		body = await request.json()
	} catch {
		throw new ValidationError('invalid JSON body')
	}
	if (!body || typeof body !== 'object' || Array.isArray(body)) {
		throw new ValidationError('body must be an object')
	}
	return body as Record<string, unknown>
}

// 주소: 비어 있거나 ZeroAddress면 null
function parseAddress(value: unknown, field: string): string | null {
	if (value === undefined || value === null || value === '') return null
	if (typeof value !== 'string' || !ethers.isAddress(value)) {
		throw new ValidationError(`${field} must be a valid address`)
	}
	const normalized = value.toLowerCase()
	return normalized === ethers.ZeroAddress ? null : normalized
}

// 게스트 주소: from이 없으면 IP를 솔트와 함께 해싱해 주소를 만든다.
// 솔트 없이 IP만 해싱하면 IPv4 공간이 작아 역산이 쉬우므로 GUEST_SALT는 필수다.
function guestAddress(request: Request, env: EvalEnv): string {
	if (!env.GUEST_SALT) {
		throw new Error('GUEST_SALT is not configured')
	}
	const ip = (request.headers.get('cf-connecting-ip') ?? request.headers.get('X-Real-IP') ?? '').trim()
	if (!ip) {
		throw new Error('client ip not available')
	}
	const digest = ethers.keccak256(ethers.toUtf8Bytes(`guest|${env.GUEST_SALT}|${ip}`))
	return ('0x' + digest.slice(-40)).toLowerCase()
}

export function parseDomain(value: unknown): string {
	if (typeof value !== 'string') throw new ValidationError('cc is required')
	const cc = value.trim().toLowerCase()
	if (cc.length > DOMAIN_MAX_LEN || !DOMAIN_RE.test(cc)) {
		throw new ValidationError('cc must be a registrable domain (max 64 chars)')
	}
	return cc
}

export function parseUuid(value: unknown, field: string): string {
	if (typeof value !== 'string' || !UUID_RE.test(value)) {
		throw new ValidationError(`${field} must be a UUID`)
	}
	return value.toLowerCase()
}

// 계약: 클라이언트(Rust)가 768차원, L2 정규화된 벡터를 보낸다
function parseVector(value: unknown): number[] {
	if (!Array.isArray(value) || value.length !== EVAL_DIMENSIONS) {
		throw new ValidationError(`vector must be an array of length ${EVAL_DIMENSIONS}`)
	}
	let sumSq = 0
	for (const x of value) {
		if (typeof x !== 'number' || !Number.isFinite(x)) {
			throw new ValidationError('vector must contain only finite numbers')
		}
		sumSq += x * x
	}
	if (Math.abs(Math.sqrt(sumSq) - 1) > 0.01) {
		throw new ValidationError('vector must be L2-normalized (norm ≈ 1)')
	}
	return value as number[]
}

// 원문: 해시는 보내온 문자열 그대로 계산하므로, 저장도 trim 없이 그대로 한다
function parseText(value: unknown): string {
	if (typeof value !== 'string') throw new ValidationError('text is required')
	if (value.trim() === '') throw new ValidationError('text must not be empty')
	if (new TextEncoder().encode(value).length > MAX_TEXT_BYTES) {
		throw new ValidationError(`text must be at most ${MAX_TEXT_BYTES} bytes`)
	}
	return value
}

function parseTextHash(value: unknown): string {
	if (typeof value !== 'string' || !HASH_RE.test(value)) {
		throw new ValidationError('text_hash must be 64 lowercase hex characters (SHA-256 of text)')
	}
	return value
}

async function sha256Hex(text: string): Promise<string> {
	const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))
	return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('')
}

// UUID v7 (앞 48비트가 밀리초 타임스탬프라 시간순 정렬 가능)
export function uuidv7(): string {
	const ms = Date.now()
	const b = new Uint8Array(16)
	crypto.getRandomValues(b)
	b[0] = Math.floor(ms / 2 ** 40) & 0xff
	b[1] = Math.floor(ms / 2 ** 32) & 0xff
	b[2] = Math.floor(ms / 2 ** 24) & 0xff
	b[3] = Math.floor(ms / 2 ** 16) & 0xff
	b[4] = Math.floor(ms / 2 ** 8) & 0xff
	b[5] = ms & 0xff
	b[6] = (b[6] & 0x0f) | 0x70
	b[8] = (b[8] & 0x3f) | 0x80
	const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')
	return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`
}

// 멱등 키. 대기열 방식에서는 필수다 (재시도와 중복 저장을 막는 기준)
function parseRequestId(value: unknown): string {
	if (typeof value !== 'string' || !REQUEST_ID_RE.test(value)) {
		throw new ValidationError('request_id is required (16-64 characters of A-Z, a-z, 0-9, _ or -)')
	}
	return value
}

interface Replay {
	id: string
	root: string
	ref: string | null
	from: string
	cc: string
	status: string
}

// 같은 request_id가 이미 저장되었거나 대기 중인지 확인한다. 다른 내용이면 409
async function findReplay(env: EvalEnv, requestId: string, cc: string, ref: string | null): Promise<Replay | null> {
	const stored = await env.DB.prepare(
		`SELECT "id", "root", "ref", "from", "cc" FROM evaluations WHERE "request_id" = ?1`
	).bind(requestId).first<Omit<Replay, 'status'>>()
	const found = stored
		? { ...stored, status: 'stored' }
		: await env.DB.prepare(
			`SELECT "id", "root", "ref", "from", "cc", "state" AS "status" FROM eval_queue WHERE "request_id" = ?1`
		).bind(requestId).first<Replay>()
	if (!found) return null
	if (found.cc !== cc || found.ref !== ref) {
		throw new ConflictError('request_id was already used for a different evaluation')
	}
	return found
}

// 부모는 저장된 평가 또는 대기열 항목일 수 있다
async function findParent(env: EvalEnv, ref: string): Promise<{ cc: string; root: string; state: string } | null> {
	const stored = await env.DB.prepare(
		`SELECT "cc", "root" FROM evaluations WHERE "id" = ?1`
	).bind(ref).first<{ cc: string; root: string }>()
	if (stored) return { ...stored, state: 'stored' }
	return env.DB.prepare(
		`SELECT "cc", "root", "state" FROM eval_queue WHERE "id" = ?1`
	).bind(ref).first<{ cc: string; root: string; state: string }>()
}

// POST /evaluations : 원문·해시·벡터를 대기열(eval_queue)에 넣는다.
// 저장(D1 evaluations)과 벡터 저장(Vectorize)은 cron이 욕설 검사를 통과한 뒤에 한다.
async function enqueueEvaluation(request: Request, env: EvalEnv, origin: string): Promise<Response> {
	const body = await readJson(request)
	const requestId = parseRequestId(body.request_id)
	const cc = parseDomain(body.cc)
	const text = parseText(body.text)
	const textHash = parseTextHash(body.text_hash)
	// 해시가 원문과 맞지 않으면 저장하지 않는다. 클라이언트가 알 수 있도록 422를 돌려준다
	if ((await sha256Hex(text)) !== textHash) {
		throw new HashMismatchError('text_hash does not match text')
	}
	// from 우선순위: 검증된 세션 주소 > 클라이언트 값(임시) > 게스트 주소
	// verifiedAddress는 읽기 전용 S3 자격 증명이 설정된 경우에만 값을 돌려준다 (auth.ts 참고)
	const from = (await verifiedAddress(request, env)) ?? parseAddress(body.from, 'from') ?? guestAddress(request, env)
	const to = parseAddress(body.to, 'to')
	// bcc: 콘텐츠 해시 주소(앱이 hashId로 만든 0x+40자리). 없거나 ZeroAddress면 NULL
	const bcc = parseAddress(body.bcc, 'bcc')
	const vector = parseVector(body.vector)
	const ref = body.ref === undefined || body.ref === null ? null : parseUuid(body.ref, 'ref')

	// 같은 request_id: 이미 저장되었거나 대기 중이면 처음 결과를 돌려준다 (앱의 재시도)
	const replay = await findReplay(env, requestId, cc, ref)
	if (replay) return json(replayBody(replay), 200, origin)

	// 단일 부모. 평가는 추가만 하므로 순환 참조가 생기지 않음. 부모와 같은 cc만 허용
	// 대기열에서 거부되었거나 실패한 부모에는 답글을 달 수 없다
	let root: string | null = null
	if (ref) {
		const parent = await findParent(env, ref)
		if (!parent) throw new ValidationError('parent evaluation not found')
		if (parent.cc !== cc) throw new ValidationError('reply must use the same cc as its parent')
		if (parent.state === 'rejected' || parent.state === 'failed') {
			throw new ValidationError('parent evaluation was rejected')
		}
		root = parent.root
	}

	const id = uuidv7()
	root = root ?? id
	const now = Date.now()

	try {
		await env.DB.prepare(
			`INSERT INTO eval_queue ("id", "request_id", "from", "to", "cc", "ref", "root", "text", "text_hash", "vector", "state", "attempts", "created_at", "updated_at", "bcc")
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', 0, ?11, ?11, ?12)`
		)
			.bind(id, requestId, from, to, cc, ref, root, text, textHash, new Float32Array(vector).buffer, now, bcc)
			.run()
	} catch (err) {
		// 같은 request_id가 거의 동시에 들어와 유니크 제약에 걸렸다면, 먼저 들어간 항목을 돌려준다
		const again = await findReplay(env, requestId, cc, ref)
		if (again) return json(replayBody(again), 200, origin)
		throw err
	}

	return json({ id, root, ref, from, status: 'pending' }, 202, origin)
}

function replayBody(r: Replay) {
	return { id: r.id, root: r.root, ref: r.ref, from: r.from, status: r.status }
}

interface VoteRecord {
	eval_id: string
	voter: string
	// session(확인된 로그인) | guest
	voter_kind: string
	kind: string
	value: number
}

// 1 추천, -1 비추천, 0 철회. 숫자만 받는다 (true나 "1"은 받지 않는다)
function parseVoteValue(value: unknown): number {
	if (value !== 1 && value !== -1 && value !== 0) throw new ValidationError('value must be 1, -1 or 0')
	return value
}

// 가중치는 cron이 정한다 (verdict.ts PARAMS.weights). 생략하거나 null이면 vote (POST /evaluations의 ref처럼)
function parseVoteKind(value: unknown): string {
	if (value === undefined || value === null) return 'vote'
	if (value !== 'vote' && value !== 'vote_outcome') throw new ValidationError('kind must be vote or vote_outcome')
	return value
}

// POST /evaluations/votes : 저장이 끝난 평가에 표를 남긴다. 등급 계산은 cron이 한다 (verdict_dirty 표시)
async function castVote(request: Request, env: EvalEnv, origin: string): Promise<Response> {
	const body = await readJson(request)
	const requestId = parseRequestId(body.request_id)
	const id = parseUuid(body.id, 'id')
	const value = parseVoteValue(body.value)
	const kind = parseVoteKind(body.kind)

	// 앱은 이 404를 (메시지까지 보고) 영구 거절로 처리한다. 대기열에만 있는 평가도 아직 투표할 수 없다
	const evaluation = await env.DB.prepare(`SELECT "from" FROM evaluations WHERE "id" = ?1`).bind(id).first<{ from: string }>()
	if (!evaluation) return json({ error: 'evaluation not found' }, 404, origin)

	// 투표자는 서버가 정한다: 검증된 세션 주소, 없으면 게스트 주소. 클라이언트가 보낸 from은 쓰지 않는다 (남의 주소로 투표하지 못하게)
	const session = await verifiedAddress(request, env)
	const vote: VoteRecord = {
		eval_id: id,
		voter: session ?? guestAddress(request, env),
		voter_kind: session ? 'session' : 'guest',
		kind,
		value,
	}

	// 같은 request_id: 처음 응답을 그대로 돌려준다 (앱의 재시도). 내용이 다르면 409
	const replay = await findVote(env, requestId, vote)
	if (replay) return replayVote(env, replay, evaluation.from, origin)

	const now = Date.now()
	try {
		await env.DB.prepare(
			`INSERT INTO eval_votes ("id", "request_id", "eval_id", "voter", "voter_kind", "kind", "value", "created_at")
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)`
		)
			.bind(uuidv7(), requestId, id, vote.voter, vote.voter_kind, kind, value, now)
			.run()
	} catch (err) {
		// 같은 request_id가 거의 동시에 들어와 유니크 제약에 걸렸다면, 먼저 들어간 표로 답한다
		const again = await findVote(env, requestId, vote)
		if (again) return replayVote(env, again, evaluation.from, origin)
		throw err
	}
	// 표를 넣은 뒤에 표시한다. 먼저 표시하면 cron이 표시를 읽고 지우는 사이에 들어온 표를 놓칠 수 있다
	await markDirty(env, id, now)
	return json(voteBody(vote, evaluation.from), 202, origin)
}

// 같은 request_id로 저장된 표. 평가·종류·값 중 하나라도 다르면 409.
// 투표자는 비교하지 않는다: 재시도 사이에 게스트 IP가 바뀌거나 세션 확인(S3)이 잠깐 실패해 게스트로 떨어져도 같은 요청이다.
// 앱은 409를 영구 거절로 처리하므로, 투표자까지 비교하면 정상 재시도가 거절로 남는다. 답은 처음 저장한 표(그때의 투표자)로 한다
async function findVote(env: EvalEnv, requestId: string, vote: VoteRecord): Promise<VoteRecord | null> {
	const found = await env.DB.prepare(
		`SELECT "eval_id", "voter", "voter_kind", "kind", "value" FROM eval_votes WHERE "request_id" = ?1`
	).bind(requestId).first<VoteRecord>()
	if (!found) return null
	if (found.eval_id !== vote.eval_id || found.kind !== vote.kind || found.value !== vote.value) {
		throw new ConflictError('request_id was already used for a different vote')
	}
	return found
}

// 재시도에 답할 때도 다시 표시한다. 표와 표시는 따로 쓰므로(batch 없음) 처음 요청이 표만 넣고 실패했을 수 있다
async function replayVote(env: EvalEnv, vote: VoteRecord, author: string, origin: string): Promise<Response> {
	await markDirty(env, vote.eval_id, Date.now())
	return json(voteBody(vote, author), 200, origin)
}

// cron이 이 평가의 판정을 다시 계산하도록 표시한다. 이미 있으면 시각만 늦춘다 (cron은 읽어 둔 시각 이하일 때만 지운다)
async function markDirty(env: EvalEnv, evalId: string, at: number): Promise<void> {
	await env.DB.prepare(`INSERT OR REPLACE INTO verdict_dirty ("eval_id", "at") VALUES (?1, ?2)`).bind(evalId, at).run()
}

// counted: 이 표가 등급 계산에 들어가는지. 게스트와 작성자 자신의 표는 기록만 하고 세지 않는다 (cron이 가중치 0으로 센다)
function voteBody(v: VoteRecord, author: string) {
	return { id: v.eval_id, value: v.value, kind: v.kind, counted: v.voter_kind === 'session' && v.voter !== author, status: 'accepted' }
}

// 검색 범위
// - app: POST /evaluations/search (앱용). to 필터 허용, 응답에 to 포함
// - public: POST /public/evaluations/search (공개 웹용). to 필터 거부, 응답에서 to 제외
type SearchScope = 'app' | 'public'

// POST .../search : cc, root, ref(, to) 범위 필터를 검색 전에 적용
async function searchEvaluations(request: Request, env: EvalEnv, origin: string, scope: SearchScope): Promise<Response> {
	const body = await readJson(request)
	const vector = parseVector(body.vector)

	const topK = body.topK === undefined ? 10 : body.topK
	if (typeof topK !== 'number' || !Number.isInteger(topK) || topK < 1 || topK > TOP_K_MAX) {
		throw new ValidationError(`topK must be an integer between 1 and ${TOP_K_MAX}`)
	}

	const filter: Record<string, string> = {}
	if (body.cc !== undefined) filter.cc = parseDomain(body.cc)
	if (body.root !== undefined) filter.root = parseUuid(body.root, 'root')
	if (body.ref !== undefined) filter.ref = parseUuid(body.ref, 'ref')
	// 공개 검색에서는 주소(to) 조건을 허용하지 않는다 (주소 기반 추론 방지)
	if (body.to !== undefined) {
		if (scope === 'public') throw new ValidationError('to filter is not available in public search')
		const to = parseAddress(body.to, 'to')
		if (!to) throw new ValidationError('to must be a non-zero address')
		filter.to = to
	}
	// 등급 조건. 판정은 Vectorize 메타데이터가 아니라 D1(eval_verdicts)에 있어서 받아 온 뒤에 거른다
	const tiers = body.tier === undefined ? null : parseTierFilter(body.tier)

	const result = await env.EVAL_VECTORIZE.query(vector, {
		// 거르고도 topK가 차도록 등급 조건이 있으면 세 배까지 받아 온다
		topK: tiers ? Math.min(TOP_K_MAX, topK * 3) : topK,
		returnMetadata: true,
		...(Object.keys(filter).length ? { filter } : {}),
	})
	const verdicts = await verdictSummaries(env, result.matches.map((m) => m.id))
	const matches = tiers ? result.matches.filter((m) => tiers.includes(verdicts.get(m.id)?.tier ?? 'unrated')) : result.matches

	return json(
		{
			matches: matches.slice(0, topK).map((m) => {
				const md = (m.metadata ?? {}) as Record<string, unknown>
				const base = {
					id: m.id,
					score: m.score,
					cc: md.cc ?? null,
					root: md.root ?? null,
					ref: md.ref ?? null,
				}
				// 판정 행이 없으면 null (아직 판정 전)
				const verdict = verdicts.get(m.id) ?? null
				// 앱 응답에는 to를 포함하고, 공개 응답에서는 제외한다
				return scope === 'app' ? { ...base, to: md.to ?? null, verdict } : { ...base, verdict }
			}),
		},
		200,
		origin
	)
}

// premium | discount | rated (rated = premium, neutral, discount)
function parseTierFilter(value: unknown): string[] {
	if (value === 'premium' || value === 'discount') return [value]
	if (value === 'rated') return RATED_TIERS
	throw new ValidationError('tier must be premium, discount or rated')
}

export interface VerdictSummary {
	tier: string
	grade: number | null
	rank: number | null
	evidence: number
}

// 검색 결과와 그래프 노드에 붙이는 판정 요약. ids는 50개 이하로만 부른다 (D1은 문장당 바인딩이 100개까지)
export async function verdictSummaries(env: EvalEnv, ids: string[]): Promise<Map<string, VerdictSummary>> {
	const out = new Map<string, VerdictSummary>()
	if (ids.length === 0) return out
	const rs = await env.DB.prepare(
		`SELECT "eval_id", "tier", "grade", "rank", "evidence" FROM eval_verdicts WHERE "eval_id" IN (${binds(ids.length)})`
	).bind(...ids).all<VerdictSummary & { eval_id: string }>()
	for (const r of rs.results) out.set(r.eval_id, { tier: r.tier, grade: r.grade, rank: r.rank, evidence: r.evidence })
	return out
}

// IN 목록용 자리표시자 ?1, ?2, ...
function binds(n: number): string {
	return Array.from({ length: n }, (_, i) => `?${i + 1}`).join(', ')
}

// GET /evaluations/status?request_id=... : 앱이 자기 요청의 처리 결과를 확인한다.
// request_id를 아는 쪽만 조회할 수 있다 (무작위 16바이트 이상). 주소는 돌려주지 않는다.
async function readStatus(request: Request, env: EvalEnv, origin: string): Promise<Response> {
	const requestId = parseRequestId(new URL(request.url).searchParams.get('request_id'))
	const stored = await env.DB.prepare(
		`SELECT "id", "root", "ref" FROM evaluations WHERE "request_id" = ?1`
	).bind(requestId).first<{ id: string; root: string; ref: string | null }>()
	if (stored) {
		return json({ id: stored.id, root: stored.root, ref: stored.ref, status: 'stored' }, 200, origin)
	}
	const queued = await env.DB.prepare(
		`SELECT "id", "root", "ref", "state", "last_error" FROM eval_queue WHERE "request_id" = ?1`
	).bind(requestId).first<{ id: string; root: string; ref: string | null; state: string; last_error: string | null }>()
	if (queued) {
		const reason = queued.state === 'rejected' || queued.state === 'failed' ? queued.last_error : null
		return json({ id: queued.id, root: queued.root, ref: queued.ref, status: queued.state, reason }, 200, origin)
	}
	return json({ error: 'not found' }, 404, origin)
}

interface VerdictRow {
	eval_id: string
	tier: string
	grade: number | null
	rank: number | null
	value: number
	evidence: number
	up: number
	down: number
	level: string | null
	// JSON 배열 문자열
	reasons: string
	recipe: string
	updated_at: number
}

// GET /evaluations/verdicts?ids=a,b,c : 평가별 판정을 요청 순서대로 돌려준다. 투표자는 돌려주지 않는다
async function readVerdicts(request: Request, env: EvalEnv, origin: string): Promise<Response> {
	const ids = parseIdList(new URL(request.url).searchParams.get('ids'))
	const rs = await env.DB.prepare(
		`SELECT "eval_id", "tier", "grade", "rank", "value", "evidence", "up", "down", "level", "reasons", "recipe", "updated_at"
		 FROM eval_verdicts WHERE "eval_id" IN (${binds(ids.length)})`
	).bind(...ids).all<VerdictRow>()
	const rows = new Map(rs.results.map((r) => [r.eval_id, r]))
	const verdicts = ids.map((id) => {
		const r = rows.get(id)
		// 판정 행이 없다: 표가 없거나 cron이 아직 계산하지 않았다
		if (!r) {
			return { id, tier: 'unrated', grade: null, rank: null, value: 0, evidence: 0, up: 0, down: 0, level: null, reasons: ['low_evidence', 'no_baseline'], recipe: RECIPE, updated_at: null }
		}
		return {
			id,
			tier: r.tier,
			grade: r.grade,
			rank: r.rank,
			value: r.value,
			evidence: r.evidence,
			up: r.up,
			down: r.down,
			level: r.level,
			reasons: JSON.parse(r.reasons) as string[],
			recipe: r.recipe,
			updated_at: r.updated_at,
		}
	})
	return json({ verdicts }, 200, origin)
}

// 쉼표로 구분한 UUID 1~50개 (중복 포함 개수). 중복은 처음 나온 자리만 남긴다
function parseIdList(raw: string | null): string[] {
	const ids = raw ? raw.split(',').map((s) => s.trim().toLowerCase()) : []
	if (ids.length === 0 || ids.length > VERDICT_IDS_MAX || !ids.every((id) => UUID_RE.test(id))) {
		throw new ValidationError(`ids must be 1-${VERDICT_IDS_MAX} comma-separated UUIDs`)
	}
	return [...new Set(ids)]
}

// 평가 저장/검색/상태/투표/판정 경로만 처리. 다른 경로는 null을 반환해 다음 핸들러로 넘김
export async function handleEvaluations(request: Request, env: EvalEnv): Promise<Response | null> {
	const { pathname } = new URL(request.url)
	const isAppSearch = pathname === '/evaluations/search'
	const isPublicSearch = pathname === '/public/evaluations/search'
	const isStatus = pathname === '/evaluations/status'
	const isVotes = pathname === '/evaluations/votes'
	const isVerdicts = pathname === '/evaluations/verdicts'
	if (pathname !== '/evaluations' && !isAppSearch && !isPublicSearch && !isStatus && !isVotes && !isVerdicts) return null

	const origin = request.headers.get('Origin') ?? ''

	if (request.method === 'OPTIONS') {
		return new Response(null, { status: 204, headers: corsHeaders(origin) })
	}
	if (request.method !== (isStatus || isVerdicts ? 'GET' : 'POST')) {
		return json({ error: 'method not allowed' }, 405, origin)
	}

	// 요청 제한 (ratelimit.ts). 저장과 투표는 저장 제한기, 검색과 판정 조회는 읽기 제한기를 쓴다.
	// status 조회는 제한하지 않는다: D1 한 건 조회뿐이고, 앱이 대기 중인 항목을 한꺼번에 확인한다
	if (!isStatus) {
		const limiter = pathname === '/evaluations' || isVotes ? env.EVAL_WRITE_LIMITER : env.EVAL_READ_LIMITER
		if (await isRateLimited(request, limiter)) return rateLimited(origin)
	}

	try {
		if (isStatus) return await readStatus(request, env, origin)
		if (isVerdicts) return await readVerdicts(request, env, origin)
		if (isVotes) return await castVote(request, env, origin)
		if (pathname === '/evaluations') return await enqueueEvaluation(request, env, origin)
		return await searchEvaluations(request, env, origin, isAppSearch ? 'app' : 'public')
	} catch (err) {
		if (err instanceof ValidationError) return json({ error: err.message }, 400, origin)
		if (err instanceof HashMismatchError) return json({ error: err.message }, 422, origin)
		if (err instanceof ConflictError) return json({ error: err.message }, 409, origin)
		console.log('evaluations error', err)
		return json({ error: 'internal error' }, 500, origin)
	}
}
