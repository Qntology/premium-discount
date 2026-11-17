// QR 코드 그리기. 매트릭스는 qrcode-generator가 만들고, 화면에는 SVG로 그린다 (innerHTML 없이 DOM으로 만든다).
// QR은 스캐너가 읽을 수 있도록 테마와 상관없이 흰 바탕에 검은 모듈로 고정한다.
import qrcode from 'qrcode-generator'

/** QR 규격이 요구하는 바깥 여백 (모듈 수) */
export const QUIET_ZONE = 4

/** 문자열을 QR 매트릭스(true = 어두운 모듈)로 바꾼다. 오류 정정은 M(약 15%)이다 */
export function qrModules(text: string): boolean[][] {
	const qr = qrcode(0, 'M')
	qr.addData(text)
	qr.make()
	const n = qr.getModuleCount()
	return Array.from({ length: n }, (_, r) => Array.from({ length: n }, (_, c) => qr.isDark(r, c)))
}

/** SVG path 데이터. 가로로 이어진 어두운 모듈은 사각형 하나로 묶는다 */
export function qrPath(modules: boolean[][]): string {
	const parts: string[] = []
	modules.forEach((row, y) => {
		let x = 0
		while (x < row.length) {
			if (!row[x]) {
				x++
				continue
			}
			let end = x
			while (end < row.length && row[end]) end++
			parts.push(`M${x} ${y}h${end - x}v1h-${end - x}z`)
			x = end
		}
	})
	return parts.join('')
}

const SVG_NS = 'http://www.w3.org/2000/svg'

/** target 안에 QR을 그린다 (이전 내용은 지운다). label은 스크린 리더용 설명이다 */
export function renderQr(target: HTMLElement, text: string, label: string): SVGSVGElement {
	const modules = qrModules(text)
	const size = modules.length + QUIET_ZONE * 2
	const svg = document.createElementNS(SVG_NS, 'svg')
	svg.setAttribute('viewBox', `0 0 ${size} ${size}`)
	svg.setAttribute('role', 'img')
	svg.setAttribute('aria-label', label)
	svg.setAttribute('shape-rendering', 'crispEdges')
	const bg = document.createElementNS(SVG_NS, 'rect')
	bg.setAttribute('width', String(size))
	bg.setAttribute('height', String(size))
	bg.setAttribute('fill', '#ffffff')
	const path = document.createElementNS(SVG_NS, 'path')
	path.setAttribute('d', qrPath(modules))
	path.setAttribute('transform', `translate(${QUIET_ZONE} ${QUIET_ZONE})`)
	path.setAttribute('fill', '#000000')
	svg.append(bg, path)
	target.replaceChildren(svg)
	return svg
}
