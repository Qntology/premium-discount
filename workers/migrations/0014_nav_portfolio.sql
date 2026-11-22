-- Phase 5 Part 2 + Phase 7 Part 4: 다양성 보존 4-쿼터 공개 조회 · 대표 기록 차분 치환 · Pinned-SLRU · 배치 멱등성.
ALTER TABLE nav_courses ADD COLUMN "start_cp" TEXT NOT NULL DEFAULT '';
ALTER TABLE nav_courses ADD COLUMN "goal_cp" TEXT NOT NULL DEFAULT '';
ALTER TABLE nav_courses ADD COLUMN "pace_ms" INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nav_courses ADD COLUMN "interaction_traits" INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nav_courses ADD COLUMN "ema_splits" TEXT;
ALTER TABLE nav_courses ADD COLUMN "era_record" TEXT;
ALTER TABLE nav_courses ADD COLUMN "era_record_day" INTEGER NOT NULL DEFAULT 0;

-- 계정별 대표 기록 스냅샷: 재주행 개인 최고 단축 시 nav_courses.runs 증가 없이 sums 차분 치환 및 record 갱신.
ALTER TABLE nav_registrations ADD COLUMN "rep_splits" TEXT;
ALTER TABLE nav_registrations ADD COLUMN "rep_type" INTEGER NOT NULL DEFAULT 0;

-- 다중 기기 동시성 단조 병합용 총시간 컬럼 및 수동 핀 보호 플래그.
ALTER TABLE nav_trials ADD COLUMN "best_total_ms" INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nav_trials ADD COLUMN "pinned" INTEGER NOT NULL DEFAULT 0;

-- POST /nav/trials 배치 재전송 멱등 영수증(24시간 TTL, cron 정리).
CREATE TABLE IF NOT EXISTS nav_batch_receipts (
	"batch_id" TEXT PRIMARY KEY,
	"account" TEXT NOT NULL,
	"response_json" TEXT NOT NULL,
	"created_ms" INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_nav_courses_entry ON nav_courses ("app", "start_cp", "runs");
CREATE INDEX IF NOT EXISTS idx_nav_courses_pace ON nav_courses ("app", "pace_ms");
CREATE INDEX IF NOT EXISTS idx_nav_courses_traits ON nav_courses ("app", "interaction_traits", "runs");
CREATE INDEX IF NOT EXISTS idx_nav_batch_receipts_created ON nav_batch_receipts ("created_ms");
