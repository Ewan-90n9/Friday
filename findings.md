# 发现与决策

## 需求
- 仅 tracing 运行日志规范（不覆盖诊断数据持久化）
- 动态级别调整（无需重启即可切换 INFO/DEBUG/TRACE）
- 7 天日志保留
- panic 写入日志文件
- stderr 捕获（当前被丢弃）
- 关键入口 #[instrument]（多会话并发串联）
- 不截断、不脱敏
- 人类可读格式（维持 fmt::layer 默认）

## 研究发现

### 当前日志架构
- `infra/logging.rs`（23 行）：tracing 双输出（stdout + 每日轮转文件），EnvFilter，WorkerGuard
- 26 处 tracing 宏，集中在 lifecycle.rs / spawn.rs / stream.rs
- 默认级别 debug，RUST_LOG 可覆盖
- 无 #[instrument]、无 span、无 panic hook、无文件清理、无动态级别

### 关键代码位置
| 文件 | 行号 | 要点 |
|------|------|------|
| `infra/logging.rs:6` | `init()` 返回 `WorkerGuard` | 需改为返回 `LoggingGuard` |
| `infra/logging.rs:12-13` | `EnvFilter::try_from_default_env()` | 需用 reload::Layer 包装 |
| `lib.rs:14-18` | `AppState` struct | 需加 `filter_handle` 字段 |
| `lib.rs:28` | `init(data_dir)` 调用 | 需适配新返回类型 |
| `lib.rs:37` | `app.manage(guard)` | 仍需保活 WorkerGuard |
| `lifecycle.rs:52-56` | `send_message_cmd` 签名 | `session_id: Option<String>` |
| `lifecycle.rs:57-61` | 手动 `info!("send_message_cmd called")` | 需移除，由 instrument 替代 |
| `lifecycle.rs:114` | `spawn_active(&pool, prompt_text, oc_session_id)` | 需加 `friday_session_id.clone()` 参数 |
| `spawn.rs:63-67` | `spawn_active` 签名 | 需加 `session_id: String` 参数 |
| `stream.rs:190` | `let AgentProcess { mut child, stdout, .. } = agent;` | stderr 被 `..` 忽略 |
| `stream.rs:204` | `raw = %&line[..line.len().min(200)]` | 截断 200 字符，需去掉 |
| `stream.rs:180-186` | `consume_stream` 签名 | agent/bus/pool/agents/cancel 都不实现 Debug |
| `events.rs:70-72` | `emit` 成功路径无日志 | 需加 debug! |
| `ipc.ts:1-50` | 前端 IPC 绑定 | 需加 setLogLevel |
| `Cargo.toml:18` | `tracing-subscriber` with `env-filter` | reload 是内置模块，无需额外 feature |

### instrument skip 清单（实际需要的）
| 函数 | skip 参数 | 保留参数 |
|------|----------|----------|
| `send_message_cmd` | `state` | `session_id`(Option), `message` |
| `stop_agent_cmd` | `state` | `session_id` |
| `close_session_cmd` | `state` | `session_id` |
| `spawn_active` | `pool` | `session_id`, `message`, `opencode_session_id` |
| `consume_stream` | `agent, bus, pool, agents, cancel` | `session_id` |
| `detect_agents_cmd` | `state` | — |

### stderr 捕获方案
- `consume_stream` 解构时取出 `stderr: ChildStderr`
- 在 stdout 循环前 `tokio::spawn` 一个独立 task：
  ```rust
  let stderr_sid = session_id.clone();
  let stderr_handle = tokio::spawn(async move {
      use tokio::io::{AsyncBufReadExt, BufReader};
      let reader = BufReader::new(stderr);
      let mut lines = reader.lines();
      while let Ok(Some(line)) = lines.next_line().await {
          tracing::warn!(session_id = %stderr_sid, raw = %line, "stderr line");
      }
  });
  ```
- 函数末尾（child.wait 之后）`let _ = stderr_handle.await;`

### reload::Handle 用法
```rust
use tracing_subscriber::reload;

let filter = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("debug"));
let (filter_layer, filter_handle) = reload::Layer::new(filter);

tracing_subscriber::registry()
    .with(filter_layer)
    .with(fmt::layer().with_writer(std::io::stdout))
    .with(fmt::layer().with_writer(non_blocking))
    .init();
```

`set_level`:
```rust
pub fn set_level(handle: &reload::Handle<EnvFilter>, level: &str) -> Result<(), String> {
    let new_filter = EnvFilter::new(level);
    handle.reload(new_filter).map_err(|e| e.to_string())?;
    tracing::info!(new_level = level, "log level changed");
    Ok(())
}
```

### panic hook
```rust
let prev_hook = std::panic::take_hook();
std::panic::set_hook(Box::new(move |info| {
    let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
    let payload = info.payload().downcast_ref::<&str>().copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("panic payload");
    tracing::error!(location = %location, payload = %payload, "panic");
    prev_hook(info);
}));
```

### cleanup_old_logs
```rust
fn cleanup_old_logs(log_dir: &Path, max_days: u64) {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(max_days * 86400);
    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let path = entry.path();
                        tracing::debug!(path = %path.display(), "removing old log file");
                        if let Err(e) = std::fs::remove_file(&path) {
                            tracing::warn!(?e, path = %path.display(), "failed to remove old log file");
                        }
                    }
                }
            }
        }
    }
}
```

## 技术决策
| 决策 | 理由 |
|------|------|
| LoggingGuard 持有 WorkerGuard + Handle | 一个结构体管理两个生命周期 |
| filter_handle 存入 AppState | Tauri command 通过 State<AppState> 访问 |
| set_log_level_cmd 放在 lifecycle.rs | 与其他 app 级 command 一致 |
| spawn_active 新增 session_id 参数 | 让 span 能传播 session_id |
| stderr 用独立 tokio task | 与 stdout 循环并行，互不阻塞 |
| send_message_cmd 不声明 fields(session_id) | 参数是 Option<String>，实际 ID 在函数内确定 |
| consume_stream skip 5 个参数 | agent/bus/pool/agents/cancel 均不实现 Debug |

## 遇到的问题
| 问题 | 解决方案 |
|------|---------|
|      |         |

## 资源
- spec: `docs/superpowers/specs/2026-08-21-logging-standard-design.md`
- tracing reload docs: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/reload/
- tracing instrument docs: https://docs.rs/tracing/latest/tracing/attr.instrument.html

## 视觉/浏览器发现
<!-- 关键：每执行2次查看/浏览器操作后必须更新此部分 -->
<!-- 多模态内容必须立即以文本形式记录 -->
-

---
*每执行2次查看/浏览器/搜索操作后更新此文件*
*防止视觉信息丢失*
