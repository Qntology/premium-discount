// 명령을 부르는 길. Tauri 웹뷰 안이면 Tauri의 invoke를, 브라우저(개발 중)에서는 dev-server.mjs 브리지를 쓴다.
// Tauri에서 window.__TAURI__를 쓰려면 tauri.conf.json의 app.withGlobalTauri를 true로 둔다 (apps/core/README.md).
import type { Invoke } from './commands.ts'

type TauriGlobal = { core?: { invoke?: Invoke } }

/** 브리지: POST /invoke {cmd, args}. 실패한 명령은 Rust가 준 {kind, message}를 그대로 던진다 (toCommandFailure가 받는다) */
export const httpInvoke: Invoke = async (cmd, args) => {
	const res = await fetch('/invoke', {
		method: 'POST',
		// 다른 사이트의 폼이 보내지 못하게 하는 표지 (dev-server.mjs가 확인한다)
		headers: { 'content-type': 'application/json', 'x-pd-ui': '1' },
		body: JSON.stringify({ cmd, args: (args as { args?: unknown } | undefined)?.args ?? null }),
	})
	const body = await res.json().catch(() => ({ ok: false, error: { kind: 'http', message: `bridge answered ${res.status}` } }))
	if (!body.ok) throw body.error
	return body.result
}

export function pickInvoke(): Invoke {
	const tauri = (globalThis as { __TAURI__?: TauriGlobal }).__TAURI__
	return tauri?.core?.invoke ?? httpInvoke
}

type OpenerGlobal = { openUrl?: (url: string) => Promise<void> }

/** 이 기기의 다른 앱(메일 앱)을 연다. Tauri에서는 opener 플러그인이 Android의 Intent와 데스크톱의 기본 앱 열기를 맡는다.
 *  Tauri 밖(개발용 브라우저)에서는 브라우저의 링크 열기로 대신한다. mailto: 만 열 수 있다 (apps/desktop/capabilities). */
export async function openExternal(url: string): Promise<void> {
	const opener = (globalThis as { __TAURI__?: { opener?: OpenerGlobal } }).__TAURI__?.opener
	if (opener?.openUrl) return opener.openUrl(url)
	window.location.href = url
}
