CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    env TEXT NOT NULL,
    service TEXT NOT NULL,
    symptom TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    closed_at TEXT
);

CREATE TABLE IF NOT EXISTS diagnosis_steps (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    step_type TEXT NOT NULL,
    content TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    args TEXT,
    risk_level TEXT NOT NULL,
    status TEXT NOT NULL,
    output TEXT,
    raw_stdout TEXT,
    elapsed_ms INTEGER,
    error TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS environments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT,
    port INTEGER,
    user TEXT,
    transport_type TEXT NOT NULL,
    k8s_namespace TEXT,
    k8s_pod TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_diagnosis_steps_session ON diagnosis_steps(session_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);
