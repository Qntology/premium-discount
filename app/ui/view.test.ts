// 실행: node --experimental-strip-types --test app/ui/view.test.ts  (premium-discount 폴더에서)
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { STATES } from './localdb.ts'
import { canVote, describeVote, nextVote, tierView } from './view.ts'
import { MAX_TEXT_BYTES, accountView, byteCounter, describeDownload, describeDraftModel, describeFilter, formatDraft, formatBytes, isModelMissing, modelChoices, describeError, describeLoginError, describeLoginPhase, describeStartup, loginMode, mailtoLink, MAIL_BODY, MAIL_SUBJECT, normalizePlatform, formatCountdown, shortAddress, describeSubmit, formatAgo, previewText, reasonLabel, scorePercent, sourceView, stateView, utf8Length } from './view.ts'

test('로컬 상태 여섯 가지가 모두 한국어 라벨을 가진다', () => {
	for (const s of STATES) assert.notEqual(stateView(s).label, s, s)
	assert.equal(stateView('pending').label, '검수 중')
	assert.equal(stateView('stored').tone, 'ok')
	assert.equal(stateView('rejected').tone, 'bad')
	assert.equal(stateView('nope').label, 'nope', '모르는 상태는 값 그대로')
})

test('거부 사유: Rust가 로컬 행에 남기는 실제 형식을 읽기 쉬운 말로', () => {
	// 사유와 함께 거부 (cron의 질문 이름이 쉼표로 이어진다)
	assert.equal(reasonLabel('server status rejected: profanity'), '욕설')
	assert.equal(reasonLabel('server status rejected: nsfw,illegal'), '음란·폭력, 불법·개인정보')
	// 사유 없이 거부, 재시도 한도를 넘겨 실패
	assert.equal(reasonLabel('server status rejected'), '서버가 거부했습니다')
	assert.match(reasonLabel('server status failed'), /재시도 한도 초과/)
	// 답글을 단 글이 거부됨 (로컬 id가 들어간다), 서버가 요청을 받아들이지 않음
	assert.equal(reasonLabel('parent local-3 was rejected'), '답글을 단 글이 거부됨')
	assert.equal(reasonLabel('parent rejected'), '답글을 단 글이 거부됨')
	assert.equal(reasonLabel('server returned 400: {"error":"x"}'), '서버가 요청을 받아들이지 않았습니다 (400)')
	// 접두어 없는 값과 모르는 사유는 그대로
	assert.equal(reasonLabel('nsfw,illegal'), '음란·폭력, 불법·개인정보')
	assert.equal(reasonLabel('something new'), 'something new')
	assert.equal(reasonLabel(''), '')
})

test('바이트 수는 UTF-8 기준이고 서버 한도(8192)와 같다', () => {
	assert.equal(MAX_TEXT_BYTES, 8192)
	assert.equal(utf8Length('abc'), 3)
	assert.equal(utf8Length('가'), 3)
	assert.equal(utf8Length('😀'), 4)
	assert.equal(byteCounter('가'.repeat(2730)).over, false)
	assert.equal(byteCounter('가'.repeat(2731)).over, true, '8193바이트')
	assert.equal(byteCounter('abc').label, '3 / 8,192 바이트')
})

test('미리보기는 공백을 접고 길면 자른다', () => {
	assert.equal(previewText('  a\n\n b   c '), 'a b c')
	assert.equal(previewText('가'.repeat(100), 10), '가'.repeat(10) + '…')
})

test('점수는 0~100%로 맞춘다', () => {
	assert.equal(scorePercent(0.951), '95%')
	assert.equal(scorePercent(-0.2), '0%')
	assert.equal(scorePercent(1.3), '100%')
})

test('시각 표시: 방금, 분, 시간, 날짜', () => {
	const now = Date.UTC(2026, 10, 14, 12, 0, 0)
	assert.equal(formatAgo(now - 10_000, now), '방금')
	assert.equal(formatAgo(now - 5 * 60_000, now), '5분 전')
	assert.equal(formatAgo(now - 3 * 3600_000, now), '3시간 전')
	assert.match(formatAgo(now - 3 * 86400_000, now), /^\d{4}\.\d{2}\.\d{2}$/)
	assert.equal(formatAgo(now + 5000, now), '방금', '시계가 어긋나도 음수가 나오지 않는다')
})

test('저장 결과 안내: 차단은 글을 남기고, 접수·대기는 글을 비운다', () => {
	const blocked = describeSubmit({ status: 'blocked' })
	assert.equal(blocked.keepText, true)
	assert.match(blocked.detail!, /저장하지 않았고/)
	for (const status of ['accepted', 'saved'] as const) {
		const n = describeSubmit({ status, id: 'a', root: 'a', ref: null, from: '0x1' })
		assert.equal(n.keepText, false, status)
	}
	const queued = describeSubmit({ status: 'queued', local_id: 'local-1' })
	assert.equal(queued.keepText, false)
	assert.match(queued.title, /오프라인/)
	assert.equal(describeSubmit({ status: 'accepted', id: 'a', root: 'a', ref: null, from: '0x1' }).tone, 'busy')
})

test('오류 안내: Rust가 돌려주는 실제 문구를 한국어로 바꾼다', () => {
	// 실제 main 실행에서 받은 문구 (app/tests/e2e)
	assert.equal(describeError({ kind: 'invalid_input', message: 'invalid input: cc must be a registrable domain: localhost' }).title, '도메인 형식이 올바르지 않습니다')
	assert.match(describeError({ kind: 'invalid_input', message: 'invalid input: text must be at most 8192 bytes' }).title, /너무 깁니다 \(최대 8,192바이트\)/)
	assert.equal(describeError({ kind: 'invalid_input', message: 'invalid input: input uses 9000 tokens, limit is 8192' }).title, '글이 너무 길어 처리할 수 없습니다')
	assert.equal(describeError({ kind: 'invalid_input', message: 'invalid input: text must not be empty' }).title, '내용을 입력하세요')
	assert.equal(describeError({ kind: 'offline', message: 'x' }).title, '서버에 연결할 수 없습니다')
	const unknown = describeError({ kind: 'internal', message: 'boom' })
	assert.equal(unknown.detail, 'boom', '모르는 오류는 원문을 보여 준다')
	for (const kind of ['invalid_input', 'offline', 'embed', 'storage', 'queue_full', 'rejected', 'http'] as const) {
		assert.equal(describeError({ kind, message: 'm' }).keepText, true, '오류일 때는 입력한 글을 지우지 않는다')
	}
})

test('검색 결과 출처: 내 글과 다른 사용자를 구분한다', () => {
	assert.deepEqual(sourceView({ source: 'both' }), { label: '내 글', mine: true })
	// local이어도 "서버 반영 전"이라고 단정하지 않는다 (서버 검색이 실패했을 때도 local이 된다)
	assert.deepEqual(sourceView({ source: 'local' }), { label: '내 글', mine: true })
	assert.equal(sourceView({ source: 'remote' }).mine, false)
})

test('주소 줄이기와 남은 시간 표시', () => {
	assert.equal(shortAddress('0x00000000000000000000000000000000000000cc'), '0x0000…00cc')
	assert.equal(shortAddress('0x12'), '0x12')
	assert.equal(shortAddress(null), '')
	assert.equal(formatCountdown(15 * 60), '15:00')
	assert.equal(formatCountdown(65), '1:05')
	assert.equal(formatCountdown(-3), '0:00')
})

test('계정 표시: 게스트와 로그인, 등록 주소 안내', () => {
	const guest = accountView({ state: 'anonymous', address: null })
	assert.deepEqual([guest.signedIn, guest.label], [false, '게스트'])
	assert.match(guest.author, /로그인하면 내 계정 주소로 등록/)
	const user = accountView({ state: 'signed_in', address: '0x00000000000000000000000000000000000000cc' })
	assert.deepEqual([user.signedIn, user.label, user.author], [true, '0x0000…00cc', '등록 주소: 0x0000…00cc'])
	assert.equal(accountView({ state: 'signed_in', address: null }).label, '로그인됨')
})

test('로그인 오류: Rust가 돌려주는 실제 문구를 알아본다', () => {
	// 실제 문구: AppError::Server → "server returned {status}: {body}", 연결 실패는 offline
	assert.match(describeLoginError({ kind: 'server', message: 'server returned 503: {"error":"login is not configured"}' }), /로그인이 아직 설정되지 않았습니다/)
	assert.match(describeLoginError({ kind: 'server', message: 'server returned 503: {"state":"unavailable","address":null}' }), /잠시 응답하지 않습니다/)
	assert.match(describeLoginError({ kind: 'server', message: 'server returned 429: {"error":"rate limit exceeded"}' }), /요청이 너무 많습니다/)
	assert.match(describeLoginError({ kind: 'offline', message: 'x' }), /연결할 수 없습니다/)
	assert.match(describeLoginError({ kind: 'rejected', message: 'rejected: login is not supported by this backend' }), /지원하지 않습니다/)
	assert.match(describeLoginError({ kind: 'storage', message: 'boom' }), /boom/, '모르는 오류는 원문을 보여 준다')
})

test('로그인 창 상태 줄', () => {
	assert.equal(describeLoginPhase('starting').tone, 'busy')
	const waiting = describeLoginPhase('waiting', { remainingSec: 872 })
	assert.deepEqual([waiting.title, waiting.detail], ['메일을 기다리는 중입니다', '남은 시간 14:32'])
	assert.equal(describeLoginPhase('waiting').detail, undefined)
	const done = describeLoginPhase('done', { address: '0x00000000000000000000000000000000000000cc' })
	assert.deepEqual([done.tone, done.detail], ['ok', '0x0000…00cc'])
	assert.equal(describeLoginPhase('expired').tone, 'warn')
	assert.equal(describeLoginPhase('error', { error: '연결 실패' }).detail, '연결 실패')
})

test('시작 실패 안내: 모델이 없을 때는 해결 방법을 함께 보여 준다', () => {
	const base = { model_dir: '/home/u/models/embeddinggemma-2', data_dir: '/home/u/.local/share/app', server_url: 'https://premium.discount' }
	// 실제 문구: EmbeddingGemma::load가 폴더가 없을 때 돌려주는 오류
	const missing = describeStartup({ ...base, error: 'embedding failed: 토크나이저 로드 실패: No such file or directory (os error 2)' })
	assert.equal(missing.title, '모델이 필요합니다')
	assert.equal(missing.tone, 'info', '처음 실행처럼 모델만 없으면 고장처럼 보이지 않게')
	assert.ok(missing.hints.some((h) => h.includes('/home/u/models/embeddinggemma-2')))
	assert.ok(missing.hints.some((h) => h.includes('PD_MODEL_DIR')))
	assert.ok(missing.hints.some((h) => h.includes('https://premium.discount')))
	const other = describeStartup({ ...base, error: 'storage failed: 폴더를 만들 수 없습니다' })
	assert.ok(!other.hints.some((h) => h.includes('PD_MODEL_DIR')), '모델과 상관없는 오류에는 모델 안내를 붙이지 않는다')
	assert.equal(other.title, '앱을 시작하지 못했습니다')
	assert.equal(other.tone, 'bad')
	assert.equal(describeStartup({ ...base, error: null }).error, '알 수 없는 오류')
})

test('메일 링크: 받는 사람은 그대로, 제목과 본문은 인코딩해서 채운다', () => {
	const address = `${'ab12'.repeat(10)}.premium.discount@oauth.email`
	const link = mailtoLink(address)
	assert.ok(link.startsWith(`mailto:${address}?`), '주소의 @와 .은 그대로 둔다 (메일 앱이 받는 사람으로 읽는다)')
	const url = new URL(link)
	assert.equal(url.protocol, 'mailto:')
	assert.equal(url.searchParams.get('subject'), MAIL_SUBJECT)
	assert.equal(url.searchParams.get('body'), MAIL_BODY, '한글과 공백이 깨지지 않고 되돌아온다')
	assert.ok(!link.includes(' ') && !link.includes('\n'), '공백과 줄바꿈이 그대로 들어가지 않는다')
})

test('플랫폼에 따라 로그인 방식이 갈린다 (모바일은 링크, 데스크톱은 QR)', () => {
	assert.equal(normalizePlatform('android'), 'android')
	assert.equal(normalizePlatform('ios'), 'ios')
	assert.equal(normalizePlatform('linux'), 'desktop')
	assert.equal(normalizePlatform(undefined), 'desktop')
	assert.deepEqual([loginMode('android'), loginMode('ios'), loginMode('desktop')], ['link', 'link', 'qr'])
})

test('모델 내려받기: 크기 표시, 선택지, 진행 문구', () => {
	assert.equal(formatBytes(574_347_110), '574.3MB')
	assert.equal(formatBytes(1_521_085_798), '1.52GB')
	assert.equal(formatBytes(-1), '-')
	const choices = modelChoices({ text_bytes: 574_347_110, full_bytes: 1_521_085_798 })
	assert.deepEqual(choices.map((c) => [c.variant, c.recommended]), [['text', true], ['full', false]])
	assert.match(choices[0].label, /텍스트 전용 · 574\.3MB/)

	// 49.97% → 49% (다 받기 전에 100%를 보이지 않도록 내림)
	const running = describeDownload({ state: 'running', done_bytes: 287_000_000, total_bytes: 574_347_110, error: null })
	assert.equal(running.percent, 49)
	assert.ok(running.busy && /287\.0MB \/ 574\.3MB \(49%\)/.test(running.text), running.text)
	assert.equal(describeDownload({ state: 'running', done_bytes: 574_347_109, total_bytes: 574_347_110, error: null }).percent, 99)
	const failed = describeDownload({ state: 'failed', done_bytes: 10, total_bytes: 100, error: '저장 공간이 부족합니다' })
	assert.ok(failed.resumable && failed.tone === 'bad' && failed.text.includes('저장 공간이 부족합니다'))
	assert.ok(describeDownload({ state: 'cancelled', done_bytes: 50, total_bytes: 100, error: null }).text.includes('이어받습니다'))
	assert.equal(describeDownload({ state: 'done', done_bytes: 100, total_bytes: 100, error: null }).percent, 100)
	assert.equal(describeDownload({ state: 'idle', done_bytes: 0, total_bytes: 0, error: null }).percent, null)
})

test('시작 실패가 모델 파일 때문인지 가린다 (실제 Rust 문구)', () => {
	assert.ok(isModelMissing('embedding failed: 토크나이저 로드 실패: No such file or directory (os error 2)'))
	assert.ok(isModelMissing('embedding failed: 가중치 파일이 없습니다: model.safetensors 또는 model-text.safetensors (/m)'))
	assert.ok(!isModelMissing('storage failed: 폴더를 만들 수 없습니다'))
	assert.ok(!isModelMissing(null))
})

test('등급 배지: grade 다섯 단계와 판정 전', () => {
	const v = (tier: string, grade: number | null) => ({ tier, grade, rank: null, evidence: 3 }) as any
	assert.deepEqual(tierView(v('premium', 2)), { label: '강력 추천', tone: 'ok' })
	assert.deepEqual(tierView(v('premium', 1)), { label: '추천', tone: 'ok' })
	assert.deepEqual(tierView(v('neutral', 0)), { label: '보통', tone: 'muted' })
	assert.deepEqual(tierView(v('discount', -1)), { label: '비추천', tone: 'bad' })
	assert.deepEqual(tierView(v('discount', -2)), { label: '강력 비추천', tone: 'bad' })
	assert.equal(tierView(v('unrated', null)).label, '판정 전')
	assert.equal(tierView(null).label, '판정 전')
	assert.equal(tierView(undefined).label, '판정 전')
})

test('투표: 남의 서버 글에만, 같은 버튼을 다시 누르면 철회', () => {
	assert.equal(canVote({ id: 'a', source: 'remote' }), true)
	assert.equal(canVote({ id: 'a', source: 'both' }), false, '내 글은 서버가 세지 않는다')
	assert.equal(canVote({ id: null, source: 'local' }), false)
	assert.equal(nextVote(null, 1), 1)
	assert.equal(nextVote(1, 1), 0)
	assert.equal(nextVote(1, -1), -1)
	assert.equal(nextVote(undefined, -1), -1)
	assert.equal(describeVote({ status: 'sent', request_id: 'r', counted: true }, 1).tone, 'ok')
	assert.match(describeVote({ status: 'sent', request_id: 'r', counted: false }, -1).detail!, /로그인/)
	assert.equal(describeVote({ status: 'queued', request_id: 'r' }, 0).tone, 'warn')
	assert.match(describeVote({ status: 'queued', request_id: 'r' }, 0).title, /거뒀/)
})

test('욕설 필터 상태: 기본 사전 + 받기, 받는 중, 모델 사용, 설정으로 끔', () => {
	const idle = { state: 'idle' as const, done_bytes: 0, total_bytes: 0, error: null }
	const base = { installed: false, active: 'dictionary' as const, download: idle, bytes: 678_198_702 }
	const d = describeFilter(base)
	assert.equal(d.action, '더 정확한 모델 받기 (678.2MB)')
	assert.match(d.text, /기본 사전/)
	const running = describeFilter({ ...base, download: { state: 'running', done_bytes: 339_099_351, total_bytes: 678_198_702, error: null } })
	assert.ok(running.busy && running.action === null && /50%/.test(running.text), running.text)
	assert.equal(describeFilter({ ...base, installed: true, active: 'laya' }).text, '욕설 필터: laya-multilingual 모델 (이 기기에서 판정)')
	assert.match(describeFilter({ ...base, installed: true }).text, /filter=dictionary/)
	const failed = describeFilter({ ...base, download: { state: 'failed', done_bytes: 1, total_bytes: 2, error: '저장 공간이 부족합니다' } })
	assert.ok(failed.text.includes('저장 공간이 부족합니다') && failed.action !== null, '실패하면 다시 받을 수 있다')
})

test('초안 모델 상태와 초안 표시', () => {
	const idle = { state: 'idle' as const, done_bytes: 0, total_bytes: 0, error: null }
	const none = describeDraftModel({ installed: false, download: idle, bytes: 1_517_658_062 })
	assert.equal(none.action, '초안 모델 받기 (1.52GB)')
	assert.equal(none.ready, false)
	assert.ok(describeDraftModel({ installed: false, download: { state: 'running', done_bytes: 1, total_bytes: 4, error: null }, bytes: 4 }).busy)
	assert.ok(describeDraftModel({ installed: true, download: idle, bytes: 1 }).ready)
	assert.equal(formatDraft({ type: 2, head: '겨울 패딩 할인', body: '요약', link: 'https://x', digest: 'ab' }), 'type: 2 discount\nhead: 겨울 패딩 할인\nbody: 요약\nlink: https://x\ndigest: ab')
})
