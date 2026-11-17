// 요청 제한. Cloudflare Workers Rate Limiting 바인딩(wrangler.json의 ratelimits)을 쓴다. D1 테이블은 늘리지 않는다.
// - 대기열 적재(POST /evaluations)는 cron의 Alibaba 결정 API 호출 비용을 만든다.
// - 투표(POST /evaluations/votes)는 cron의 판정 재계산 비용을 만든다. 저장과 같은 쓰기 제한을 함께 쓴다.
// - 검색(/evaluations/search, /public/evaluations/search), 판정 조회(/evaluations/verdicts), 그래프(/graph*)는 D1·Vectorize 조회 비용을 만든다.
//
// 키는 클라이언트 IP(CF-Connecting-IP)다. from·authorKey는 클라이언트가 바꿀 수 있어 키로 쓰지 않는다.
// - 바인딩이 없으면(로컬 개발, 테스트) 제한하지 않는다.
// - IP를 알 수 없으면 제한하지 않는다. 운영에서는 Cloudflare가 항상 이 헤더를 붙인다.
// - 바인딩 호출이 실패하면 요청을 막지 않는다 (제한 장치의 장애가 서비스 장애가 되지 않게).

export interface RateLimitEnv {
	// POST /evaluations와 POST /evaluations/votes (함께 센다). 임시값: IP당 분당 20건
	EVAL_WRITE_LIMITER?: RateLimit
	// 검색, 판정 조회, 그래프 조회. 임시값: IP당 분당 60건
	EVAL_READ_LIMITER?: RateLimit
}

// 429 응답의 Retry-After (초). 바인딩의 period와 같게 둔다
export const RETRY_AFTER_SECONDS = 60

export async function isRateLimited(request: Request, limiter: RateLimit | undefined): Promise<boolean> {
	if (!limiter) return false
	const ip = (request.headers.get('cf-connecting-ip') ?? '').trim()
	if (!ip) return false
	try {
		const { success } = await limiter.limit({ key: ip })
		return !success
	} catch (err) {
		console.log('rate limiter error', err)
		return false
	}
}
