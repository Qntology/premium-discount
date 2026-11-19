// www 의 보안 · 경합 수정 둘 — Rust 판(workers/www/src/auth.rs · navtrials.rs · analytic.rs)의 같은 수정과 1:1. id 는 docs/workers-rs.md 의 "고친 버그" 표와 같다.
// 모양은 fixes/www.mjs 와 같다 ({ id, worker: "www", file, find, replace, count? }). find 가 정규식이면 그대로 맞춘다.
//
// fix-wallet-replay: 지갑 서명 글에는 경로만 있고 쿼리가 없어(contracts/wallet_v1.json), 잡은 DELETE /nav/trials?app=X 를 5분 안에
//   DELETE /nav/trials 로 다시 보내면 콜렉션 전부가 지워졌다. 서명한 글의 SHA-256 을 서명 시각 + 5분까지 wallet_nonces(0010)에 기억해
//   다시 쓰면 틀린 서명과 같이 null(→ 401). 기한 지난 키 지우기(10개까지)와 키 잡기(INSERT … ON CONFLICT DO NOTHING)는 batch 하나.
// fix-nav-lost-update: storeRuns 가 읽은 값으로 계산한 절대값을 upsert 해(excluded."runs" · "sums" …) 동시에 들어온 요청의 수를 잃었고,
//   같은 사용자의 동시 요청 둘이 모두 처음 등록으로 셌다. 등록은 batch 앞에서 잡고(바뀐 행 1 = 처음), 수는 상대 갱신, 읽은 값에서
//   계산한 JSON(best · record · sums)과 밀어내기는 읽은 값에 조건을 걸어 쓰고, 바뀐 행이 0 이면 다시 읽어 3번까지 다시 시도한다.
const AUTH = "workers/www/src/auth.ts";
const NAV = "workers/www/src/navtrials.ts";
const ANALYTIC = "workers/www/src/analytic.ts";

// 새 storeRuns (원본 331 ~ 535 를 바꾼다). String.raw 라 TS 안의 \n · \t 는 TS 문자열의 이스케이프로 남는다 — 그래서 이 글에는 ` 와 ${ 가 없다
const STORE_RUNS = String.raw`// 고침(fix-nav-lost-update): 조건 건 쓰기가 맞지 않을 때(다른 요청이 그 사이 그 행을 바꿈) 다시 읽어 계산해 다시 쓰는 횟수
const NAV_RETRIES = 3
const NAV_CONFLICT = 'nav trials: concurrent updates did not settle'
const SQL_REGISTER = 'INSERT INTO nav_registrations ("id", "day") VALUES (?1, ?2) ON CONFLICT DO NOTHING'
const SQL_COLLECTION = 'SELECT * FROM nav_trials WHERE "account" = ?1 AND "app" = ?2'
const SQL_EVICT = 'DELETE FROM nav_trials WHERE "account" = ?1 AND "app" = ?2 AND "course" = ?3'
const SQL_EVICT_IF = 'DELETE FROM nav_trials WHERE "account" = ?1 AND "app" = ?2 AND "course" = ?3 AND "touched" = ?4'
const SQL_TRIAL_ROW = 'SELECT * FROM nav_trials WHERE "account" = ?1 AND "app" = ?2 AND "course" = ?3'
const SQL_INSERT_TRIAL = 'INSERT INTO nav_trials ("account", "kind", "app", "course", "steps", "runs", "premium", "discount", "best", "last", "last_day", "touched") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) ON CONFLICT ("account", "app", "course") DO NOTHING'
const SQL_UPDATE_TRIAL = 'UPDATE nav_trials SET "runs" = "runs" + ?5, "premium" = "premium" + ?6, "discount" = "discount" + ?7, "best" = ?8, "last" = ?9, "last_day" = MAX("last_day", ?10), "touched" = ?11 WHERE "account" = ?1 AND "app" = ?2 AND "course" = ?3 AND "best" = ?4'
const SQL_COURSE_ROW = 'SELECT * FROM nav_courses WHERE "course" = ?1'
const SQL_INSERT_COURSE = 'INSERT INTO nav_courses ("course", "app", "steps", "runs", "premium", "discount", "record", "sums", "last_day") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT ("course") DO NOTHING'
const SQL_UPDATE_COURSE = 'UPDATE nav_courses SET "runs" = "runs" + 1, "premium" = "premium" + ?4, "discount" = "discount" + ?5, "record" = ?6, "sums" = ?7, "last_day" = MAX("last_day", ?8) WHERE "course" = ?1 AND "record" = ?2 AND "sums" = ?3'
const SQL_COUNT_UP = 'INSERT INTO nav_counts ("hash", "day", "type", "recipe", "n") VALUES (?1, ?2, ?3, ?4, 1)\n\t\t\t\t ON CONFLICT ("hash", "day", "type", "recipe") DO UPDATE SET "n" = "n" + 1'
const SQL_VECTOR = 'INSERT INTO nav_vectors ("hash", "space", "dims", "vector", "day") VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING'
const changesOf = (res: any) => Number(res?.meta?.changes ?? 0)
// 이 요청이 내 콜렉션의 한 행에 더한 런들: 수(상대 갱신) · 그 런들 중 최고 기록(처음으로 가장 빠른 것) · 가장 늦은 날짜
interface RunAdds {
	runs: number
	premium: number
	discount: number
	best: number[]
	bestTotal: number
	lastDay: number
}
type WriteCheck = { kind: 'evict'; app: string; index: number } | { kind: 'trial'; key: string } | { kind: 'course'; course: string; insert: boolean }

// 런들을 계정의 콜렉션에 넣는다. 순서: 확인 → (지갑) 기한 지난 내 코스 지우기 → 앱마다 콜렉션 읽기 → 보낸 순서대로 적용 →
// 등록 잡기 → 계정 없는 합계 · 개수 · 대표 벡터와 함께 한 번에 쓰기 → 조건이 맞지 않은 쓰기만 다시
export async function storeRuns(db: D1Database, account: NavAccount, items: unknown[], nowMs: number): Promise<RunResult[]> {
	const today = dayOf(nowMs)
	const oldest = account.kind === 'wallet' ? walletOldestDay(nowMs) : NAV_FIRST_DAY
	const results: RunResult[] = []
	const runs: (ParsedRun | null)[] = []
	for (const item of items) {
		try {
			const run = parseRun(item, today)
			const expired = run.day < oldest
			runs.push(expired ? null : run)
			results.push({ course: run.course, status: expired ? 'expired' : 'stored' })
		} catch (err) {
			if (!(err instanceof RunInvalid)) throw err
			runs.push(null)
			results.push({ course: null, status: 'invalid', error: err.message })
		}
	}
	if (runs.every((r) => r === null)) return results

	// 지갑: 기한이 지난 내 코스부터 지운다 (모든 지갑은 cron 도 1분마다 지운다)
	if (account.kind === 'wallet') {
		await db.prepare('DELETE FROM nav_trials WHERE "account" = ?1 AND "last_day" < ?2').bind(account.key, oldest).run()
	}

	// 앱마다 내 콜렉션 (최대 10행). base 는 읽은 행 그대로 — 조건 건 쓰기가 그 값(best · touched)을 본다
	const collections = new Map<string, Map<string, any>>()
	for (const app of new Set(runs.flatMap((r) => (r ? [r.app] : [])))) {
		const rows = await db.prepare(SQL_COLLECTION).bind(account.key, app).all<CollectionRecord>()
		collections.set(app, new Map(rows.results.map((r) => [r.course, { ...r, best: JSON.parse(r.best), last: JSON.parse(r.last), base: r }])))
	}

	// 보낸 순서대로 적용한다 (원본과 같은 계산). adds 는 이 요청이 그 행에 더한 것
	const dirty = new Map<string, { app: string; row: any }>()
	const adds = new Map<string, RunAdds>()
	const evicted: { app: string; course: string; base?: CollectionRecord; index: number }[] = []
	const note = (key: string, run: ParsedRun, premium: number, discount: number) => {
		const a = adds.get(key)
		if (!a) {
			adds.set(key, { runs: 1, premium, discount, best: run.splits, bestTotal: run.total, lastDay: run.day })
			return
		}
		a.runs += 1
		a.premium += premium
		a.discount += discount
		if (run.total < a.bestTotal) {
			a.best = run.splits
			a.bestTotal = run.total
		}
		a.lastDay = Math.max(a.lastDay, run.day)
	}
	runs.forEach((run, i) => {
		if (!run) return
		const collection = collections.get(run.app)!
		const touched = nowMs * 1000 + i
		const premium = run.type === 1 ? 1 : 0
		const discount = run.type === -1 ? 1 : 0
		const key = run.app + '|' + run.course
		const row = collection.get(run.course)
		if (row) {
			const best = run.total < row.best[row.best.length - 1]
			row.runs += 1
			row.premium += premium
			row.discount += discount
			if (best) row.best = run.splits
			row.last = run.splits
			row.last_day = Math.max(row.last_day, run.day)
			row.touched = touched
			results[i] = { course: run.course, status: 'repeat', best }
			dirty.set(key, { app: run.app, row })
			note(key, run, premium, discount)
		} else {
			let status: RunStatus = 'stored'
			let gone: string | undefined
			if (collection.size >= TRIAL_POLICY.maxCourses) {
				// 가장 오래 달리지 않은 코스 자리를 덮어쓴다
				const oldestRow = [...collection.values()].reduce((a, b) => (b.touched < a.touched ? b : a))
				collection.delete(oldestRow.course)
				dirty.delete(run.app + '|' + oldestRow.course)
				adds.delete(run.app + '|' + oldestRow.course)
				evicted.push({ app: run.app, course: oldestRow.course, base: oldestRow.base, index: i })
				status = 'replaced'
				gone = oldestRow.course
			}
			const fresh = {
				course: run.course,
				steps: JSON.stringify(run.steps),
				runs: 1,
				premium,
				discount,
				best: run.splits,
				last: run.splits,
				last_day: run.day,
				touched,
			}
			collection.set(run.course, fresh)
			dirty.set(key, { app: run.app, row: fresh })
			adds.delete(key)
			note(key, run, premium, discount)
			results[i] = { course: run.course, status, ...(gone ? { evicted: gone } : {}) }
		}
	})

	// 사용자마다 코스 등록은 한 번: 사용자 코스 id = hashId(<계정 키> + <코스 id>).
	// 등록을 먼저 잡는다: INSERT … ON CONFLICT DO NOTHING 의 바뀐 행이 1 이면 이 요청이 처음이다 (같은 요청에서 같은 코스를 두 번 보내면 앞의 것만).
	// 아래에서 쓰지 못하면 잡은 등록을 되돌린다
	const valid = runs.filter((r): r is ParsedRun => r !== null)
	const registrationOf = (run: ParsedRun) => cachedHashId(account.key + run.course)
	const firstRun = new Map<string, ParsedRun>()
	for (const run of valid) if (!firstRun.has(registrationOf(run))) firstRun.set(registrationOf(run), run)
	const ids = [...firstRun.keys()]
	const claimOut = await db.batch(ids.map((id) => db.prepare(SQL_REGISTER).bind(id, firstRun.get(id)!.day)))
	const claimed = new Set(ids.filter((_, k) => changesOf(claimOut[k]) > 0))
	const fresh = new Set<ParsedRun>()
	const seen = new Set<string>()
	runs.forEach((run, i) => {
		if (!run) return
		const id = registrationOf(run)
		const first = !seen.has(id)
		seen.add(id)
		results[i].duplicate = !(first && claimed.has(id))
		if (first && claimed.has(id)) fresh.add(run)
	})

	const changed = new Map<string, ParsedRun>()
	const checks: (WriteCheck | null)[] = []
	let out: any[] = []
	try {
		// 계정 없는 코스 합계: 코스 id 가 없으면 새로 만들고(created), 있으면 처음 등록하는 런만 더한다(원본과 같은 계산)
		const totals = await readCourses(db, [...new Set(valid.map((r) => r.course))])
		runs.forEach((run, i) => {
			if (!run) return
			const prev = totals.get(run.course)
			results[i].created = !prev
			if (!fresh.has(run)) return
			if (!prev) {
				totals.set(run.course, {
					course: run.course,
					app: run.app,
					steps: JSON.stringify(run.steps),
					runs: 1,
					premium: run.type === 1 ? 1 : 0,
					discount: run.type === -1 ? 1 : 0,
					record: run.splits,
					sums: run.splits,
					last_day: run.day,
				})
				changed.set(run.course, run)
				return
			}
			if (run.total < prev.record[prev.record.length - 1]) prev.record = run.splits
			prev.runs += 1
			prev.premium += run.type === 1 ? 1 : 0
			prev.discount += run.type === -1 ? 1 : 0
			prev.sums = prev.sums.map((s, k) => s + (run.splits[k] ?? 0))
			prev.last_day = Math.max(prev.last_day, run.day)
			changed.set(run.course, run)
		})

		const statements: D1PreparedStatement[] = []
		const add = (s: D1PreparedStatement, check: WriteCheck | null = null) => {
			statements.push(s)
			checks.push(check)
		}
		for (const e of evicted) {
			// 읽은 행은 그때의 touched 일 때만 지운다 (그 사이 다른 요청이 다시 달린 코스를 지우지 않게). 이 요청에서 넣었다가 밀려난 행은 D1 에 없다
			if (e.base) add(db.prepare(SQL_EVICT_IF).bind(account.key, e.app, e.course, e.base.touched), { kind: 'evict', app: e.app, index: e.index })
			else add(db.prepare(SQL_EVICT).bind(account.key, e.app, e.course))
		}
		for (const [key, { app, row }] of dirty) {
			const a = adds.get(key)!
			if (row.base) {
				// 읽은 행: 수는 더하고, best 는 읽은 글일 때만 바꾼다
				add(db.prepare(SQL_UPDATE_TRIAL).bind(account.key, app, row.course, row.base.best, a.runs, a.premium, a.discount, JSON.stringify(row.best), JSON.stringify(row.last), a.lastDay, row.touched), { kind: 'trial', key })
			} else {
				add(db.prepare(SQL_INSERT_TRIAL).bind(account.key, account.kind, app, row.course, row.steps, row.runs, row.premium, row.discount, JSON.stringify(row.best), JSON.stringify(row.last), row.last_day, row.touched), { kind: 'trial', key })
			}
		}
		for (const t of totals.values()) {
			if (!changed.has(t.course)) continue
			const run = changed.get(t.course)!
			if (t.stored) {
				add(db.prepare(SQL_UPDATE_COURSE).bind(t.course, t.stored.record, t.stored.sums, run.type === 1 ? 1 : 0, run.type === -1 ? 1 : 0, JSON.stringify(t.record), JSON.stringify(t.sums), run.day), { kind: 'course', course: t.course, insert: false })
			} else {
				add(db.prepare(SQL_INSERT_COURSE).bind(t.course, t.app, t.steps, t.runs, t.premium, t.discount, JSON.stringify(t.record), JSON.stringify(t.sums), t.last_day), { kind: 'course', course: t.course, insert: true })
			}
		}
		const countUp = (hash: string, day: number, type: 1 | -1) => db.prepare(SQL_COUNT_UP).bind(hash, day, type, NAV_RECIPE)
		for (const run of fresh) {
			if (run.type !== 0) add(countUp(run.course, run.day, run.type))
		}

		// 고스트 따라가기 판정. 고스트가 공개됐는지는 이 요청을 쓰기 전의 합계로 본다
		const ghosts = await readCourses(db, [...new Set(runs.flatMap((r) => (r?.ghost ? [r.ghost] : [])))])
		runs.forEach((run, i) => {
			if (!run?.ghost) return
			const ghost = ghosts.get(run.ghost)
			const marks =
				fresh.has(run) && ghost && ghost.app === run.app && ghost.runs >= TRIAL_POLICY.publishMinRuns
					? routeMarks(run.steps, JSON.parse(ghost.steps) as StepRef[])
					: null
			if (!marks) {
				results[i].route = null
				return
			}
			for (const m of marks) add(countUp(checkpointId(m.step), run.day, m.type))
			results[i].route = { ghost: run.ghost, premium: marks.filter((m) => m.type === 1).length, discount: marks.filter((m) => m.type === -1).length }
		})
		for (const run of runs) {
			for (const v of run?.vectors ?? []) {
				add(db.prepare(SQL_VECTOR).bind(v.hash, v.space, v.vector.length, v.vector.buffer, run!.day))
			}
		}
		if (statements.length > 0) out = await db.batch(statements)
	} catch (err) {
		// 쓰지 못했다: 이번에 잡은 등록을 되돌린다 (원본은 등록도 같은 batch 라 실패하면 남지 않았다). 되돌리기의 실패는 삼킨다
		if (claimed.size > 0) {
			try {
				for (const part of chunks([...claimed], IN_CHUNK)) {
					await db.prepare('DELETE FROM nav_registrations WHERE "id" IN (' + part.map((_, k) => '?' + (k + 1)).join(', ') + ')').bind(...part).run()
				}
			} catch {}
		}
		throw err
	}

	// 조건이 맞지 않은 쓰기: 다시 읽어 계산해 다시 쓴다
	const retryTrials: string[] = []
	const retryCourses: string[] = []
	const retryEvictions: { app: string; index: number }[] = []
	out.forEach((res, k) => {
		const c = checks[k]
		if (!c || changesOf(res) > 0) return
		if (c.kind === 'trial') retryTrials.push(c.key)
		else if (c.kind === 'course') {
			// 새 코스를 넣으려 했는데 다른 요청이 먼저 만들었다
			if (c.insert) runs.forEach((run, i) => {
				if (run && run.course === c.course) results[i].created = false
			})
			retryCourses.push(c.course)
		} else retryEvictions.push({ app: c.app, index: c.index })
	})
	for (const key of retryTrials) {
		const { app, row } = dirty.get(key)!
		const a = adds.get(key)!
		let done = false
		for (let attempt = 0; attempt < NAV_RETRIES && !done; attempt++) {
			const cur = await db.prepare(SQL_TRIAL_ROW).bind(account.key, app, row.course).first<CollectionRecord>()
			if (!cur) {
				const res = await db.prepare(SQL_INSERT_TRIAL).bind(account.key, account.kind, app, row.course, row.steps, a.runs, a.premium, a.discount, JSON.stringify(a.best), JSON.stringify(row.last), a.lastDay, row.touched).run()
				done = changesOf(res) > 0
				continue
			}
			const stored = JSON.parse(cur.best)
			const best = a.bestTotal < stored[stored.length - 1] ? a.best : stored
			const res = await db.prepare(SQL_UPDATE_TRIAL).bind(account.key, app, row.course, cur.best, a.runs, a.premium, a.discount, JSON.stringify(best), JSON.stringify(row.last), a.lastDay, row.touched).run()
			done = changesOf(res) > 0
		}
		if (!done) throw new Error(NAV_CONFLICT)
	}
	for (const course of retryCourses) {
		const run = changed.get(course)!
		const premium = run.type === 1 ? 1 : 0
		const discount = run.type === -1 ? 1 : 0
		let done = false
		for (let attempt = 0; attempt < NAV_RETRIES && !done; attempt++) {
			const cur = await db.prepare(SQL_COURSE_ROW).bind(course).first<CourseRecord>()
			if (!cur) {
				const res = await db.prepare(SQL_INSERT_COURSE).bind(course, run.app, JSON.stringify(run.steps), 1, premium, discount, JSON.stringify(run.splits), JSON.stringify(run.splits), run.day).run()
				done = changesOf(res) > 0
				continue
			}
			const record = JSON.parse(cur.record)
			const sums = JSON.parse(cur.sums)
			const nextRecord = run.total < record[record.length - 1] ? run.splits : record
			const nextSums = sums.map((s: number, k: number) => s + (run.splits[k] ?? 0))
			const res = await db.prepare(SQL_UPDATE_COURSE).bind(course, cur.record, cur.sums, premium, discount, JSON.stringify(nextRecord), JSON.stringify(nextSums), run.day).run()
			done = changesOf(res) > 0
		}
		if (!done) throw new Error(NAV_CONFLICT)
	}
	for (const { app, index } of retryEvictions) {
		// 밀어내려던 코스를 그 사이 다른 요청이 다시 달렸다: 콜렉션을 다시 읽어 넘치면 이 요청이 쓰지 않은 것 중 가장 오래된 것을 밀어낸다
		const written = new Set([...dirty.values()].filter((d) => d.app === app).map((d) => d.row.course))
		let done = false
		for (let attempt = 0; attempt < NAV_RETRIES && !done; attempt++) {
			const rows = (await db.prepare(SQL_COLLECTION).bind(account.key, app).all<CollectionRecord>()).results
			const candidates = rows.filter((r) => !written.has(r.course))
			if (rows.length <= TRIAL_POLICY.maxCourses || candidates.length === 0) {
				done = true
				break
			}
			const oldestRow = candidates.reduce((a, b) => (b.touched < a.touched ? b : a))
			const res = await db.prepare(SQL_EVICT_IF).bind(account.key, app, oldestRow.course, oldestRow.touched).run()
			if (changesOf(res) > 0) {
				results[index].evicted = oldestRow.course
				done = true
			}
		}
		if (!done) throw new Error(NAV_CONFLICT)
	}
	return results
}`;

export default [
  // ---- fix-wallet-replay ----
  {
    id: "fix-wallet-replay", worker: "www", file: AUTH,
    find: "export async function verifyWalletRequest(request: Request, body: Uint8Array, nowMs: number = Date.now()): Promise<string | null> {",
    replace: "export async function verifyWalletRequest(request: Request, body: Uint8Array, nowMs: number = Date.now(), db?: D1Database): Promise<string | null> {",
  },
  {
    id: "fix-wallet-replay", worker: "www", file: AUTH,
    find: "\ttry {\n\t\treturn ethers.verifyMessage(message, signature).toLowerCase() === address ? address : null\n\t} catch {\n\t\treturn null\n\t}\n}",
    replace: [
      "\tlet signer: string",
      "\ttry {",
      "\t\tsigner = ethers.verifyMessage(message, signature).toLowerCase()",
      "\t} catch {",
      "\t\treturn null",
      "\t}",
      "\tif (signer !== address) return null",
      "\tif (db && !(await claimWalletNonce(db, message, timeMs, nowMs))) return null",
      "\treturn address",
      "}",
      "",
      "// 받은 서명(서명한 글의 SHA-256)을 서명 시각 + 5분까지 기억한다. 바뀐 행이 1 이면 처음 쓰는 서명이다. 기한 지난 키는 요청마다 10개까지 지운다",
      "async function claimWalletNonce(db: D1Database, message: string, timeMs: number, nowMs: number): Promise<boolean> {",
      "\tconst nonce = await sha256Hex(new TextEncoder().encode(message))",
      "\tconst out = await db.batch([",
      "\t\tdb.prepare('DELETE FROM wallet_nonces WHERE \"nonce\" IN (SELECT \"nonce\" FROM wallet_nonces WHERE \"expires_at\" < ?1 LIMIT 10)').bind(nowMs),",
      "\t\tdb.prepare('INSERT INTO wallet_nonces (\"nonce\", \"expires_at\") VALUES (?1, ?2) ON CONFLICT DO NOTHING').bind(nonce, timeMs + WALLET_MAX_SKEW_MS),",
      "\t])",
      "\treturn Number(out[1]?.meta?.changes ?? 0) === 1",
      "}",
    ].join("\n"),
  },
  {
    id: "fix-wallet-replay", worker: "www", file: NAV,
    find: "const address = await verifyWalletRequest(request, body, nowMs)",
    replace: "const address = await verifyWalletRequest(request, body, nowMs, env.DB)",
  },
  {
    id: "fix-wallet-replay", worker: "www", file: ANALYTIC,
    find: "? await verifyWalletRequest(request, body, nowMs)",
    replace: "? await verifyWalletRequest(request, body, nowMs, env.DB)",
  },
  // ---- fix-nav-lost-update ----
  {
    id: "fix-nav-lost-update", worker: "www", file: NAV,
    find: "out.set(r.course, { ...r, record: JSON.parse(r.record), sums: JSON.parse(r.sums) })",
    replace: "out.set(r.course, { ...r, record: JSON.parse(r.record), sums: JSON.parse(r.sums), stored: { record: r.record, sums: r.sums } })",
  },
  {
    id: "fix-nav-lost-update", worker: "www", file: NAV,
    find: /\/\/ 런들을 계정의 콜렉션에 넣는다\.[^\n]*\r?\n\/\/ 계정 없는[^\n]*\r?\nexport async function storeRuns\([\s\S]*?\n\tif \(statements\.length > 0\) await db\.batch\(statements\)\r?\n\treturn results\r?\n\}/g,
    replace: STORE_RUNS,
  },
];
