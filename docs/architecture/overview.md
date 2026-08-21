# 架构总览

## 设计决策总表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 定位 | 远程环境运行时故障诊断 Agent |
| 2 | 目标环境 | SSH + K8s 双通道，直连 |
| 3 | 计算分层 | 全部在 Rust 后端，前端纯展示 |
| 4 | 分层 | 前端 / 应用 / Agent编排 / 诊断工具 / 执行 / 知识 |
| 5 | 执行层 vs 诊断工具层 | 拆分，通道与语义解耦 |
| 6 | Agent↔工具接口 | Tool Registry（MCP tool 注册） |
| 7 | 知识层形态 | 结构化 YAML + 自然语言（结构化规则 + Prompt 混合） |
| 8 | 前后端通信 | command 发起 + event 流式推送，session_id 归属 |
| 9 | 安全边界 | 分级拦截：只读自主 / 低风险确认 / 高风险强制确认 |
| 10 | 会话状态 | 持久化（SQLite），应用层管生命周期 |
| 11 | 存储 | SQLite；文件布局统一在 `app_data_dir`，见 [基础设施](infrastructure.md#文件布局) |
| 12 | 凭证 | OS 密钥链存私钥/密码，其余配置明文入 SQLite |
| 13 | LLM 接入 | v1 集成本机已有 agent CLI，后续再接 LLM |
| 14 | 工具暴露 | Friday 作为 MCP Server |
| 15 | Agent 驱动 | v1 支持 opencode，命令行传 prompt，临时 MCP config，流式 JSON |
| 16 | 知识注入 | prompt 精简索引 + MCP 工具按需获取 |
| 17a | 并发模型 | MCP Server 单实例共享，按 session_id 路由 |
| 17b | 取消模型 | 停 agent 保留 SSH 连接；关会话才断开连接 |
| 17c | 错误处理 | 基础设施错误 Friday 重试；业务错误返回 agent 决策；agent 崩溃不自动重启 |
| 17d | 日志 | tracing + 文件轮转，运行日志与诊断数据分离 |

## 分层架构

```
┌─────────────────────────────────────────────────────────┐
│ 前端层 (React)                                           │
│ - 对话流 / 工具执行过程可视化 / 结果渲染                  │
│ - 纯展示，无业务逻辑                                      │
└──────────────────┬──────────────────────────────────────┘
                   │ Tauri IPC
                   │   command: start_diagnosis / stop_agent /
                   │            close_session / confirm_tool /
                   │            cancel_diagnosis
                   │   event: agent_started / tool_executing /
                   │          tool_result / llm_thinking /
                   │          confirm_required / agent_stopped /
                   │          agent_crashed / diagnosis_done /
                   │          session_closed
┌──────────────────▼──────────────────────────────────────┐
│ 应用层 (Rust)                                            │
│ - 会话管理（创建/恢复/关闭，持久化到 SQLite）              │
│ - 凭证管理（OS 密钥链存私钥/密码，配置明文入 SQLite）       │
│ - 生命周期编排（spawn/kill agent CLI，管理 SSH 连接池）    │
│ - 事件总线（session_id 归属，推送到前端）                  │
│ - 日志（tracing + 文件轮转，运行日志与诊断数据分离）       │
└──────────────────┬──────────────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────────────┐
│ Agent 编排层 (Rust)                                      │
│ - spawn agent CLI 子进程（v1: opencode）                  │
│ - 命令行传 prompt；MCP config 注入走 opencode 自身配置机制  │
│   Friday 不单独管理临时配置文件                             │
│ - 捕获流式 JSON 输出 → 推 event 给前端                    │
│ - 知识层索引拼入 prompt                                  │
│   （"OOM→调 get_playbook(symptom=oom)"）                 │
│ - 错误处理：agent 崩溃不自动重启，推 event 给用户          │
└──────────────────┬──────────────────────────────────────┘
                   │ MCP 协议（单实例共享，按 session_id 路由）
┌──────────────────▼──────────────────────────────────────┐
│ 诊断工具层 (Rust MCP Server)                              │
│ - Tool Registry：每个工具注册                             │
│   name/schema/handler/risk_level                         │
│ - 结构化封装：jstat/jcmd/arthas/读日志/读dump              │
│   → 结构化输出                                           │
│ - 风险分级拦截：                                          │
│     只读自主 → 直接执行                                    │
│     低风险 → 推 confirm_required 事件，等前端确认           │
│     高风险 → 醒目警告，强制确认                             │
│ - get_playbook(symptom) → 返回诊断路径 YAML 内容           │
│ - 错误处理：                                              │
│     连接失败 → Friday 自动重试 2 次                        │
│     连接中断 → 自动重连 1 次                               │
│     工具超时/解析失败/attach 失败                          │
│     → 返回 error 给 agent 决策                             │
└──────────────────┬──────────────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────────────┐
│ 执行层 (Rust)                                            │
│ - ExecChannel trait：统一接口，run(cmd) → stdout/stderr    │
│ - SSH Transport（russh 实现）                             │
│ - K8s Transport（kubectl exec 实现）                      │
│ - 连接池：按 session 复用连接，停 agent 保留，              │
│   关会话才断开                                            │
└──────────────────┬──────────────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────────────┐
│ 知识层 (<app_data>/playbooks/)                             │
│ - YAML/TOML：故障模式 → 推荐工具序列 + 指标判读说明         │
│ - 自然语言说明（注入 agent prompt 索引）                   │
│ - agent 运行时生成，用户可编辑，不改 Rust 代码              │
└─────────────────────────────────────────────────────────┘
```
