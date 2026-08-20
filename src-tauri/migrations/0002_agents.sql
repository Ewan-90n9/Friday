CREATE TABLE IF NOT EXISTS agents (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    display_name TEXT NOT NULL,
    path         TEXT NOT NULL,
    version      TEXT,
    source       TEXT NOT NULL,
    is_active    INTEGER NOT NULL DEFAULT 0,
    detected_at  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_agents_active ON agents(is_active) WHERE is_active = 1;
