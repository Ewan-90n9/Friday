-- 服务 → 代码仓映射记忆（code_* 工具）。
-- repo_url 权威；last_ref 仅提示（分支经常变，agent 每次仍须与用户确认）。
CREATE TABLE IF NOT EXISTS service_repos (
    service      TEXT PRIMARY KEY,
    repo_url     TEXT NOT NULL,
    last_ref     TEXT,
    updated_at   TEXT NOT NULL,
    last_used_at TEXT NOT NULL
);
