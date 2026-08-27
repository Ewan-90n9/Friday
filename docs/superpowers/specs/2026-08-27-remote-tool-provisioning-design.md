# Friday 远程工具装备机制设计（JDK 为首个工具包）

- 日期：2026-08-27
- 状态：已评审（设计各节均已与用户逐节确认）
- 定位：为诊断工具（jstat/jcmd/jstack/jmap，后续 arthas 等）在目标环境上的可用性提供保障。生产环境只有 JRE，诊断工具缺失是常态，本设计解决"探测 → 下载 → 落地 → 复用"的完整链路。

## 1. 背景与问题

- Friday 的 JVM 诊断能力依赖 jstat/jcmd 等工具，但生产环境普遍只装 JRE。
- 目标环境为 BiSheng JDK（华为内部构建），混合架构（x86_64/aarch64）、多版本共存。
- JDK 需从华为内网 Artifactory 精确匹配版本下载（BiSheng 版本串与线上 JRE 一一对应）。
- 网络情况：目标环境可达内网镜像源（优先自己拉取），但可能失败，需要 Friday 下载后推送兜底。

## 2. 核心决策表

| # | 决策点 | 结论 |
|---|--------|------|
| 1 | 总体形态 | 通用装备机制（`ToolPackage` trait + 包注册表），JDK 是第一个实现，arthas 等后续复用 |
| 2 | 下载通道 | 双通道：A 目标环境 curl/wget 自拉（优先）→ B Friday 本地下载 + SFTP 上传（兜底） |
| 3 | 版本匹配 | 精确匹配 BiSheng 版本串（如 205.2.0.110.B001），从 `java -version` 输出解析 |
| 4 | URL 映射 | Artifactory 三段目录（product/major/full）+ 固定文件名模式；base URL 可配置，推导规则硬编码 |
| 5 | 安装位置 | `/tmp/friday-tools/`，跨会话持久复用，不清理 |
| 6 | 缓存策略 | 远端文件系统是唯一事实源（先查目录，缺了才下载）；本地（Friday 侧）按 URL sha256 缓存 |
| 7 | 触发方式 | Agent 通过 MCP 工具 `ensure_tool` 自主调用；playbook prerequisites 可声明依赖 |
| 8 | 风险级别 | Low（只写 /tmp/friday-tools，不碰 JVM 与系统目录）→ 走现有简单确认拦截 |
| 9 | 状态管理 | 无状态表——远端目录即状态，环境重装后自然重新探测 |
| 10 | 传输扩展 | ExecChannel trait 新增 `upload()`（russh SFTP），download 留接口不实现 |
| 11 | 供应商范围 | 本期仅支持 BiSheng；其他 JDK 厂商返回 unsupported_vendor 明确报错 |

## 3. 模块结构

新增 `src-tauri/src/provision/`，与 `tools/`、`exec/` 平级：

```
src-tauri/src/provision/
├── package.rs    # ToolPackage trait + 包注册表（HashMap<name, package>）+ 并发锁
├── jdk.rs        # JDK 包实现：探测 / BiSheng 解析 / URL 生成 / 远端命令编排
└── transfer.rs   # 本地下载（curl.exe 模式，复用 embedding.rs 经验）+ 本地缓存管理
```

### ToolPackage trait（代码级扩展点）

```rust
#[async_trait]
pub trait ToolPackage: Send + Sync {
    fn name(&self) -> &str;                    // "jdk"
    async fn probe(&self, ctx: &ProvisionContext) -> Result<ProbeInfo, ProvisionError>;
    async fn ensure(&self, ctx: &ProvisionContext) -> Result<ProvisionResult, ProvisionError>;
}

// ProvisionContext: exec channel + 本地缓存路径 + 阶段超时配置 + EventBus（进度事件）
// ProbeInfo: { java_version, bisheng_version, arch }（探测结果，ensure 内部也复用）
// ProvisionResult: { tool_home, bins: HashMap<String, String>, cached: bool, elapsed_ms }
```

后续工具包（arthas 是单 jar + 启动命令）各自实现该 trait，注册进包注册表即可获得下载/缓存/双通道/进度上报全部基础设施。

### 并发保护

注册表内按 `(env_id, package)` 维度 tokio Mutex 串行化：同一环境同一包的并发请求排队，后者先重新查缓存（可能前者已完成）直接命中。

## 4. ensure_tool("jdk") 核心流程

```
1. 探测   java_bin -version ; echo "---" ; uname -m   （一条 SSH 命令拿全，避免多次往返）
          → BiSheng 版本串 + OpenJDK 版本（如 21.0.11）+ 架构（x86_64→x64，aarch64→aarch64）
2. 查缓存 test -x /tmp/friday-tools/jdk-21.0.11/bin/jcmd
          ├─ 存在 → 直接返回（cached: true）
          └─ 不存在 ↓
3. 解析   版本串 → Artifactory 下载 URL（规则见 §5）
4. 下载   通道A：目标环境上 curl/wget 自拉（优先）
          通道B：失败 → Friday 本地下载（本地缓存）→ SFTP 推送
5. 落地   tar -xzf 解压到 /tmp/friday-tools/ → 顶层目录 mv 规范化为 jdk-{openjdk_version}
          （BiSheng 包内顶层目录名可能与规范名不同，统一后缓存检查才有确定路径）→ 删 tar 包
6. 验证   test -x bin/jcmd && test -x bin/jstat
7. 返回   tool_home + 各工具二进制完整路径（agent 后续 run_command 直接用全路径）
```

- `java_bin` 默认 `java`；多版本共存时 agent 可传目标服务实际使用的 java 路径（从 ps 进程命令行或服务配置发现）。
- BiSheng 串通常在 `java -version` 的 stdout（部分版本在 stderr），解析器两路都扫。
- 远端命令超时沿用 run_command 的"断连杀进程"策略（drop channel）。
- 阶段超时：探测 15s / 下载 600s / 解压 120s / 验证 15s，总上限 ~13 分钟；MCP 层不额外包裹（各阶段已自限）。

## 5. BiSheng 版本解析与 URL 映射

独立纯函数（`provision/jdk.rs` 内），便于充分单测。

### 输入样例

```bash
java_bin -version ; echo "---" ; uname -m
# stderr: openjdk version "21.0.11" 2025-04-15
#         OpenJDK Runtime Environment (build 21.0.11+9-LTS)
#         OpenJDK 64-Bit Server VM (build 21.0.11+9-LTS, mixed mode)
# stdout: BiSheng_JDK_Enterprise_205.2.0.110.B001
# ---
# x86_64
```

### 解析规则

```
BiSheng_JDK_Enterprise_205.2.0.110.B001
     │        │        │
     │        │        └─ full: 原串原样保留（_ 不动）
     │        └─ major: 205
     └─ product: BiSheng JDK Enterprise（字母段内 _ 替换为空格；分隔数字段的 _ 不动）

URL = {base}/
      {URL_encode(product)}/{URL_encode(product + " " + major)}/{URL_encode(full)}/
      jdk-{openjdk_version}-linux-{arch}.tar.gz
```

实例（base 为华为 Artifactory）：

```
https://cmc-szver-artifactory.cmc.tools.huawei.com/artifactory/cmc-software-release/
  BiSheng%20JDK%20Enterprise/BiSheng%20JDK%20Enterprise%20205/BiSheng_JDK_Enterprise_205.2.0.110.B001/
  jdk-21.0.11-linux-x64.tar.gz
```

- 正则捕获 BiSheng 版本串：字母段中的 `_` 还原为空格得 product 名，拼 `product + " " + major` 得 major_dir。
- 解析失败返回明确错误（附原始串），agent 可回退 run_command 人工排查。
- arch 映射：`x86_64` → `x64`，`aarch64` → `aarch64`，其他值报错。

### 可配置性

- Artifactory base URL 为**全局设置项**（SQLite 设置表），默认值即华为仓库，SettingsDialog 提供编辑。
- 目录推导规则（product/major/full 三段式）**硬编码**在 BiSheng 解析器——纯函数 + 单测保障，不做配置语言（YAGNI）。

### 下载校验

不校验 checksum（内网可信源）。验证：HTTP 200 + 文件大小 > 50MB（防半截文件）+ 解压后 `bin/jcmd` 可执行（最终验证）。

## 6. 双通道下载与 SFTP 传输

### 通道 A：目标环境自拉（优先）

```bash
command -v curl || command -v wget     # 探测下载器
curl -fL --connect-timeout 15 --max-time 600 -o /tmp/friday-tools/jdk-21.0.11.tar.gz '<URL>' \
  || wget -T 15 -t 2 -O /tmp/friday-tools/jdk-21.0.11.tar.gz '<URL>'
```

判据：exit_code == 0 且文件存在。失败（无下载器/网络不通/超时）自动降级通道 B，无需 agent 重试。

### 通道 B：Friday 下载 + SFTP 推送（兜底）

1. **本地下载**：`provision/transfer.rs` 的 `download_to_cache(url) -> PathBuf`，curl.exe 模式（对齐 embedding.rs 实践）。落 `Paths::cache_dir()`（`<app_data>/cache/`，新增进 `ensure_dirs()`）。按 URL sha256 命名缓存，多环境同版本只下载一次。
2. **SFTP 上传**：ExecChannel trait 新增：

```rust
/// 上传文件到远端路径（SFTP 或等价实现）
async fn upload(&self, local: &Path, remote_path: &str)
    -> Result<(), Box<dyn Error + Send + Sync>>;
```

russh 侧用 `channel_open_sftp` 实现，32KB 块流式写。该扩展点同时服务后续 artifacts 回拉（heap dump 拉回本地），本期只实现 upload。
3. **落地**：上传到 `/tmp/friday-tools/jdk-<version>.tar.gz`，之后解压/验证与通道 A 共用同一套远端命令。

### 失败语义

两通道都失败 → 结构化错误 `{error: "provision_failed", stage: "download_a|download_local|upload|extract|verify", message, url}` + 可行动建议。

### 进度可见性

MCP 调用同步阻塞（agent 等结果），但通过 EventBus 发 `AppEvent::ProvisionProgress { session_id, tool, stage, detail }`，前端 ToolCallCard 显示阶段（探测/下载/解压/验证 + 通道）。阶段级粒度，不做字节级进度。

## 7. Agent 集成

### MCP 工具定义

```
名称：ensure_tool
风险级：Low；needs_channel: false（handler 按 environment 参数自取 channel，与 run_command 同模式）
schema：{
  environment: string (required),
  tool: "jdk" (required, enum 目前仅 "jdk"),
  java_bin: string (optional, 默认 "java")
}
描述：确保目标环境已装备指定诊断工具包（当前支持 jdk）。生产环境通常只有 JRE，
缺少 jstat/jcmd 等诊断工具；本工具探测目标 JVM 版本并下载匹配的 JDK 到
/tmp/friday-tools（不影响系统 Java）。返回 tool_home 及各工具完整路径，
后续请用全路径调用（如 /tmp/friday-tools/jdk-21.0.11/bin/jcmd <pid> GC.heap_info）。
重复调用安全：已装备时直接返回。在 JVM 诊断前调用一次。
```

### 返回结构（agent 消费）

```json
{
  "success": true,
  "tool": "jdk",
  "cached": true,
  "java_version": "21.0.11",
  "bisheng_version": "BiSheng_JDK_Enterprise_205.2.0.110.B001",
  "arch": "x64",
  "tool_home": "/tmp/friday-tools/jdk-21.0.11",
  "bins": {
    "jcmd": "/tmp/friday-tools/jdk-21.0.11/bin/jcmd",
    "jstat": "/tmp/friday-tools/jdk-21.0.11/bin/jstat",
    "jstack": "/tmp/friday-tools/jdk-21.0.11/bin/jstack",
    "jmap": "/tmp/friday-tools/jdk-21.0.11/bin/jmap"
  },
  "elapsed_ms": 8500
}
```

### System prompt（agent/prompt.rs 工具使用 section）

新增引导：诊断 JVM 相关问题时，先调用 ensure_tool 装备 JDK，再用返回的 bin 全路径通过 run_command 执行 jstat/jcmd。Playbook 无需改动（prerequisites 自由文本即可表达"先装备 JDK"；结构化 JVM 工具批次落地时在其内部隐式 ensure，双保险）。

### 前端

无专门 UI。Low 风险确认卡片（现有 ConfirmCard）显示"准备在目标环境下载 ~200MB JDK"；装备过程经 ProvisionProgress 事件在 ToolCallCard 展示阶段。SettingsDialog 加 Artifactory base URL 文本输入。

## 8. 错误处理与日志

| 阶段 | 错误 | 处理 |
|---|---|---|
| 探测 | `java -version` 失败 / java_bin 不存在 | `probe_failed` + 建议先 run_command 确认 java 路径 |
| 解析 | BiSheng 串不匹配 | `parse_failed` + 原始输出 + 建议 fallback（run_command 手动处理） |
| 解析 | 非 BiSheng 环境 | `unsupported_vendor`（本期仅支持 BiSheng） |
| 下载 A | 无 curl/wget、网络不通、超时 | 自动降级通道 B（warn 日志），不打扰 agent |
| 下载 B | 本地下载失败 | `provision_failed` + URL 与 curl 错误 |
| 上传 | SFTP 失败/中断 | `provision_failed` + 清理远端半截文件（rm -f） |
| 解压 | tar 失败（磁盘满/坏包） | `provision_failed` + stage=extract + 清理残留目录 |
| 验证 | bin/jcmd 不存在 | `provision_failed` + stage=verify + 建议检查 base URL 配置 |
| 并发 | 同环境同包进行中 | 串行等待，后者缓存命中直接返回 |

- 日志遵守 [日志规范](../../../docs/architecture/logging-standard.md)：每阶段一条 info!（env_id、session_id、stage、URL、elapsed），降级 warn!，失败 error!，不截断不脱敏。
- tool_calls 表照常落库（MCP 层既有逻辑）。

## 9. 测试

对齐 run_command.rs 的测试风格：

- **纯函数单测（大头）**：BiSheng 版本串解析（标准格式、_ 与空格边界、arch 映射、URL 拼接含 URL encode、畸形串报错）、阶段超时值。
- **MockChannel 集成测试**：注入 Mock ExecChannel（探测返回预置 `java -version` 输出 → 缓存命中分支返回 cached:true）；Mock upload 验证通道 B 选择。
- **SshTransport::upload**：不做真实网络单测（对齐现有 ssh.rs 实践），类型检查 + 手工验证。
- **验证命令**：`cargo test --manifest-path src-tauri/Cargo.toml`、`cargo check --manifest-path src-tauri/Cargo.toml`、`pnpm typecheck`（SettingsDialog 有改动）。

## 10. 对现有系统的影响

| 模块 | 变更 |
|---|---|
| `src-tauri/src/provision/`（新增） | package.rs / jdk.rs / transfer.rs |
| `src-tauri/src/exec/channel.rs` | ExecChannel trait 新增 `upload()` 方法 |
| `src-tauri/src/exec/ssh.rs` | SshTransport 实现 upload（russh SFTP）；测试 Mock 实现按需返回 |
| `src-tauri/src/tools/builtin/ensure_tool.rs`（新增） | MCP 工具 handler，注册进 lib.rs |
| `src-tauri/src/agent/prompt.rs` | 工具使用 section 新增 ensure_tool 引导 |
| `src-tauri/src/infra/paths.rs` | 新增 `cache_dir()` + ensure_dirs 覆盖 |
| `src-tauri/src/app/` + 前端 | Artifactory base URL 设置项（SQLite 设置表 + SettingsDialog 输入框 + ipc.ts 绑定） |
| `src-tauri/src/app/events.rs` | AppEvent 新增 ProvisionProgress 变体 |
| DB migration | 新增全局设置表（key-value） |

## 11. 明确不做（YAGNI）

- checksum 校验（内网可信源）
- 字节级下载进度
- 多 JDK 版本并存管理（按 OpenJDK 版本目录名天然隔离，够用）
- /tmp/friday-tools 清理策略（tmp 重启自动清，交给环境自身）
- Artifactory 目录列表 API 探测
- 非 BiSheng JDK 供应商支持（报 unsupported_vendor）
- SFTP download（artifacts 回拉是后续独立特性，仅留 trait 扩展点）
