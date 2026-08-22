CREATE TABLE IF NOT EXISTS session_messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT,
    status TEXT,
    seq INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session_message_parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    part_type TEXT NOT NULL,
    seq INTEGER NOT NULL,
    text TEXT,
    tool_name TEXT,
    tool_args TEXT,
    tool_status TEXT,
    tool_output TEXT,
    tool_elapsed_ms INTEGER,
    FOREIGN KEY (message_id) REFERENCES session_messages(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_messages_session ON session_messages(session_id);
CREATE INDEX IF NOT EXISTS idx_session_message_parts_message ON session_message_parts(message_id);
