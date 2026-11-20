// stage_cases.json 만들기: web/desktop/stage.ts 의 지금 동작을 그대로 적는다.
// Rust(tests/stage_cases.rs)와 TS(web/desktop/stage.test.ts)가 이 표 하나를 읽어 같은 답을 내는지 본다.
// stage.ts 를 고쳤으면 다시 만들고, 바뀐 줄을 Rust 쪽(src/stage.rs)에도 옮긴다. 표를 손으로 고치지 않는다.
//
// 실행 (premium-discount 폴더에서): node --experimental-strip-types crates/premium-discount-window/tests/gen_stage_cases.mjs
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { DOCK_MARGIN, DRAG_THRESHOLD, STAGE_SIZE, classifyPointer, growsTo, nextStage, placeWindow } from '../../../web/desktop/stage.ts'

const stages = [0, 1, 2]
const events = ['pill', 'feed-top', 'expand', 'escape', 'collapse']

const hd = { x: 0, y: 0, width: 1920, height: 1040 } // 작업 표시줄 40px 를 뺀 1080p
const hi = { x: 0, y: 0, width: 2880, height: 1560 } // 150% 배율 1080p
const qhd = { x: 0, y: 0, width: 2560, height: 1400 } // 125% 배율 1440p
const left = { x: -1920, y: 0, width: 1920, height: 1080 } // 주 모니터 왼쪽 (x 음수)
const leftHi = { x: -2880, y: -200, width: 2880, height: 1560 } // 왼쪽 위, 150%
const top = { x: 0, y: -1080, width: 1920, height: 1080 } // 주 모니터 위 (y 음수)
const taskbarLeft = { x: 48, y: 0, width: 1872, height: 1080 } // 작업 표시줄이 왼쪽
const taskbarTop = { x: 0, y: 40, width: 1920, height: 1040 } // 작업 표시줄이 위
const tiny = { x: 0, y: 0, width: 300, height: 400 } // 창보다 작은 작업 영역
const tinyNeg = { x: -800, y: -600, width: 300, height: 400 }

/** [이름, 단계, 작업 영역, 배율, 모서리, 여백] */
const placeInputs = [
	['1080p 처음 알약', 0, hd, 1, null, DOCK_MARGIN],
	['1080p 처음 미니 피드', 1, hd, 1, null, DOCK_MARGIN],
	['1080p 처음 서랍', 2, hd, 1, null, DOCK_MARGIN],
	['150% 서랍', 2, hi, 1.5, null, DOCK_MARGIN],
	['150% 알약', 0, hi, 1.5, null, DOCK_MARGIN],
	['125% 알약', 0, qhd, 1.25, null, DOCK_MARGIN],
	['125% 미니 피드', 1, qhd, 1.25, null, DOCK_MARGIN],
	['125% 서랍', 2, qhd, 1.25, null, DOCK_MARGIN],
	['175% 알약 (반올림)', 0, hi, 1.75, null, DOCK_MARGIN],
	['110% 알약 (소수 배율)', 0, hd, 1.1, null, DOCK_MARGIN],
	['115% 미니 피드 (소수 배율)', 1, hd, 1.15, null, DOCK_MARGIN],
	['왼쪽 모니터 미니 피드', 1, left, 1, null, DOCK_MARGIN],
	['왼쪽 위 모니터 150% 서랍', 2, leftHi, 1.5, null, DOCK_MARGIN],
	['위 모니터 알약', 0, top, 1, null, DOCK_MARGIN],
	['작업 표시줄 왼쪽', 1, taskbarLeft, 1, null, DOCK_MARGIN],
	['작업 표시줄 위', 2, taskbarTop, 1, null, DOCK_MARGIN],
	['배율 0 은 1', 0, hd, 0, null, DOCK_MARGIN],
	['음수 배율은 1', 2, hd, -2, null, DOCK_MARGIN],
	['여백 0', 0, hd, 1, null, 0],
	['여백 24, 150%', 1, hi, 1.5, null, 24],
	['끌어 둔 창을 서랍으로 키움', 2, hd, 1, { right: 1000, bottom: 600 }, DOCK_MARGIN],
	['왼쪽 위 구석에서 키우면 안으로 민다', 2, hd, 1, { right: 370, bottom: 70 }, DOCK_MARGIN],
	['모서리가 화면 밖이면 당긴다', 0, hd, 1, { right: 5000, bottom: 5000 }, DOCK_MARGIN],
	['모서리가 왼쪽 위 밖', 1, hd, 1, { right: -500, bottom: -500 }, DOCK_MARGIN],
	['끌어 둔 창을 알약으로 줄임', 0, hd, 1, { right: 1000, bottom: 600 }, DOCK_MARGIN],
	['150% 끌어 둔 창', 2, hi, 1.5, { right: 1500, bottom: 1000 }, DOCK_MARGIN],
	['왼쪽 모니터 끌어 둔 창', 2, left, 1, { right: -100, bottom: 700 }, DOCK_MARGIN],
	['왼쪽 위 모니터 150% 끌어 둔 창', 1, leftHi, 1.5, { right: -2500, bottom: 0 }, DOCK_MARGIN],
	['모서리가 있으면 여백을 쓰지 않는다', 0, hd, 1, { right: 1920, bottom: 1040 }, DOCK_MARGIN],
	['작업 영역보다 큰 창은 왼쪽 위', 2, tiny, 1, null, DOCK_MARGIN],
	['작업 영역보다 큰 창 (음수 좌표)', 2, tinyNeg, 1, null, DOCK_MARGIN],
	['작업 영역보다 큰 창 (모서리)', 2, tiny, 1, { right: 9000, bottom: 9000 }, DOCK_MARGIN],
]

const place_window = placeInputs.map(([name, stage, area, scale, anchor, margin]) => {
	const size = STAGE_SIZE[stage]
	return { name, size, area, scale, anchor, margin, expected: placeWindow(size, area, scale, anchor, margin) }
})

const next_stage = []
for (const ready of [true, false])
	for (const current of stages)
		for (const event of events) next_stage.push({ current, event, ready, expected: nextStage(current, event, { ready }) })

/** [dx, dy, 문턱] — 경계는 정확히 떨어지는 값(3-4-5)만 쓴다. hypot 의 마지막 자리는 구현마다 다를 수 있다 */
const pointerInputs = [
	[0, 0, DRAG_THRESHOLD],
	[3, 3, DRAG_THRESHOLD],
	[4, 3, DRAG_THRESHOLD],
	[-3, -4, DRAG_THRESHOLD],
	[5, 0, DRAG_THRESHOLD],
	[0, 5, DRAG_THRESHOLD],
	[-6, 0, DRAG_THRESHOLD],
	[0, -2, DRAG_THRESHOLD],
	[4.99, 0, DRAG_THRESHOLD],
	[0, -4.99, DRAG_THRESHOLD],
	[3.5, 3.5, DRAG_THRESHOLD],
	[3.6, 3.6, DRAG_THRESHOLD],
	[0.5, 4.9, DRAG_THRESHOLD],
	[120, -80, DRAG_THRESHOLD],
	[9, 4, 10],
	[8, 6, 10],
	[0, 0, 0],
	[2.5, 0, 2.5],
]
const classify_pointer = pointerInputs.map(([dx, dy, threshold]) => ({ dx, dy, threshold, expected: classifyPointer(dx, dy, threshold) }))

const grows_to = []
for (const from of stages) for (const to of stages) grows_to.push({ from, to, expected: growsTo(from, to) })

const out = {
	note: 'web/desktop/stage.ts 의 동작을 tests/gen_stage_cases.mjs 로 적은 표. 손으로 고치지 말고 다시 만든다',
	constants: { stage_size: stages.map((s) => STAGE_SIZE[s]), dock_margin: DOCK_MARGIN, drag_threshold: DRAG_THRESHOLD },
	place_window,
	next_stage,
	classify_pointer,
	grows_to,
}

// 한 사례가 한 줄이 되게 쓴다 (바뀐 사례가 diff 에서 바로 보인다)
const lines = ['{']
const keys = Object.keys(out)
keys.forEach((key, i) => {
	const comma = i < keys.length - 1 ? ',' : ''
	const value = out[key]
	if (Array.isArray(value)) {
		lines.push(`  ${JSON.stringify(key)}: [`)
		value.forEach((c, j) => lines.push(`    ${JSON.stringify(c)}${j < value.length - 1 ? ',' : ''}`))
		lines.push(`  ]${comma}`)
	} else {
		lines.push(`  ${JSON.stringify(key)}: ${JSON.stringify(value)}${comma}`)
	}
})
lines.push('}')

const target = fileURLToPath(new URL('./stage_cases.json', import.meta.url))
writeFileSync(target, lines.join('\n') + '\n')
console.log(`${target}: place_window ${place_window.length}, next_stage ${next_stage.length}, classify_pointer ${classify_pointer.length}, grows_to ${grows_to.length}`)
