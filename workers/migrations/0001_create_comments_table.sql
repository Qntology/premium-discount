CREATE TABLE IF NOT EXISTS items (
    "id" TEXT PRIMARY KEY,
    "type" TEXT,
    "from" TEXT,
    "to" TEXT,
    "cc" TEXT,
    "inf" TEXT,
    "ref" TEXT,
    "digest" TEXT,
    "data" BLOB NULL,
    "created_at" INTEGER,
    "updated_at" INTEGER
);

CREATE TABLE IF NOT EXISTS crons (
    "id" TEXT PRIMARY KEY,
    "cc" TEXT,
    "bcc" TEXT,
    "job" BLOB NULL,
    "ref" TEXT,
    "created_at" INTEGER,
    "updated_at" INTEGER
);

CREATE TABLE IF NOT EXISTS console (
    "id" TEXT PRIMARY KEY,
    "bcc" TEXT,
    "log" TEXT,
    "created_at" INTEGER
);