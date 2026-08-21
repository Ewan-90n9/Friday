# 基础设施

## 凭证管理

- **SSH 私钥**：引用用户已有的 `~/.ssh/` 路径，不复制不存储；Friday 自管理的 key 路径记录在 SQLite，key 本身仍在用户 ssh 目录。
- **LLM API key / 目标机密码**：存 OS 密钥链（Windows Credential Manager / macOS Keychain / Linux Secret Service），通过 Rust `keyring` crate 跨平台封装。SQLite 里只存环境标识。
- **其余配置**（环境信息、连接参数等）：明文入 SQLite。内网工具，安全要求从简。

## 文件布局

所有运行时文件统一在 Tauri `app_data_dir()`（identifier `com.friday.app`）下，通过 `infra/paths.rs` 集中解析，不内联 `join`。

```
<app_data>/
├── friday.db                        # SQLite: sessions/agents/diagnosis_steps/tool_calls/environments
├── logs/
│   └── friday.log.{date}            # tracing 每日轮转, 7 天自动清理
├── playbooks/                       # agent 运行时生成的诊断知识 (YAML), 用户可编辑
├── skills/                          # Friday 自有 skill (agent 生成, 能力包)
├── prompts/                         # 预留: 未来 GUI 编辑人格 prompt 的覆盖层
│                                    #   v1 为空 — 代码内 const 作默认; 有 friday.md 则覆盖
└── artifacts/                       # 从目标机拉取的产物, 按会话隔离, 持久保留
    └── <session_id>/
```

**不纳入 Friday 文件管理的边界**：

| 项 | 归属 | 说明 |
|----|------|------|
| opencode 工作环境/skill | 用户 `~/.opencode/` | Friday 不管理，spawn 设 PWD=home 复用 |
| SSH 私钥 | 用户 `~/.ssh/` | 引用不复制 |
| 凭证（密码/API key） | OS 密钥链 | 不落文件 |
| migrations | 编译进二进制 `include_str!` | 非运行时文件 |
| 真正临时文件 | `std::env::temp_dir()` | 若出现纯临时需求，不持久化 |

## 日志与可观测

- **Friday 运行日志**：`tracing` + `tracing-appender` 文件轮转，写入 `Paths::log_dir()`（即 `<app_data>/logs/`）。INFO 为主，关键路径 DEBUG。
- **诊断过程数据**：会话/步骤/工具调用/结果持久化到 SQLite，供用户回看历史诊断。
- 两者分离，互不污染。
- 详细规范见 [日志规范（强制约束）](logging-standard.md)。
