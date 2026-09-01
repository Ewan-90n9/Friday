# arthas attach 命令修复设计（CLI 参数 + telnet 禁用 + stop 认证）

日期：2026-09-01
状态：已评审通过
关联 issue：#7（第二次重开：v0.11.1 后 attach 仍失败）

## 背景

v0.11.1 修复了包下发（vendored zip + SFTP 直传，已生效——用户日志不再有 404）。本次失败在更深层：**arthas-boot 启动即崩**。

目标机日志（`/tmp/arthas-friday-3241191.log`）：

```
CLIException: The value '--pid' is not accepted by the argument 'pid'
Usage: arthas-boot [-h] [--target-ip <value>] [--telnet-port <value>] [--http-port <value>] ... [pid]
```

### 根因调查（本地 math-game JVM + arthas 4.3.5 全链路复现验证）

| # | 问题 | 证据来源 |
|---|---|---|
| 1（阻断） | arthas-boot 的 pid 是**位置参数**（`@Argument(index=0)`），无 `--pid` 选项 → CLI 解析异常立即 exit 1 → 端口不监听 → 探活 60s 超时 | 目标机日志 + Bootstrap.java 源码（4.3.5 tag） |
| 2（坑） | 修复时若 CLI 传 `--telnet-port -1`，Bootstrap attach 前的 telnet 预检（`findProcessByTelnetClient`）对 -1 端口抛 `IllegalArgumentException: port out of range:-1` → exit 1 | **本地实测复现**（java -jar arthas-boot.jar --attach-only --telnet-port -1 ... → exit 1） |
| 3（潜伏） | properties 缺 `arthas.localConnectionNonAuth=true` → `arthas_close` 的 HTTP stop（`/api` + Bearer）401：arthas 侧 `/api` 路径只认 Basic 认证/本地连接免密，Bearer 提取仅对 MCP endpoint 生效 → stop 失败 → agent 残留目标 JVM | BasicHttpAuthenticatorHandler.java + SecurityAuthenticatorImpl.java（反编译）+ 本地实测（无该配置 stop 401） |

### arthas 4.3.5 配置机制（源码确认）

- Bootstrap（boot 进程）：CLI 选项 `--http-port`/`--telnet-port`/`--attach-only`/`--password` 等；pid 位置参数。attach 前做 telnet 端口预检（启动 TelnetConsole 连一次，IOException 忽略、其他异常 exit 1）
- Agent（目标 JVM 内）：`ArthasBootstrap` 以 core jar 所在目录为 `arthas.home`，自动加载 `{home}/arthas.properties`；优先级 命令行 > Env > System Properties > arthas.properties，**`arthas.config.overrideAll=true` 反转为 properties 最高**
- MCP server：`arthas.mcpEndpoint` 非空即启动，挂在与 HTTP server 同端口；MCP 认证 Bearer token == `arthas.password`
- 本地验证通过的完整链路：attach（位置 pid + attach-only + telnet 预检端口）→ 18563 监听 → MCP initialize（Bearer）成功返回 serverInfo → 错误 token 401 → HTTP stop 后端口关闭

## 修复设计

全部改动集中在 `src-tauri/src/arthas/attach.rs`（命令构造纯函数 + 编排端口分配两处调用点）。

### 1. `attach_command` 重写（修 #1 #2）

```
cd {home} && nohup {java} -jar arthas-boot.jar \
  --attach-only --http-port {http_port} --telnet-port {telnet_det_port} {pid} \
  < /dev/null >> /tmp/arthas-friday-{pid}.log 2>&1 & echo attach-started
```

- pid 改为**位置参数**（放最后，与 usage 一致）
- `--attach-only`：attach 完成后 boot 进程退出（telnet 已禁用，不启动交互 client；也避免 nohup 挂着一个无用的 telnet client 进程）
- `--http-port {http_port}`：显式传 Friday 分配的端口（agent 侧真实监听端口，与探活/隧道一致）
- `--telnet-port {telnet_det_port}`：传一个**有效空闲端口**给 Bootstrap 的预检用（预检连接被拒 → 忽略 → 通过；agent 侧因 overrideAll 不绑这个端口）

### 2. `arthas_properties_content` 追加配置（修 #2 #3）

```
arthas.config.overrideAll=true       ← properties 覆盖 CLI：agent 侧 telnetPort=-1 真正禁用 telnet
arthas.telnetPort=-1                 ←（已有）agent 侧禁 telnet
arthas.httpPort={http_port}          ←（已有）
arthas.password={token}              ←（已有）
arthas.mcpEndpoint=/mcp              ←（已有）
arthas.localConnectionNonAuth=true   ← 新增：stop 走 /api 时本地连接免密（arthas 官方包默认配置，此前被覆盖丢失）
arthas.ip=127.0.0.1                  ← 新增：HTTP/MCP 只绑回环（arthas 官方包默认，此前覆盖时丢了）
```

### 3. 端口分配改为一次取两个（配合 #2）

`find_free_port_command` 扩展：返回**两个**空闲端口——第一个作 http（18563-18572 段），第二个作 telnet 预检（自然落在 18573+，只要空闲即可，无段约束）。`parse_free_port` 相应返回 `(http_port, telnet_det_port)`。

### 4. 不变的

- 探活逻辑（`wait_http_ready` 轮询 http_port）、隧道、MCP 握手、`arthas_close` 的 stop 命令构造（`/api` + Bearer + curl/wget）——`localConnectionNonAuth` 修好后 stop 即可 200
- 包下发（v0.11.1 已修）、java 解析、用户对齐逻辑
- 前端无改动

## 测试策略（TDD）

attach.rs 已有命令构造纯函数测试（`test_arthas_properties_content`、`test_attach_command` 等——按现状 grep 确认命名），全部改为断言新形态：

- `attach_command`：pid 是位置参数（正则断言 `arthas-boot\.jar.*{pid}$` 结尾、不含 `--pid`）、含 `--attach-only`、http/telnet 两个端口正确嵌入
- `arthas_properties_content`：含 overrideAll/localConnectionNonAuth/ip 三项 + 原有四项；httpPort 值正确
- `find_free_port_command`/`parse_free_port`：两端口解析（成功两个、仅一个空闲时报错、输出格式变化）
- 编排层若有两端口传递的类型改动，编译驱动覆盖

本地手工回归（实现者已做过的 math-game 全链路）不在 CI 范围；发布后用户验证。

## 产物影响

无（纯 Rust 逻辑，不动资源）。
