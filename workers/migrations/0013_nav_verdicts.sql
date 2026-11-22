-- Phase 5 Part 1: PD-Verdict v2 (nav 트랙) 공식 판정 테이블 · 목적지 동류 3단계 스코프 · 14일 에폭 하트비트.
CREATE TABLE IF NOT EXISTS nav_verdicts (
	"hash" TEXT PRIMARY KEY,
	"kind" TEXT NOT NULL CHECK ("kind" IN ('course', 'checkpoint')),
	"app" TEXT NOT NULL,
	"scope_to" TEXT,
	"evidence" REAL NOT NULL,
	"value" REAL NOT NULL,
	"up" INTEGER NOT NULL DEFAULT 0,
	"down" INTEGER NOT NULL DEFAULT 0,
	"level" TEXT,
	"rank" REAL,
	"tier" TEXT NOT NULL,
	"grade" INTEGER,
	"tier_since" INTEGER NOT NULL,
	"reasons" TEXT NOT NULL DEFAULT '[]',
	"recipe" TEXT NOT NULL,
	"superseded" INTEGER NOT NULL DEFAULT 0,
	"updated_at" INTEGER NOT NULL,
	"judged_at" INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nav_verdicts_scope ON nav_verdicts ("kind", "app", "scope_to", "tier");
CREATE INDEX IF NOT EXISTS idx_nav_verdicts_updated ON nav_verdicts ("updated_at");

CREATE TABLE IF NOT EXISTS nav_verdict_scopes (
	"scope_key" TEXT PRIMARY KEY,
	"n" INTEGER NOT NULL,
	"mean" REAL NOT NULL,
	"sd" REAL NOT NULL,
	"updated_at" INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS nav_verdict_dirty (
	"hash" TEXT PRIMARY KEY,
	"kind" TEXT NOT NULL CHECK ("kind" IN ('course', 'checkpoint')),
	"app" TEXT NOT NULL,
	"scope_to" TEXT,
	"at" INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nav_verdict_dirty_at ON nav_verdict_dirty ("at");

-- 반감기 14일 에폭당 1회 생존 재확증(Epoch-Renewable Heartbeat):
-- nav_courses.runs(고유 완주자 수 >= 11)는 nav_registrations 로 영구 1회만 가산하되,
-- 14일 이상 지난 다음 에폭 재주행 시 nav_counts 에 생존 관측 1건 갱신을 허용한다.
CREATE TABLE IF NOT EXISTS nav_epoch_pulses (
	"id" TEXT PRIMARY KEY,
	"epoch" INTEGER NOT NULL,
	"day" INTEGER NOT NULL,
	"sign" INTEGER NOT NULL CHECK ("sign" IN (-1, 1))
);
