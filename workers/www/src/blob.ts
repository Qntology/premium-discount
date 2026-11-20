// 사건 BLOB 코덱: D1 의 data 칸(gzip 한 JSON)을 어떤 모양으로 받아도 같은 객체로 푼다.
// logis-center commerce/src/modes/runtime.ts 의 decodeAnalyticBlob 을 옮기면서, 그 시험이 it.fails 로 남겨 둔 세 결함을 고쳤다.
// 같은 규칙의 Rust 판은 crates/premium-discount-analytic/src/blob.rs 이고, 둘은 contracts/analytic_blob_v1.json 을 함께 통과한다.
//   BUG-RT-2: base64 로 온 gzip 이 50 바이트 이하면 풀지 않고 {} 를 냈다. 이제 길이와 상관없이 gzip 머리(1f 8b)를 본다
//   BUG-RT-3: Uint8Array 조각(subarray)을 밑의 버퍼 전체로 읽었다. byteOffset · byteLength 를 지킨다
//   BUG-RT-4: JSON 원시값('42', 'null')을 그대로 돌려줬다. 사건은 객체여야 하므로 객체가 아니면 {} 다
import { gzip, ungzip } from 'pako'

export type Blob0 = Record<string, unknown>

// 푼 결과의 크기 상한 (압축 폭탄 막기)
export const MAX_DECODED_BYTES = 4 * 1024 * 1024

function isRecord(v: unknown): v is Blob0 {
	return typeof v === 'object' && v !== null && !Array.isArray(v)
}

function objectOrEmpty(v: unknown): Blob0 {
	return isRecord(v) ? v : {}
}

function isGzip(bytes: Uint8Array): boolean {
	return bytes.length >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b
}

// 바이트(gzip 이거나 UTF-8 JSON)를 객체로. 풀 수 없거나 객체가 아니면 {}
export function decodeBytes(bytes: Uint8Array): Blob0 {
	try {
		let text: Uint8Array = bytes
		if (isGzip(bytes)) {
			text = ungzip(bytes)
			if (text.length > MAX_DECODED_BYTES) return {}
		}
		return objectOrEmpty(JSON.parse(new TextDecoder('utf-8').decode(text)))
	} catch {
		return {}
	}
}

function byteArray(items: unknown[]): Uint8Array | null {
	for (const v of items) {
		if (typeof v !== 'number' || !Number.isInteger(v) || v < 0 || v > 255) return null
	}
	return Uint8Array.from(items as number[])
}

// {"0":31,"1":139,…}: 열쇠가 0 부터 이어진 숫자일 때만 바이트다
function indexedBytes(m: Blob0): Uint8Array | null {
	const keys = Object.keys(m)
	const out = new Uint8Array(keys.length)
	for (const k of keys) {
		if (!/^\d+$/.test(k)) return null
		const i = Number(k)
		const v = m[k]
		if (i >= keys.length || typeof v !== 'number' || !Number.isInteger(v) || v < 0 || v > 255) return null
		out[i] = v
	}
	return out
}

function base64Bytes(s: string): Uint8Array | null {
	const compact = s.replace(/\s+/g, '')
	if (!/^[A-Za-z0-9+/]*={0,2}$/.test(compact) || compact.length % 4 === 1) return null
	try {
		return Uint8Array.from(atob(compact), (c) => c.charCodeAt(0))
	} catch {
		return null
	}
}

// D1 · Worker 가 준 data 값을 사건 객체로 푼다. 풀 수 없으면 {}
export function decodeBlob(raw: unknown): Blob0 {
	if (raw instanceof Uint8Array) return decodeBytes(raw) // 조각이면 그 범위만 (BUG-RT-3)
	if (raw instanceof ArrayBuffer) return decodeBytes(new Uint8Array(raw))
	if (ArrayBuffer.isView(raw)) return decodeBytes(new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength))
	if (Array.isArray(raw)) {
		const bytes = byteArray(raw)
		return bytes ? decodeBytes(bytes) : {}
	}
	if (isRecord(raw)) {
		// D1 행({…, data: BLOB}) · Node Buffer({type: 'Buffer', data: […]})면 data 를 푼다
		const inner = raw.data
		if (Array.isArray(inner) || typeof inner === 'string' || isRecord(inner) || inner instanceof Uint8Array || inner instanceof ArrayBuffer) {
			return decodeBlob(inner)
		}
		// 열쇠가 모두 숫자면 JSON.stringify(Uint8Array) 다: 이어지지 않으면 망가진 것
		if (Object.keys(raw).every((k) => /^\d+$/.test(k))) {
			const bytes = indexedBytes(raw)
			return bytes ? decodeBytes(bytes) : {}
		}
		return raw
	}
	if (typeof raw === 'string') {
		try {
			return objectOrEmpty(JSON.parse(raw)) // 원시값이면 {} (BUG-RT-4)
		} catch {
			const bytes = base64Bytes(raw)
			return bytes ? decodeBytes(bytes) : {} // 길이와 상관없이 gzip 머리를 본다 (BUG-RT-2)
		}
	}
	return {}
}

// 객체를 gzip 한 JSON 바이트로 (D1 에 저장하는 모양)
export function encodeBlob(obj: Blob0): Uint8Array {
	return gzip(new TextEncoder().encode(JSON.stringify(obj)))
}

export function bytesToBase64(bytes: Uint8Array): string {
	let s = ''
	for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
	return btoa(s)
}

// D1 이 BLOB 을 바이트 숫자 배열 · ArrayBuffer 로 돌려줄 때, 응답에 싣는 base64 로
export function blobToBase64(raw: unknown): string {
	if (raw instanceof Uint8Array) return bytesToBase64(raw)
	if (raw instanceof ArrayBuffer) return bytesToBase64(new Uint8Array(raw))
	if (Array.isArray(raw)) {
		const b = byteArray(raw)
		return b ? bytesToBase64(b) : ''
	}
	return ''
}
