// Alibaba Cloud Model Studio Decision Model API로 텍스트 유해성을 판정한다.
// 근거: 공식 API 레퍼런스 (Decision Model API, /compatible-mode/v1/systemone)
//
// 요청: POST, Authorization: Bearer <DASHSCOPE_API_KEY>, model = decision-model-preview
//       state = 판정할 텍스트, questions.<이름> = { type: "noul", instructions }
//       질문은 한 번의 호출에 여러 개를 묶을 수 있다. 호출 횟수가 그대로이므로 비용은 늘지 않는다.
// 응답: answers.<이름> = { type: "noul", noul: <P(yes) 0~1 실수> }
//
// 판정 기준: 질문 중 하나라도 P(yes) >= THRESHOLD 이면 거부(profane)다. 공식 문서에 결정 임계값은 없다 (임시값 0.5).
// 응답이 예상 형태가 아니면 예외를 던진다. 알 수 없는 응답을 clean으로 보지 않는다 (fail-closed).
// 보낸 질문 중 하나라도 답이 빠져 있으면 같은 이유로 예외다 (일부만 검사된 결과를 통과로 보지 않는다).

export const DECISION_MODEL = 'decision-model-preview'
export const THRESHOLD = 0.5

// 질문 이름은 거부 사유(eval_queue.last_error)로 그대로 쓰인다. 앱의 status 조회가 이 값을 돌려준다.
export type Category = 'profanity' | 'nsfw' | 'illegal'

export const QUESTIONS: Record<Category, string> = {
	profanity: '이 텍스트에 욕설이나 모욕적인 비속어가 포함되어 있는가? 포함되어 있으면 yes.',
	nsfw: '이 텍스트에 성적으로 노골적이거나 음란한 표현, 또는 폭력을 노골적으로 묘사하거나 조장하는 표현이 포함되어 있는가? 포함되어 있으면 yes.',
	illegal:
		'이 텍스트가 불법 촬영물, 범죄나 자해를 조장하는 내용, 또는 타인의 개인정보(전화번호·주소·주민등록번호 등) 노출을 포함하는가? 포함되어 있으면 yes.',
}

const CATEGORIES = Object.keys(QUESTIONS) as Category[]

export type Verdict = 'clean' | 'profane'

export interface Moderation {
	verdict: Verdict
	// 임계값을 넘긴 질문들. clean이면 비어 있다
	flagged: Category[]
}

export interface ModerationEnv {
	DASHSCOPE_API_KEY: string
	// 예: https://{WorkspaceId}.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/systemone
	DECISION_ENDPOINT: string
}

export async function moderate(env: ModerationEnv, text: string): Promise<Moderation> {
	const questions: Record<string, { type: 'noul'; instructions: string }> = {}
	for (const c of CATEGORIES) questions[c] = { type: 'noul', instructions: QUESTIONS[c] }

	const res = await fetch(env.DECISION_ENDPOINT, {
		method: 'POST',
		headers: {
			'Content-Type': 'application/json',
			Authorization: `Bearer ${env.DASHSCOPE_API_KEY}`,
		},
		body: JSON.stringify({ model: DECISION_MODEL, state: text, questions }),
	})
	if (!res.ok) {
		// 응답 본문에는 원문이 들어가지 않으므로 상태 코드만 남긴다
		throw new Error(`decision api responded ${res.status}`)
	}
	return parseModeration(await res.json())
}

export function parseModeration(body: unknown): Moderation {
	const answers = (body as { answers?: Record<string, unknown> } | null)?.answers
	if (!answers || typeof answers !== 'object') {
		throw new Error('unrecognized decision response shape')
	}
	const flagged: Category[] = []
	for (const c of CATEGORIES) {
		const answer = answers[c] as { type?: unknown; noul?: unknown } | undefined
		if (!answer || answer.type !== 'noul' || typeof answer.noul !== 'number' || !Number.isFinite(answer.noul)) {
			throw new Error('unrecognized decision response shape')
		}
		if (answer.noul >= THRESHOLD) flagged.push(c)
	}
	return { verdict: flagged.length > 0 ? 'profane' : 'clean', flagged }
}
