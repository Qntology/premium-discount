-- 신구 대체 (premium-discount-relay `Relation::Supersedes`): 같은 장소(cc) · 대상(to)의 새 평가가 옛 평가의 주장에서 수치 · 단위 ·
-- 형용사만 바꿨다("피해 20% → 15%", "배송비 3,000원 → 무료"). 비교는 Stanza · 임베딩이 있는 쪽(Raon Zena 의 판정 보조, 앱)이 하고
-- www 가 POST /evaluations/supersedes 로 받아 검증해 기록한다. cron 은 대체된 평가(old_id)를 판정 입력 superseded 로 다뤄
-- 근거와 상관없이 discount(grade -2, 이유 superseded)로 둔다.
-- 적용 순서: 0009 -> (그 뒤에) www와 cron을 배포한다. cron의 판정 조회가 이 테이블을 읽는다.

CREATE TABLE IF NOT EXISTS eval_supersedes (
    "old_id"     TEXT NOT NULL,              -- 대체된 평가 (evaluations.id)
    "new_id"     TEXT NOT NULL,              -- 대체한 평가 (evaluations.id, cc · to 가 같고 created_at 이 더 늦음)
    "reporter"   TEXT NOT NULL,              -- 보고한 확인된 로그인 주소 (두 평가 중 하나의 작성자)
    "request_id" TEXT NOT NULL UNIQUE,       -- 멱등 키 (보고한 쪽이 만든 값)
    "created_at" INTEGER NOT NULL,
    PRIMARY KEY ("old_id", "new_id")
);
CREATE INDEX IF NOT EXISTS idx_eval_supersedes_old ON eval_supersedes ("old_id");
