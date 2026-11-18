// PD-Verdict v1: 추천(premium) / 비추천(discount) 판정. 순수 함수만 있다 (D1·네트워크 없음).
//
// 계약은 contracts/verdict_v1.json이고, 그 파일은 contracts/gen_verdict_v1.py(참조 구현)가 만든다.
// workers/cron/test/verdict.test.ts가 이 파일의 PARAMS와 사례를 모두 대조한다. raon-zena의 zena-verdict(Rust)도 같은 파일을 통과한다.
//
// 1. 관측마다 u = weight × credit × 2^(−age/H). age는 now − at_ms (미래 시각은 0). sign 0(철회)은 세지 않는다
// 2. E = Σu, v = Σ(u × sign) / (E + k)
// 3. 기준선: secondary → primary → global 중 n ≥ min_obs인 첫 단계. 있으면 r = tanh((v − mean) / (sd + eps))
// 4. 등급: 대체됨 → discount / E < min_evidence → unrated / 들어가기 / 머무르기(직전 등급) / neutral
// 5. grade: premium +2(r ≥ strong) 또는 +1, neutral 0, discount −2(대체됨 또는 r ≤ −strong) 또는 −1, unrated null

export const RECIPE = 'pd-verdict/1'

export type Level = 'secondary' | 'primary' | 'global'
export type Tier = 'premium' | 'neutral' | 'discount' | 'unrated'

export const LEVELS: readonly Level[] = ['secondary', 'primary', 'global']

export interface Obs {
	kind: string
	// +1 추천 쪽, −1 비추천 쪽, 0 철회
	sign: number
	weight: number
	// 0..1 (결과 크레딧. 투표는 1)
	credit: number
	at_ms: number
}

export interface Baseline {
	n: number
	mean: number
	sd: number
}

export type Baselines = Partial<Record<Level, Baseline>>

export interface TrackParams {
	half_life_ms: number
	min_obs: Record<Level, number>
}

export interface Params {
	k: number
	eps: number
	enter: number
	exit: number
	floor: number
	strong: number
	min_evidence: number
	duel_rate: number
	duel_min: number
	tracks: Record<string, TrackParams>
	weights: Record<string, number>
}

const DAY = 86_400_000

// contracts/verdict_v1.json의 params와 같아야 한다 (테스트가 대조한다)
export const PARAMS: Params = {
	k: 2.0,
	eps: 0.05,
	enter: 0.5,
	exit: 0.3,
	floor: 0.1,
	strong: 0.8,
	min_evidence: 2.0,
	duel_rate: 0.65,
	duel_min: 3,
	tracks: {
		pd: { half_life_ms: 90 * DAY, min_obs: { secondary: 5, primary: 10, global: 30 } },
		drop: { half_life_ms: 14 * DAY, min_obs: { secondary: 6, primary: 3, global: 10 } },
		web: { half_life_ms: 14 * DAY, min_obs: { secondary: 8, primary: 4, global: 12 } },
		chat: { half_life_ms: 14 * DAY, min_obs: { secondary: 6, primary: 3, global: 10 } },
		coach: { half_life_ms: 14 * DAY, min_obs: { secondary: 6, primary: 3, global: 10 } },
	},
	weights: {
		vote: 1.0,
		vote_outcome: 0.5,
		manual: 2.0,
		feedback: 1.0,
		outcome: 1.0,
		reaction: 0.6,
		engage: 0.2,
		model: 0.3,
	},
}

export interface Prev {
	tier: Tier
	since_ms: number
}

export interface Duel {
	wins: number
	losses: number
}

export interface Verdict {
	evidence: number
	value: number
	level: Level | null
	rank: number | null
	tier: Tier
	grade: number | null
	since_ms: number
	reasons: string[]
}

function track(params: Params, name: string): TrackParams {
	const t = params.tracks[name]
	if (!t) throw new Error(`unknown verdict track: ${name}`)
	return t
}

// 근거량 E와 값 v
export function evidence(obs: readonly Obs[], nowMs: number, trackName: string, params: Params = PARAMS): { evidence: number; value: number } {
	const halfLife = track(params, trackName).half_life_ms
	let e = 0
	let s = 0
	for (const o of obs) {
		if (o.sign === 0) continue
		const age = Math.max(0, nowMs - o.at_ms)
		const u = o.weight * o.credit * 2 ** (-age / halfLife)
		e += u
		s += u * o.sign
	}
	return { evidence: e, value: s / (e + params.k) }
}

export interface JudgeInput {
	evidence: number
	value: number
	baselines: Baselines
	prev: Prev
	nowMs: number
	track: string
	duel?: Duel
	superseded?: boolean
}

// 이미 계산한 E·v와 기준선으로 등급을 정한다 (cron은 피어의 등급을 다시 매길 때 표를 다시 읽지 않는다)
export function judge(input: JudgeInput, params: Params = PARAMS): Verdict {
	const t = track(params, input.track)
	const { evidence: e, value: v } = input
	let level: Level | null = null
	let base: Baseline | null = null
	for (const lv of LEVELS) {
		const b = input.baselines[lv]
		if (b && b.n >= t.min_obs[lv]) {
			level = lv
			base = b
			break
		}
	}
	const r = base ? Math.tanh((v - base.mean) / (base.sd + params.eps)) : null
	let win: number | null = null
	let loss: number | null = null
	if (input.duel && input.duel.wins + input.duel.losses >= params.duel_min) {
		const meetings = input.duel.wins + input.duel.losses
		win = input.duel.wins / meetings
		loss = input.duel.losses / meetings
	}
	const prev = input.prev.tier
	const superseded = input.superseded === true

	const reasons: string[] = []
	let tier: Tier
	if (superseded) {
		tier = 'discount'
		reasons.push('superseded')
	} else if (e < params.min_evidence) {
		tier = 'unrated'
		reasons.push('low_evidence')
	} else {
		const rankHigh = r !== null && r >= params.enter
		const rankLow = r !== null && r <= -params.enter
		const duelWin = win !== null && win >= params.duel_rate
		const duelLoss = loss !== null && loss >= params.duel_rate
		if (v >= params.floor && (rankHigh || duelWin)) {
			tier = 'premium'
			reasons.push(rankHigh ? 'rank_high' : 'duel_win')
		} else if (v <= -params.floor && (rankLow || duelLoss)) {
			tier = 'discount'
			reasons.push(rankLow ? 'rank_low' : 'duel_loss')
		} else if (prev === 'premium' && v > 0 && ((r !== null && r >= params.exit) || (r === null && duelWin))) {
			tier = 'premium'
			reasons.push('held')
		} else if (prev === 'discount' && v < 0 && ((r !== null && r <= -params.exit) || (r === null && duelLoss))) {
			tier = 'discount'
			reasons.push('held')
		} else {
			tier = 'neutral'
			if (rankHigh || rankLow || duelWin || duelLoss) reasons.push('floor')
		}
	}
	reasons.push(level ? `level:${level}` : 'no_baseline')

	let grade: number | null
	if (tier === 'premium') grade = r !== null && r >= params.strong ? 2 : 1
	else if (tier === 'discount') grade = superseded || (r !== null && r <= -params.strong) ? -2 : -1
	else if (tier === 'neutral') grade = 0
	else grade = null

	const since = tier === prev ? input.prev.since_ms : input.nowMs
	return { evidence: e, value: v, level, rank: r, tier, grade, since_ms: since, reasons }
}

export interface VerdictCase {
	now_ms: number
	track: string
	prev: Prev
	obs: Obs[]
	baseline?: Baselines
	duel?: Duel
	superseded?: boolean
}

// 관측에서 판정까지 (계약 사례의 입력 그대로)
export function verdict(c: VerdictCase, params: Params = PARAMS): Verdict {
	const { evidence: e, value: v } = evidence(c.obs, c.now_ms, c.track, params)
	return judge(
		{ evidence: e, value: v, baselines: c.baseline ?? {}, prev: c.prev, nowMs: c.now_ms, track: c.track, duel: c.duel, superseded: c.superseded },
		params
	)
}

// 기준선 스냅샷 (모집단 표준편차). 판정 가능한(E ≥ min_evidence) 노드의 v만 넣는다
export function baselineOf(values: readonly number[]): Baseline {
	const n = values.length
	if (n === 0) return { n: 0, mean: 0, sd: 0 }
	const mean = values.reduce((a, b) => a + b, 0) / n
	const variance = values.reduce((a, b) => a + (b - mean) * (b - mean), 0) / n
	return { n, mean, sd: Math.sqrt(variance) }
}
