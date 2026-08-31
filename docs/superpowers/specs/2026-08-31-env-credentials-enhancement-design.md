# 环境管理功能增强设计（多凭证录入 + 默认用户指定 + 凭证编辑）

日期：2026-08-31
状态：已评审通过（方案 B，全文各节均获用户确认）

## 背景与目标

Friday 的环境弹窗（新增/编辑共用）存在三个问题：

1. **新增环境时无法录入多用户凭证**：多用户凭证区仅在编辑模式渲染，新增后必须再打开编辑弹窗补录。
2. **新增环境时无法指定默认登录用户**：默认凭证固定为主表单填写的用户，录入阶段不能切换（要保存后再编辑）。
3. **编辑弹窗凭证添加表单存在 CSS bug**：认证方式下拉框 class 同时含 `w-full`（来自 `inputCls`）与 `w-28`，Tailwind v4 产物中 `.w-full` 后置生效 → 下拉框占满整行，用户名输入框被挤到 0 宽。用户看到的是"下拉框左侧一块不可输入的空白"，且无法输入用户名，点"添加凭证"必然报"用户名不能为空"。

### 已确认的决策

| 决策点 | 结论 |
|---|---|
| UI 形态 | **方案 B：统一凭证列表**。主表单只留名称/主机/端口；所有用户进凭证列表，★ 标默认、点星切换 |
| 新增/编辑形态 | 完全同构（编辑模式移除主表单的用户/认证/密码四字段） |
| 测试连接 | 凭证粒度：对列表中某个凭证发起测试 |
| 凭证编辑 | 支持（改密码/私钥路径/认证方式） |
| 编辑模式提交 | 所有凭证变更本地暂存，点保存一并提交（与新增一致） |
| 后端接口 | **单个原子命令 `save_environment_cmd` 覆盖新增+编辑**，入参携带全量凭证 |
| 凭证编辑实现 | 新增 `update` 逻辑并入 save 命令（不是前端删了重加） |
| 下拉框 bug | 随重构自然解决（凭证表单整体重写，冲突 class 消失） |

## UI 设计

弹窗结构（新增/编辑同一个表单）：

```
新增/编辑环境
├─ 主表单：名称 / 主机 / 端口
├─ 凭证列表（★ = 默认登录用户，点星切换）
│   ├─ ★ opc · 私钥 · ~/.ssh/id_ed25519        [测试] [编辑] [移除]
│   └─ ☆ svcapp · 密码                          [测试] [设为默认] [编辑] [移除]
├─ 添加凭证行：用户名 / 认证方式▾ / 私钥路径 / 密码-口令 / [添加]
├─ 测试连接结果展示（现有样式复用）
└─ [取消] [保存]
```

要点：

- 主表单移除用户名/认证方式/私钥路径/密码四字段；`environments` 表列不动（仍作镜像）。
- 默认凭证 = 日常 SSH 连接用户（连接池/run_command/jvm_*/传输/隧道均走默认凭证），星标即指定，新增时当场可切。
- **凭证暂存区（本地状态）**：
  - 新增模式：空列表起步，逐条添加/移除/编辑/切默认。
  - 编辑模式：打开弹窗时 `list_env_credentials_cmd` 加载现有凭证进暂存区，之后所有变更（增/删/改/设默认）都只改本地状态，点保存才提交。
  - 添加/编辑凭证行内表单：用户名、认证方式（密码/私钥）、私钥路径（私钥时）、密码或口令（编辑已有凭证时留空 = 不修改）。
- **测试连接（凭证粒度）**：凭证列表每行一个测试入口。新增模式用表单暂存参数直接测；编辑模式改过的字段用表单值、密码留空读 keychain 已存值；新加凭证密码留空时报错提示填写，不做猜测回退。复用现有 `test_connection_params_cmd`，不新增命令。
- **未保存确认**：编辑模式下有暂存变更时关弹窗，弹确认（"有未保存的凭证变更，确定放弃？"）。新增模式同理。

## 数据流与后端命令

### 核心命令：`save_environment_cmd`（新增，覆盖新增+编辑）

```
save_environment_cmd {
  environment_id: Option<String>,   // None = 新增环境
  name: String,
  host: String,
  port: Option<u16>,
  credentials: Vec<CredentialInput> // 全量凭证列表
}

CredentialInput {
  id: Option<String>,        // None = 新增的凭证
  username: String,
  auth_type: "private_key" | "password",
  private_key_path: Option<String>,
  secret: Option<String>,    // None/空 = 不修改已有 secret；新凭证按需可空（私钥无口令）
  is_default: bool           // 恰好一条为 true
}
```

- 被移除的凭证不出现在 `credentials` 里，后端 diff 得出删除集。

### 后端处理流程

1. **Diff**：入参凭证集 vs 现有 `env_credentials` 行 → 新增集 / 更新集 / 删除集。
2. **校验**：
   - 名称/host 非空、端口合法、名称查重（排除自身）；
   - 用户名非空、列表内不重复、与保留行不冲突；
   - 认证方式合法；`private_key` 必须有路径；
   - **恰好一个默认凭证；`credentials` 至少 1 条**。
3. **DB 事务**：upsert `environments` 行 + 增/删/改 `env_credentials` 行（含 `is_default` 标记翻转）。
4. **Keychain**（事务提交后执行，沿用 `add_credential` 的"写失败回滚 DB"模式）：
   - 新增凭证：写 `friday/env/{env_id}/cred/{cred_id}`；
   - 删除凭证：删对应条目；
   - 修改凭证：secret 非空则覆盖；认证方式切换时清旧条目再写新（沿用 `should_clear_secret_on_update` 语义）；secret 留空且认证未切换 = 不动。
   - 任一 keychain 操作失败 → 删除补偿已写条目 + 回滚 DB 事务，整体报错。
5. **镜像同步**：默认凭证的 `username`/`auth_type`/`private_key_path` 镜像到 `environments` 三列（现有语义，旧路径消费者保持一致）。
6. **连接失效**：保存成功后断开该环境的池化连接与专用连接（复用 `delete_environment_cmd` 中 `disconnect_for_env` 的既有逻辑），下次使用按新凭证重连。
7. **返回**：保存后的 `EnvironmentRow` + `Vec<EnvCredentialRow>`，前端刷新 envStore 与弹窗状态。

### 命令增删

- **新增**：`save_environment_cmd`。
- **保留**：`list_env_credentials_cmd`（编辑弹窗加载）、`test_connection_params_cmd`。
- **删除**：`add_environment_cmd`、`update_environment_cmd`、`add_env_credential_cmd`、`delete_env_credential_cmd`、`set_default_env_credential_cmd`（弹窗是唯一调用方，保留两套入口容易漂移）。
- 前端 `src/lib/ipc.ts` 同步：新增 `saveEnvironment`，删除上述五个绑定。
- `migrate_legacy`（启动时旧环境迁移）不受影响，保留。

## 错误处理

- **校验错误**：前端保存前即时校验（具体字段提示）+ 后端命令内再校验（双保险）。
- **Keychain 写失败**：整体回滚（DB 事务回滚 + 已写 keychain 条目补偿删除），弹窗报错不关闭，用户可重试。
- **保存中途 IPC 失败**：DB 事务保证无半套凭证；keychain 残留无引用条目无害，重试覆盖。
- **放弃编辑**：有暂存变更时关弹窗弹确认，避免误触丢变更。

## 测试策略

Rust 单元测试（`env_credentials.rs` / `environments.rs` 现有测试旁）：

- 新增路径：env + 多凭证 + 恰好一个默认 + 镜像三列 + keychain 写入；
- 编辑路径 diff：新增集/更新集/删除集正确；默认切换翻转 `is_default` 并镜像；
- 校验分支：重复用户名、零凭证、多默认、私钥缺路径、名称查重；
- secret 语义：留空不改、非空覆盖、认证切换清旧条目；
- keychain 写失败 → DB 回滚无残留。

前端：`pnpm typecheck`；手动验证新增/编辑/逐凭证测试连接/放弃确认四条流。

回归确认：编辑保存后旧连接断开、下次按新凭证重连。

## 不做的事（YAGNI）

- 不改 `env_credentials` 表结构与 `environments` 镜像列机制；
- 不做凭证用户名修改（编辑凭证改认证/secret/路径，用户名错了移除重加）；
- 不做批量导入凭证 / 凭证备注 / 有效期等扩展字段；
- 不做连接池外的凭证引用（arthas attach 用户对齐仍按用户名查非默认凭证，行为不变）。
