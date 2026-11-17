-- 평가 대기열: 원문·해시·벡터를 먼저 저장하고, cron이 욕설 검사를 거친 뒤 evaluations와 Vectorize로 옮긴다.
-- 처리가 끝나면 text와 vector는 지운다 (rejected·failed도 마찬가지).
CREATE TABLE IF NOT EXISTS eval_queue (
    "id"          TEXT PRIMARY KEY,           -- 저장될 때 evaluations.id와 같은 UUID v7
    "request_id"  TEXT NOT NULL UNIQUE,       -- 멱등 키 (앱이 만든 값)
    "from"        TEXT NOT NULL,
    "to"          TEXT,
    "cc"          TEXT NOT NULL,
    "ref"         TEXT,                       -- 부모 평가 id (저장된 평가 또는 대기열 항목)
    "root"        TEXT NOT NULL,
    "text"        TEXT,                       -- 원문. 처리 후 NULL
    "text_hash"   TEXT NOT NULL,              -- SHA-256 hex (보내온 원문 기준)
    "vector"      BLOB,                       -- Float32 x 768 (3072 bytes). 처리 후 NULL
    "state"       TEXT NOT NULL,              -- pending | processing | rejected | failed
    "attempts"    INTEGER NOT NULL DEFAULT 0,
    "lease_until" INTEGER,                    -- processing 상태의 만료 시각(ms). 지나면 다른 실행이 다시 집어 간다
    "last_error"  TEXT,
    "created_at"  INTEGER NOT NULL,
    "updated_at"  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_eval_queue_state_created ON eval_queue ("state", "created_at");
