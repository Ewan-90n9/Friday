# Friday JFR 飞行记录分析设计（jfr_* 工具 + JMC 工人进程 + 远程录制闭环）

- 日期：2026-09-03
- 状态：已评审（各节均与用户逐节确认）
- 上游：[JDK 原生命令结构化工具设计](2026-08-28-jdk-native-tools-design.md)（jcmd 执行模式）；[文件传输设计](2026-08-29-file-transfer-design.md)（拉回 + download_complete_hook）；[堆快照分析设计](2026-08-29-heap-analysis-design.md)（本地 Java 工人进程模式，本设计大量复用其形态）
- 外部依赖：[scarletbean01/jmc-mcp-server](https://github.com/scarletbean01/jmc-mcp-server)（MIT，JMC 9.1.1 核心库驱动的 JFR 分析 MCP server；fork 后构建分发，见 §2）

## 1. 背景与目标

现状：Friday 的 CPU 侧诊断有 arthas_profiler（async-profiler 火焰图），内存侧有 heap_*（MAT 深度分析），但缺少一个**低开销、全维度**的持续观测手段。JFR（Java Flight Recorder）是 JVM 内建的飞行记录器，一次录制同时覆盖 GC、内存分配、锁竞争、IO、异常、JIT、safepoint、线程生命周期等维度，profile 档开销约 1–3%，为生产环境热开启设计。目前 Friday 对 JFR 完全没有支持（代码库零相关代码）。

目标：`jfr_record` 一句话完成"远程热开启录制 → 落盘 → 自动拉回 → 本地预热"，`jfr_*` 分析工具族让 agent 自主完成从录制到根因结论的完整诊断闭环（含 JMC 规则引擎、专家诊断系、A/B 对比），用户不再需要手动开 JMC GUI。

### 调研结论（jmc-mcp-server）

- JMC 9.1.1 核心库驱动，**69 个 MCP 工具**覆盖 JFR 分析全流程（概览/规则引擎、GC 与内存、CPU 与代码、线程与锁、IO 与网络、异常、分配、系统趋势、smart_* 专家诊断系、虚拟线程等）；
- **stdio 传输**（本地子进程），与 Friday 现有 heap analyzer 同构；
- 要求 **Java 25+**（`maven.compiler.release=25`）；Quarkus + ClojureScript 技术栈较重（内嵌 Web Dashboard）；
- **无 Releases 产物**，只能源码构建（fat JAR `target/jmc-mcp-1.0.0-SNAPSHOT.jar`）；
- 上游自带无状态设计：所有工具接收 `jfr_file_path` 参数，内部有 TTL 录制缓存（1h 过期、文件变更检测、内存压力驱逐、软引用）；
- 重型工具支持 `async: true` 后台任务模式（get_job_status/get_job_result 轮询）；
- 单人维护、1 star 的早期项目（质量与维护风险由 fork 控版本对冲）。

### 目标 JVM 兼容性（热开启矩阵）

| 目标 JVM | 运行时 `jcmd JFR.start` | 说明 |
|---|---|---|
| OpenJDK/Oracle JDK 11+（含 17/21/25，各发行版） | ✅ 完全支持 | JFR 开源内置，零启动参数，随时开/停/dump，可与已运行录制共存 |
| Oracle JDK 8 | ⚠️ 受限 | 商业特性，默认要求启动参数 `-XX:+UnlockCommercialVMOption -XX:+FlightRecorder` |
| OpenJDK 8 | ❌ 不支持 | 无内置 JFR（个别厂商发行版除外） |

JDK 8 目标调用 `jfr_record` 走 `record_failed` 错误路径（jcmd 错误透传）；prompt 的 TOOL_GUIDANCE 引导 JDK 8 场景改用 `arthas_profiler`。

### 关键权衡（已评审定案）

| 备选 | 结论 |
|---|---|
| 仅本地分析（不做录制工具） | 否——agent 需多步手工编排（run_command 录制 + file_download），闭环断链 |
| 持续录制 + 按需 dump（jfr_start/jfr_dump/jfr_stop） | 否——需管理远程录制会话状态，工具多、出错面大，v1 用一次性定时录制覆盖主流场景 |
| agent 直连第二台 MCP server | 否——同 heap 分析定案（工具面膨胀、依赖裸露、工人进程无人管） |
| 自研最小 JMC server（JMC core + MCP SDK） | 否——69 工具的聚合分析逻辑（GC 分阶段、火焰图数据、相关性引擎）自研工作量是 fork 的数倍 |
| fork 并裁剪（剔除 Dashboard/async 队列） | 否——fork diff 变大失去同步上游能力，体积代价可接受 |
| 每个文件独立 worker 进程 | 否——Quarkus 启动秒级开销、进程数膨胀，与上游单 server+缓存设计相悖 |
| **定案：fork jmc-mcp-server（仅降 Java 21）+ CI 发布 JAR + Friday 无状态代理（方案 1）** | 下游完全沿用 heap analyzer 的 vendoring/生命周期模式；Friday 侧无会话层（上游自带缓存） |

## 2. 决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 链路范围 | 完整闭环：`jfr_record`（录制+落盘+拉回）→ `.jfr` 下载完成自动预热 → `jfr_*` 分析 |
| 2 | 录制模型 | 一次性定时录制：`jcmd JFR.start settings=<档> duration=Ns filename=...`，无远程会话状态管理 |
| 3 | 上游依赖 | fork 上游（我们 org 下），仅改 `maven.compiler.release` 25→21，GitHub Actions 构建 fat JAR 发 fork Releases；升级锁 fork tag、随 Friday 发版 |
| 4 | Java 依赖 | JMC worker 要求本机 Java ≥21（降级后与 heap analyzer 统一；降级失败回退 ≥25，见 §10 风险闸门）；复用 `analyzer::java::detect_java` |
| 5 | Friday 侧会话模型 | 无状态代理（方案 1）：无 open/close 工具、无 session 层；文件内存归上游缓存（TTL/驱逐），Friday 只管 worker 进程生命周期 |
| 6 | 工具面 | 精选 22 个（§3）：1 录制 + 21 分析；剔除 async 任务轮询（强制 `async:false`）、JMX 直连、内部工具、call_tree 交互树、冗余对 |
| 7 | worker 进程数 | 全局唯一，懒启动；`-Xmx4g` 常量起步（JFR 缓存为主要内存消费者，先简单后调优） |
| 8 | 回收 | 空闲 15min 自动退出（idle reaper 30s tick）；传输错误 invalidate + 懒重建；无会话关闭联动（无会话表） |
| 9 | 预热 | `.jfr` 下载完成后台调 `jfr_overview` 触发上游缓存加载，`provision_progress`（tool=`jfr_record`、stage=`analyze`），1800s 硬超时 |
| 10 | 工具分类 | 新 `ToolCategory::Jfr`，枚举序在 `Heap` 之后；前端第 7 组"JFR 分析" |

## 3. 工具契约

MCP 层自动加 `friday_` 前缀。分析对象是**本机** `.jfr` 文件，无 `environment` 参数；所有分析工具 `needs_channel: false`、RiskLevel::ReadOnly。

### 3.1 录制工具（jcmd 侧）

| 工具 | 关键参数 | 语义 | 风险 | 默认/上限超时 |
|---|---|---|---|---|
| `jfr_record` | `environment`、`pid`、`duration_secs`（10–600，默认 60）、`settings`（`profile`/`default`，默认 `profile`） | 一次性定时录制：`jcmd <pid> JFR.start name=friday-<ts> settings=<档> duration=Ns filename=/tmp/friday-tools/recording-<pid>-<ts>.jfr` → 轮询 `JFR.check` + stat 大小稳定判定落盘 → TransferState(Download) 后台拉回 `artifacts/<session>/recording-<pid>-<ts>.jfr`（成功清理远端）→ 返回 `{transfer_id, local_path}` | Low | 600s / 1800s |

录制等待（JFR.check 轮询 + 大小稳定判定）在工具调用超时预算内同步完成；duration 到期但文件未出现/未稳定 → `record_not_found`（附远端路径与已等待时长）。

### 3.2 分析工具（JMC 代理，21 个，全 ReadOnly）

公共 schema：`local_path`（必填）+ `args`（可选透传对象：`top_n` / `thread_name` / `package_prefix` / `focus` / `start_time` / `end_time` 等，工具描述说明各自可用项）+ `timeout_secs`。所有代理调用**强制注入 `async: false`**（禁用上游后台任务模式，靠 Friday 超时分层）。

| Friday 工具 | 上游工具 | 用途 | 默认/上限超时 |
|---|---|---|---|
| `jfr_overview` | `jfr_overview` | 录制摘要/事件数/JVM 信息（预热也用它） | 60s / 300s |
| `jfr_rules` | `jfr_rules` | JMC 规则引擎自动瓶颈检测 | 60s / 300s |
| `jfr_quick_analysis` | `smart_quick_analysis` | 一键宏诊断仪表盘（严重度分类+主瓶颈） | 300s / 1800s |
| `jfr_gc_detail` | `gc_detail` | GC 分阶段暂停/GC cause/堆趋势 | 60s / 300s |
| `jfr_memory_leaks` | `memory_leaks` | 老对象采样泄漏分析 | 300s / 1800s |
| `jfr_predictive_leak` | `smart_predictive_leak_analysis` | 线性回归数学检测泄漏 | 300s / 1800s |
| `jfr_allocation_hotspots` | `allocation_hotspots` | 分配热点（类+调用点） | 60s / 300s |
| `jfr_hot_methods` | `hot_methods` | CPU 热点方法 | 60s / 300s |
| `jfr_thread_cpu` | `thread_cpu` | 线程级 CPU | 60s / 300s |
| `jfr_cpu_flame` | `cpu_flame` | CPU 火焰图数据 | 300s / 1800s |
| `jfr_thread_contention` | `thread_contention` | 锁竞争/阻塞/等待 | 60s / 300s |
| `jfr_deadlock_detection` | `deadlock_detection` | 死锁环检测 | 60s / 300s |
| `jfr_io_hotspots` | `io_hotspots` | 慢/高频 IO（含调用点） | 60s / 300s |
| `jfr_exceptions` | `exception_analysis` | 异常抛出统计 | 60s / 300s |
| `jfr_errors` | `error_analysis` | OOM/SOError 等错误严重度分类 | 60s / 300s |
| `jfr_safepoints` | `safepoint_analysis` | GC 外 STW/safepoint 暂停 | 60s / 300s |
| `jfr_virtual_threads` | `virtual_threads` | 虚拟线程 pinning（JDK 21+） | 60s / 300s |
| `jfr_stack_trace_search` | `smart_stack_trace_search` | 跨 13 类事件栈正则搜索 | 300s / 1800s |
| `jfr_correlate` | `smart_correlate` | 锁↔IO↔热点方法相关性链 | 300s / 1800s |
| `jfr_request_waterfall` | `smart_request_waterfall` | 线程时序瀑布（锁→IO→CPU→异常） | 300s / 1800s |
| `jfr_compare` | `smart_compare_recordings` | 两个录制的 A/B 对比；参数为 `baseline_local_path` + `target_local_path`（其余 `args` 透传） | 300s / 1800s |

**明确剔除**：`live_recording`（JMX 直连，与远程诊断模式不符）、`get_job_status`/`get_job_result`（async 已禁用）、`health_check`（内部）、Web Dashboard（不启用 HTTP 模式）、`call_tree`/`expand_call_tree`/`diff_call_tree`/`expand_diff_call_tree`（交互式下钻树，多轮交互成本高，v2 按需加）、`gc_analysis`/`gc_cause`/`gc_recommendations` 等（`gc_detail` 覆盖）、其余各领域次级工具（`smart_diff_stack_traces`/`jdk_bug_reference`/`container_metrics` 等，后续批次按需加）。

## 4. 模块与代码组织

```
src-tauri/src/jfr/                  # 引擎层（管进程、管协议；无会话层）
├── mod.rs                          # 模块入口
├── client.rs                       # JmcClient trait（call_tool/shutdown，测试缝）
│                                   #   + McpJmcClient：TokioChildProcess spawn + stderr drain
│                                   #   + 60s 握手 + Windows \\?\ 前缀剥离
│                                   #   复用 analyzer::client::extract_text / analyzer::java 探测
└── manager.rs                      # JmcManager：懒启动 + 空闲回收 + invalidate + 预热
                                    #   ClientFactory 注入缝（对齐 analyzer 模式）

src-tauri/src/tools/builtin/jfr/    # 工具契约层（薄层）
├── mod.rs                          # JfrToolHandler（录制分支 + 代理分支）
│                                   #   render()：64KB 截断 + artifacts jfr-<uuid>.md
│                                   #   register_all()（category: ToolCategory::Jfr）
└── mapping.rs                      # 纯函数：jcmd 参数构造（duration/settings 白名单校验）
                                    #   + 代理工具名/参数映射 + async:false 注入
```

链路：

```
agent CLI ──HTTP──▶ Friday MCP server (ToolRegistry: jfr_* 工具)
                        │ handler（Friday 原生契约）
                        ├── jfr_record ──▶ jdk_cache/JFR.start ──▶ TransferManager 拉回
                        │                                                    │ .jfr 完成
                        ▼                                                    ▼
                  JmcManager ◀────────── download_complete_hook（扩展名分发）
                  （Rust 托管层）──stdio(MCP)──▶ jmc-mcp fork JAR
                                               （vendored, JVM 工人进程）
```

### 工件分发链

```
GitHub fork（我们 org）─ 仅改 compiler release 25→21 ─▶ CI（GitHub Actions）
  mvn clean package ─▶ fat JAR ─▶ fork Releases（tag 形如 v0.1.0-jfriday）
                                        │
scripts/fetch-jmc-jar.ps1（对齐 fetch-analyzer-jar.ps1：下载/幂等/.downloading 原子落盘）
  → src-tauri/resources/jmc/jmc-mcp-<ver>.jar
  → tauri.conf.json bundle.resources 加 "resources/jmc/*"
  → lib.rs 双候选 resource_dir 解析（resources/jmc/ 与 jmc/，dev/打包两布局）
```

启动时 JAR 缺失只 `tracing::warn!`（工具返回结构化错误，不阻断应用）。

### 联动改动

- `tools/category.rs`：`ToolCategory::Jfr` 加在 `Heap` 之后（枚举声明序 = 面板展示序，单一事实来源）；
- 前端：`ToolsPanel.tsx` `CATEGORY_META` 第 7 组（中文标签"JFR 分析"）+ `src/lib/types.ts` `ToolCategory` union；
- `transfer/mod.rs`：`download_complete_hook` 从单闭包泛化为列表（或扩展名分发的组合 hook）——`.hprof` → MAT 预热、`.jfr` → JMC 预热；transfer 模块对 jfr 模块无编译期依赖；
- `lib.rs`：resource 解析、`JmcManager` 入 `AppState`、注册 jfr 工具组、hook 装配；
- `agent/prompt.rs` TOOL_GUIDANCE：JFR 快速排查指引（性能类问题先 `jfr_record` → `jfr_quick_analysis`/`jfr_rules` → 按领域下钻；JDK 8 目标引导改用 `arthas_profiler`）；prompt 测试断言关键词；
- `docs/architecture/overview.md` 诊断工具层补 jfr_* 系列；
- `infra/paths.rs`：无新目录类别（录制文件直接进 `artifacts/<session>/`）；
- Cargo：无新增依赖（rmcp 已含 client + transport-child-process feature）。

## 5. 工人进程生命周期

**JmcManager（全局单例，无会话层）规则**：

1. **懒启动**：首次 `query()` 或预热触发 `ensure_client()` → Java 探测（≥21）→ spawn `java -Xmx4g -jar <vendored jar>`（stdio MCP，stderr 全量 drain 记录）→ 60s 握手 → 常驻。
2. **预热**：TransferManager `.jfr` 下载完成（扩展名判定 + Download + Completed）→ 回调 JmcManager → 推 `provision_progress`（tool=`jfr_record`、stage=`analyze`，前端复用现有渲染）→ 后台调 `jfr_overview`（1800s 硬超时）触发上游解析+建缓存；失败不 invalidate（下次 query 重试），不打断对话流。
3. **透传**：`query(local_path, upstream_name, upstream_args, timeout)` → 注入 `jfr_file_path` + `async:false` → 上游调用；inflight 计数。
4. **空闲退出**：无 inflight 且 15min 未用 → graceful shutdown（stdin 关 → 3s → kill）。上游缓存随进程退出释放；本地 `.jfr` 文件保留，重分析时懒重启重加载。
5. **崩溃自愈**：传输错误 → `invalidate()`（丢 client + 后台 best-effort shutdown）→ 下次调用懒重建（同 analyzer 模式）；超时不杀进程（仅该次调用失败）。
6. **无会话关闭联动**：Friday 会话关闭不动作（无会话表；artifacts 文件清理沿用现有会话清理策略，上游进程 15min idle 自然退出）。

## 6. 错误处理

全走 handler 侧 `ToolOutput.data.error`（对齐 heap_*/jvm_* 惯例）。传输错误（`Err`）与工具错误（`is_error`）严格区分，前者触发 invalidate。

| error code | 触发 | 处理语义 |
|---|---|---|
| `invalid_args` | duration 超范围 / settings 非白名单 / 缺 pid / 缺 local_path | 直接返回 |
| `invalid_path` | local_path 文件不存在 | 直接返回 |
| `java_missing` | 本机无 Java 或版本 < 21 | 引导安装 JDK 21+（复用 analyzer 映射，附探测版本） |
| `jmc_unavailable` | JAR 缺失 / spawn 失败 / 握手超时 | 提示运行 `scripts/fetch-jmc-jar.ps1`；附 stderr 摘要 |
| `jmc_timeout` | 上游工具调用超时（默认/上限截断） | 不杀工人进程；引导调大 timeout_secs 或缩小时间窗 |
| `upstream_error` | 上游返回 is_error（文件损坏/路径不存在等） | 原样透传上游文本 |
| `record_failed` | `JFR.start` 失败（JDK 8 不支持/权限） | 透传 jcmd stderr；JDK 8 场景引导 arthas_profiler |
| `record_not_found` | duration 到期后文件未出现/大小不稳定 | 附远端路径与已等待时长 |

日志（遵从[日志规范](../../architecture/logging-standard.md)）：manager 启动/退出/invalidate `info!`/`error!` 带完整 stderr；工具入口 `#[instrument]`（session_id/local_path/工具名）；JFR.start/JFR.check 的 jcmd 命令与输出全量记录。

## 7. 测试策略

1. **单元测试（mock client，`JmcClient` trait 注入）**：懒启动仅一次；invalidate 后懒重建；空闲回收时序；传输错误 invalidate；预热失败不阻断后续 query；超时不杀进程；
2. **mapping 纯函数**：jcmd 参数构造（duration 边界 10/600/越界、settings 白名单）、async:false 注入、compare 双路径映射、代理参数透传；
3. **录制链路**：mock SSH channel（run_command 测试模式）验证 JFR.start 命令形态、JFR.check 轮询与大小稳定判定、TransferState 构造（远端清理标志/本地路径）；`record_not_found` 路径；
4. **预热联动**：transfer completed 回调扩展名分发（.jfr 触发 JMC、.hprof 仍触发 MAT、其他不触发）；预热失败不影响 transfer 终态；
5. **集成测试 `#[ignore]`**（需本机 Java 21+ + fetch 脚本已跑）：测试内用 `jcmd JFR.start` 对自身 JVM 录制生成样例 `.jfr` → 真实 spawn → `jfr_overview` → `jfr_rules` → 传输错误 invalidate → 重建；同时充当 fork JAR 的 Java 21 兼容性验证；
6. **prompt**：TOOL_GUIDANCE 含 jfr_* 关键词与 JDK 8 兜底指引；
7. **前端**：`pnpm typecheck`；工具面板第 7 组渲染人工验证；`cargo check` / `cargo test`。

## 8. 明确不做（YAGNI）

- 持续录制会话管理（jfr_start/jfr_dump/jfr_stop）——后续批次按需加；
- 对已在运行的录制做抢救性 dump（`JFR.dump`）——后续批次按需加；
- call_tree / diff_call_tree 交互式下钻树系；
- 上游其余 40+ 次级工具代理（event_schema/search_events/time_series 等）——按需追加；
- 自研 JFR 聚合分析逻辑；
- Java 自动下载 / 内置专用 JRE；
- JMC worker 侧 Web Dashboard（不启用 HTTP 端口）；
- upstream async 任务模式（get_job_status/get_job_result）——强制同步 + Friday 超时分层；
- `-Xmx` 按文件大小动态计算（v1 常量 4g，观察后调优）。

## 9. 实施风险闸门

fork 降级 Java 21 是第一张多米诺，实施计划的**第一步**即：fork → 改 compiler release → CI 构建 JAR → 跑通最小集成测试（§7.5）。若降级失败（上游使用 25-only API），回退方案：JMC worker 单独要求 Java 25（`detect_java` 阈值参数化，每个 worker 各自最低版本），heap analyzer 维持 21，仅更新探测逻辑与文档，Friday 侧其余设计不变。
