# Friday 工具面板分组化设计

- 日期：2026-09-02
- 状态：已评审（各节均与用户逐节确认）
- 范围：右侧栏 ToolsPanel（诊断工具面板）的分组化重构 + 后端工具元数据补充分类字段

## 1. 背景与问题

右侧「诊断工具」面板当前是 51 个工具的扁平列表（`src/components/tools/ToolsPanel.tsx`）：

- **无分组**：arthas_* 独占 27 项，与环境/JVM/堆/文件传输工具混排，扫视成本高；
- **顺序随机**：`ToolRegistry.list()` 直接迭代 HashMap，每次启动顺序都不同；
- **无折叠**：51 张卡片从上拉到下，密度低且没有条理；
- **命名不一致**：面板显示 `heap_open`（注册表名），聊天流工具卡片显示 `friday_heap_open`（opencode 按 MCP server name 加前缀），两处对不上。

面板定位（与用户确认）：**浏览参考**——了解 Friday 有哪些诊断能力，不需要搜索/参数查阅等主动查找交互。

## 2. 决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 分组维度 | 按功能模块（与后端模块边界一致） |
| 2 | 分组归属数据源 | `ToolDef` 新增 `category` 字段，注册时声明（单一事实来源） |
| 3 | 顺序 | 后端排序：分类固定序 → 名称字母序（顺带修复随机序） |
| 4 | Arthas 27 项 | 单组（不拆两级子分组） |
| 5 | 默认展开状态 | **全部默认折叠**（6 个组头一览能力全貌） |
| 6 | 列表项密度 | 两行式：名称（mono）+ 风险徽章，下接一行截断描述 |
| 7 | 风险徽章 | 保留，标签缩短为 只读 / 低 / 高 |
| 8 | 命名统一 | 面板显示 `friday_` 前缀（前端展示层规则），与聊天流一致 |
| 9 | 搜索/过滤 | 不加（纯浏览定位） |
| 10 | 折叠状态持久化 | 不做（组件内 state） |

## 3. 分组清单与顺序

展示顺序大体沿诊断流程：定位环境/进程 → JVM 基础诊断 → 堆快照深度分析 → Arthas 动态诊断 → 文件传输 → 通用。

| 分组（category） | 中文标签 | 工具 | 数量 |
|---|---|---|---|
| `environment` | 环境与进程 | list_environments / list_processes / ensure_tool / run_command | 4 |
| `jvm` | JVM 诊断 | jvm_gc_stats / jvm_thread_dump / jvm_heap_info / jvm_vm_info / jvm_class_histogram / jvm_heap_dump | 6 |
| `heap` | 堆快照分析 | heap_open / heap_close / heap_histogram / heap_dominator_tree / heap_leak_suspects / heap_object_info / heap_path_to_gc_roots / heap_references / heap_threads | 9 |
| `arthas` | Arthas 动态诊断 | arthas_open / arthas_close + 25 个代理工具 | 27 |
| `file_transfer` | 文件传输 | file_download / file_upload / transfer_status / transfer_cancel | 4 |
| `builtin` | 通用 | echo | 1 |

注意：`list_processes` 定义在 `tools/builtin/jvm/processes.rs` 但归属 `environment`——category 按工具语义声明，与代码模块位置解耦。

## 4. 后端设计

### 4.1 新增 `ToolCategory` 枚举

新文件 `src-tauri/src/tools/category.rs`（对齐 risk.rs 的模式）：

```rust
use serde::{Deserialize, Serialize};

/// 工具分类。声明顺序即面板分组展示顺序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Environment,
    Jvm,
    Heap,
    Arthas,
    FileTransfer,
    Builtin,
}
```

`tools/mod.rs` 加 `pub mod category;`。

### 4.2 `ToolDef` 增加 category 字段

`registry.rs` 的 `ToolDef` 增加 `pub category: ToolCategory`。归属在各工具构造点声明（约 17 处 `ToolDef { ... }` 字面量）：

- `builtin/mod.rs` echo → `Builtin`
- `builtin/run_command.rs` / `list_environments.rs` / `ensure_tool.rs` → `Environment`
- `builtin/jvm/processes.rs` list_processes → `Environment`
- `builtin/jvm/simple.rs` 5 个 + `jvm/heap_dump.rs` → `Jvm`
- `builtin/heap/mod.rs` `heap_tool_def` 集中构造（1 处覆盖 9 个）→ `Heap`
- `builtin/arthas/mod.rs` `arthas_tool_def` 集中构造（1 处覆盖 27 个）→ `Arthas`
- `builtin/file_transfer.rs` 4 个字面量 → `FileTransfer`

heap/arthas/file_transfer 已有集中构造函数，实际改动点比工具数少得多。

### 4.3 `list_tools_cmd` 下发 category 并稳定排序

`app/lifecycle.rs`：

- `ToolInfo` 增加 `pub category: ToolCategory` 字段；
- 映射后排序：`(category, name)` 双键升序。排序抽成纯函数 `sort_tool_infos(&mut [ToolInfo])` 便于单测；
- `registry.list()` 与 MCP 层 `list_tools` **保持不动**（MCP 侧顺序无关紧要，不改变既有语义）；
- `ToolInfo.name` 维持注册表原名（不带前缀），前缀统一由前端展示层处理（见 §5.3）。

## 5. 前端设计（ToolsPanel 重构）

### 5.1 数据与分组

- `src/lib/types.ts`：新增 `export type ToolCategory = "environment" | "jvm" | "heap" | "arthas" | "file_transfer" | "builtin";`，`ToolInfo` 增加 `category: ToolCategory`；
- ToolsPanel 按 `category` 分组。后端已排好序，前端按到达序分组渲染即可；
- 分类元数据（展示顺序 + 中文标签 + 图标）在前端维护一张有序表 `CATEGORY_META`：

| category | 标签 | Phosphor 图标 |
|---|---|---|
| environment | 环境与进程 | Desktop |
| jvm | JVM 诊断 | Cpu |
| heap | 堆快照分析 | ChartPie |
| arthas | Arthas 动态诊断 | Terminal |
| file_transfer | 文件传输 | ArrowsLeftRight |
| builtin | 通用 | Gear |

图标 weight="regular"、size 12，与面板头部 Wrench（size 14）风格一致。

### 5.2 结构与交互

```
诊断工具  51                      ← 头部不变
────────────────────────────     （示意：堆快照分析组处于展开态；
▸ 环境与进程            4          默认全部折叠，见下方说明）
▸ JVM 诊断              6
▾ 堆快照分析            9        ← 组头：Caret + 图标 + 标签 + 计数，点击整行切换
    heap_dominator_tree   只读   ← 项：两行式
    支配树 Top N（retained …）   ← 描述单行截断，title 悬浮全文
    heap_histogram        只读
    类直方图：按类聚合的 …
▸ Arthas 动态诊断       27
▸ 文件传输              4
▸ 通用                  1
```

- **默认状态**：全部折叠（6 个组头约 170px，能力全貌一目了然）；展开状态为组件内 `useState`，不做持久化；
- **组头**：`CaretRight`（折叠）/ `CaretDown`（展开，对齐设计语言 5.3 的折叠指示），hover 提亮，`aria-expanded` 可访问性属性，计数右对齐（mono 字体，同现有头部计数风格）；
- **列表项**（已确认密度方案 B）：
  - 第一行：mono 工具名（`text-xs` truncate，title 全名）+ 右侧风险徽章；
  - 第二行：描述单行截断（`text-xs text-muted-foreground truncate`，title 全文）；
- **风险徽章**：沿用现有配色（success/warning/destructive token），标签缩短为 只读 / 低 / 高；
- 加载中 / 空态 / 错误态逻辑不变。

### 5.3 命名统一

opencode 客户端按 MCP server name 给工具加 `friday_` 前缀（注册表存无前缀名，见 `builtin/mod.rs` 注释），聊天流工具卡片因此显示 `friday_heap_open`。面板渲染时统一 `friday_${tool.name}`，两处一致。前缀规则写代码注释说明来源，防止后人误删。

## 6. 错误处理

- 前端遇到未知 `category` 值：回退归入「通用」组（防御性；后端枚举全覆盖 + 单测保证正常不出现）；
- 未知 `risk_level` 回退逻辑已存在，保持不动。

## 7. 测试

- **Rust 单测**：
  - 各模块既有 `test_tool_def_metadata` 系列测试补充 `category` 断言（echo / run_command / list_environments / ensure_tool / file_transfer / jvm 系列 / heap / arthas）；
  - 排序纯函数单测：分类序正确、同分类内名称字母序、空列表；
  - registry.rs 现有测试的 `make_tool_def` 辅助构造补 category 字段；
- **验证命令**：`cargo check --manifest-path src-tauri/Cargo.toml`、`cargo test --manifest-path src-tauri/Cargo.toml`、`pnpm typecheck`；
- **视觉手检**：默认全部折叠、逐组展开、Arthas 组 27 项完整、`friday_` 前缀、徽章配色、描述 title 悬浮。

## 8. 非目标

- 不加搜索/过滤框；
- 不做折叠状态持久化；
- 不改三栏布局、侧栏宽度、可折叠侧栏（设计语言 §10 演进项）；
- 不下发 `input_schema`、不做参数展开查阅（超出「浏览参考」定位）;
- 不改 MCP 层工具名与前缀机制（`friday_` 由 opencode 客户端添加，面板为展示层规则）。
