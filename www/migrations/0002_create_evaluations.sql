-- 평가 시스템: 주소/도메인 메타데이터만 저장 (원문, 벡터는 저장하지 않음. 벡터는 Vectorize)
CREATE TABLE IF NOT EXISTS evaluations (
    "id"         TEXT PRIMARY KEY,           -- UUID v7, Vectorize ID와 동일
    "from"       TEXT NOT NULL,              -- 작성자 주소 (클라이언트 값 또는 게스트 주소)
    "to"         TEXT,                       -- 팀/수신 주소, 없으면 NULL
    "cc"         TEXT NOT NULL,              -- 등록 도메인 (예: example.com)
    "bcc"        TEXT,                       -- 보류 (현재 항상 NULL)
    "ref"        TEXT,                       -- 부모 평가 id (단일), 최상위면 NULL
    "root"       TEXT NOT NULL,              -- 스레드 최상위 id (최상위 자신이면 자기 id)
    "created_at" INTEGER NOT NULL,           -- Date.now() 밀리초
    "updated_at" INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_evaluations_ref        ON evaluations ("ref");
CREATE INDEX IF NOT EXISTS idx_evaluations_root       ON evaluations ("root");
CREATE INDEX IF NOT EXISTS idx_evaluations_cc_created ON evaluations ("cc", "created_at");
