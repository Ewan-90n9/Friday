# 进度日志

## 会话：2026-08-21

### 阶段 1：logging.rs 核心重写
- **状态：** pending
- **开始时间：** —
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/infra/logging.rs`

### 阶段 2：lib.rs + AppState 集成
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/lib.rs`
  - `src-tauri/src/app/lifecycle.rs`（set_log_level_cmd）

### 阶段 3：agent/stream.rs — stderr 捕获 + instrument + 去截断
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/agent/stream.rs`

### 阶段 4：agent/spawn.rs — instrument + session_id 参数
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/agent/spawn.rs`

### 阶段 5：app/lifecycle.rs — instruments + entry logs
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/app/lifecycle.rs`

### 阶段 6：app/agents.rs — instrument + entry logs
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/app/agents.rs`

### 阶段 7：app/events.rs — emit debug 日志
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src-tauri/src/app/events.rs`

### 阶段 8：前端 ipc.ts — setLogLevel 绑定
- **状态：** pending
- 执行的操作：
  -
- 创建/修改的文件：
  - `src/lib/ipc.ts`

### 阶段 9：验证
- **状态：** pending
- 执行的操作：
  - `cargo check --manifest-path src-tauri/Cargo.toml`
  - `cargo test --manifest-path src-tauri/Cargo.toml`
  - `pnpm typecheck`

## 测试结果
| 测试 | 输入 | 预期结果 | 实际结果 | 状态 |
|------|------|---------|---------|------|
|      |      |         |         |      |

## 错误日志
| 时间戳 | 错误 | 尝试次数 | 解决方案 |
|--------|------|---------|---------|
|        |      | 1       |         |

## 五问重启检查
| 问题 | 答案 |
|------|------|
| 我在哪里？ | 阶段 1（未开始） |
| 我要去哪里？ | 阶段 2-9 |
| 目标是什么？ | 落地 tracing 日志规范：动态级别、7 天清理、panic hook、stderr 捕获、instrument、去截断、全 command 埋点 |
| 我学到了什么？ | 见 findings.md |
| 我做了什么？ | 完成设计 spec + 创建实现计划 |

---
*每个阶段完成后或遇到错误时更新此文件*
