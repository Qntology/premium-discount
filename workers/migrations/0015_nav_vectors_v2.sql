-- Phase 5 Part 3: 최초 선점 오염을 깨뜨리는 2-단계 신뢰 합의 대표 벡터(Consensus EMA Medoid).
-- confirmations == 1 (provisional) -> 독립 후발 주자와 cos >= 0.88 합의 시 EMA 평활화 & confirmations += 1 -> >= 3 시 locked = 1.
-- cos < 0.65 충돌 시 교체 후보(candidate_vector)에 2인 이상 상호 일치하면 오염 초기 벡터를 자동 교체한다.
ALTER TABLE nav_vectors ADD COLUMN "confirmations" INTEGER NOT NULL DEFAULT 1;
ALTER TABLE nav_vectors ADD COLUMN "locked" INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nav_vectors ADD COLUMN "candidate_vector" BLOB;
ALTER TABLE nav_vectors ADD COLUMN "candidate_hits" INTEGER NOT NULL DEFAULT 0;
