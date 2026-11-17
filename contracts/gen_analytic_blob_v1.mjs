// 웹 분석 사건 BLOB 코덱의 계약 사례. contracts/analytic_blob_v1.json 을 만든다.
//
// 실행: node contracts/gen_analytic_blob_v1.mjs   (premium-discount 폴더에서. www/node_modules 의 pako 를 쓴다. 파일을 다시 쓴다)
//
// D1 의 data 칸은 gzip 한 JSON 이고, 꺼내는 쪽에 따라 모양이 다르다: 바이트 숫자 배열(D1), {type: 'Buffer', data}(Node),
// {"0":…}(JSON.stringify(Uint8Array)), base64 글(Worker 응답), JSON 글. 서버(www/src/blob.ts)와 Rust(crates/premium-discount-analytic
// 의 blob)는 모든 사례에서 expect 를 내야 한다. logis-center runtime.ts 의 decodeAnalyticBlob 은 BUG-RT-2(작은 base64 gzip)와
// BUG-RT-4(원시값) 사례에서 틀린다.
import { createRequire } from 'node:module'
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const require = createRequire(import.meta.url)
const pako = require('../www/node_modules/pako')

const enc = new TextEncoder()
const event = { schema: 'web-event/1', type: 'click', href: 'https://shop.example.com/checkout', target: { pug: 'button | 간편결제' } }
const json = JSON.stringify(event)
const gz = pako.gzip(enc.encode(json))
const small = pako.gzip(enc.encode('{"a":1}'))
const b64 = (bytes) => Buffer.from(bytes).toString('base64')
const indexed = (bytes) => Object.fromEntries(Array.from(bytes, (b, i) => [String(i), b]))

const cases = [
	{ name: 'small base64 gzip (BUG-RT-2)', input: b64(small), expect: { a: 1 } },
	{ name: 'base64 gzip', input: b64(gz), expect: event },
	{ name: 'base64 of plain JSON', input: b64(enc.encode(json)), expect: event },
	{ name: 'JSON text', input: json, expect: event },
	{ name: 'JSON number text (BUG-RT-4)', input: '42', expect: {} },
	{ name: 'JSON null text (BUG-RT-4)', input: 'null', expect: {} },
	{ name: 'JSON string text', input: '"x"', expect: {} },
	{ name: 'JSON array text', input: '[1,2]', expect: {} },
	{ name: 'D1 byte array (gzip)', input: Array.from(gz), expect: event },
	{ name: 'D1 byte array (plain JSON)', input: Array.from(enc.encode(json)), expect: event },
	{ name: 'Node Buffer object', input: { type: 'Buffer', data: Array.from(gz) }, expect: event },
	{ name: 'D1 row', input: { id: 'x', type: 'click', data: Array.from(gz) }, expect: event },
	{ name: 'JSON.stringify(Uint8Array)', input: indexed(gz), expect: event },
	{ name: 'already decoded object', input: { type: 'hover', dwell_ms: 820 }, expect: { type: 'hover', dwell_ms: 820 } },
	{ name: 'broken base64', input: 'not base64 !!', expect: {} },
	{ name: 'byte out of range', input: [300, 1], expect: {} },
	{ name: 'gap in indexed bytes', input: { 0: 31, 2: 139 }, expect: {} },
	{ name: 'number', input: 7, expect: {} },
	{ name: 'null', input: null, expect: {} },
]
// 이 사례들의 gzip 은 pako 기본값(시각 0, 압축 6)이다. 다른 gzip 으로 다시 만들면 바이트가 달라도 expect 는 같아야 한다
const out = { schema: 'analytic-blob/1', note: 'decodeBlob(input) == expect. gzip bytes: pako defaults (mtime 0, level 6)', small_gzip_bytes: small.length, cases }
writeFileSync(fileURLToPath(new URL('./analytic_blob_v1.json', import.meta.url)), JSON.stringify(out, null, 1) + '\n')
console.log(`analytic_blob_v1.json: ${cases.length} cases (small gzip ${small.length} bytes)`)
