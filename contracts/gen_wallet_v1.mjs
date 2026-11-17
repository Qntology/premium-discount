// 지갑 갈래(사용자가 시드 구문을 보관하는 익명 계정) 참조 구현. contracts/wallet_v1.json 을 만든다.
//
// 실행: node contracts/gen_wallet_v1.mjs   (premium-discount 폴더에서. www/node_modules 의 ethers 를 쓴다. 파일을 다시 쓴다)
//
// 이 파일이 지갑 갈래의 기준이다. 서버(www/src/auth.ts)와 Rust 라이브러리(crates/premium-discount-identity)는 이 파일이 만든
// wallet_v1.json 의 값을 그대로 내야 한다.
//
// 규칙
// 1. 지갑: BIP-39 영어 12단어, 경로 m/44'/60'/0'/0/0 (ethers Wallet.createRandom · Wallet.fromPhrase 의 기본값)
// 2. 서명: EIP-191 personal_sign (ethers signMessage). RFC 6979 결정적 서명이라 같은 키 · 같은 글이면 같은 서명이다
// 3. 요청 서명: 아래 글(줄바꿈은 \n, 끝에 줄바꿈 없음)을 서명해 헤더 x-pd-address · x-pd-time · x-pd-signature 로 보낸다
//      premium.discount request v1
//      address: <소문자 주소>
//      time: <유닉스 ms>
//      method: <대문자 메서드>
//      path: <경로, 쿼리 없이>
//      body-sha256: <본문 바이트의 SHA-256, 소문자 hex>
//    서버는 시각이 5분 안이고 서명에서 되살린 주소가 x-pd-address 와 같을 때만 받는다

import { createRequire } from 'node:module'
import { createHash } from 'node:crypto'
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const require = createRequire(import.meta.url)
const { ethers } = require('../www/node_modules/ethers')

const SCHEME = 'pd-wallet/1'
const PATH = "m/44'/60'/0'/0/0"
const MAX_SKEW_MS = 300000

function requestMessage({ address, timeMs, method, path, body }) {
	const sha = createHash('sha256').update(Buffer.from(body, 'utf8')).digest('hex')
	return [
		'premium.discount request v1',
		`address: ${address.toLowerCase()}`,
		`time: ${timeMs}`,
		`method: ${method.toUpperCase()}`,
		`path: ${path}`,
		`body-sha256: ${sha}`,
	].join('\n')
}

const phrases = [
	// Hardhat · Foundry 의 공개 시험 시드 구문 (실제 자산에 쓰면 안 된다)
	'test test test test test test test test test test test junk',
	// BIP-39 공식 시험 벡터
	'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about',
]
const wallets = phrases.map((phrase) => {
	const w = ethers.Wallet.fromPhrase(phrase)
	if (w.path !== PATH) throw new Error(`경로가 다르다: ${w.path}`)
	return { phrase, address: w.address.toLowerCase(), private_key: w.privateKey }
})

const signer = ethers.Wallet.fromPhrase(phrases[0])
const texts = ['hello', '패치 1.4: 보스 체력 15% 너프', '']
const messages = []
for (const text of texts) {
	messages.push({ text, signature: await signer.signMessage(text) })
}

const requestInputs = [
	{ timeMs: 1763380800000, method: 'POST', path: '/nav/trials', body: '{"runs":[{"schema":"trial/1","type":"premium"}]}' },
	{ timeMs: 1763380860123, method: 'get', path: '/nav/verdicts', body: '' },
]
const requests = []
for (const r of requestInputs) {
	const message = requestMessage({ address: signer.address, ...r })
	requests.push({
		address: signer.address.toLowerCase(),
		time_ms: r.timeMs,
		method: r.method,
		path: r.path,
		body: r.body,
		body_sha256: createHash('sha256').update(Buffer.from(r.body, 'utf8')).digest('hex'),
		message,
		signature: await signer.signMessage(message),
	})
}

// 받으면 안 되는 시드 구문: 체크섬이 틀림, 단어 수가 틀림, 단어표에 없는 낱말
const invalidPhrases = [
	'test test test test test test test test test test test test',
	'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon',
	'test test test test test test test test test test test 장바구니',
]
for (const phrase of invalidPhrases) {
	let accepted = true
	try {
		ethers.Wallet.fromPhrase(phrase)
	} catch {
		accepted = false
	}
	if (accepted) throw new Error(`ethers 가 받는 시드 구문이다: ${phrase}`)
}

const out = {
	scheme: SCHEME,
	generated_by: 'contracts/gen_wallet_v1.mjs',
	ethers: ethers.version,
	path: PATH,
	max_skew_ms: MAX_SKEW_MS,
	wallets,
	messages,
	requests,
	invalid_phrases: invalidPhrases,
}
const file = fileURLToPath(new URL('./wallet_v1.json', import.meta.url))
writeFileSync(file, JSON.stringify(out, null, 2) + '\n', 'utf8')
for (const w of wallets) console.log(w.address, w.phrase.split(' ').length, '단어')
console.log(`서명 ${messages.length}개, 요청 ${requests.length}개, 거절할 시드 구문 ${invalidPhrases.length}개`)
