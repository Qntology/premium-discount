-- bcc 컬럼 활성화: 앱이 보내는 콘텐츠 해시 주소(hashId, 0x+40자리)를 대기열과 evaluations에 저장한다.
-- evaluations.bcc는 0002부터 있었지만 항상 NULL이었다. 대기열에는 이 컬럼이 없어서 추가한다.
-- 적용 순서: 0004 -> 0005 -> (그 뒤에) www와 cron을 배포한다. cron의 SELECT가 eval_queue.bcc를 읽으므로
-- 마이그레이션 전에 cron을 먼저 배포하면 대기열 처리가 전부 실패한다.
ALTER TABLE eval_queue ADD COLUMN "bcc" TEXT;

-- 같은 콘텐츠를 이미 평가했는지 찾는 용도 (NULL은 제외)
CREATE INDEX IF NOT EXISTS idx_evaluations_bcc ON evaluations ("bcc") WHERE "bcc" IS NOT NULL;
