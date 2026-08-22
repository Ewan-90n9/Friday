# Friday 记忆系统 — 设计文档

- 日期：2026-08-22
- 状态：待实现
- 前置：[Agent 对话管道设计](2026-08-20-agent-conversation-design.md)（已实现）、[会话管理增强](2026-08-22-session-management-enhancement-design.md)（已实现）

## 1. 背景与目标

### 1.1 问题

Friday 当前只有会话级记忆——全量消息和工具调用持久化在 SQLite。存在两个效率问题：

1. **单会话过长**：诊断会话可能跑几十轮工具调用，全量加载和渲染慢。
2. **跨会话无关联**：新诊断不知道历史诊断发生了什么，相同的故障每次从零开始排查。

### 1.2 目标

让 Friday 越用越聪明。诊断完成后自动从会话数据中提取经验，向量化存入经验库。新诊断时语义检索相关经验注入 prompt，避免重复排查、快速指向已知根因方向。

### 1.3 术语

| 术语 | 含义 |
|------|------|
| **经验 (experience)** | Friday 自己诊断后提炼的知识，独立于 session |
| **案例 (case)** | 未来从互联网获取的他人诊断案例（不在本次范围） |
| **会话摘要 (session summary)** | 绑定 session 的摘要，用于前端展示 |

### 1.4 范围

- **会话摘要**：诊断完成时生成，存入 SQLite，供前端展示
- **经验库**：诊断完成时提取经验卡片，本地嵌入模型向量化，存入 sqlite-vec，新诊断时语义检索注入 prompt
- **全自动学习**：无需用户干预，无人工确认环节

### 1.5 不做（YAGNI）

- 不做中间摘要（长会话进行中的摘要）— 价值不足，YAGNI
- 不做 `recall_experiences` MCP 工具 — 前置依赖 MCP Server 基础设施，v1 只做 spawn 时自动注入
- 不做经验的时间衰减/过期清理 — 服务重构、JVM 升级非高频事件，后续用户反馈有问题再加
- 不做经验管理 UI（查看/删除/编辑）— 后续按需加
- 不做 `extraction_method` 字段 — 其降权用途被 outcome 过滤覆盖
- 不做 agent CLI auto-memory 的细粒度禁用 — 暂时并存，后续调研

## 2. 前提与约束

### 2.1 agent CLI 自管理对话历史

agent CLI（opencode / codeagentcli）通过 `--sessions <id>` 标志自管理单会话的完整对话历史。Friday **不重复做这件事**——不每次 spawn 时把最近 N 轮对话塞进 prompt。

### 2.2 agent CLI auto-memory 并存

codeagentcli 有自身的 auto-memory 功能，opencode 可能有类似机制。v1 不禁用，与 Friday 记忆系统并存。后续调研是否有细粒度标志（配置文件或环境变量）只禁用 auto-memory，不影响其他功能。

> 注意：codeagentcli 的 `--bare` 标志可以禁用 auto-memory，但同时禁用 hooks、LSP、plugin sync 等大量功能，副作用过大，不采用。且 `--bare` 仅对 codeagentcli 有效，opencode 无对应标志。

### 2.3 经验完全独立于 session

经验卡片生成后脱离 session 独立存在。`experiences` 表不引用 `session_id`，删除 session 不影响经验。经验卡片内容必须自洽完整——生成后无法回溯原始会话补全。

## 3. 两层记忆

### 3.1 第一层：会话摘要（Session Summary）

| 项 | 决策 |
|---|---|
| 职责 | 前端展示会话概要 |
| 生成时机 | 诊断完成时生成一次，无中间摘要 |
| 生成者 | `spawn_one_shot`，与经验卡片同一次 LLM 调用 |
| 输入 | 结构化压缩后的会话数据（详见 §4.2） |
| 存储 | SQLite `session_summaries` 表，`session_id` 外键 `ON DELETE CASCADE` |
| 失败处理 | 后台异步，不阻塞 `consume_stream`，失败记日志不重试 |

### 3.2 第二层：经验库（Experience Index）

| 项 | 决策 |
|---|---|
| 职责 | 跨会话语义检索相似诊断经验 |
| 卡片内容 | 症状、服务、语言、根因、排查路径（LLM 压缩自然语言）、经验提炼、outcome |
| outcome 判定 | LLM 判定 `positive` / `negative` / `uncertain`；AgentCrashed/AgentStopped 直接标 `negative` |
| 向量化文本 | 用户首条消息原文（查询和入库用同一文本，语义空间对齐） |
| 嵌入模型 | bge-small-zh-v1.5 via fastembed（ONNX Runtime），预编译进安装包 |
| 模型加载 | 应用启动时预加载到 `AppState` |
| 向量存储 | SQLite + sqlite-vec（官方 Rust 绑定，独立连接，不走 sqlx pool） |
| 检索 | 分层查询：`positive` top-2 + `negative` top-1；`uncertain` 不参与检索 |
| 相似度阈值 | 余弦相似度低于 0.5 不注入（默认值，可调） |
| 注入时机 | 仅会话首次 spawn 时注入，后续 spawn 不重复注入 |
| 注入位置 | system prompt 和用户消息之间，独立 section |
| 去重合并 | 正例按 症状+语言+服务+根因 四元组去重；反例按 症状+语言+服务 三元组去重 |
| 增量更新 | 匹配时追加新排查步骤/经验提炼，`occurrence_count` +1，`last_seen_at` 更新 |
| session 关联 | 无——经验完全独立 |
| 时间衰减 | v1 不做，纯 similarity 排序 |

## 4. 核心机制

### 4.1 统一收尾逻辑

`consume_stream`（`stream.rs`）当前有三条退出路径：

1. **`DiagnosisDone`**（`exit_ok=true`，line 421）— agent 正常退出
2. **`AgentCrashed`**（`exit_ok=false`，line 431）— agent 崩溃
3. **`AgentStopped`**（用户取消，line 392）— cancel 分支当前直接 return

**问题**：路径 3（cancel）自己做了 flush DB 和状态更新后直接 return，跳过了统一收尾逻辑。且反例恰恰来自路径 2 和 3，但原设计只在路径 1 触发摘要生成。

**调整**：三条路径统一到函数末尾收尾：

1. flush DB → 更新消息状态 → 从 agents map 移除 → emit 事件（立即完成）
2. `tokio::spawn` 后台 task 做摘要 + 经验生成（不阻塞主流程）
3. 后台 task 完成后只写 DB，不推 event；失败只记日志

具体改动：cancel 分支的 early return 改为设置 `exit_reason` 标志后 break 循环，不再直接 return。`exit_reason` 决定 outcome：

- `DiagnosisDone` → LLM 判定 positive/negative/uncertain
- `AgentCrashed` → outcome 直接标 `negative`
- `AgentStopped` → outcome 直接标 `negative`

### 4.2 `spawn_one_shot`

一次性 LLM 调用，产出：会话摘要、经验卡片全部字段、outcome 判定、结构化字段（env/service/symptom/language/root_cause）回写 sessions 表。

| 项 | 决策 |
|---|---|
| spawn 方式 | 不带 `--sessions`，不走 stream 解析，直接读 stdout 拿完整输出 |
| 输入 | 结构化压缩后的会话数据：用户消息完整保留；agent 文本回复完整保留；工具调用只保留 `tool_name` + `args` + `output` 前 20 行，丢弃 `raw_stdout` |
| 输出格式 | prompt 要求 LLM 输出 JSON 代码块 |
| 并发 | API 配额冲突暂不处理（摘要生成是低频操作，与用户主动诊断撞上的概率低） |
| 失败处理 | 后台 task 失败只记日志，不影响前端流程 |

### 4.3 输出解析策略（分层降级）

LLM 输出不保证遵守格式，代码分层降级兜底：

1. **JSON 代码块提取**：从 stdout 正则匹配 ` ```json ... ``` ` 或 ` ``` ... ``` `，取内容做 `serde_json::from_str`。成功则用。
2. **逐行扫描**：代码块提取失败时，扫描每行尝试 `serde_json::from_str`。兼容 LLM 直接输出裸 JSON。
3. **部分字段降级**：JSON 解析成功但字段缺失时：
   - 缺 `outcome` → 默认 `uncertain`
   - 缺 `language` → 默认 `"unknown"`
   - 缺 `root_cause` → outcome 为 `negative` 则合理，正常入库；outcome 为 `positive` 但无根因 → 降级为 `uncertain`
   - 缺 `investigation_path` / `experience_lesson` → 存空字符串
4. **规则提取回退**：完全提取不到 JSON 时：
   - 摘要：取 stdout 全文前 500 字
   - 结构化字段：从 sessions 表已有数据取
   - outcome：根据退出路径判定（DiagnosisDone → `uncertain`，AgentCrashed/Stopped → `negative`）
5. **跳过不入库**：回退也失败时记 `error!` 日志，跳过该经验。宁可少一条，不存脏数据。

### 4.4 向量化

**嵌入模型**：bge-small-zh-v1.5（512 维，95MB），通过 `fastembed` crate（基于 ONNX Runtime）加载，CPU 推理。

- 模型文件预编译进 Tauri 安装包（作为 Tauri resource），运行时解压到 `Paths::models_dir()`。用户开箱即用，不下载。
- 应用启动时（`lib.rs` setup）预加载模型到 `AppState.embedding: Arc<EmbeddingModel>`，避免首次 spawn 时加载延迟（1-3 秒）。
- 常驻内存约 95MB，可接受。

**向量化文本**：用户首条消息原文。

- 入库：诊断完成时，取 sessions 表中该会话的第一条用户消息（`role='user'`, `seq=0`），原文向量化。
- 查询：新建会话首次 spawn 时，取用户当前消息原文向量化。
- 查询和入库用同一种文本（用户首条消息），语义空间对齐。

**向量存储**：sqlite-vec。

- 使用 `sqlite-vec` crate（官方 Rust 绑定），通过 `sqlite3vec_init` 自动注册扩展，不走动态加载。
- 独立 `rusqlite::Connection` 管理向量读写，不依赖 sqlx pool（sqlx 默认禁用 extension loading）。
- 案例检索是低频操作，不需要连接池。
- native 库平台相关（Windows `.dll` / Linux `.so` / macOS `.dylib`），Tauri 打包时按目标平台包含。

### 4.5 经验检索与注入

**检索时机**：仅会话首次 spawn 时（`session_id == None`，新建会话）。后续 spawn（`session_id == Some(id)`）不重复注入——agent CLI 通过 `--sessions` 已有第一次注入的经验在上下文里。

**检索策略**：

```
1. 用户首条消息原文 → 嵌入模型推理生成向量
2. sqlite-vec 近邻查询（experiences_vec JOIN experiences 按 outcome 过滤）：
   - positive: ORDER BY distance LIMIT 2
   - negative: ORDER BY distance LIMIT 1
   - uncertain 不参与检索
3. 余弦相似度 < 0.5 的经验不注入（默认阈值，可调）
4. 空结果时不注入（首次诊断或无匹配时 prompt 无经验 section）
```

**注入位置**：system prompt 和用户消息之间，独立 section。

**注入格式**：

```
{system_prompt}

---

## 历史经验参考
### 经验 1（成功）：OrderService OOM
症状：内存持续增长，Full GC 频繁
根因：线程池无上限泄漏
排查路径：jstat 显示 Full GC 频繁 → arthas thread 发现 2000+ 线程 → 定位 ThreadPoolExecutor
经验：OOM 先查线程数

### 经验 2（未成功）：OrderService OOM
症状：内存持续增长
排查路径：jmap dump → 分析无泄漏点
经验：dump 分析无果时，考虑线程泄漏而非堆泄漏

---

用户消息：{message}
```

- 正例标注"成功"，反例标注"未成功"
- `uncertain` 经验不注入
- 不受 system prompt override 影响——经验注入是应用层逻辑，用户自定义 `friday.md` 不影响经验注入

### 4.6 经验去重与增量更新

新经验入库前，先做去重检查。去重分两步：

1. **向量检索找候选**：用新经验的 `query_text`（用户首条消息原文）向量化，在 `experiences_vec` 中做近邻查询，取 top-5 候选。
2. **精确字段匹配确认**：在候选集中按结构化字段做精确匹配：
   - 正例：症状+语言+服务+根因 四元组完全匹配
   - 反例：症状+语言+服务 三元组完全匹配（反例无根因）

两步原因：全表扫描结构化字段效率低，先用向量缩小范围到 top-5，再精确匹配确认。

**正例去重**：按 症状+语言+服务+根因 四元组判定。

- 四元组完全匹配已有正例 → 增量更新（见下）
- 四元组不匹配（根因不同）→ 新增独立经验

**反例去重**：按 症状+语言+服务 三元组判定（反例没有根因）。

- 三元组匹配已有反例 → 保留最近一条，合并经验提炼
- 三元组不匹配 → 新增独立经验

**增量更新规则**（四元组匹配时）：

- 新经验的排查路径有已有经验没有的步骤 → 追加到 `investigation_path`
- 新经验的提炼有新观点 → 追加到 `experience_lesson`
- `occurrence_count` +1
- `last_seen_at` 更新为最新时间

**跨 outcome 处理**：

- 已有是反例，新经验是正例 → 用新经验替换（正例优先，更新排查路径和根因）
- 已有是正例，新经验也是正例 → 增量更新
- 已有是反例，新经验也是反例 → 保留一个，合并经验提炼
- 已有是正例，新经验是反例 → 保留正例，反例的经验提炼追加为补充说明

## 5. 数据库变更

### 5.1 新增表

```sql
-- 会话摘要（绑定 session，删 session 级联删除）
CREATE TABLE IF NOT EXISTS session_summaries (
    session_id TEXT PRIMARY KEY,
    summary_text TEXT NOT NULL,
    generated_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

-- 经验库（完全独立，不引用 session）
CREATE TABLE IF NOT EXISTS experiences (
    id TEXT PRIMARY KEY,
    symptom TEXT NOT NULL,
    service TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'unknown',
    root_cause TEXT,
    investigation_path TEXT NOT NULL DEFAULT '',
    experience_lesson TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL CHECK(outcome IN ('positive', 'negative', 'uncertain')),
    occurrence_count INTEGER NOT NULL DEFAULT 1,
    last_seen_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    query_text TEXT NOT NULL  -- 用户首条消息原文，用于重新向量化（如模型升级）
);

-- 向量表（sqlite-vec 虚拟表）
CREATE VIRTUAL TABLE IF NOT EXISTS experiences_vec USING vec0(
    id TEXT PRIMARY KEY,
    embedding FLOAT[512]
);
```

### 5.2 sessions 表新增字段

```sql
ALTER TABLE sessions ADD COLUMN language TEXT DEFAULT 'unknown';
```

由 `spawn_one_shot` 的 LLM 推断后回写。

### 5.3 infra/paths.rs 新增

```rust
pub fn models_dir(&self) -> PathBuf {
    self.root.join("models")
}
```

`ensure_dirs()` 加入 `models_dir`。

## 6. 集成点

| 现有模块 | 改动 |
|---|---|
| `spawn.rs` | 新增 `spawn_one_shot(prompt) -> String`；`build_prompt` 增加经验注入 section（仅首次 spawn 时）；`spawn_active` 增加 `is_first_message: bool` 参数 |
| `stream.rs` | cancel 分支改为 break 而非 return；三条路径统一收尾；末尾 `tokio::spawn` 后台摘要+经验生成 task |
| `infra/paths.rs` | 新增 `models_dir()` + `ensure_dirs()` |
| `infra/db.rs` | migration 0006：`session_summaries`、`experiences`、`experiences_vec`、`sessions.language`；sqlite-vec 扩展加载 |
| `knowledge/` | 新增 `memory` 模块：`generate_summary()`、`extract_experience()`、`embed()`、`recall_experiences()`、`upsert_experience()`。接口设计 MCP-ready——`recall_experiences(symptom, service, env) -> Vec<Experience>` 可直接被未来 MCP 工具 handler 调用 |
| `lib.rs` setup | 预加载 bge-small-zh-v1.5 模型到 `AppState.embedding` |
| `AppState` | 新增 `embedding: Arc<EmbeddingModel>`、`vec_conn: Arc<Mutex<rusqlite::Connection>>` |

## 7. 数据流

### 7.1 诊断完成 → 生成经验

```
consume_stream 三条退出路径统一收尾
  │
  ├─ flush DB → 更新消息状态 → 从 agents map 移除 → emit 事件（立即完成）
  │
  └─ tokio::spawn 后台 task
       │
       ├─ 从 SQLite 读取会话数据（结构化压缩：工具调用只保留 name+args+output 前 20 行）
       │
       ├─ spawn_one_shot（一次性 agent CLI 调用，不带 --sessions）
       │    ├─ prompt：要求输出 JSON，包含摘要 + 经验卡片字段 + outcome + 结构化字段
       │    └─ stdout → 分层降级解析
       │
       ├─ 回写 sessions 表（language、env、service、symptom、root_cause）
       │
       ├─ 存入 session_summaries 表
       │
       ├─ 经验卡片去重检查
       │    ├─ 正例：症状+语言+服务+根因 四元组向量检索
       │    └─ 反例：症状+语言+服务 三元组向量检索
       │
       ├─ 命中 → 增量更新（追加排查步骤/经验提炼，occurrence_count+1）
       └─ 未命中 → 新增经验 + 向量化存入 experiences_vec
```

### 7.2 新诊断开始 → 检索经验

```
send_message_cmd（session_id == None，新建会话）
  │
  ├─ 用户消息原文 → 嵌入模型推理（AppState.embedding，已预加载）
  │
  ├─ sqlite-vec 近邻查询
  │    ├─ positive top-2
  │    └─ negative top-1
  │
  ├─ 相似度阈值过滤
  │
  ├─ build_prompt
  │    ├─ system_prompt（受 override 影响）
  │    ├─ --- 分隔符
  │    ├─ ## 历史经验参考（正例标注"成功"，反例标注"未成功"）
  │    ├─ --- 分隔符
  │    └─ 用户消息：{message}
  │
  └─ spawn_active（注入经验后的 prompt）
```

## 8. 待调研（不阻塞 v1 实现）

| 项 | 说明 |
|---|---|
| codeagentcli / opencode 细粒度禁用 auto-memory | 是否有配置文件或环境变量只禁用 auto-memory，不影响其他功能 |
| sqlite-vec Windows 打包 | native 库（`.dll`）作为 Tauri resource 的打包方式和运行时加载路径 |
| bge-small-zh-v1.5 模型打包 | 模型文件（95MB）作为 Tauri resource 的打包流程，运行时解压到 `models_dir()` |

## 9. 后续演进

| 演进项 | 说明 |
|---|---|
| `recall_experiences` MCP 工具 | MCP Server 基础设施就绪后，注册 `recall_experiences(symptom, service, env)` 工具，调用 `knowledge::memory::recall_experiences()`。数据层不用改。 |
| 互联网案例获取 | 从互联网获取他人诊断案例，存入独立的 `cases` 表，与 `experiences` 分离。 |
| 经验时间衰减 | 如果用户反馈老经验干扰，加 `last_seen_at` 时间衰减权重。 |
| 经验管理 UI | 查看/删除/编辑经验。 |
| Playbook 自动演化 | 多个相似经验呈现同一模式时，自动生成 playbook。延续现有知识层设计。 |
| 工具生成 | agent 遇到现有工具无法满足的需求时，自动生成诊断脚本，验证后注册为 MCP 工具。 |
