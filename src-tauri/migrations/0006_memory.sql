-- Session summaries (bound to session, cascade delete)
CREATE TABLE IF NOT EXISTS session_summaries (
    session_id TEXT PRIMARY KEY,
    summary_text TEXT NOT NULL,
    generated_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- Experiences (fully independent, no session reference)
CREATE TABLE IF NOT EXISTS experiences (
    id TEXT PRIMARY KEY,
    symptom TEXT NOT NULL,
    service TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'unknown',
    root_cause TEXT,
    investigation_path TEXT NOT NULL DEFAULT '',
    experience_lesson TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL CHECK(outcome IN ('positive', 'negative', 'uncertain')),
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    last_seen_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    query_text TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_experiences_outcome ON experiences(outcome);
