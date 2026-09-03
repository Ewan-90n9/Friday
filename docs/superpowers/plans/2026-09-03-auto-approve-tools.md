# 免确认模式（Auto-Approve Tools）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 全局免确认开关——开启后所有工具调用（含 Low/High 风险）跳过二次确认直接执行，关闭后行为与现状完全一致。

**Architecture:** `app_settings` 新增布尔键 `auto_approve_tools`（默认 false）；`tools/risk.rs` 新增 `should_confirm` 纯函数；唯一拦截点 `mcp/server.rs` 的 `call_tool` 每次调用现读设置决定是否走确认流程。前端设置弹窗新增分区（开启需内联确认一次）、顶栏琥珀色徽标、`settingsStore` 扩展。

**Tech Stack:** Tauri 2（Rust + sqlx SQLite）、React + TypeScript + Zustand。

**Spec:** `docs/superpowers/specs/2026-09-03-auto-approve-tools-design.md`

**测试说明:** `call_tool` 需要 rmcp `RequestContext`，无法在单测中构造。拦截逻辑的单元覆盖 = `should_confirm` 真值表 + 设置读取兜底；端到端行为（免确认直出执行卡片 / 恢复确认）走 Task 8 手动清单。

---

### Task 1: `should_confirm` 纯函数

**Files:**
- Modify: `src-tauri/src/tools/risk.rs`

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/tools/risk.rs` 末尾追加：

```rust

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_confirm_truth_table() {
        // 关闭（默认）：ReadOnly 直通，Low/High 需确认
        assert!(!should_confirm(RiskLevel::ReadOnly, false));
        assert!(should_confirm(RiskLevel::Low, false));
        assert!(should_confirm(RiskLevel::High, false));
        // 开启：全部免确认
        assert!(!should_confirm(RiskLevel::ReadOnly, true));
        assert!(!should_confirm(RiskLevel::Low, true));
        assert!(!should_confirm(RiskLevel::High, true));
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml should_confirm`
Expected: 编译失败，`cannot find function should_confirm`

- [ ] **Step 3: 实现**

在 `RiskLevel` enum 定义之后追加：

```rust

/// Low/High 风险工具且未开启免确认模式时才需要用户确认
pub fn should_confirm(risk_level: RiskLevel, auto_approve: bool) -> bool {
    matches!(risk_level, RiskLevel::Low | RiskLevel::High) && !auto_approve
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml should_confirm`
Expected: `test_should_confirm_truth_table ... ok`

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/tools/risk.rs
git commit -m "feat: add should_confirm for auto-approve gate"
```

---

### Task 2: 设置键读写 + Tauri commands

**Files:**
- Modify: `src-tauri/src/app/settings.rs`
- Modify: `src-tauri/src/lib.rs:311-312`（invoke_handler 注册区）

- [ ] **Step 1: 写失败测试**

在 `src-tauri/src/app/settings.rs` 的 `mod tests` 内追加（`use super::*;` 已覆盖所需导入）：

```rust
    #[tokio::test]
    async fn test_auto_approve_tools_defaults_false_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        assert!(!auto_approve_tools(&pool).await);
    }

    #[tokio::test]
    async fn test_auto_approve_tools_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_AUTO_APPROVE_TOOLS, "true").await.unwrap();
        assert!(auto_approve_tools(&pool).await);
        set_setting(&pool, KEY_AUTO_APPROVE_TOOLS, "false").await.unwrap();
        assert!(!auto_approve_tools(&pool).await);
    }

    #[tokio::test]
    async fn test_auto_approve_tools_invalid_value_falls_back_false() {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        set_setting(&pool, KEY_AUTO_APPROVE_TOOLS, "yes").await.unwrap();
        assert!(!auto_approve_tools(&pool).await);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml auto_approve`
Expected: 编译失败，`cannot find function auto_approve_tools` / `cannot find value KEY_AUTO_APPROVE_TOOLS`

- [ ] **Step 3: 实现常量、getter、setter、commands**

在 `src-tauri/src/app/settings.rs` 中，`DEFAULT_ARTIFACTORY_BASE_URL` 常量之后追加：

```rust
pub const KEY_AUTO_APPROVE_TOOLS: &str = "auto_approve_tools";
```

在 `normalize_base_url` 函数之后、`get_artifactory_base_url_cmd` 之前追加：

```rust
/// 读取免确认模式开关：缺失、非法值、DB 错误一律返回 false（fail-safe，
/// 绝不因读不到设置而放行高风险操作）
pub async fn auto_approve_tools(pool: &SqlitePool) -> bool {
    match get_setting(pool, KEY_AUTO_APPROVE_TOOLS).await {
        Ok(Some(value)) if value == "true" => true,
        Ok(Some(value)) if value == "false" => false,
        Ok(Some(value)) => {
            tracing::warn!(
                key = KEY_AUTO_APPROVE_TOOLS,
                value = %value,
                "invalid auto_approve_tools value, falling back to false"
            );
            false
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(
                key = KEY_AUTO_APPROVE_TOOLS,
                error = %e,
                "failed to read auto_approve_tools, falling back to false"
            );
            false
        }
    }
}
```

在文件末尾（`set_artifactory_base_url_cmd` 之后、`mod tests` 之前）追加：

```rust
#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn get_auto_approve_tools_cmd(state: State<'_, crate::AppState>) -> Result<bool, String> {
    tracing::info!("get_auto_approve_tools_cmd called");
    Ok(auto_approve_tools(&state.db).await)
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn set_auto_approve_tools_cmd(
    state: State<'_, crate::AppState>,
    enabled: bool,
) -> Result<(), String> {
    tracing::info!(enabled = enabled, "set_auto_approve_tools_cmd called");
    let value = if enabled { "true" } else { "false" };
    set_setting(&state.db, KEY_AUTO_APPROVE_TOOLS, value)
        .await
        .map_err(|e| {
            tracing::error!(key = KEY_AUTO_APPROVE_TOOLS, error = %e, "failed to persist auto_approve_tools");
            e.to_string()
        })
}
```

- [ ] **Step 4: 注册 commands**

在 `src-tauri/src/lib.rs` 的 `invoke_handler` 列表中，`app::settings::set_artifactory_base_url_cmd,` 之后追加两行：

```rust
            app::settings::get_auto_approve_tools_cmd,
            app::settings::set_auto_approve_tools_cmd,
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml auto_approve`
Expected: 3 个新测试全部 `ok`

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/app/settings.rs src-tauri/src/lib.rs
git commit -m "feat: auto_approve_tools setting with fail-safe getter and commands"
```

---

### Task 3: 拦截点改动（mcp/server.rs）

**Files:**
- Modify: `src-tauri/src/mcp/server.rs:22`（import）
- Modify: `src-tauri/src/mcp/server.rs:167-168`（call_tool 确认分支）

- [ ] **Step 1: 改 import**

将 `src-tauri/src/mcp/server.rs:22`：

```rust
use crate::tools::risk::RiskLevel;
```

改为：

```rust
use crate::tools::risk::{RiskLevel, should_confirm};
```

- [ ] **Step 2: 改确认分支**

将 `call_tool` 中（约 167-168 行）：

```rust
            // Confirmation flow for Low/High risk tools
            if matches!(risk_level, RiskLevel::Low | RiskLevel::High) {
```

改为：

```rust
            // Confirmation flow for Low/High risk tools; skipped entirely
            // when auto-approve mode is enabled (global setting)
            let auto_approve = crate::app::settings::auto_approve_tools(&self.pool).await;
            if auto_approve && matches!(risk_level, RiskLevel::Low | RiskLevel::High) {
                tracing::info!(
                    session_id = %session_id,
                    tool = %tool_name,
                    ?risk_level,
                    "tool call auto-approved (auto-approve mode enabled)"
                );
            }
            if should_confirm(risk_level, auto_approve) {
```

确认分支内部（confirm_id 生成到 120s 超时 match 结束，原 169-221 行）**逐字节不变**。

- [ ] **Step 3: 编译 + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部通过（含 Task 1/2 新增测试）

- [ ] **Step 4: 提交**

```bash
git add src-tauri/src/mcp/server.rs
git commit -m "feat: skip tool confirmation when auto-approve mode enabled"
```

---

### Task 4: 前端 IPC 绑定 + settingsStore 扩展

**Files:**
- Modify: `src/lib/ipc.ts:110-112`（setArtifactoryBaseUrl 之后）
- Modify: `src/store/settingsStore.ts`

- [ ] **Step 1: 加 IPC 绑定**

在 `src/lib/ipc.ts` 的 `setArtifactoryBaseUrl` 函数之后追加：

```typescript
export async function getAutoApproveTools(): Promise<boolean> {
  return invoke<boolean>("get_auto_approve_tools_cmd");
}

export async function setAutoApproveTools(enabled: boolean): Promise<void> {
  return invoke<void>("set_auto_approve_tools_cmd", { enabled });
}
```

- [ ] **Step 2: 扩展 settingsStore**

`src/store/settingsStore.ts` 全文替换为：

```typescript
import { create } from "zustand";
import {
  getArtifactoryBaseUrl,
  setArtifactoryBaseUrl,
  getAutoApproveTools,
  setAutoApproveTools,
} from "@/lib/ipc";

interface SettingsStore {
  artifactoryBaseUrl: string;
  autoApprove: boolean;
  loading: boolean;
  saving: boolean;
  error: string | null;
  load: () => Promise<void>;
  saveBaseUrl: (url: string) => Promise<boolean>;
  saveAutoApprove: (enabled: boolean) => Promise<boolean>;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export const useSettingsStore = create<SettingsStore>((set, get) => ({
  artifactoryBaseUrl: "",
  autoApprove: false,
  loading: false,
  saving: false,
  error: null,

  load: async () => {
    set({ loading: true, error: null });
    try {
      const [url, autoApprove] = await Promise.all([
        getArtifactoryBaseUrl(),
        getAutoApproveTools(),
      ]);
      set({ artifactoryBaseUrl: url, autoApprove });
    } catch (e) {
      set({ error: errMsg(e) });
    } finally {
      set({ loading: false });
    }
  },

  saveBaseUrl: async (url) => {
    set({ saving: true, error: null });
    try {
      await setArtifactoryBaseUrl(url);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },

  saveAutoApprove: async (enabled) => {
    set({ saving: true, error: null });
    try {
      await setAutoApproveTools(enabled);
      set({ autoApprove: enabled });
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    } finally {
      set({ saving: false });
    }
  },
}));
```

- [ ] **Step 3: 类型检查**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 4: 提交**

```bash
git add src/lib/ipc.ts src/store/settingsStore.ts
git commit -m "feat: ipc bindings and store for auto-approve setting"
```

---

### Task 5: 设置弹窗免确认分区

**Files:**
- Modify: `src/components/agents/AgentSettingsDialog.tsx`

- [ ] **Step 1: 加组件状态与处理器**

在 `AgentSettingsDialog.tsx` 中，`const [urlDraft, setUrlDraft] = useState("");` / `const [savingUrl, setSavingUrl] = useState(false);`（34-35 行）之后追加：

```tsx
  const autoApprove = useSettingsStore((s) => s.autoApprove);
  const saveAutoApprove = useSettingsStore((s) => s.saveAutoApprove);

  const [confirmAutoApprove, setConfirmAutoApprove] = useState(false);
  const [savingAutoApprove, setSavingAutoApprove] = useState(false);

  const handleToggleAutoApprove = async (next: boolean) => {
    if (!next) {
      // 关闭直接生效，不确认
      setConfirmAutoApprove(false);
      setSavingAutoApprove(true);
      try {
        await saveAutoApprove(false);
      } finally {
        setSavingAutoApprove(false);
      }
      return;
    }
    // 开启需确认一次
    setConfirmAutoApprove(true);
  };

  const handleConfirmAutoApprove = async () => {
    setSavingAutoApprove(true);
    try {
      const ok = await saveAutoApprove(true);
      if (ok) setConfirmAutoApprove(false);
    } finally {
      setSavingAutoApprove(false);
    }
  };
```

- [ ] **Step 2: 加分区 JSX**

在 Artifactory 分区结束（`</div>` + `</div>`，原 181-182 行）与「手动添加」分区之间插入：

```tsx
        {/* Auto-approve tools */}
        <div className="border-t border-border shrink-0">
          <div className="px-5 py-3 space-y-2">
            <label className="flex items-center gap-2 text-sm text-foreground cursor-pointer">
              <input
                type="checkbox"
                checked={autoApprove || confirmAutoApprove}
                onChange={(e) => handleToggleAutoApprove(e.target.checked)}
                disabled={savingAutoApprove}
              />
              免确认模式
            </label>
            <p className="text-xs text-muted-foreground">
              开启后所有工具调用免确认直接执行（含高风险：任意命令、堆 dump、文件上传），仅建议内网非生产环境开启
            </p>
            {confirmAutoApprove && (
              <div className="rounded-md border border-warning/60 bg-warning/5 px-3 py-2 space-y-2">
                <p className="text-xs text-warning">
                  开启后 agent 执行任何操作都不再需要你确认，包括 run_command、heap_dump、file_upload
                  等高风险操作。确定开启？
                </p>
                <div className="flex items-center gap-2">
                  <button
                    onClick={handleConfirmAutoApprove}
                    disabled={savingAutoApprove}
                    className="px-3 py-1 rounded-md bg-warning text-warning-foreground text-xs hover:bg-warning/80 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {savingAutoApprove ? "开启中..." : "确认开启"}
                  </button>
                  <button
                    onClick={() => setConfirmAutoApprove(false)}
                    disabled={savingAutoApprove}
                    className="px-3 py-1 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    取消
                  </button>
                </div>
              </div>
            )}
            {settingsError && (
              <p className="text-xs text-destructive break-words">{settingsError}</p>
            )}
          </div>
        </div>
```

注 1：`settingsError`（30 行）已存在，此处复用；`warning` 色值为设计语言琥珀警示色（`globals.css` `--color-warning: #EAB308`）。
注 2：checkbox 绑定 `autoApprove || confirmAutoApprove`——受控组件：勾选瞬间 store 未变，需借 `confirmAutoApprove` 让勾选状态可见，取消时自然回退。

- [ ] **Step 3: 类型检查**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 4: 提交**

```bash
git add src/components/agents/AgentSettingsDialog.tsx
git commit -m "feat(ui): auto-approve section in settings dialog with inline confirm"
```

---

### Task 6: 顶栏免确认徽标

**Files:**
- Modify: `src/components/layout/TopBar.tsx`

- [ ] **Step 1: 加 import 与启动加载**

`TopBar.tsx` 顶部 import 区改为：

```tsx
import { useState, useRef, useEffect } from "react";
import { GearSix, ShieldWarning } from "@phosphor-icons/react";
import { FridayMark } from "@/components/FridayMark";
import { useAgentStore } from "@/store/agentStore";
import { useSettingsStore } from "@/store/settingsStore";
import { AgentSettingsDialog } from "@/components/agents/AgentSettingsDialog";
import type { AgentRow } from "@/lib/types";
```

`TopBar` 组件内（`const error = ...` 之后）追加：

```tsx
  const autoApprove = useSettingsStore((s) => s.autoApprove);
  const loadSettings = useSettingsStore((s) => s.load);

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);
```

- [ ] **Step 2: 加徽标 JSX**

在右侧 `<div className="flex items-center gap-1">` 内、agent 状态按钮之前插入：

```tsx
        {autoApprove && (
          <button
            onClick={() => setSettingsOpen(true)}
            className="flex items-center gap-1.5 px-2 py-1 rounded-md bg-warning/10 border border-warning/20 text-warning text-xs cursor-pointer hover:bg-warning/20 transition-colors"
            aria-label="免确认模式已开启，点击打开设置"
          >
            <ShieldWarning size={14} weight="regular" aria-hidden="true" />
            免确认
          </button>
        )}
```

- [ ] **Step 3: 类型检查**

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 4: 提交**

```bash
git add src/components/layout/TopBar.tsx
git commit -m "feat(ui): amber auto-approve badge in top bar"
```

---

### Task 7: 文档同步

**Files:**
- Modify: `docs/architecture/error-handling.md:23`
- Modify: `docs/architecture/overview.md:15`

- [ ] **Step 1: error-handling.md**

在 §安全边界 末行「拦截点在 Tool Registry dispatch 前：每个 tool 注册时声明 risk_level，MCP server 在执行前检查。」之后追加一行：

```markdown
全局免确认开关：设置弹窗中的「免确认模式」开启后，Low/High 工具均跳过确认直接执行；设置读取失败一律回落确认模式。详见 [免确认模式设计](../superpowers/specs/2026-09-03-auto-approve-tools-design.md)。
```

- [ ] **Step 2: overview.md**

将决策 #9（15 行）：

```markdown
| 9 | 安全边界 | 分级拦截：只读自主 / 低风险确认 / 高风险强制确认 |
```

改为：

```markdown
| 9 | 安全边界 | 分级拦截：只读自主 / 低风险确认 / 高风险强制确认；全局「免确认模式」开启后全部豁免 |
```

- [ ] **Step 3: 提交**

```bash
git add docs/architecture/error-handling.md docs/architecture/overview.md
git commit -m "docs: auto-approve mode in security boundary docs"
```

---

### Task 8: 最终验证 + 手动清单

**Files:** 无新改动（验证任务）

- [ ] **Step 1: 全量自动化验证**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部通过

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无错误

Run: `pnpm typecheck`
Expected: 无错误

- [ ] **Step 2: 手动验证（`pnpm tauri dev`）**

按 spec 手动清单逐项验证：

1. 设置弹窗勾选「免确认模式」→ 出现内联确认条 → 取消 → checkbox 回退未勾选
2. 再次勾选 → 确认开启 → 顶栏出现琥珀色「免确认」徽标，点击徽标打开设置弹窗
3. 会话中让 agent 执行 `run_command`（High）→ 无确认卡片，直接出现执行卡片；日志中有 `tool call auto-approved`
4. 取消勾选（直接生效）→ 徽标消失 → 再执行同一工具 → ConfirmCard 恢复
5. 会话进行中切换开关 → 下一次工具调用立即按新状态执行（现读设置）
6. 开启前已挂起的确认卡片仍可手动处理 / 120s 超时

- [ ] **Step 3: 收尾**

验证全部通过后，如 spec 中「已实现功能」需要登记（AGENTS.md 维护者自行决定），否则无需额外提交。
