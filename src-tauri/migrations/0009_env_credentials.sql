-- 环境多用户凭证：一个环境可录多个用户（密码或私钥），其中一个默认。
-- 默认凭证 = Friday 日常连接（连接池 / run_command / jvm_*）使用的 SSH 用户。
-- 密钥本体不入库，存 OS keychain：friday/env/{env_id}/cred/{cred_id}
CREATE TABLE IF NOT EXISTS env_credentials (
    id TEXT PRIMARY KEY,
    environment_id TEXT NOT NULL,
    username TEXT NOT NULL,
    auth_type TEXT NOT NULL DEFAULT 'private_key',
    private_key_path TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_env_credentials_env ON env_credentials(environment_id);
