-- 멱등 키: 앱이 같은 저장을 재시도해도 한 번만 저장되도록 한다. NULL(옛 클라이언트)은 유니크 검사에서 제외한다.
ALTER TABLE evaluations ADD COLUMN "request_id" TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_evaluations_request_id ON evaluations ("request_id") WHERE "request_id" IS NOT NULL;
