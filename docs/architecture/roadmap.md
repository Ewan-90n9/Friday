# v1 范围与后续演进

| 项 | v1 | 后续 |
|----|----|------|
| Agent CLI | opencode | 扩展 claude code / codex 等 |
| 目标环境 | SSH 单通道（K8s 场景经 SSH 执行 kubectl） | — |
| LLM 接入 | 集成本机 agent CLI | 自建 LLM client trait，直连 LLM API |
| 持久化 | SQLite | — |
| 工具库 | run_command + 脚本工具热插拔 | 结构化 JVM 工具批次（jstat/jcmd/arthas/读日志/读dump） |
| 知识层 | playbook 库（内置装载/导入提炼/经验沉淀 + 人工审核） | 爬虫管线 |
