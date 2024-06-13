-- -- Add up migration script here
CREATE TABLE IF NOT EXISTS pow_service (
    id CHAR(80) PRIMARY KEY,
    status INTEGER NOT NULL,
    data TEXT,
    ts TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
) WITHOUT ROWID;