# 运行时模型

## 前后端通信契约

- **command 只负责"发起"和"取消"**，不阻塞等待结果——`start_diagnosis` 立即返回 session_id，后续全走 event。
- **event 按 session_id 归属**，前端按 session 过滤，支持多会话并行。
- **LLM 输出流式 token 推送**，让用户看到 Agent "在想什么"。
- **工具执行有生命周期事件**（executing→result/error），前端可渲染执行进度和结果。

## 并发模型

- MCP Server 单实例共享，所有 agent CLI 连同一个。
- `tokio` async 并发，MCP 工具调用按 session_id 路由；SSH 连接按 environment_id 池化（跨会话共享），工具调用以 environment 参数指定目标环境。
- 多会话并行，受本机资源约束（同时 3-5 个诊断会话为上限）。
- 连接生命周期：空闲 10 分钟后台巡检自动断开；环境删除立即断开。断开在后台 task 执行（fire-and-forget），不阻塞连接池。

## 取消模型

| 用户动作 | agent CLI | SSH 连接 | 会话状态 |
|---------|-----------|---------|---------|
| 停 agent（补充信息/跑偏了） | kill，保留会话 | 保留（环境级连接与会话无关） | active，可重新 spawn agent |
| 关闭会话 | kill | 保留（环境级连接由空闲清理管理） | closed，持久化保留 |
| 删除环境 | - | 立即断开该环境连接 | - |

停 agent 顺序：
1. SIGTERM agent CLI → 超时 SIGKILL 兜底
2. abort 正在执行的 MCP 工具调用（待决确认全部取消）
3. 推 event: `agent_stopped`
4. 会话状态保持 active

关闭会话顺序：
1. 停 agent（同上）
2. 会话标记 closed（不动 SSH 连接）
3. 推 event: `session_closed`
