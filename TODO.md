# TODO

规划中的功能清单，按落地阶段排序。来源：[知识库与工具库伞形总纲设计](docs/superpowers/specs/2026-08-26-knowledge-tool-umbrella-design.md)。每阶段开工前先写子 spec（docs/superpowers/specs/），完成一项勾一项。

## 阶段 1：SSH 通道 + run_command（从演示品变产品）

- [x] SSH Transport 真实现（russh 替换 `exec/ssh.rs` 占位）
- [x] 删除 `exec/k8s.rs`（K8s 场景经 SSH 执行 kubectl，不做 K8s API transport）
- [x] run_command 工具：`{ command, timeout_secs? }`，风险级 High，走现有确认拦截
- [x] SSH 凭证接入现有 credential 模块（私钥引用 `~/.ssh/`，密码走 OS 密钥链）
- [x] 连接失败重试 2 次 / 中断重连 1 次（overview.md 既有约定）
- [x] Environment 管理 UI/CRUD（session 关联目标环境，`environment_id` 列已就位）

> 实现备注（与原条目的偏差，见 [阶段 1 spec](docs/superpowers/specs/2026-08-26-phase1-ssh-run-command-design.md)）：环境与会话解耦——连接按 environment_id 池化而非 session 关联；agent 通过 `list_environments` 工具自主发现环境，`run_command` 以 environment 参数指定目标。

## 阶段 2：Playbook 存储 + 注入（知识库卖点上线）

依赖阶段 1（内置 playbook 引用 run_command）。

- [ ] `playbooks` 表 + `playbooks_vec` 向量表（sqlite-vec，与经验库同构）
- [ ] playbook 数据模型：三层（steps 主干 + anti_patterns/reference_notes 旁注 + source_content 原文附件）
- [ ] 审核状态机：draft → active → disabled；内置装载即 active，按 id 幂等升级，用户改过的副本跳过
- [ ] `get_playbook(id)` MCP 工具（替换原 `get_playbook(symptom)` 设计）
- [ ] 首次 spawn 语义检索注入摘要（top-3，余弦相似度 ≥ 0.5），置于经验 section 之前
- [ ] 首批内置 playbook：Java 高频故障路径（OOM / CPU 飙高 / GC 频繁 / 死锁等，清单在子 spec 定稿）
- [ ] 经验沉淀收敛：同一四元组 `occurrence_count ≥ 3` 生成 playbook 草稿；审核通过后经验标 `converged` 不再注入

## 阶段 3：导入管线 + 审核面板（团队知识灌入）

依赖阶段 2。

- [ ] 导入源：URL / markdown / docx 抓取文本
- [ ] LLM 提炼管线（映射规则见伞形总纲 §5.4：动作→steps、弯路→anti_patterns、机制→reference_notes、全文→source_content）
- [ ] 提炼产物入 draft，不污染 active
- [ ] 审核 UI：draft 列表 + 原文/提炼左右对照 + 通过 / 驳回 / 编辑后通过
- [ ] playbook 管理 UI：列表、启停、删除
- [ ] YAML 导入/导出（交换格式）

## 阶段 4：脚本工具热插拔（工具库开放性上线）

依赖阶段 1（remote 脚本），可与阶段 2/3 并行。

- [ ] ToolRegistry 动态化：`HashMap` + 不可变 `Arc` → `RwLock`，运行时注册/注销
- [ ] 脚本工具形态：`<tools_dir>/<name>/manifest.toml + 脚本`（name/description/input_schema/risk_level/exec_location/script）
- [ ] manifest 加载校验（风险级非法值拒绝装载；引用完整性）
- [ ] 脚本执行：remote（经 SSH）/ local（本机）；参数 JSON 注入环境变量或 stdin
- [ ] agent 自服务注册：写脚本 + 清单后无需重启接入

## 后续批次（阶段 4 之后）

- [ ] 结构化 JVM 工具：jvm_gc_stats（jstat）、jcmd、arthas、read_log、read_dump——逐步替换 playbook 中的 run_command 命令模板
- [ ] jvm_* 工具接入用户对齐/多凭证能力（pre-flight 用户检查 + 按 jvm_user 查凭证的通用工具函数，arthas 批次已落地基础设施，见 [arthas 对接设计](docs/superpowers/specs/2026-08-31-arthas-mcp-integration-design.md)）
- [ ] 爬虫管线（摄入管线抽象为 `KnowledgeIngest` trait，前两条管线先按 trait 落地）
- [ ] K8s 诊断 playbook 内容（SSH 上跑 kubectl 的知识条目，无新机制）
- [ ] 经验时间衰减 / playbook 使用效果反馈（视用户反馈）

## 已知技术债（顺手修）

- [ ] 工具事件去重（MCP Server 与 opencode stdout 双发 ToolExecuting/ToolResult，v1 有意保留）
- [ ] Agent 停止时 in-flight 工具调用的取消（长时间诊断命令需要 per-session cancel）
- [ ] opencode 配置 JSONC 注释丢失（可换 `jsonc-parser` crate）
