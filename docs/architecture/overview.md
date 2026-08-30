# 架构总览

## 设计决策总表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 定位 | 远程环境运行时故障诊断 Agent |
| 2 | 目标环境 | SSH 单通道直连；K8s 场景 SSH 到目标环境执行 kubectl，不做 K8s API transport |
| 3 | 计算分层 | 全部在 Rust 后端，前端纯展示 |
| 4 | 分层 | 前端 / 应用 / Agent编排 / 诊断工具 / 执行 / 知识 |
| 5 | 执行层 vs 诊断工具层 | 拆分，通道与语义解耦 |
| 6 | Agent↔工具接口 | Tool Registry（MCP tool 注册） |
| 7 | 知识层形态 | 结构化 playbook（主干 steps + 旁注 + 原文附件），SQLite 为运行时权威 |
| 8 | 前后端通信 | command 发起 + event 流式推送，session_id 归属 |
| 9 | 安全边界 | 分级拦截：只读自主 / 低风险确认 / 高风险强制确认 |
| 10 | 会话状态 | 持久化（SQLite），应用层管生命周期 |
| 11 | 存储 | SQLite；文件布局统一在 `app_data_dir`，见 [基础设施](infrastructure.md#文件布局) |
| 12 | 凭证 | OS 密钥链存私钥/密码，其余配置明文入 SQLite |
| 13 | LLM 接入 | v1 集成本机已有 agent CLI，后续再接 LLM |
| 14 | 工具暴露 | Friday 作为 MCP Server |
| 15 | Agent 驱动 | v1 支持 opencode，命令行传 prompt，临时 MCP config，流式 JSON |
| 16 | 知识注入 | 首次 spawn 语义检索注入摘要 + get_playbook(id) 按需获取全文 |
| 17a | 并发模型 | MCP Server 单实例共享，按 session_id 路由 |
| 17b | 取消模型 | 停 agent / 关会话不断开 SSH 连接（连接按环境池化，空闲 10min 自动断开；环境删除立即断开） |
| 17c | 错误处理 | 基础设施错误 Friday 重试；业务错误返回 agent 决策；agent 崩溃不自动重启 |
| 17d | 日志 | tracing + 文件轮转，运行日志与诊断数据分离 |
| 18 | 裸 shell | 无自由任意执行；run_command 工具 High 级确认兜底 |
| 19 | 知识信任 | 内置 playbook 直接生效；导入提炼/经验沉淀需人工审核 |
| 20 | 自定义工具 | 脚本 + 清单（manifest）热插拔，agent 可自服务注册 |

> 知识库与工具库的总体设计见 [知识库与工具库伞形总纲设计](../superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md)。

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
│ - playbook 语义检索结果注入 prompt                        │
│   （首次 spawn top-3 摘要 + get_playbook(id) 指引）        │
│ - 错误处理：agent 崩溃不自动重启，推 event 给用户          │
└──────────────────┬──────────────────────────────────────┘
                   │ MCP 协议（单实例共享，按 session_id 路由）
┌──────────────────▼──────────────────────────────────────┐
│ 诊断工具层 (Rust MCP Server)                              │
│ - Tool Registry：每个工具注册                             │
│   name/schema/handler/risk_level                         │
│ - 内置工具（Rust handler）+ 脚本工具（manifest+脚本）      │
│   统一注册，脚本工具热插拔                                │
│ - run_command 受控兜底（High 级确认）                     │
│ - 结构化封装（首批 JVM 工具已落地：                       │
│   list_processes / jvm_gc_stats / jvm_thread_dump    │
│   / jvm_heap_info / jvm_vm_info / jvm_class_histogram     │
│   / jvm_heap_dump；堆快照分析 heap_* 系列（MAT 引擎，   │
│   自动预热）已落地；arthas/读日志/读dump 后续批次）        │
│ - 风险分级拦截：                                          │
│     只读自主 → 直接执行                                    │
│     低风险 → 推 confirm_required 事件，等前端确认           │
│     高风险 → 醒目警告，强制确认                             │
│ - get_playbook(id) → 返回完整 playbook（步骤+判读+反模式） │
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
│ - SSH Transport（russh 实现，唯一通道；                    │
│   K8s 场景也经 SSH 执行 kubectl）                          │
│ - 连接池：按 environment_id 复用连接（跨会话共享），         │
│   空闲 10min 自动断开；会话与环境解耦，agent 经              │
│   list_environments 发现环境、run_command 指定目标          │
└──────────────────┬──────────────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────────────┐
│ 知识层 (SQLite playbooks 表 + playbooks_vec 向量表)        │
│ - 结构化 playbook：症状 → 工具序列 + 判读 + 反模式          │
│   + 原文附件（三层模型）                                  │
│ - 三条摄入管线：内置装载（免审）/ 导入提炼 / 经验沉淀        │
│   （draft→active→disabled 审核状态机）                    │
│ - 语义检索注入 prompt；经验库为培育池，成熟模式             │
│   （occurrence≥3）沉淀为 playbook 草稿                    │
└─────────────────────────────────────────────────────────┘
```
