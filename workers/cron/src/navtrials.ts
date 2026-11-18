// 접근성 내비게이션 타임트라이얼(trial/1)의 서버 보관 규칙. www(받기 · 보여 주기: workers/www/src/navtrials.ts)와 cron(지갑 기록 지우기)이 함께 쓴다.
// 기기 쪽 같은 규칙은 crates/premium-discount-nav trial.rs 다(MAX_STEPS · MAX_COURSES · tier).
//
// - 플로우(코스) 하나는 체크포인트 maxSteps 개까지, 계정 · 앱(웹)마다 코스 maxCourses 개까지. 받는 수 제한(10분에 100개)은 없앴다.
// - 같은 코스를 다시 달리면 내 콜렉션의 내 기록(내가 달린 횟수 · 최고 · 마지막)만 고치고, 새 코스가 넘치면 가장 오래 달리지 않은
//   코스를 덮어쓴다.
// - 계정은 새로 만드는 것을 막지 않는다. 지갑(익명 계정) 기록은 마지막으로 달린 날짜 기준 walletMaxAgeDays 일까지 둔다.
//   세션(QR 메일 로그인) 기록은 사용자가 지울 때까지 둔다.
// - 코스 id 는 체인이다: hashId(<앱 id> + <첫 체크포인트 id>), 다음부터 hashId(<이전 코스 id> + <체크포인트 id>), 결승의 값.
//   같은 길은 같은 id 라 두 번 생기지 않는다. 사용자마다 코스 등록은 한 번이다(사용자 코스 id = hashId(<계정 키> + <코스 id>)).
//   서버에 그 코스 id 가 없으면 새 코스를 만들고, 있으면 처음 등록하는 사용자마다
//   계정 없는 합계를 한 번 더한다. 합계는 줄지 않는다. runs(등록한 사용자 수)가 publishMinRuns 이상인 코스만 다른 사용자에게 보인다.

export const TRIAL_SCHEMA = 'trial/1'
// 런의 종류(SDS 결정)를 판정에 넣는 규칙. contracts/verdict_v2.json 의 recipe 와 같다
export const NAV_RECIPE = 'pd-verdict/2'
export const DAY_MS = 86_400_000
// 받는 날짜의 하한: 2026-01-01
export const NAV_FIRST_DAY = 20454

export const TRIAL_POLICY = {
	// 플로우(코스) 하나의 체크포인트 수. 기기가 끝 쪽 10개로 자른다
	maxSteps: 10,
	// 계정 · 앱마다 콜렉션의 코스 수
	maxCourses: 10,
	// 지갑 기록을 두는 날 수 (오늘 포함)
	walletMaxAgeDays: 30,
	// 다른 사용자에게 보이는 코스의 등록 하한 = 등록한 사용자 수 (6차 답 8: 같은 해시의 기록이 10개 이하면 공개하지 않음)
	publishMinRuns: 11,
	// 구간 시간 단위 · 런 최대 길이
	splitUnitMs: 100,
	maxRunMs: 3_600_000,
	// 코스 등급을 매기는 결정 수 하한
	tierMinDecided: 3,
} as const

export type Tier = 'premium' | 'neutral' | 'discount' | 'unrated'

export function dayOf(ms: number): number {
	return Math.floor(ms / DAY_MS)
}

// 지갑 기록으로 둘 수 있는 가장 오래된 날짜. 마지막으로 달린 날이 이보다 앞선 지갑 코스는 지운다
export function walletOldestDay(nowMs: number): number {
	return dayOf(nowMs) - TRIAL_POLICY.walletMaxAgeDays + 1
}

// 코스 등급: 결정이 난 런이 3개보다 적으면 unrated, premium 이 2/3 이상이면 premium, discount 가 2/3 이상이면 discount,
// 그 사이는 neutral. crates/premium-discount-nav trial.rs 의 tier 와 같은 정수 비교다
export function tier(premium: number, discount: number): Tier {
	const decided = premium + discount
	if (decided < TRIAL_POLICY.tierMinDecided) return 'unrated'
	if (premium * 3 >= decided * 2) return 'premium'
	if (discount * 3 >= decided * 2) return 'discount'
	return 'neutral'
}

// 기한이 지난 지갑 코스를 지운다 (모든 지갑 계정). 지운 행 수를 돌려준다
export async function expireWalletTrials(db: D1Database, nowMs: number): Promise<number> {
	const res = await db.prepare(`DELETE FROM nav_trials WHERE "kind" = 'wallet' AND "last_day" < ?1`).bind(walletOldestDay(nowMs)).run()
	return Number(res.meta?.changes ?? 0)
}
