# Friday JDK 原生命令结构化工具设计

- 日期：2026-08-28
- 状态：已评审（各节均与用户逐节确认）
- 上游：[知识库与工具库伞形总纲设计](2026-08-26-knowledge-tool-umbrella-design.md) §9 延后项"结构化 JVM 工具批次"的落地
- 前置依赖：[工具系统框架](2026-08-23-tool-system-design.md)、ensure_tool JDK 投放（已实现）

## 1. 背景与目标

现状：agent 做 JVM 诊断的路径是 `ensure_tool` 装备 JDK（投放 BiSheng JDK 到远端 `/tmp/friday-tools/`）→ 拿返回的全路径 → 每次 `run_command` 执行 jstat/jcmd。问题：

- run_command 是 High 级工具，每次执行都要用户确认——高频只读诊断动作（看 GC、抓线程转储）被确认摩擦拖慢；
- 裸 shell 透传没有语义，agent 要自己拼路径和参数，容易出错；
- 伞形总纲预留的"结构化封装（jstat/jcmd…）→ 结构化输出"批次尚未落地。

目标：把常用 JDK 原生命令封装为**语义动作级**结构化工具（与 run_command/ensure_tool 同一 ToolRegistry），agent 按诊断意图直接调用，只读动作零确认直通。

## 2. 决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 抽象粒度 | 语义动作级（jvm_gc_stats 等），不是裸命令透传 |
| 2 | 首批范围 | 核心只读集 + 重型工具，共 7 个 |
| 3 | JDK 路径来源 | 依赖显式 `ensure_tool`；投放路径按环境缓存在进程内，jvm_* 工具自动取用 |
| 4 | 路径缓存失效 | 不做执行前预检；执行遇 exit 127 / "No such file or directory" 即清缓存并引导重新 ensure_tool |
| 5 | 输出形态 | 原样透传 stdout/stderr + 元信息（命令原文/exit_code/elapsed），不解析表格为 JSON |
| 6 | 风险分级 | 5 工具 ReadOnly 直通；class_histogram Low（默认触发 full GC）；heap_dump High |
| 7 | heap dump 文件 | 远端生成后自动 SFTP 拉回本地 session artifacts，成功后删远端文件 |
| 8 | 代码组织 | 共享执行内核（JvmExecCore）+ 每工具薄定义；find/heap_dump 特例独立 handler |
| 9 | 注入防护 | pid 强制正整数；heap dump 不开放自定义文件名（Friday 固定命名） |

## 3. 工具清单与 Schema

MCP 层自动加 `friday_` 前缀（同现有 echo → friday_echo）。所有工具参数统一含 `environment`（环境名，list_environments 返回的 name）。

| 工具 | 参数 | 远端命令 | 风险级 |
|---|---|---|---|
| `list_java_processes` | 无业务参数 | `ps -eo pid=,user=,args=`，Rust 侧过滤含 `java` 的行（agent 从完整命令行识别目标服务） | ReadOnly |
| `jvm_gc_stats` | `pid`（正整数）；可选 `interval_ms`、`count`（连续采样） | `<jdk>/bin/jstat -gcutil <pid> [interval_ms count]` | ReadOnly |
| `jvm_thread_dump` | `pid` | `<jdk>/bin/jcmd <pid> Thread.print -l` | ReadOnly |
| `jvm_heap_info` | `pid` | `<jdk>/bin/jcmd <pid> GC.heap_info` | ReadOnly |
| `jvm_vm_info` | `pid`；`info_type` 枚举 `version`/`uptime`/`command_line`/`flags`/`system_properties`，默认 `command_line` | `<jdk>/bin/jcmd <pid> VM.<info_type>` | ReadOnly |
| `jvm_class_histogram` | `pid`；可选 `all`（bool，默认 false；false 为 live 视图会触发一次 full GC，true 加 `-all` 含死对象不强制 GC） | `<jdk>/bin/jcmd <pid> GC.class_histogram [-all]` | Low |
| `jvm_heap_dump` | `pid`；文件名不开放（固定 `/tmp/friday-tools/heapdump-<pid>-<时间戳>.hprof`） | 三阶段流程见 §5 | High |

所有 jvm 工具 `needs_channel: false`——对齐 run_command/ensure_tool 惯例：handler 内经共享 `resolve_environment` 函数按 `environment` 参数自行获取 channel（错误走 handler 侧结构化 error code，而非 MCP server 层的 CallToolResult::error 路径）。

设计要点：

- `list_java_processes` 是诊断流程第一步（找 pid），不依赖 JDK 投放；
- `jstat` 固定 `-gcutil`（占用百分比视图，最高频）；其他视图（-gc/-gccapacity 等）走 run_command 兜底；
- `pid` 拼入 shell 命令，强制正整数校验消除注入面；
- 工具命名对齐 `list_environments` 风格；
- 堆外内存、arthas 等后续批次不在本设计范围。

## 4. 代码组织与共享执行内核

```
src-tauri/src/tools/builtin/jvm/
├── mod.rs          # 模块入口 + 工具注册函数
├── core.rs         # JvmExecCore：共享执行内核
├── simple.rs       # 5 个标准工具（gc_stats/thread_dump/heap_info/vm_info/class_histogram）
│                   #   各自只有「命令构造 + schema/描述」，共用 core 的通用执行路径
├── processes.rs    # list_java_processes 专属 handler
└── heap_dump.rs    # jvm_heap_dump 专属 handler（三阶段）
```

**JvmExecCore 职责**：

1. 环境名 → env 记录：把 run_command/ensure_tool 中重复的环境查找逻辑提取为共享函数（`resolve_environment`），三处共用；
2. 从 ExecChannelPool 获取 channel；
3. 查 JdkCache 拿 JDK 布局，拼 `<全路径> <参数>` 命令；
4. 超时包裹执行（超时矩阵见 §6）；
5. 输出组装：复用 run_command 的截断 + 落盘机制——64KB 头部截断、完整输出写 session artifacts 目录（session_id 为 UUID 的路径校验沿用）、截断注记完整输出路径。

**JdkCache**（新 struct，lib.rs 创建，注入 ensure_tool handler 与 JvmExecCore）：

- 形态：进程内 `Mutex<HashMap<env_id, JdkLayout>>`；`JdkLayout { tool_home: String, bins: HashMap<String, String> }`，字段对齐 `ProvisionResult`（ensure_tool 的返回结构）；
- 写入：`ensure_tool` 成功时写入（无论 cached:true 探测命中还是新投放）；
- 失效：不做执行前预检。执行时 exit 127 或 stderr 含 "No such file or directory"（远端 /tmp 被清理）→ 清除该环境缓存，返回 `jdk_missing_on_remote` 引导重新 `ensure_tool`（幂等，探测命中即恢复，不重复下载）；
- 不持久化：Friday 重启后缓存为空 → jvm_* 报 `jdk_not_provisioned` → agent 调 ensure_tool 恢复；
- `ensure_tool` 工具描述同步更新：装备后即可直接调用 jvm_* 工具，无需再向 run_command 传全路径。

## 5. heap dump 三阶段流程

单次工具调用内顺序执行：

```
① 生成   <jdk>/bin/jcmd <pid> GC.heap_dump /tmp/friday-tools/heapdump-<pid>-<ts>.hprof
② 校验   远端 stat：文件存在且大小 > 0（jcmd exit 非 0 直接透传，进 dump_failed）
③ 拉回   SFTP download → 本地 session artifacts 目录；成功后 rm -f 远端文件
```

支撑改动：

- `ExecChannel` trait 新增 `download(remote_path: &str, local: &Path)`，默认返回未实现错误（对齐现有 `upload` 的模式）；`SshTransport` 以 russh-sftp 实现下载（镜像现有 upload 实现）；
- 传输进度：复用 `provision_progress` 事件机制推送（已传字节/总大小），前端工具卡片已有消费逻辑；
- 远端清理：下载**成功后**删除远端 dump（删的是 Friday 自己构造路径的文件，/tmp 空间宝贵）；下载失败保留远端文件并在返回中注明远端路径；
- 返回：本地路径 + 文件大小 + 分阶段耗时（生成/下载分开计时）+ 远端是否已清理；
- agent 不解析 `.hprof`（MAT 级分析超出范围）；工具描述写明价值是把标准 dump 文件交付给用户，引导 agent 告知用户本地路径。

## 6. 错误处理与超时

错误分类（复用现有 code 体系 + 新增）：

| error code | 触发 | 处理语义 |
|---|---|---|
| `invalid_params` | 缺参数 / pid 非正整数 | 直接返回 |
| `environment_not_found` | 环境名不存在 | 引导 `list_environments`（沿用现有文案） |
| `jdk_not_provisioned` | JdkCache 无该环境记录 | 引导先调 `ensure_tool` |
| `jdk_missing_on_remote` | exit 127 / stderr 含 "No such file or directory" | 清缓存，引导重新 `ensure_tool` |
| `connection_error` | 连接失败（Friday 自动重试 2 次后仍失败） | 沿用现有语义 |
| `timeout_error` | 超时；断开连接终止远端进程 | 沿用 run_command 语义 |
| `dump_failed` / `download_failed` | heap dump 专属 | dump_failed 透传 jcmd stderr；download_failed 注明远端文件保留的路径 |
| 业务错误（进程不存在/attach 失败/权限不足等） | jcmd/jstat exit 非 0 且非 127 场景 | **原样透传 stdout/stderr/exit_code，agent 自行决策**（符合"业务错误返回 agent 决策"的架构约定） |

超时矩阵（所有工具开放可选 `timeout_secs`，钳制逻辑复用 run_command 的 clamp 模式）：

| 工具 | 默认 | 上限 |
|---|---|---|
| list_java_processes | 30s | 120s |
| jvm_gc_stats | 30s | 300s |
| jvm_thread_dump / jvm_heap_info / jvm_vm_info | 60s | 300s |
| jvm_class_histogram | 120s | 600s |
| jvm_heap_dump 生成阶段 | 300s | 600s |
| jvm_heap_dump 下载阶段 | 1800s | 3600s |

注：`jvm_gc_stats` 带 interval/count 采样时耗时 = 间隔×次数，agent 需自行调大 timeout_secs。

日志（遵从 [日志规范](../../architecture/logging-standard.md)）：每个 handler 入口 `#[instrument]`（session_id/env/pid/命令原文），成功 `info!` 带 elapsed_ms，错误路径 `warn!`/`error!`，远端 stderr 完整记录不截断。

## 7. 联动改动

- **系统提示词**（`agent/prompt.rs` TOOL_GUIDANCE）：现有"ensure_tool 装备后用 bins 全路径走 run_command"改为"JVM 诊断流程：`list_environments` → `list_java_processes` 找 pid → `ensure_tool` 装备 JDK → 直接调 `jvm_*` 结构化工具"；run_command 重新定位为纯兜底（非 JVM 领域命令、jstat 其他视图等长尾）。现有断言 `test_tool_guidance_mentions_ensure_tool` 等同步更新。
- **文档**：`docs/architecture/overview.md` 诊断工具层"结构化封装（jstat/jcmd/arthas/读日志/读dump，后续批次）"更新为首批已落地并列表；umbrella 设计 §9 延后项表标记本批次完成。
- **ensure_tool 描述**：返回值语义从"给 run_command 传全路径"改为"装备后直接调 jvm_* 工具"。

## 8. 测试策略

全单测，沿用现有 MockChannel 注入 ExecChannelPool 的模式：

- 每 handler：正常路径 / 业务错误透传 / 超时断连（连接从池中移除）/ 缓存 miss 引导文案；
- JdkCache：写入、127 检测清除、miss 报错；
- download：trait 默认未实现 + RecordingChannel 式 mock 验证 heap_dump 三阶段编排（生成→校验→下载→远端清理→失败保留）；
- 注入防护：pid 非数字/负数/带字符一律 `invalid_params`；
- prompt：TOOL_GUIDANCE 包含新流程关键词（list_java_processes / jvm_ 前缀 / ensure_tool）。

## 9. 明确不做（YAGNI）

- jstat 其他视图、jinfo、NMT、jmap dump 旧语法——run_command 兜底；
- dump 自动分析（MAT 集成）；
- JdkCache 持久化 / 远端目录自动探测；
- playbook 联动（阶段 2 未开工，无内置 playbook 需更新引用）；
- arthas、读日志、读 dump 等其他结构化工具批次。
