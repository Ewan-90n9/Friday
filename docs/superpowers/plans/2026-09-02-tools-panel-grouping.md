# 工具面板分组化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 右侧「诊断工具」面板从 51 项扁平随机序列表重构为按功能模块分组、默认全部折叠、两行式列表项的可折叠分组列表。

**Architecture:** 后端 `ToolDef` 新增 `category` 枚举字段（注册时声明，单一事实来源），`list_tools_cmd` 下发 category 并按「分类声明序 → 名称字母序」稳定排序；前端按 category 分组渲染（组头可折叠 + Phosphor 图标 + 计数），列表项为「名称 + 风险徽章 / 一行描述」两行式，显示名统一加 `friday_` 前缀。MCP 层（`registry.list()` / `mcp/server.rs list_tools`）**不动**。

**Tech Stack:** Rust (Tauri backend, serde)、React + TypeScript + Tailwind、@phosphor-icons/react v2。

**Spec:** [docs/superpowers/specs/2026-09-02-tools-panel-grouping-design.md](../specs/2026-09-02-tools-panel-grouping-design.md)

**验证命令：** `cargo check --manifest-path src-tauri/Cargo.toml`、`cargo test --manifest-path src-tauri/Cargo.toml`、`pnpm typecheck`（均在仓库根目录执行）。

---

### Task 1: ToolCategory 枚举 + ToolDef.category 字段（全部构造点）

**Files:**
- Create: `src-tauri/src/tools/category.rs`
- Modify: `src-tauri/src/tools/mod.rs`
- Modify: `src-tauri/src/tools/registry.rs`
- Modify: `src-tauri/src/tools/builtin/mod.rs`（echo）
- Modify: `src-tauri/src/tools/builtin/run_command.rs`
- Modify: `src-tauri/src/tools/builtin/list_environments.rs`
- Modify: `src-tauri/src/tools/builtin/ensure_tool.rs`
- Modify: `src-tauri/src/tools/builtin/jvm/processes.rs`（list_processes → Environment）
- Modify: `src-tauri/src/tools/builtin/jvm/simple.rs`（5 个构造点）
- Modify: `src-tauri/src/tools/builtin/jvm/heap_dump.rs`
- Modify: `src-tauri/src/tools/builtin/heap/mod.rs`（heap_tool_def，1 处覆盖 9 个工具）
- Modify: `src-tauri/src/tools/builtin/arthas/mod.rs`（arthas_tool_def，1 处覆盖 27 个工具）
- Modify: `src-tauri/src/tools/builtin/file_transfer.rs`（4 个构造点）

说明：Rust 结构体加字段会导致所有构造点编译失败，无法拆成更小的编译绿单元，因此本任务一次性完成数据管道接入；行为断言（TDD）在 Task 2/3 进行。

- [ ] **Step 1: 创建 category.rs**

创建 `src-tauri/src/tools/category.rs`：

```rust
use serde::{Deserialize, Serialize};

/// 工具分类。声明顺序即面板分组展示顺序（environment → jvm → heap → arthas → file_transfer → builtin）。
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

- [ ] **Step 2: tools/mod.rs 声明模块**

`src-tauri/src/tools/mod.rs` 当前内容是 4 行 pub mod；在 `pub mod builtin;` 后加一行：

```rust
pub mod builtin;
pub mod category;
pub mod confirm;
pub mod registry;
pub mod risk;
```

- [ ] **Step 3: registry.rs 加字段**

`src-tauri/src/tools/registry.rs`：

3a. 在 `use super::risk::RiskLevel;`（第 1 行）后加：

```rust
use super::category::ToolCategory;
```

3b. `ToolDef`（第 26-35 行）在 `pub risk_level: RiskLevel,` 后加字段：

```rust
    /// 面板分组归属（见 tools/category.rs；枚举声明序即分组展示序）
    pub category: ToolCategory,
```

3c. 测试辅助 `make_tool_def`（第 84-98 行）在 `risk_level: risk,` 后加：

```rust
            category: ToolCategory::Environment,
```

- [ ] **Step 4: 各构造点补 category**

每个文件统一模式：在现有 `use crate::tools::risk::RiskLevel;` 旁加 `use crate::tools::category::ToolCategory;`，然后在每个 `ToolDef { ... }` 字面量的 `risk_level: ...` 行后加一行 `category: ToolCategory::X,`。归属分配（与语义对齐，`list_processes` 虽在 jvm 模块但归 Environment）：

| 文件 | 构造点 | category |
|---|---|---|
| `builtin/mod.rs` | `echo_tool_def` | `Builtin` |
| `builtin/run_command.rs` | `run_command_tool_def` | `Environment` |
| `builtin/list_environments.rs` | `list_environments_tool_def` | `Environment` |
| `builtin/ensure_tool.rs` | `ensure_tool_tool_def` | `Environment` |
| `builtin/jvm/processes.rs` | `list_processes_tool_def` | `Environment` |
| `builtin/jvm/simple.rs` | 5 个 `*_tool_def` | `Jvm` |
| `builtin/jvm/heap_dump.rs` | `jvm_heap_dump_tool_def` | `Jvm` |
| `builtin/heap/mod.rs` | `heap_tool_def` | `Heap` |
| `builtin/arthas/mod.rs` | `arthas_tool_def` | `Arthas` |
| `builtin/file_transfer.rs` | `file_transfer_tool_defs` 内 4 个字面量 | `FileTransfer` |

代表性示例（`builtin/mod.rs` echo，其余同理）：

```rust
use crate::tools::category::ToolCategory;
```

```rust
pub fn echo_tool_def() -> ToolDef {
    ToolDef {
        // ... name / description / input_schema 不变 ...
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Builtin,   // ← 新增行
        needs_channel: false,
        handler: Arc::new(EchoHandler),
    }
}
```

`file_transfer.rs` 的 4 个字面量中 `transfer_status` 和 `transfer_cancel` 的锚点行相同（都是 `risk_level: RiskLevel::ReadOnly,`），分别用各自的 handler 行做上下文区分：

```rust
            risk_level: RiskLevel::Low,
            category: ToolCategory::FileTransfer,
            needs_channel: false,
            handler: Arc::new(FileDownloadHandler(tools.clone())),
```

```rust
            risk_level: RiskLevel::High,
            category: ToolCategory::FileTransfer,
            needs_channel: false,
            handler: Arc::new(FileUploadHandler(tools.clone())),
```

```rust
            risk_level: RiskLevel::ReadOnly,
            category: ToolCategory::FileTransfer,
            needs_channel: false,
            handler: Arc::new(TransferStatusHandler(tools.clone())),
```

```rust
            risk_level: RiskLevel::ReadOnly,
            category: ToolCategory::FileTransfer,
            needs_channel: false,
            handler: Arc::new(TransferCancelHandler(tools)),
```

`jvm/simple.rs` 5 处：`jvm_gc_stats` / `jvm_thread_dump` / `jvm_heap_info` / `jvm_vm_info` / `jvm_class_histogram` 每个 `risk_level:` 行后加 `category: ToolCategory::Jvm,`。

- [ ] **Step 5: 编译检查**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过（若报某构造点缺 category 字段，说明该文件漏改，补上）。

- [ ] **Step 6: 跑既有测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部通过（既有测试未涉及 category，不应有失败）。

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/tools/
git commit -m "feat: tool category metadata in registry"
```

---

### Task 2: metadata 测试补 category 断言

**Files:**
- Modify: `src-tauri/src/tools/builtin/mod.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/run_command.rs`（tests，新增测试）
- Modify: `src-tauri/src/tools/builtin/list_environments.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/ensure_tool.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/jvm/processes.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/jvm/simple.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/jvm/heap_dump.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/heap/mod.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/file_transfer.rs`（tests）
- Modify: `src-tauri/src/tools/builtin/arthas/mod.rs`（新增 tests 模块）

说明：Task 1 已设置字段值，本任务的断言是回归保护（验证归属不被改错），非红绿循环。

- [ ] **Step 1: builtin/mod.rs — echo**

`test_echo_tool_def_has_correct_metadata` 中 `assert_eq!(def.risk_level, RiskLevel::ReadOnly);` 后加：

```rust
        assert_eq!(def.category, ToolCategory::Builtin);
```

- [ ] **Step 2: run_command.rs — 新增 metadata 测试（原本没有）**

在 `mod tests` 内（`test_clamp_timeout_default_when_missing` 之前）加：

```rust
    #[test]
    fn test_tool_def_metadata() {
        let def = run_command_tool_def(
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            Arc::new(tokio::sync::Mutex::new(crate::exec::pool::ExecChannelPool::new())),
            std::path::PathBuf::from("/tmp/x"),
        );
        assert_eq!(def.name, "run_command");
        assert_eq!(def.risk_level, RiskLevel::High);
        assert_eq!(def.category, ToolCategory::Environment);
        assert!(!def.needs_channel);
    }
```

- [ ] **Step 3: list_environments.rs**

`test_tool_def_metadata` 中 `assert_eq!(def.risk_level, crate::tools::risk::RiskLevel::ReadOnly);` 后加：

```rust
        assert_eq!(def.category, ToolCategory::Environment);
```

- [ ] **Step 4: ensure_tool.rs**

`test_tool_def_metadata` 中 `assert_eq!(def.risk_level, RiskLevel::Low);` 后加：

```rust
        assert_eq!(def.category, ToolCategory::Environment);
```

- [ ] **Step 5: jvm/processes.rs**

`test_tool_def_metadata` 中 `assert_eq!(def.risk_level, RiskLevel::ReadOnly);` 后加：

```rust
        assert_eq!(def.category, ToolCategory::Environment);
```

- [ ] **Step 6: jvm/simple.rs — 5 个工具**

`test_tool_defs_metadata` 的 risk 断言块后、`drop(tmp);` 前加：

```rust
        assert_eq!(jvm_gc_stats_tool_def(core.clone()).category, ToolCategory::Jvm);
        assert_eq!(jvm_thread_dump_tool_def(core.clone()).category, ToolCategory::Jvm);
        assert_eq!(jvm_heap_info_tool_def(core.clone()).category, ToolCategory::Jvm);
        assert_eq!(jvm_vm_info_tool_def(core.clone()).category, ToolCategory::Jvm);
        assert_eq!(jvm_class_histogram_tool_def(core.clone()).category, ToolCategory::Jvm);
```

- [ ] **Step 7: jvm/heap_dump.rs**

`test_tool_def_metadata` 中 `assert_eq!(def.risk_level, RiskLevel::High);` 后加：

```rust
        assert_eq!(def.category, ToolCategory::Jvm);
```

- [ ] **Step 8: heap/mod.rs — register_all 九工具**

`test_register_all_nine_tools_all_readonly` 的 for 循环内加一行（与既有两行断言并列）：

```rust
            assert_eq!(d.category, ToolCategory::Heap, "{name}");
```

- [ ] **Step 9: file_transfer.rs — 4 个工具**

`test_tool_def_metadata` 末尾既有循环 `for d in &defs { assert!(!d.needs_channel); }` 内加一行：

```rust
            assert_eq!(d.category, ToolCategory::FileTransfer, "{}", d.name);
```

- [ ] **Step 10: arthas/mod.rs — 新增 tests 模块（原本没有）**

文件末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::arthas::manager::{ArthasConfig, AttachFactory};
    use crate::tools::category::ToolCategory;

    #[test]
    fn test_arthas_tool_def_metadata() {
        // dummy attach factory：metadata 测试不触发 attach，构造一个必然失败的闭包即可
        let factory: AttachFactory =
            Arc::new(|_req| Box::pin(async { Err(ManagerError::Attach("dummy".to_string())) }));
        let manager = Arc::new(ArthasManager::new(factory, ArthasConfig::default()));
        let def = arthas_tool_def(
            "arthas_open",
            "test",
            RiskLevel::Low,
            OPEN,
            ArthasToolKind::Open,
            manager,
            sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
            std::path::PathBuf::from("/tmp/x"),
        );
        assert_eq!(def.name, "arthas_open");
        assert_eq!(def.category, ToolCategory::Arthas);
        assert_eq!(def.risk_level, RiskLevel::Low);
        assert!(!def.needs_channel);
    }
}
```

- [ ] **Step 11: 跑测试**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部通过。若 arthas dummy factory 闭包类型推断失败，显式标注返回类型：`Box::pin(async { Err(ManagerError::Attach("dummy".to_string())) } as Pin<Box<dyn Future<Output = Result<crate::arthas::manager::AttachedSession, ManagerError>> + Send>>)`。

- [ ] **Step 12: Commit**

```bash
git add src-tauri/src/tools/builtin/
git commit -m "test: assert tool category in metadata tests"
```

---

### Task 3: list_tools_cmd 下发 category + 稳定排序（TDD）

**Files:**
- Modify: `src-tauri/src/app/lifecycle.rs`（ToolInfo 结构、list_tools_cmd、tests）

- [ ] **Step 1: 写失败的排序测试**

`lifecycle.rs` 末尾 `mod tests` 内（`use crate::infra::db;` 后）加 import 与两个测试：

```rust
    use crate::tools::category::ToolCategory;

    #[test]
    fn test_sort_tool_infos_orders_by_category_then_name() {
        let mk = |name: &str, category: ToolCategory| ToolInfo {
            name: name.to_string(),
            description: String::new(),
            risk_level: crate::tools::risk::RiskLevel::ReadOnly,
            category,
        };
        let mut infos = vec![
            mk("heap_open", ToolCategory::Heap),
            mk("arthas_watch", ToolCategory::Arthas),
            mk("jvm_gc_stats", ToolCategory::Jvm),
            mk("echo", ToolCategory::Builtin),
            mk("heap_close", ToolCategory::Heap),
            mk("arthas_dashboard", ToolCategory::Arthas),
        ];
        sort_tool_infos(&mut infos);
        let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "jvm_gc_stats",
                "heap_close",
                "heap_open",
                "arthas_dashboard",
                "arthas_watch",
                "echo",
            ]
        );
    }

    #[test]
    fn test_sort_tool_infos_empty() {
        let mut infos: Vec<ToolInfo> = Vec::new();
        sort_tool_infos(&mut infos);
        assert!(infos.is_empty());
    }
```

- [ ] **Step 2: 跑测试确认失败（编译错误即 RED）**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_sort_tool_infos`
Expected: 编译失败——`ToolInfo` 没有 `category` 字段、`sort_tool_infos` 未定义。

- [ ] **Step 3: 实现 ToolInfo.category + sort_tool_infos + 接线**

`lifecycle.rs` 中将现有 `ToolInfo` 与 `list_tools_cmd`（第 322-342 行）替换为：

```rust
#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub risk_level: crate::tools::risk::RiskLevel,
    pub category: crate::tools::category::ToolCategory,
}

/// 面板展示排序：category 声明序 → 名称字母序。
/// ToolRegistry 内部是 HashMap，list() 迭代序随机，必须显式排序保证每次启动顺序一致。
fn sort_tool_infos(infos: &mut [ToolInfo]) {
    infos.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.name.cmp(&b.name)));
}

#[tauri::command]
pub async fn list_tools_cmd(
    state: State<'_, crate::AppState>,
) -> Result<Vec<ToolInfo>, String> {
    let tools = state.tool_registry.list();
    let mut infos: Vec<ToolInfo> = tools
        .into_iter()
        .map(|def| ToolInfo {
            name: def.name.clone(),
            description: def.description.clone(),
            risk_level: def.risk_level,
            category: def.category,
        })
        .collect();
    sort_tool_infos(&mut infos);
    Ok(infos)
}
```

注意：`registry.list()` 与 `mcp/server.rs` 的 `list_tools` 保持不动（MCP 侧顺序无关紧要，不改变既有语义）。

- [ ] **Step 4: 跑测试确认通过（GREEN）**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_sort_tool_infos`
Expected: 2 个测试 PASS。

- [ ] **Step 5: 全量编译与测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml; cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过，全部测试 PASS（serde 序列化 ToolCategory 为 snake_case，如 `"environment"`）。

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/app/lifecycle.rs
git commit -m "feat: list_tools_cmd returns category with stable sort"
```

---

### Task 4: 前端类型 + ToolsPanel 分组重构

**Files:**
- Modify: `src/lib/types.ts`（ToolInfo + ToolCategory）
- Modify: `src/components/tools/ToolsPanel.tsx`（整文件重写）

说明：前端无单测框架（AGENTS.md 仅要求 `pnpm typecheck`），验证 = 类型检查 + Task 5 视觉手检。

- [ ] **Step 1: types.ts 加 ToolCategory**

`src/lib/types.ts` 第 113-117 行的 `ToolInfo` 改为（新增类型放在其上方）：

```ts
export type ToolCategory =
  | "environment"
  | "jvm"
  | "heap"
  | "arthas"
  | "file_transfer"
  | "builtin";

export interface ToolInfo {
  name: string;
  description: string;
  risk_level: RiskLevel;
  category: ToolCategory;
}
```

- [ ] **Step 2: 重写 ToolsPanel.tsx**

`src/components/tools/ToolsPanel.tsx` 整文件替换为：

```tsx
import { useEffect, useMemo, useState } from "react";
import {
  Wrench,
  CircleNotch,
  CaretRight,
  CaretDown,
  Desktop,
  Cpu,
  ChartPie,
  Terminal,
  ArrowsLeftRight,
  Gear,
} from "@phosphor-icons/react";
import type { Icon } from "@phosphor-icons/react";
import { listTools } from "@/lib/ipc";
import type { ToolCategory, ToolInfo } from "@/lib/types";

const RISK_LABELS: Record<string, { label: string; className: string }> = {
  read_only: { label: "只读", className: "bg-success/10 text-success border-success/20" },
  low: { label: "低", className: "bg-warning/10 text-warning border-warning/20" },
  high: { label: "高", className: "bg-destructive/10 text-destructive border-destructive/20" },
};

// 分组展示顺序沿诊断流程：定位环境/进程 → JVM 基础诊断 → 堆分析 → Arthas → 文件传输 → 通用
// 与后端 tools/category.rs 的 ToolCategory 声明序一致
const CATEGORY_META: { key: ToolCategory; label: string; icon: Icon }[] = [
  { key: "environment", label: "环境与进程", icon: Desktop },
  { key: "jvm", label: "JVM 诊断", icon: Cpu },
  { key: "heap", label: "堆快照分析", icon: ChartPie },
  { key: "arthas", label: "Arthas 动态诊断", icon: Terminal },
  { key: "file_transfer", label: "文件传输", icon: ArrowsLeftRight },
  { key: "builtin", label: "通用", icon: Gear },
];

export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 全部默认折叠；仅组件内 state，不持久化
  const [collapsed, setCollapsed] = useState<Record<ToolCategory, boolean>>({
    environment: true,
    jvm: true,
    heap: true,
    arthas: true,
    file_transfer: true,
    builtin: true,
  });

  useEffect(() => {
    listTools()
      .then(setTools)
      .catch((e) => setError(String(e)));
  }, []);

  // 后端已按 category → name 排序，按到达序分桶即可；未知 category 回退通用组
  const grouped = useMemo(() => {
    const buckets = new Map<ToolCategory, ToolInfo[]>();
    for (const meta of CATEGORY_META) buckets.set(meta.key, []);
    for (const tool of tools ?? []) {
      const key = buckets.has(tool.category) ? tool.category : "builtin";
      buckets.get(key)!.push(tool);
    }
    return buckets;
  }, [tools]);

  return (
    <section className="flex-1 flex flex-col min-h-0">
      {/* Header */}
      <div className="flex items-center gap-2 h-10 px-4 border-b border-border shrink-0">
        <Wrench size={14} weight="regular" className="text-muted-foreground" aria-hidden="true" />
        <span
          className="text-xs font-medium text-muted-foreground uppercase tracking-wide"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          诊断工具
        </span>
        {tools && (
          <span className="text-xs text-muted-foreground/60 ml-auto">{tools.length}</span>
        )}
      </div>

      {/* Grouped tool list */}
      <div className="flex-1 overflow-y-auto px-3 py-3">
        {error && (
          <div className="text-destructive text-xs px-1 py-2">{error}</div>
        )}
        {tools === null && !error && (
          <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground text-xs">
            <CircleNotch size={14} weight="regular" className="animate-spin" aria-hidden="true" />
            加载中…
          </div>
        )}
        {tools !== null && tools.length === 0 && (
          <div className="py-8 text-center text-muted-foreground text-xs leading-relaxed">
            暂无已注册工具
          </div>
        )}
        {tools !== null &&
          tools.length > 0 &&
          CATEGORY_META.map((meta) => {
            const items = grouped.get(meta.key)!;
            // 后端数据缺失的分类不渲染空组头
            if (items.length === 0) return null;
            const isCollapsed = collapsed[meta.key];
            const GroupIcon = meta.icon;
            return (
              <div key={meta.key} className="mb-1">
                <button
                  type="button"
                  aria-expanded={!isCollapsed}
                  onClick={() =>
                    setCollapsed((c) => ({ ...c, [meta.key]: !c[meta.key] }))
                  }
                  className="w-full flex items-center gap-1.5 px-1.5 py-1.5 rounded-md hover:bg-surface-2/60 text-left"
                >
                  {isCollapsed ? (
                    <CaretRight
                      size={12}
                      weight="bold"
                      className="text-muted-foreground shrink-0"
                      aria-hidden="true"
                    />
                  ) : (
                    <CaretDown
                      size={12}
                      weight="bold"
                      className="text-muted-foreground shrink-0"
                      aria-hidden="true"
                    />
                  )}
                  <GroupIcon
                    size={12}
                    className="text-muted-foreground shrink-0"
                    aria-hidden="true"
                  />
                  <span className="text-xs font-medium text-foreground/90">{meta.label}</span>
                  <span
                    className="ml-auto text-xs text-muted-foreground/60"
                    style={{ fontFamily: "var(--font-mono)" }}
                  >
                    {items.length}
                  </span>
                </button>
                {!isCollapsed && (
                  <ul className="flex flex-col mt-0.5">
                    {items.map((tool) => {
                      const risk = RISK_LABELS[tool.risk_level] ?? {
                        label: tool.risk_level,
                        className: "bg-muted/50 text-muted-foreground border-border",
                      };
                      // opencode 客户端按 MCP server name 给工具加 friday_ 前缀
                      // （注册表存无前缀名），展示层补齐前缀与聊天流工具卡片一致
                      const displayName = `friday_${tool.name}`;
                      return (
                        <li key={tool.name} className="px-2.5 py-1.5 pl-5">
                          <div className="flex items-center gap-1.5">
                            <code
                              className="text-xs text-foreground font-medium truncate"
                              style={{ fontFamily: "var(--font-mono)" }}
                              title={displayName}
                            >
                              {displayName}
                            </code>
                            <span
                              className={`shrink-0 ml-auto px-1.5 py-px rounded text-[10px] border ${risk.className}`}
                              style={{ fontFamily: "var(--font-mono)" }}
                            >
                              {risk.label}
                            </span>
                          </div>
                          <p
                            className="text-xs text-muted-foreground truncate"
                            title={tool.description}
                          >
                            {tool.description}
                          </p>
                        </li>
                      );
                    })}
                  </ul>
                )}
              </div>
            );
          })}
      </div>
    </section>
  );
}
```

- [ ] **Step 3: 类型检查**

Run: `pnpm typecheck`
Expected: 无错误。（若 `Icon` 类型导入报错，改用 `import type { Icon as PhosphorIcon } from "@phosphor-icons/react";` 并同步改 CATEGORY_META 类型标注。）

- [ ] **Step 4: Commit**

```bash
git add src/lib/types.ts src/components/tools/ToolsPanel.tsx
git commit -m "feat(ui): grouped collapsible tools panel"
```

---

### Task 5: 全量验证与视觉手检

**Files:** 无新改动（如手检发现问题，修复后追加 commit）

- [ ] **Step 1: 后端全量检查**

Run: `cargo check --manifest-path src-tauri/Cargo.toml; cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 编译零警告新增、全部测试 PASS。

- [ ] **Step 2: 前端类型检查**

Run: `pnpm typecheck`
Expected: 无错误。

- [ ] **Step 3: 视觉手检**

Run: `pnpm tauri dev`，打开诊断页右侧「诊断工具」面板，逐项核对：

- [ ] 默认 6 个组全部折叠，只显示组头（图标 + 标签 + 计数：4 / 6 / 9 / 27 / 4 / 1）
- [ ] 组头计数合计 = 51，与面板头部总数一致
- [ ] 点击组头展开/收起，箭头 ▸/▾ 正确切换，`aria-expanded` 属性正确
- [ ] 列表项两行式：mono 名称带 `friday_` 前缀 + 右侧风险徽章，下接一行截断描述
- [ ] 描述与名称悬浮显示全文（title）
- [ ] Arthas 组展开后 27 项完整，组内按名称字母序
- [ ] 风险徽章配色：只读=绿 / 低=黄 / 高=红
- [ ] 折叠状态不持久化：切页/重开面板后恢复全折叠

- [ ] **Step 4: 收尾 commit（如有修复）**

```bash
git add -A
git commit -m "fix: tools panel grouping visual fixes"
```

---

## Self-Review 记录

- **Spec 覆盖**：§4.1 枚举（Task 1 Step 1）✓；§4.2 全部构造点（Task 1 Step 4 表格与示例）✓；§4.3 ToolInfo + 排序 + MCP 层不动（Task 3）✓；§5.1 前端类型与 CATEGORY_META（Task 4 Step 1/2）✓；§5.2 默认全折叠 + 组头交互 + 两行式（Task 4 Step 2）✓；§5.3 friday_ 前缀（Task 4 Step 2 注释处）✓；§6 未知 category 回退 builtin（Task 4 grouped 分桶 + 空组不渲染）✓；§7 测试（Task 2/3 + Task 5 视觉清单）✓；§8 非目标均未越界 ✓。
- **占位符扫描**：无 TBD/TODO；每步含完整代码或精确锚点。
- **类型一致性**：`ToolCategory` Rust 枚举（snake_case serde）↔ TS 联合类型字符串一致；`sort_tool_infos(&mut [ToolInfo])` 签名在测试与实现一致；`CATEGORY_META` 的 `Icon` 类型已验证存在于 @phosphor-icons/react v2 导出。
