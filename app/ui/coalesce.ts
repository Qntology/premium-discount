// 동시에 여러 번 불리는 갱신을 합친다.
// 진행 중에 또 불리면 버리지 않고, 끝난 뒤 한 번만 더 실행한다 (그 사이 바뀐 상태를 놓치지 않게). 여러 번 불려도 한 번이다.
// 더 하는 쪽이 있으면(flush) 그 쪽을 따른다. 돌려주는 Promise는 그 호출이 요청한 갱신이 끝났을 때 이행된다.

export interface RefreshOptions {
	flush: boolean
}

export function coalesced(run: (opts: RefreshOptions) => Promise<void>): (opts: RefreshOptions) => Promise<void> {
	let running: Promise<void> | null = null
	let rerun: { flush: boolean; promise: Promise<void> } | null = null

	function refresh(opts: RefreshOptions): Promise<void> {
		if (!running) {
			running = run(opts)
				.catch(() => {}) // 실패는 run이 알린다. 다음 갱신을 막지 않는다
				.finally(() => {
					running = null
				})
			return running
		}
		if (!rerun) {
			const state = { flush: opts.flush, promise: Promise.resolve() }
			state.promise = running.then(() => {
				rerun = null
				return refresh({ flush: state.flush })
			})
			rerun = state
		} else {
			rerun.flush = rerun.flush || opts.flush
		}
		return rerun.promise
	}
	return refresh
}
