# 堆快照分析（heap_* 工具 + MAT 工人进程）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把堆快照分析做成 Friday 原生 MCP 工具（9 个 `heap_*`），底层托管 vendored jvm-heap-dump-mcp JAR（MAT 内核）作为 JVM 工人进程，agent 无需用户手动开 MAT 即可完成 leak suspects / 支配树 / GC root 链级别的根因分析。

**Architecture:** Friday MCP server 的 ToolRegistry 注册 9 个 heap_* 工具（全 ReadOnly，与 jvm_* 同款形态）；`analyzer/` 引擎层以 rmcp client（stdio `TokioChildProcess`）驱动工人进程，管理会话（local_path 为主键、watch 通知合流、LRU 上限 3、空闲 15min 退出、崩溃自愈）；TransferManager 在 heap dump 拉回 completed 后触发自动预热（MAT 建索引）。

**Tech Stack:** Rust (tokio, rmcp 3.x 新增 `client` + `transport-child-process` + `transport-async-rw` features), Tauri resources (vendored JAR), 上游 [Djaler/jvm-heap-dump-mcp v0.2.0](https://github.com/Djaler/jvm-heap-dump-mcp)（MIT）。

**Spec:** [docs/superpowers/specs/2026-08-29-heap-analysis-design.md](../specs/2026-08-29-heap-analysis-design.md)

---

## 全局约定

- **TDD 铁律**：每个行为先写失败测试 → 运行确认失败原因正确 → 最小实现 → 运行通过 → 提交。**Task 1 / Task 8 / Task 10 是配置/接线/文档任务（TDD 例外，已在任务内注明），其余任务必须走红绿循环。**
- 测试命令统一：`cargo test --manifest-path src-tauri/Cargo.toml <模块过滤>`；全量：`cargo test --manifest-path src-tauri/Cargo.toml`。
- 每个任务结束时 `cargo check --manifest-path src-tauri/Cargo.toml` 必须干净（无 warning）。
- 日志规范：manager 启动/退出/崩溃 `info!`/`error!`（工人 stderr 逐行记录）；工具调用入口 `info!`；错误路径 `warn!`/`error!`。
- rmcp 3.x 个别签名若与计划有出入（如 `peer()`/`cancel()` 返回形态），以 `cargo check` 编译错误为准微调调用方式，**不改变行为语义**。

## 与 spec 的偏差（调研上游源码后确定）

1. **`heap_histogram` 无 `group_by_classloader` 参数**——上游 `get_class_histogram` 不支持；改为暴露上游真实能力 `sort_by`（retained_heap/shallow_heap/objects）+ `filter`（类名正则）。
2. **`heap_threads` 增加可选 `filter`**（线程名正则）——上游 `get_threads` 原生支持，对 OOM 排查高价值。
3. **JAR 不进 git**——仓库惯例（resources/model 也是构建时下载、gitignore），改用 `scripts/fetch-analyzer-jar.ps1` 构建时获取；分发上仍是 vendored 进安装包（tauri resources），运行时零下载。CI（release.yml）新增下载步骤（同 embedding model 先例）。
4. **上游锁 v0.2.0**；上游输出为 markdown 文本（非 JSON），Friday 原样透传 + 落盘。

## 关键事实（上游源码调研结论）

- 上游工具（MCP name → 关键参数）：
  - `open_heap_dump(path, id?)` — **支持调用方自定义 session id**（Friday 传 UUID，无需解析返回）
  - `close_heap_dump(id)`、`get_leak_suspects(id)`
  - `get_class_histogram(id, sortBy?: RETAINED_HEAP|SHALLOW_HEAP|OBJECTS, filter?, limit?=50)`
  - `get_dominator_tree(id, limit?=30)` / `get_dominator_tree_children(id, objectId, limit?=30)`
  - `get_object_info(id, objectId)`、`get_path_to_gc_roots(id, objectId, limit?=10)`
  - `get_outbound_references(id, objectId, limit?=50)` / `get_inbound_references(id, objectId, limit?=50)`
  - `get_threads(id, filter?, sortBy?)`
  - `objectId` 为整数；输出均为 markdown 表格文本。
- rmcp：`TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn() -> io::Result<(TokioChildProcess, Option<ChildStderr>)>`（**默认 stderr 是 inherit，必须显式 piped 并读取**，否则 JVM stderr 可能塞满管道死锁）；client 模式 `().serve(transport).await?`；`service.peer().clone()` 后 `peer.call_tool(CallToolRequestParam { name, arguments })`；`CallToolResult { content: Vec<ContentBlock>, is_error: Option<bool> }`（本项目 `src/mcp/server.rs` 已用 `CallToolResult::success/error` + `ContentBlock::text`，是本地权威参照）。
- MAT 在 hprof 旁生成索引文件，进程重启后 open 命中索引秒级恢复（无需 Friday 额外处理）。

## 文件结构

```
新建：
scripts/fetch-analyzer-jar.ps1                  # 构建时下载 JAR（Task 1）
src-tauri/src/analyzer/mod.rs                   # 模块入口 + 常量 + re-exports
src-tauri/src/analyzer/java.rs                  # Java 21+ 探测（Task 2）
src-tauri/src/analyzer/client.rs                # client trait + mock + rmcp stdio 实现（Task 3）
src-tauri/src/analyzer/session.rs               # dump 会话映射（watch phase，Task 4）
src-tauri/src/analyzer/manager.rs               # 生命周期 + open 合流 + LRU + 空闲 + 预热（Task 5）
src-tauri/src/tools/builtin/heap/mod.rs         # 9 个工具 defs + handler（Task 6）
src-tauri/src/tools/builtin/heap/mapping.rs     # Friday 参数 → 上游工具/参数 纯映射（Task 6）
src-tauri/resources/analyzer/.gitkeep           # JAR 目录占位（Task 1）

修改：
src-tauri/Cargo.toml                            # rmcp features（Task 1）
src-tauri/tauri.conf.json                       # resources（Task 1）
.gitignore                                      # JAR gitignore（Task 1）
src-tauri/src/transfer/mod.rs                   # DownloadCompleteHook（Task 7）
src-tauri/src/lib.rs                            # 装配 + AppState（Task 8）
src-tauri/src/app/lifecycle.rs                  # 会话关闭联动（Task 8）
src-tauri/src/tools/builtin/mod.rs              # pub mod heap（Task 6）
src-tauri/src/agent/prompt.rs                   # TOOL_GUIDANCE（Task 9）
src-tauri/src/tools/builtin/jvm/heap_dump.rs    # 描述引导 heap_*（Task 9）
.github/workflows/release.yml                   # CI 下载 JAR（Task 10）
docs/architecture/overview.md                   # 工具层列表（Task 10）
docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md  # §9 表（Task 10）
AGENTS.md                                       # 已实现功能（Task 10）
```

---

### Task 1: JAR vendoring 基础设施（配置任务，TDD 例外——构建配置）

**Files:**
- Create: `scripts/fetch-analyzer-jar.ps1`
- Create: `src-tauri/resources/analyzer/.gitkeep`
- Modify: `src-tauri/Cargo.toml:37`（rmcp 行）
- Modify: `src-tauri/tauri.conf.json:28-31`（resources）
- Modify: `.gitignore`（追加）

- [ ] **Step 1: 创建下载脚本** `scripts/fetch-analyzer-jar.ps1`：

```powershell
param(
    [string]$Version = "0.2.0"
)
$ErrorActionPreference = "Stop"
$url = "https://github.com/Djaler/jvm-heap-dump-mcp/releases/download/v$Version/jvm-heap-dump-mcp-$Version-all.jar"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\analyzer"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir "jvm-heap-dump-mcp-$Version-all.jar"
if (Test-Path $dest) {
    Write-Host "JAR already present: $dest"
    exit 0
}
Write-Host "Downloading $url"
$tmp = "$dest.downloading"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
Move-Item $tmp $dest
Write-Host "Downloaded: $dest ($((Get-Item $dest).Length) bytes)"
```

- [ ] **Step 2: 占位与 gitignore**：创建空文件 `src-tauri/resources/analyzer/.gitkeep`；`.gitignore` 末尾追加：

```
# Heap analyzer vendored JAR (fetched at build time, see scripts/fetch-analyzer-jar.ps1)
src-tauri/resources/analyzer/*.jar
```

- [ ] **Step 3: Cargo features**：`src-tauri/Cargo.toml` 中 rmcp 行改为：

```toml
rmcp = { version = "3", features = ["server", "macros", "transport-streamable-http-server", "client", "transport-child-process", "transport-async-rw"] }
```

- [ ] **Step 4: tauri resources**：`src-tauri/tauri.conf.json` 的 `bundle.resources` 改为：

```json
    "resources": [
      "resources/model/*",
      "resources/model/onnx/*",
      "resources/analyzer/*"
    ],
```

- [ ] **Step 5: 下载 JAR 并验证**

Run: `./scripts/fetch-analyzer-jar.ps1`
Expected: `Downloaded: ...\src-tauri\resources\analyzer\jvm-heap-dump-mcp-0.2.0-all.jar (~29,xxx,xxx bytes)`

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过

- [ ] **Step 6: Commit**

```bash
git add scripts/fetch-analyzer-jar.ps1 src-tauri/resources/analyzer/.gitkeep src-tauri/Cargo.toml src-tauri/tauri.conf.json .gitignore
git commit -m "chore: vendor heap analyzer jar fetch script and build config"
```

---

### Task 2: Java 探测（analyzer/java.rs）

**Files:**
- Create: `src-tauri/src/analyzer/mod.rs`
- Create: `src-tauri/src/analyzer/java.rs`
- Modify: `src-tauri/src/lib.rs:1-10`（加 `mod analyzer;`）

- [ ] **Step 1: 模块骨架**：`src-tauri/src/analyzer/mod.rs`：

```rust
pub mod java;
```

`src-tauri/src/lib.rs` 顶部模块声明区（`mod agent;` 之后）加：

```rust
mod analyzer;
```

- [ ] **Step 2: 写失败测试**：`src-tauri/src/analyzer/java.rs`：

```rust
use std::path::PathBuf;

pub struct JavaInfo {
    pub path: PathBuf,
    pub major: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_modern_versions() {
        assert_eq!(parse_java_version(r#"openjdk version "21.0.3" 2024-04-16"#), Some(21));
        assert_eq!(parse_java_version(r#"openjdk version "17.0.2" 2022-01-18"#), Some(17));
        assert_eq!(parse_java_version(r#"openjdk version "25" 2025-09-16"#), Some(25));
        // BiSheng JDK 打印标准格式
        assert_eq!(parse_java_version(r#"openjdk version "21.0.11" 2025-01-21"#), Some(21));
    }

    #[test]
    fn test_parse_legacy_1_8_format() {
        assert_eq!(parse_java_version(r#"java version "1.8.0_391""#), Some(8));
        assert_eq!(parse_java_version(r#"java version "1.8.0_391" Java(TM) SE"#), Some(8));
    }

    #[test]
    fn test_parse_garbage_returns_none() {
        assert_eq!(parse_java_version(""), None);
        assert_eq!(parse_java_version("xyz"), None);
        assert_eq!(parse_java_version("Runtime Environment (build 25+36)"), None);
    }

    #[test]
    fn test_java_candidates_prefers_java_home_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        std::fs::write(bin.join(exe), "").unwrap();

        let cands = java_candidates(Some(tmp.path().to_str().unwrap()));
        assert_eq!(cands.first().unwrap(), &bin.join(exe));

        // JAVA_HOME 不存在 → 不在候选里
        let cands = java_candidates(Some("C:/definitely/not/here"));
        assert!(cands.iter().all(|p| !p.to_string_lossy().contains("not/here")));
    }
}
```

- [ ] **Step 3: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::java`
Expected: 编译失败 `cannot find function parse_java_version` / `java_candidates`

- [ ] **Step 4: 最小实现**：java.rs 在 `use` 区后、tests 前补：

```rust
/// 解析 `java -version` 输出主版本号。处理两种格式：
/// - 现代格式 `openjdk version "21.0.3"` → 21
/// - 旧格式 `java version "1.8.0_391"` → 8（主版本 1 时取次版本）
pub fn parse_java_version(output: &str) -> Option<u32> {
    let rest = output.split("version").nth(1)?;
    let quoted = rest.split('"').nth(1)?;
    if quoted.is_empty() {
        return None;
    }
    let mut parts = quoted.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    match first {
        1 => {
            let second = parts.next()?;
            let digits: String = second.chars().take_while(|c| c.is_ascii_digit()).collect();
            let v: u32 = digits.parse().ok()?;
            if v == 0 { None } else { Some(v) }
        }
        v => Some(v),
    }
}

/// 候选 java 路径：JAVA_HOME/bin/java 优先（文件存在才入列），其次 PATH（which 解析）
pub fn java_candidates(java_home: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = java_home {
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        let p = PathBuf::from(home).join("bin").join(exe);
        if p.is_file() {
            out.push(p);
        }
    }
    if let Ok(p) = which::which("java") {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// 探测 Java 21+：逐候选执行 `java -version`。Err 附带可读原因（含探测到的版本号）。
pub async fn detect_java() -> Result<JavaInfo, String> {
    let candidates = java_candidates(std::env::var("JAVA_HOME").ok().as_deref());
    let mut last_err = String::from("未找到 java 可执行文件（已检查 JAVA_HOME 与 PATH）");
    for path in candidates {
        match probe_version(&path).await {
            Ok(Some(v)) if v >= 21 => return Ok(JavaInfo { path, major: v }),
            Ok(Some(v)) => last_err = format!("找到 {} 但为 Java {v}，需要 21+", path.display()),
            Ok(None) => last_err = format!("无法解析 {} 的版本输出", path.display()),
            Err(e) => last_err = format!("执行 {} -version 失败: {e}", path.display()),
        }
    }
    Err(last_err)
}

async fn probe_version(java_path: &std::path::Path) -> Result<Option<u32>, String> {
    let out = tokio::process::Command::new(java_path)
        .arg("-version")
        .output()
        .await
        .map_err(|e| e.to_string())?;
    // `java -version` 惯例输出到 stderr，stdout 兜底
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stdout));
    Ok(parse_java_version(&text))
}
```

- [ ] **Step 5: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::java`
Expected: 4 个测试 PASS

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/analyzer/ src-tauri/src/lib.rs
git commit -m "feat: java 21+ detection for heap analyzer"
```

---

### Task 3: 分析器 client（trait + mock + rmcp stdio 实现）

**Files:**
- Create: `src-tauri/src/analyzer/client.rs`
- Modify: `src-tauri/src/analyzer/mod.rs`

- [ ] **Step 1: 写失败测试**：创建 `src-tauri/src/analyzer/client.rs`：

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;

/// 一次上游工具调用结果（上游输出为 markdown 文本）
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

#[async_trait]
pub trait HeapAnalyzerClient: Send + Sync {
    /// 调用上游 MCP 工具。Err = 传输/进程层错误（进程疑似死亡）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    /// 终止工人进程
    async fn shutdown(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_joins_text_blocks() {
        let result = rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text("hello"),
            rmcp::model::ContentBlock::text("world"),
        ]);
        assert_eq!(extract_text(&result), "hello\nworld");
    }

    #[test]
    fn test_extract_text_empty_content() {
        let result = rmcp::model::CallToolResult::success(vec![]);
        assert_eq!(extract_text(&result), "");
    }

    #[tokio::test]
    async fn test_mock_client_records_calls() {
        let mock = MockHeapAnalyzerClient::ok("S");
        let out = mock.call_tool("open_heap_dump", &serde_json::json!({"path": "x"})).await;
        assert!(out.is_ok());
        assert_eq!(out.unwrap().text, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "open_heap_dump");
    }
}
```

- [ ] **Step 2: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::client`
Expected: 编译失败 `cannot find extract_text` / `MockHeapAnalyzerClient`

- [ ] **Step 3: 实现**：client.rs 在 trait 定义后、tests 前补：

```rust
/// 从 CallToolResult 提取全部 text 内容块（拼接）
pub fn extract_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match &block.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// rmcp stdio 子进程实现：java -Xmx<n>g -jar <jar>，MCP client 角色
pub struct McpHeapAnalyzerClient {
    service: Arc<rmcp::service::RunningService<rmcp::RoleClient, ()>>,
    peer: rmcp::service::Peer<rmcp::RoleClient>,
}

/// 启动工人进程并完成 MCP 握手（60s 超时）
pub async fn spawn_analyzer_client(
    java: &crate::analyzer::java::JavaInfo,
    jar_path: &Path,
    xmx_gb: u32,
) -> Result<McpHeapAnalyzerClient, String> {
    use rmcp::ServiceExt;

    let mut cmd = tokio::process::Command::new(&java.path);
    cmd.arg(format!("-Xmx{xmx_gb}g")).arg("-jar").arg(jar_path);
    let (transport, stderr) =
        rmcp::transport::child_process::TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动分析器进程失败: {e}"))?;

    // 日志规范：子进程 stderr 必须读取记录（同时防止管道写满阻塞 JVM）
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!(target: "heap_analyzer", "worker: {line}");
            }
        });
    }

    tracing::info!(java = %java.path.display(), jar = %jar_path.display(), xmx_gb, pid = ?transport.id(), "heap analyzer worker spawning");
    let service = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ().serve(transport),
    )
    .await
    .map_err(|_| "分析器进程初始化超时（60s）".to_string())?
    .map_err(|e| format!("分析器 MCP 握手失败: {e}"))?;

    let peer = service.peer().clone();
    Ok(McpHeapAnalyzerClient { service: Arc::new(service), peer })
}

#[async_trait]
impl HeapAnalyzerClient for McpHeapAnalyzerClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        use rmcp::service::ClientServiceExt;

        let mut arguments = serde_json::Map::new();
        if let Value::Object(map) = args {
            for (k, v) in map {
                arguments.insert(k.clone(), v.clone());
            }
        }
        let result = self
            .peer
            .call_tool(rmcp::model::CallToolRequestParam {
                name: name.into(),
                arguments: Some(arguments),
            })
            .await
            .map_err(|e| format!("MCP 调用失败: {e}"))?;
        Ok(CallOutcome {
            text: extract_text(&result),
            is_error: result.is_error.unwrap_or(false),
        })
    }

    async fn shutdown(&self) {
        if let Err(e) = self.service.cancel().await {
            tracing::warn!(?e, "heap analyzer service cancel failed");
        }
        tracing::info!("heap analyzer worker shut down");
    }
}

// ── 测试 mock（全 crate 测试可用）──

#[cfg(test)]
pub struct MockHeapAnalyzerClient {
    pub calls: Arc<tokio::sync::Mutex<Vec<(String, Value)>>>,
    pub shutdown_count: Arc<std::sync::atomic::AtomicUsize>,
    handler: Arc<
        dyn Fn(&str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallOutcome, String>> + Send>>
            + Send
            + Sync,
    >,
}

#[cfg(test)]
impl MockHeapAnalyzerClient {
    pub fn with_fn<F, Fut>(f: F) -> Self
    where
        F: Fn(&str, &Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<CallOutcome, String>> + Send + 'static,
    {
        Self {
            calls: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            shutdown_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            handler: Arc::new(move |name, args| Box::pin(f(name, args))),
        }
    }

    /// 所有调用成功返回固定文本
    pub fn ok(text: &str) -> Self {
        let text = text.to_string();
        Self::with_fn(move |_name, _args| {
            let text = text.clone();
            async move { Ok(CallOutcome { text, is_error: false }) }
        })
    }
}

#[cfg(test)]
#[async_trait]
impl HeapAnalyzerClient for MockHeapAnalyzerClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        self.calls.lock().await.push((name.to_string(), args.clone()));
        (self.handler)(name, args).await
    }

    async fn shutdown(&self) {
        self.shutdown_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
```

> 注：若 `block.raw` / `RawContent` 与 rmcp 3.x 实际形态不符（`cargo check` 会指出），按编译错误调整 match 分支——`ContentBlock::text()` 构造已在 `src/mcp/server.rs` 验证可用，提取逻辑语义不变。

`src-tauri/src/analyzer/mod.rs` 更新为：

```rust
pub mod client;
pub mod java;
```

- [ ] **Step 4: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::client`
Expected: 3 个测试 PASS

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 干净（rmcp client API 编译通过；若个别签名不符按编译错误微调）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/analyzer/
git commit -m "feat: heap analyzer client trait, mock and rmcp stdio client"
```

---

### Task 4: dump 会话映射（analyzer/session.rs）

**Files:**
- Create: `src-tauri/src/analyzer/session.rs`
- Create: `src-tauri/src/analyzer/manager.rs`（本任务仅 ManagerError 类型，Task 5 扩展）
- Modify: `src-tauri/src/analyzer/mod.rs`

- [ ] **Step 1: ManagerError 前置**：创建 `src-tauri/src/analyzer/manager.rs`（仅类型）：

```rust
#[derive(Debug, Clone, thiserror::Error)]
pub enum ManagerError {
    #[error("{0}")]
    JavaMissing(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("分析调用超时（{0}s），工人进程保留未受影响")]
    Timeout(u64),
    #[error("该 dump 尚未打开")]
    NotOpen { warming: bool },
    #[error("{0}")]
    Upstream(String),
}
```

`src-tauri/src/analyzer/mod.rs` 更新为：

```rust
pub mod client;
pub mod java;
pub mod manager;
pub mod session;
```

- [ ] **Step 2: 写失败测试**：创建 `src-tauri/src/analyzer/session.rs`：

```rust
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::sync::watch;

/// 同时打开的 dump 会话上限（LRU 逐出）
pub const MAX_OPEN_DUMPS: usize = 3;

/// 单个 dump 的会话状态。watch 通道广播状态变迁（多等待者合流）。
#[derive(Debug, Clone)]
pub enum EntryPhase {
    Warming,
    Ready { summary: String },
    Failed { error: crate::analyzer::manager::ManagerError },
}

pub struct DumpEntry {
    pub analyzer_session_id: String,
    phase_tx: std::sync::Arc<watch::Sender<EntryPhase>>,
    pub last_touched: Instant,
}

#[derive(Default)]
pub struct DumpSessions {
    entries: std::collections::HashMap<PathBuf, DumpEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed() -> EntryPhase {
        EntryPhase::Failed { error: crate::analyzer::manager::ManagerError::Unavailable("boom".into()) }
    }

    #[test]
    fn test_begin_starts_warming_and_returns_receiver() {
        let mut s = DumpSessions::new();
        let (rx, victim) = s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        assert!(victim.is_none());
        assert_eq!(s.len(), 1);
        assert!(matches!(*rx.borrow(), EntryPhase::Warming));
        assert!(matches!(s.phase(Path::new("/a.hprof")), Some(EntryPhase::Warming)));
    }

    #[test]
    fn test_set_phase_ready_notifies_receiver() {
        let mut s = DumpSessions::new();
        let (rx, _) = s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        assert!(s.set_phase(Path::new("/a.hprof"), EntryPhase::Ready { summary: "SUM".into() }));
        assert!(matches!(*rx.borrow(), EntryPhase::Ready { .. }));
        assert!(!s.set_phase(Path::new("/nope"), EntryPhase::Ready { summary: String::new() }));
    }

    #[test]
    fn test_set_phase_failed_keeps_entry_for_waiters() {
        let mut s = DumpSessions::new();
        let (rx, _) = s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        s.set_phase(Path::new("/a.hprof"), failed());
        assert!(matches!(*rx.borrow(), EntryPhase::Failed { .. }));
        assert_eq!(s.len(), 1, "failed entry kept so waiters can read the error");
    }

    #[test]
    fn test_remove_returns_analyzer_id() {
        let mut s = DumpSessions::new();
        s.begin(PathBuf::from("/a.hprof"), "id-1".into());
        assert_eq!(s.remove(Path::new("/a.hprof")).as_deref(), Some("id-1"));
        assert_eq!(s.remove(Path::new("/a.hprof")), None);
        assert!(s.is_empty());
    }

    #[test]
    fn test_evict_lru_picks_oldest_ready_and_skips_warming() {
        let mut s = DumpSessions::new();
        for (p, id) in [("/a.hprof", "a"), ("/b.hprof", "b"), ("/c.hprof", "c")] {
            s.begin(PathBuf::from(p), id.into());
            s.set_phase(Path::new(p), EntryPhase::Ready { summary: "S".into() });
        }
        // b 重新 begin（转 Warming，不可逐出）
        s.begin(PathBuf::from("/b.hprof"), "b2".into());
        let victim = s.evict_lru();
        assert_eq!(
            victim.map(|(p, id)| (p.display().to_string(), id)),
            Some(("/a.hprof".to_string(), "a".to_string()))
        );
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn test_touch_affects_lru_order() {
        let mut s = DumpSessions::new();
        for (p, id) in [("/a.hprof", "a"), ("/b.hprof", "b")] {
            s.begin(PathBuf::from(p), id.into());
            s.set_phase(Path::new(p), EntryPhase::Ready { summary: "S".into() });
        }
        s.touch(Path::new("/a.hprof")); // a 最新 → 逐出 b
        let (p, id) = s.evict_lru().unwrap();
        assert_eq!((p.display().to_string(), id.as_str()), ("/b.hprof".to_string(), "b"));
    }

    #[test]
    fn test_remove_under_dir_scopes_by_prefix() {
        let mut s = DumpSessions::new();
        let base = Path::new("/artifacts/sess-1");
        for (p, id) in [
            ("/artifacts/sess-1/a.hprof", "a"),
            ("/artifacts/sess-1/b.hprof", "b"),
            ("/artifacts/sess-2/c.hprof", "c"),
        ] {
            s.begin(PathBuf::from(p), id.into());
        }
        let removed = s.remove_under_dir(base);
        assert_eq!(removed.len(), 2);
        assert!(removed.iter().all(|(p, _)| p.starts_with(base)));
        assert_eq!(s.len(), 1);
        assert_eq!(s.analyzer_id(Path::new("/artifacts/sess-2/c.hprof")).as_deref(), Some("c"));
    }
}
```

- [ ] **Step 3: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::session`
Expected: 编译失败 `no method named begin/phase/set_phase/... found for struct DumpSessions`

- [ ] **Step 4: 实现**：session.rs 在 struct 定义后、tests 前补：

```rust
impl DumpSessions {
    pub fn new() -> Self {
        Self { entries: std::collections::HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 新建 warming 条目（覆盖同路径旧条目，Failed 重试路径）。返回 (phase 订阅者, LRU 逐出受害者)。
    /// 超过上限时逐出最久未访问的 Ready 条目。
    pub fn begin(
        &mut self,
        path: PathBuf,
        analyzer_session_id: String,
    ) -> (watch::Receiver<EntryPhase>, Option<(PathBuf, String)>) {
        let (tx, rx) = watch::channel(EntryPhase::Warming);
        self.entries.insert(
            path.clone(),
            DumpEntry {
                analyzer_session_id,
                phase_tx: std::sync::Arc::new(tx),
                last_touched: Instant::now(),
            },
        );
        let victim = if self.entries.len() > MAX_OPEN_DUMPS { self.evict_lru() } else { None };
        (rx, victim)
    }

    pub fn phase(&self, path: &Path) -> Option<EntryPhase> {
        self.entries.get(path).map(|e| e.phase_tx.borrow().clone())
    }

    pub fn receiver(&self, path: &Path) -> Option<watch::Receiver<EntryPhase>> {
        self.entries.get(path).map(|e| e.phase_tx.subscribe())
    }

    pub fn analyzer_id(&self, path: &Path) -> Option<String> {
        self.entries.get(path).map(|e| e.analyzer_session_id.clone())
    }

    /// 落定 phase（Warming → Ready/Failed）并刷新 LRU 时间；条目不存在（已被 close/逐出）→ false
    pub fn set_phase(&mut self, path: &Path, phase: EntryPhase) -> bool {
        match self.entries.get_mut(path) {
            Some(e) => {
                e.phase_tx.send_replace(phase);
                e.last_touched = Instant::now();
                true
            }
            None => false,
        }
    }

    pub fn touch(&mut self, path: &Path) {
        if let Some(e) = self.entries.get_mut(path) {
            e.last_touched = Instant::now();
        }
    }

    /// 移除条目，返回 analyzer_session_id（供上游 close）
    pub fn remove(&mut self, path: &Path) -> Option<String> {
        self.entries.remove(path).map(|e| e.analyzer_session_id)
    }

    /// LRU 逐出：移除最久未访问的 Ready 条目（Warming 不逐出）
    pub fn evict_lru(&mut self) -> Option<(PathBuf, String)> {
        let victim = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(*e.phase_tx.borrow(), EntryPhase::Ready { .. }))
            .min_by_key(|(_, e)| e.last_touched)
            .map(|(p, _)| p.clone())?;
        let entry = self.entries.remove(&victim)?;
        Some((victim, entry.analyzer_session_id))
    }

    /// 移除 base 目录下全部条目（Friday 会话关闭联动）
    pub fn remove_under_dir(&mut self, base: &Path) -> Vec<(PathBuf, String)> {
        let victims: Vec<PathBuf> = self
            .entries
            .keys()
            .filter(|p| p.starts_with(base))
            .cloned()
            .collect();
        victims
            .into_iter()
            .filter_map(|p| self.remove(&p).map(|id| (p, id)))
            .collect()
    }
}
```

- [ ] **Step 5: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::session`
Expected: 7 个测试 PASS

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 干净

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/analyzer/
git commit -m "feat: dump session map with watch-based phase"
```

---

### Task 5: HeapAnalyzerManager（生命周期 + open 合流 + LRU + 空闲退出 + 崩溃自愈 + 预热）

**Files:**
- Modify: `src-tauri/src/analyzer/manager.rs`（ManagerError 保留，追加实现）

- [ ] **Step 1: 写失败测试**：manager.rs 末尾追加 tests 模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::client::{CallOutcome, HeapAnalyzerClient, MockHeapAnalyzerClient};
    use crate::app::events::EventBus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const GB: u64 = 1024 * 1024 * 1024;

    fn manager_with(
        mock: &Arc<MockHeapAnalyzerClient>,
        artifacts: &std::path::Path,
        config: ManagerConfig,
    ) -> (HeapAnalyzerManager, Arc<AtomicUsize>) {
        let spawns = Arc::new(AtomicUsize::new(0));
        let s2 = spawns.clone();
        let mock2 = mock.clone();
        let factory: ClientFactory = Arc::new(move |_xmx| {
            let mock = mock2.clone();
            let s2 = s2.clone();
            Box::pin(async move {
                s2.fetch_add(1, Ordering::SeqCst);
                let c: Arc<dyn HeapAnalyzerClient> = mock;
                Ok(c)
            })
        });
        (
            HeapAnalyzerManager::new(factory, EventBus::disabled(), artifacts.to_path_buf(), config),
            spawns,
        )
    }

    fn dump_file(dir: &std::path::Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "fake hprof").unwrap();
        p
    }

    async fn open_ready(mgr: &HeapAnalyzerManager, path: &std::path::Path) -> OpenOutcome {
        mgr.open(SID, path, 30).await.expect("open should succeed")
    }

    #[test]
    fn test_xmx_gb_for_matrix() {
        assert_eq!(xmx_gb_for(0), 4);
        assert_eq!(xmx_gb_for(GB), 4); // 1.5GB → ceil 2 → clamp 4
        assert_eq!(xmx_gb_for(3 * GB), 5); // 4.5 → 5
        assert_eq!(xmx_gb_for(6 * GB), 9);
        assert_eq!(xmx_gb_for(9 * GB), 12); // 13.5 → 14 → clamp 12
        assert_eq!(xmx_gb_for(100 * GB), 12);
    }

    #[tokio::test]
    async fn test_open_caches_summary_and_reuses_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("SUMMARY"));
        let (mgr, spawns) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        assert_eq!(open_ready(&mgr, &a).await.summary, "SUMMARY");
        assert_eq!(open_ready(&mgr, &a).await.summary, "SUMMARY");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1, "second open must hit Ready cache");
        drop(calls);
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_concurrent_open_dedups_to_single_upstream_call() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                    Ok(CallOutcome { text: "SUMMARY".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        let (r1, r2) = tokio::join!(mgr.open(SID, &a, 30), mgr.open(SID, &a, 30));
        assert_eq!(r1.unwrap().summary, "SUMMARY");
        assert_eq!(r2.unwrap().summary, "SUMMARY");
        let calls = mock.calls.lock().await;
        let opens = calls.iter().filter(|(n, _)| n == "open_heap_dump").count();
        assert_eq!(opens, 1, "concurrent opens must dedup to one upstream call");
    }

    #[tokio::test]
    async fn test_open_evicts_lru_when_exceeding_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let paths: Vec<PathBuf> = ["a.hprof", "b.hprof", "c.hprof", "d.hprof"]
            .iter()
            .map(|n| dump_file(tmp.path(), n))
            .collect();
        for p in &paths[..3] {
            open_ready(&mgr, p).await;
        }
        let o = open_ready(&mgr, &paths[3]).await;
        assert_eq!(o.evicted, vec![paths[0].clone()], "oldest ready session must be evicted");
        {
            let calls = mock.calls.lock().await;
            let a_open_id = calls
                .iter()
                .find(|(n, args)| n == "open_heap_dump" && args["path"].as_str().unwrap().ends_with("a.hprof"))
                .map(|(_, args)| args["id"].as_str().unwrap().to_string())
                .expect("a.hprof open call recorded");
            let closes: Vec<_> = calls.iter().filter(|(n, _)| n == "close_heap_dump").collect();
            assert_eq!(closes.len(), 1, "evicted session closed upstream");
            assert_eq!(closes[0].1["id"].as_str().unwrap(), a_open_id);
        }
        assert!(matches!(
            mgr.query(&paths[0], "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
    }

    #[tokio::test]
    async fn test_query_requires_ready_and_reports_warming() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "ok".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        // 未打开
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
        // 预热中
        let mgr2 = mgr.clone();
        let a2 = a.clone();
        let h = tokio::spawn(async move {
            mgr2.open(SID, &a2, 30).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: true })
        ));
        h.await.unwrap();
    }

    #[tokio::test]
    async fn test_query_routes_and_injects_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        mgr.query(&a, "get_class_histogram", &serde_json::json!({"limit": 5}), 5)
            .await
            .unwrap();
        let calls = mock.calls.lock().await;
        let open_id = calls[0].1["id"].as_str().unwrap();
        assert_eq!(calls[1].0, "get_class_histogram");
        assert_eq!(calls[1].1["id"].as_str().unwrap(), open_id);
        assert_eq!(calls[1].1["limit"], 5);
    }

    #[tokio::test]
    async fn test_query_upstream_tool_error_keeps_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "MAT error: bad query".into(), is_error: true })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        match mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await {
            Err(ManagerError::Upstream(text)) => assert!(text.contains("MAT error")),
            other => panic!("expected Upstream, got {other:?}"),
        }
        // 会话仍有效：open 命中缓存（无新增上游 open 调用）
        assert_eq!(open_ready(&mgr, &a).await.summary, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "open_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_query_transport_error_invalidates_and_respawns() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Err("transport closed".to_string())
                }
            }
        }));
        let (mgr, spawns) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::Unavailable(_))
        ));
        // 会话已全部失效 → 再查是 NotOpen 而非 Unavailable
        assert!(matches!(
            mgr.query(&a, "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "dead worker shut down");
        // 重新 open → 工厂重新拉起
        open_ready(&mgr, &a).await;
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_open_task_factory_failure_surfaces_error() {
        let tmp = tempfile::tempdir().unwrap();
        let factory: ClientFactory = Arc::new(|_xmx| {
            Box::pin(async { Err(ManagerError::JavaMissing("no java".into())) })
        });
        let mgr = HeapAnalyzerManager::new(
            factory,
            EventBus::disabled(),
            tmp.path().to_path_buf(),
            ManagerConfig::default(),
        );
        let a = dump_file(tmp.path(), "a.hprof");
        assert!(matches!(
            mgr.open(SID, &a, 5).await,
            Err(ManagerError::JavaMissing(_))
        ));
        // Failed 条目可重试（再次 open 仍是同错误，不死循环）
        assert!(matches!(
            mgr.open(SID, &a, 5).await,
            Err(ManagerError::JavaMissing(_))
        ));
    }

    #[tokio::test]
    async fn test_timeout_does_not_kill_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let hist_calls = Arc::new(AtomicUsize::new(0));
        let hc = hist_calls.clone();
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(move |name, _args| {
            let name = name.to_string();
            let hc = hc.clone();
            async move {
                if name == "open_heap_dump" {
                    Ok(CallOutcome { text: "S".into(), is_error: false })
                } else if name == "get_class_histogram" && hc.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    Ok(CallOutcome { text: "hist".into(), is_error: false })
                } else {
                    Ok(CallOutcome { text: "hist".into(), is_error: false })
                }
            }
        }));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        assert!(matches!(
            mgr.query(&a, "get_class_histogram", &serde_json::json!({}), 1).await,
            Err(ManagerError::Timeout(1))
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 0, "timeout must NOT kill worker");
        // 会话未被破坏：再次查询（快速路径）成功
        mgr.query(&a, "get_class_histogram", &serde_json::json!({}), 5)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_close_removes_and_calls_upstream_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        assert!(mgr.close(&a, 5).await.unwrap());
        {
            let calls = mock.calls.lock().await;
            assert_eq!(calls.iter().filter(|(n, _)| n == "close_heap_dump").count(), 1);
        }
        // 幂等：再次 close 返回 false 且不再上游调用
        assert!(!mgr.close(&a, 5).await.unwrap());
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "close_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_idle_exit_shuts_down_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, spawns) = manager_with(
            &mock,
            tmp.path(),
            ManagerConfig {
                idle_timeout: std::time::Duration::from_millis(150),
                idle_tick: std::time::Duration::from_millis(20),
            },
        );
        let a = dump_file(tmp.path(), "a.hprof");
        open_ready(&mgr, &a).await;
        mgr.close(&a, 5).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "idle worker must exit");
        // 空闲期间有会话则不退出
        let b = dump_file(tmp.path(), "b.hprof");
        open_ready(&mgr, &b).await;
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "worker with open session must stay");
        mgr.close(&b, 5).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 2);
        // 退出后再 open → 工厂重新拉起
        open_ready(&mgr, &a).await;
        assert_eq!(spawns.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_warm_up_opens_in_background() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let a = dump_file(tmp.path(), "a.hprof");
        mgr.warm_up(SID, &a).await;
        // warm_up 完成后 open 命中缓存（无新增上游调用）
        assert_eq!(open_ready(&mgr, &a).await.summary, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.iter().filter(|(n, _)| n == "open_heap_dump").count(), 1);
    }

    #[tokio::test]
    async fn test_close_for_friday_session_scoped_to_artifacts_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join("artifacts");
        let sid1 = "11111111-1111-1111-1111-111111111111";
        let sid2 = "22222222-2222-2222-2222-222222222222";
        let dir1 = crate::tools::builtin::run_command::artifact_dir_for(&artifacts, sid1);
        let dir2 = crate::tools::builtin::run_command::artifact_dir_for(&artifacts, sid2);
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir1.join("a.hprof"), "fake").unwrap();
        std::fs::write(dir2.join("b.hprof"), "fake").unwrap();

        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, &artifacts, ManagerConfig::default());
        open_ready(&mgr, &dir1.join("a.hprof")).await;
        open_ready(&mgr, &dir2.join("b.hprof")).await;

        mgr.close_for_friday_session(sid1).await;
        {
            let calls = mock.calls.lock().await;
            let closes: Vec<_> = calls.iter().filter(|(n, _)| n == "close_heap_dump").collect();
            assert_eq!(closes.len(), 1, "only sid1's dump closed");
            let closed_id = closes[0].1["id"].as_str().unwrap();
            let a_open_id = calls
                .iter()
                .find(|(n, args)| n == "open_heap_dump" && args["path"].as_str().unwrap().contains("a.hprof"))
                .map(|(_, args)| args["id"].as_str().unwrap().to_string())
                .unwrap();
            assert_eq!(closed_id, a_open_id);
        }
        // sid2 的 dump 仍可查询
        mgr.query(&dir2.join("b.hprof"), "get_leak_suspects", &serde_json::json!({}), 5)
            .await
            .unwrap();
        // sid1 的不可
        assert!(matches!(
            mgr.query(&dir1.join("a.hprof"), "get_leak_suspects", &serde_json::json!({}), 5).await,
            Err(ManagerError::NotOpen { warming: false })
        ));
    }
}
```

- [ ] **Step 2: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::manager`
Expected: 编译失败（`HeapAnalyzerManager`/`ManagerConfig`/`OpenOutcome`/`ClientFactory`/`xmx_gb_for` 未定义）

- [ ] **Step 3: 实现**：manager.rs 在 ManagerError 定义后补（`use` 区放文件顶部）：

```rust
use crate::analyzer::client::{CallOutcome, HeapAnalyzerClient};
use crate::analyzer::session::{DumpSessions, EntryPhase};
use crate::app::events::{AppEvent, EventBus};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// open 任务（预热/显式 open）的内部硬超时，对齐 heap_open 工具超时上限
const OPEN_TASK_TIMEOUT_SECS: u64 = 1800;
/// upstream close 调用的固定超时
const CLOSE_TIMEOUT_SECS: u64 = 60;

pub type ClientFactory = Arc<
    dyn Fn(
            u32,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Arc<dyn HeapAnalyzerClient>, ManagerError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ManagerConfig {
    /// 无会话且无调用持续该时长后退出工人进程
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}

pub struct OpenOutcome {
    pub summary: String,
    pub evicted: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct HeapAnalyzerManager {
    inner: Arc<tokio::sync::Mutex<ManagerInner>>,
    spawn_lock: Arc<tokio::sync::Mutex<()>>,
    client_factory: ClientFactory,
    bus: EventBus,
    artifacts_dir: PathBuf,
    config: ManagerConfig,
}

struct ManagerInner {
    client: Option<Arc<dyn HeapAnalyzerClient>>,
    sessions: DumpSessions,
    inflight: u32,
    last_active: Instant,
}

/// -Xmx 预算：dump 大小 × 1.5，向上取整 GB，clamp [4, 12]
pub fn xmx_gb_for(dump_size_bytes: u64) -> u32 {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let need = dump_size_bytes as f64 * 1.5;
    ((need / GB).ceil() as u32).clamp(4, 12)
}

/// 会话 phase 订阅者类型别名（open 等待用）
type PhaseRx = tokio::sync::watch::Receiver<EntryPhase>;

impl HeapAnalyzerManager {
    pub fn new(
        client_factory: ClientFactory,
        bus: EventBus,
        artifacts_dir: PathBuf,
        config: ManagerConfig,
    ) -> Self {
        let mgr = Self {
            inner: Arc::new(tokio::sync::Mutex::new(ManagerInner {
                client: None,
                sessions: DumpSessions::new(),
                inflight: 0,
                last_active: Instant::now(),
            })),
            spawn_lock: Arc::new(tokio::sync::Mutex::new(())),
            client_factory,
            bus,
            artifacts_dir,
            config: config.clone(),
        };
        mgr.spawn_idle_reaper();
        mgr
    }

    /// 打开 dump（MAT 建索引）。Ready 命中秒回（缓存 summary）；Warming 合流等待；
    /// Failed 重试。检查与 begin 在同一锁内完成（并发安全去重）。
    pub async fn open(
        &self,
        session_id: &str,
        path: &Path,
        timeout_secs: u64,
    ) -> Result<OpenOutcome, ManagerError> {
        tracing::info!(session_id, dump = %path.display(), timeout_secs, "heap analyzer open");

        enum Step {
            Cached(String),
            Attach(PhaseRx),
            Begin { analyzer_id: String, rx: PhaseRx, victim: Option<(PathBuf, String)> },
        }

        let step = {
            let mut inner = self.inner.lock().await;
            match inner.sessions.phase(path) {
                Some(EntryPhase::Ready { summary }) => {
                    inner.sessions.touch(path);
                    inner.last_active = Instant::now();
                    Step::Cached(summary)
                }
                Some(EntryPhase::Warming) => {
                    Step::Attach(inner.sessions.receiver(path).expect("warming entry has receiver"))
                }
                Some(EntryPhase::Failed { .. }) | None => {
                    let analyzer_id = uuid::Uuid::new_v4().to_string();
                    let (rx, victim) = inner.sessions.begin(path.to_path_buf(), analyzer_id.clone());
                    Step::Begin { analyzer_id, rx, victim }
                }
            }
        };

        let mut evicted = Vec::new();
        let mut rx = match step {
            Step::Cached(summary) => return Ok(OpenOutcome { summary, evicted }),
            Step::Attach(rx) => rx,
            Step::Begin { analyzer_id, rx, victim } => {
                if let Some((victim_path, victim_id)) = victim {
                    tracing::info!(victim = %victim_path.display(), "evicting lru dump session");
                    self.close_upstream_quietly(&victim_id).await;
                    evicted.push(victim_path);
                }
                let dump_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let mgr = self.clone();
                let p = path.to_path_buf();
                tokio::spawn(async move {
                    mgr.run_open_task(&p, analyzer_id, dump_size).await;
                });
                rx
            }
        };

        // 等待完成（先查当前值，避免与已完成的 open 竞态）
        loop {
            match rx.borrow().clone() {
                EntryPhase::Ready { summary } => {
                    let mut inner = self.inner.lock().await;
                    inner.sessions.touch(path);
                    inner.last_active = Instant::now();
                    return Ok(OpenOutcome { summary, evicted });
                }
                EntryPhase::Failed { error } => return Err(error),
                EntryPhase::Warming => {}
            }
            match tokio::time::timeout(Duration::from_secs(timeout_secs), rx.changed()).await {
                Err(_) => return Err(ManagerError::Timeout(timeout_secs)),
                Ok(Err(_)) => {
                    return Err(ManagerError::Unavailable(
                        "分析会话已失效（工人进程可能已崩溃），请重试 heap_open".into(),
                    ))
                }
                Ok(Ok(())) => {}
            }
        }
    }

    /// 查询类工具：要求 dump 已 Ready，注入 analyzer session id 后路由到上游工具。
    pub async fn query(
        &self,
        path: &Path,
        upstream_tool: &str,
        upstream_args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, ManagerError> {
        let (analyzer_id, client) = {
            let mut inner = self.inner.lock().await;
            match inner.sessions.phase(path) {
                Some(EntryPhase::Ready { .. }) => {
                    inner.sessions.touch(path);
                    inner.last_active = Instant::now();
                    let id = inner.sessions.analyzer_id(path).expect("ready entry has id");
                    (id, inner.client.clone())
                }
                Some(EntryPhase::Warming) => return Err(ManagerError::NotOpen { warming: true }),
                Some(EntryPhase::Failed { .. }) | None => {
                    return Err(ManagerError::NotOpen { warming: false })
                }
            }
        };
        let client = client.ok_or_else(|| ManagerError::Unavailable("工人进程不在运行".into()))?;

        let mut args = upstream_args.clone();
        if let Some(map) = args.as_object_mut() {
            map.insert("id".to_string(), serde_json::json!(analyzer_id));
        }

        match self.guarded_call(&client, upstream_tool, &args, timeout_secs).await {
            Err(ManagerError::Unavailable(e)) => {
                tracing::error!(error = %e, "analyzer worker unavailable during query, invalidating");
                self.invalidate().await;
                Err(ManagerError::Unavailable(e))
            }
            other => other,
        }
    }

    /// 关闭 dump 会话（幂等，上游错误仅告警）。返回是否原本处于打开（含预热中）状态。
    pub async fn close(&self, path: &Path, timeout_secs: u64) -> Result<bool, ManagerError> {
        let analyzer_id = {
            let mut inner = self.inner.lock().await;
            inner.last_active = Instant::now();
            inner.sessions.remove(path)
        };
        let Some(analyzer_id) = analyzer_id else {
            return Ok(false);
        };
        if let Some(client) = self.existing_client().await {
            let res = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
            )
            .await;
            match res {
                Err(_) => tracing::warn!(dump = %path.display(), "heap analyzer close timed out"),
                Ok(Err(e)) => tracing::warn!(dump = %path.display(), error = %e, "heap analyzer close failed"),
                Ok(Ok(o)) if o.is_error => {
                    tracing::warn!(dump = %path.display(), text = %o.text, "heap analyzer close upstream error")
                }
                _ => {}
            }
        }
        Ok(true)
    }

    /// heap dump 拉回完成后的自动预热：open（建索引，硬超时 1800s）+ provision_progress 事件。
    /// 调用方（传输钩子）负责 spawn。
    pub async fn warm_up(&self, session_id: &str, path: &Path) {
        let progress = |detail: String| AppEvent::ProvisionProgress {
            session_id: session_id.to_string(),
            tool: "jvm_heap_dump".to_string(),
            stage: "analyze".to_string(),
            detail,
        };
        self.bus.emit(session_id, progress(format!(
            "拉回完成，后台分析预热开始（MAT 建索引）：{}",
            path.display()
        )));
        match self.open(session_id, path, OPEN_TASK_TIMEOUT_SECS).await {
            Ok(_) => self.bus.emit(
                session_id,
                progress(format!("分析就绪，heap_* 工具可直接查询：{}", path.display())),
            ),
            Err(e) => self.bus.emit(
                session_id,
                progress(format!("分析预热失败（不影响对话，可手动 heap_open 重试）：{e}")),
            ),
        }
    }

    /// Friday 会话关闭联动：关闭该会话 artifacts 目录下全部 dump 会话（不主动拉起工人进程）。
    pub async fn close_for_friday_session(&self, session_id: &str) {
        let dir = crate::tools::builtin::run_command::artifact_dir_for(&self.artifacts_dir, session_id);
        let removed = {
            let mut inner = self.inner.lock().await;
            inner.last_active = Instant::now();
            inner.sessions.remove_under_dir(&dir)
        };
        if removed.is_empty() {
            return;
        }
        tracing::info!(session_id, count = removed.len(), "closing dump sessions for friday session");
        if let Some(client) = self.existing_client().await {
            for (_path, analyzer_id) in removed {
                let _ = tokio::time::timeout(
                    Duration::from_secs(CLOSE_TIMEOUT_SECS),
                    client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
                )
                .await;
            }
        }
    }

    // ── 内部 ──

    /// open 的后台任务：ensure client → 上游 open → 落定 phase。
    /// 注：若任务 panic，warming 条目会滞留（等待者超时返回）；rmcp 调用路径无 panic 预期。
    async fn run_open_task(&self, path: &Path, analyzer_id: String, dump_size: u64) {
        let xmx_gb = xmx_gb_for(dump_size);
        let client = match self.ensure_client(xmx_gb).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(dump = %path.display(), error = %e, "heap analyzer open: ensure client failed");
                self.finish_phase(path, EntryPhase::Failed { error: e }).await;
                return;
            }
        };
        let args = serde_json::json!({ "path": path.to_string_lossy(), "id": analyzer_id });
        let result = tokio::time::timeout(
            Duration::from_secs(OPEN_TASK_TIMEOUT_SECS),
            client.call_tool("open_heap_dump", &args),
        )
        .await;
        let phase = match result {
            Err(_) => EntryPhase::Failed {
                error: ManagerError::Timeout(OPEN_TASK_TIMEOUT_SECS),
            },
            Ok(Err(e)) => {
                // 传输层错误 = 工人进程疑似死亡：先失效全部，再落定 Failed
                tracing::error!(dump = %path.display(), error = %e, "heap analyzer open: transport error");
                self.invalidate().await;
                EntryPhase::Failed {
                    error: ManagerError::Unavailable(e),
                }
            }
            Ok(Ok(outcome)) if outcome.is_error => EntryPhase::Failed {
                error: ManagerError::Upstream(outcome.text),
            },
            Ok(Ok(outcome)) => EntryPhase::Ready { summary: outcome.text },
        };
        self.finish_phase(path, phase).await;
    }

    /// 带超时 + inflight 计数的上游调用
    async fn guarded_call(
        &self,
        client: &Arc<dyn HeapAnalyzerClient>,
        tool: &str,
        args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, ManagerError> {
        {
            let mut inner = self.inner.lock().await;
            inner.inflight += 1;
        }
        let result = tokio::time::timeout(Duration::from_secs(timeout_secs), client.call_tool(tool, args)).await;
        {
            let mut inner = self.inner.lock().await;
            inner.inflight -= 1;
            inner.last_active = Instant::now();
        }
        match result {
            Err(_) => Err(ManagerError::Timeout(timeout_secs)),
            Ok(Err(e)) => Err(ManagerError::Unavailable(e)),
            Ok(Ok(outcome)) if outcome.is_error => Err(ManagerError::Upstream(outcome.text)),
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }

    async fn ensure_client(&self, xmx_gb: u32) -> Result<Arc<dyn HeapAnalyzerClient>, ManagerError> {
        {
            let inner = self.inner.lock().await;
            if let Some(c) = &inner.client {
                return Ok(c.clone());
            }
        }
        let _g = self.spawn_lock.lock().await;
        {
            let inner = self.inner.lock().await;
            if let Some(c) = &inner.client {
                return Ok(c.clone());
            }
        }
        let client = (self.client_factory)(xmx_gb).await?;
        tracing::info!(xmx_gb, "heap analyzer worker process started");
        let mut inner = self.inner.lock().await;
        inner.client = Some(client.clone());
        inner.last_active = Instant::now();
        Ok(client)
    }

    async fn existing_client(&self) -> Option<Arc<dyn HeapAnalyzerClient>> {
        self.inner.lock().await.client.clone()
    }

    /// 工人进程失效：摘除客户端 + 清空全部会话（等待者经 watch sender drop 感知错误）+ 尽力 shutdown
    async fn invalidate(&self) {
        let client = {
            let mut inner = self.inner.lock().await;
            let client = inner.client.take();
            inner.sessions = DumpSessions::new();
            inner.last_active = Instant::now();
            client
        };
        if let Some(c) = client {
            c.shutdown().await;
        }
    }

    async fn finish_phase(&self, path: &Path, phase: EntryPhase) {
        let mut inner = self.inner.lock().await;
        inner.sessions.set_phase(path, phase);
        inner.last_active = Instant::now();
    }

    async fn close_upstream_quietly(&self, analyzer_id: &str) {
        if let Some(client) = self.existing_client().await {
            let _ = tokio::time::timeout(
                Duration::from_secs(CLOSE_TIMEOUT_SECS),
                client.call_tool("close_heap_dump", &serde_json::json!({ "id": analyzer_id })),
            )
            .await;
        }
    }

    fn spawn_idle_reaper(&self) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(mgr.config.idle_tick);
            loop {
                ticker.tick().await;
                let client = {
                    let inner = mgr.inner.lock().await;
                    let should = inner.client.is_some()
                        && inner.sessions.is_empty()
                        && inner.inflight == 0
                        && inner.last_active.elapsed() >= mgr.config.idle_timeout;
                    if should { inner.client.take() } else { None }
                };
                if let Some(client) = client {
                    tracing::info!("heap analyzer worker idle (no sessions, no calls), shutting down");
                    client.shutdown().await;
                }
            }
        });
    }
}
```

> 实现说明（给执行者）：
> 1. `PhaseRx` 类型别名放在文件顶层（xmx_gb_for 之后）；`enum Step` 在 `open()` 函数体内（合法，函数体可定义 item）。
> 2. `test_query_requires_ready_and_reports_warming` 需要 `HeapAnalyzerManager: Clone`（已 derive）。

- [ ] **Step 4: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::manager`
Expected: 13 个测试全部 PASS（`test_timeout_does_not_kill_worker` 约 1s）

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 干净

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/analyzer/manager.rs
git commit -m "feat: HeapAnalyzerManager lifecycle (open dedup, LRU, idle exit, crash recovery)"
```

---

### Task 6: heap_* 工具层（9 个工具 + 参数映射）

**Files:**
- Create: `src-tauri/src/tools/builtin/heap/mod.rs`
- Create: `src-tauri/src/tools/builtin/heap/mapping.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs:1-5`（加 `pub mod heap;`）

- [ ] **Step 1: 写失败测试（mapping 纯函数）**：创建 `src-tauri/src/tools/builtin/heap/mapping.rs`：

```rust
use serde_json::{json, Value};

use super::HeapToolKind;

/// Friday heap 工具参数 → 上游 jvm-heap-dump-mcp 工具名 + 参数。Err(String) → invalid_params。
pub fn build(kind: HeapToolKind, args: &Value) -> Result<(String, Value), String> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_histogram_defaults_and_overrides() {
        let (name, args) = build(HeapToolKind::Histogram, &json!({})).unwrap();
        assert_eq!(name, "get_class_histogram");
        assert_eq!(args["limit"], 30);
        assert_eq!(args["sortBy"], "RETAINED_HEAP");

        let (_, args) = build(
            HeapToolKind::Histogram,
            &json!({"top": 5, "sort_by": "shallow_heap", "filter": "com\\.example\\."}),
        )
        .unwrap();
        assert_eq!(args["limit"], 5);
        assert_eq!(args["sortBy"], "SHALLOW_HEAP");
        assert_eq!(args["filter"], "com\\.example\\.");
    }

    #[test]
    fn test_histogram_rejects_bad_sort_and_limit() {
        assert!(build(HeapToolKind::Histogram, &json!({"sort_by": "bogus"})).is_err());
        assert!(build(HeapToolKind::Histogram, &json!({"top": 0})).is_err());
        assert!(build(HeapToolKind::Histogram, &json!({"top": 999})).is_err());
    }

    #[test]
    fn test_dominator_tree_root_vs_children() {
        let (name, args) = build(HeapToolKind::DominatorTree, &json!({})).unwrap();
        assert_eq!(name, "get_dominator_tree");
        assert_eq!(args["limit"], 30);

        let (name, args) =
            build(HeapToolKind::DominatorTree, &json!({"parent_object_id": 42, "top": 10})).unwrap();
        assert_eq!(name, "get_dominator_tree_children");
        assert_eq!(args["objectId"], 42);
        assert_eq!(args["limit"], 10);

        assert!(build(HeapToolKind::DominatorTree, &json!({"parent_object_id": -1})).is_err());
    }

    #[test]
    fn test_object_id_required_positive() {
        assert!(build(HeapToolKind::ObjectInfo, &json!({})).is_err());
        assert!(build(HeapToolKind::ObjectInfo, &json!({"object_id": -1})).is_err());
        let (_, args) = build(HeapToolKind::ObjectInfo, &json!({"object_id": 7})).unwrap();
        assert_eq!(args["objectId"], 7);
    }

    #[test]
    fn test_references_direction() {
        let (name, args) =
            build(HeapToolKind::References, &json!({"object_id": 9, "direction": "inbound"})).unwrap();
        assert_eq!(name, "get_inbound_references");
        assert_eq!(args["objectId"], 9);
        assert_eq!(args["limit"], 50);

        let (name, _) =
            build(HeapToolKind::References, &json!({"object_id": 9, "direction": "outbound"})).unwrap();
        assert_eq!(name, "get_outbound_references");

        assert!(build(HeapToolKind::References, &json!({"object_id": 9})).is_err());
        assert!(build(HeapToolKind::References, &json!({"object_id": 9, "direction": "sideways"})).is_err());
    }

    #[test]
    fn test_threads_filter_passthrough() {
        let (name, args) = build(HeapToolKind::Threads, &json!({"filter": "http-nio"})).unwrap();
        assert_eq!(name, "get_threads");
        assert_eq!(args["filter"], "http-nio");
    }

    #[test]
    fn test_leak_suspects_no_extra_args() {
        let (name, args) = build(HeapToolKind::LeakSuspects, &json!({})).unwrap();
        assert_eq!(name, "get_leak_suspects");
        assert!(args.as_object().unwrap().is_empty());
    }
}
```

创建 `src-tauri/src/tools/builtin/heap/mod.rs`（先只含类型 + handler 测试）：

```rust
pub mod mapping;

use crate::analyzer::manager::{HeapAnalyzerManager, ManagerError};
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tools::builtin::jvm::core::{clamp_or, error_output};

/// (default_secs, max_secs)
type Timeouts = (u64, u64);
const OPEN: Timeouts = (600, 1800);
const CLOSE: Timeouts = (30, 60);
const QUERY: Timeouts = (60, 300);

#[derive(Debug, Clone, Copy)]
pub enum HeapToolKind {
    Open,
    Close,
    LeakSuspects,
    Histogram,
    DominatorTree,
    ObjectInfo,
    PathToGcRoots,
    References,
    Threads,
}

pub struct HeapToolHandler {
    pub manager: Arc<HeapAnalyzerManager>,
    pub artifacts_dir: PathBuf,
    pub kind: HeapToolKind,
    pub timeouts: Timeouts,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::client::{HeapAnalyzerClient, MockHeapAnalyzerClient};
    use crate::analyzer::manager::{ClientFactory, ManagerConfig};
    use crate::app::events::EventBus;
    use crate::tools::registry::ToolRegistry;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SID: &str = "123e4567-e89b-12d3-a456-426614174000";

    async fn setup(
        mock: Arc<MockHeapAnalyzerClient>,
    ) -> (tempfile::TempDir, Arc<HeapAnalyzerManager>, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));
        let s2 = spawns.clone();
        let m2 = mock.clone();
        let factory: ClientFactory = Arc::new(move |_xmx| {
            let m2 = m2.clone();
            let s2 = s2.clone();
            Box::pin(async move {
                s2.fetch_add(1, Ordering::SeqCst);
                let c: Arc<dyn HeapAnalyzerClient> = m2;
                Ok(c)
            })
        });
        let mgr = Arc::new(HeapAnalyzerManager::new(
            factory,
            EventBus::disabled(),
            artifacts.clone(),
            ManagerConfig::default(),
        ));
        (tmp, mgr, artifacts)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: SID.into(), channel: None }
    }

    fn dump(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "fake").unwrap();
        p
    }

    fn def<'a>(reg: &'a ToolRegistry, name: &str) -> &'a ToolDef {
        reg.get(name).unwrap()
    }

    async fn registry(mock: Arc<MockHeapAnalyzerClient>) -> (tempfile::TempDir, ToolRegistry) {
        let (tmp, mgr, artifacts) = setup(mock).await;
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, mgr, artifacts);
        (tmp, reg)
    }

    #[tokio::test]
    async fn test_register_all_nine_tools_all_readonly() {
        let (tmp, reg) = registry(Arc::new(MockHeapAnalyzerClient::ok("S"))).await;
        for name in [
            "heap_open",
            "heap_close",
            "heap_leak_suspects",
            "heap_histogram",
            "heap_dominator_tree",
            "heap_object_info",
            "heap_path_to_gc_roots",
            "heap_references",
            "heap_threads",
        ] {
            let d = def(&reg, name);
            assert_eq!(d.risk_level, RiskLevel::ReadOnly, "{name}");
            assert!(!d.needs_channel, "{name}");
        }
        drop(tmp);
    }

    #[tokio::test]
    async fn test_heap_open_happy_path() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("SUMMARY"));
        let (tmp, reg) = registry(mock.clone()).await;
        let p = dump(tmp.path(), "a.hprof");
        let out = def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "open_heap_dump");
        assert_eq!(out.data["result"], "SUMMARY");
        assert_eq!(out.data["truncated"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_heap_open_missing_file_invalid_params() {
        let (tmp, reg) = registry(Arc::new(MockHeapAnalyzerClient::ok("S"))).await;
        let out = def(&reg, "heap_open")
            .execute(serde_json::json!({"local_path": "C:/definitely/nope.hprof", "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_query_without_open_dump_not_open() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        let out = def(&reg, "heap_histogram")
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "dump_not_open");
        assert!(out.data["message"].as_str().unwrap().contains("heap_open"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_histogram_arg_mapping_end_to_end() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock.clone()).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_histogram")
            .handler
            .execute(
                serde_json::json!({"local_path": p.to_string_lossy(), "top": 5, "session_id": SID}),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "get_class_histogram");
        let calls = mock.calls.lock().await;
        let (name, args) = calls.last().unwrap();
        assert_eq!(name, "get_class_histogram");
        assert_eq!(args["limit"], 5);
        assert_eq!(args["sortBy"], "RETAINED_HEAP");
        assert!(!args["id"].as_str().unwrap().is_empty());
        drop(tmp);
    }

    #[tokio::test]
    async fn test_references_invalid_direction_invalid_params() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_references")
            .handler
            .execute(
                serde_json::json!({"local_path": p.to_string_lossy(), "object_id": 1, "direction": "sideways", "session_id": SID}),
                &ctx(),
            )
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_object_info_missing_object_id_invalid_params() {
        let (tmp, reg) = registry(Arc::new(MockHeapAnalyzerClient::ok("S"))).await;
        let p = dump(tmp.path(), "a.hprof");
        let out = def(&reg, "heap_object_info")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_large_output_truncated_and_persisted() {
        let big = "x".repeat(70 * 1024);
        let mock = Arc::new(MockHeapAnalyzerClient::ok(&big));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_leak_suspects")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["truncated"], true);
        let full = out.data["full_output_path"].as_str().unwrap();
        assert!(std::fs::metadata(full).map(|m| m.len() as usize > 70 * 1024).unwrap_or(false));
        assert!(out.data["result"].as_str().unwrap().contains("[truncated"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_heap_close_after_open() {
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (tmp, reg) = registry(mock.clone()).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_close")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["was_open"], true);
        let calls = mock.calls.lock().await;
        assert!(calls.iter().any(|(n, _)| n == "close_heap_dump"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upstream_tool_error_passthrough() {
        let mock = Arc::new(MockHeapAnalyzerClient::with_fn(|name, _args| {
            let name = name.to_string();
            async move {
                if name == "open_heap_dump" {
                    Ok(crate::analyzer::client::CallOutcome { text: "S".into(), is_error: false })
                } else {
                    Ok(crate::analyzer::client::CallOutcome { text: "MAT boom".into(), is_error: true })
                }
            }
        }));
        let (tmp, reg) = registry(mock).await;
        let p = dump(tmp.path(), "a.hprof");
        def(&reg, "heap_open")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        let out = def(&reg, "heap_leak_suspects")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy(), "session_id": SID}), &ctx())
            .await;
        assert!(!out.success);
        // 业务错误透传：无 error code，result 携带上游文本
        assert_eq!(out.data["error"], serde_json::Value::Null);
        assert_eq!(out.data["upstream_is_error"], true);
        assert!(out.data["result"].as_str().unwrap().contains("MAT boom"));
        drop(tmp);
    }
}
```

`src-tauri/src/tools/builtin/mod.rs` 模块声明区加：

```rust
pub mod heap;
```

- [ ] **Step 2: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::heap`
Expected: 编译失败（`register_all` 未定义；mapping `build` 是 `unimplemented!()`——注意先让编译通过后跑测试确认 panic 失败）

- [ ] **Step 3: 实现 mapping**：mapping.rs 替换 `build` 存根：

```rust
pub fn build(kind: HeapToolKind, args: &Value) -> Result<(String, Value), String> {
    match kind {
        HeapToolKind::Open | HeapToolKind::Close => Err("内部错误：open/close 不经 mapping".into()),
        HeapToolKind::LeakSuspects => Ok(("get_leak_suspects".into(), json!({}))),
        HeapToolKind::Histogram => {
            let limit = limit_arg(args, "top", 30)?;
            let sort_by = match args.get("sort_by").and_then(|v| v.as_str()) {
                None | Some("retained_heap") => "RETAINED_HEAP",
                Some("shallow_heap") => "SHALLOW_HEAP",
                Some("objects") => "OBJECTS",
                Some(other) => {
                    return Err(format!("sort_by 非法: {other}（可选 retained_heap / shallow_heap / objects）"))
                }
            };
            let mut a = json!({ "limit": limit, "sortBy": sort_by });
            if let Some(f) = args.get("filter").and_then(|v| v.as_str()) {
                a["filter"] = json!(f);
            }
            Ok(("get_class_histogram".into(), a))
        }
        HeapToolKind::DominatorTree => {
            let limit = limit_arg(args, "top", 30)?;
            match optional_object_id(args, "parent_object_id")? {
                None => Ok(("get_dominator_tree".into(), json!({ "limit": limit }))),
                Some(oid) => Ok(("get_dominator_tree_children".into(), json!({ "objectId": oid, "limit": limit }))),
            }
        }
        HeapToolKind::ObjectInfo => Ok(("get_object_info".into(), json!({ "objectId": object_id(args)? }))),
        HeapToolKind::PathToGcRoots => {
            Ok(("get_path_to_gc_roots".into(), json!({ "objectId": object_id(args)? })))
        }
        HeapToolKind::References => {
            let direction = args
                .get("direction")
                .and_then(|v| v.as_str())
                .ok_or("missing required parameter: direction（outbound / inbound）")?;
            let upstream = match direction {
                "outbound" => "get_outbound_references",
                "inbound" => "get_inbound_references",
                other => return Err(format!("direction 非法: {other}（可选 outbound / inbound）")),
            };
            Ok((
                upstream.into(),
                json!({ "objectId": object_id(args)?, "limit": limit_arg(args, "top", 50)? }),
            ))
        }
        HeapToolKind::Threads => {
            let mut a = json!({});
            if let Some(f) = args.get("filter").and_then(|v| v.as_str()) {
                a["filter"] = json!(f);
            }
            Ok(("get_threads".into(), a))
        }
    }
}

fn object_id(args: &Value) -> Result<i64, String> {
    let n = args
        .get("object_id")
        .and_then(|v| v.as_i64())
        .ok_or("missing required parameter: object_id（正整数，来自 heap_dominator_tree / heap_histogram / heap_references 结果）")?;
    if n <= 0 {
        return Err("object_id 必须是正整数".into());
    }
    Ok(n)
}

fn optional_object_id(args: &Value, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(v) => {
            let n = v.as_i64().ok_or_else(|| format!("{key} 必须是正整数"))?;
            if n <= 0 {
                return Err(format!("{key} 必须是正整数"));
            }
            Ok(Some(n))
        }
    }
}

fn limit_arg(args: &Value, key: &str, default: i64) -> Result<i64, String> {
    match args.get(key).and_then(|v| v.as_i64()) {
        None => Ok(default),
        Some(n) if (1..=200).contains(&n) => Ok(n),
        Some(n) => Err(format!("{key} 必须在 1..=200 之间，收到 {n}")),
    }
}
```

- [ ] **Step 4: 实现 handler + 工具定义**：mod.rs 在 handler struct 后、tests 前补：

```rust
#[async_trait]
impl ToolHandler for HeapToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(local_path) = args.get("local_path").and_then(|v| v.as_str()) else {
            return error_output("invalid_params", "missing required parameter: local_path");
        };
        let path = match resolve_local_path(local_path) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_params", &e),
        };
        let timeout_secs =
            clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts.0, self.timeouts.1);
        let start = std::time::Instant::now();
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, dump = %path.display(), "heap tool executing");

        match self.kind {
            HeapToolKind::Open => match self.manager.open(&ctx.session_id, &path, timeout_secs).await {
                Ok(outcome) => {
                    let mut out = render(&ctx.session_id, &self.artifacts_dir, "open_heap_dump", local_path, &outcome.summary, start).await;
                    if !outcome.evicted.is_empty() {
                        out.data["evicted"] = serde_json::json!(
                            outcome.evicted.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
                        );
                    }
                    out
                }
                Err(e) => manager_error_output(e),
            },
            HeapToolKind::Close => match self.manager.close(&path, timeout_secs).await {
                Ok(was_open) => ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "tool": "close_heap_dump",
                        "local_path": local_path,
                        "was_open": was_open,
                    }),
                    raw_stdout: None,
                },
                Err(e) => manager_error_output(e),
            },
            kind => {
                let (upstream_name, upstream_args) = match mapping::build(kind, &args) {
                    Ok(v) => v,
                    Err(e) => return error_output("invalid_params", &e),
                };
                match self.manager.query(&path, &upstream_name, &upstream_args, timeout_secs).await {
                    Ok(outcome) => {
                        render(&ctx.session_id, &self.artifacts_dir, &upstream_name, local_path, &outcome.text, start).await
                    }
                    Err(e) => manager_error_output(e),
                }
            }
        }
    }
}

/// 结果组装：64KB 头部截断 + 完整结果落盘 session artifacts（复用 run_command 机制）
async fn render(
    session_id: &str,
    artifacts_dir: &Path,
    upstream_tool: &str,
    local_path: &str,
    text: &str,
    start: std::time::Instant,
) -> ToolOutput {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (body, truncated) = truncate_output(text);
    let session_dir = artifact_dir_for(artifacts_dir, session_id);
    let artifact_path = session_dir.join(format!("heap-{}.md", uuid::Uuid::new_v4()));
    let mut full_output_path = None;
    if tokio::fs::create_dir_all(&session_dir).await.is_ok() {
        if tokio::fs::write(&artifact_path, text).await.is_ok() {
            full_output_path = Some(artifact_path);
        } else {
            tracing::warn!(session_id, tool = upstream_tool, "failed to persist full heap tool output");
        }
    }
    let result_field = if truncated {
        match &full_output_path {
            Some(p) => format!("{body}\n[truncated, full output: {}]", p.display()),
            None => format!("{body}\n[truncated]"),
        }
    } else {
        body
    };
    tracing::info!(session_id, tool = upstream_tool, elapsed_ms, truncated, "heap tool executed");
    ToolOutput {
        success: true,
        data: serde_json::json!({
            "tool": upstream_tool,
            "local_path": local_path,
            "result": result_field,
            "elapsed_ms": elapsed_ms,
            "truncated": truncated,
            "full_output_path": full_output_path.as_ref().map(|p| p.display().to_string()),
        }),
        raw_stdout: Some(text.to_string()),
    }
}

/// ManagerError → 结构化错误输出。Upstream（MAT 业务错误）走透传（无 error code，对齐 jvm_* 惯例）。
fn manager_error_output(e: ManagerError) -> ToolOutput {
    match e {
        ManagerError::JavaMissing(m) => {
            error_output("java_missing", &format!("本机 Java 21+ 不可用：{m}。请安装 JDK 21+ 后重试。"))
        }
        ManagerError::Unavailable(m) => error_output(
            "analyzer_unavailable",
            &format!("{m}。可重试一次；连续失败请查看 Friday 日志。"),
        ),
        ManagerError::Timeout(t) => error_output(
            "analyzer_timeout",
            &format!("分析调用超时（{t}s）。工人进程未受影响，可重试。"),
        ),
        ManagerError::NotOpen { warming } => {
            if warming {
                error_output("dump_not_open", "该 dump 正在预热（MAT 建索引，GB 级需分钟级）。请稍候后重试 heap_open。")
            } else {
                error_output("dump_not_open", "该 dump 尚未打开。请先调用 heap_open(local_path)。")
            }
        }
        ManagerError::Upstream(text) => ToolOutput {
            success: false,
            data: serde_json::json!({ "upstream_is_error": true, "result": text }),
            raw_stdout: Some(text),
        },
    }
}

/// local_path 解析：相对路径以 cwd 补全 + 必须是已存在文件
fn resolve_local_path(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("local_path 不能为空".into());
    }
    let mut p = PathBuf::from(raw);
    if p.is_relative() {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        p = cwd.join(p);
    }
    if !p.is_file() {
        return Err(format!("文件不存在: {}", p.display()));
    }
    Ok(p)
}

fn heap_tool_def(
    name: &str,
    description: &str,
    schema: serde_json::Value,
    kind: HeapToolKind,
    timeouts: Timeouts,
    manager: &Arc<HeapAnalyzerManager>,
    artifacts_dir: &Path,
) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        risk_level: RiskLevel::ReadOnly,
        needs_channel: false,
        handler: Arc::new(HeapToolHandler {
            manager: manager.clone(),
            artifacts_dir: artifacts_dir.to_path_buf(),
            kind,
            timeouts,
        }),
    }
}

/// 注册全部 heap_* 工具（lib.rs 调用）
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    manager: Arc<HeapAnalyzerManager>,
    artifacts_dir: PathBuf,
) {
    registry.register(heap_tool_def(
        "heap_open",
        "打开本机堆转储（.hprof）建立 MAT 分析会话并返回 heap 总览（大小/对象数/类数/GC root 数）。GB 级 dump 建索引需分钟级；jvm_heap_dump 拉回后自动预热，命中时本调用秒回。local_path 用 jvm_heap_dump / transfer_status 返回的本机路径。分析完成后建议 heap_close 释放内存。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径（transfer completed 返回的 local_path）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 600，上限 1800（大 dump 建索引慢）" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Open,
        OPEN,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_close",
        "关闭堆转储分析会话并释放工人进程内存（MAT 索引文件保留，重开秒级）。会话结束或长期不用时调用；未打开时调用安全（幂等）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 30，上限 60" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Close,
        CLOSE,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_leak_suspects",
        "MAT 自动泄漏嫌疑报告（嫌疑点描述 + retained heap + 概率）。OOM 根因分析首选第一步。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::LeakSuspects,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_histogram",
        "类直方图：按类聚合的实例数 / shallow / retained heap，支持类名正则过滤与排序。定位哪类对象吃掉了内存。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "top": { "type": "number", "description": "返回条数，默认 30，上限 200" },
                "sort_by": { "type": "string", "enum": ["retained_heap", "shallow_heap", "objects"], "description": "排序键，默认 retained_heap" },
                "filter": { "type": "string", "description": "类名正则过滤（如 com\\\\.example\\\\.）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Histogram,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_dominator_tree",
        "支配树 Top N（retained heap 最大的对象）。传 parent_object_id 进入子树下钻。定位内存根因的主要工具。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "parent_object_id": { "type": "integer", "description": "下钻父节点 objectId（来自支配树/直方图结果）；不传则返回根级 Top" },
                "top": { "type": "number", "description": "返回条数，默认 30，上限 200" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::DominatorTree,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_object_info",
        "对象详情：类 / shallow / retained / GC root 类型 / 全部字段值。object_id 来自 heap_dominator_tree / heap_histogram / heap_references 结果。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "object_id": { "type": "integer", "description": "目标对象 objectId（正整数）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path", "object_id"]
        }),
        HeapToolKind::ObjectInfo,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_path_to_gc_roots",
        "对象到 GC root 的最短引用链——确认泄漏、找出持有者的关键工具。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "object_id": { "type": "integer", "description": "目标对象 objectId（正整数）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path", "object_id"]
        }),
        HeapToolKind::PathToGcRoots,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_references",
        "对象的引用关系：direction=outbound 看它引用谁，inbound 看谁引用它（引用图下钻）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "object_id": { "type": "integer", "description": "目标对象 objectId（正整数）" },
                "direction": { "type": "string", "enum": ["outbound", "inbound"], "description": "引用方向" },
                "top": { "type": "number", "description": "返回条数，默认 50，上限 200" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path", "object_id", "direction"]
        }),
        HeapToolKind::References,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
    registry.register(heap_tool_def(
        "heap_threads",
        "堆转储中的线程列表：retained heap + 栈帧。定位哪个线程持有大量内存（如 ThreadLocal 泄漏）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "堆转储文件的本机绝对路径" },
                "filter": { "type": "string", "description": "线程名正则过滤（如 http-nio）" },
                "timeout_secs": { "type": "number", "description": "超时秒数，默认 60，上限 300" }
            },
            "required": ["local_path"]
        }),
        HeapToolKind::Threads,
        QUERY,
        &manager,
        &artifacts_dir,
    ));
}
```

- [ ] **Step 5: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::heap`
Expected: mapping 7 个 + mod 10 个测试 PASS

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 干净

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tools/builtin/heap/ src-tauri/src/tools/builtin/mod.rs
git commit -m "feat: heap_* structured tools (9 tools over MAT worker)"
```

---

### Task 7: 传输完成预热钩子（TransferManager + analyzer 联动）

**Files:**
- Modify: `src-tauri/src/transfer/mod.rs`
- Modify: `src-tauri/src/analyzer/mod.rs`（钩子构造函数放 manager.rs，mod.rs re-export）
- Modify: `src-tauri/src/analyzer/manager.rs`

- [ ] **Step 1: 写失败测试（transfer 侧）**：transfer/mod.rs 的 tests 模块末尾追加：

```rust
    #[tokio::test]
    async fn test_download_complete_hook_invoked_for_completed_downloads_only() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mut mgr = TransferManager::new(db, EventBus::disabled());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(Direction, Status)>::new()));
        let s2 = seen.clone();
        mgr.set_download_complete_hook(Arc::new(move |state: &TransferState| {
            s2.lock().unwrap().push((state.direction, state.status));
        }));
        let mgr = Arc::new(mgr);

        // completed download → 触发
        let id = mgr.start(make_state(Direction::Download, "/tmp/a.hprof", "s1")).await;
        mgr.finish(&id, Status::Completed, None, 10, 10).await;
        // failed download → 不触发
        let id2 = mgr.start(make_state(Direction::Download, "/tmp/b.hprof", "s1")).await;
        mgr.finish(&id2, Status::Failed, Some("x".into()), 5, 10).await;
        // upload completed → 不触发
        let id3 = mgr.start(make_state(Direction::Upload, "/tmp/c.hprof", "s1")).await;
        mgr.finish(&id3, Status::Completed, None, 10, 10).await;

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen, vec![(Direction::Download, Status::Completed)]);
    }
```

- [ ] **Step 2: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer::tests::test_download_complete_hook`
Expected: 编译失败 `no method named set_download_complete_hook`

- [ ] **Step 3: 实现（transfer 侧）**：

`transfer/mod.rs`：`ChannelFactory` 类型定义后加：

```rust
/// 下载完成回调注入点（heap dump 拉回 → 分析预热）。必须在 Arc 包装前设置。
pub type DownloadCompleteHook = Arc<dyn Fn(&TransferState) + Send + Sync>;
```

`TransferManager` struct 加字段：

```rust
    download_complete_hook: Option<DownloadCompleteHook>,
```

`TransferManager::new` 初始化 `download_complete_hook: None`，并加 setter（对齐 `set_channel_factory` 模式）：

```rust
    /// 注入下载完成回调（测试用 / heap 分析预热）。必须在 Arc 包装前调用。
    pub fn set_download_complete_hook(&mut self, hook: DownloadCompleteHook) {
        self.download_complete_hook = Some(hook);
    }
```

`finish()` 末尾（`self.evict_finished().await;` 之后）加：

```rust
        // 下载完成回调（heap dump 拉回 → 分析预热）。失败不影响传输终态。
        if event.direction == Direction::Download && event.status == Status::Completed {
            if let Some(hook) = &self.download_complete_hook {
                hook(&event);
            }
        }
```

- [ ] **Step 4: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer`
Expected: 既有测试 + 新测试全部 PASS

- [ ] **Step 5: 写失败测试（analyzer 钩子侧）**：manager.rs tests 末尾追加：

```rust
    #[tokio::test]
    async fn test_download_complete_hook_warms_hprof_only() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockHeapAnalyzerClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, tmp.path(), ManagerConfig::default());
        let mgr = Arc::new(mgr);
        let hook = download_complete_hook(&mgr);

        let a = dump_file(tmp.path(), "a.hprof");
        let mut st = crate::transfer::state::TransferState::new(
            crate::transfer::state::Direction::Download,
            SID,
            "env-1",
            "/tmp/remote/a.hprof",
            a.clone(),
            false,
        );
        hook(&st);
        // 非 hprof 不触发
        let log = dump_file(tmp.path(), "b.log");
        st.local_path = log;
        st.id = "t2".into();
        hook(&st);

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let calls = mock.calls.lock().await;
        let opens: Vec<_> = calls.iter().filter(|(n, _)| n == "open_heap_dump").collect();
        assert_eq!(opens.len(), 1, "only the hprof must be warmed");
        assert!(opens[0].1["path"].as_str().unwrap().ends_with("a.hprof"));
    }
```

- [ ] **Step 6: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer::manager::tests::test_download_complete_hook`
Expected: 编译失败 `cannot find function download_complete_hook`

- [ ] **Step 7: 实现（analyzer 侧）**：manager.rs 末尾（impl 块外）加：

```rust
/// 传输完成钩子：下载的 .hprof 完成后触发自动预热（lib.rs 注入 TransferManager）。
/// 其余扩展名直接忽略；预热失败只记事件，不影响传输终态。
pub fn download_complete_hook(manager: &Arc<HeapAnalyzerManager>) -> crate::transfer::DownloadCompleteHook {
    let mgr = manager.clone();
    Arc::new(move |state: &crate::transfer::state::TransferState| {
        let is_hprof = state
            .local_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("hprof"))
            .unwrap_or(false);
        if !is_hprof {
            return;
        }
        let mgr = mgr.clone();
        let session_id = state.session_id.clone();
        let path = state.local_path.clone();
        tokio::spawn(async move {
            mgr.warm_up(&session_id, &path).await;
        });
    })
}
```

`src-tauri/src/analyzer/mod.rs` 更新为：

```rust
pub mod client;
pub mod java;
pub mod manager;
pub mod session;

pub use manager::{download_complete_hook, HeapAnalyzerManager, ManagerConfig, ManagerError};
```

- [ ] **Step 8: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml analyzer`
Expected: 全部 PASS

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/transfer/mod.rs src-tauri/src/analyzer/
git commit -m "feat: heap dump download completion hook for analysis warm-up"
```

---

### Task 8: 应用装配（lib.rs + AppState + 会话关闭联动）（接线任务，TDD 例外——纯胶水，靠编译 + 全量测试守护）

**Files:**
- Modify: `src-tauri/src/analyzer/manager.rs`（production_client_factory + ANALYZER_JAR_NAME）
- Modify: `src-tauri/src/analyzer/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/app/lifecycle.rs:256-283`（close_session_cmd）

- [ ] **Step 1: 生产工厂**：manager.rs 末尾追加：

```rust
/// vendored 分析器 JAR 文件名（scripts/fetch-analyzer-jar.ps1 下载）
pub const ANALYZER_JAR_NAME: &str = "jvm-heap-dump-mcp-0.2.0-all.jar";

/// 生产 client 工厂：Java 探测（Ok 结果进程内缓存）→ stdio 子进程 MCP client。
/// jar 缺失（未跑 fetch 脚本）→ Unavailable 引导。
pub fn production_client_factory(jar_path: Option<PathBuf>) -> ClientFactory {
    Arc::new(move |xmx_gb: u32| {
        let jar = jar_path.clone();
        Box::pin(async move {
            static JAVA_CACHE: std::sync::OnceLock<crate::analyzer::java::JavaInfo> = std::sync::OnceLock::new();
            let java = match JAVA_CACHE.get() {
                Some(j) => j.clone(),
                None => match crate::analyzer::java::detect_java().await {
                    Ok(info) => {
                        let _ = JAVA_CACHE.set(info.clone());
                        info
                    }
                    Err(e) => return Err(ManagerError::JavaMissing(e)),
                },
            };
            let jar = jar.ok_or_else(|| {
                ManagerError::Unavailable(
                    "分析器 JAR 缺失（resources/analyzer/）。请运行 scripts/fetch-analyzer-jar.ps1 后重启。"
                        .to_string(),
                )
            })?;
            match crate::analyzer::client::spawn_analyzer_client(&java, &jar, xmx_gb).await {
                Ok(c) => Ok(Arc::new(c)),
                Err(e) => Err(ManagerError::Unavailable(e)),
            }
        })
    })
}
```

analyzer/mod.rs 的 re-export 行更新为：

```rust
pub use manager::{
    download_complete_hook, production_client_factory, HeapAnalyzerManager, ManagerConfig,
    ManagerError, ANALYZER_JAR_NAME,
};
```

- [ ] **Step 2: lib.rs 装配**：

`mod analyzer;` 已在 Task 2 加入。

(a) 在 `let vec_store = ...` 块（约 67-78 行）之后、`// Create shared state for MCP server` 之前插入：

```rust
            // 堆快照分析：vendored MAT 工人进程（resources/analyzer JAR + 本机 Java 21+）
            let analyzer_jar = resource_dir.as_ref().and_then(|r| {
                let candidates = [
                    r.join("resources").join("analyzer").join(crate::analyzer::ANALYZER_JAR_NAME),
                    r.join("analyzer").join(crate::analyzer::ANALYZER_JAR_NAME),
                ];
                candidates.into_iter().find(|p| p.exists())
            });
            if analyzer_jar.is_none() {
                tracing::warn!(
                    "heap analyzer JAR missing (resources/analyzer/{}); heap_* tools will report analyzer_unavailable",
                    crate::analyzer::ANALYZER_JAR_NAME
                );
            }
            let analyzer_manager = Arc::new(crate::analyzer::HeapAnalyzerManager::new(
                crate::analyzer::production_client_factory(analyzer_jar),
                EventBus::new(handle.clone()),
                paths.artifacts_dir(),
                crate::analyzer::ManagerConfig::default(),
            ));
```

(b) TransferManager 创建（84-87 行）改为：

```rust
            // 文件传输：TransferManager（后台异步传输引擎）+ 4 个工具；
            // heap dump 拉回完成 → 自动预热分析（钩子须在 Arc 包装前注入）
            let mut transfer_manager = crate::transfer::TransferManager::new(
                pool.clone(),
                EventBus::new(handle.clone()),
            );
            transfer_manager
                .set_download_complete_hook(crate::analyzer::download_complete_hook(&analyzer_manager));
            let transfer_manager = Arc::new(transfer_manager);
```

(c) `crate::tools::builtin::jvm::register_all(...)` 调用块之后加：

```rust
            crate::tools::builtin::heap::register_all(
                &mut tool_registry,
                analyzer_manager.clone(),
                paths.artifacts_dir(),
            );
```

(d) `AppState` struct 加字段：

```rust
    pub analyzer: Arc<crate::analyzer::HeapAnalyzerManager>,
```

`app.manage(AppState { ... })` 对应加：

```rust
                analyzer: analyzer_manager,
```

- [ ] **Step 3: 会话关闭联动**：`app/lifecycle.rs` 的 `close_session_cmd`，在 `session::close_session(...)` 成功之后、`SessionClosed` 事件之前加：

```rust
    // 关闭该会话 artifacts 下的堆分析会话（释放 MAT 工人进程内存；索引保留）
    state.analyzer.close_for_friday_session(&session_id).await;
```

- [ ] **Step 4: 验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 干净

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全量 PASS（无回归）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/analyzer/ src-tauri/src/lib.rs src-tauri/src/app/lifecycle.rs
git commit -m "feat: wire heap analyzer into app state and session close"
```

---

### Task 9: 系统提示词 + jvm_heap_dump 描述引导

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs:30-37`（TOOL_GUIDANCE）
- Modify: `src-tauri/src/tools/builtin/jvm/heap_dump.rs:203,175`（description + note）

- [ ] **Step 1: 写失败测试**：prompt.rs tests 末尾追加：

```rust
    #[test]
    fn test_tool_guidance_mentions_heap_tools() {
        assert!(TOOL_GUIDANCE.contains("heap_open"));
        assert!(TOOL_GUIDANCE.contains("heap_leak_suspects"));
        assert!(TOOL_GUIDANCE.contains("heap_dominator_tree"));
        assert!(TOOL_GUIDANCE.contains("heap_path_to_gc_roots"));
        assert!(TOOL_GUIDANCE.contains("heap_close"));
        assert!(TOOL_GUIDANCE.contains("不要让用户手动开 MAT"));
    }
```

heap_dump.rs tests 末尾追加：

```rust
    #[tokio::test]
    async fn test_tool_def_guides_to_heap_tools() {
        let ch = Arc::new(DumpChannel { dump_exit: 0, stat_size: "1", calls: TokioMutex::new(Vec::new()) });
        let (tmp, core, mgr) = setup(ch).await;
        let def = jvm_heap_dump_tool_def(core, crate::app::events::EventBus::disabled(), mgr);
        assert!(def.description.contains("heap_open"), "description should guide to heap_* tools");
        drop(tmp);
    }
```

- [ ] **Step 2: 验证失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt::tests::test_tool_guidance_mentions_heap_tools tools::builtin::jvm::heap_dump::tests::test_tool_def_guides_to_heap_tools`
Expected: 两个测试 FAIL（TOOL_GUIDANCE 无 heap 关键词；description 无 heap_open）

- [ ] **Step 3: 实现**：

(a) prompt.rs TOOL_GUIDANCE 中「文件传输」条目里的 `completed（下载场景把 local_path 告知用户，artifacts 目录可用 MAT 等分析）` 改为 `completed（下载场景把 local_path 告知用户；堆快照会自动预热并可直接用 heap_* 工具分析）`；并在该条目之后新增一条：

```
- 堆快照分析（本机 MAT 引擎）：jvm_heap_dump 拉回完成后自动预热建索引，用 heap_open(local_path) 获取总览（预热命中秒回）→ heap_leak_suspects（泄漏嫌疑）/ heap_dominator_tree（支配树下钻）→ heap_path_to_gc_roots（引用链定责）→ heap_object_info / heap_references / heap_threads / heap_histogram 按需下钻；object_id 取自这些工具的返回。全程自主完成根因分析，不要让用户手动开 MAT。分析结束调 heap_close 释放内存。
```

(b) heap_dump.rs `jvm_heap_dump_tool_def` 的 description 中 `completed 后 local_path 在本机会话 artifacts 目录，请告知用户用 MAT 等工具分析。` 改为 `completed 后 dump 自动预热，直接用 heap_open(local_path) 等 heap_* 工具自主分析根因（MAT 引擎，本机需 Java 21+）。`；`note` 字段中 `completed 后把 local_path 告知用户；failed 时远端文件保留，可用 file_download 重试（断点续传）。` 改为 `completed 后自动预热分析，用 heap_open(local_path) 起步做根因分析；failed 时远端文件保留，可用 file_download 重试（断点续传）。`

- [ ] **Step 4: 验证通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt tools::builtin::jvm::heap_dump`
Expected: 全部 PASS（含既有 `test_full_flow_starts_background_download` 的 note 断言——"轮询"仍在）

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全量 PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/prompt.rs src-tauri/src/tools/builtin/jvm/heap_dump.rs
git commit -m "feat: prompt guidance for heap analysis, update heap dump description"
```

---

### Task 10: 文档 + CI + 最终验证（文档任务，TDD 例外）

**Files:**
- Modify: `.github/workflows/release.yml`（42 行 model 步骤之后）
- Modify: `docs/architecture/overview.md:77-80`
- Modify: `docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md:251-258`（§9 表）
- Modify: `AGENTS.md`（已实现功能）

- [ ] **Step 1: CI 下载 JAR**：release.yml 在「Download embedding model」步骤之后加：

```yaml
      - name: Download heap analyzer JAR
        shell: pwsh
        run: ./scripts/fetch-analyzer-jar.ps1
```

- [ ] **Step 2: overview.md**：诊断工具层的

```
│   / jvm_heap_dump；arthas/读日志/读dump 后续批次）        │
```

改为：

```
│   / jvm_heap_dump；堆快照分析 heap_* 系列（MAT 引擎，   │
│   自动预热）已落地；arthas/读日志/读dump 后续批次）        │
```

- [ ] **Step 3: umbrella 设计 §9 表**：`| 结构化 JVM 工具批次 |` 行之后追加一行：

```
| 堆快照分析（MAT） | ✅ 已落地（见 [堆快照分析设计](2026-08-29-heap-analysis-design.md)）：heap_* 工具 + MAT 工人进程（vendored JAR，rmcp client stdio 托管），dump 拉回自动预热 |
```

- [ ] **Step 4: AGENTS.md**：「已实现功能」列表末尾追加：

```
- **堆快照分析**：heap_* 系列 9 个 MCP 工具（MAT 内核）。Friday 作为 MCP client 托管 vendored jvm-heap-dump-mcp JAR 工人进程（stdio，需本机 Java 21+，vendoring 走 `scripts/fetch-analyzer-jar.ps1` 构建时获取）；dump 拉回完成自动预热（MAT 建索引，provision_progress 事件）；LRU 会话管理（上限 3）、空闲 15min 自动退出、崩溃自动重启、会话关闭联动释放
```

- [ ] **Step 5: 最终验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 干净

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全量 PASS

Run: `pnpm typecheck`
Expected: 通过（前端零改动，确认无意外破坏）

- [ ] **Step 6: 手动冒烟（可选，需本机 Java 21+ 与已下载 JAR）**

Run: `pnpm tauri dev`，在会话中让 agent 对某个已拉回的 dump 调 `heap_open` → `heap_leak_suspects`，确认：MAT 工人进程启动（日志 `heap analyzer worker spawning`）、预热事件出现在聊天流、分析结果渲染。

- [ ] **Step 7: Commit**

```bash
git add .github/workflows/release.yml docs/architecture/overview.md docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md AGENTS.md
git commit -m "docs: heap analysis landed (overview, umbrella, AGENTS, release ci)"
```

---

## 完成标准

- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` 全量 PASS
- [ ] `cargo check` / `pnpm typecheck` 干净
- [ ] 9 个 heap_* 工具注册进 ToolRegistry（全 ReadOnly、needs_channel=false）
- [ ] 拉回 .hprof 自动预热（transfer hook → warm_up → provision_progress 事件）
- [ ] 会话关闭联动释放分析会话
- [ ] 文档四处（overview / umbrella / AGENTS / release CI）同步

