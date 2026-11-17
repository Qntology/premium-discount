// 세션 자격 증명 처리 (OAuth3 hash/token: 브라우저는 쿠키, 앱은 헤더).
//
// 검증: S3 hash/<hash> 객체를 읽기 전용으로 조회해 token 일치, email 존재, ip 일치를 확인한다.
// 읽기 전용 AWS 자격 증명(SessionEnv)이 설정되어 있을 때만 동작한다. 설정이 없거나 어떤 확인이든 실패하면 null이다.
// null이면 저장되는 from은 클라이언트 값 또는 게스트 주소다 (evaluations.ts의 우선순위).
//
// 세션은 두 종류다.
// 1) 웹 세션: cdn(oauth.network)이 발급한다. S3 객체의 token이 요청의 token과 같아야 한다.
// 2) 앱 세션: 이 Worker가 발급한다 (issueSession). token = HMAC-SHA256(SESSION_SECRET, "session|" + hash)라서 저장소가 필요 없다.
//    사용자가 QR(mailto:<hash>.<호스트>@oauth.email)의 주소로 메일을 보내면 SES가 S3 hash/<hash>에 email을 기록하고(README 10-3),
//    그 email이 생기면 로그인이 끝난다. 메일은 다른 기기(폰)에서 보내므로 ip는 비교하지 않는다.
//
// S3 객체 형식은 이 저장소 밖(oauth.network)이 정한다. 웹 세션은 JSON(token, email, 선택적으로 ip)을, 앱 세션은 email만 가정했고,
// 실제 객체로 확인되지 않았다. 배포 전에 실제 hash/<hash> 객체 한 건으로 확인해야 한다.
// 쓰기 경로가 있는 api/cdn의 세션 응답은 검증용으로 쓰지 않는다.

import { AwsClient } from 'aws4fetch'
import { ethers } from 'ethers'

const HASH_RE = /^[0-9a-f]{40}$/
const TOKEN_RE = /^[0-9a-f]{64}$/
const ZERO_RE = /^0+$/

export interface SessionCredentials {
	hash: string
	token: string
}

// 세션 검증용 읽기 전용 자격 증명. 모두 있어야 켜진다 (wrangler secret put). 하나라도 없으면 검증하지 않는다.
export interface SessionEnv {
	// 예: oauth-sessions
	SESSION_S3_BUCKET?: string
	// 예: ap-northeast-2
	SESSION_S3_REGION?: string
	// s3:GetObject만 허용한 읽기 전용 키여야 한다
	SESSION_S3_ACCESS_KEY_ID?: string
	SESSION_S3_SECRET_ACCESS_KEY?: string
	// 앱 세션 발급용 비밀 (wrangler secret put SESSION_SECRET). 없으면 앱 로그인을 시작할 수 없다
	SESSION_SECRET?: string
	// QR 메일 주소의 호스트 부분 (<hash>.<호스트>@oauth.email). 기본 premium.discount
	SESSION_MAIL_HOST?: string
}

function parseCookies(header: string): Map<string, string> {
	const out = new Map<string, string>()
	for (const part of header.split(';')) {
		const i = part.indexOf('=')
		if (i < 0) continue
		out.set(part.slice(0, i).trim(), part.slice(i + 1).trim())
	}
	return out
}

// 0x 접두사 제거, 공백 제거, 소문자화. 앱은 0x를 붙여 보낼 수 있고 브라우저 쿠키는 붙지 않는다.
function normalizeHex(value: string): string {
	let v = value.trim().toLowerCase()
	if (v.startsWith('0x')) v = v.slice(2)
	return v
}

// hash/token을 읽는다. 브라우저는 쿠키(hash, token), 앱은 헤더(x-oauth-hash, x-oauth-token)로 보낸다.
// 헤더가 하나라도 있으면 헤더만 쓴다. 형식이 맞지 않으면 null (익명 방문자로 취급).
// 앱이 오프라인일 때 보내는 자리값(0x000..., 0x000...)은 세션이 아니므로 null이다.
export function readSessionCredentials(request: Request): SessionCredentials | null {
	const headerHash = request.headers.get('x-oauth-hash')
	const headerToken = request.headers.get('x-oauth-token')
	let hash: string
	let token: string
	if (headerHash !== null || headerToken !== null) {
		hash = headerHash ?? ''
		token = headerToken ?? ''
	} else {
		const cookies = parseCookies(request.headers.get('Cookie') ?? '')
		hash = cookies.get('hash') ?? ''
		token = cookies.get('token') ?? ''
	}
	hash = normalizeHex(hash)
	token = normalizeHex(token)
	if (!HASH_RE.test(hash) || !TOKEN_RE.test(token)) return null
	if (ZERO_RE.test(hash) || ZERO_RE.test(token)) return null
	return { hash, token }
}

// 이메일 → 주소. oauth.network(cdn/api)과 같은 식: computeAddress(hashMessage(email))
export function addressFromEmail(email: string): string {
	return ethers.computeAddress(ethers.hashMessage(email)).toLowerCase()
}

// 문자열 비교에서 길이와 내용이 맞는 위치를 흘리지 않도록 끝까지 비교한다
function timingSafeEqual(a: string, b: string): boolean {
	const ea = new TextEncoder().encode(a)
	const eb = new TextEncoder().encode(b)
	let diff = ea.length ^ eb.length
	const n = Math.max(ea.length, eb.length)
	for (let i = 0; i < n; i++) diff |= (ea[i] ?? 0) ^ (eb[i] ?? 0)
	return diff === 0
}

const enc = new TextEncoder()

async function hmacHex(secret: string, message: string): Promise<string> {
	const key = await crypto.subtle.importKey('raw', enc.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign'])
	const sig = await crypto.subtle.sign('HMAC', key, enc.encode(message))
	return Array.from(new Uint8Array(sig), (b) => b.toString(16).padStart(2, '0')).join('')
}

const tokenFor = (secret: string, hash: string) => hmacHex(secret, 'session|' + hash)

export interface IssuedSession {
	hash: string
	token: string
	// 사용자가 메일을 보낼 주소와, QR에 넣을 mailto: 링크 (웹의 QR과 같은 모양)
	address: string
	mailto: string
}

// 앱 세션 발급. 저장하지 않는다: 같은 hash로 token을 다시 계산할 수 있으므로 검증 때 비밀만 있으면 된다.
// hash는 QR에 보이지만 token은 보이지 않는다 (token은 이 응답으로만 앱에 전달된다).
export async function issueSession(env: SessionEnv): Promise<IssuedSession> {
	if (!env.SESSION_SECRET) throw new Error('SESSION_SECRET is not configured')
	const bytes = new Uint8Array(20)
	crypto.getRandomValues(bytes)
	const hash = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
	const token = await tokenFor(env.SESSION_SECRET, hash)
	const address = `${hash}.${env.SESSION_MAIL_HOST || 'premium.discount'}@oauth.email`
	return { hash, token, address, mailto: 'mailto:' + encodeURIComponent(address) }
}

export type SessionCheck =
	| { state: 'verified'; email: string }
	// 우리가 발급한 세션이지만 아직 메일이 도착하지 않았다
	| { state: 'pending' }
	// 자격 증명이 맞지 않는다 (익명으로 취급)
	| { state: 'invalid' }
	// 확인할 수 없다 (S3 설정이 없거나 S3 오류). 로그인으로 취급하지 않는다
	| { state: 'unavailable' }

// 세션을 확인한다. verified가 아닌 모든 경우는 로그인이 아니다 (fail-closed).
// - 웹 세션: S3 객체의 token 일치 + email 존재 + 객체에 ip가 있으면 CF-Connecting-IP와 일치
// - 앱 세션: token이 HMAC과 일치 + S3 객체에 email 존재 (객체가 아직 없으면 pending)
// S3는 GetObject 권한만 있으면 없는 키에도 403을 돌려주므로, 403과 404는 "아직 없음"으로 본다.
export async function checkSession(creds: SessionCredentials, env: SessionEnv, ip: string): Promise<SessionCheck> {
	const { SESSION_S3_BUCKET: bucket, SESSION_S3_REGION: region } = env
	const accessKeyId = env.SESSION_S3_ACCESS_KEY_ID
	const secretAccessKey = env.SESSION_S3_SECRET_ACCESS_KEY
	if (!bucket || !region || !accessKeyId || !secretAccessKey) return { state: 'unavailable' }

	const issued = env.SESSION_SECRET ? timingSafeEqual(creds.token, await tokenFor(env.SESSION_SECRET, creds.hash)) : false
	try {
		const aws = new AwsClient({ accessKeyId, secretAccessKey, region, service: 's3' })
		// creds.hash는 40자리 hex로 검증된 값이라 경로에 그대로 써도 안전하다
		const res = await aws.fetch(`https://${bucket}.s3.${region}.amazonaws.com/hash/${creds.hash}`)
		if (res.status === 403 || res.status === 404) return { state: issued ? 'pending' : 'invalid' }
		if (!res.ok) return { state: 'unavailable' }
		const obj = (await res.json()) as { token?: unknown; email?: unknown; ip?: unknown } | null
		if (!obj || typeof obj !== 'object') return { state: issued ? 'pending' : 'invalid' }
		const email = typeof obj.email === 'string' ? obj.email.trim() : ''
		if (issued) return email ? { state: 'verified', email } : { state: 'pending' }
		if (typeof obj.token !== 'string' || !timingSafeEqual(normalizeHex(obj.token), creds.token)) return { state: 'invalid' }
		if (!email) return { state: 'invalid' }
		if (typeof obj.ip === 'string' && obj.ip !== '' && obj.ip !== ip) return { state: 'invalid' }
		return { state: 'verified', email: obj.email as string }
	} catch {
		return { state: 'unavailable' }
	}
}

// 검증된 세션의 주소. 검증되지 않았으면 null
export async function verifiedAddress(request: Request, env?: SessionEnv): Promise<string | null> {
	const creds = readSessionCredentials(request)
	if (!creds || !env) return null
	const ip = (request.headers.get('cf-connecting-ip') ?? '').trim()
	const check = await checkSession(creds, env, ip)
	return check.state === 'verified' ? addressFromEmail(check.email) : null
}

// 인증의 두 번째 갈래: 지갑(익명 계정). 사용자가 시드 구문을 보관하는 ethers 지갑이고, 요청마다 EIP-191 서명을 붙인다.
// 위의 세션(익명 아님: QR 메일 로그인)과 달리 서버는 지갑 주소만 안다. 이메일 · 전화번호 같은 식별 정보는 없다.
// 규칙과 기준값은 contracts/gen_wallet_v1.mjs · wallet_v1.json 이고, 기기 쪽 crates/premium-discount-identity 가 같은 값을 낸다.
// 서명하는 글(줄바꿈 \n):
//   premium.discount request v1 / address: <소문자 주소> / time: <유닉스 ms> / method: <대문자 메서드> / path: <경로> / body-sha256: <hex>
export const WALLET_REQUEST_VERSION = 'premium.discount request v1'
export const WALLET_MAX_SKEW_MS = 300_000
const WALLET_ADDRESS_RE = /^0x[0-9a-f]{40}$/
const WALLET_SIGNATURE_RE = /^0x[0-9a-fA-F]{130}$/
const WALLET_TIME_RE = /^\d{1,15}$/

async function sha256Hex(body: Uint8Array): Promise<string> {
	const digest = await crypto.subtle.digest('SHA-256', body)
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

// 지갑이 서명하는 글
export async function walletRequestMessage(address: string, timeMs: number, method: string, path: string, body: Uint8Array): Promise<string> {
	return [
		WALLET_REQUEST_VERSION,
		`address: ${address.trim().toLowerCase()}`,
		`time: ${timeMs}`,
		`method: ${method.trim().toUpperCase()}`,
		`path: ${path}`,
		`body-sha256: ${await sha256Hex(body)}`,
	].join('\n')
}

// 지갑 서명이 맞으면 그 주소(소문자), 아니면 null. 헤더는 x-pd-address · x-pd-time · x-pd-signature 이고,
// 시각이 5분 안이며 서명에서 되살린 주소가 x-pd-address 와 같아야 한다. body 는 요청 본문 바이트 그대로다(본문을 읽은 쪽이 넘긴다).
// 같은 서명을 다시 보내는 것은 막지 않는다: 받는 쪽이 다룬다(투표는 request_id 로 버리고, 타임트라이얼은 계정 없는 합계를 계정마다
// 코스 하나에 하루 한 번만 센다. 5분 안에 같은 요청을 다시 보내면 내 콜렉션의 횟수만 는다)
export async function verifyWalletRequest(request: Request, body: Uint8Array, nowMs: number = Date.now()): Promise<string | null> {
	const address = (request.headers.get('x-pd-address') ?? '').trim().toLowerCase()
	const timeText = (request.headers.get('x-pd-time') ?? '').trim()
	const signature = (request.headers.get('x-pd-signature') ?? '').trim()
	if (!WALLET_ADDRESS_RE.test(address) || !WALLET_TIME_RE.test(timeText) || !WALLET_SIGNATURE_RE.test(signature)) return null
	const timeMs = Number(timeText)
	if (Math.abs(nowMs - timeMs) > WALLET_MAX_SKEW_MS) return null
	const message = await walletRequestMessage(address, timeMs, request.method, new URL(request.url).pathname, body)
	try {
		return ethers.verifyMessage(message, signature).toLowerCase() === address ? address : null
	} catch {
		return null
	}
}
