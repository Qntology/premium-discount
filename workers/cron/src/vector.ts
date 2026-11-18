// D1 BLOB 컬럼에서 읽은 벡터를 Float32Array로 바꾼다.
//
// 쓸 때는 ArrayBuffer(Float32 x 768 = 3072바이트)로 넣지만, Workers 바인딩이 읽어 줄 때는 ArrayBuffer가 아니라
// 바이트 숫자 배열(number[])로 돌아온다 (D1 Worker API 문서의 타입 변환 표: ArrayBuffer는 BLOB으로 쓰이고
// 읽을 때 Array.from 결과가 된다). 배열을 그대로 new Float32Array(...)에 넘기면 길이 3072, 값 0~255인
// 가짜 벡터가 만들어지므로, 형태를 가려서 바이트로 모은 뒤 길이를 검사하고 Float32로 읽는다.
//
// 오류 메시지에는 길이와 형태만 넣는다 (벡터 값은 넣지 않는다).

// workers/www/src/evaluations.ts의 EVAL_DIMENSIONS와 같아야 한다 (google/embeddinggemma-2 출력 차원)
export const VECTOR_DIMENSIONS = 768
const BYTES_PER_FLOAT = 4

export function toVector(raw: unknown): Float32Array {
	let bytes: Uint8Array
	if (raw instanceof ArrayBuffer) {
		bytes = new Uint8Array(raw)
	} else if (ArrayBuffer.isView(raw)) {
		bytes = new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength)
	} else if (Array.isArray(raw)) {
		// 바이트 값이 아닌 것이 섞여 있으면 Uint8Array.from이 조용히 값을 깎아 버리므로 먼저 검사한다
		for (let i = 0; i < raw.length; i++) {
			const b = raw[i]
			if (typeof b !== 'number' || !Number.isInteger(b) || b < 0 || b > 255) {
				throw new Error('vector blob contains a non-byte value')
			}
		}
		bytes = Uint8Array.from(raw as number[])
	} else {
		throw new Error('vector blob has an unexpected type')
	}

	if (bytes.byteLength !== VECTOR_DIMENSIONS * BYTES_PER_FLOAT) {
		throw new Error(`vector blob has ${bytes.byteLength} bytes, expected ${VECTOR_DIMENSIONS * BYTES_PER_FLOAT}`)
	}
	// slice()로 오프셋 0의 새 버퍼에 복사한다. Float32Array는 4바이트 정렬된 시작 위치가 필요하다
	return new Float32Array(bytes.slice().buffer)
}
