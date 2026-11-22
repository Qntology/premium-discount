-- Phase 4 Part 2: 서버 갈림길 대결 집계(nav_shortcut_duels)와 3인 합의 체크포인트 선행 승격(Pioneer Promotion).
-- duel_reg_id = hashId("nav-duel/1|" || account.key || fork_chain_id || CP(taken) || CP(skipped))
-- 한 계정은 동일 갈림길의 특정 (taken, skipped) 대결에 평생 1회만 기여한다.
CREATE TABLE IF NOT EXISTS nav_shortcut_duel_regs (
	"id" TEXT PRIMARY KEY,
	"day" INTEGER NOT NULL
);

-- 풀코스 공개 임계치(publishMinRuns = 11) 도달 전이라도, 독립 계정 3명 이상이 동일 갈림길에서
-- 평균 margin >= 0.15 및 saved_ms >= 1,500ms 단축을 입증하면 taken 체크포인트를 선행 승격한다.
CREATE TABLE IF NOT EXISTS nav_shortcut_duels (
	"fork_course" TEXT NOT NULL,
	"skipped_cp" TEXT NOT NULL,
	"taken_cp" TEXT NOT NULL,
	"app" TEXT NOT NULL,
	"wins" INTEGER NOT NULL DEFAULT 0,
	"saved_ms_sum" INTEGER NOT NULL DEFAULT 0,
	"margin_sum" REAL NOT NULL DEFAULT 0.0,
	"promoted" INTEGER NOT NULL DEFAULT 0,
	"last_day" INTEGER NOT NULL,
	PRIMARY KEY ("fork_course", "skipped_cp", "taken_cp")
);
CREATE INDEX IF NOT EXISTS idx_nav_shortcut_duels_app ON nav_shortcut_duels ("app", "promoted", "wins");
