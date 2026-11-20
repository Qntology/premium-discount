// 웹 행동 분석 수집 (tracker-logis-center 의 Worker 를 옮김, 2026-11-18)
//
// 사이트 주인이 자기 사이트에 수집 스크립트(web/www/static/js/tracker.js)를 심으면, 방문자의 사건(web-event/1)이
// POST /analytic/events 로 들어와 사이트(cc = hashId(host))마다 쌓인다. 구조화(속성 없는 PUG → 요약 → embeddinggemma 임베딩)와
// 퍼널 · 마찰 판정은 서버가 아니라 사이트 주인의 앱이 기기에서 한다(crates/premium-discount-analytic, apps/core/src/analytic.rs).
// 서버는 받은 그대로를 gzip 해 두고(updated_at = 0: 원시), 사이트 주인에게만 돌려준다.
//
// tracker-logis-center 와 다른 점:
//   - 받는 것: outerHTML 대신 속성 없는 PUG + 셀렉터용 조상 모양. 주소는 쿼리를 빼고, 글의 이메일 · 숫자 네 개 이상은 '#'.
//     서버가 한 번 더 거른다(sanitizeEvent). 방문자 쿠키(hash · token, S3 계정) · IP · 지역 정보는 받지도 남기지도 않는다
//   - 사이트 확인: oauth.network 의 client_id/secret 대신 지갑 서명 + 사이트 첫 화면의
//     <meta name="premium-discount-site" content="<지갑 주소>">. 읽기는 확인된 주인만
//   - 원 Worker 의 결함: pageId = hashId(cookies.cc + pathname) 를 cookies.cc 를 정하기 전에 계산해 'undefined/…' 로
//     해싱했고, GET 은 href 만 바꾸면 누구의 사이트든 1000건을 돌려줬다
//   - 사건 종류: click · hover · change 에 enter · scroll · submit · back · leave (마찰 신호)를 더했다
import { ethers } from 'ethers'
import { ungzip } from 'pako'

import { blobToBase64, decodeBytes, encodeBlob } from './blob'
import type { EvalEnv } from './evaluations'
import { readSessionCredentials, verifiedAddress, verifyWalletRequest } from './auth'
import { isRateLimited, RETRY_AFTER_SECONDS } from './ratelimit'

export const ANALYTIC_EVENTS_PATH = '/analytic/events'
export const ANALYTIC_SITES_PATH = '/analytic/sites'
export const EVENT_SCHEMA = 'web-event/1'
export const SITE_META_NAME = 'premium-discount-site'

const EVENT_TYPES = ['enter', 'click', 'hover', 'change', 'scroll', 'submit', 'back', 'leave'] as const
type EventType = (typeof EVENT_TYPES)[number]
const MAX_BODY_BYTES = 256 * 1024
const MAX_EVENTS_PER_POST = 20
const MAX_PUG_CHARS = 4000
const MAX_RELATED = 100
const MAX_CHAIN = 5
const MAX_SID = 64
const MAX_LIST = 500
const DEFAULT_LIST = 200
const MAX_SITE_PAGE_BYTES = 512 * 1024
const ADDRESS_RE = /^0x[0-9a-f]{40}$/
const WALLET_HEADERS = ['x-pd-address', 'x-pd-time', 'x-pd-signature']

export interface AnalyticEnv extends EvalEnv {
	// 수집 전용 요청 제한 (IP 마다). 없으면 평가 쓰기 제한을 함께 쓴다
	ANALYTIC_WRITE_LIMITER?: RateLimit
}

export type Fetcher = (url: string, init?: RequestInit) => Promise<Response>

export interface DomNode {
	tag: string
	id?: string | null
	classes?: string[]
	attrs?: [string, string][]
	nth_of_type?: number
}

export interface WebElement {
	chain: DomNode[]
	pug: string
}

export interface WebEvent {
	type: EventType
	href: string
	sid: string
	seq: number
	t_ms: number
	target?: WebElement
	item?: WebElement
	related?: WebElement[]
	rage?: boolean
	dwell_ms?: number
	flips?: number
	field?: string
	dirty?: number
}

const hashIds = new Map<string, string>()
export function hashId(text: string): string {
	const hit = hashIds.get(text)
	if (hit) return hit
	const id = ethers.computeAddress(ethers.hashMessage(text)).toLowerCase()
	if (hashIds.size > 10_000) hashIds.clear()
	hashIds.set(text, id)
	return id
}

// 주소에서 쿼리와 문서 안 위치를 뺀다. 해시 라우팅(#/cart, #!/cart)은 남긴다. http(s) 가 아니면 null (Rust event::clean_href 와 같다)
export function cleanHref(href: string): string | null {
	const h = String(href ?? '').trim()
	const sep = h.indexOf('://')
	if (sep <= 0) return null
	const scheme = h.slice(0, sep).toLowerCase()
	if (scheme !== 'http' && scheme !== 'https') return null
	const rest = h.slice(sep + 3)
	const hashAt = rest.indexOf('#')
	const fragment = hashAt >= 0 ? rest.slice(hashAt + 1) : null
	const route = fragment !== null && (fragment.startsWith('/') || fragment.startsWith('!/')) ? fragment : null
	const cut = rest.search(/[?#]/)
	const base = cut >= 0 ? rest.slice(0, cut) : rest
	if ((base.split('/')[0] ?? '') === '') return null
	if (route !== null) {
		const q = route.indexOf('?')
		return `${scheme}://${base}#${q >= 0 ? route.slice(0, q) : route}`
	}
	return `${scheme}://${base}`
}

// 주소의 호스트 (소문자, 사용자 정보 · 포트 없이)
export function hostOf(href: string): string | null {
	const sep = href.indexOf('://')
	if (sep <= 0) return null
	const authority = href.slice(sep + 3).split(/[/?#]/)[0] ?? ''
	const host = (authority.split('@').pop() ?? '').split(':')[0]?.toLowerCase() ?? ''
	return host === '' ? null : host
}

function looksPrivate(word: string): boolean {
	const at = word.indexOf('@')
	const email = at > 0 && word.slice(at + 1).includes('.')
	return email || (word.match(/[0-9]/g)?.length ?? 0) >= 4
}

// 글에서 개인정보로 보이는 낱말(이메일, 숫자 네 개 이상)을 '#' 로 바꾼다. 줄 구조 · 들여쓰기는 그대로 (Rust event::scrub_text 와 같다)
export function scrubText(text: string): string {
	return String(text ?? '')
		.split('\n')
		.map((line) => {
			const indent = line.match(/^ */)?.[0] ?? ''
			return indent + line.slice(indent.length).split(' ').map((w) => (looksPrivate(w) ? '#' : w)).join(' ')
		})
		.join('\n')
}

function cleanString(v: unknown, max: number): string | undefined {
	return typeof v === 'string' && v.length > 0 ? v.slice(0, max) : undefined
}

function cleanNode(v: unknown): DomNode | null {
	if (!v || typeof v !== 'object') return null
	const n = v as Record<string, unknown>
	const tag = cleanString(n.tag, 40)
	if (!tag || !/^[a-zA-Z0-9-]+$/.test(tag)) return null
	const out: DomNode = { tag: tag.toLowerCase() }
	const id = cleanString(n.id, 80)
	if (id) out.id = id
	if (Array.isArray(n.classes)) out.classes = n.classes.filter((c): c is string => typeof c === 'string' && c.length <= 80).slice(0, 8)
	if (Array.isArray(n.attrs)) {
		out.attrs = n.attrs
			.filter((a): a is [string, string] => Array.isArray(a) && typeof a[0] === 'string' && typeof a[1] === 'string')
			.map(([k, val]) => [k.slice(0, 40), val.slice(0, 80)] as [string, string])
			.slice(0, 6)
	}
	if (typeof n.nth_of_type === 'number' && Number.isInteger(n.nth_of_type) && n.nth_of_type > 0) out.nth_of_type = Math.min(n.nth_of_type, 10_000)
	return out
}

function cleanElement(v: unknown): WebElement | null {
	if (!v || typeof v !== 'object') return null
	const e = v as Record<string, unknown>
	const chain = Array.isArray(e.chain) ? e.chain.slice(0, MAX_CHAIN).map(cleanNode).filter((n): n is DomNode => n !== null) : []
	const pug = typeof e.pug === 'string' ? scrubText(e.pug).slice(0, MAX_PUG_CHARS) : ''
	if (chain.length === 0 && pug.trim() === '') return null
	return { chain, pug }
}

function count(v: unknown, max: number): number | undefined {
	return typeof v === 'number' && Number.isFinite(v) && v >= 0 ? Math.min(Math.floor(v), max) : undefined
}

// 받은 사건 하나를 검사하고 다듬는다. 모양이 틀리면 null
export function sanitizeEvent(raw: unknown): WebEvent | null {
	if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return null
	const e = raw as Record<string, unknown>
	if (typeof e.type !== 'string' || !(EVENT_TYPES as readonly string[]).includes(e.type)) return null
	const href = cleanHref(String(e.href ?? ''))
	const sid = cleanString(e.sid, MAX_SID)
	if (!href || !sid) return null
	const out: WebEvent = { type: e.type as EventType, href, sid, seq: count(e.seq, 1e9) ?? 0, t_ms: count(e.t_ms, 864e5) ?? 0 }
	const target = cleanElement(e.target)
	if (target) out.target = target
	// 누른 요소가 속한 목록 항목 (형제와 같은 단위)
	const item = cleanElement(e.item)
	if (item) out.item = item
	if (Array.isArray(e.related)) {
		const related = e.related.slice(0, MAX_RELATED).map(cleanElement).filter((x): x is WebElement => x !== null)
		if (related.length > 0) out.related = related
	}
	if (e.rage === true) out.rage = true
	const dwell = count(e.dwell_ms, 3_600_000)
	if (dwell !== undefined) out.dwell_ms = dwell
	const flips = count(e.flips, 1000)
	if (flips !== undefined) out.flips = flips
	const field = cleanString(e.field, 20)
	if (field && /^[a-z-]+$/.test(field)) out.field = field
	const dirty = count(e.dirty, 1000)
	if (dirty !== undefined) out.dirty = dirty
	return out
}

function cors(origin: string, owner: boolean): Record<string, string> {
	// 수집은 어느 사이트에서나 온다(자격 증명 없음). 주인 읽기 · 등록은 지갑 헤더를 받는다
	return {
		'Access-Control-Allow-Origin': owner ? origin || '*' : '*',
		'Access-Control-Allow-Methods': owner ? 'GET, POST, DELETE, OPTIONS' : 'POST, OPTIONS',
		'Access-Control-Allow-Headers': owner ? 'Content-Type, x-oauth-hash, x-oauth-token, ' + WALLET_HEADERS.join(', ') : 'Content-Type, Content-Encoding',
		'Access-Control-Max-Age': '86400',
		Vary: 'Origin',
	}
}

function reply(body: unknown, status: number, origin: string, owner: boolean, extra: Record<string, string> = {}): Response {
	return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...cors(origin, owner), ...extra } })
}

function readBody(bytes: Uint8Array, encoding: string | null): unknown {
	const text = encoding && encoding.toLowerCase().includes('gzip') ? new TextDecoder().decode(ungzip(bytes)) : new TextDecoder().decode(bytes)
	return JSON.parse(text)
}

async function collect(request: Request, env: AnalyticEnv, nowMs: number, origin: string): Promise<Response> {
	if (await isRateLimited(request, env.ANALYTIC_WRITE_LIMITER ?? env.EVAL_WRITE_LIMITER)) {
		return reply({ error: 'rate limit exceeded' }, 429, origin, false, { 'Retry-After': String(RETRY_AFTER_SECONDS) })
	}
	if (Number(request.headers.get('Content-Length') ?? 0) > MAX_BODY_BYTES) return reply({ error: 'body too large' }, 413, origin, false)
	const bytes = new Uint8Array(await request.arrayBuffer())
	if (bytes.length > MAX_BODY_BYTES) return reply({ error: 'body too large' }, 413, origin, false)
	let parsed: unknown
	try {
		parsed = readBody(bytes, request.headers.get('Content-Encoding'))
	} catch {
		return reply({ error: 'invalid JSON body' }, 400, origin, false)
	}
	const list = Array.isArray(parsed) ? parsed : parsed && typeof parsed === 'object' && Array.isArray((parsed as { events?: unknown }).events) ? (parsed as { events: unknown[] }).events : [parsed]
	if (list.length === 0 || list.length > MAX_EVENTS_PER_POST) return reply({ error: `send 1 to ${MAX_EVENTS_PER_POST} events` }, 400, origin, false)
	const events = list.map(sanitizeEvent)
	if (events.some((e) => e === null)) return reply({ error: `every event must be ${EVENT_SCHEMA}` }, 400, origin, false)
	// 브라우저가 붙인 Origin 의 호스트와 사건 주소의 호스트가 다르면 남의 사이트 사건이다
	const originHost = origin ? hostOf(origin) : null
	const statements = []
	for (const e of events as WebEvent[]) {
		const host = hostOf(e.href) as string
		if (originHost && originHost !== host) return reply({ error: 'event host does not match the page origin' }, 403, origin, false)
		const cc = hashId(host)
		const { sid, ...data } = e
		statements.push(
			env.DB.prepare(
				`INSERT INTO web_events ("id","cc","session","type","seq","data","created_at","updated_at") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)`
			).bind(crypto.randomUUID(), cc, hashId(cc + sid), e.type, e.seq, encodeBlob({ schema: EVENT_SCHEMA, ...data }).buffer, nowMs)
		)
	}
	await env.DB.batch(statements)
	return reply({ stored: statements.length }, 200, origin, false)
}

// 사이트 첫 화면의 <meta name="premium-discount-site" content="…"> 값들 (쉼표로 여럿)
export function siteMetaOwners(html: string): string[] {
	const out: string[] = []
	for (const m of html.matchAll(/<meta\b[^>]*>/gi)) {
		const tag = m[0]
		const name = tag.match(/\bname\s*=\s*["']?([^"'\s>]+)/i)?.[1]
		if (!name || name.toLowerCase() !== SITE_META_NAME) continue
		const content = tag.match(/\bcontent\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/i)
		const value = content?.[1] ?? content?.[2] ?? content?.[3] ?? ''
		for (const a of value.split(',')) {
			const addr = a.trim().toLowerCase()
			if (ADDRESS_RE.test(addr)) out.push(addr)
		}
	}
	return out
}

function normalizeHost(raw: unknown): string | null {
	if (typeof raw !== 'string') return null
	const h = raw.includes('://') ? hostOf(raw.trim()) : hostOf('https://' + raw.trim())
	return h && /^[a-z0-9.-]+$/.test(h) && h.includes('.') ? h : null
}

async function readLimited(res: Response, max: number): Promise<string> {
	const reader = res.body?.getReader()
	if (!reader) return ''
	const chunks: Uint8Array[] = []
	let total = 0
	while (total < max) {
		const { done, value } = await reader.read()
		if (done || !value) break
		chunks.push(value)
		total += value.length
	}
	await reader.cancel().catch(() => undefined)
	const all = new Uint8Array(Math.min(total, max))
	let at = 0
	for (const c of chunks) {
		const take = Math.min(c.length, all.length - at)
		all.set(c.subarray(0, take), at)
		at += take
	}
	return new TextDecoder().decode(all)
}

async function owns(db: D1Database, cc: string, owner: string): Promise<boolean> {
	const row = await db.prepare(`SELECT 1 AS ok FROM analytic_sites WHERE "cc" = ?1 AND "owner" = ?2`).bind(cc, owner).first<{ ok: number }>()
	return row !== null
}

async function sites(request: Request, env: AnalyticEnv, nowMs: number, origin: string, fetcher: Fetcher, body: Uint8Array, owner: string): Promise<Response> {
	const url = new URL(request.url)
	if (request.method === 'GET') {
		const { results } = await env.DB.prepare(`SELECT "host", "cc", "verified_at" FROM analytic_sites WHERE "owner" = ?1 ORDER BY "host"`).bind(owner).all()
		return reply({ owner, sites: results }, 200, origin, true)
	}
	if (request.method === 'DELETE') {
		const host = normalizeHost(url.searchParams.get('host'))
		if (!host) return reply({ error: 'host is required' }, 400, origin, true)
		const res = await env.DB.prepare(`DELETE FROM analytic_sites WHERE "cc" = ?1 AND "owner" = ?2`).bind(hashId(host), owner).run()
		return reply({ host, deleted: Number(res.meta?.changes ?? 0) }, 200, origin, true)
	}
	let host: string | null = null
	try {
		host = normalizeHost((JSON.parse(new TextDecoder().decode(body)) as { host?: unknown }).host)
	} catch {
		return reply({ error: 'invalid JSON body' }, 400, origin, true)
	}
	if (!host) return reply({ error: 'host must be a domain name' }, 400, origin, true)
	const expected = `<meta name="${SITE_META_NAME}" content="${owner}">`
	let html = ''
	try {
		const res = await fetcher(`https://${host}/`, { redirect: 'follow', headers: { Accept: 'text/html' } })
		if (!res.ok) return reply({ error: `https://${host}/ answered ${res.status}`, expected }, 403, origin, true)
		html = await readLimited(res, MAX_SITE_PAGE_BYTES)
	} catch {
		return reply({ error: `could not fetch https://${host}/`, expected }, 403, origin, true)
	}
	if (!siteMetaOwners(html).includes(owner)) return reply({ error: 'the site does not name this wallet', expected }, 403, origin, true)
	const cc = hashId(host)
	await env.DB.prepare(
		`INSERT INTO analytic_sites ("cc", "host", "owner", "verified_at") VALUES (?1, ?2, ?3, ?4)
		 ON CONFLICT ("cc", "owner") DO UPDATE SET "verified_at" = excluded."verified_at"`
	).bind(cc, host, owner, nowMs).run()
	return reply({ host, cc, owner, verified_at: nowMs }, 200, origin, true)
}

async function events(request: Request, env: AnalyticEnv, origin: string, owner: string): Promise<Response> {
	const url = new URL(request.url)
	const host = normalizeHost(url.searchParams.get('host'))
	if (!host) return reply({ error: 'host is required' }, 400, origin, true)
	const cc = hashId(host)
	if (!(await owns(env.DB, cc, owner))) return reply({ error: 'verify the site first (POST /analytic/sites)' }, 403, origin, true)
	if (request.method === 'DELETE') {
		const before = Number(url.searchParams.get('before') ?? NaN)
		if (!Number.isFinite(before) || before <= 0) return reply({ error: 'before (epoch ms) is required' }, 400, origin, true)
		const res = await env.DB.prepare(`DELETE FROM web_events WHERE "cc" = ?1 AND "created_at" < ?2`).bind(cc, before).run()
		return reply({ host, deleted: Number(res.meta?.changes ?? 0) }, 200, origin, true)
	}
	const since = Math.max(0, Number(url.searchParams.get('since') ?? 0) || 0)
	const after = url.searchParams.get('after_id') ?? ''
	const limit = Math.min(MAX_LIST, Math.max(1, Number(url.searchParams.get('limit') ?? DEFAULT_LIST) || DEFAULT_LIST))
	// (created_at, id) 커서: 같은 시각에 여러 행이 있어도 잘린 곳부터 이어 읽는다
	const { results } = await env.DB.prepare(
		`SELECT "id", "type", "session", "seq", "data", "created_at", "updated_at" FROM web_events
		 WHERE "cc" = ?1 AND ("created_at" > ?2 OR ("created_at" = ?2 AND "id" > ?3))
		 ORDER BY "created_at", "id" LIMIT ?4`
	).bind(cc, since, after, limit).all<Record<string, unknown>>()
	const rows = results.map((r) => ({ ...r, data: blobToBase64(r.data) }))
	const last = rows[rows.length - 1]
	return reply(
		{ host, cc, rows, next: last ? { since: last.created_at, after_id: last.id } : null, more: rows.length === limit },
		200,
		origin,
		true
	)
}

export async function handleAnalytic(request: Request, env: AnalyticEnv, nowMs: number = Date.now(), fetcher: Fetcher = fetch): Promise<Response | null> {
	const url = new URL(request.url)
	if (url.pathname !== ANALYTIC_EVENTS_PATH && url.pathname !== ANALYTIC_SITES_PATH) return null
	const origin = request.headers.get('Origin') ?? ''
	const method = request.method
	const ownerCall = url.pathname === ANALYTIC_SITES_PATH || method !== 'POST'
	if (method === 'OPTIONS') return new Response(null, { status: 204, headers: cors(origin, ownerCall && request.headers.get('Access-Control-Request-Method') !== 'POST') })
	try {
		if (url.pathname === ANALYTIC_EVENTS_PATH && method === 'POST') return await collect(request, env, nowMs, origin)
		if (!['GET', 'POST', 'DELETE'].includes(method)) return reply({ error: 'method not allowed' }, 405, origin, true)
		if (await isRateLimited(request, method === 'GET' ? env.EVAL_READ_LIMITER : env.EVAL_WRITE_LIMITER)) {
			return reply({ error: 'rate limit exceeded' }, 429, origin, true, { 'Retry-After': String(RETRY_AFTER_SECONDS) })
		}
		// 주인 쪽은 인증이 있어야 한다: 지갑 헤더가 있으면 지갑 서명(본문 바이트 그대로)만, 없으면 QR 로그인 세션.
		// 주인의 주소는 지갑 주소, 또는 세션 이메일의 hashId (logis-center oauth 의 사이트 확인 meta 와 같은 식)
		const body = new Uint8Array(await request.arrayBuffer())
		if (body.length > 4096) return reply({ error: 'body too large' }, 413, origin, true)
		const owner = WALLET_HEADERS.some((h) => request.headers.has(h))
			? await verifyWalletRequest(request, body, nowMs)
			: readSessionCredentials(request)
				? await verifiedAddress(request, env)
				: null
		if (!owner) return reply({ error: 'sign in, or sign the request with a wallet (x-pd-address, x-pd-time, x-pd-signature)' }, 401, origin, true)
		if (url.pathname === ANALYTIC_SITES_PATH) return await sites(request, env, nowMs, origin, fetcher, body, owner)
		return await events(request, env, origin, owner)
	} catch (err) {
		console.log('analytic error', err)
		return reply({ error: 'internal error' }, 500, origin, ownerCall)
	}
}

// 시험 · 앱 확인용: 저장된 data 칸을 사건으로 푼다
export function decodeStoredEvent(data: Uint8Array): Record<string, unknown> {
	return decodeBytes(data)
}
