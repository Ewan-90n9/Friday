# 错误处理与安全边界

## 错误处理分层

| 错误类型 | 处理者 | 策略 |
|---------|--------|------|
| SSH 连接建立失败 | Friday | 重试 2 次（间隔递增），仍失败返回 connection_error |
| SSH 连接中途断开 | Friday | 自动重连 1 次，仍失败返回 connection_error |
| 工具命令超时 | Friday | 不重试，返回 timeout_error 给 agent |
| 工具输出解析失败 | Friday | 不重试，返回原始 stdout 给 agent |
| arthas attach 失败 | Friday | 不重试，返回 error 给 agent |
| Agent CLI 崩溃 | Friday | 不重启，推 event: `agent_crashed` 给前端 |

## 安全边界（工具风险分级）

| 级别 | 示例 | 策略 |
|------|------|------|
| 只读自主 | 读日志、jstat、jcmd `Thread.print`、arthas `dashboard` | 直接执行 |
| 低风险需确认 | arthas `trace`/`watch`（注入字节码） | 提示用户，一键确认 |
| 高风险强制确认 | `jmap -dump`（触发 STW）、arthas `redefine`（热改类） | 醒目警告，需显式确认 |

拦截点在 Tool Registry dispatch 前：每个 tool 注册时声明 risk_level，MCP server 在执行前检查。
