// 앱 로그인 엔드포인트.
//   POST /session/start : 앱 세션을 발급한다 {hash, token, address, mailto}. 사용자는 mailto 링크(QR)의 주소로 메일을 보낸다.
//   GET  /session       : 앱이 x-oauth-hash / x-oauth-token 헤더로 현재 상태를 묻는다 {state, address}
//                          anonymous | pending(메일 대기) | signed_in(address) | unavailable(503)
// 발급은 저장하지 않는다 (auth.ts issueSession). 응답에 token이 들어 있으므로 캐시하지 않는다.
import { addressFromEmail, checkSession, issueSession, readSessionCredentials } from './auth'
import { corsHeaders, type EvalEnv } from './evaluations'
import { isRateLimited, RETRY_AFTER_SECONDS } from './ratelimit'

const PATHS = ['/session', '/session/start']

function json(body: unknown, status: number, origin: string, extra: Record<string, string> = {}): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json', 'Cache-Control': 'no-store', ...corsHeaders(origin), ...extra },
	})
}

export async function handleSession(request: Request, env: EvalEnv): Promise<Response | null> {
	const { pathname } = new URL(request.url)
	if (!PATHS.includes(pathname)) return null
	const origin = request.headers.get('Origin') ?? ''
	if (request.method === 'OPTIONS') return new Response(null, { status: 204, headers: corsHeaders(origin) })

	const start = pathname === '/session/start'
	if (request.method !== (start ? 'POST' : 'GET')) return json({ error: 'method not allowed' }, 405, origin)

	// 발급은 저장 제한기, 상태 확인(S3 읽기 한 번)은 읽기 제한기를 쓴다
	if (await isRateLimited(request, start ? env.EVAL_WRITE_LIMITER : env.EVAL_READ_LIMITER)) {
		return json({ error: 'rate limit exceeded' }, 429, origin, { 'Retry-After': String(RETRY_AFTER_SECONDS) })
	}

	try {
		if (start) {
			if (!env.SESSION_SECRET) return json({ error: 'login is not configured' }, 503, origin)
			return json(await issueSession(env), 201, origin)
		}
		const creds = readSessionCredentials(request)
		if (!creds) return json({ state: 'anonymous', address: null }, 200, origin)
		const ip = (request.headers.get('cf-connecting-ip') ?? '').trim()
		const check = await checkSession(creds, env, ip)
		switch (check.state) {
			case 'verified':
				return json({ state: 'signed_in', address: addressFromEmail(check.email) }, 200, origin)
			case 'pending':
				return json({ state: 'pending', address: null }, 200, origin)
			case 'invalid':
				return json({ state: 'anonymous', address: null }, 200, origin)
			default:
				return json({ state: 'unavailable', address: null }, 503, origin)
		}
	} catch (err) {
		console.log('session error', err)
		return json({ error: 'internal error' }, 500, origin)
	}
}
