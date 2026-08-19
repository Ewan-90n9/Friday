# 运行时模型

## 前后端通信契约

- **command 只负责"发起"和"取消"**，不阻塞等待结果——`start_diagnosis` 立即返回 session_id，后续全走 event。
- **event 按 session_id 归属**，前端按 session 过滤，支持多会话并行。
- **LLM 输出流式 token 推送**，让用户看到 Agent "在想什么"。
- **工具执行有生命周期事件**（executing→result/error），前端可渲染执行进度和结果。

## 并发模型

- MCP Server 单实例共享，所有 agent CLI 连同一个。
- `tokio` async 并发，MCP 工具调用按 session_id 路由到对应 SSH 连接。
- 多会话并行，受本机资源约束（同时 3-5 个诊断会话为上限）。

## 取消模型

| 用户动作 | agent CLI | SSH 连接 | 会话状态 |
|---------|-----------|---------|---------|
| 停 agent（补充信息/跑偏了） | kill，保留会话 | 保留 | active，可重新 spawn agent |
| 关闭会话 | kill | 断开 | closed，持久化保留 |

停 agent 顺序：
1. SIGTERM agent CLI → 超时 SIGKILL 兜底
2. abort 正在执行的 MCP 工具调用
3. 推 event: `agent_stopped`
4. 会话状态保持 active，SSH 连接保留

关闭会话顺序：
1. 停 agent（同上）
2. 关闭 SSH/kubectl 连接
3. 会话标记 closed
4. 推 event: `session_closed`
