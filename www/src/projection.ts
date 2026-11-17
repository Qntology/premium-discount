// 공개 응답용 투영. 원본 주소(from, to)는 D1에 그대로 두고, 웹에는 표시용 키만 내려준다.

/**
 * 작성자 주소 -> 표시용 키.
 * 서버 비밀 키(AUTHOR_KEY_SECRET)로 HMAC-SHA256을 계산해 앞 12자리만 쓴다.
 * 비밀 키를 모르면 주소를 대조하거나 복원할 수 없고, 같은 작성자는 항상 같은 키가 나온다.
 */
export async function authorKey(address: string, secret: string): Promise<string> {
	if (!secret) {
		throw new Error('AUTHOR_KEY_SECRET is not configured')
	}
	const enc = new TextEncoder()
	const key = await crypto.subtle.importKey(
		'raw',
		enc.encode(secret),
		{ name: 'HMAC', hash: 'SHA-256' },
		false,
		['sign']
	)
	const sig = await crypto.subtle.sign('HMAC', key, enc.encode(address.toLowerCase()))
	const hex = Array.from(new Uint8Array(sig), (b) => b.toString(16).padStart(2, '0')).join('')
	return hex.slice(0, 12)
}
