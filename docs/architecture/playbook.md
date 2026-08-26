# 知识层（Playbook 库）

> 总纲：[知识库与工具库伞形总纲设计](../superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md)。本页是知识库的架构视图。

## 定位

用**确定可用的知识**确保 agent 别乱试：诊断开始时把对症的、被验证过的排查路径交给 agent。**纯引导**——playbook 是辅助不是强制，agent 不按 playbook 走、不调 `get_playbook` 也能直接调任意工具；"确保"靠知识质量、信任机制与注入时机，不靠强制拦截。

## 数据模型（三层：主干 + 旁注 + 原文附件）

- **主干 `steps`**：工具调用序列。每步含：
  - `tool`：引用工具名——与工具库的**唯一契约**，注册/装载时校验引用完整性
  - `args`：命令模板，`<pid>`、`<日志路径>` 等为运行时参数占位
  - `when`：自然语言分支条件（如 "Step1 判定为堆内泄漏"）
  - `interpret`：结果判读 + 转步指引
- **旁注**：`anti_patterns`（踩坑/无效路径——"别乱试"的另一半）、`reference_notes`（机制说明/指标判读表/方法论）、`prerequisites`（前提条件）、`notes`（修复指导）
- **原文附件 `source_content`**：导入来源保留全文，用于审核对照与将来重新提炼
- 检索锚点：`symptoms` 文本（多条）+ `applicability`（language/framework）

## 存储

- 运行时权威：SQLite `playbooks` 表 + `playbooks_vec` 向量表（sqlite-vec，与经验库同构）
- YAML 仅作导入/导出交换格式

## 审核状态机（信任机制）

```
                 ┌──────────┐
  builtin ──────►│  active  │◄────── 用户审核通过
                 └────▲─────┘
  import/experience   │
  提炼生成 ──────► ┌───┴───┐    用户禁用         ┌──────────┐
                  │ draft │ ─────────────────► │ disabled │
                  └───┬───┘ ◄───────────────── └──────────┘
                      │        用户重新启用
                      └─ 用户驳回 → 直接删除
```

- `draft`：不注入、检索不可见、get_playbook 不返回
- `active`：注入与检索的唯一来源
- `disabled`：冻结保留，可重新启用
- **内置 playbook 装载即 active（确定可用的基本盘）；导入提炼、经验沉淀一律 draft 起步，人工审核后生效**
- 内置 playbook 按 id 幂等升级；用户改过的副本跳过，不覆盖用户改动

## 三条摄入管线

| 管线 | 触发 | 信任路径 |
|---|---|---|
| 内置装载 | 应用启动，随版本发布 | 直接 active |
| 导入提炼 | 用户给 URL/markdown/docx | LLM 提炼（映射规则见总纲 §5.4）→ draft → 审核 |
| 经验沉淀 | 经验库同一四元组模式 `occurrence_count ≥ 3` | 合并生成草稿 → draft → 审核；通过后对应经验标 `converged`，不再注入 |

爬虫管线延后；摄入管线抽象为 `KnowledgeIngest` trait 留扩展点。

## 匹配与注入

- 首次 spawn 时用户消息向量化，语义检索 active playbook（top-3，余弦相似度 ≥ 0.5）
- 摘要形态注入 prompt（标题 + 适用条件 + 步骤概览，一行一步），放在经验 section 之前（信任级更高）
- `get_playbook(id)` MCP 工具返回完整内容（steps/interpret/anti_patterns/reference_notes）——agent 拿注入的 id 主动获取，不限首次 spawn
- 已收敛经验（converged）不重复注入，避免同一知识从两个 section 重复出现
