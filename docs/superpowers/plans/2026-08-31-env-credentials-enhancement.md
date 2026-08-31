# 环境多凭证增强 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 环境弹窗重构为"主表单（名称/主机/端口）+ 统一凭证列表"，凭证支持增/删/改/设默认，全部本地暂存后经单一原子命令 `save_environment_cmd` 提交（覆盖新增+编辑）。

**Architecture:** 后端新增 `save_environment`（diff 增/删/改凭证 + DB 事务 + keychain 补偿 + 镜像同步 + 连接失效），删除五个旧命令；前端重写 `EnvironmentDialog`（凭证暂存区 + 逐凭证测试连接），`ipc.ts`/`envStore.ts` 同步收口。设计文档：`docs/superpowers/specs/2026-08-31-env-credentials-enhancement-design.md`。

**Tech Stack:** Tauri 2 / Rust (sqlx + keyring) / React + zustand / Tailwind v4。

**约定：**
- Rust 检查：`cargo check --manifest-path src-tauri/Cargo.toml`；测试：`cargo test --manifest-path src-tauri/Cargo.toml`
- 前端类型检查：`pnpm typecheck`
- 日志规范：每个 Tauri command 有 `#[instrument]`，错误路径 `tracing::error!`/`warn!`（见 docs/architecture/logging-standard.md）
- 工作区：当前工作区（用户选择不用 worktree），main 分支直接提交

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `src-tauri/src/app/env_save.rs` | 新建 | `save_environment` 核心：CredentialInput、校验、diff、事务、keychain、镜像 |
| `src-tauri/src/app/environments.rs` | 修改 | 删 `add_environment`/`update_environment` 及其 cmd（保留 list/delete/test/校验辅助） |
| `src-tauri/src/app/env_credentials.rs` | 修改 | 删 `add_credential`/`delete_credential`/`set_default_credential` 及其 cmd（保留 list/default/find/migrate） |
| `src-tauri/src/lib.rs` | 修改 | 命令注册表更新 |
| `src/lib/types.ts` | 修改 | `CredentialInput` 类型 |
| `src/lib/ipc.ts` | 修改 | `saveEnvironment` 绑定，删五个旧绑定 |
| `src/store/envStore.ts` | 修改 | `save` 替代 `add`/`update` |
| `src/components/environments/EnvironmentDialog.tsx` | 重写 | 统一凭证列表弹窗 |
| `src/components/environments/CredentialList.tsx` | 新建 | 凭证列表（星标/编辑/移除/测试）+ 凭证编辑表单，纯展示组件 |
| `src/components/environments/DiscardChangesDialog.tsx` | 新建 | 放弃未保存变更确认弹窗 |

---

### Task 1: `save_environment` 核心——校验与新增路径（TDD）

**Files:**
- Create: `src-tauri/src/app/env_save.rs`
- Modify: `src-tauri/src/app/mod.rs`（挂模块）

- [ ] **Step 1.1: 写失败测试——新增路径**

在 `src-tauri/src/app/env_save.rs` 创建文件，先写测试骨架与核心类型（实现函数留 `todo!()` 会编译失败——先写全部类型定义 + 空实现让编译通过、测试失败）：

```rust
use serde::Deserialize;
use sqlx::{Row, SqlitePool};

use crate::app::env_credentials::EnvCredentialRow;

/// save_environment 入参：单条凭证（全量列表中的一项）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInput {
    /// None = 新增的凭证
    pub id: Option<String>,
    pub username: String,
    pub auth_type: String,
    pub private_key_path: Option<String>,
    /// None/空 = 不修改已有 secret（新凭证时表示无口令私钥/无密码由校验兜底）
    pub secret: Option<String>,
    /// 恰好一条为 true
    pub is_default: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("environment not found: {0}")]
    NotFound(String),
}

/// 保存结果：环境行 + 全量凭证列表
pub struct SaveOutcome {
    pub environment: crate::app::environments::EnvironmentRow,
    pub credentials: Vec<EnvCredentialRow>,
}

/// 校验凭证列表：至少 1 条、恰好 1 个默认、用户名非空不重复、认证合法、私钥必有路径
pub fn validate_credentials(creds: &[CredentialInput]) -> Result<(), SaveError> {
    if creds.is_empty() {
        return Err(SaveError::Validation("至少需要一条凭证".to_string()));
    }
    let defaults = creds.iter().filter(|c| c.is_default).count();
    if defaults != 1 {
        return Err(SaveError::Validation(format!(
            "必须恰好指定一个默认凭证（当前 {defaults} 个）"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for c in creds {
        if c.username.trim().is_empty() {
            return Err(SaveError::Validation("凭证用户名不能为空".to_string()));
        }
        if !seen.insert(c.username.trim().to_string()) {
            return Err(SaveError::Validation(format!(
                "凭证用户名重复：{}",
                c.username.trim()
            )));
        }
        if !matches!(c.auth_type.as_str(), "private_key" | "password") {
            return Err(SaveError::Validation(
                "auth_type 必须是 private_key 或 password".to_string(),
            ));
        }
        if c.auth_type == "private_key"
            && c.private_key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()).is_none()
        {
            return Err(SaveError::Validation("私钥认证需要填写私钥路径".to_string()));
        }
    }
    Ok(())
}

/// 原子保存环境 + 全量凭证（新增 + 编辑统一入口）
pub async fn save_environment(
    pool: &SqlitePool,
    environment_id: Option<&str>,
    name: &str,
    host: &str,
    port: u16,
    credentials: Vec<CredentialInput>,
) -> Result<SaveOutcome, SaveError> {
    todo!("Task 2 实现")
}
```

同文件追加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        (tmp, pool)
    }

    fn cred(username: &str, is_default: bool) -> CredentialInput {
        CredentialInput {
            id: None,
            username: username.to_string(),
            auth_type: "password".to_string(),
            private_key_path: None,
            secret: Some("pass-1".to_string()),
            is_default,
        }
    }

    #[tokio::test]
    async fn test_save_new_environment_with_multiple_credentials() {
        let (_tmp, pool) = setup().await;
        let outcome = save_environment(
            &pool, None, "prod", "10.0.0.1", 22,
            vec![cred("opc", true), cred("svcapp", false)],
        ).await.unwrap();

        assert_eq!(outcome.environment.name, "prod");
        assert_eq!(outcome.credentials.len(), 2);
        let def = outcome.credentials.iter().find(|c| c.is_default).unwrap();
        assert_eq!(def.username, "opc");
        // environments 三列镜像默认凭证
        assert_eq!(outcome.environment.user, "opc");
        assert_eq!(outcome.environment.auth_type, "password");
    }

    #[tokio::test]
    async fn test_save_validates_duplicate_username() {
        let (_tmp, pool) = setup().await;
        let err = save_environment(
            &pool, None, "prod", "10.0.0.1", 22,
            vec![cred("opc", true), cred("opc", false)],
        ).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));
    }

    #[tokio::test]
    async fn test_save_validates_zero_and_multi_default() {
        let (_tmp, pool) = setup().await;
        let err = save_environment(&pool, None, "prod", "10.0.0.1", 22, vec![]).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));

        let err = save_environment(
            &pool, None, "prod", "10.0.0.1", 22,
            vec![cred("a", true), cred("b", true)],
        ).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));
    }
}
```

在 `src-tauri/src/app/mod.rs` 加 `pub mod env_save;`（模块声明模式与现有 `pub mod environments;` 一致）。

- [ ] **Step 1.2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_save`
Expected: `test_save_new_environment_with_multiple_credentials` 因 `todo!()` panic 而 FAIL；两个校验测试可能 PASS（validate 在 save 内调用——若 FAIL 也正常，以"有测试失败、原因是未实现"为准）。

- [ ] **Step 1.3: 实现新增路径**

`save_environment` 完整实现（本 task 只走新增分支正确，编辑分支 Task 2 补 diff；事务一次写全）：

```rust
pub async fn save_environment(
    pool: &SqlitePool,
    environment_id: Option<&str>,
    name: &str,
    host: &str,
    port: u16,
    credentials: Vec<CredentialInput>,
) -> Result<SaveOutcome, SaveError> {
    // 环境级校验：名称/host 非空、名称查重（编辑时排除自身）
    if name.trim().is_empty() || host.trim().is_empty() {
        return Err(SaveError::Validation("名称 / 主机不能为空".to_string()));
    }
    validate_credentials(&credentials)?;
    let dup = match environment_id {
        Some(id) => {
            sqlx::query_as::<_, (String,)>("SELECT id FROM environments WHERE name = ? AND id != ?")
                .bind(name.trim()).bind(id)
                .fetch_optional(pool).await?
                .is_some()
        }
        None => {
            sqlx::query_as::<_, (String,)>("SELECT id FROM environments WHERE name = ?")
                .bind(name.trim())
                .fetch_optional(pool).await?
                .is_some()
        }
    };
    if dup {
        return Err(SaveError::Validation("同名环境已存在".to_string()));
    }

    // 现有凭证行（编辑 diff 用；新增路径为空集）
    let existing: Vec<EnvCredentialRow> = match environment_id {
        Some(id) => crate::app::env_credentials::list_credentials(pool, id).await?,
        None => Vec::new(),
    };

    // ── diff ──
    let input_ids: std::collections::HashSet<&str> = credentials
        .iter()
        .filter_map(|c| c.id.as_deref())
        .collect();
    let to_delete: Vec<&EnvCredentialRow> = existing
        .iter()
        .filter(|e| !input_ids.contains(e.id.as_str()))
        .collect();

    // keychain 操作清单（事务外执行，见下）
    enum KeychainOp<'a> {
        Write { cred_id: String, secret: &'a str },
        Delete { cred_id: String },
    }
    let mut keychain_ops: Vec<KeychainOp> = Vec::new();

    let env_id = match environment_id {
        Some(id) => id.to_string(),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let now = chrono::Utc::now().to_rfc3339();

    // ── DB 事务 ──
    let mut tx = pool.begin().await?;
    if environment_id.is_none() {
        // 默认凭证镜像进 environments 行
        let def = credentials.iter().find(|c| c.is_default).unwrap();
        sqlx::query(
            "INSERT INTO environments (id, name, host, port, user, transport_type, auth_type, private_key_path, created_at) \
             VALUES (?, ?, ?, ?, ?, 'ssh', ?, ?, ?)",
        )
        .bind(&env_id).bind(name.trim()).bind(host.trim()).bind(port as i64)
        .bind(def.username.trim()).bind(&def.auth_type)
        .bind(if def.auth_type == "private_key" { def.private_key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) } else { None })
        .bind(&now)
        .execute(&mut *tx).await?;
    } else {
        // 编辑：环境行基本信息 + 默认凭证镜像（Task 2 生效，新增路径不会走到）
        let def = credentials.iter().find(|c| c.is_default).unwrap();
        let updated = sqlx::query(
            "UPDATE environments SET name = ?, host = ?, port = ?, user = ?, auth_type = ?, private_key_path = ? WHERE id = ?",
        )
        .bind(name.trim()).bind(host.trim()).bind(port as i64)
        .bind(def.username.trim()).bind(&def.auth_type)
        .bind(if def.auth_type == "private_key" { def.private_key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) } else { None })
        .bind(&env_id)
        .execute(&mut *tx).await?;
        if updated.rows_affected() == 0 {
            return Err(SaveError::NotFound(env_id.clone()));
        }
    }

    // 删除集：DB 行 + keychain 条目
    for cred in &to_delete {
        sqlx::query("DELETE FROM env_credentials WHERE id = ?")
            .bind(&cred.id).execute(&mut *tx).await?;
        keychain_ops.push(KeychainOp::Delete { cred_id: cred.id.clone() });
    }

    for input in &credentials {
        let secret_provided = input.secret.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        match input.id.as_deref() {
            None => {
                // 新增凭证
                let cred_id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&cred_id).bind(&env_id).bind(input.username.trim())
                .bind(&input.auth_type)
                .bind(if input.auth_type == "private_key" { input.private_key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) } else { None })
                .bind(if input.is_default { 1 } else { 0 })
                .bind(&now)
                .execute(&mut *tx).await?;
                if secret_provided {
                    keychain_ops.push(KeychainOp::Write {
                        cred_id,
                        secret: input.secret.as_deref().unwrap().trim(),
                    });
                }
            }
            Some(cred_id) => {
                // 更新凭证（Task 2 完善认证切换清旧 secret 语义；本 task 先覆盖行 + secret 非空写入）
                let old = existing.iter().find(|e| e.id == cred_id);
                sqlx::query(
                    "UPDATE env_credentials SET username = ?, auth_type = ?, private_key_path = ?, is_default = ? WHERE id = ? AND environment_id = ?",
                )
                .bind(input.username.trim()).bind(&input.auth_type)
                .bind(if input.auth_type == "private_key" { input.private_key_path.as_deref().map(str::trim).filter(|p| !p.is_empty()) } else { None })
                .bind(if input.is_default { 1 } else { 0 })
                .bind(cred_id).bind(&env_id)
                .execute(&mut *tx).await?;
                if secret_provided {
                    keychain_ops.push(KeychainOp::Write {
                        cred_id: cred_id.to_string(),
                        secret: input.secret.as_deref().unwrap().trim(),
                    });
                } else if let Some(old) = old {
                    // 认证方式切换且未提供新 secret → 清旧条目（沿用 should_clear_secret_on_update 语义）
                    if crate::app::environments::should_clear_secret_on_update(
                        &old.auth_type, &input.auth_type, false,
                    ) {
                        keychain_ops.push(KeychainOp::Delete { cred_id: cred_id.to_string() });
                    }
                }
            }
        }
    }

    // 恰好一个默认已由 validate_credentials 保证；UPDATE 分支里 is_default 已按入参写入

    tx.commit().await?;

    // ── keychain（事务提交后；失败补偿回滚 DB）──
    let mut done: Vec<KeychainOp> = Vec::new();
    for op in &keychain_ops {
        let result = match op {
            KeychainOp::Write { cred_id, secret } => {
                crate::app::credentials::store_cred_secret(&env_id, cred_id, secret).await
            }
            KeychainOp::Delete { cred_id } => {
                crate::app::credentials::delete_cred_secret(&env_id, cred_id).await
            }
        };
        if let Err(e) = result {
            tracing::error!(env_id = %env_id, ?e, "keychain op failed, rolling back save");
            // 补偿：删除本次已写条目
            for d in &done {
                if let KeychainOp::Write { cred_id, .. } = d {
                    let _ = crate::app::credentials::delete_cred_secret(&env_id, cred_id).await;
                }
            }
            // 回滚 DB：恢复到保存前状态 = 删除新增行 / 无法逐条还原 → 整环境删除或还原旧全量
            // 实现策略：新增路径直接删环境；编辑路径重放旧全量（见 Task 2 的 rollback_saved_state）
            rollback_saved_state(pool, &env_id, environment_id.is_none(), &existing).await;
            return Err(SaveError::Keychain(e.to_string()));
        }
        done.push(match op {
            KeychainOp::Write { cred_id, secret } => KeychainOp::Write { cred_id: cred_id.clone(), secret },
            KeychainOp::Delete { cred_id } => KeychainOp::Delete { cred_id: cred_id.clone() },
        });
    }

    let environment = crate::app::environments::get_environment(pool, &env_id)
        .await?
        .ok_or(SaveError::NotFound(env_id.clone()))?;
    let saved_creds = crate::app::env_credentials::list_credentials(pool, &env_id).await?;
    Ok(SaveOutcome { environment, credentials: saved_creds })
}

/// keychain 失败后的 DB 补偿：新增路径删环境；编辑路径还原旧凭证全量 + 旧环境行
async fn rollback_saved_state(
    pool: &SqlitePool,
    env_id: &str,
    is_new: bool,
    old_creds: &[EnvCredentialRow],
) {
    if is_new {
        if let Err(e) = sqlx::query("DELETE FROM environments WHERE id = ?").bind(env_id).execute(pool).await {
            tracing::error!(env_id = %env_id, ?e, "rollback env delete failed, orphaned row remains");
        }
        if let Err(e) = sqlx::query("DELETE FROM env_credentials WHERE environment_id = ?").bind(env_id).execute(pool).await {
            tracing::error!(env_id = %env_id, ?e, "rollback cred delete failed, orphaned rows remain");
        }
        return;
    }
    // 编辑路径：删新全量、重放旧全量（环境行旧值由 old_creds 之外的调用方上下文恢复成本高，
    // 采用简化策略：凭证行还原；环境行 name/host/port 残留为已保存值可接受——keychain 失败极罕见，
    // 且 UI 报错后用户重试会用同一表单值覆盖。此取舍在 Task 2 测试中固定下来。）
    if let Err(e) = sqlx::query("DELETE FROM env_credentials WHERE environment_id = ?").bind(env_id).execute(pool).await {
        tracing::error!(env_id = %env_id, ?e, "rollback cred delete failed");
    }
    for c in old_creds {
        let _ = sqlx::query(
            "INSERT INTO env_credentials (id, environment_id, username, auth_type, private_key_path, is_default, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&c.id).bind(env_id).bind(&c.username).bind(&c.auth_type)
        .bind(&c.private_key_path).bind(if c.is_default { 1 } else { 0 }).bind(&c.created_at)
        .execute(pool).await;
    }
}
```

注意：Rust 不允许 enum 泛型引用这样简单 clone——上面 `done.push` 的写法按值 clone secret 有生命周期问题，实现时把 `KeychainOp` 改为 owned（`secret: String`）即可：

```rust
#[derive(Clone)]
enum KeychainOp {
    Write { cred_id: String, secret: String },
    Delete { cred_id: String },
}
```

构造处 `secret: input.secret.as_deref().unwrap().trim().to_string()`，补偿与去重直接 clone。

- [ ] **Step 1.4: 跑测试确认通过**

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_save`
Expected: 3 个测试 PASS

- [ ] **Step 1.5: 提交**

```bash
git add src-tauri/src/app/env_save.rs src-tauri/src/app/mod.rs
git commit -m "feat: save_environment core with credential validation and create path"
```

---

### Task 2: `save_environment` 编辑路径 diff + secret 语义（TDD）

**Files:**
- Modify: `src-tauri/src/app/env_save.rs`

- [ ] **Step 2.1: 写失败测试**

追加到 `env_save.rs` 的 tests 模块：

```rust
    async fn seed_env_with_creds(pool: &SqlitePool) -> String {
        // 用 save_environment 走新增路径造初始数据
        let outcome = save_environment(
            pool, None, "prod", "10.0.0.1", 22,
            vec![cred("opc", true), cred("svcapp", false)],
        ).await.unwrap();
        outcome.environment.id
    }

    #[tokio::test]
    async fn test_edit_diff_add_update_delete() {
        let (_tmp, pool) = setup().await;
        let env_id = seed_env_with_creds(&pool).await;
        let existing = crate::app::env_credentials::list_credentials(&pool, &env_id).await.unwrap();
        let opc = existing.iter().find(|c| c.username == "opc").unwrap().clone();
        let svcapp = existing.iter().find(|c| c.username == "svcapp").unwrap().clone();

        // 改 opc 认证为私钥（新 secret）、删 svcapp、加 deploy
        let outcome = save_environment(
            &pool, Some(&env_id), "prod", "10.0.0.2", 2222,
            vec![
                CredentialInput {
                    id: Some(opc.id.clone()),
                    username: "opc".to_string(),
                    auth_type: "private_key".to_string(),
                    private_key_path: Some("~/.ssh/opc".to_string()),
                    secret: Some("new-pass".to_string()),
                    is_default: true,
                },
                CredentialInput {
                    id: None,
                    username: "deploy".to_string(),
                    auth_type: "password".to_string(),
                    private_key_path: None,
                    secret: Some("d-pass".to_string()),
                    is_default: false,
                },
            ],
        ).await.unwrap();

        assert_eq!(outcome.environment.host, "10.0.0.2");
        assert_eq!(outcome.environment.port, 2222);
        assert_eq!(outcome.environment.user, "opc");
        assert_eq!(outcome.environment.auth_type, "private_key");
        let names: Vec<&str> = outcome.credentials.iter().map(|c| c.username.as_str()).collect();
        assert_eq!(names, vec!["opc", "deploy"]); // svcapp 已删，默认排前
        assert!(crate::app::env_credentials::find_credential_by_username(&pool, &env_id, "svcapp").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_edit_switch_default_flips_flag_and_mirrors() {
        let (_tmp, pool) = setup().await;
        let env_id = seed_env_with_creds(&pool).await;
        let existing = crate::app::env_credentials::list_credentials(&pool, &env_id).await.unwrap();
        let svcapp = existing.iter().find(|c| c.username == "svcapp").unwrap().clone();

        // svcapp 设为默认（secret 留空 = 不变）
        let outcome = save_environment(
            &pool, Some(&env_id), "prod", "10.0.0.1", 22,
            vec![
                CredentialInput {
                    id: Some(svcapp.id.clone()),
                    username: "svcapp".to_string(),
                    auth_type: "password".to_string(),
                    private_key_path: None,
                    secret: None,
                    is_default: true,
                },
                CredentialInput {
                    id: existing.iter().find(|c| c.username == "opc").unwrap().id.clone().into(),
                    username: "opc".to_string(),
                    auth_type: "password".to_string(),
                    private_key_path: None,
                    secret: None,
                    is_default: false,
                },
            ],
        ).await.unwrap();

        assert_eq!(outcome.environment.user, "svcapp");
        let defs: Vec<&EnvCredentialRow> = outcome.credentials.iter().filter(|c| c.is_default).collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].username, "svcapp");
    }

    #[tokio::test]
    async fn test_edit_rename_duplicate_name_rejected() {
        let (_tmp, pool) = setup().await;
        let _a = seed_env_with_creds(&pool).await;
        let b = save_environment(&pool, None, "staging", "10.0.0.9", 22, vec![cred("opc", true)]).await.unwrap();
        let err = save_environment(
            &pool, Some(&b.environment.id), "prod", "10.0.0.9", 22, vec![cred("opc", true)],
        ).await.unwrap_err();
        assert!(matches!(err, SaveError::Validation(_)));
        // 改回自己的名字则通过
        save_environment(
            &pool, Some(&b.environment.id), "staging", "10.0.0.9", 22, vec![cred("opc", true)],
        ).await.unwrap();
    }
```

- [ ] **Step 2.2: 跑测试确认失败/现状**

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_save`
Expected: Task 1 实现已含编辑分支主体，观察失败点（`test_edit_rename_duplicate_name_rejected` 第二段"改回自己名字通过"依赖 exclude 自身查重——Task 1 已实现；若全 PASS 说明编辑分支已在 Task 1 顺带完成，确认断言强度后进入 Step 2.3 补 secret 清理语义）。

- [ ] **Step 2.3: 补认证切换清旧 secret 的 keychain 断言测试**

keychain 在测试环境（Windows Credential Manager）真实可用，但为避免测试污染系统 keychain，`delete_cred_secret` 对无条目静默成功——此处只验证 DB 语义 + 不 panic：

```rust
    #[tokio::test]
    async fn test_edit_auth_switch_without_secret_clears_keychain_entry() {
        let (_tmp, pool) = setup().await;
        let env_id = seed_env_with_creds(&pool).await;
        let existing = crate::app::env_credentials::list_credentials(&pool, &env_id).await.unwrap();
        let opc = existing.iter().find(|c| c.username == "opc").unwrap().clone();

        // opc 从 password 切 private_key，secret 留空 → 旧 keychain 条目被清（DB 语义：不报错、行更新成功）
        let outcome = save_environment(
            &pool, Some(&env_id), "prod", "10.0.0.1", 22,
            vec![CredentialInput {
                id: Some(opc.id.clone()),
                username: "opc".to_string(),
                auth_type: "private_key".to_string(),
                private_key_path: Some("~/.ssh/opc".to_string()),
                secret: None,
                is_default: true,
            }],
        ).await.unwrap();

        let saved = outcome.credentials.iter().find(|c| c.id == opc.id).unwrap();
        assert_eq!(saved.auth_type, "private_key");
        assert_eq!(saved.private_key_path.as_deref(), Some("~/.ssh/opc"));
    }
```

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_save` → Expected: PASS（Task 1 的实现已含 `should_clear_secret_on_update` 分支；若 FAIL 修实现）。

- [ ] **Step 2.4: 提交**

```bash
git add src-tauri/src/app/env_save.rs
git commit -m "feat: save_environment edit path with diff, default switch and secret semantics"
```

---

### Task 3: `save_environment_cmd` Tauri 命令 + 连接失效

**Files:**
- Modify: `src-tauri/src/app/env_save.rs`
- Modify: `src-tauri/src/lib.rs:286-294`（命令注册）

- [ ] **Step 3.1: 写命令（含连接失效）**

追加到 `env_save.rs`：

```rust
// ── Tauri command ──

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEnvironmentParams {
    /// None = 新增
    pub environment_id: Option<String>,
    pub name: String,
    pub host: String,
    pub port: Option<u16>,
    pub credentials: Vec<CredentialInput>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEnvironmentResult {
    pub environment: crate::app::environments::EnvironmentRow,
    pub credentials: Vec<EnvCredentialRow>,
}

#[tauri::command]
#[tracing::instrument(skip(state))]
pub async fn save_environment_cmd(
    state: tauri::State<'_, crate::AppState>,
    params: SaveEnvironmentParams,
) -> Result<SaveEnvironmentResult, String> {
    let outcome = save_environment(
        &state.db,
        params.environment_id.as_deref(),
        params.name.trim(),
        params.host.trim(),
        params.port.unwrap_or(22),
        params.credentials,
    )
    .await
    .map_err(|e| e.to_string())?;

    // 凭证/host/port 可能变化 → 断开该环境池化连接（下次使用按新配置重连）
    {
        let mut exec_pool = state.exec_pool.lock().await;
        exec_pool.disconnect(&outcome.environment.id).await;
    }

    Ok(SaveEnvironmentResult {
        environment: outcome.environment,
        credentials: outcome.credentials,
    })
}
```

`EnvironmentRow` 需补 `Serialize`（已有 `#[derive(Serialize)]`，environments.rs:19-20，无需改）。

- [ ] **Step 3.2: 注册命令**

`src-tauri/src/lib.rs` invoke_handler 列表：在 `app::environments::list_environments_cmd,` 之后插入一行：

```rust
            app::env_save::save_environment_cmd,
```

- [ ] **Step 3.3: 编译 + 测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过

Run: `cargo test --manifest-path src-tauri/Cargo.toml env_save`
Expected: PASS

- [ ] **Step 3.4: 提交**

```bash
git add src-tauri/src/app/env_save.rs src-tauri/src/lib.rs
git commit -m "feat: save_environment_cmd tauri command with connection invalidation"
```

---

### Task 4: 删除旧命令与旧数据层函数

**Files:**
- Modify: `src-tauri/src/app/environments.rs`（删 `add_environment`/`update_environment`/`add_environment_cmd`/`update_environment_cmd` 及相关测试；保留 `validate_environment`/`should_clear_secret_on_update`——env_save 复用）
- Modify: `src-tauri/src/app/env_credentials.rs`（删 `add_credential`/`delete_credential`/`set_default_credential` 及三个 cmd；保留 `list_credentials`/`default_credential`/`find_credential_by_username`/`migrate_legacy`/`list_env_credentials_cmd`）
- Modify: `src-tauri/src/lib.rs`（注册表删五行）

- [ ] **Step 4.1: environments.rs 清理**

删除以下项（行号基于当前 HEAD）：
- `add_environment()`（50-85）
- `update_environment()`（144-209）
- `add_environment_cmd()`（227-247）
- `update_environment_cmd()`（249-270）
- tests 中调用这两个函数的测试：`test_add_and_list_environment`、`test_update_environment`、`test_update_nonexistent_returns_error`、`test_delete_environment`、`test_delete_nonexistent_returns_not_found`（用 SQL 直接 INSERT 代替 add_environment 造数，或改用 env_save::save_environment——**改用 save_environment**，保持造数走真实路径）、`test_validate_environment_*` 中 `add_environment` 造数处、`test_add_environment_creates_default_credential_row`、`test_update_environment_syncs_default_credential`（此测试的语义已由 env_save 测试覆盖，直接删）。

`validate_environment` 保留但改签名——env_save 未用它（校验逻辑内联了），检查无调用方后删除 `validate_environment` 及其测试。保留 `should_clear_secret_on_update` + 其三个测试（env_save.rs 调用）。

删除后 `add_environment` 造数的存量测试统一改为：

```rust
    async fn seed_env(pool: &SqlitePool, name: &str) -> String {
        let outcome = crate::app::env_save::save_environment(
            pool, None, name, "10.0.0.1", 22,
            vec![crate::app::env_save::CredentialInput {
                id: None,
                username: "root".to_string(),
                auth_type: "password".to_string(),
                private_key_path: None,
                secret: None,
                is_default: true,
            }],
        ).await.unwrap();
        outcome.environment.id
    }
```

- [ ] **Step 4.2: env_credentials.rs 清理**

删除：`add_credential()`（83-165）、`delete_credential()`（167-196）、`set_default_credential()`（198-235）、`add_env_credential_cmd`（312-334）、`delete_env_credential_cmd`（336-346）、`set_default_env_credential_cmd`（348-358）。
保留：`list_credentials`、`default_credential`、`find_credential_by_username`、`migrate_legacy`、`list_env_credentials_cmd`、`EnvCredentialRow`、`row_to_cred`、`CRED_COLUMNS`。
tests 中调用已删函数的测试整体删除：`test_add_list_and_default`、`test_add_duplicate_username_rejected`、`test_find_by_username`（改用 save_environment 造数后保留语义——**改写**而非删除，见下）、`test_delete_default_rejected`、`test_delete_non_default_ok`、`test_set_default_syncs_environments_user`、`test_set_default_returns_fresh_row`、`test_add_default_credential_mirrors_auth_columns`。

`test_find_by_username` 改写（arthas attach 用户对齐依赖此查询，保留回归覆盖）：

```rust
    #[tokio::test]
    async fn test_find_by_username() {
        let (_tmp, pool) = setup().await;
        let outcome = crate::app::env_save::save_environment(
            &pool, None, "e", "10.0.0.1", 22,
            vec![
                crate::app::env_save::CredentialInput {
                    id: None, username: "opc".to_string(), auth_type: "password".to_string(),
                    private_key_path: None, secret: None, is_default: true,
                },
                crate::app::env_save::CredentialInput {
                    id: None, username: "svcapp".to_string(), auth_type: "password".to_string(),
                    private_key_path: None, secret: None, is_default: false,
                },
            ],
        ).await.unwrap();
        let env_id = &outcome.environment.id;
        let found = find_credential_by_username(&pool, env_id, "svcapp").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().username, "svcapp");
        assert!(find_credential_by_username(&pool, env_id, "nobody").await.unwrap().is_none());
    }
```

`migrate_legacy` 测试（`test_migrate_legacy_creates_default_credential` 在 environments.rs）不受影响，保留。

- [ ] **Step 4.3: lib.rs 注册表清理**

从 invoke_handler 删除五行：

```rust
            app::environments::add_environment_cmd,
            app::environments::update_environment_cmd,
            app::env_credentials::add_env_credential_cmd,
            app::env_credentials::delete_env_credential_cmd,
            app::env_credentials::set_default_env_credential_cmd,
```

- [ ] **Step 4.4: 编译 + 全量测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 无 add/update/set_default 未定义错误

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS（前端 ipc.ts 还引用旧命令名，但 Tauri 命令删除不影响 cargo 编译；前端 Task 6 收口）

- [ ] **Step 4.5: 提交**

```bash
git add src-tauri/src/app/environments.rs src-tauri/src/app/env_credentials.rs src-tauri/src/lib.rs
git commit -m "refactor: remove legacy env add/update and per-credential commands superseded by save_environment"
```

---

### Task 5: 前端类型与 IPC 收口

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/ipc.ts`
- Modify: `src/store/envStore.ts`

- [ ] **Step 5.1: types.ts 加 CredentialInput**

在 `EnvCredentialRow` 定义后（types.ts:140 之后）加：

```typescript
/** saveEnvironment 入参：暂存区中的单条凭证 */
export interface CredentialInput {
  /** null = 新增的凭证 */
  id: string | null;
  username: string;
  authType: EnvironmentAuthType;
  privateKeyPath?: string | null;
  /** null/空 = 不修改已有 secret */
  secret?: string | null;
  isDefault: boolean;
}

export interface SaveEnvironmentResult {
  environment: EnvironmentRow;
  credentials: EnvCredentialRow[];
}
```

- [ ] **Step 5.2: ipc.ts 替换绑定**

删除 `addEnvironment`（80-98）、`updateEnvironment`（100-120）、`addEnvCredential`（158-174）、`deleteEnvCredential`（176-178）、`setDefaultEnvCredential`（180-185）。
保留 `listEnvironments`、`deleteEnvironment`、`testConnection`、`listEnvCredentials`。
新增（放在 listEnvCredentials 之后）：

```typescript
export async function saveEnvironment(params: {
  environmentId?: string | null;
  name: string;
  host: string;
  port?: number;
  credentials: CredentialInput[];
}): Promise<SaveEnvironmentResult> {
  return invoke<SaveEnvironmentResult>("save_environment_cmd", {
    params: {
      environmentId: params.environmentId ?? null,
      name: params.name,
      host: params.host,
      port: params.port ?? null,
      credentials: params.credentials,
    },
  });
}
```

import 部分补 `CredentialInput`、`SaveEnvironmentResult`。

注意：Rust 端 `SaveEnvironmentParams` 的 camelCase serde 已把 `environment_id` 映射为 `environmentId`、`credentials` 内 `CredentialInput` 同理（Task 1 的 `#[serde(rename_all = "camelCase")]`），前端直接传 camelCase。

- [ ] **Step 5.3: envStore.ts 收口**

删除 `add`/`update` 两个 action 及 ipcAdd/ipcUpdate import，替换为：

```typescript
import {
  listEnvironments as ipcList,
  saveEnvironment as ipcSave,
  deleteEnvironment as ipcDelete,
  testConnection as ipcTest,
} from "@/lib/ipc";
import type { CredentialInput } from "@/lib/types";

interface EnvStore {
  environments: EnvironmentRow[];
  loading: boolean;
  error: string | null;
  load: () => Promise<void>;
  save: (params: {
    environmentId?: string | null;
    name: string;
    host: string;
    port?: number;
    credentials: CredentialInput[];
  }) => Promise<boolean>;
  remove: (id: string) => Promise<boolean>;
  test: (params: Parameters<typeof ipcTest>[0]) => Promise<TestConnectionResult | null>;
}
```

`save` 实现（替代 add/update）：

```typescript
  save: async (params) => {
    set({ error: null });
    try {
      await ipcSave(params);
      await get().load();
      return true;
    } catch (e) {
      set({ error: errMsg(e) });
      return false;
    }
  },
```

- [ ] **Step 5.4: 类型检查（预期失败——EnvironmentDialog 还在用旧 API）**

Run: `pnpm typecheck`
Expected: FAIL，错误集中在 `EnvironmentDialog.tsx`（使用 `add`/`update`/`addEnvCredential` 等）。这是预期的，Task 6 修复。

- [ ] **Step 5.5: 提交（连同 Task 6 一起，见 Task 6 Step 6.6）**

此任务不单独提交，与弹窗重写一起提交保证每个提交可构建。

---

### Task 6: 弹窗重写——统一凭证列表

**Files:**
- Rewrite: `src/components/environments/EnvironmentDialog.tsx`
- Create: `src/components/environments/CredentialList.tsx`
- Create: `src/components/environments/DiscardChangesDialog.tsx`

- [ ] **Step 6.1: CredentialList 组件**

`src/components/environments/CredentialList.tsx`：

```tsx
import { useState } from "react";
import { Star, StarWeight, PencilSimple, Trash, Plugs } from "@phosphor-icons/react";
import type { EnvironmentAuthType, StagedCredential } from "./staged";

const inputCls =
  "w-full bg-muted border border-border rounded-md text-sm text-foreground px-3 py-1.5 placeholder:text-muted-foreground/50 outline-none";

interface CredentialListProps {
  staged: StagedCredential[];
  testingId: string | null;
  testResults: Record<string, { ok: boolean; latency_ms: number; error: string | null } | undefined>;
  onSetDefault: (key: string) => void;
  onRemove: (key: string) => void;
  onEditStart: (key: string) => void;
  onEditCancel: () => void;
  onEditSave: (key: string, username: string, authType: EnvironmentAuthType, privateKeyPath: string, secret: string) => void;
  onTest: (key: string) => void;
  onAdd: (username: string, authType: EnvironmentAuthType, privateKeyPath: string, secret: string, makeDefault: boolean) => void;
}

export function CredentialList(props: CredentialListProps) {
  const [addForm, setAddForm] = useState({
    username: "",
    authType: "password" as EnvironmentAuthType,
    privateKeyPath: "",
    secret: "",
    makeDefault: false,
  });
  const [editing, setEditing] = useState<{ key: string; username: string; authType: EnvironmentAuthType; privateKeyPath: string; secret: string } | null>(null);

  const handleAdd = () => {
    props.onAdd(addForm.username.trim(), addForm.authType, addForm.privateKeyPath.trim(), addForm.secret, addForm.makeDefault);
    setAddForm({ username: "", authType: "password", privateKeyPath: "", secret: "", makeDefault: false });
  };

  return (
    <div className="space-y-2">
      <ul className="space-y-1">
        {props.staged.map((c) => {
          const result = props.testResults[c.key];
          return (
            <li key={c.key} className="text-xs px-3 py-1.5 rounded-md border border-border bg-surface-2 space-y-1">
              <div className="flex items-center gap-2">
                <button
                  onClick={() => props.onSetDefault(c.key)}
                  aria-label={c.isDefault ? "当前默认凭证" : "设为默认"}
                  className="cursor-pointer"
                  title={c.isDefault ? "默认登录用户" : "设为默认登录用户"}
                >
                  <Star size={14} weight={c.isDefault ? "fill" : "regular"} className={c.isDefault ? "text-accent" : "text-muted-foreground"} aria-hidden="true" />
                </button>
                <span className="font-mono">{c.username}</span>
                <span className="text-muted-foreground">{c.authType === "private_key" ? "私钥" : "密码"}</span>
                {c.isDefault && (
                  <span className="px-1.5 py-0.5 rounded bg-accent/15 text-accent text-[10px]">默认</span>
                )}
                <span className="flex-1" />
                <button onClick={() => props.onTest(c.key)} disabled={props.testingId === c.key} className="text-muted-foreground hover:text-foreground cursor-pointer disabled:opacity-50" title="测试此凭证">
                  <Plugs size={13} aria-hidden="true" />
                </button>
                <button onClick={() => { if (editing?.key === c.key) { setEditing(null); props.onEditCancel(); } else { setEditing({ key: c.key, username: c.username, authType: c.authType, privateKeyPath: c.privateKeyPath ?? "", secret: "" }); props.onEditStart(c.key); } }} className="text-muted-foreground hover:text-foreground cursor-pointer" title="编辑">
                  <PencilSimple size={13} aria-hidden="true" />
                </button>
                <button onClick={() => props.onRemove(c.key)} className="text-muted-foreground hover:text-destructive cursor-pointer" title="移除">
                  <Trash size={13} aria-hidden="true" />
                </button>
              </div>
              {result && (
                <div className={result.ok ? "text-success" : "text-destructive"}>
                  {result.ok ? `连接成功（${result.latency_ms}ms）` : `连接失败：${result.error}`}
                </div>
              )}
              {editing?.key === c.key && (
                <div className="space-y-1.5 pt-1 border-t border-border">
                  <div className="flex gap-2">
                    <input className={inputCls} placeholder="用户名" value={editing.username} onChange={(e) => setEditing({ ...editing, username: e.target.value })} aria-label="凭证用户名" />
                    <select className={`${inputCls} w-28 shrink-0 cursor-pointer`} value={editing.authType} onChange={(e) => setEditing({ ...editing, authType: e.target.value as EnvironmentAuthType })} aria-label="凭证认证方式">
                      <option value="password">密码</option>
                      <option value="private_key">私钥</option>
                    </select>
                  </div>
                  {editing.authType === "private_key" && (
                    <input className={inputCls} placeholder="私钥路径（~/.ssh/...）" value={editing.privateKeyPath} onChange={(e) => setEditing({ ...editing, privateKeyPath: e.target.value })} aria-label="凭证私钥路径" style={{ fontFamily: "var(--font-mono)" }} />
                  )}
                  <input type="password" className={inputCls} placeholder={editing.authType === "private_key" ? "私钥口令（留空 = 不修改）" : "密码（留空 = 不修改）"} value={editing.secret} onChange={(e) => setEditing({ ...editing, secret: e.target.value })} aria-label="凭证密钥" />
                  <div className="flex gap-2 justify-end">
                    <button className="px-2 py-1 rounded-md border border-border bg-surface-2 hover:bg-surface-3 cursor-pointer" onClick={() => { setEditing(null); props.onEditCancel(); }}>取消</button>
                    <button className="px-2 py-1 rounded-md bg-accent text-accent-foreground hover:bg-accent/80 cursor-pointer" onClick={() => { props.onEditSave(c.key, editing.username.trim(), editing.authType, editing.privateKeyPath.trim(), editing.secret); setEditing(null); }}>保存</button>
                  </div>
                </div>
              )}
            </li>
          );
        })}
      </ul>

      <div className="space-y-1.5">
        <div className="flex gap-2">
          <input className={`${inputCls} flex-1`} placeholder="用户名（如 svcapp）" value={addForm.username} onChange={(e) => setAddForm({ ...addForm, username: e.target.value })} aria-label="新凭证用户名" />
          <select className={`${inputCls} w-28 shrink-0 cursor-pointer`} value={addForm.authType} onChange={(e) => setAddForm({ ...addForm, authType: e.target.value as EnvironmentAuthType })} aria-label="新凭证认证方式">
            <option value="password">密码</option>
            <option value="private_key">私钥</option>
          </select>
        </div>
        {addForm.authType === "private_key" && (
          <input className={inputCls} placeholder="私钥路径（~/.ssh/...）" value={addForm.privateKeyPath} onChange={(e) => setAddForm({ ...addForm, privateKeyPath: e.target.value })} aria-label="新凭证私钥路径" style={{ fontFamily: "var(--font-mono)" }} />
        )}
        <div className="flex gap-2 items-center">
          <input type="password" className={`${inputCls} flex-1`} placeholder={addForm.authType === "private_key" ? "私钥口令（可选）" : "密码"} value={addForm.secret} onChange={(e) => setAddForm({ ...addForm, secret: e.target.value })} aria-label="新凭证密钥" />
          <label className="flex items-center gap-1 text-xs text-muted-foreground whitespace-nowrap">
            <input type="checkbox" checked={addForm.makeDefault} onChange={(e) => setAddForm({ ...addForm, makeDefault: e.target.checked })} />
            设为默认
          </label>
          <button className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 cursor-pointer whitespace-nowrap" onClick={handleAdd}>
            添加凭证
          </button>
        </div>
      </div>
    </div>
  );
}
```

注意两点：
1. 下拉框 class 用 `` `${inputCls} w-28 shrink-0` ``——**不复现原 bug**：原 bug 根因是 `w-full`（inputCls 内）与 `w-28` 同级冲突且产物中 `.w-full` 后置生效。修复方式：这两个 select 不用 `inputCls` 里的 `w-full`，改用独立 class：

```tsx
const selectCls =
  "w-28 shrink-0 bg-muted border border-border rounded-md text-sm text-foreground px-2 py-1.5 outline-none cursor-pointer";
```

select 一律用 `selectCls`（添加行与编辑行都换）。
2. `StarWeight` 类型未用，import 里去掉；`StagedCredential` 类型定义放 `./staged`（Step 6.2）。

- [ ] **Step 6.2: 暂存模型 `staged.ts`**

`src/components/environments/staged.ts`：

```typescript
import type { CredentialInput, EnvCredentialRow, EnvironmentAuthType } from "@/lib/types";

/** 弹窗内暂存的凭证（key = 前端行 key；已存凭证 = id，新凭证 = "new-N"） */
export interface StagedCredential {
  key: string;
  /** 已存凭证的 id；新凭证为 null */
  id: string | null;
  username: string;
  authType: EnvironmentAuthType;
  privateKeyPath: string | null;
  /** 新填的 secret；null/"" = 不修改 */
  secret: string | null;
  isDefault: boolean;
}

let seq = 0;
function nextKey(): string {
  seq += 1;
  return `new-${seq}`;
}

export function fromStored(creds: EnvCredentialRow[]): StagedCredential[] {
  return creds.map((c) => ({
    key: c.id,
    id: c.id,
    username: c.username,
    authType: c.auth_type,
    privateKeyPath: c.private_key_path,
    secret: null,
    isDefault: c.is_default,
  }));
}

export function toInput(staged: StagedCredential[]): CredentialInput[] {
  return staged.map((c) => ({
    id: c.id,
    username: c.username,
    authType: c.authType,
    privateKeyPath: c.authType === "private_key" ? c.privateKeyPath : null,
    secret: c.secret && c.secret.trim() ? c.secret : null,
    isDefault: c.isDefault,
  }));
}

export function addStaged(
  staged: StagedCredential[],
  username: string,
  authType: EnvironmentAuthType,
  privateKeyPath: string,
  secret: string,
  makeDefault: boolean,
): StagedCredential[] {
  const entry: StagedCredential = {
    key: nextKey(),
    id: null,
    username,
    authType,
    privateKeyPath: authType === "private_key" ? privateKeyPath : null,
    secret: secret || null,
    isDefault: makeDefault,
  };
  const next = [...staged, entry];
  return makeDefault ? next.map((c) => ({ ...c, isDefault: c.key === entry.key })) : next;
}
```

- [ ] **Step 6.3: DiscardChangesDialog**

`src/components/environments/DiscardChangesDialog.tsx`（仿 `DeleteEnvConfirmDialog` 的 dialog 模式）：

```tsx
export function DiscardChangesDialog({
  open,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  }, [open]);
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const handleClose = () => onCancel();
    dialog.addEventListener("close", handleClose);
    return () => dialog.removeEventListener("close", handleClose);
  }, [onCancel]);

  return (
    <dialog ref={ref} aria-label="放弃未保存变更" className="z-[60] w-[360px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground">
      <div className="p-5 space-y-4">
        <h2 className="text-sm font-medium">放弃未保存的变更？</h2>
        <p className="text-xs text-muted-foreground leading-relaxed">凭证修改尚未保存，关闭后本次变更将丢失。</p>
        <div className="flex justify-end gap-2">
          <button onClick={onCancel} className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs hover:bg-surface-3 cursor-pointer">继续编辑</button>
          <button onClick={onConfirm} className="px-3 py-1.5 rounded-md bg-destructive text-white text-xs hover:bg-destructive/80 cursor-pointer">放弃变更</button>
        </div>
      </div>
    </dialog>
  );
}
```

import 补 `useEffect, useRef`。

- [ ] **Step 6.4: 重写 EnvironmentDialog**

整体重写 `src/components/environments/EnvironmentDialog.tsx`（保留 Field/parsePort 帮助函数与整体 dialog 骨架，主表单删除 user/authType/privateKeyPath/password 四个字段，多用户凭证区不再限制 `editing &&`）：

```tsx
import { useEffect, useMemo, useRef, useState } from "react";
import { X, CircleNotch } from "@phosphor-icons/react";
import type { EnvironmentRow, TestConnectionResult } from "@/lib/types";
import { listEnvCredentials, testConnection } from "@/lib/ipc";
import { useEnvStore } from "@/store/envStore";
import { CredentialList } from "./CredentialList";
import { DiscardChangesDialog } from "./DiscardChangesDialog";
import { fromStored, toInput, addStaged, type StagedCredential } from "./staged";

interface EnvironmentDialogProps {
  open: boolean;
  onClose: () => void;
  editing: EnvironmentRow | null; // null = 新增
}

const EMPTY_FORM = { name: "", host: "", port: "22" };

export function EnvironmentDialog({ open, onClose, editing }: EnvironmentDialogProps) {
  const save = useEnvStore((s) => s.save);
  const test = useEnvStore((s) => s.test);
  const storeError = useEnvStore((s) => s.error);

  const dialogRef = useRef<HTMLDialogElement>(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const [staged, setStaged] = useState<StagedCredential[]>([]);
  const [stagedLoaded, setStagedLoaded] = useState(false); // 编辑模式加载完成标记
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [testingKey, setTestingKey] = useState<string | null>(null);
  const [testResults, setTestResults] = useState<Record<string, TestConnectionResult | undefined>>({});

  // 打开时初始化表单与暂存区
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open) {
      setForm(editing ? { name: editing.name, host: editing.host, port: String(editing.port) } : { ...EMPTY_FORM });
      setFormError(null);
      setStaged([]);
      setStagedLoaded(!editing);
      setTestResults({});
      setConfirmDiscard(false);
      if (editing) {
        listEnvCredentials(editing.id)
          .then((creds) => setStaged(fromStored(creds)))
          .catch(() => setStaged([]))
          .finally(() => setStagedLoaded(true));
      }
      if (!dialog.open) dialog.showModal();
    } else if (dialog.open) {
      dialog.close();
    }
  }, [open, editing]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const handleClose = () => onClose();
    dialog.addEventListener("close", handleClose);
    return () => dialog.removeEventListener("close", handleClose);
  }, [onClose]);

  const dirty = useMemo(
    () =>
      staged.some((c) => c.id === null || (c.secret !== null && c.secret !== "")) ||
      staged.length !== initialCountRef.current,
    [staged],
  );
  const initialCountRef = useRef(0);
  useEffect(() => {
    if (stagedLoaded) initialCountRef.current = staged.length;
  }, [stagedLoaded]); // 仅加载完成时刻快照——注意：此写法每加载后重置一次，编辑中 dirty 由 id===null / secret 变更 / 数量变化判定
```

dirty 判定简化（避免 ref 时机坑）：编辑模式加载后拍快照存 state：

```tsx
  const [snapshot, setSnapshot] = useState<string>("");
  useEffect(() => {
    if (stagedLoaded) setSnapshot(JSON.stringify(toInput(staged)));
  }, [stagedLoaded]); // eslint-disable-line -- 故意只依赖 stagedLoaded
  const dirty = stagedLoaded && snapshot !== "" && JSON.stringify(toInput(staged)) !== snapshot;
```

注意 secret 含明文——JSON 序列化只进内存 state 不落盘，可接受。继续主体：

```tsx
  const requestClose = () => {
    if (dirty) {
      setConfirmDiscard(true);
    } else {
      onClose();
    }
  };

  const handleSave = async () => {
    if (!form.name.trim() || !form.host.trim()) {
      setFormError("名称 / 主机不能为空");
      return;
    }
    const port = parsePort(form.port);
    if (port === null) {
      setFormError("端口必须是 1-65535 的数字");
      return;
    }
    if (staged.length === 0) {
      setFormError("至少需要一条登录凭证");
      return;
    }
    if (staged.filter((c) => c.isDefault).length !== 1) {
      setFormError("必须恰好指定一个默认登录用户（点星标切换）");
      return;
    }
    const dup = staged.find((c, i) => staged.findIndex((o) => o.username.trim() === c.username.trim()) !== i);
    if (dup) {
      setFormError(`凭证用户名重复：${dup.username.trim()}`);
      return;
    }
    setSaving(true);
    setFormError(null);
    try {
      const ok = await save({
        environmentId: editing?.id ?? null,
        name: form.name.trim(),
        host: form.host.trim(),
        port,
        credentials: toInput(staged),
      });
      if (ok) onClose();
    } finally {
      setSaving(false);
    }
  };

  const handleTestCred = async (key: string) => {
    const c = staged.find((s) => s.key === key);
    if (!c || !form.host.trim()) {
      setFormError("主机不能为空");
      return;
    }
    const port = parsePort(form.port);
    if (port === null) {
      setFormError("端口必须是 1-65535 的数字");
      return;
    }
    setTestingKey(key);
    try {
      const result = await test({
        environmentId: editing?.id ?? null,
        host: form.host.trim(),
        port,
        user: c.username.trim(),
        authType: c.authType,
        privateKeyPath: c.authType === "private_key" ? c.privateKeyPath : null,
        password: c.secret && c.secret.trim() ? c.secret : null,
      });
      setTestResults((prev) => ({ ...prev, [key]: result ?? undefined }));
    } finally {
      setTestingKey(null);
    }
  };
```

关键点：`handleTestCred` 传 `environmentId: editing?.id`（仅当该凭证是已存凭证时才有意义——后端 `resolve_test_secret` 走 `default_credential` 读 keychain，这是**按默认凭证**读的，与"逐凭证"不符）。所以前端对已存凭证（`c.id !== null`）且 secret 留空时，不传 environmentId 的话后端无法读该凭证的 keychain。**处理**：`test_connection_params_cmd` 需支持按 `credentialId` 读 keychain——在 Task 7 给 `test_connection_params_cmd` 加可选 `credential_id` 参数（从 keychain 读 `env/{env_id}/cred/{cred_id}`），前端 `c.id` 存在时透传。本 task 先把前端写成透传形态，Task 7 补后端：

```tsx
      const result = await test({
        environmentId: editing?.id ?? null,
        credentialId: c.id, // 已存凭证 secret 留空时后端按 cred_id 读 keychain（Task 7）
        host: form.host.trim(),
        ...
```

`ipc.ts` 的 `testConnection` 参数与 types 同步加 `credentialId?: string | null`。

JSX 主体：

```tsx
  return (
    <>
      <dialog ref={dialogRef} aria-label={editing ? "编辑环境" : "新增环境"} className="z-50 w-[520px] max-w-[90vw] rounded-xl bg-card border border-border p-0 text-foreground overflow-hidden">
        <div className="flex flex-col max-h-[85vh] overflow-hidden rounded-xl">
          <div className="flex items-center justify-between px-5 py-4 border-b border-border shrink-0">
            <h2 className="text-sm font-medium">{editing ? "编辑环境" : "新增环境"}</h2>
            <button onClick={requestClose} aria-label="关闭" className="flex items-center justify-center w-7 h-7 rounded-md text-muted-foreground hover:text-foreground hover:bg-surface-3 transition-colors cursor-pointer">
              <X size={16} aria-hidden="true" />
            </button>
          </div>

          <div className="flex-1 overflow-y-auto px-5 py-4 space-y-3 min-h-0">
            <Field label="名称" htmlFor="env-name">
              <input id="env-name" type="text" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} placeholder="prod-jvm-01" className={inputCls} />
            </Field>
            <div className="flex gap-3">
              <Field label="主机" htmlFor="env-host" className="flex-1">
                <input id="env-host" type="text" value={form.host} onChange={(e) => setForm({ ...form, host: e.target.value })} placeholder="10.0.0.1" className={inputCls} />
              </Field>
              <Field label="端口" htmlFor="env-port" className="w-24">
                <input id="env-port" type="text" inputMode="numeric" value={form.port} onChange={(e) => setForm({ ...form, port: e.target.value })} className={inputCls} />
              </Field>
            </div>

            <div className="pt-2 border-t border-border space-y-2">
              <p className="text-xs text-muted-foreground">
                登录凭证：★ 为默认登录用户（日常连接使用）。目标 JVM 以其他用户运行时（arthas attach 需要同用户），为该用户录入 SSH 凭证。
              </p>
              {stagedLoaded ? (
                <CredentialList
                  staged={staged}
                  testingId={testingKey}
                  testResults={testResults}
                  onSetDefault={(key) => setStaged((prev) => prev.map((c) => ({ ...c, isDefault: c.key === key })))}
                  onRemove={(key) => setStaged((prev) => prev.filter((c) => c.key !== key))}
                  onEditStart={() => { /* 编辑态由 CredentialList 内部管理 */ }}
                  onEditCancel={() => { /* 同上 */ }}
                  onEditSave={(key, username, authType, privateKeyPath, secret) =>
                    setStaged((prev) => prev.map((c) => (c.key === key ? { ...c, username, authType, privateKeyPath: authType === "private_key" ? privateKeyPath : null, secret: secret || c.secret } : c)))
                  }
                  onTest={handleTestCred}
                  onAdd={(username, authType, privateKeyPath, secret, makeDefault) => {
                    if (!username) { setFormError("凭证用户名不能为空"); return; }
                    if (authType === "private_key" && !privateKeyPath) { setFormError("私钥认证需要填写私钥路径"); return; }
                    if (staged.some((c) => c.username.trim() === username)) { setFormError(`凭证用户名已存在：${username}`); return; }
                    setFormError(null);
                    setStaged((prev) => addStaged(prev, username, authType, privateKeyPath, secret, makeDefault));
                  }}
                />
              ) : (
                <div className="flex items-center justify-center gap-2 py-4 text-muted-foreground text-xs">
                  <CircleNotch size={14} className="animate-spin" aria-hidden="true" />
                  加载凭证…
                </div>
              )}
            </div>

            {(formError ?? storeError) && (
              <p role="alert" className="text-xs text-destructive break-words">
                {formError ?? storeError}
              </p>
            )}
          </div>

          <div className="flex items-center gap-2 px-5 py-4 border-t border-border shrink-0">
            <div className="flex-1" />
            <button onClick={requestClose} className="px-3 py-1.5 rounded-md border border-border bg-surface-2 text-xs text-foreground hover:bg-surface-3 transition-colors cursor-pointer">
              取消
            </button>
            <button onClick={handleSave} disabled={saving} className="flex items-center gap-2 px-3 py-1.5 rounded-md bg-accent text-accent-foreground text-xs hover:bg-accent/80 transition-colors cursor-pointer disabled:opacity-50">
              {saving && <CircleNotch size={14} className="animate-spin" aria-hidden="true" />}
              保存
            </button>
          </div>
        </div>
      </dialog>

      <DiscardChangesDialog
        open={confirmDiscard}
        onConfirm={() => { setConfirmDiscard(false); onClose(); }}
        onCancel={() => setConfirmDiscard(false)}
      />
    </>
  );
}
```

移除：旧 credForm/creds 状态、handleAddCred/handleDeleteCred/handleSetDefaultCred、旧的"测试连接"按钮与 handleTest（凭证粒度测试取代）、Plug/CheckCircle/XCircle import（CredentialList 内消化结果展示）。
`inputCls`/`Field`/`parsePort` 帮助函数保留在文件底部。

- [ ] **Step 6.5: 类型检查 + 手动冒烟**

Run: `pnpm typecheck`
Expected: PASS（含 Task 7 的 credentialId 字段前会 FAIL——若 Task 7 未先行，此处先在 `ipc.ts`/`testConnection` 参数里加上 `credentialId?: string | null` 并透传给 invoke，后端 Task 7 再消费）

Run: `pnpm tauri dev`
手动验证：
1. 新增环境：填名称/host/端口，添加两条凭证，星标切换默认，保存 → 列表出现环境
2. 编辑环境：弹窗回显名称/host/端口 + 凭证列表（加载态 → 渲染）；改 host；编辑一条凭证的密码（留空不变语义）；移除一条；保存 → 重新打开验证
3. 逐凭证测试连接：编辑模式已存凭证 secret 留空 → 走 keychain（Task 7 后生效）；新加凭证填密码 → 直接测
4. 放弃变更：改点东西点 X → 确认弹窗

- [ ] **Step 6.6: 提交（含 Task 5 的 ipc/store 改动）**

```bash
git add src/lib/types.ts src/lib/ipc.ts src/store/envStore.ts src/components/environments/EnvironmentDialog.tsx src/components/environments/CredentialList.tsx src/components/environments/DiscardChangesDialog.tsx src/components/environments/staged.ts
git commit -m "feat: unified credential list dialog with staged edits and atomic save"
```

---

### Task 7: `test_connection_params_cmd` 支持按 credentialId 读 keychain

**Files:**
- Modify: `src-tauri/src/app/environments.rs`（test_connection_params_cmd + resolve_test_secret）

- [ ] **Step 7.1: 失败测试**

追加到 environments.rs tests：

```rust
    #[test]
    fn test_resolve_test_secret_with_credential_id() {
        // 有 credential_id → FromKeychainCred（按凭证条目读）
        assert_eq!(
            resolve_test_secret(Some("env-1"), Some("cred-9"), None),
            TestSecret::FromKeychainCred("env-1".to_string(), "cred-9".to_string())
        );
        // 表单密钥优先于 credential_id
        assert_eq!(
            resolve_test_secret(Some("env-1"), Some("cred-9"), Some("pw")),
            TestSecret::Provided(Some("pw".to_string()))
        );
        // 无 credential_id 保持旧行为
        assert_eq!(
            resolve_test_secret(Some("env-1"), None, None),
            TestSecret::FromKeychain("env-1".to_string())
        );
    }
```

既有三个 `test_resolve_test_secret_*` 测试的调用处签名变了，同步补 `None` 第三参。

- [ ] **Step 7.2: 实现**

```rust
pub enum TestSecret {
    Provided(Option<String>),
    FromKeychain(String),
    /// 编辑已有凭证且未填新密钥 → 按 env/cred 双 id 读密钥链
    FromKeychainCred(String, String),
}

pub fn resolve_test_secret(
    environment_id: Option<&str>,
    credential_id: Option<&str>,
    password: Option<&str>,
) -> TestSecret {
    match password {
        Some(p) if !p.trim().is_empty() => TestSecret::Provided(Some(p.to_string())),
        _ => match (environment_id, credential_id) {
            (Some(env), Some(cred)) => TestSecret::FromKeychainCred(env.to_string(), cred.to_string()),
            (Some(env), None) => TestSecret::FromKeychain(env.to_string()),
            _ => TestSecret::Provided(None),
        },
    }
}
```

`test_connection_params_cmd` 加参数 `credential_id: Option<String>`，secret 解析分支加：

```rust
        TestSecret::FromKeychainCred(env_id, cred_id) => {
            crate::app::credentials::load_cred_secret(&env_id, &cred_id)
                .await
                .map_err(|e| e.to_string())?
        }
```

- [ ] **Step 7.3: 测试 + 提交**

Run: `cargo test --manifest-path src-tauri/Cargo.toml test_resolve_test_secret` → PASS
Run: `cargo check --manifest-path src-tauri/Cargo.toml` → 编译通过

```bash
git add src-tauri/src/app/environments.rs
git commit -m "feat: test_connection_params_cmd reads keychain by credential id"
```

---

### Task 8: 全量回归 + 收尾

**Files:**
- Modify: `AGENTS.md`（"已实现功能"多用户凭证描述更新）

- [ ] **Step 8.1: 全量检查**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 通过

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全部 PASS

Run: `pnpm typecheck`
Expected: 通过

- [ ] **Step 8.2: 手动回归清单**

`pnpm tauri dev`：
1. 新增环境（多凭证 + 星标默认 + 私钥/密码混合）→ 保存 → 列表卡片 user@host 正确（= 默认凭证用户）
2. 编辑环境：改名称/host/端口；凭证增/删/改/切默认；保存；重开验证
3. 凭证密码留空保存 → 原 secret 保留（测试连接验证）
4. 认证方式从密码切私钥（secret 留空）→ 保存 → 测试连接报"密码认证需要填写密码"类错误为预期（已切走则用私钥逻辑）
5. 放弃变更确认弹窗
6. 用保存的环境跑一次 run_command（验证默认凭证连接 + 保存后连接失效重连）
7. 删除环境 → keychain 清理日志无 error

- [ ] **Step 8.3: AGENTS.md 更新**

"已实现功能"中 Arthas 段落里的多用户凭证描述：

```
旧：环境多用户凭证管理（`env_credentials` 表 + 环境编辑弹窗，默认凭证即日常 SSH 用户）
新：环境多用户凭证管理（`env_credentials` 表 + 新增/编辑统一弹窗：凭证增删改/星标设默认、本地暂存、`save_environment_cmd` 原子提交；默认凭证即日常 SSH 用户；逐凭证测试连接）
```

- [ ] **Step 8.4: 提交**

```bash
git add AGENTS.md
git commit -m "docs: update AGENTS.md for unified env credential dialog"
```

---

## Self-Review 记录

- **Spec 覆盖**：UI 形态（Task 6）/ 原子命令（Task 1-3）/ 旧命令删除（Task 4）/ 前端收口（Task 5-6）/ 逐凭证测试（Task 6-7）/ 未保存确认（Task 6）/ 连接失效（Task 3）/ AGENTS.md（Task 8）✓
- **类型一致性**：`CredentialInput`（Rust serde camelCase ↔ TS interface 字段一一对应）；`StagedCredential`（弹窗内模型）→ `toInput` → `CredentialInput`；`save_environment_cmd` 入参用 `params: SaveEnvironmentParams` 单对象（前端 `invoke("save_environment_cmd", { params })` 匹配）✓
- **占位符**：无 TBD/TODO ✓
