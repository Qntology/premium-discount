-- Phase 4 Part 1: 1계정 다중 우회 표 증폭 차단(nav_route_ballots) 및 목적지 조건부 평판(nav_route_fit_counts).
-- ballot_id = hashId("nav-ballot/2|" || account.key || app || CP(goal) || CP(g_k))
-- 코스 ID 가 달라도 같은 (계정, 앱, 결승, 대상 체크포인트)에는 1표만 허용하고, 의견 번복 시 이전 부호를 상쇄 갱신한다.
CREATE TABLE IF NOT EXISTS nav_route_ballots (
	"id" TEXT PRIMARY KEY,
	"sign" INTEGER NOT NULL CHECK ("sign" IN (-1, 1)),
	"day" INTEGER NOT NULL
);

-- 목적지 조건부 경로 적합도 집계: 특정 결승(goal)으로 가는 길에서 체크포인트(hash)가 선택(+1)·우회(-1)된 횟수.
-- 요소 자체 고유 마찰(dead_end_marks 등)은 기존 nav_counts(hash)에 분리 기록한다.
CREATE TABLE IF NOT EXISTS nav_route_fit_counts (
	"hash" TEXT NOT NULL,
	"goal" TEXT NOT NULL,
	"day" INTEGER NOT NULL,
	"type" INTEGER NOT NULL CHECK ("type" IN (-1, 1)),
	"recipe" TEXT NOT NULL,
	"n" INTEGER NOT NULL,
	PRIMARY KEY ("hash", "goal", "day", "type", "recipe")
);
CREATE INDEX IF NOT EXISTS idx_nav_route_fit_lookup ON nav_route_fit_counts ("recipe", "goal", "hash");
