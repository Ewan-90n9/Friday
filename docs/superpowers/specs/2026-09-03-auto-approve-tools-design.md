# 免确认模式（Auto-Approve Tools）设计

## 概述

Friday 定位于内网非生产环境的故障诊断，用户反馈当前 Low/High 风险工具的二次确认在日常使用中过于繁琐。本 spec 设计一个**全局免确认开关**：开启后所有工具调用（含高风险）跳过确认直接执行，关闭后行为与现状完全一致。

改动范围：`app_settings` 新增一个键、`mcp/server.rs` 拦截点一处判断、设置弹窗一个分区、顶栏一个状态徽标。不改动 ConfirmRegistry、120s 超时、取消联动等现有机制。

## 核心决策

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 开关粒度 | 全局单开关，不按环境/会话区分 |
| 2 | 豁免范围 | Low + High 全部豁免（含 run_command、jvm_heap_dump、file_upload） |
| 3 | 存储 | `app_settings` KV 表新增键 `auto_approve_tools`，默认 `"false"`，无需迁移 |
| 4 | 生效时机 | 每次工具调用现读设置，会话中途切换即时生效，无需重启 |
| 5 | 开启摩擦 | 开启时内联确认条确认一次；关闭直接生效 |
| 6 | 挂起确认 | 开关切换不影响已挂起的确认卡片，由用户处理或 120s 超时 |
| 7 | 聊天流展示 | 免确认模式下静默跳过：无确认卡片，直接出现执行卡片 |
| 8 | 状态感知 | 顶栏琥珀色"免确认"徽标，点击打开设置弹窗 |
| 9 | 读取失败兜底 | DB 异常/值非法一律视为 false，回落确认模式（fail-safe） |
| 10 | 审计 | 自动放行时记 `info!` 日志（tool + risk_level）；`tool_calls` 表照常持久化 risk_level |

## 数据层

### 设置键

- 键名：`auto_approve_tools`，值 `"true"` / `"false"`，默认 `"false"`
- 沿用 `src-tauri/src/app/settings.rs` 现有模式：`KEY_AUTO_APPROVE_TOOLS` / `DEFAULT_AUTO_APPROVE_TOOLS` 常量 + 类型化 getter `get_auto_approve_tools(pool) -> bool` + `set_auto_approve_tools(pool, bool)`
- getter 对缺失键、非法值、DB 错误统一返回 `false`（错误记 `warn!` 日志）
- 新增 `get_auto_approve_tools_cmd` / `set_auto_approve_tools_cmd` 两个 Tauri command（`#[tracing::instrument]`），注册进 `invoke_handler`

## 后端：拦截点改动

### 判定逻辑

`src-tauri/src/tools/risk.rs` 新增纯函数：

```rust
pub fn should_confirm(risk_level: RiskLevel, auto_approve: bool) -> bool {
    matches!(risk_level, RiskLevel::Low | RiskLevel::High) && !auto_approve
}
```

### call_tool 流程

`src-tauri/src/mcp/server.rs` 的 `call_tool` 中，现有判断（约 168 行）改为：

1. 读取 risk_level 后调用 `get_auto_approve_tools(&self.pool)`（server 已持有 pool，无新管线）
2. `should_confirm(risk_level, auto_approve)` 为 true → 走现有确认流程（confirm_id + ConfirmRequired + oneshot + 120s 超时），**逐字节不变**
3. 为 false（含 ReadOnly、或开关开启时的 Low/High）→ 跳过确认，直接进入执行流程：exec channel 获取 → `ToolExecuting` 事件 → `handler.execute()` → `ToolResult` 事件 → 持久化 `tool_calls`（risk_level 照常记录）
4. 开关开启导致跳过确认时记 `info!`（含 tool 名 + risk_level + session_id）

### 不变的部分

- `ConfirmRegistry`、`confirm_tool_cmd`、`cancel_for_session`（stop/close session 联动）、120s 超时：全部不动
- 开关切换不批量处理已挂起确认：开启前发出的确认卡片仍由用户手动处理或超时（保守方向；关闭时更无影响）

## 前端

### 设置弹窗

`AgentSettingsDialog.tsx` 新增 `border-t` 分区"免确认模式"：

- checkbox + 描述文案（明示豁免含高风险操作：任意命令执行、堆 dump、文件上传；仅建议内网非生产环境开启）
- **开启需确认一次**：勾选后分区下方展开内联确认条（警示文案 + [确认开启] / [取消] 按钮），确认后立即调用 set IPC 持久化生效；取消则 checkbox 回退。不走嵌套弹窗
- 取消勾选（关闭）不弹确认，直接持久化
- 状态与动作放 `settingsStore`（新增 `autoApprove` + load/save 动作），应用启动时加载（顶栏徽标需要）

### 顶栏徽标

- `TopBar.tsx`：`autoApprove` 为 true 时显示琥珀色"免确认"徽标（呼应设计语言 §5.4 琥珀警示色），点击打开设置弹窗；false 不显示

### 聊天流

- 零改动：免确认模式下后端不发 `ConfirmRequired`，前端自然只走 `tool_executing` → 结果卡片流程
- `ipc.ts` 新增 get/set 绑定

## 日志

- 自动放行：`info!`（tool、risk_level、session_id）
- 设置读取失败：`warn!`（键名 + 错误）
- 设置写入失败：`error!`（遵循日志规范，错误路径必须有 error/warn）

## 测试

### Rust 单测

- `should_confirm` 真值表：3 风险级 × 开关两态
- 设置 roundtrip：set true → get true；set false → get false
- 兜底：键缺失 → false；非法值（"yes" 等）→ false
- 拦截点集成：开关开启时 High 工具直接执行（无 ConfirmRegistry 写入）；关闭时走确认流程

### 手动验证清单

1. 开启开关（经内联确认条）后执行 run_command → 直出执行卡片，无确认卡片，日志有自动放行记录
2. 关闭开关后同一工具 → ConfirmCard 恢复
3. 徽标随开关增减，点击打开设置弹窗
4. 会话中途切换开关即时生效
5. 开启前已挂起的确认卡片仍可手动处理 / 120s 超时

## 文档同步

- `docs/architecture/error-handling.md` §安全边界：补全局免确认开关说明
- `docs/architecture/overview.md` 决策 #9（分级拦截）：加开关备注
