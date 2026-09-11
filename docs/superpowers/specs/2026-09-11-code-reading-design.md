# 代码读取能力（code_* 工具集）设计

日期：2026-09-11
状态：已评审通过

## 背景

Friday 的运行时诊断工具已齐备（jvm_* / heap_* / jfr_* / arthas_*），但诊断链路止步于运行时数据：线程栈帧只有类名+行号、heap dump 泄漏对象只有类型、jad 反编译只有字节码回推——看不到源码，根因定责到不了「哪行代码写的」。补上读代码能力，诊断闭环补全最后一块拼图。

### 用户确认的关键约束

1. **代码来源**：git 仓 URL，Friday 在开发者本机 clone，不读目标环境（部署环境通常只有 jar 没有 source）
2. **认证**：公司强制 HTTPS 访问 git 仓；凭证依赖本机 git 配置（credential manager / .netrc / URL 内嵌 token），Friday 不管理 Git 凭证
3. **clone 策略**：完整克隆（保留全部历史，git log / blame 可用）
4. **问询交互**：agent 对话中自主问询；**仓地址按服务名记住（权威），分支每次与用户确认（上次值仅作提示）**
5. **ref 泛化**：ref 接受分支 / tag / commitid；agent 发现代码与部署不匹配时主动询问用户 commitid 精确对齐
6. **工具范围**：读 / 搜 / 列三件套
7. **映射管理**：设置弹窗加列表页（服务映射 + 缓存仓库）

## 方案选型

三个候选：**A. Friday 自建 `code_*` MCP 工具集**（推荐、已采纳）；**B. Friday 只负责 clone，读代码靠 agent CLI 原生文件工具**；**C. 远端读源码**（SSH 目标环境——部署环境无 source，已排除）。

采纳 A 的核心理由：Friday 的架构原则是工具统一 MCP 注册、统一风险分级、统一日志。读代码是诊断动作，理应与其他工具同一体系。B 方案 opencode 对项目目录外的文件访问有权限域限制、需动 agent CLI 配置、两个 provider 行为不一致，且读代码动作绕过 Friday 日志体系，诊断过程审计有盲区，长期不可控。

## 工具集

新增 `ToolCategory::Code`（第 9 组，声明序排在 Arthas 之后、FileTransfer 之前；前端 `ToolCategory` 联合类型同步加 `code`）。全部**只读自主**（不碰目标环境，无需确认），全部 `needs_channel: false`（纯本地，同 echo / get_playbook 模式，无环境也能用）。

| 工具 | 参数 | 行为 |
|------|------|------|
| `code_get_repo` | `service` | 查映射记忆 → `{repo_url, last_ref}` 或 `not_found` |
| `code_open_repo` | `repo_url`, `ref`, `service?` | 确保仓就绪（后台 clone / fetch + checkout），立即返回 `{repo_id, status}`；带 `service` 时 upsert 映射（`repo_url` 权威覆盖、`last_ref` 记本次 ref） |
| `code_repo_status` | `repo_id` | 轮询：`cloning` / `fetching` / `ready` / `failed` + 进度百分比 + `error_code` |
| `code_read_file` | `repo_id`, `path`, `offset?`, `limit?` | 读文件，输出带 1-based 行号；默认前 2000 行，分页读取 |
| `code_search` | `repo_id`, `pattern`, `glob?`, `context?`, `max_results?` | 正则搜索（ripgrep 风格：匹配行 + 上下文行 + 行号 + 文件路径），尊重 .gitignore |
| `code_list_files` | `repo_id`, `path?`, `glob?` | 列目录 / glob 匹配文件列表，上限 500 条 + 截断提示 |

关键设计点：

- **repo_id = hash(repo_url + ref)**：内容寻址，同仓同 ref 重复 open 幂等返回同一 id，agent 多轮调用无状态负担
- **ref 泛化**：`ref` 接受任何 git 可解析引用——分支名 / tag 名 / commitid（SHA）。git worktree / checkout 原生支持三者，`last_ref` 存任意 ref 字符串
- **异步 open**：完整 clone 大仓可能几分钟，`code_open_repo` 立即返回（同 jfr_record 模式），进度从 git stderr 解析（`Receiving objects: XX%`），agent 轮询 `code_repo_status`；就绪前调读工具返回 `repo_not_ready`（提示轮询 status，勿重复 open——同 (url, ref) 并发 open 任务去重）
- **栈帧回源动线**：线程栈 `com.foo.Bar.baz(Bar.java:123)` → `code_list_files(glob="**/Bar.java")` 或 `code_search("class Bar")` 定位文件 → `code_read_file` 行号直接对上

## CodeRepoManager（存储布局与生命周期）

### 磁盘布局

`Paths` 新增 `repos_dir()`（遵循文件管理约定，不散落 `.join()`）：

```
app_data_dir/repos/
  <url_hash>/                 # 一个仓一族
    clone/                    # 主 clone：完整历史（一次拉取，永久复用）
    wt/<ref_hash>/            # 每个 (repo, ref) 一个 git worktree
```

**为什么 worktree**：主 clone 共享全部 git 对象，worktree 只占工作文件（同盘硬链接），秒级创建。多会话同时 open 同仓不同分支天然隔离、互不 checkout 踩踏——git 原生为这种场景设计的机制。

### open 流程三分支

1. 主 clone 不存在 → 后台 `git clone <url> clone/`，完成后建 worktree
2. 主 clone 存在 → `git fetch origin` + `worktree add` / checkout 到 ref
3. 同 (url, ref) 的 worktree 已就绪 → 直接复用，仍做一次 fetch 刷新（分支经常变，用户报的分支名可能已前移）

### fetch 降级

- fetch 网络失败但本地已有该 ref → 用本地缓存继续，status 返回 `ready` 附 `stale_warning`（告诉 agent 提示用户代码可能不是最新）
- 本地没有该 ref → `failed`（`fetch_failed`）

### 生命周期

**不做定时回收**：worktree / clone 落盘持久、跨会话复用（重新 open 是秒级本地操作，回收再 clone 反而亏）。磁盘增长出口在设置页缓存管理（见下）。

### git 子进程

- spawn 本机 `git`，stdout / stderr 全量读取记日志（符合日志规范）；clone / fetch 进度解析 git stderr 的 `Receiving objects: XX%` 尾值
- git 依赖惰性检测：open 时检测 PATH，缺失报 `git_not_found` + 安装指引（不做启动时检测）

### URL 策略

允许 `https://`（公司强制的主形态）和 `file://`（内网裸 git 服务、测试 fixture）；`ssh://` / `git@` 明确拒绝，报错引导用户换 https 地址。

## 映射存储与设置 UI

### SQLite 映射表

新增 migration `0010_service_repos.sql`：

```sql
CREATE TABLE service_repos (
  service      TEXT PRIMARY KEY,
  repo_url     TEXT NOT NULL,
  last_ref     TEXT,          -- 仅提示用，每次仍须与用户确认
  updated_at   TEXT NOT NULL,
  last_used_at TEXT NOT NULL
);
```

- 写入：`code_open_repo` 带 `service` 时 upsert（`repo_url` 权威覆盖、`last_ref` 记本次）
- 读取：`code_get_repo(service)`，命中时刷新 `last_used_at`

### 设置弹窗「代码仓」区块

两个列表：

1. **服务映射**：服务名、仓地址、上次分支、最后使用时间 + 删除按钮
2. **缓存仓库**：仓地址、磁盘占用、分支数（worktree 数）+ 删除按钮（删整个 `<url_hash>/`，主 clone + worktrees 一起清）——完整克隆的磁盘增长出口在这里。该仓有进行中的 clone / fetch 任务时拒绝删除（报错提示稍后重试）

IPC command：`list_service_repos_cmd` / `delete_service_repo_cmd` / `list_repo_cache_cmd` / `delete_repo_cache_cmd`，前端绑定同步进 `src/lib/ipc.ts`。

## TOOL_GUIDANCE 注入

agent 行为约定（追加进 `agent/prompt.rs` 的 TOOL_GUIDANCE）：

- 需要读源码时（栈帧回源、业务逻辑确认）先 `code_get_repo(服务名)` 查记忆；有记忆 → 向用户**确认仓地址仍对 + 确认本次分支**（分支经常变，上次值仅作提示）；无记忆 → 问用户仓地址 + 分支
- 用户报的 ref 可以是分支 / tag / commitid；tag 优先于分支猜测发布版本
- **代码不匹配信号**（栈帧行号对不上、jad 反编译与源码结构不一致、code_search 找不到预期符号）→ 主动询问用户部署的 commitid（可指引从部署系统 / 镜像 label / `META-INF/MANIFEST` 的 Implementation-Build 查），拿 commitid 重新 `code_open_repo` 精确对齐部署版本
- `code_open_repo(url, ref, service)` → 轮询 `code_repo_status` 至 `ready`（`cloning` / `fetching` 稍候再查，**勿重复 open**；同 url+ref 重复调用幂等）→ `code_read_file` / `code_search` / `code_list_files`
- 线程栈类名定位：`code_list_files(glob="**/类名.java")` 或 `code_search("class 类名")`，栈帧行号直接对 `code_read_file` 的行号
- `stale_warning` → 告知用户代码可能非最新；源码与部署版本可能不一致，可用 arthas `jad` 反编译交叉验证

## 错误处理

错误码表（工具返回结构化 error，agent 可决策重试或求助用户）：

| error_code | 场景 | agent 动作建议 |
|---|---|---|
| `git_not_found` | PATH 无 git | 引导用户安装 git |
| `repo_not_ready` | 读/搜/列时仓还在 clone/fetch | 轮询 `code_repo_status`，勿重复 open |
| `clone_failed` / `fetch_failed` | 网络/权限/URL 错误 | 看 stderr 摘要；认证失败引导用户检查本机 git 凭证 |
| `invalid_ref` | 分支/tag/commitid 不存在（fetch 成功后仍解析不到） | 与用户确认 ref 拼写或换 commitid |
| `path_not_found` / `path_outside_repo` | 路径不存在 / 越界（`..` 逃逸 worktree） | 拒绝，提示合法路径 |
| `pattern_invalid` | 正则编译失败 | 修正正则 |

fetch 降级规则：fetch 失败但本地已有该 ref → `ready` + `stale_warning`；本地没有 → `fetch_failed`。

### 安全边界

- 读操作全部限定在 worktree 目录内（canonicalize + 前缀校验防 `..` 逃逸）
- 工具只读——不提供写 / 删仓文件的能力；仓删除只走设置 UI command（不经 agent）

## 测试

`cargo test`，遵循现有模块内嵌 tests 模式：

- **单测**：repo_id 稳定性、ref 解析三分支、路径逃逸拦截、行号分页边界、regex / glob 参数校验、错误码映射、`service_repos` upsert / `last_used_at` 刷新
- **集成测**：用 `file://` 裸仓 fixture（tempdir 建源仓）走通 open→ready→read / search / list 全链路、fetch 降级（本地有 ref 时断网继续）、同 (url, ref) 并发 open 去重、映射 upsert 与 get 往返
- **前端**：`pnpm typecheck` 过；设置区块沿用现有弹窗组件模式
