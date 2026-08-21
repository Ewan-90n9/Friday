# 基础设施

## 凭证管理

- **SSH 私钥**：引用用户已有的 `~/.ssh/` 路径，不复制不存储；Friday 自管理的 key 路径记录在 SQLite，key 本身仍在用户 ssh 目录。
- **LLM API key / 目标机密码**：存 OS 密钥链（Windows Credential Manager / macOS Keychain / Linux Secret Service），通过 Rust `keyring` crate 跨平台封装。SQLite 里只存环境标识。
- **其余配置**（环境信息、连接参数等）：明文入 SQLite。内网工具，安全要求从简。

## 日志与可观测

- **Friday 运行日志**：`tracing` + `tracing-appender` 文件轮转，写入 Tauri app data dir 下的 `logs/` 目录。INFO 为主，关键路径 DEBUG。
- **诊断过程数据**：会话/步骤/工具调用/结果持久化到 SQLite，供用户回看历史诊断。
- 两者分离，互不污染。
- 详细规范见 [日志规范（强制约束）](logging-standard.md)。
