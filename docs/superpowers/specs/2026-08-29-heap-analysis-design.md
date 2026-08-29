# Friday 堆快照分析设计（heap_* 工具 + MAT 工人进程）

- 日期：2026-08-29
- 状态：已评审（各节均与用户逐节确认）
- 上游：[JDK 原生命令结构化工具设计](2026-08-28-jdk-native-tools-design.md) §9 延后项"dump 自动分析（MAT 集成）"的落地；[文件传输设计](2026-08-29-file-transfer-design.md) 的衔接（dump 拉回 → 分析）
- 外部依赖：[Djaler/jvm-heap-dump-mcp](https://github.com/Djaler/jvm-heap-dump-mcp)（MIT，Eclipse MAT 驱动的 MCP server，JAR 分发）

## 1. 背景与目标

现状：`jvm_heap_dump` 完成"生成 → 校验 → 后台拉回 → 交付 local_path"，agent 只能告知用户路径、让其手动用 MAT 分析。诊断链路在最后一步断掉。

目标：把堆快照分析做成 Friday 原生 MCP 工具（`heap_*` 系列，与 `jvm_*` 同款形态），agent 能自主完成 leak suspects / 支配树 / GC root 路径级别的深度分析并给出根因结论；用户不再需要手动开 MAT。

### 调研结论（jvm-heap-dump-mcp）

- Eclipse MAT 驱动的 MCP server（stdio），17 个工具覆盖深度分析全流程（open / leak_suspects / histogram / dominator_tree / path_to_gc_roots / object_info / inbound+outbound references / threads / OQL 等）；
- 依赖 Java 21+；npx 分发（JAR ~28MB 后台下载）；
- 内存需求与 dump 大小成正比，默认 `-Xmx4g`；
- 会话模型：单进程多会话（session ID 区分已打开的 dump）。

### 关键权衡（已评审定案）

| 备选 | 结论 |
|---|---|
| agent 直连第二台 MCP server（config 注入） | 否——工具面膨胀、依赖裸露、工人进程无人管 |
| 纯 Rust 复刻 MAT（支配树/retained/leak suspects） | 否——数月级工程量，v1 不现实 |
| 每次 call 一次性 MAT headless 进程 | 否——交互式下钻做不了 |
| Rust 基础分析兜底 + MAT 深度分析 | 否——MAT 已覆盖直方图且质量更高，砍掉避免双实现 |
| **定案：Friday 原生工具契约 + 底层托管 JVM 工人进程跑 MAT 内核（复用上游 JAR）** | agent 只感知 Friday 一台 server，heap_* 与 jvm_* 统一形态 |

## 2. 决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 集成形态 | 能力长在 Friday 自己的 MCP server 上：ToolRegistry 注册 `heap_*` 原生工具，handler 内部经 rmcp client（stdio）驱动工人进程 |
| 2 | 工人进程实现 | 复用 jvm-heap-dump-mcp 现成 JAR，不写/不维护 Java 代码；升级锁上游 release、随 Friday 发版 |
| 3 | JAR 获取 | vendoring 进安装包（Tauri resources），零下载、离线可用；内网环境不依赖 npmjs/GitHub |
| 4 | Java 依赖 | 启动前探测 PATH/JAVA_HOME 并校验 ≥21；缺失返回结构化错误 `java_missing` 引导安装，不自动下载 |
| 5 | 工具面 | 精选 9 个（§3），全 ReadOnly 直通；OQL/字符串搜索/数组查看留后续批次 |
| 6 | dump 会话主键 | 以 `local_path` 为主键（agent 不感知 MAT session ID），Friday 侧维护映射 |
| 7 | open 时机 | 拉回 completed 自动预热（后台 open + 建索引）；agent 调 heap_open 命中则秒回 |
| 8 | 工人进程数 | 全局唯一，多会话共享；`-Xmx` 启动时按待 open dump 计算：`clamp(dump_size × 1.5, 4G, 12G)`（向上取整 GB），进程存活期间固定 |
| 9 | 回收 | 空闲 15min 自动退出；Friday 会话关闭联动 close 其 dump 会话；同时 open 上限 3（LRU 逐出） |
| 10 | 索引复用 | MAT 索引文件落盘在 hprof 旁（artifacts 目录内）；进程重启/崩溃后重新 open 命中索引秒级加载 |

## 3. 工具契约

MCP 层自动加 `friday_` 前缀。分析对象是**本机** dump 文件，无 `environment` 参数；所有工具 `needs_channel: false`，统一传 `local_path`（transfer completed 后 agent 拿到的本地路径）。

| 工具 | 关键参数 | 语义 | 默认/上限超时 |
|---|---|---|---|
| `heap_open` | `local_path` | 打开 dump：MAT 建索引（分钟级）+ 返回 heap 总览（大小/对象数/类数/GC root 数）；命中预热则秒回 | 600s / 1800s |
| `heap_leak_suspects` | `local_path` | MAT 自动泄漏嫌疑报告（嫌疑点描述 + retained 比例） | 60s / 300s |
| `heap_histogram` | `local_path`；可选 `top`（默认 30）、`group_by_classloader`（bool） | 类直方图（实例数/shallow/retained） | 60s / 300s |
| `heap_dominator_tree` | `local_path`；可选 `parent_object_id`（下钻 children）、`top` | 支配树 Top；传 object_id 进入子树 | 60s / 300s |
| `heap_object_info` | `local_path`、`object_id` | 对象详情：类/shallow/retained/字段值 | 60s / 300s |
| `heap_path_to_gc_roots` | `local_path`、`object_id` | 最短 GC root 路径链 | 60s / 300s |
| `heap_references` | `local_path`、`object_id`；`direction` 枚举 `outbound`/`inbound` | 双向引用列表（下钻） | 60s / 300s |
| `heap_threads` | `local_path` | 线程列表 + 栈帧 + retained heap | 60s / 300s |
| `heap_close` | `local_path` | 关闭 dump 会话，释放工人进程内存 | 30s / 60s |

设计要点：

- 全 ReadOnly：分析动作无副作用，风险分级零确认直通（架构约定 9）；真正的风险（生成 dump 的 STW）已在 `jvm_heap_dump` High 级拦截；
- 输出：结构化 JSON + 完整结果落盘 session artifacts（复用 run_command 的截断 + 落盘机制，直方图/支配树可能数百行）；
- `object_id` 为上游 MAT 对象标识的透传字符串，Friday 不解析其内部结构。

## 4. 模块与代码组织

```
src-tauri/src/analyzer/            # 引擎层（管进程、管协议、管会话映射）
├── mod.rs                         # 模块入口
├── manager.rs                     # HeapAnalyzerManager：进程生命周期 + open 去重 + 预热状态机 + LRU
├── client.rs                      # HeapAnalyzerClient trait + rmcp client 实现（stdio 子进程握手/调用）
├── java.rs                        # Java 探测（PATH/JAVA_HOME → 版本解析 ≥21，结果缓存）
└── session.rs                     # local_path → analyzer 会话映射；Friday 会话关闭联动

src-tauri/src/tools/builtin/heap/  # 工具契约层（薄层，对齐 jvm/ 的组织方式）
├── mod.rs                         # 注册 9 个工具
└── ...                            # 每工具薄定义（schema/描述/错误码映射，共用 manager）
```

链路：

```
agent CLI ──HTTP──▶ Friday MCP server (ToolRegistry: heap_* 工具)
                       │ handler（Friday 原生契约）
                       ▼
                 HeapAnalyzerManager ──stdio(MCP)──▶ jvm-heap-dump-mcp JAR
                 （Rust 托管层）                      （vendored, JVM 工人进程）
                       ▲
                 TransferManager（heap dump 拉回 completed → 触发自动预热）
```

依赖与打包变更：

- `Cargo.toml`：rmcp 加 `client` + `transport-child-process` feature（同 crate，无新依赖）；
- `tauri.conf.json` resources 增加 JAR（`resources/analyzer/jvm-heap-dump-mcp-<version>-all.jar`），启动时 resolve 出绝对路径注入 manager；
- 前端零改动（工具卡片复用现有渲染，分析进度复用 provision_progress 事件）。

## 5. 工人进程生命周期

**HeapAnalyzerManager（全局单例）规则**：

1. **懒启动**：首个 heap_* 调用或预热触发时启动工人进程（`java -Xmx<n> -jar <vendored jar>`，stdio MCP），握手后常驻。启动前先跑 Java 探测；`java_missing` 直接短路。
2. **内存预算**：`-Xmx = clamp(dump_size × 1.5, 4G, 12G)`，向上取整到 GB，进程启动时按待 open 的 dump 计算并固定。存活期间 open 更大 dump 导致 MAT OOM 时按上游业务错误透传（agent 引导 heap_close 释放后，空闲退出再按新预算重启）。
3. **自动预热**：TransferManager `finish()` 判定 heap dump 下载完成（`local_path` 以 `.hprof` 结尾 + Direction::Download + Status::Completed）→ 回调 manager → 推 `provision_progress` 事件（tool=`jvm_heap_dump`、stage=`analyze`，前端工具卡片复用现有渲染）→ 后台 open。预热状态机 per local_path：`warming / ready / failed`；失败不打断对话流，状态留在 manager 等 heap_open 透传。
4. **open 去重合流**：并发的 heap_open 同一路径只向上游发一次 open，其余 await 同一结果；预热 warming 中 agent 又调 heap_open 同样合流。
5. **会话上限与 LRU**：同时 open 上限 3 个 dump；超限时 close 最久未访问的（返回值注明被逐出，索引保留可重开）。
6. **空闲退出**：无打开会话且无进行中调用持续 15min → 工人进程退出释放内存。
7. **会话关闭联动**：Friday 会话关闭 → close 该会话 artifacts 目录下的 dump 会话（索引保留）。
8. **崩溃自愈**：工人进程异常退出 → 记 `error!`（完整 stderr）→ 已 open 会话标记失效 → 下次调用自动重启进程，引导重新 open（命中落盘索引，秒级恢复）。

## 6. 错误处理

全走 handler 侧 `ToolOutput.data.error`（对齐 jvm_* 惯例）：

| error code | 触发 | 处理语义 |
|---|---|---|
| `invalid_params` | local_path 缺失/文件不存在/object_id 非法 | 直接返回 |
| `java_missing` | 探测不到 Java 或版本 < 21 | 引导 agent 告知用户装 JDK 21+ 后重试；错误信息附探测到的版本号（若有） |
| `analyzer_unavailable` | 工人进程启动/握手失败/崩溃 | 附 stderr 摘要，agent 可重试；连续失败引导查 Friday 日志 |
| `dump_not_open` | 查询类工具但该 local_path 未 open | 引导先调 heap_open（或等预热完成） |
| `analyzer_timeout` | 工具调用超时 | **不杀工人进程**（区别于 SSH 工具超时断连语义），仅该次调用失败 |
| 上游业务错误 | MAT 报错（损坏 hprof、OOM 等） | 原样透传 stdout/stderr/exit_code，agent 自行决策（架构约定"业务错误返回 agent 决策"） |

日志（遵从 [日志规范](../../architecture/logging-standard.md)）：manager 启动/退出/崩溃 `info!`/`error!` 带完整 stderr；每次工具调用入口 `#[instrument]`（session_id/local_path/工具名）；Java 探测结果记 `info!`。

## 7. 联动改动

- **TransferManager**：`finish()` 增加 heap dump 完成回调注入（构造注入 `Option<Arc<dyn Fn(...)>>` 或等价机制，保持 transfer 模块对 analyzer 无编译期依赖）；
- **系统提示词**（`agent/prompt.rs` TOOL_GUIDANCE）：JVM 诊断流程尾部追加"heap dump 拉回后用 heap_leak_suspects / heap_dominator_tree / heap_path_to_gc_roots 自主分析根因，无需让用户手动开 MAT"；`jvm_heap_dump` 工具描述同步更新（从"告知用户用 MAT 分析"改为"拉回后可直接用 heap_* 工具分析"）；
- **文档**：`docs/architecture/overview.md` 诊断工具层补 heap_* 系列；umbrella 设计延后项表标记完成。

## 8. 测试策略

全单测，沿用 Mock 注入模式（`HeapAnalyzerClient` trait + mock 实现）：

- `client.rs`：正常返回 / 上游错误透传 / 超时 / 握手失败；
- `manager.rs`：懒启动仅一次；open 去重合流（并发同路径只发一次）；LRU 逐出；空闲退出计时；崩溃自动重启 + 会话失效；`-Xmx` 计算矩阵（dump 大小 → 预算取整）；
- `java.rs`：各发行版版本字符串解析（Temurin/BiSheng/OpenJDK 格式）、PATH 缺失、JAVA_HOME 优先级；
- 工具 handler：参数校验、`dump_not_open` 引导、错误码映射、落盘与截断；
- 预热联动：transfer completed 回调触发 open（mock client 验证调用序列）、非 .hprof 下载不触发、预热失败不影响 transfer 终态；
- prompt：TOOL_GUIDANCE 包含 heap_* 关键词。

## 9. 明确不做（YAGNI）

- OQL / find_strings / inspect_array / list_sessions 工具——后续批次按需加；
- Rust 自研 hprof 解析器（无 Java 兜底分析）；
- Java 自动下载；
- JAR 源码定制/fork（锁上游 release 版本，vendoring 升级走 Friday 发版）；
- 分析结果持久化（MAT 索引文件天然落盘，报告即用即弃）；
- 工人进程多实例/并行分析（全局唯一，串行会话管理）。
