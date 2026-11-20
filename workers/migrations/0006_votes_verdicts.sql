-- 투표와 판정 (PD-Verdict v1, cron/src/verdict.ts). www가 투표를 받아 기록하고, cron이 근거량(E)·값(v)을 계산해 등급을 매긴다.
-- 적용 순서: 0006 -> (그 뒤에) www와 cron을 배포한다 (0005와 같다). www의 투표·판정 조회·검색과 cron의 processVerdicts가
-- 이 테이블들을 읽고 쓰므로, 마이그레이션 전에 배포하면 투표 저장과 검색, 판정 처리가 전부 실패한다.

-- 투표 기록: 추가만 한다. (평가, 투표자, 종류)마다 가장 최근 행(created_at, 같으면 id 순)이 유효한 표다. 값 0 = 철회
-- 행을 덮어쓰지 않는 이유: 늦게 도착한 옛 request_id의 재시도가 새 표를 되돌리면 안 된다 (재시도는 request_id로 찾아 그대로 답한다)
CREATE TABLE IF NOT EXISTS eval_votes (
    "id"          TEXT PRIMARY KEY,          -- UUID v7
    "request_id"  TEXT NOT NULL UNIQUE,      -- 멱등 키 (앱이 만든 값)
    "eval_id"     TEXT NOT NULL,             -- evaluations.id (저장이 끝난 평가만)
    "voter"       TEXT NOT NULL,             -- 0x 주소
    "voter_kind"  TEXT NOT NULL,             -- session(확인된 로그인) | guest
    "kind"        TEXT NOT NULL,             -- vote | vote_outcome
    "value"       INTEGER NOT NULL,          -- 1 추천 | -1 비추천 | 0 철회
    "created_at"  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_eval_votes_latest ON eval_votes ("eval_id", "voter", "kind", "created_at");

-- 판정 결과: 평가마다 한 행. cron만 쓰고 www는 읽기만 한다. 행이 없으면 아직 판정 전이다 (unrated로 보여 준다)
CREATE TABLE IF NOT EXISTS eval_verdicts (
    "eval_id"    TEXT PRIMARY KEY,           -- evaluations.id
    "cc"         TEXT NOT NULL,              -- 범위(기준선) 계산용 사본 (evaluations.cc)
    "to"         TEXT,                       -- 범위 계산용 사본 (evaluations.to), 없으면 NULL
    "evidence"   REAL NOT NULL,              -- 근거량 E (updated_at 시점의 감쇠 반영)
    "value"      REAL NOT NULL,              -- 값 v (-1..1)
    "up"         INTEGER NOT NULL,           -- 세어진(가중치 > 0) 최근 추천 수
    "down"       INTEGER NOT NULL,           -- 세어진 최근 비추천 수
    "level"      TEXT,                       -- 쓴 기준선 단계 secondary | primary | global, 없으면 NULL
    "rank"       REAL,                       -- 기준선 대비 순위 r (-1..1), 기준선이 없으면 NULL
    "tier"       TEXT NOT NULL,              -- premium | neutral | discount | unrated
    "grade"      INTEGER,                    -- 2 | 1 | 0 | -1 | -2, unrated면 NULL
    "tier_since" INTEGER NOT NULL,           -- 지금 등급이 시작된 시각(ms). 등급이 그대로면(머무르기 포함) 바뀌지 않는다
    "reasons"    TEXT NOT NULL,              -- 사유 JSON 배열 (예: ["rank_high","level:secondary"])
    "recipe"     TEXT NOT NULL,              -- 판정 규칙 (pd-verdict/1)
    "updated_at" INTEGER NOT NULL,           -- E·v를 계산한 시각
    "judged_at"  INTEGER NOT NULL            -- 등급을 매긴 시각
);
CREATE INDEX IF NOT EXISTS idx_eval_verdicts_scope ON eval_verdicts ("cc", "to", "tier");
-- cron의 sweep(가장 오래전에 계산한 행부터 다시 계산)이 매 실행 전체를 정렬하지 않도록
CREATE INDEX IF NOT EXISTS idx_eval_verdicts_updated ON eval_verdicts ("updated_at");

-- 기준선 스냅샷: 범위마다 판정 가능한(E >= min_evidence) 평가들의 v 분포. cron이 건드린 범위만 다시 계산한다
CREATE TABLE IF NOT EXISTS verdict_scopes (
    "scope_key"  TEXT PRIMARY KEY,           -- 'cc:<cc>|to:<to>'(secondary) | 'cc:<cc>'(primary) | '*'(global)
    "n"          INTEGER NOT NULL,           -- 표본 수
    "mean"       REAL NOT NULL,              -- v 평균
    "sd"         REAL NOT NULL,              -- v 모집단 표준편차
    "updated_at" INTEGER NOT NULL
);

-- 다시 계산할 평가: 투표가 들어오면 www가 표시하고, cron이 계산을 마치면 지운다
CREATE TABLE IF NOT EXISTS verdict_dirty (
    "eval_id" TEXT PRIMARY KEY,              -- evaluations.id
    "at"      INTEGER NOT NULL               -- 마지막 표시 시각(ms). cron은 읽어 둔 at 이하일 때만 지운다 (그 사이 들어온 표의 표시는 남는다)
);
