# JMC JFR 飞行记录分析 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Friday 新增 JFR 完整诊断闭环：`jfr_record`（远程热开启录制 → 自动拉回）+ 21 个 `jfr_*` JMC 分析工具（本地 stdio 工人进程，fork 自 jmc-mcp-server 并降级 Java 21）。

**Architecture:** 复制 `analyzer/`（heap MAT 集成）的成熟模式裁剪：`jfr/` 模块（client + manager，**无会话层**——上游 jmc-mcp-server 自带 TTL 缓存，工具直接接收 `jfr_file_path`）；`tools/builtin/jfr/` 工具契约层（record 工具走 jcmd + TransferManager，代理工具走 JmcManager 透传）；TransferManager 下载完成钩子泛化为列表（.hprof → MAT 预热、.jfr → JMC 预热）。

**Tech Stack:** Rust (Tauri 2, rmcp 3.1.4 stdio client, tokio)、React/TS（工具面板第 7 组）、PowerShell（fetch 脚本）、Maven/GitHub Actions（fork 侧构建）。

**Spec:** `docs/superpowers/specs/2026-09-03-jmc-jfr-analysis-design.md`

**约定（全任务生效）：**

- 日志遵从 `docs/architecture/logging-standard.md`：错误路径 `tracing::error!/warn!`，子进程 stderr 全量 drain；不做截断脱敏
- 测试命令：`cargo test --manifest-path src-tauri/Cargo.toml`（可加模块路径过滤）；检查：`cargo check --manifest-path src-tauri/Cargo.toml`、`pnpm typecheck`
- 提交信息沿用仓库现有风格（`feat:/docs:` 前缀，中文描述可选）
- **不要手动改版本号**（CI 从 tag 注入）

---

## 前置条件（人工，仓库外 —— 风险闸门，spec §9）

实施 Task 1 的 fetch 脚本**之前**需要先完成 fork 侧准备（需要 GitHub 账号操作，agent 无法代办；与用户协作完成）：

1. Fork https://github.com/scarletbean01/jmc-mcp-server 到用户可控的 GitHub org（若用户即上游作者，直接在上游开分支）。
2. 改根 `pom.xml`：`<maven.compiler.release>25</maven.compiler.release>` → `21`（若各子模块 pom 有独立 release 属性，一并改；用 `grep -r "compiler.release"` 确认）。**构建仍用 JDK 25 toolchain（setup-java java-version: 25），release=21 只降字节码级别**——产物可跑在 JRE 21+。
3. 在 fork 根目录添加 `.github/workflows/release.yml`：

```yaml
name: release
on:
  push:
    tags: ["v*"]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: "25"
      - name: Build fat JAR (bytecode 21)
        run: mvn -B clean package -DskipTests
      - name: Locate and rename fat JAR
        run: |
          jar=$(find . -name "*.jar" -path "*/target/*" ! -name "*-sources.jar" ! -name "original-*" | xargs ls -S | head -1)
          echo "found: $jar"
          cp "$jar" jmc-mcp-1.0.0.jar
      - uses: softprops/action-gh-release@v2
        with:
          files: jmc-mcp-1.0.0.jar
```

4. 提交改动 → `git tag v1.0.0-jfriday && git push origin v1.0.0-jfriday`。
5. 验证 Actions 构建成功且 Release 资产含 `jmc-mcp-1.0.0.jar`。
6. **风险闸门**：若 `mvn package` 在 release=21 下编译失败（上游使用 Java 25-only API），停止本计划并回退：保持 compiler release=25，Task 4 的 `production_client_factory` 处改为要求 Java 25（`analyzer::java::detect_java` 的阈值需参数化，jfr 侧传 25），并更新 spec §2 决策 4 与 AGENTS.md。Friday 侧其余设计不变。
7. 记下 fork 的 GitHub owner 名（Task 1 的 fetch 脚本默认 `scarletbean01`，不同则改 `-Owner` 参数默认值）。

---

### Task 1: fetch 脚本 + 资源目录 + 打包配置

**Files:**
- Create: `scripts/fetch-jmc-jar.ps1`
- Create: `src-tauri/resources/jmc/.gitkeep`
- Modify: `.gitignore`（文件末尾追加）
- Modify: `src-tauri/tauri.conf.json:28-33`（resources 数组）
- Modify: `.github/workflows/release.yml:62-68`（下载步骤后追加）

- [ ] **Step 1: 创建 `src-tauri/resources/jmc/.gitkeep`**

空文件（对齐 `resources/analyzer/.gitkeep` 模式）。

- [ ] **Step 2: 创建 `scripts/fetch-jmc-jar.ps1`**

```powershell
param(
    [string]$Owner = "scarletbean01",
    [string]$Tag = "v1.0.0-jfriday"
)
$ErrorActionPreference = "Stop"
$jarName = "jmc-mcp-1.0.0.jar"
$url = "https://github.com/$Owner/jmc-mcp-server/releases/download/$Tag/$jarName"
$destDir = Join-Path $PSScriptRoot "..\src-tauri\resources\jmc"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
$dest = Join-Path $destDir $jarName
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

注：若前置条件的 fork owner 不是 `scarletbean01`，把 `-Owner` 默认值改成实际 owner。

- [ ] **Step 3: `.gitignore` 末尾追加**

```
# JMC vendored JAR (fetched at build time, see scripts/fetch-jmc-jar.ps1)
src-tauri/resources/jmc/*.jar
src-tauri/resources/jmc/*.downloading
```

- [ ] **Step 4: `src-tauri/tauri.conf.json` resources 数组加一行**

找到（约 28-33 行）：

```json
    "resources": [
      "resources/model/*",
      "resources/model/onnx/*",
      "resources/analyzer/*",
      "resources/arthas/*"
    ],
```

改为：

```json
    "resources": [
      "resources/model/*",
      "resources/model/onnx/*",
      "resources/analyzer/*",
      "resources/arthas/*",
      "resources/jmc/*"
    ],
```

- [ ] **Step 5: `.github/workflows/release.yml` 下载步骤后追加**

在 `- name: Download arthas package` 块之后、`- name: Inject version from tag` 之前插入：

```yaml
      - name: Download JMC JAR
        shell: pwsh
        run: ./scripts/fetch-jmc-jar.ps1
```

- [ ] **Step 6: 验证脚本幂等逻辑（若 fork Release 已就绪则真实执行）**

Run: `./scripts/fetch-jmc-jar.ps1`（两次）
Expected: 第一次 `Downloading ...` + `Downloaded: ...`，第二次 `JAR already present`。若 fork Release 尚未就绪，跳过真实下载（Task 4 的 `#[ignore]` 集成测试前补跑）。

- [ ] **Step 7: Commit**

```bash
git add scripts/fetch-jmc-jar.ps1 src-tauri/resources/jmc/.gitkeep .gitignore src-tauri/tauri.conf.json .github/workflows/release.yml
git commit -m "feat: fetch script and packaging for vendored JMC JAR"
```

---

### Task 2: ToolCategory::Jfr 枚举 + 前端第 7 组

**Files:**
- Modify: `src-tauri/src/tools/category.rs`
- Modify: `src/lib/types.ts:113-119`
- Modify: `src/components/tools/ToolsPanel.tsx:26-46`

- [ ] **Step 1: `src-tauri/src/tools/category.rs` 加 `Jfr` 变体（Heap 之后）**

替换枚举定义与文档注释：

```rust
/// 工具分类。声明顺序即面板分组展示顺序（environment → jvm → heap → jfr → arthas → file_transfer → builtin）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Environment,
    Jvm,
    Heap,
    Jfr,
    Arthas,
    FileTransfer,
    Builtin,
}
```

- [ ] **Step 2: `src/lib/types.ts` ToolCategory union 加 `"jfr"`（heap 之后）**

```typescript
export type ToolCategory =
  | "environment"
  | "jvm"
  | "heap"
  | "jfr"
  | "arthas"
  | "file_transfer"
  | "builtin";
```

- [ ] **Step 3: `src/components/tools/ToolsPanel.tsx` 三处更新**

3a. import 列表加 `ChartLine`：

```typescript
import {
  Wrench,
  CircleNotch,
  CaretRight,
  CaretDown,
  Desktop,
  Cpu,
  ChartPie,
  ChartLine,
  Terminal,
  ArrowsLeftRight,
  Gear,
} from "@phosphor-icons/react";
```

3b. `CATEGORY_META`（26-33 行）在 heap 行后插入 jfr 行，并更新上方注释：

```typescript
// 分组展示顺序沿诊断流程：定位环境/进程 → JVM 基础诊断 → 堆分析 → JFR 飞行记录 → Arthas → 文件传输 → 通用
// 与后端 tools/category.rs 的 ToolCategory 声明序一致
const CATEGORY_META: { key: ToolCategory; label: string; icon: Icon }[] = [
  { key: "environment", label: "环境与进程", icon: Desktop },
  { key: "jvm", label: "JVM 诊断", icon: Cpu },
  { key: "heap", label: "堆快照分析", icon: ChartPie },
  { key: "jfr", label: "JFR 飞行记录", icon: ChartLine },
  { key: "arthas", label: "Arthas 动态诊断", icon: Terminal },
  { key: "file_transfer", label: "文件传输", icon: ArrowsLeftRight },
  { key: "builtin", label: "通用", icon: Gear },
];
```

3c. `collapsed` 初始 state（39-46 行）加 `jfr: true`：

```typescript
  const [collapsed, setCollapsed] = useState<Record<ToolCategory, boolean>>({
    environment: true,
    jvm: true,
    heap: true,
    jfr: true,
    arthas: true,
    file_transfer: true,
    builtin: true,
  });
```

- [ ] **Step 4: 验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过。

Run: `pnpm typecheck`
Expected: 通过。

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools`
Expected: 现有工具测试全部通过（分类断言用相等而非序号，不受影响）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tools/category.rs src/lib/types.ts src/components/tools/ToolsPanel.tsx
git commit -m "feat: ToolCategory::Jfr and tools panel group"
```

---

### Task 3: jfr/client.rs —— JmcClient trait + stdio 工人进程

**Files:**
- Create: `src-tauri/src/jfr/mod.rs`
- Create: `src-tauri/src/jfr/client.rs`
- Modify: `src-tauri/src/lib.rs:1-11`（mod 声明加 `mod jfr;`）

说明：本模块是 `analyzer/client.rs` 的同构裁剪，复用其 `CallOutcome`/`extract_text`（`pub`）、`analyzer::java::JavaInfo`/`detect_java`、`analyzer::manager::strip_verbatim_prefix`，不复制实现。

- [ ] **Step 1: 创建 `src-tauri/src/jfr/mod.rs`（本任务只声明 client）**

```rust
pub mod client;
```

（`pub mod manager;` 在 Task 4 Step 1 补。）

- [ ] **Step 2: 创建 `src-tauri/src/jfr/client.rs`（trait + 纯函数 + 单测先行）**

```rust
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

/// 一次上游工具调用结果（复用 analyzer 的形状；上游输出为 markdown 文本）
pub use crate::analyzer::client::CallOutcome;

#[async_trait]
pub trait JmcClient: Send + Sync {
    /// 调用上游 JMC MCP 工具。Err = 传输/进程层错误（进程疑似死亡）；
    /// 工具级错误 → Ok(CallOutcome { is_error: true, .. })
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String>;
    /// 终止工人进程
    async fn shutdown(&self);
}

/// 构造 JMC 工人进程 JVM 参数（纯函数，单独可测）。
/// UTF-8 强制三件套同 analyzer（issue #6：zh-CN Windows JVM 默认 GBK 输出）。
pub fn jmc_jvm_args(jar_path: &Path, xmx_gb: u32) -> Vec<String> {
    vec![
        format!("-Xmx{xmx_gb}g"),
        "-Dfile.encoding=UTF-8".to_string(),
        "-Dstdout.encoding=UTF-8".to_string(),
        "-Dstderr.encoding=UTF-8".to_string(),
        "-jar".to_string(),
        jar_path.to_string_lossy().into_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jmc_jvm_args_force_utf8() {
        let args = jmc_jvm_args(Path::new(r"C:\opt\jmc.jar"), 4);
        assert!(args.contains(&"-Dfile.encoding=UTF-8".to_string()), "args: {args:?}");
        assert!(args.contains(&"-Dstdout.encoding=UTF-8".to_string()));
        assert!(args.contains(&"-Dstderr.encoding=UTF-8".to_string()));
        assert_eq!(args.first().unwrap(), "-Xmx4g");
        assert_eq!(args.last().unwrap(), r"C:\opt\jmc.jar");
    }
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jfr::client`
Expected: PASS（1 个）。

- [ ] **Step 4: 补全 `McpJmcClient` + `spawn_jmc_client` + mock + 集成测试**

在 `jmc_jvm_args` 函数之后追加（与 `analyzer/client.rs:34-219` 同构，差异仅日志 target/文案）：

```rust
/// rmcp stdio 子进程实现：java -Xmx<n>g -jar <jar>，MCP client 角色。
/// rmcp 3.1.4 适配：`RunningService::cancel(self)` 消费所有权，故 service 存于
/// `Mutex<Option<..>>` 供 shutdown 取出取消；工具调用走克隆的 `Peer`。
pub struct McpJmcClient {
    peer: rmcp::service::Peer<rmcp::RoleClient>,
    service: tokio::sync::Mutex<Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>>,
}

/// 启动 JMC 工人进程并完成 MCP 握手（60s 超时）
pub async fn spawn_jmc_client(
    java: &crate::analyzer::java::JavaInfo,
    jar_path: &Path,
    xmx_gb: u32,
) -> Result<McpJmcClient, String> {
    use rmcp::ServiceExt;

    // verbatim 前缀（\\?\）会导致 java -jar ClassNotFound（issue #6），传入前必须剥掉
    let jar_path = crate::analyzer::manager::strip_verbatim_prefix(jar_path);

    let mut cmd = tokio::process::Command::new(&java.path);
    cmd.args(jmc_jvm_args(&jar_path, xmx_gb));
    let (transport, stderr) =
        rmcp::transport::child_process::TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 JMC 工人进程失败: {e}"))?;

    // 日志规范：子进程 stderr 必须读取记录（同时防止管道写满阻塞 JVM）。
    // read_until + from_utf8_lossy（GBK 安全），与 analyzer 相同。
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::with_capacity(256);
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF：进程退出
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf);
                        let line = line.trim_end_matches(['\n', '\r']);
                        if !line.is_empty() {
                            tracing::info!(target: "jmc_worker", "worker: {line}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "jmc worker stderr drain ended with error");
                        break;
                    }
                }
            }
        });
    }

    tracing::info!(java = %java.path.display(), jar = %jar_path.display(), xmx_gb, pid = ?transport.id(), "jmc worker spawning");
    let service = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ().serve(transport),
    )
    .await
    .map_err(|_| "JMC 工人进程初始化超时（60s）".to_string())?
    .map_err(|e| format!("JMC MCP 握手失败: {e}"))?;

    let peer = service.peer().clone();
    Ok(McpJmcClient {
        peer,
        service: tokio::sync::Mutex::new(Some(service)),
    })
}

#[async_trait]
impl JmcClient for McpJmcClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        // rmcp 3.1.4：Peer 侧入口为 call_tool_once；JMC 工具为一次性请求/响应，
        // 非 Complete 响应一律按传输层错误处理（对齐 analyzer client 适配写法）。
        let mut arguments = serde_json::Map::new();
        if let Value::Object(map) = args {
            for (k, v) in map {
                arguments.insert(k.clone(), v.clone());
            }
        } else {
            tracing::warn!(tool = %name, "non-object args passed to jmc client, treated as empty");
        }
        // rmcp 3.1.4：CallToolRequestParams 为 non_exhaustive，只能经 Default 构造
        let mut params = rmcp::model::CallToolRequestParams::default();
        params.name = name.to_string().into();
        params.arguments = Some(arguments);
        let result = self
            .peer
            .call_tool_once(params)
            .await
            .map_err(|e| format!("MCP 调用失败: {e}"))?;
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result,
            other => return Err(format!("MCP 调用返回非最终结果: {other:?}")),
        };
        Ok(CallOutcome {
            text: crate::analyzer::client::extract_text(&result),
            is_error: result.is_error.unwrap_or(false),
        })
    }

    async fn shutdown(&self) {
        // cancel 消费 RunningService：取出后优雅关闭传输（关 stdin → 等 3s → kill）
        if let Some(service) = self.service.lock().await.take() {
            match service.cancel().await {
                Ok(reason) => {
                    tracing::info!(reason = ?reason, "jmc worker shut down");
                }
                Err(e) => {
                    tracing::warn!(?e, "jmc worker service cancel failed");
                }
            }
        }
    }
}

// ── 测试 mock（全 crate 测试可用）──

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
pub struct MockJmcClient {
    pub calls: Arc<tokio::sync::Mutex<Vec<(String, Value)>>>,
    pub shutdown_count: Arc<std::sync::atomic::AtomicUsize>,
    handler: Arc<
        dyn Fn(&str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CallOutcome, String>> + Send>>
            + Send
            + Sync,
    >,
}

#[cfg(test)]
impl MockJmcClient {
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
impl JmcClient for MockJmcClient {
    async fn call_tool(&self, name: &str, args: &Value) -> Result<CallOutcome, String> {
        self.calls.lock().await.push((name.to_string(), args.clone()));
        (self.handler)(name, args).await
    }

    async fn shutdown(&self) {
        self.shutdown_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
```

并在 `mod tests` 内追加：

```rust
    #[tokio::test]
    async fn test_mock_client_records_calls() {
        let mock = MockJmcClient::ok("S");
        let out = mock.call_tool("jfr_overview", &serde_json::json!({"jfr_file_path": "x"})).await;
        assert!(out.is_ok());
        assert_eq!(out.unwrap().text, "S");
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "jfr_overview");
    }

    #[tokio::test]
    async fn test_mock_client_error_and_shutdown_count() {
        let mock = MockJmcClient::with_fn(|_name, _args| async { Err("boom".to_string()) });
        let out = mock.call_tool("jfr_overview", &serde_json::json!({})).await;
        match out {
            Err(e) => assert_eq!(e, "boom"),
            Ok(_) => panic!("expected Err, got Ok"),
        }
        mock.shutdown().await;
        mock.shutdown().await;
        assert_eq!(
            mock.shutdown_count.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    /// verbatim（\\?\）前缀的 JAR 路径必须仍能完成 MCP 握手（issue #6 回归，对齐 analyzer 同名测试）。
    /// 需要本机 Java 21+ 与已下载的 JAR（scripts/fetch-jmc-jar.ps1），
    /// 不进常规测试（CI 无 java），显式 `--ignored` 运行。
    /// ⚠ Java 21 降级验证闸门：本机 java -version 应为 21.x 才真正验证降级成功。
    #[tokio::test]
    #[ignore = "requires local Java 21 and vendored JAR (run scripts/fetch-jmc-jar.ps1)"]
    async fn test_spawn_jmc_client_with_verbatim_jar_path() {
        let java = crate::analyzer::java::detect_java()
            .await
            .expect("Java 21+ required for this test");
        let jar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/jmc/jmc-mcp-1.0.0.jar");
        assert!(jar.is_file(), "JAR missing: {} (run scripts/fetch-jmc-jar.ps1)", jar.display());
        // 复现 Tauri resource_dir() 返回的 verbatim 形式
        let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", jar.display()));
        let client = spawn_jmc_client(&java, &verbatim, 4)
            .await
            .expect("MCP handshake must succeed with verbatim jar path");
        let out = client
            .call_tool("jfr_overview", &serde_json::json!({"jfr_file_path": "nonexistent.jfr", "async": false}))
            .await
            .expect("tools/call must work");
        // 文件不存在 → 上游工具级错误（is_error=true），但传输层正常
        assert!(out.is_error, "expected tool-level error for nonexistent jfr, got: {}", out.text);
        client.shutdown().await;
    }
```

- [ ] **Step 5: `src-tauri/src/lib.rs` 模块声明区加 `mod jfr;`（按字母序，infra 与 knowledge 之间）**

```rust
mod agent;
mod analyzer;
mod app;
mod arthas;
mod exec;
mod infra;
mod jfr;
mod knowledge;
mod mcp;
mod provision;
mod tools;
mod transfer;
```

- [ ] **Step 6: 验证**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jfr::`
Expected: 3 PASS + 1 ignored。

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过。

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/jfr/ src-tauri/src/lib.rs
git commit -m "feat: JmcClient trait and stdio worker client"
```

---
### Task 4: jfr/manager.rs —— JmcManager（无会话层生命周期）

**Files:**
- Create: `src-tauri/src/jfr/manager.rs`
- Modify: `src-tauri/src/jfr/mod.rs`

- [ ] **Step 1: `src-tauri/src/jfr/mod.rs` 补 manager**

```rust
pub mod client;
pub mod manager;

pub use manager::{
    download_complete_hook, production_client_factory, ClientFactory, JmcConfig, JmcError,
    JmcManager, JMC_JAR_NAME, JMC_XMX_GB,
};
```

- [ ] **Step 2: 创建 manager.rs（类型骨架 + 测试先行；JmcManager 尚未实现 → 编译失败即失败态）**

创建 `src-tauri/src/jfr/manager.rs`，内容为「类型骨架 + 完整测试模块」：

```rust
use crate::analyzer::client::CallOutcome;
use crate::app::events::{AppEvent, EventBus};
use crate::jfr::client::JmcClient;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 预热（jfr_overview 触发上游建缓存）的内部硬超时，对齐 heap open 上限
const WARMUP_TASK_TIMEOUT_SECS: u64 = 1800;

/// vendored JMC JAR 文件名（scripts/fetch-jmc-jar.ps1 下载）
pub const JMC_JAR_NAME: &str = "jmc-mcp-1.0.0.jar";
/// JMC 工人进程堆预算（v1 常量起步，spec §2 决策 7）
pub const JMC_XMX_GB: u32 = 4;

#[derive(Debug, Clone, thiserror::Error)]
pub enum JmcError {
    #[error("{0}")]
    JavaMissing(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("JMC 调用超时（{0}s），工人进程保留未受影响")]
    Timeout(u64),
    #[error("{0}")]
    Upstream(String),
}

pub type ClientFactory = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Arc<dyn JmcClient>, JmcError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct JmcConfig {
    /// 无进行中调用持续该时长后退出工人进程
    pub idle_timeout: Duration,
    /// 空闲巡检间隔
    pub idle_tick: Duration,
}

impl Default for JmcConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(15 * 60),
            idle_tick: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jfr::client::MockJmcClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn manager_with(mock: &Arc<MockJmcClient>, config: JmcConfig) -> (JmcManager, Arc<AtomicUsize>) {
        let spawns = Arc::new(AtomicUsize::new(0));
        let s2 = spawns.clone();
        let mock2 = mock.clone();
        let factory: ClientFactory = Arc::new(move || {
            let mock = mock2.clone();
            let s2 = s2.clone();
            Box::pin(async move {
                s2.fetch_add(1, Ordering::SeqCst);
                let c: Arc<dyn JmcClient> = mock;
                Ok(c)
            })
        });
        (JmcManager::new(factory, EventBus::disabled(), config), spawns)
    }

    #[tokio::test]
    async fn test_query_lazy_spawns_once() {
        let mock = Arc::new(MockJmcClient::ok("OVERVIEW"));
        let (mgr, spawns) = manager_with(&mock, JmcConfig::default());
        let out = mgr
            .query("jfr_overview", &serde_json::json!({"jfr_file_path": "a.jfr"}), 5)
            .await
            .expect("query should succeed");
        assert_eq!(out.text, "OVERVIEW");
        mgr.query("jfr_rules", &serde_json::json!({"jfr_file_path": "a.jfr"}), 5)
            .await
            .unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "worker must spawn exactly once");
    }

    #[tokio::test]
    async fn test_query_upstream_error_kept_as_upstream() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Ok(CallOutcome { text: "bad jfr file".into(), is_error: true })
        }));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        match mgr.query("jfr_overview", &serde_json::json!({}), 5).await {
            Err(JmcError::Upstream(text)) => assert!(text.contains("bad jfr file")),
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_query_transport_error_invalidates_and_respawns() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Err("transport closed".to_string())
        }));
        let (mgr, spawns) = manager_with(&mock, JmcConfig::default());
        assert!(matches!(
            mgr.query("jfr_overview", &serde_json::json!({}), 5).await,
            Err(JmcError::Unavailable(_))
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "dead worker shut down");
        // 下次调用懒重建（再失败但工厂已再次拉起）
        assert!(matches!(
            mgr.query("jfr_overview", &serde_json::json!({}), 5).await,
            Err(JmcError::Unavailable(_))
        ));
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_timeout_does_not_kill_worker() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let mock = Arc::new(MockJmcClient::with_fn(move |_name, _args| {
            let c2 = c2.clone();
            async move {
                if c2.fetch_add(1, Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Ok(CallOutcome { text: "ok".into(), is_error: false })
            }
        }));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        assert!(matches!(
            mgr.query("jfr_overview", &serde_json::json!({}), 1).await,
            Err(JmcError::Timeout(1))
        ));
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 0, "timeout must NOT kill worker");
        mgr.query("jfr_overview", &serde_json::json!({}), 5).await.unwrap();
    }

    #[tokio::test]
    async fn test_idle_exit_shuts_down_worker() {
        let mock = Arc::new(MockJmcClient::ok("S"));
        let (mgr, spawns) = manager_with(
            &mock,
            JmcConfig {
                idle_timeout: Duration::from_millis(150),
                idle_tick: Duration::from_millis(20),
            },
        );
        mgr.query("jfr_overview", &serde_json::json!({}), 5).await.unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(mock.shutdown_count.load(Ordering::SeqCst), 1, "idle worker must exit");
        // 退出后再调用 → 工厂重新拉起
        mgr.query("jfr_overview", &serde_json::json!({}), 5).await.unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_warm_up_calls_overview_in_background() {
        let mock = Arc::new(MockJmcClient::ok("SUMMARY"));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        mgr.warm_up("sid-1", Path::new("/tmp/a.jfr")).await;
        let calls = mock.calls.lock().await;
        assert_eq!(calls.len(), 1, "warm_up issues exactly one jfr_overview");
        assert_eq!(calls[0].0, "jfr_overview");
        assert_eq!(calls[0].1["jfr_file_path"], "/tmp/a.jfr");
        assert_eq!(calls[0].1["async"], false);
    }

    #[tokio::test]
    async fn test_warm_up_failure_does_not_break_next_query() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Ok(CallOutcome { text: "corrupt".into(), is_error: true })
        }));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        mgr.warm_up("sid-1", Path::new("/tmp/a.jfr")).await;
        // 预热失败不阻断：后续 query 照常透传上游错误
        assert!(matches!(
            mgr.query("jfr_overview", &serde_json::json!({"jfr_file_path": "/tmp/a.jfr"}), 5).await,
            Err(JmcError::Upstream(_))
        ));
    }

    #[test]
    fn test_jmc_manager_new_outside_tokio_runtime_does_not_panic() {
        // 回归：lib.rs 的 Tauri setup 是同步上下文，new() 不得依赖运行时
        let factory: ClientFactory = Arc::new(|| {
            Box::pin(async { Err(JmcError::Unavailable("x".into())) })
        });
        let _mgr = JmcManager::new(factory, EventBus::disabled(), JmcConfig::default());
    }

    /// hook 扩展名分发：.jfr 触发预热，.hprof/其他不触发
    #[tokio::test]
    async fn test_download_complete_hook_only_fires_for_jfr() {
        let mock = Arc::new(MockJmcClient::ok("S"));
        let (mgr, _s) = manager_with(&mock, JmcConfig::default());
        let hook = download_complete_hook(&Arc::new(mgr.clone()));
        let mk = |name: &str| {
            crate::transfer::state::TransferState::new(
                crate::transfer::state::Direction::Download,
                "sid-1",
                "env-1",
                "/tmp/r.jfr",
                PathBuf::from(format!("C:/tmp/{name}")),
                false,
            )
        };
        hook(&mk("a.jfr"));
        hook(&mk("b.hprof"));
        hook(&mk("c.txt"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let calls = mock.calls.lock().await;
        let overviews: Vec<_> = calls.iter().filter(|(n, _)| *n == "jfr_overview").collect();
        assert_eq!(overviews.len(), 1, "only .jfr triggers warm_up, calls: {calls:?}");
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jfr::manager`
Expected: 编译失败（`JmcManager` / `download_complete_hook` 未定义）。

- [ ] **Step 4: 实现 JmcManager（追加到 `JmcConfig` 的 `impl Default` 之后、`mod tests` 之前）**

```rust
/// JMC 工人进程管理器（全局单例，无会话层：上游 jmc-mcp-server 自带 TTL 录制缓存，
/// 所有工具直接接收 jfr_file_path；Friday 只管进程生命周期）。
#[derive(Clone)]
pub struct JmcManager {
    inner: Arc<tokio::sync::Mutex<JmcInner>>,
    spawn_lock: Arc<tokio::sync::Mutex<()>>,
    client_factory: ClientFactory,
    bus: EventBus,
    config: JmcConfig,
}

struct JmcInner {
    client: Option<Arc<dyn JmcClient>>,
    inflight: u32,
    last_active: Instant,
    /// reaper 只在首个工人进程拉起时 spawn 一次（new() 无 runtime 上下文，禁止 tokio::spawn）
    reaper_spawned: bool,
}

impl JmcManager {
    pub fn new(client_factory: ClientFactory, bus: EventBus, config: JmcConfig) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(JmcInner {
                client: None,
                inflight: 0,
                last_active: Instant::now(),
                reaper_spawned: false,
            })),
            spawn_lock: Arc::new(tokio::sync::Mutex::new(())),
            client_factory,
            bus,
            config: config.clone(),
        }
    }

    /// 透传调用上游工具。传输错误 → invalidate + 懒重建（下次调用经工厂重拉）。
    pub async fn query(
        &self,
        upstream_tool: &str,
        upstream_args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, JmcError> {
        let client = self.ensure_client().await?;
        match self.guarded_call(&client, upstream_tool, upstream_args, timeout_secs).await {
            Err(JmcError::Unavailable(e)) => {
                tracing::error!(tool = %upstream_tool, error = %e, "jmc worker unavailable during query, invalidating");
                self.invalidate().await;
                Err(JmcError::Unavailable(e))
            }
            other => other,
        }
    }

    /// .jfr 拉回完成后的自动预热：后台调 jfr_overview 触发上游缓存加载 +
    /// provision_progress 事件。失败只记事件，不影响传输终态与后续调用。
    pub async fn warm_up(&self, session_id: &str, path: &Path) {
        let progress = |detail: String| AppEvent::ProvisionProgress {
            session_id: session_id.to_string(),
            tool: "jfr_record".to_string(),
            stage: "analyze".to_string(),
            detail,
        };
        self.bus.emit(
            session_id,
            progress(format!(
                "JFR 拉回完成，后台分析预热开始（JMC 解析建缓存）：{}",
                path.display()
            )),
        );
        let args = serde_json::json!({ "jfr_file_path": path.to_string_lossy(), "async": false });
        match self.query("jfr_overview", &args, WARMUP_TASK_TIMEOUT_SECS).await {
            Ok(_) => self.bus.emit(
                session_id,
                progress(format!("分析就绪，jfr_* 工具可直接查询：{}", path.display())),
            ),
            Err(e) => self.bus.emit(
                session_id,
                progress(format!("JFR 分析预热失败（不影响对话，可直接用 jfr_overview 重试）：{e}")),
            ),
        }
    }

    /// 显式停机（测试清理用；平时靠 idle reaper）
    pub async fn shutdown(&self) {
        let client = self.inner.lock().await.client.take();
        if let Some(c) = client {
            c.shutdown().await;
        }
    }

    // ── 内部 ──

    /// 带超时 + inflight 计数的上游调用
    async fn guarded_call(
        &self,
        client: &Arc<dyn JmcClient>,
        tool: &str,
        args: &serde_json::Value,
        timeout_secs: u64,
    ) -> Result<CallOutcome, JmcError> {
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
            Err(_) => Err(JmcError::Timeout(timeout_secs)),
            Ok(Err(e)) => Err(JmcError::Unavailable(e)),
            Ok(Ok(outcome)) if outcome.is_error => Err(JmcError::Upstream(outcome.text)),
            Ok(Ok(outcome)) => Ok(outcome),
        }
    }

    /// 确保工人进程客户端存在（不存在则经工厂拉起）。
    /// 首次拉起时启动 idle reaper（此处必在 async 上下文中运行）。
    async fn ensure_client(&self) -> Result<Arc<dyn JmcClient>, JmcError> {
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
        let client = (self.client_factory)().await?;
        tracing::info!("jmc worker process started");
        let mut spawn_reaper = false;
        {
            let mut inner = self.inner.lock().await;
            inner.client = Some(client.clone());
            inner.last_active = Instant::now();
            if !inner.reaper_spawned {
                inner.reaper_spawned = true;
                spawn_reaper = true;
            }
        }
        if spawn_reaper {
            self.spawn_idle_reaper();
        }
        Ok(client)
    }

    /// 工人进程失效：摘除客户端 + 尽力 shutdown（无会话表可清）
    async fn invalidate(&self) {
        let client = {
            let mut inner = self.inner.lock().await;
            let client = inner.client.take();
            inner.last_active = Instant::now();
            client
        };
        if let Some(c) = client {
            c.shutdown().await;
        }
    }

    /// 空闲巡检任务：无进行中调用且超过 idle_timeout 后关闭工人进程。
    /// 由 ensure_client 在首个客户端拉起后启动（每份共享状态恰一次）。
    fn spawn_idle_reaper(&self) {
        let mgr = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(mgr.config.idle_tick);
            loop {
                ticker.tick().await;
                let client = {
                    let mut inner = mgr.inner.lock().await;
                    let should = inner.client.is_some()
                        && inner.inflight == 0
                        && inner.last_active.elapsed() >= mgr.config.idle_timeout;
                    if should { inner.client.take() } else { None }
                };
                if let Some(client) = client {
                    tracing::info!("jmc worker idle (no inflight calls), shutting down");
                    client.shutdown().await;
                }
            }
        });
    }
}

/// 传输完成钩子：下载的 .jfr 完成后触发 JMC 预热（lib.rs 注入 TransferManager）。
/// 其余扩展名直接忽略；预热失败只记事件，不影响传输终态。
pub fn download_complete_hook(manager: &Arc<JmcManager>) -> crate::transfer::DownloadCompleteHook {
    let mgr = manager.clone();
    Arc::new(move |state: &crate::transfer::state::TransferState| {
        let is_jfr = state
            .local_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jfr"))
            .unwrap_or(false);
        if !is_jfr {
            return;
        }
        tracing::debug!(transfer_id = %state.id, jfr = %state.local_path.display(), session_id = %state.session_id, "jfr download complete, warming up analysis");
        let mgr = mgr.clone();
        let session_id = state.session_id.clone();
        let path = state.local_path.clone();
        tokio::spawn(async move {
            mgr.warm_up(&session_id, &path).await;
        });
    })
}

/// 生产 client 工厂：Java 探测（Ok 结果进程内缓存）→ stdio 子进程 MCP client。
/// jar 缺失（未跑 fetch 脚本）→ Unavailable 引导。
/// Java 阈值 21（fork 已降级；若前置条件降级失败回退，需将 detect_java 的
/// 版本判断参数化并在此要求 25，spec §9）。
pub fn production_client_factory(jar_path: Option<PathBuf>) -> ClientFactory {
    Arc::new(move || {
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
                    Err(e) => return Err(JmcError::JavaMissing(e)),
                },
            };
            let jar = jar.ok_or_else(|| {
                JmcError::Unavailable(
                    "JMC JAR 缺失（resources/jmc/）。请运行 scripts/fetch-jmc-jar.ps1 后重启。"
                        .to_string(),
                )
            })?;
            match crate::jfr::client::spawn_jmc_client(&java, &jar, JMC_XMX_GB).await {
                Ok(c) => {
                    let c: Arc<dyn JmcClient> = Arc::new(c);
                    Ok(c)
                }
                Err(e) => Err(JmcError::Unavailable(e)),
            }
        })
    })
}
```

（类型骨架中的 import 已含 `use std::sync::Arc;`，实现代码直接引用。）

- [ ] **Step 5: 追加真实工人集成测试（#[ignore]）到 manager.rs `mod tests` 末尾**

```rust
    /// 端到端集成（spec §7.5）：真实 spawn → jfr_overview → jfr_rules。
    /// 样例 .jfr 由 jcmd 对本机 JVM 录制生成。需要本机 Java 21（不是 25——
    /// 这才是降级闸门的真实验证）、jcmd、以及 fetch 脚本已下载的 JAR。
    #[tokio::test]
    #[ignore = "requires local Java 21, jcmd and vendored JAR"]
    async fn test_real_worker_overview_and_rules() {
        let java = crate::analyzer::java::detect_java()
            .await
            .expect("Java 21+ required for this test");
        assert_eq!(java.major, 21, "run with Java 21 to validate the downgrade gate (found {})", java.major);
        let jcmd = java.path.parent().unwrap().join(if cfg!(windows) { "jcmd.exe" } else { "jcmd" });
        assert!(jcmd.is_file(), "jcmd not found next to java: {}", jcmd.display());
        let tmp = tempfile::tempdir().unwrap();
        let jfr = tmp.path().join("sample.jfr");
        let out = std::process::Command::new(&jcmd)
            .args([
                format!("{}", std::process::id()),
                "JFR.start".to_string(),
                "name=friday-it".to_string(),
                "settings=default".to_string(),
                "duration=5s".to_string(),
                format!("filename={}", jfr.display()),
            ])
            .output()
            .expect("jcmd JFR.start must run");
        assert!(
            out.status.success(),
            "JFR.start failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !jfr.is_file() || std::fs::metadata(&jfr).map(|m| m.len()).unwrap_or(0) == 0 {
            assert!(std::time::Instant::now() < deadline, "recording file never materialized");
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let jar = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources/jmc")
            .join(JMC_JAR_NAME);
        assert!(jar.is_file(), "JAR missing: {} (run scripts/fetch-jmc-jar.ps1)", jar.display());
        let java_f = java.clone();
        let jar_f = jar.clone();
        let factory: ClientFactory = Arc::new(move || {
            let java = java_f.clone();
            let jar = jar_f.clone();
            Box::pin(async move {
                match crate::jfr::client::spawn_jmc_client(&java, &jar, JMC_XMX_GB).await {
                    Ok(c) => Ok(Arc::new(c) as Arc<dyn JmcClient>),
                    Err(e) => Err(JmcError::Unavailable(e)),
                }
            })
        });
        let mgr = JmcManager::new(factory, EventBus::disabled(), JmcConfig::default());
        let args = serde_json::json!({ "jfr_file_path": jfr.to_string_lossy(), "async": false });
        let out = mgr.query("jfr_overview", &args, 300).await.expect("jfr_overview");
        assert!(!out.text.trim().is_empty(), "overview output should not be empty");
        let out = mgr.query("jfr_rules", &args, 300).await.expect("jfr_rules");
        assert!(!out.text.trim().is_empty(), "rules output should not be empty");
        mgr.shutdown().await;
    }
```

注：`JavaInfo.major` 当前标 `#[allow(dead_code)]`（生产路径未读）——测试读取无需移除该属性。

- [ ] **Step 6: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml jfr::`
Expected: 10 PASS（含 hook/生命周期），2 ignored（client 1 + manager 1）。

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/jfr/
git commit -m "feat: JmcManager worker lifecycle with prewarm hook"
```

---

### Task 5: TransferManager 钩子泛化（单钩子 → 列表）

**Files:**
- Modify: `src-tauri/src/transfer/mod.rs:22-58`（字段与方法）、`mod.rs:242-247`（finish 触发点）、`mod.rs:445-469`（现有测试改造）
- Modify: `src-tauri/src/lib.rs:122-123`（调用点改名）

- [ ] **Step 1: 先改测试（编译失败即失败态）**

`src-tauri/src/transfer/mod.rs` 测试模块中，把现有 `test_download_complete_hook_invoked_for_completed_downloads_only`（445-469 行）替换为下面两个测试：

```rust
    #[tokio::test]
    async fn test_download_complete_hook_invoked_for_completed_downloads_only() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mut mgr = TransferManager::new(db, EventBus::disabled());
        let seen = Arc::new(std::sync::Mutex::new(Vec::<(Direction, Status)>::new()));
        let s2 = seen.clone();
        mgr.add_download_complete_hook(Arc::new(move |state: &TransferState| {
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

    #[tokio::test]
    async fn test_multiple_download_complete_hooks_all_fire() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mut mgr = TransferManager::new(db, EventBus::disabled());
        let hits = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        for name in ["mat", "jmc"] {
            let h = hits.clone();
            mgr.add_download_complete_hook(Arc::new(move |state: &TransferState| {
                h.lock().unwrap().push(format!("{name}:{}", state.remote_path));
            }));
        }
        let mgr = Arc::new(mgr);
        let id = mgr.start(make_state(Direction::Download, "/tmp/a.jfr", "s1")).await;
        mgr.finish(&id, Status::Completed, None, 10, 10).await;
        let hits = hits.lock().unwrap().clone();
        assert_eq!(
            hits,
            vec!["mat:/tmp/a.jfr", "jmc:/tmp/a.jfr"],
            "both hooks fire in registration order"
        );
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer::tests`
Expected: 编译失败（`add_download_complete_hook` 不存在）。

- [ ] **Step 3: 实现**

3a. 字段（`TransferManager` struct，约 25-32 行）：`download_complete_hook: Option<DownloadCompleteHook>` 改为 `download_complete_hooks: Vec<DownloadCompleteHook>`；上方类型注释（22-23 行）更新为：

```rust
/// 下载完成回调注入点（.hprof → MAT 预热、.jfr → JMC 预热等，按注册序全部触发）。
/// 必须在 Arc 包装前注册。
pub type DownloadCompleteHook = Arc<dyn Fn(&TransferState) + Send + Sync>;
```

3b. `new()` 中 `download_complete_hook: None` → `download_complete_hooks: Vec::new()`。

3c. setter（原 55-58 行）替换为：

```rust
    /// 注册下载完成回调（按注册序触发）。必须在 Arc 包装前调用，可多次。
    pub fn add_download_complete_hook(&mut self, hook: DownloadCompleteHook) {
        self.download_complete_hooks.push(hook);
    }
```

3d. `finish()` 触发点（原 242-247 行）替换为：

```rust
        // 下载完成回调（heap dump → MAT 预热、jfr → JMC 预热）。失败不影响传输终态。
        if event.direction == Direction::Download && event.status == Status::Completed {
            for hook in &self.download_complete_hooks {
                hook(&event);
            }
        }
```

- [ ] **Step 4: 修 lib.rs 调用点（122-123 行）**

```rust
            transfer_manager
                .add_download_complete_hook(crate::analyzer::download_complete_hook(&analyzer_manager));
```

- [ ] **Step 5: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml transfer::`
Expected: 全部 PASS。

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过（无 `set_download_complete_hook` 残留引用）。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/transfer/mod.rs src-tauri/src/lib.rs
git commit -m "feat: generalize download complete hooks to a list"
```

---

### Task 6: tools/builtin/jfr/mapping.rs —— 纯函数层

**Files:**
- Create: `src-tauri/src/tools/builtin/jfr/mapping.rs`
- Create: `src-tauri/src/tools/builtin/jfr/mod.rs`（本任务先只含 `pub mod mapping;`，Task 7 补全）
- Modify: `src-tauri/src/tools/builtin/mod.rs`（模块声明列表加 `pub mod jfr;`，按字母序）

- [ ] **Step 1: 创建 mapping.rs（类型 + 测试先行）**

```rust
use serde_json::{json, Value};

/// 代理型分析工具的 Friday → 上游映射（Compare 单独走 build_compare）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JfrProxyKind {
    Overview,
    Rules,
    QuickAnalysis,
    GcDetail,
    MemoryLeaks,
    PredictiveLeak,
    AllocationHotspots,
    HotMethods,
    ThreadCpu,
    CpuFlame,
    ThreadContention,
    DeadlockDetection,
    IoHotspots,
    Exceptions,
    Errors,
    Safepoints,
    VirtualThreads,
    StackTraceSearch,
    Correlate,
    RequestWaterfall,
}

impl JfrProxyKind {
    pub fn upstream_name(&self) -> &'static str {
        match self {
            JfrProxyKind::Overview => "jfr_overview",
            JfrProxyKind::Rules => "jfr_rules",
            JfrProxyKind::QuickAnalysis => "smart_quick_analysis",
            JfrProxyKind::GcDetail => "gc_detail",
            JfrProxyKind::MemoryLeaks => "memory_leaks",
            JfrProxyKind::PredictiveLeak => "smart_predictive_leak_analysis",
            JfrProxyKind::AllocationHotspots => "allocation_hotspots",
            JfrProxyKind::HotMethods => "hot_methods",
            JfrProxyKind::ThreadCpu => "thread_cpu",
            JfrProxyKind::CpuFlame => "cpu_flame",
            JfrProxyKind::ThreadContention => "thread_contention",
            JfrProxyKind::DeadlockDetection => "deadlock_detection",
            JfrProxyKind::IoHotspots => "io_hotspots",
            JfrProxyKind::Exceptions => "exception_analysis",
            JfrProxyKind::Errors => "error_analysis",
            JfrProxyKind::Safepoints => "safepoint_analysis",
            JfrProxyKind::VirtualThreads => "virtual_threads",
            JfrProxyKind::StackTraceSearch => "smart_stack_trace_search",
            JfrProxyKind::Correlate => "smart_correlate",
            JfrProxyKind::RequestWaterfall => "smart_request_waterfall",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_record_params_defaults() {
        let (d, s) = validate_record_params(&json!({})).unwrap();
        assert_eq!(d, 60);
        assert_eq!(s, "profile");
    }

    #[test]
    fn test_validate_record_params_bounds() {
        assert!(validate_record_params(&json!({"duration_secs": 10})).is_ok());
        assert!(validate_record_params(&json!({"duration_secs": 600})).is_ok());
        assert!(validate_record_params(&json!({"duration_secs": 9})).is_err());
        assert!(validate_record_params(&json!({"duration_secs": 601})).is_err());
        assert!(validate_record_params(&json!({"settings": "default"})).is_ok());
        assert!(validate_record_params(&json!({"settings": "boot"})).is_err());
    }

    #[test]
    fn test_jfr_start_command_shape() {
        let cmd = jfr_start_command(
            "/tmp/jdk/bin/jcmd",
            1234,
            "friday-777",
            60,
            "profile",
            "/tmp/friday-tools/recording-1234-777.jfr",
        );
        assert_eq!(
            cmd,
            "/tmp/jdk/bin/jcmd 1234 JFR.start name=friday-777 settings=profile duration=60s filename=/tmp/friday-tools/recording-1234-777.jfr"
        );
    }

    #[test]
    fn test_effective_record_timeout_matrix() {
        assert_eq!(effective_record_timeout(None, 60), 600);
        assert_eq!(effective_record_timeout(None, 300), 600);
        assert_eq!(effective_record_timeout(None, 600), 720);
        assert_eq!(effective_record_timeout(Some(1000), 60), 1000);
        assert_eq!(effective_record_timeout(Some(9999), 60), 1800);
        assert_eq!(effective_record_timeout(Some(30), 600), 720);
        assert_eq!(effective_record_timeout(Some(0), 60), 600);
        assert_eq!(effective_record_timeout(Some(-5), 60), 600);
    }

    #[test]
    fn test_build_proxy_maps_path_and_forces_sync() {
        let (name, args) = build_proxy(
            JfrProxyKind::HotMethods,
            r"C:\artifacts\a.jfr",
            Some(&json!({"top_n": 5, "async": true})),
        );
        assert_eq!(name, "hot_methods");
        assert_eq!(args["jfr_file_path"], r"C:\artifacts\a.jfr");
        assert_eq!(args["top_n"], 5);
        assert_eq!(args["async"], false, "async must be forced false even if caller passes true");
    }

    #[test]
    fn test_build_proxy_without_extra_args() {
        let (name, args) = build_proxy(JfrProxyKind::QuickAnalysis, "/tmp/a.jfr", None);
        assert_eq!(name, "smart_quick_analysis");
        assert_eq!(args["jfr_file_path"], "/tmp/a.jfr");
        assert_eq!(args["async"], false);
        assert_eq!(args.as_object().unwrap().len(), 2);
    }

    #[test]
    fn test_build_compare_two_paths() {
        let (name, args) =
            build_compare("/tmp/base.jfr", "/tmp/target.jfr", Some(&json!({"async": true})));
        assert_eq!(name, "smart_compare_recordings");
        assert_eq!(args["baseline_jfr_path"], "/tmp/base.jfr");
        assert_eq!(args["target_jfr_path"], "/tmp/target.jfr");
        assert_eq!(args["async"], false);
    }

    #[test]
    fn test_upstream_name_table_complete() {
        let kinds = [
            JfrProxyKind::Overview,
            JfrProxyKind::Rules,
            JfrProxyKind::QuickAnalysis,
            JfrProxyKind::GcDetail,
            JfrProxyKind::MemoryLeaks,
            JfrProxyKind::PredictiveLeak,
            JfrProxyKind::AllocationHotspots,
            JfrProxyKind::HotMethods,
            JfrProxyKind::ThreadCpu,
            JfrProxyKind::CpuFlame,
            JfrProxyKind::ThreadContention,
            JfrProxyKind::DeadlockDetection,
            JfrProxyKind::IoHotspots,
            JfrProxyKind::Exceptions,
            JfrProxyKind::Errors,
            JfrProxyKind::Safepoints,
            JfrProxyKind::VirtualThreads,
            JfrProxyKind::StackTraceSearch,
            JfrProxyKind::Correlate,
            JfrProxyKind::RequestWaterfall,
        ];
        assert_eq!(kinds.len(), 20);
        let names: Vec<&str> = kinds.iter().map(|k| k.upstream_name()).collect();
        assert!(names.iter().all(|n| !n.is_empty()));
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "upstream names must be unique");
    }
}
```

- [ ] **Step 2: 建模块声明，运行测试确认失败**

`src-tauri/src/tools/builtin/jfr/mod.rs`：

```rust
pub mod mapping;
```

`src-tauri/src/tools/builtin/mod.rs` 模块声明列表按字母序加：

```rust
pub mod jfr;
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::jfr`
Expected: 编译失败（`validate_record_params` 等未定义）。

- [ ] **Step 3: 实现纯函数（`JfrProxyKind` impl 之后、`mod tests` 之前）**

```rust
/// jfr_record 参数校验：duration_secs（10..=600，默认 60）+ settings 白名单（默认 profile）。
/// Err(String) → invalid_args。
pub fn validate_record_params(args: &Value) -> Result<(u32, String), String> {
    let duration = match args.get("duration_secs").and_then(|v| v.as_i64()) {
        None => 60,
        Some(n) if (10..=600).contains(&n) => n as u32,
        Some(n) => return Err(format!("duration_secs 必须在 10~600 之间，收到 {n}")),
    };
    let settings = match args.get("settings").and_then(|v| v.as_str()) {
        None | Some("profile") => "profile".to_string(),
        Some("default") => "default".to_string(),
        Some(other) => return Err(format!("settings 非法: {other}（可选 profile / default）")),
    };
    Ok((duration, settings))
}

/// jfr_record 有效总超时：默认 600/上限 1800，但必须容纳 duration + 120s 落盘余量。
pub fn effective_record_timeout(user: Option<i64>, duration_secs: u32) -> u64 {
    let base = match user {
        Some(t) if t > 0 => (t as u64).min(1800),
        _ => 600,
    };
    base.max(duration_secs as u64 + 120).min(1800)
}

/// JFR.start 命令构造（一次性定时录制；name/remote_path 由 handler 生成，纯函数可测）
pub fn jfr_start_command(
    jcmd: &str,
    pid: u32,
    name: &str,
    duration_secs: u32,
    settings: &str,
    remote_path: &str,
) -> String {
    format!("{jcmd} {pid} JFR.start name={name} settings={settings} duration={duration_secs}s filename={remote_path}")
}

/// 代理工具：local_path → jfr_file_path + args 透传合并；async 强制 false
/// （禁用上游后台任务模式，靠 Friday 超时分层，spec §3.2）。
pub fn build_proxy(kind: JfrProxyKind, local_path: &str, extra: Option<&Value>) -> (String, Value) {
    let mut map = serde_json::Map::new();
    map.insert("jfr_file_path".to_string(), json!(local_path));
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    // 最后强制覆盖：即使调用方透传了 async:true 也压回 false
    map.insert("async".to_string(), json!(false));
    (kind.upstream_name().to_string(), Value::Object(map))
}

/// A/B 对比：双路径映射 + args 透传合并；async 强制 false
pub fn build_compare(baseline: &str, target: &str, extra: Option<&Value>) -> (String, Value) {
    let mut map = serde_json::Map::new();
    map.insert("baseline_jfr_path".to_string(), json!(baseline));
    map.insert("target_jfr_path".to_string(), json!(target));
    if let Some(Value::Object(extra)) = extra {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    map.insert("async".to_string(), json!(false));
    ("smart_compare_recordings".to_string(), Value::Object(map))
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::jfr`
Expected: 全部 PASS（8 个）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/tools/builtin/jfr/ src-tauri/src/tools/builtin/mod.rs
git commit -m "feat: jfr tool mapping pure functions"
```

---
### Task 7: tools/builtin/jfr/mod.rs —— 工具契约层（record + 21 代理）

**Files:**
- Modify: `src-tauri/src/tools/builtin/jfr/mod.rs`（Task 6 只含 `pub mod mapping;`，本任务补全）

- [ ] **Step 1: 先写测试（追加 `mod tests` 到 mod.rs；实现未写 → 编译失败即失败态）**

在 mod.rs 现有 `pub mod mapping;` 之后追加完整测试模块：

```rust
#[cfg(test)]
mod tests {
    use crate::app::events::EventBus;
    use crate::exec::channel::{ExecChannel, ExecOutput};
    use crate::jfr::client::MockJmcClient;
    use crate::jfr::manager::{ClientFactory, JmcConfig, JmcManager};
    use crate::tools::builtin::jvm::core::JvmExecCore;
    use crate::tools::builtin::jvm::jdk_cache::JdkLayout;
    use crate::tools::category::ToolCategory;
    use crate::tools::registry::{ToolContext, ToolRegistry};
    use crate::tools::risk::RiskLevel;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;

    const SID: &str = "123e4567-e89b-12d3-a456-426614174000";

    /// JFR 感知的可编程 mock channel（对齐 heap_dump.rs 的 DumpChannel 模式）
    struct JfrChannel {
        start_exit: i32,
        stat_size: &'static str,
        calls: TokioMutex<Vec<String>>,
    }

    #[async_trait]
    impl ExecChannel for JfrChannel {
        async fn run(
            &self,
            cmd: &str,
        ) -> Result<ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
            self.calls.lock().await.push(cmd.to_string());
            if cmd.contains("JFR.start") {
                return Ok(ExecOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: self.start_exit,
                });
            }
            if cmd.starts_with("stat -c %s") {
                return Ok(ExecOutput {
                    stdout: self.stat_size.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                });
            }
            Ok(ExecOutput { stdout: String::new(), stderr: String::new(), exit_code: 0 })
        }
        async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            Ok(())
        }
        async fn disconnect(&self) {}
        async fn is_alive(&self) -> bool {
            true
        }
    }

    async fn setup(channel: Arc<dyn ExecChannel>) -> (tempfile::TempDir, Arc<JvmExecCore>, Arc<crate::transfer::TransferManager>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        let env_id = crate::app::env_save::save_environment(
            &db,
            None,
            "prod",
            "10.0.0.1",
            22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        )
        .await
        .unwrap()
        .environment
        .id;
        let exec_pool = Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new()));
        exec_pool.lock().await.insert_channel(env_id.clone(), channel).await;
        let mut bins = HashMap::new();
        bins.insert("jcmd".to_string(), "/tmp/jdk/bin/jcmd".to_string());
        let jdk_cache = Arc::new(crate::tools::builtin::jvm::jdk_cache::JdkCache::new());
        jdk_cache
            .set(&env_id, JdkLayout { tool_home: "/tmp/jdk".into(), bins })
            .await;
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let core = Arc::new(JvmExecCore {
            db: db.clone(),
            exec_pool,
            jdk_cache,
            artifacts_dir: artifacts.clone(),
        });
        let mgr = Arc::new(crate::transfer::TransferManager::new(db, EventBus::disabled()));
        (tmp, core, mgr)
    }

    fn jmc_manager(mock: Arc<MockJmcClient>) -> Arc<JmcManager> {
        let factory: ClientFactory = Arc::new(move || {
            let m = mock.clone();
            Box::pin(async move { Ok(m as Arc<dyn crate::jfr::client::JmcClient>) })
        });
        Arc::new(JmcManager::new(factory, EventBus::disabled(), JmcConfig::default()))
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: SID.into(), channel: None }
    }

    fn def<'a>(reg: &'a ToolRegistry, name: &str) -> &'a crate::tools::registry::ToolDef {
        reg.get(name).unwrap()
    }

    async fn registry(
        channel: Arc<dyn ExecChannel>,
        mock: Arc<MockJmcClient>,
    ) -> (tempfile::TempDir, ToolRegistry) {
        let (tmp, core, transfer) = setup(channel).await;
        let mut reg = ToolRegistry::new();
        register_all(
            &mut reg,
            jmc_manager(mock),
            core,
            EventBus::disabled(),
            transfer,
            tmp.path().join("artifacts"),
        );
        (tmp, reg)
    }

    fn jfr_file(dir: &std::path::Path) -> std::path::PathBuf {
        let p = dir.join("a.jfr");
        std::fs::write(&p, "fake jfr").unwrap();
        p
    }

    fn std_channel(stat: &'static str) -> Arc<JfrChannel> {
        Arc::new(JfrChannel { start_exit: 0, stat_size: stat, calls: TokioMutex::new(Vec::new()) })
    }

    #[tokio::test]
    async fn test_register_all_twenty_two_tools() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        let expected = [
            "jfr_record",
            "jfr_overview",
            "jfr_rules",
            "jfr_quick_analysis",
            "jfr_gc_detail",
            "jfr_memory_leaks",
            "jfr_predictive_leak",
            "jfr_allocation_hotspots",
            "jfr_hot_methods",
            "jfr_thread_cpu",
            "jfr_cpu_flame",
            "jfr_thread_contention",
            "jfr_deadlock_detection",
            "jfr_io_hotspots",
            "jfr_exceptions",
            "jfr_errors",
            "jfr_safepoints",
            "jfr_virtual_threads",
            "jfr_stack_trace_search",
            "jfr_correlate",
            "jfr_request_waterfall",
            "jfr_compare",
        ];
        assert_eq!(expected.len(), 22);
        for name in expected {
            let d = def(&reg, name);
            assert_eq!(d.category, ToolCategory::Jfr, "{name}");
            assert!(!d.needs_channel, "{name}");
        }
        assert_eq!(def(&reg, "jfr_record").risk_level, RiskLevel::Low);
        assert_eq!(def(&reg, "jfr_overview").risk_level, RiskLevel::ReadOnly);
        assert_eq!(def(&reg, "jfr_compare").risk_level, RiskLevel::ReadOnly);
        drop(tmp);
    }

    /// start_paused：wait_for_recording 的轮询休眠走 tokio 虚拟时钟，秒级等待瞬时完成
    #[tokio::test(start_paused = true)]
    async fn test_record_full_flow_starts_background_download() {
        let ch = std_channel("54321");
        let (tmp, reg) = registry(ch.clone(), Arc::new(MockJmcClient::ok("S"))).await;
        let out = def(&reg, "jfr_record")
            .handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234", "duration_secs": 10, "timeout_secs": 30}),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        let tid = out.data["transfer_id"].as_str().unwrap();
        assert!(!tid.is_empty());
        assert!(out.data["local_path"].as_str().unwrap().ends_with(".jfr"));
        assert_eq!(out.data["remote_size"], 54321);
        // 命令序列：JFR.start → 若干 stat 轮询
        let calls = ch.calls.lock().await;
        assert!(calls[0].contains("JFR.start"));
        assert!(calls[0].contains("duration=10s"));
        assert!(calls[0].contains("settings=profile"));
        assert!(calls[0].contains("filename=/tmp/friday-tools/recording-1234-"));
        assert!(calls.iter().skip(1).all(|c| c.starts_with("stat -c %s")));
        assert!(calls.len() >= 2);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_record_start_failure_passthrough_with_jdk8_hint() {
        let ch = Arc::new(JfrChannel { start_exit: 1, stat_size: "0", calls: TokioMutex::new(Vec::new()) });
        let (tmp, reg) = registry(ch, Arc::new(MockJmcClient::ok("S"))).await;
        let out = def(&reg, "jfr_record")
            .handler
            .execute(serde_json::json!({"environment": "prod", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "record_failed");
        assert!(
            out.data["message"].as_str().unwrap().contains("arthas_profiler"),
            "JDK 8 fallback hint required"
        );
        drop(tmp);
    }

    #[tokio::test(start_paused = true)]
    async fn test_record_file_never_materializes() {
        let (tmp, reg) = registry(std_channel("0"), Arc::new(MockJmcClient::ok("S"))).await;
        let out = def(&reg, "jfr_record")
            .handler
            .execute(
                serde_json::json!({"environment": "prod", "pid": "1234", "duration_secs": 10, "timeout_secs": 30}),
                &ctx(),
            )
            .await;
        assert!(!out.success, "out: {}", out.data);
        assert_eq!(out.data["error"], "record_not_found");
        assert!(out.data["message"].as_str().unwrap().contains("friday-tools"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_record_invalid_args() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        for args in [
            serde_json::json!({"environment": "prod"}),
            serde_json::json!({"pid": "1234"}),
            serde_json::json!({"environment": "prod", "pid": "1234", "duration_secs": 5}),
            serde_json::json!({"environment": "prod", "pid": "1234", "settings": "boot"}),
            serde_json::json!({"environment": "prod", "pid": "1; rm -rf /"}),
        ] {
            let out = def(&reg, "jfr_record").handler.execute(args, &ctx()).await;
            assert!(!out.success, "args should be rejected");
            assert_eq!(out.data["error"], "invalid_args");
        }
        drop(tmp);
    }

    #[tokio::test]
    async fn test_record_environment_not_found() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        let out = def(&reg, "jfr_record")
            .handler
            .execute(serde_json::json!({"environment": "nope", "pid": "1234"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_proxy_routes_to_upstream_with_path_and_sync() {
        let mock = Arc::new(MockJmcClient::ok("OVERVIEW"));
        let (tmp, reg) = registry(std_channel("1"), mock.clone()).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_overview")
            .handler
            .execute(
                serde_json::json!({"local_path": p.to_string_lossy(), "args": {"start_time": "2026-09-03T10:00:00Z"}}),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "jfr_overview");
        let calls = mock.calls.lock().await;
        let (name, args) = calls.last().unwrap();
        assert_eq!(name, "jfr_overview");
        assert_eq!(args["jfr_file_path"].as_str().unwrap(), p.to_string_lossy());
        assert_eq!(args["start_time"], "2026-09-03T10:00:00Z");
        assert_eq!(args["async"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_proxy_missing_params_and_file() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        // 缺 local_path
        let out = def(&reg, "jfr_hot_methods").handler.execute(serde_json::json!({}), &ctx()).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_args");
        // 文件不存在
        let out = def(&reg, "jfr_hot_methods")
            .handler
            .execute(serde_json::json!({"local_path": "C:/definitely/nope.jfr"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_path");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_compare_maps_two_paths() {
        let mock = Arc::new(MockJmcClient::ok("DIFF"));
        let (tmp, reg) = registry(std_channel("1"), mock.clone()).await;
        let base = jfr_file(tmp.path());
        let target = {
            let p = tmp.path().join("b.jfr");
            std::fs::write(&p, "fake").unwrap();
            p
        };
        let out = def(&reg, "jfr_compare")
            .handler
            .execute(
                serde_json::json!({
                    "baseline_local_path": base.to_string_lossy(),
                    "target_local_path": target.to_string_lossy()
                }),
                &ctx(),
            )
            .await;
        assert!(out.success, "out: {}", out.data);
        assert_eq!(out.data["tool"], "smart_compare_recordings");
        let calls = mock.calls.lock().await;
        let (name, args) = calls.last().unwrap();
        assert_eq!(name, "smart_compare_recordings");
        assert_eq!(args["baseline_jfr_path"].as_str().unwrap(), base.to_string_lossy());
        assert_eq!(args["target_jfr_path"].as_str().unwrap(), target.to_string_lossy());
        assert_eq!(args["async"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_compare_requires_both_paths() {
        let (tmp, reg) = registry(std_channel("1"), Arc::new(MockJmcClient::ok("S"))).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_compare")
            .handler
            .execute(serde_json::json!({"baseline_local_path": p.to_string_lossy()}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_args");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_jmc_unavailable_error_code() {
        let mock = Arc::new(MockJmcClient::with_fn(|_name, _args| async {
            Err("transport closed".to_string())
        }));
        let (tmp, reg) = registry(std_channel("1"), mock).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_overview")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy()}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "jmc_unavailable");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upstream_error_passthrough_and_truncation() {
        let big = format!("JMC boom\n{}", "x".repeat(70 * 1024));
        let mock = Arc::new(MockJmcClient::with_fn(move |_name, _args| {
            let big = big.clone();
            async move { Ok(crate::analyzer::client::CallOutcome { text: big, is_error: true }) }
        }));
        let (tmp, reg) = registry(std_channel("1"), mock).await;
        let p = jfr_file(tmp.path());
        let out = def(&reg, "jfr_rules")
            .handler
            .execute(serde_json::json!({"local_path": p.to_string_lossy()}), &ctx())
            .await;
        assert!(!out.success);
        // 业务错误透传：无 error code，result 携带上游文本 + upstream_is_error
        assert_eq!(out.data["error"], serde_json::Value::Null);
        assert_eq!(out.data["upstream_is_error"], true);
        assert!(out.data["result"].as_str().unwrap().contains("JMC boom"));
        assert_eq!(out.data["truncated"], true);
        assert!(out.data["result"].as_str().unwrap().contains("[truncated"));
        let full = out.data["full_output_path"].as_str().unwrap();
        assert!(std::fs::metadata(full).map(|m| m.len() as usize > 70 * 1024).unwrap_or(false));
        drop(tmp);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::jfr`
Expected: 编译失败（`register_all` / `JfrRecordHandler` 等未定义）。

- [ ] **Step 3: 实现 —— 在 `pub mod mapping;` 与 `mod tests` 之间写 imports + 类型 + handler**

```rust
use crate::app::events::{AppEvent, EventBus};
use crate::exec::channel::ExecChannel;
use crate::jfr::{JmcError, JmcManager};
use crate::tools::builtin::jvm::core::{
    clamp_or, error_output, is_jdk_missing, parse_pid, require_bins, resolve_environment,
    JvmExecCore,
};
use crate::tools::builtin::run_command::{artifact_dir_for, truncate_output};
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// (default_secs, max_secs)
type Timeouts = (u64, u64);
const RECORD: Timeouts = (600, 1800);
const QUERY: Timeouts = (60, 300);
const HEAVY: Timeouts = (300, 1800);

/// 录制落盘轮询间隔（虚拟时钟友好，测试 start_paused 可瞬时推进）
const RECORD_POLL_INTERVAL_SECS: u64 = 3;

/// jfr_record：一次性定时录制 + 后台拉回
pub struct JfrRecordHandler {
    pub core: Arc<JvmExecCore>,
    pub bus: EventBus,
    pub transfer: Arc<crate::transfer::TransferManager>,
}

/// jfr_compare / 代理分析工具
pub struct JfrProxyHandler {
    pub jmc: Arc<JmcManager>,
    pub artifacts_dir: PathBuf,
    pub kind: JfrToolKind,
    pub timeouts: Timeouts,
}

#[derive(Debug, Clone, Copy)]
pub enum JfrToolKind {
    Compare,
    Proxy(mapping::JfrProxyKind),
}

#[async_trait]
impl ToolHandler for JfrRecordHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.execute_record(&args, ctx).await
    }
}

#[async_trait]
impl ToolHandler for JfrProxyHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        match self.kind {
            JfrToolKind::Compare => self.execute_compare(&args, ctx).await,
            JfrToolKind::Proxy(kind) => self.execute_proxy(kind, &args, ctx).await,
        }
    }
}

impl JfrRecordHandler {
    async fn execute_record(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return error_output("invalid_args", "missing required parameter: environment");
        };
        let Some(pid) = args.get("pid").and_then(|v| parse_pid(v)) else {
            return error_output("invalid_args", "pid 必须是正整数字符串");
        };
        let (duration_secs, settings) = match mapping::validate_record_params(args) {
            Ok(v) => v,
            Err(e) => return error_output("invalid_args", &e),
        };
        let timeout_secs = mapping::effective_record_timeout(
            args.get("timeout_secs").and_then(|v| v.as_i64()),
            duration_secs,
        );

        let (env, channel) = match resolve_environment(&self.core.db, &self.core.exec_pool, environment).await {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                return error_output(
                    "environment_not_found",
                    &format!(
                        "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
                    ),
                );
            }
            Err(e) => return error_output("connection_error", &e),
        };

        // JDK 路径：查缓存，miss 引导 ensure_tool
        let Some(layout) = self.core.jdk_cache.get(&env.id).await else {
            tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, "jdk not provisioned (cache miss)");
            return error_output(
                "jdk_not_provisioned",
                "该环境尚未装备 JDK。请先调用 ensure_tool(environment, tool=\"jdk\") 装备，然后重试本工具。",
            );
        };
        let bins = match require_bins(&layout, &["jcmd"]) {
            Ok(b) => b,
            Err(e) => return error_output("jdk_not_provisioned", &e),
        };
        let jcmd = &bins[0];

        // ① 一次性定时录制（文件名 Friday 固定构造——不开放自定义，注入面）
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let remote_path = format!("/tmp/friday-tools/recording-{pid}-{ts}.jfr");
        let name = format!("friday-{ts}");
        let start_cmd = mapping::jfr_start_command(jcmd, pid, &name, duration_secs, &settings, &remote_path);

        tracing::info!(session_id = %ctx.session_id, env_id = %env.id, pid, command = %start_cmd, "jfr record: starting");
        self.emit_progress(
            &ctx.session_id,
            "record",
            &format!("JFR 录制已启动（{duration_secs}s，settings={settings}），等待落盘…"),
        );

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let start_timeout = timeout_secs.min(120);
        let start_output = match tokio::time::timeout(
            std::time::Duration::from_secs(start_timeout),
            channel.run(&start_cmd),
        )
        .await
        {
            Err(_) => {
                tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, timeout_secs = start_timeout, "JFR.start timed out, dropping connection");
                {
                    let mut pool = self.core.exec_pool.lock().await;
                    pool.disconnect(&env.id).await;
                }
                return error_output(
                    "timeout_error",
                    &format!("JFR.start 超时（{start_timeout}s）；ssh 连接已断开"),
                );
            }
            Ok(Err(e)) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, error = %e, "JFR.start exec failed");
                return error_output("connection_error", &e.to_string());
            }
            Ok(Ok(output)) => {
                if is_jdk_missing(output.exit_code, &output.stderr) {
                    tracing::warn!(session_id = %ctx.session_id, env_id = %env.id, "jdk missing on remote, clearing cache");
                    self.core.jdk_cache.clear(&env.id).await;
                    return error_output(
                        "jdk_missing_on_remote",
                        "远端 JDK 已不存在（可能 /tmp 被清理）。请重新调用 ensure_tool 装备后重试。",
                    );
                }
                if output.exit_code != 0 {
                    // JFR.start 失败：透传 jcmd 输出 + 兼容性提示（JDK 8 场景）
                    tracing::error!(session_id = %ctx.session_id, env_id = %env.id, exit_code = output.exit_code, "JFR.start command failed");
                    return ToolOutput {
                        success: false,
                        data: serde_json::json!({
                            "error": "record_failed",
                            "message": "JFR.start 失败。目标 JVM 兼容性：JDK 11+ 开箱即用；Oracle JDK 8 需启动参数 -XX:+UnlockCommercialVMOption -XX:+FlightRecorder；OpenJDK 8 无 JFR——此类场景改用 arthas_profiler。",
                            "stdout": output.stdout,
                            "stderr": output.stderr,
                            "exit_code": output.exit_code,
                        }),
                        raw_stdout: Some(output.stdout),
                    };
                }
                output
            }
        };

        // ② 等待录制落盘：duration 到期 + 文件大小稳定（两次轮询相等且非零）
        let remote_size = match wait_for_recording(&channel, &remote_path, duration_secs, deadline).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, env_id = %env.id, remote_path, "recording file never materialized");
                return error_output("record_not_found", &e);
            }
        };

        // ③ 后台拉回：TransferManager（MCP 同步调用返回，Agent 轮询 transfer_status）
        let session_dir = artifact_dir_for(&self.core.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(format!("recording-{pid}-{ts}.jfr"));
        let state = crate::transfer::state::TransferState::new(
            crate::transfer::state::Direction::Download,
            &ctx.session_id,
            &env.id,
            &remote_path,
            local_path.clone(),
            true, // 下载成功后清理远端（Friday 自己生成的文件）
        );
        let transfer_id = self.transfer.start(state).await;

        self.emit_progress(
            &ctx.session_id,
            "download",
            "录制完成，后台拉回已启动（轮询 transfer_status 获取进度）",
        );

        tracing::info!(
            session_id = %ctx.session_id, env_id = %env.id, pid,
            transfer_id = %transfer_id,
            remote_path, remote_size, duration_secs, settings,
            "jfr recording complete, background download started"
        );

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "remote_path": remote_path,
                "remote_size": remote_size,
                "duration_secs": duration_secs,
                "settings": settings,
                "local_path": local_path.to_string_lossy(),
                "note": "JFR 录制完成，正在后台拉回。请轮询 transfer_status(transfer_id)；completed 后自动预热 JMC 分析，用 jfr_quick_analysis(local_path) / jfr_rules(local_path) 起步诊断；failed 时远端文件保留，可用 file_download 重试（断点续传）。",
            }),
            raw_stdout: Some(start_output.stdout),
        }
    }

    fn emit_progress(&self, session_id: &str, stage: &str, detail: &str) {
        self.bus.emit(
            session_id,
            AppEvent::ProvisionProgress {
                session_id: session_id.to_string(),
                tool: "jfr_record".to_string(),
                stage: stage.to_string(),
                detail: detail.to_string(),
            },
        );
    }
}

impl JfrProxyHandler {
    async fn execute_compare(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let baseline_raw = args.get("baseline_local_path").and_then(|v| v.as_str());
        let target_raw = args.get("target_local_path").and_then(|v| v.as_str());
        let (Some(baseline_raw), Some(target_raw)) = (baseline_raw, target_raw) else {
            return error_output(
                "invalid_args",
                "missing required parameters: baseline_local_path / target_local_path（两次录制各一份）",
            );
        };
        let baseline = match resolve_existing_file(baseline_raw) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_path", &e),
        };
        let target = match resolve_existing_file(target_raw) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_path", &e),
        };
        let resolved = format!("{} -> {}", baseline.display(), target.display());
        let timeout_secs =
            clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts.0, self.timeouts.1);
        let (upstream, upstream_args) = mapping::build_compare(
            &baseline.to_string_lossy(),
            &target.to_string_lossy(),
            args.get("args"),
        );
        self.run_query(&upstream, &upstream_args, &resolved, timeout_secs, ctx).await
    }

    async fn execute_proxy(
        &self,
        kind: mapping::JfrProxyKind,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let Some(local_path) = args.get("local_path").and_then(|v| v.as_str()) else {
            return error_output("invalid_args", "missing required parameter: local_path");
        };
        let path = match resolve_existing_file(local_path) {
            Ok(p) => p,
            Err(e) => return error_output("invalid_path", &e),
        };
        let resolved = path.display().to_string();
        let timeout_secs =
            clamp_or(args.get("timeout_secs").and_then(|v| v.as_i64()), self.timeouts.0, self.timeouts.1);
        let (upstream, upstream_args) = mapping::build_proxy(kind, &resolved, args.get("args"));
        self.run_query(&upstream, &upstream_args, &resolved, timeout_secs, ctx).await
    }

    async fn run_query(
        &self,
        upstream: &str,
        upstream_args: &serde_json::Value,
        resolved_path: &str,
        timeout_secs: u64,
        ctx: &ToolContext,
    ) -> ToolOutput {
        let start = std::time::Instant::now();
        tracing::info!(session_id = %ctx.session_id, upstream = %upstream, jfr = %resolved_path, timeout_secs, "jfr tool executing");
        match self.jmc.query(upstream, upstream_args, timeout_secs).await {
            Ok(outcome) => {
                render(&ctx.session_id, &self.artifacts_dir, upstream, resolved_path, &outcome.text, start, true)
                    .await
            }
            Err(e) => {
                tracing::warn!(session_id = %ctx.session_id, upstream = %upstream, error = %e, "jfr tool failed");
                self.jmc_error_output(e, &ctx.session_id, upstream, resolved_path, start)
                    .await
            }
        }
    }

    /// JmcError → 结构化错误输出。Upstream（JMC 业务错误）走透传（无 error code，
    /// 对齐 heap_*/jvm_* 惯例），但同样经过 64KB 截断 + 完整结果落盘路径。
    async fn jmc_error_output(
        &self,
        e: JmcError,
        session_id: &str,
        upstream_tool: &str,
        local_path: &str,
        start: std::time::Instant,
    ) -> ToolOutput {
        match e {
            JmcError::JavaMissing(m) => error_output(
                "java_missing",
                &format!("本机 Java 21+ 不可用：{m}。请安装 JDK 21+ 后重试。"),
            ),
            JmcError::Unavailable(m) => error_output(
                "jmc_unavailable",
                &format!("{m}。可重试一次；连续失败请查看 Friday 日志。"),
            ),
            JmcError::Timeout(t) => error_output(
                "jmc_timeout",
                &format!("JMC 分析调用超时（{t}s）。工人进程未受影响，可加大 timeout_secs 或用 start_time/end_time 缩小时间窗后重试。"),
            ),
            JmcError::Upstream(text) => {
                render(session_id, &self.artifacts_dir, upstream_tool, local_path, &text, start, false).await
            }
        }
    }
}

/// 等待录制落盘：duration 到期后文件存在（size > 0）且两次轮询大小相等 → 稳定。
/// deadline 用尽 → Err（附远端路径与已等待时长）。
/// 全程 tokio 虚拟时钟友好（测试 start_paused 瞬时推进）。
async fn wait_for_recording(
    channel: &Arc<dyn ExecChannel>,
    remote_path: &str,
    duration_secs: u32,
    deadline: tokio::time::Instant,
) -> Result<u64, String> {
    let start = tokio::time::Instant::now();
    let mut last_size: u64 = 0;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "录制到时后文件未就绪：{remote_path}（已等待 {}s）。远端文件可能仍在写入，可稍后用 file_download 手动拉回",
                start.elapsed().as_secs()
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(RECORD_POLL_INTERVAL_SECS)).await;
        let stat_cmd = format!("stat -c %s {remote_path}");
        let size: u64 = match channel.run(&stat_cmd).await {
            Ok(o) if o.exit_code == 0 => o.stdout.trim().parse().unwrap_or(0),
            _ => 0,
        };
        let elapsed = start.elapsed().as_secs();
        if elapsed >= duration_secs as u64 && size > 0 && size == last_size {
            return Ok(size);
        }
        last_size = size;
    }
}

/// 结果组装：64KB 头部截断 + 完整结果落盘 session artifacts（复用 run_command 机制）。
/// success=false 用于上游业务错误透传（upstream_is_error 标记，无 error code）。
async fn render(
    session_id: &str,
    artifacts_dir: &Path,
    upstream_tool: &str,
    local_path: &str,
    text: &str,
    start: std::time::Instant,
    success: bool,
) -> ToolOutput {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let (body, truncated) = truncate_output(text);
    let session_dir = artifact_dir_for(artifacts_dir, session_id);
    let artifact_path = session_dir.join(format!("jfr-{}.md", uuid::Uuid::new_v4()));
    let full = format!("--- tool: {upstream_tool} ---\n--- local_path: {local_path} ---\n--- full output ---\n{text}\n");
    let mut full_output_path = None;
    match tokio::fs::create_dir_all(&session_dir).await {
        Ok(()) => {
            if tokio::fs::write(&artifact_path, &full).await.is_ok() {
                full_output_path = Some(artifact_path);
            } else {
                tracing::warn!(session_id, tool = upstream_tool, "failed to persist full jfr tool output");
            }
        }
        Err(e) => {
            tracing::warn!(session_id, tool = upstream_tool, error = %e, "failed to create artifacts dir");
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
    if success {
        tracing::info!(session_id, tool = upstream_tool, elapsed_ms, truncated, "jfr tool executed");
    } else {
        tracing::warn!(session_id, tool = upstream_tool, elapsed_ms, truncated, "jfr tool upstream error passthrough");
    }
    let mut data = serde_json::json!({
        "tool": upstream_tool,
        "local_path": local_path,
        "result": result_field,
        "elapsed_ms": elapsed_ms,
        "truncated": truncated,
        "full_output_path": full_output_path.as_ref().map(|p| p.display().to_string()),
    });
    if !success {
        data["upstream_is_error"] = serde_json::json!(true);
    }
    ToolOutput {
        success,
        data,
        raw_stdout: Some(text.to_string()),
    }
}

/// local_path 解析：相对路径以 cwd 补全 + 必须是已存在文件。
fn resolve_existing_file(raw: &str) -> Result<PathBuf, String> {
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
```

- [ ] **Step 4: 实现 register_all（22 工具注册，加在 render/resolve_existing_file 之后、`mod tests` 之前）**

```rust
fn record_tool_def(
    core: &Arc<JvmExecCore>,
    bus: &EventBus,
    transfer: &Arc<crate::transfer::TransferManager>,
) -> ToolDef {
    ToolDef {
        name: "jfr_record".to_string(),
        description: "对目标 JVM 热开启 JFR 飞行录制并后台拉回（jcmd JFR.start，目标需 JDK 11+，profile 档开销约 1~3%，不中断服务）。一次性定时录制 duration_secs 秒（10~600，默认 60）后自动落盘 → 后台拉回（返回 transfer_id，轮询 transfer_status）→ completed 后自动预热 JMC 分析，直接用 jfr_quick_analysis / jfr_rules 起步。⚠ 目标 JDK 8 不支持热开启 JFR（Oracle JDK 8 需启动参数，OpenJDK 8 无 JFR），此类场景改用 arthas_profiler。需先 ensure_tool 装备 JDK。".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                "pid": { "type": "string", "description": "目标 Java 进程 PID（list_processes 返回）" },
                "duration_secs": { "type": "number", "description": "录制时长秒数，10~600，默认 60" },
                "settings": { "type": "string", "enum": ["profile", "default"], "description": "事件档位：profile 全维度（开销 1~3%），default 低开销（<1%），默认 profile" },
                "timeout_secs": { "type": "number", "description": "总超时秒数（含录制等待与落盘轮询），默认 600，上限 1800；实际下限为 duration_secs+120" }
            },
            "required": ["environment", "pid"]
        }),
        risk_level: RiskLevel::Low,
        category: ToolCategory::Jfr,
        needs_channel: false,
        handler: Arc::new(JfrRecordHandler {
            core: core.clone(),
            bus: bus.clone(),
            transfer: transfer.clone(),
        }),
    }
}

fn proxy_tool_def(
    name: &str,
    description: &str,
    kind: JfrToolKind,
    timeouts: Timeouts,
    jmc: &Arc<JmcManager>,
    artifacts_dir: &Path,
) -> ToolDef {
    let schema = match kind {
        JfrToolKind::Compare => serde_json::json!({
            "type": "object",
            "properties": {
                "baseline_local_path": { "type": "string", "description": "基准录制（如正常期）的本机路径" },
                "target_local_path": { "type": "string", "description": "对比录制（如故障期）的本机路径" },
                "args": { "type": "object", "description": "上游选项透传（start_time/end_time 等）" },
                "timeout_secs": { "type": "number", "description": format!("超时秒数，默认 {}，上限 {}", timeouts.0, timeouts.1) }
            },
            "required": ["baseline_local_path", "target_local_path"]
        }),
        JfrToolKind::Proxy(_) => serde_json::json!({
            "type": "object",
            "properties": {
                "local_path": { "type": "string", "description": "本机 JFR 录制文件绝对路径（jfr_record 返回的 local_path 或用户已有文件）" },
                "args": { "type": "object", "description": "上游分析选项透传（如 top_n / thread_name / package_prefix / focus / class_pattern / start_time / end_time，见工具描述）" },
                "timeout_secs": { "type": "number", "description": format!("超时秒数，默认 {}，上限 {}", timeouts.0, timeouts.1) }
            },
            "required": ["local_path"]
        }),
    };
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Jfr,
        needs_channel: false,
        handler: Arc::new(JfrProxyHandler {
            jmc: jmc.clone(),
            artifacts_dir: artifacts_dir.to_path_buf(),
            kind,
            timeouts,
        }),
    }
}

/// 注册全部 jfr_* 工具（lib.rs 调用）：1 录制 + 20 代理 + 1 对比
pub fn register_all(
    registry: &mut crate::tools::registry::ToolRegistry,
    jmc: Arc<JmcManager>,
    core: Arc<JvmExecCore>,
    bus: EventBus,
    transfer: Arc<crate::transfer::TransferManager>,
    artifacts_dir: PathBuf,
) {
    registry.register(record_tool_def(&core, &bus, &transfer));

    // (Friday 名, 描述, 代理类型, 超时档)——声明序即面板内名称序的输入（注册表按名排序展示）
    let proxies: &[(&str, &str, mapping::JfrProxyKind, Timeouts)] = &[
        ("jfr_overview", "JFR 录制总览：录制时长、事件数、JVM/系统信息。分析起点（jfr_record 完成预热后秒回）。args 可选：start_time/end_time。", mapping::JfrProxyKind::Overview, QUERY),
        ("jfr_rules", "JMC 规则引擎自动瓶颈检测（GC/内存/CPU/锁/IO 规则，带严重度与建议）。录制体检首选。args 可选：min_severity/start_time/end_time。", mapping::JfrProxyKind::Rules, QUERY),
        ("jfr_quick_analysis", "一键宏诊断仪表盘：自动检测主瓶颈并按严重度分类（CPU/内存/锁/IO）。性能问题第一步。args 可选：focus（cpu/memory/locks/io）/start_time/end_time。", mapping::JfrProxyKind::QuickAnalysis, HEAVY),
        ("jfr_gc_detail", "GC 深度分析：分阶段暂停耗时、GC cause 分布、堆趋势、GC 配置。args 可选：detail_level/start_time/end_time。", mapping::JfrProxyKind::GcDetail, QUERY),
        ("jfr_memory_leaks", "老对象采样泄漏分析：按类统计存活老对象（JFR 对象采样），定位疑似泄漏类；与 heap_*（MAT）互补。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::MemoryLeaks, HEAVY),
        ("jfr_predictive_leak", "数学检测内存泄漏：对 post-GC 堆使用做线性回归（r_squared 拟合度），泄漏趋势确认。args 可选：r_squared_threshold/start_time/end_time。", mapping::JfrProxyKind::PredictiveLeak, HEAVY),
        ("jfr_allocation_hotspots", "内存分配热点：按类和分配调用点统计分配速率，定位分配风暴。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::AllocationHotspots, QUERY),
        ("jfr_hot_methods", "CPU 热点方法 Top N（执行采样）。args 可选：top_n/thread_name/package_prefix/start_time/end_time。", mapping::JfrProxyKind::HotMethods, QUERY),
        ("jfr_thread_cpu", "线程级 CPU 消耗排名（执行采样）。args 可选：top_n/package_prefix/start_time/end_time。", mapping::JfrProxyKind::ThreadCpu, QUERY),
        ("jfr_cpu_flame", "CPU 火焰图数据：热点调用路径 + 线程状态。args 可选：top_n/package_prefix/start_time/end_time。", mapping::JfrProxyKind::CpuFlame, HEAVY),
        ("jfr_thread_contention", "锁竞争分析：monitor 阻塞/挂起/等待统计。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::ThreadContention, QUERY),
        ("jfr_deadlock_detection", "死锁环检测：monitor 持有/等待关系分析。args 可选：start_time/end_time。", mapping::JfrProxyKind::DeadlockDetection, QUERY),
        ("jfr_io_hotspots", "IO 热点：慢/高频文件与 socket 操作（按路径/主机），含调用点。args 可选：io_type/top_n/start_time/end_time。", mapping::JfrProxyKind::IoHotspots, QUERY),
        ("jfr_exceptions", "异常抛出统计：按异常类统计次数与栈。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::Exceptions, QUERY),
        ("jfr_errors", "严重错误分析：OutOfMemoryError/StackOverflowError 等按严重度分类。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::Errors, QUERY),
        ("jfr_safepoints", "safepoint 分析：GC 外 STW 暂停（vm operation 耗时），延迟毛刺定位。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::Safepoints, QUERY),
        ("jfr_virtual_threads", "虚拟线程分析：pinning 位点与执行失败（目标 JDK 21+）。args 可选：top_n/start_time/end_time。", mapping::JfrProxyKind::VirtualThreads, QUERY),
        ("jfr_stack_trace_search", "跨 13 类事件全栈正则搜索（非截断栈）。找人/找路径利器。args 必填：class_pattern；可选 event_type/limit/start_time/end_time。", mapping::JfrProxyKind::StackTraceSearch, HEAVY),
        ("jfr_correlate", "跨维度相关性引擎：锁↔IO↔热点方法关联成瓶颈链。args 可选：dimension/top_n/start_time/end_time。", mapping::JfrProxyKind::Correlate, HEAVY),
        ("jfr_request_waterfall", "线程时序瀑布：按时间顺序串联 锁→IO→CPU→异常 事件。args 必填：thread_name；可选 max_events/start_time/end_time。", mapping::JfrProxyKind::RequestWaterfall, HEAVY),
    ];
    for (name, desc, kind, timeouts) in proxies {
        registry.register(proxy_tool_def(
            name,
            desc,
            JfrToolKind::Proxy(*kind),
            *timeouts,
            &jmc,
            &artifacts_dir,
        ));
    }

    registry.register(proxy_tool_def(
        "jfr_compare",
        "两个 JFR 录制的 A/B 对比（优化前后、故障期 vs 正常期）：事件量/热点/暂停等维度差异汇总。",
        JfrToolKind::Compare,
        HEAVY,
        &jmc,
        &artifacts_dir,
    ));
}
```

说明：`json!` 宏的值位置支持任意 `Serialize` 表达式，schema 内的 `"description": format!(...)` 写法可直接编译，无需调整。

- [ ] **Step 5: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml tools::builtin::jfr`
Expected: 全部 PASS（mapping 8 + 契约层 10）。

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/tools/builtin/jfr/mod.rs
git commit -m "feat: jfr_record and 21 jfr analysis tools"
```

---
### Task 8: lib.rs 装配 —— JmcManager 入 AppState + 双钩子 + 注册工具组

**Files:**
- Modify: `src-tauri/src/lib.rs`（AppState 字段、setup 装配、工具注册）

- [ ] **Step 1: AppState 加字段（lib.rs 30-38 行区域）**

```rust
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub bus: EventBus,
    pub agents: Arc<Mutex<HashMap<String, agent::stream::RunningAgent>>>,
    pub filter_handle: reload::Handle<EnvFilter, Registry>,
    pub paths: Paths,
    pub embedding: Option<Arc<crate::knowledge::embedding::EmbeddingService>>,
    pub vec_store: Option<Arc<crate::knowledge::vec_store::VecStore>>,
    pub tool_registry: Arc<crate::tools::registry::ToolRegistry>,
    pub analyzer: Arc<crate::analyzer::HeapAnalyzerManager>,
    pub jmc: Arc<crate::jfr::JmcManager>,
    pub arthas: Arc<crate::arthas::manager::ArthasManager>,
    pub tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    pub exec_pool: Arc<Mutex<crate::exec::pool::ExecChannelPool>>,
    pub confirm_registry: Arc<Mutex<crate::tools::confirm::ConfirmRegistry>>,
    pub session_mapper: Arc<Mutex<crate::mcp::session_mapper::SessionMapper>>,
    pub mcp_server: Option<crate::mcp::transport::McpServerHandle>,
}
```

- [ ] **Step 2: setup 中装配 JmcManager（analyzer_manager 构造之后、`let exec_pool = ...` 之前插入）**

在 lib.rs 107 行（analyzer_manager 构造结束）之后插入：

```rust
            // JFR 飞行记录分析：vendored JMC 工人进程（resources/jmc JAR + 本机 Java 21+）
            let jmc_jar = resource_dir.as_ref().and_then(|r| {
                let candidates = [
                    r.join("resources").join("jmc").join(crate::jfr::JMC_JAR_NAME),
                    r.join("jmc").join(crate::jfr::JMC_JAR_NAME),
                ];
                candidates.into_iter().find(|p| p.exists())
            });
            if jmc_jar.is_none() {
                tracing::warn!(
                    "JMC JAR missing (resources/jmc/{}); jfr_* tools will report jmc_unavailable",
                    crate::jfr::JMC_JAR_NAME
                );
            }
            let jmc_manager = Arc::new(crate::jfr::JmcManager::new(
                crate::jfr::production_client_factory(jmc_jar),
                EventBus::new(handle.clone()),
                crate::jfr::JmcConfig::default(),
            ));
```

- [ ] **Step 3: 双钩子（lib.rs analyzer 钩子之后追加一行）**

Task 5 已把 analyzer 钩子调用改为 `add_download_complete_hook`；在其后追加：

```rust
            transfer_manager
                .add_download_complete_hook(crate::jfr::download_complete_hook(&jmc_manager));
```

- [ ] **Step 4: 注册工具组（heap::register_all 之后插入）**

```rust
            crate::tools::builtin::jfr::register_all(
                &mut tool_registry,
                jmc_manager.clone(),
                jvm_core.clone(),
                EventBus::new(handle.clone()),
                transfer_manager.clone(),
                paths.artifacts_dir(),
            );
```

注意依赖顺序：`jvm_core` 在 lib.rs 128-134 行构造（在 analyzer 之后）——`jfr::register_all` 需要 `jvm_core`，插入位置在 heap::register_all（198-202 行）之后即可满足（jvm_core 先于工具注册构造）。`jmc_manager` 需在 `transfer_manager` 之前构造（Step 2 已保证）。

- [ ] **Step 5: AppState 实例化处加字段（lib.rs 267-283 行 app.manage(AppState{...})）**

在 `analyzer: analyzer_manager,` 之后加一行：

```rust
                jmc: jmc_manager,
```

- [ ] **Step 6: 验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过。

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS（新增 jfr 测试 + 既有测试无回归）。

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat: wire JmcManager and jfr tools into app state"
```

---

### Task 9: agent/prompt.rs —— TOOL_GUIDANCE JFR 指引

**Files:**
- Modify: `src-tauri/src/agent/prompt.rs:30-39`（TOOL_GUIDANCE）
- Modify: `src-tauri/src/agent/prompt.rs`（tests 追加）

- [ ] **Step 1: 先写测试（追加到 prompt.rs `mod tests` 末尾）**

```rust
    #[test]
    fn test_tool_guidance_mentions_jfr_tools() {
        assert!(TOOL_GUIDANCE.contains("jfr_record"));
        assert!(TOOL_GUIDANCE.contains("jfr_quick_analysis"));
        assert!(TOOL_GUIDANCE.contains("jfr_compare"));
        assert!(TOOL_GUIDANCE.contains("arthas_profiler"), "JDK 8 fallback guidance required");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt`
Expected: `test_tool_guidance_mentions_jfr_tools` FAIL（TOOL_GUIDANCE 尚无 jfr 内容）。

- [ ] **Step 3: TOOL_GUIDANCE 追加 JFR 条目（在 arthas 条目之后、「用户提到的环境」条目之前插入一行）**

```rust
- JFR 飞行记录（低开销全维度观测）：性能类问题（CPU 飙高、慢请求、GC 异常、锁竞争）优先 jfr_record(environment, pid, duration_secs) 热开启录制（目标 JDK 11+，profile 档开销 1~3%）→ 轮询 transfer_status → completed 后自动预热，用 jfr_quick_analysis(local_path) 一键诊断 / jfr_rules(local_path) 规则引擎 → 按维度下钻：jfr_gc_detail / jfr_hot_methods / jfr_thread_cpu / jfr_thread_contention / jfr_io_hotspots / jfr_memory_leaks / jfr_safepoints / jfr_stack_trace_search / jfr_correlate / jfr_request_waterfall；两次录制对比用 jfr_compare(baseline_local_path, target_local_path)。目标 JDK 8 不支持 JFR 热开启，改用 arthas_profiler。
```

- [ ] **Step 4: 运行测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml agent::prompt`
Expected: 全部 PASS（含新测试）。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent/prompt.rs
git commit -m "feat: JFR workflow guidance in agent tool guidance"
```

---

### Task 10: 文档 + 全量验证

**Files:**
- Modify: `docs/architecture/overview.md:77-83`
- Modify: `AGENTS.md`（已实现功能）
- Modify: `docs/superpowers/specs/2026-09-03-jmc-jfr-analysis-design.md`（状态行）

- [ ] **Step 1: `docs/architecture/overview.md` 诊断工具层说明（77-83 行）**

把：

```
│ - 结构化封装（首批 JVM 工具已落地：
│   list_processes / jvm_gc_stats / jvm_thread_dump
│   / jvm_heap_info / jvm_vm_info / jvm_class_histogram
│   / jvm_heap_dump；堆快照分析 heap_* 系列（MAT 引擎，
│   自动预热）已落地；arthas 动态诊断 arthas_* 系列
│   （官方 MCP Server 对接，SSH 隧道代理）已落地；
│   读日志/读dump 后续批次）
```

改为：

```
│ - 结构化封装（首批 JVM 工具已落地：
│   list_processes / jvm_gc_stats / jvm_thread_dump
│   / jvm_heap_info / jvm_vm_info / jvm_class_histogram
│   / jvm_heap_dump；堆快照分析 heap_* 系列（MAT 引擎，
│   自动预热）已落地；JFR 飞行记录 jfr_* 系列（JMC 引擎，
│   远程录制→拉回→自动预热）已落地；arthas 动态诊断
│   arthas_* 系列（官方 MCP Server 对接，SSH 隧道代理）
│   已落地；读日志/读dump 后续批次）
```

- [ ] **Step 2: `AGENTS.md` 已实现功能加一条（「堆快照分析」条目之后）**

```markdown
- **JFR 飞行记录分析**：`jfr_record`（jcmd JFR.start 一次性定时录制 → 自动拉回 → `.jfr` 下载完成自动预热）+ 21 个 `jfr_*` 分析工具（JMC 内核：规则引擎/一键诊断/GC/热点方法/锁竞争/IO/异常/泄漏/相关性/A-B 对比）。Friday 作为 MCP client 托管 fork 自 jmc-mcp-server 的 JAR 工人进程（stdio，本机 Java 21+，JAR 由 `scripts/fetch-jmc-jar.ps1` 构建时获取、随安装包分发）；无会话层（上游自带缓存），空闲 15min 自动退出、传输错误 invalidate 懒重建；TransferManager 下载完成钩子按扩展名分发（.hprof → MAT 预热 / .jfr → JMC 预热）
```

同时把「诊断工具面板分组」条目中的「6 组 51 项」改为「7 组 73 项」。

- [ ] **Step 3: spec 状态行更新**

`docs/superpowers/specs/2026-09-03-jmc-jfr-analysis-design.md` 第 4 行状态改为：

```markdown
- 状态：已实施（计划见 [2026-09-03-jmc-jfr-analysis](../plans/2026-09-03-jmc-jfr-analysis.md)）
```

- [ ] **Step 4: 全量验证**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过。

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS，3 个 ignored（jfr client 1 + jfr manager 1 + analyzer 既有）。

Run: `pnpm typecheck`
Expected: 通过。

可选（若本机 Java 21 + fetch 脚本已跑 + fork Release 已就绪）：

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --ignored jfr`
Expected: 2 个集成测试 PASS（真实 JMC worker：握手 + jfr_overview + jfr_rules + verbatim 路径）。

- [ ] **Step 5: Commit**

```bash
git add docs/architecture/overview.md AGENTS.md docs/superpowers/specs/2026-09-03-jmc-jfr-analysis-design.md
git commit -m "docs: JFR analysis feature docs"
```

---

## 完成标准（对照 spec）

- [x 覆盖核对] spec §2 决策表 10 项全部落地：闭环链路（Task 7/8）、一次性录制（Task 6/7）、fork 分发（前置条件 + Task 1）、Java 21（Task 4 工厂）、无会话层（Task 4）、22 工具（Task 7）、全局唯一 worker + Xmx4g（Task 4）、空闲 15min/invalidate（Task 4）、.jfr 预热（Task 4/5/8）、ToolCategory::Jfr（Task 2）
- [x] spec §3 工具契约 22 个工具 + 超时分层 + async:false 强制（Task 6/7）
- [x] spec §6 错误码 8 个全部实现（Task 7：invalid_args/invalid_path/java_missing/jmc_unavailable/jmc_timeout/upstream 透传/record_failed/record_not_found）
- [x] spec §7 测试策略 1-7 全部有对应测试任务；#[ignore] 集成测试含 Java 21 闸门断言
- [x] spec §8 YAGNI 边界未被突破（无持续录制/无 call_tree/无 async 任务模式）
