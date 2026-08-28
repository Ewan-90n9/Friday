# Friday 知识库与工具库伞形总纲设计

- 日期：2026-08-26
- 状态：已评审（设计各节均已与用户逐节确认）
- 定位：伞形总纲（umbrella）。定义知识库与工具库两个子系统的目标、结构、相互契约和演进路线；具体实现拆分为四个子 spec，逐个走 spec → plan → 实现。

## 1. 背景与定位

Friday 的两个产品卖点：

1. **知识库**：用确定可用的知识确保 agent 别乱试——诊断开始就把对症的、被验证过的排查路径交给 agent。
2. **工具库**：封装常见的工具，避免 agent 想要获取信息的时候乱试——结构化工具让 agent 知道"有什么、怎么用、结果怎么读"。

两者都采用**纯引导**：不设强制拦截，靠注入质量与工具封装质量取胜。agent 可以不按 playbook 走、可以直接调任意工具——引导失效的代价是低效，不是失败。

现有基础（本设计的起点）：

- 工具系统框架已实现：MCP Server（rmcp + Streamable HTTP）、ToolRegistry、风险分级拦截、session 路由（见 [工具系统框架设计](2026-08-23-tool-system-design.md)）。仅有 `echo` 测试工具。
- 经验库已实现：诊断完成自动提炼经验、向量化（fastembed bge-small-zh-v1.5 + sqlite-vec）、新诊断语义注入（见 [记忆系统设计](2026-08-22-memory-system-design.md)）。
- Playbook 仅有 stub（17 行代码 + 6 行文档），本设计将其重定义并扩展为完整知识库。

## 2. 核心决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 总体结构 | 双系统各成体系：工具库=能力域，知识库=内容域，playbook 引用工具名为唯一契约 |
| 2 | 强制程度 | 纯引导，无强制拦截（沿用"playbook 是辅助不是强制"） |
| 3 | 工具暴露 | 全量直接暴露给 agent（MCP list_tools 全量返回），不经 playbook 中转 |
| 4 | 知识规范形态 | 所有来源收敛为结构化 playbook（三层：主干 steps + 旁注 + 原文附件） |
| 5 | 信任机制 | 内置 playbook 直接生效；增量（导入提炼/经验沉淀）一律人工审核后生效 |
| 6 | 经验库定位 | 培育池：经验继续自动注入；同一模式 occurrence_count ≥ 3 沉淀为 playbook 草稿；收敛后不再注入 |
| 7 | playbook 匹配 | 语义检索（复用 embedding + sqlite-vec），top-3、余弦相似度 ≥ 0.5 |
| 8 | 裸 shell | 不提供自由任意执行；提供 run_command 工具，风险级 High（每次执行需用户确认） |
| 9 | 自定义工具形态 | 脚本 + 清单（manifest），热插拔，agent 可自服务注册 |
| 10 | 首批内置工具 | 仅 run_command 一个；结构化 JVM 工具为后续批次 |
| 11 | 执行层通道 | 仅 SSH（russh）。K8s 场景也是 SSH 到目标环境执行 kubectl，不做直连 K8s API 的 transport |
| 12 | 爬虫 | 延后。摄入管线抽象为 trait 留扩展点 |
| 13 | playbook 存储 | SQLite（playbooks 表 + playbooks_vec 向量表）为运行时权威；YAML 仅作导入/导出交换格式 |

## 3. 概念模型与总架构

```
┌─────────────────────────────────────────────────────────────┐
│                        Agent (opencode / codeagentcli)        │
│                                                              │
│   自由调用任意工具（全量暴露）        参考 playbook 引导路径    │
│        │  direct                        ▲                    │
└────────┼───────────────────────────────┼────────────────────┘
         ▼ MCP                          │ get_playbook / 语义检索注入
┌────────────────────┐          ┌────────┴───────────┐
│   工具库（能力域）    │          │   知识库（内容域）    │
│                    │  引用     │                    │
│  ToolRegistry      │◄─────────│  PlaybookStore     │
│  ├ 内置工具 (Rust)  │ 契约：    │  ├ 内置 playbook    │
│  ├ 脚本工具(热插拔)  │ 工具名    │  ├ 导入提炼(审核)    │
│  └ run_command兜底  │          │  └ 经验沉淀(审核)    │
│  风险分级/确认拦截   │          │  审核状态机          │
└────────┬───────────┘          └────────┬───────────┘
         ▼                               ▲
┌────────────────────┐          ┌────────┴───────────┐
│  执行层 ExecChannel │          │  经验库（培育池，已有）│
│  SSH（唯一通道）     │          │  自动提炼·自动注入    │
└────────────────────┘          │  成熟模式→playbook草稿│
                                └────────────────────┘
```

核心概念的生命周期：

| 概念 | 定义 | 产生 | 生效方式 | 消亡 |
|---|---|---|---|---|
| **工具** | 一次诊断动作的封装（名字/参数 schema/风险级/执行体） | 内置随版本；用户/agent 写脚本+清单热插拔 | 注册即对 agent 全量暴露 | 卸载/禁用清单 |
| **Playbook** | 症状→工具调用序列+结果判读 的结构化知识 | 内置随版本；导入文档经 LLM 提炼；经验沉淀 | 审核 active 后才注入/可检索（内置免审） | 用户删除/禁用 |
| **经验** | 单次诊断的沉淀（已有系统） | 诊断完成自动提炼 | 自动注入（软提示） | 收敛进 playbook 后不再注入 |

"别乱试"的实现逻辑（纯引导三支柱）：

1. **工具侧**：结构化工具 + schema 描述让 agent 知道"有什么、怎么用"；run_command 受控兜底满足长尾。
2. **知识侧**：诊断开始时语义检索对症 playbook 注入 prompt，给 agent 一条被验证过的路径。
3. **反馈侧**：经验库持续记录什么有效什么无效，成熟模式升格为 playbook。

## 4. 工具库设计

### 4.1 分层结构

```
工具库
├── 内置工具（Rust handler，随版本发布）
│   ├── run_command      ← 首批唯一内置工具（受控兜底）
│   └── （后续批次：jvm_gc_stats、read_log 等结构化工具）
└── 脚本工具（用户/agent 创建，热插拔）
    └── <tools_dir>/
        ├── my-tool/
        │   ├── manifest.toml    # 名称、描述、参数schema、风险级、执行位置、脚本入口
        │   └── run.sh           # 实际执行体
```

### 4.2 关键机制

1. **ToolRegistry 动态化**：现有 `HashMap` + 不可变 `Arc` 改为 `RwLock`，支持运行时注册/注销。MCP `list_tools`/`call_tool` 走读锁，脚本工具装载走写锁。agent 写好新工具后无需重启即可接入。

2. **脚本工具清单（manifest.toml）**：
   - `name` / `description` / `input_schema`（JSON Schema，业务参数，session_id 仍由 MCP Server 自动注入）
   - `risk_level`（ReadOnly / Low / High——写进清单，加载时校验，非法值拒绝装载）
   - `exec_location`：`remote`（经 SSH 在目标环境执行）/ `local`（本机执行）
   - `script`：脚本路径 + 解释器（bash/python…），参数以 JSON 注入环境变量或 stdin

3. **run_command（首批核心工具）**：
   - schema：`{ command: string, timeout_secs?: number }`
   - 风险级别 **High**——每次执行走现有确认拦截（醒目警告 + 用户确认，120s 超时）
   - 输出：stdout/stderr 结构化返回，超时可控
   - agent 对它的偏好通过 prompt 引导："优先用结构化工具，run_command 是兜底"

4. **风险模型不变**：沿用现有三级拦截（ReadOnly 直通 / Low 简单确认 / High 强制确认）。

5. **内置工具与脚本工具同一注册表**：对 MCP 层完全透明，agent 看不出差别；playbook 引用两者一视同仁。

## 5. 知识库设计

### 5.1 Playbook 数据模型（三层结构：主干 + 旁注 + 原文附件）

```yaml
id: pb-netty-direct-memory-leak
title: Netty 堆外内存泄露排查
source: builtin | import | experience      # 来源，决定信任基线
source_url: "..."                          # import 来源时保留
applicability:                             # 适用条件
  language: java
  framework: netty
symptoms:                                  # 语义检索锚点（可多条）
  - "OutOfDirectMemoryError failed to allocate direct memory"
prerequisites: "服务基于 Netty，pid 已知"     # 前提，不满足先做什么
steps:                                     # 主干
  - tool: run_command                      # 引用工具名（契约点）
    when: "Always"                         # 分支条件（自然语言）
    args: { command: "jstat -gcutil <pid> 1000 5" }
    interpret: "Old 区不回落 → 堆内泄漏；回落 → 查直接内存"
anti_patterns:                             # 旁注1：踩坑/无效路径
  - "修复第一个可疑点后复发，必须回头继续查"
reference_notes: |                         # 旁注2：机制说明/指标判读表/方法论
  Netty 堆外内存由 PlatformDependent.DIRECT_MEMORY_COUNTER 计数...
notes: "..."                               # 旁注3：修复指导等
source_content: "..."                      # 原文附件（导入来源时保留全文）
status: draft | active | disabled          # 审核状态机
```

存储：SQLite `playbooks` 表（结构化字段 + source_content）+ `playbooks_vec`（sqlite-vec 虚拟表，向量化 symptoms 文本，与经验库同构）。YAML 仅作导入/导出交换格式——文件方式难做审核状态机和向量检索，运行时以 DB 为准（**修订**原 playbook.md 的"YAML 文件为准"）。

### 5.2 审核状态机

```
                 ┌──────────┐
  builtin ──────►│  active  │◄────── 用户审核通过
                 └────▲─────┘
                      │
  import/experience   │
  提炼生成 ──────► ┌───┴───┐    用户删除/禁用    ┌──────────┐
                  │ draft │ ─────────────────► │ disabled │
                  └───┬───┘ ◄───────────────── └──────────┘
                      │        用户重新启用
                      └─ 用户驳回 → 直接删除
```

- **draft**：不注入、语义检索不可见、get_playbook 不返回；待审核
- **active**：注入与检索的唯一来源
- **disabled**：保留但冻结，可重新启用
- 内置 playbook 随版本装载即 active；按 id 幂等升级，用户改过的内置副本跳过升级（不覆盖用户改动）

### 5.3 三条摄入管线

| 管线 | 触发 | 流程 |
|---|---|---|
| **内置装载** | 应用启动 | 打包的 playbook 写入/升级 DB（按 id 幂等） |
| **导入提炼** | 用户给 URL/markdown/docx | 抓取文本 → LLM 提炼成 playbook 结构 → **draft** → 审核 UI 确认 |
| **经验沉淀** | 经验库中同一模式（症状+语言+服务+根因四元组）`occurrence_count ≥ 3` | 合并生成 playbook 草稿 → **draft** → 审核；审核通过后对应经验标记 `converged`，不再注入 prompt |

agent 自动总结（用户点名的来源）走既有经验管线：诊断完成自动提炼 → 培育池 → 成熟升格，不直接进 playbook 库。

### 5.4 LLM 提炼映射规则（导入管线）

用真实文章验证过（美团《Netty堆外内存泄露排查盛宴》）。提炼 prompt 要求 LLM 按以下规则映射，而非有损压缩成模板：

| 原文内容 | 映射去向 | 原因 |
|---|---|---|
| 可远程执行的排查动作 | `steps`（暂以 run_command 承载命令模板，将来替换为结构化工具名） | agent 可走 |
| 依赖特定工具的动作（如 CAT 查指标） | 转译成通用判读写进 `interpret`（"监控平台堆外指标可能不准"） | 环境无关化 |
| 原文动作的现代等价物（改代码打日志监控 → arthas ognl 读字段） | 尽量映射到当前可用的工具化手段，不照抄原文动作 | 与时俱进 |
| 不可远程执行的开发动作（IDEA 单步 debug） | `reference_notes`/`notes` 里的方法论 | 不是诊断步骤 |
| 错误假设与弯路（修了 log4j2 后复发） | `anti_patterns` | "别乱试"的另一半 |
| 原文全文 | `source_content` 存档 | 审核对照 + 将来用更好 prompt 重新提炼 |

步骤中的 `<pid>`、`<日志路径>` 等为运行时参数占位，agent 调用时替换。

## 6. 注入与消费流

### 6.1 诊断开始时（首次 spawn，与经验注入同一时机）

```
用户消息
   ├─ 向量化（复用 AppState.embedding）
   ├─ 语义检索 playbooks_vec（仅 active）：top-3，余弦相似度 ≥ 0.5
   ├─ 同时检索经验库（现有逻辑：positive top-2 + negative top-1，已收敛的排除）
   └─ build_prompt 组装：
       system prompt（Friday 人格）
       ── 相关 Playbook ──           ← 新增 section，摘要形态：
       [id] 标题 + 适用条件 + 步骤概览   （一行一步，不含 interpret 细节）
       （提示 agent 需要细节时调 get_playbook(id)）
       ── 历史经验参考 ──             ← 现有 section（未收敛经验）
       ── 工具使用 ──                ← 现有 section（session_id）
       用户消息
```

### 6.2 消费路径

1. **被动注入**：摘要进 prompt，给 agent 指路。
2. **主动获取**：`get_playbook(id)` MCP 工具返回完整 playbook（steps/interpret/anti_patterns/reference_notes）。参数为 id（注入时已做语义匹配，agent 拿注入的 id 取全文）。工具不限首次 spawn，永远可用。

### 6.3 与经验注入的收敛关系

- 经验收敛进 playbook 后标记 `converged=1`，检索注入时排除——避免同一知识从两个 section 重复注入
- playbook 的信任级高于经验：prompt 中 playbook section 排在经验 section 前
- 后续 spawn 不重复注入（agent CLI 经 `--sessions` 已有首次注入的上下文），与现有经验注入逻辑一致

### 6.4 审核交互（伞形只定入口，细节归子 spec）

前端新增知识管理面板：draft 列表 → 审核（左右对照 source_content 原文与提炼结果）→ 通过 / 驳回（删除）/ 编辑后通过；playbook 列表的启停与删除。

## 7. 对现有系统的影响

| 现有模块/文档 | 变更 |
|---|---|
| `docs/architecture/overview.md` | 执行层改"SSH 单通道"（K8s 走 SSH+kubectl）；知识层章节扩充为本设计 |
| `docs/architecture/playbook.md` | 6 行薄文档重写为本设计的知识库章节 |
| `exec/k8s.rs` | 取消（不做 K8s API transport） |
| `tools/registry.rs` | HashMap → RwLock 动态化 |
| `knowledge/playbook.rs` | stub 重写为 PlaybookStore |
| `agent/prompt.rs` | build_prompt 新增 playbook section |
| `knowledge/memory.rs` | 检索排除 converged；沉淀检测逻辑 |
| `mcp/server.rs` | 注册 get_playbook 工具 |

## 8. 分阶段落地与子 spec 拆分

| 阶段 | 子 spec | 内容 | 交付价值 |
|---|---|---|---|
| **1** | SSH 通道 + run_command | russh 替换占位实现；run_command 工具（High 级确认）；SSH 凭证复用现有 credential 模块 | Friday 第一次真正能诊断远程环境——从演示品变产品 |
| **2** | Playbook 存储 + 注入 | playbooks 表 + 状态机 + 向量检索；get_playbook 工具；spawn 语义注入；首批内置 playbook（引用 run_command 的 Java 高频故障路径：OOM/CPU 飙高/GC 频繁/死锁等，清单在子 spec 定稿）；经验沉淀收敛 | 知识库卖点上线：开箱有可信路径，越用越准 |
| **3** | 导入管线 + 审核面板 | URL/markdown/docx 抓取 → LLM 提炼 → draft；审核 UI（原文/提炼对照）；playbook 管理列表 | 用户能把团队知识灌进来 |
| **4** | 脚本工具热插拔 | ToolRegistry RwLock 化；脚本+清单装载；agent 自服务注册 | 工具库开放性上线 |

依赖关系：2 依赖 1（内置 playbook 引用 run_command，引用校验才过）；3 依赖 2；4 依赖 1 但可与 2/3 并行。

## 9. 延后项（留扩展点，不实现）

| 项 | 扩展点设计 |
|---|---|
| 爬虫管线 | 摄入管线抽象为 trait（`KnowledgeIngest`：fetch → extract → draft），爬虫是第三个实现；前两条管线先以该 trait 落地 |
| 结构化 JVM 工具批次 | ✅ 已落地（见 [JDK 原生命令工具设计](2026-08-28-jdk-native-tools-design.md)）；playbook 步骤中的 run_command 命令模板逐步替换为结构化工具名，模型不用改 |
| K8s 场景 | 即"SSH 上跑 kubectl"的 playbook 内容，无新机制 |
| 经验时间衰减、playbook 使用效果反馈 | 现有演进项维持不变 |
