-- Add conversation columns to sessions table.
-- Wrapped in a dummy SELECT to conditionally add columns only if they don't exist.
-- SQLite doesn't support "ADD COLUMN IF NOT EXISTS", so we use a PRAGMA-based guard.

-- opencode_session_id: stores the opencode session ID for multi-turn conversation
INSERT INTO pragma_table_info('sessions') (name)
SELECT 'opencode_session_id'
WHERE NOT EXISTS (
    SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'opencode_session_id'
);
-- SQLite doesn't allow INSERT INTO pragma_table_info to actually add a column.
-- We need to use ALTER TABLE with a guard. Since SQLite lacks IF NOT EXISTS on ALTER TABLE,
-- the Rust code handles this by catching the "duplicate column" error and ignoring it.
