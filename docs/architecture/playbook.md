# 知识层（Playbook）

- 形态：结构化 YAML/TOML（故障模式 → 推荐工具序列 + 指标判读）+ 自然语言说明。
- 注入方式：prompt 精简索引 + MCP 工具 `get_playbook(symptom)` 按需获取完整内容。
- agent 不调 `get_playbook` 也能直接调诊断工具——playbook 是辅助不是强制。
- 位置：`<app_data>/playbooks/`，agent 运行时生成，用户可编辑。加 playbook 不改 Rust 代码。
