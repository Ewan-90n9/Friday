use crate::tools::builtin::run_command::artifact_dir_for;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use crate::transfer::state::{Direction, Status};
use crate::transfer::TransferManager;
use async_trait::async_trait;
use std::sync::Arc;

fn err_invalid(msg: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({ "error": "invalid_params", "message": msg }),
        raw_stdout: None,
    }
}

fn err_env_not_found(environment: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        data: serde_json::json!({
            "error": "environment_not_found",
            "message": format!(
                "环境「{environment}」不存在。请先调用 list_environments 查看可用环境；若无匹配，请让用户在右侧「环境」面板添加。"
            ),
        }),
        raw_stdout: None,
    }
}

/// 远端路径校验：必须以 / 开头
fn validate_remote_path(p: &str) -> Result<(), String> {
    if !p.starts_with('/') {
        Err(format!("remote_path 必须是绝对路径（以 / 开头）: {p}"))
    } else {
        Ok(())
    }
}

/// 远端 basename 校验（防穿越）：非空且不是 . / ..，且不含 Windows 路径分隔符或盘符冒号
fn remote_basename(p: &str) -> Result<String, String> {
    let name = p.rsplit('/').next().unwrap_or("");
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('\\')
        || name.contains(':')
    {
        Err(format!("remote_path 文件名非法: {p}"))
    } else {
        Ok(name.to_string())
    }
}

pub struct FileTransferTools {
    pub core: Arc<TransferManager>,
    pub artifacts_dir: std::path::PathBuf,
}

impl FileTransferTools {
    async fn file_download(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: environment");
        };
        let Some(remote_path) = args.get("remote_path").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: remote_path");
        };
        if let Err(e) = validate_remote_path(remote_path) {
            return err_invalid(&e);
        }
        let Ok(file_name) = remote_basename(remote_path) else {
            return err_invalid(&format!("remote_path 文件名非法: {remote_path}"));
        };
        let env = match crate::app::environments::find_by_name(self.core.db(), environment).await {
            Ok(Some(env)) => env,
            Ok(None) => return err_env_not_found(environment),
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, error = %e, "file_download: env lookup failed");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({ "error": "lookup_failed", "message": format!("查询环境失败: {e}") }),
                    raw_stdout: None,
                };
            }
        };

        // 去重提示（start 内部原子去重，这里先查一次给 Agent 明确信号）
        if let Some(existing) = self
            .core
            .find_active(&ctx.session_id, Direction::Download, remote_path)
            .await
        {
            return ToolOutput {
                success: false,
                data: serde_json::json!({
                    "error": "duplicate_transfer",
                    "message": "该文件已有进行中的下载任务。",
                    "transfer_id": existing.id,
                    "note": "请轮询 transfer_status(transfer_id) 获取结果。",
                }),
                raw_stdout: None,
            };
        }

        let session_dir = artifact_dir_for(&self.artifacts_dir, &ctx.session_id);
        let local_path = session_dir.join(&file_name);

        let state = crate::transfer::state::TransferState::new(
            Direction::Download,
            &ctx.session_id,
            &env.id,
            remote_path,
            local_path.clone(),
            false, // 独立下载不清理远端
        );
        let transfer_id = self.core.start(state).await;

        tracing::info!(session_id = %ctx.session_id, transfer_id = %transfer_id, env_id = %env.id, remote_path, "file_download: background transfer started");

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "status": "pending",
                "local_path": local_path.to_string_lossy(),
                "note": "传输已在后台启动，请轮询 transfer_status(transfer_id) 获取进度/结果。",
            }),
            raw_stdout: None,
        }
    }

    async fn file_upload(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(environment) = args.get("environment").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: environment");
        };
        let Some(local_path) = args.get("local_path").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: local_path");
        };
        let Some(remote_path) = args.get("remote_path").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: remote_path");
        };
        let local = std::path::PathBuf::from(local_path);
        if !local.is_absolute() {
            return err_invalid(&format!("local_path 必须是绝对路径: {local_path}"));
        }
        if let Err(e) = validate_remote_path(remote_path) {
            return err_invalid(&e);
        }
        if !local.is_file() {
            return err_invalid(&format!("本地文件不存在或不是普通文件: {local_path}"));
        }
        let env = match crate::app::environments::find_by_name(self.core.db(), environment).await {
            Ok(Some(env)) => env,
            Ok(None) => return err_env_not_found(environment),
            Err(e) => {
                tracing::error!(session_id = %ctx.session_id, error = %e, "file_upload: env lookup failed");
                return ToolOutput {
                    success: false,
                    data: serde_json::json!({ "error": "lookup_failed", "message": format!("查询环境失败: {e}") }),
                    raw_stdout: None,
                };
            }
        };
        if let Some(existing) = self
            .core
            .find_active(&ctx.session_id, Direction::Upload, remote_path)
            .await
        {
            return ToolOutput {
                success: false,
                data: serde_json::json!({
                    "error": "duplicate_transfer",
                    "message": "该文件已有进行中的上传任务。",
                    "transfer_id": existing.id,
                    "note": "请轮询 transfer_status(transfer_id) 获取结果。",
                }),
                raw_stdout: None,
            };
        }

        let state = crate::transfer::state::TransferState::new(
            Direction::Upload,
            &ctx.session_id,
            &env.id,
            remote_path,
            local.clone(),
            false,
        );
        let transfer_id = self.core.start(state).await;

        tracing::info!(session_id = %ctx.session_id, transfer_id = %transfer_id, env_id = %env.id, local_path, remote_path, "file_upload: background transfer started");

        ToolOutput {
            success: true,
            data: serde_json::json!({
                "transfer_id": transfer_id,
                "status": "pending",
                "note": "上传已在后台启动，请轮询 transfer_status(transfer_id) 获取进度/结果。",
            }),
            raw_stdout: None,
        }
    }

    async fn transfer_status(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let transfer_id = args.get("transfer_id").and_then(|v| v.as_str());
        let state_to_json = |s: &crate::transfer::state::TransferState| {
            let mut j = serde_json::json!({
                "transfer_id": s.id,
                "direction": s.direction,
                "status": s.status,
                "transferred_bytes": s.transferred_bytes,
                "total_bytes": s.total_bytes,
                "speed_bps": s.speed_bps,
                "attempt": s.attempt,
                "error": s.error,
                "local_path": s.local_path.to_string_lossy(),
                "remote_path": s.remote_path,
            });
            match s.status {
                Status::Completed => {
                    j["note"] = serde_json::json!("传输完成。下载场景请把 local_path 告知用户。");
                }
                Status::Failed => {
                    j["note"] = serde_json::json!("传输失败。远端文件保留（下载场景），可用 file_download 重试（断点续传）。");
                }
                Status::Retrying => {
                    j["note"] = serde_json::json!("传输中断，正在自动重试。请稍后再轮询。");
                }
                _ => {}
            }
            j
        };
        match transfer_id {
            Some(id) => match self.core.get(id).await {
                Some(s) => ToolOutput {
                    success: true,
                    data: state_to_json(&s),
                    raw_stdout: None,
                },
                None => err_invalid(&format!("transfer_id 不存在: {id}")),
            },
            None => {
                let list = self.core.list_for_session(&ctx.session_id).await;
                ToolOutput {
                    success: true,
                    data: serde_json::json!({
                        "transfers": list.iter().map(state_to_json).collect::<Vec<_>>(),
                    }),
                    raw_stdout: None,
                }
            }
        }
    }

    async fn transfer_cancel(&self, args: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(transfer_id) = args.get("transfer_id").and_then(|v| v.as_str()) else {
            return err_invalid("missing required parameter: transfer_id");
        };
        let ok = self.core.cancel(transfer_id).await;
        ToolOutput {
            success: ok,
            data: if ok {
                serde_json::json!({ "cancelled": true, "transfer_id": transfer_id })
            } else {
                serde_json::json!({
                    "cancelled": false,
                    "transfer_id": transfer_id,
                    "message": "任务不存在或已结束",
                })
            },
            raw_stdout: None,
        }
    }
}

// 每个工具一个薄 handler struct
pub struct FileDownloadHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for FileDownloadHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.file_download(args, ctx).await
    }
}
pub struct FileUploadHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for FileUploadHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.file_upload(args, ctx).await
    }
}
pub struct TransferStatusHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for TransferStatusHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.transfer_status(args, ctx).await
    }
}
pub struct TransferCancelHandler(pub Arc<FileTransferTools>);
#[async_trait]
impl ToolHandler for TransferCancelHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        self.0.transfer_cancel(args, ctx).await
    }
}

pub fn file_transfer_tool_defs(
    mgr: Arc<TransferManager>,
    artifacts_dir: std::path::PathBuf,
) -> Vec<ToolDef> {
    let tools = Arc::new(FileTransferTools { core: mgr, artifacts_dir });
    vec![
        ToolDef {
            name: "file_download".to_string(),
            description: "从远端环境下载文件到本地（后台异步传输，支持断点续传）。启动后立即返回 transfer_id，必须轮询 transfer_status(transfer_id) 至终态。下载完成后文件在本机会话 artifacts 目录（返回 local_path），请把路径告知用户。远端文件不会被删除。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "environment": { "type": "string", "description": "目标环境名称（list_environments 返回的 name）" },
                    "remote_path": { "type": "string", "description": "远端文件绝对路径" }
                },
                "required": ["environment", "remote_path"]
            }),
            risk_level: RiskLevel::Low,
            needs_channel: false,
            handler: Arc::new(FileDownloadHandler(tools.clone())),
        },
        ToolDef {
            name: "file_upload".to_string(),
            description: "上传本地文件到远端环境（后台异步传输）。⚠ 上传任意本地文件需用户确认。启动后立即返回 transfer_id，必须轮询 transfer_status(transfer_id) 至终态。上传失败重试会整体重传覆盖远端半成品。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "environment": { "type": "string", "description": "目标环境名称" },
                    "local_path": { "type": "string", "description": "本地文件绝对路径" },
                    "remote_path": { "type": "string", "description": "远端目标绝对路径" }
                },
                "required": ["environment", "local_path", "remote_path"]
            }),
            risk_level: RiskLevel::High,
            needs_channel: false,
            handler: Arc::new(FileUploadHandler(tools.clone())),
        },
        ToolDef {
            name: "transfer_status".to_string(),
            description: "查询后台传输任务状态（file_download/file_upload/jvm_heap_dump 的拉回均产生传输任务）。传 transfer_id 查单条；不传则列出本会话全部传输。终态：completed（下载场景带 local_path 可交付用户）/ failed（远端文件保留，可重试）/ cancelled；retrying 表示自动重试中，请稍后再查。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "transfer_id": { "type": "string", "description": "传输任务 ID（可选，缺省列出全部）" }
                }
            }),
            risk_level: RiskLevel::ReadOnly,
            needs_channel: false,
            handler: Arc::new(TransferStatusHandler(tools.clone())),
        },
        ToolDef {
            name: "transfer_cancel".to_string(),
            description: "取消进行中的后台传输任务。已下载的部分保留（下次 file_download 同文件可断点续传）。".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "transfer_id": { "type": "string", "description": "要取消的传输任务 ID" }
                },
                "required": ["transfer_id"]
            }),
            risk_level: RiskLevel::ReadOnly,
            needs_channel: false,
            handler: Arc::new(TransferCancelHandler(tools)),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (tempfile::TempDir, Arc<FileTransferTools>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("friday.db")).await.unwrap();
        crate::app::environments::add_environment(
            &db, "prod", "10.0.0.1", 22, "root", "password", None, None,
        ).await.unwrap();
        let mgr = Arc::new(TransferManager::new(db, crate::app::events::EventBus::disabled()));
        let artifacts = tmp.path().join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        (tmp, Arc::new(FileTransferTools { core: mgr, artifacts_dir: artifacts }))
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "123e4567-e89b-12d3-a456-426614174000".into(), channel: None }
    }

    #[tokio::test]
    async fn test_download_rejects_relative_remote_path() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "prod", "remote_path": "tmp/a.hprof"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_download_rejects_bad_basename() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        for p in ["/tmp/", "/tmp/..", "/tmp/."] {
            let out = h.execute(
                serde_json::json!({"environment": "prod", "remote_path": p}),
                &ctx(),
            ).await;
            assert!(!out.success, "path {p} must be rejected");
            assert_eq!(out.data["error"], "invalid_params");
        }
        drop(tmp);
    }

    #[test]
    fn test_remote_basename_validation() {
        assert!(remote_basename("/tmp/a.hprof").is_ok());
        assert!(remote_basename("/tmp/").is_err());
        assert!(remote_basename("/tmp/..").is_err());
        assert!(remote_basename("/tmp/a\\b").is_err());
        assert!(remote_basename("/tmp/C:\\evil").is_err());
    }

    #[tokio::test]
    async fn test_download_rejects_windows_traversal_in_basename() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        for p in ["/tmp/..\\..\\..\\evil.exe", "/tmp/C:\\evil", "/tmp/a\\b.hprof"] {
            let out = h.execute(
                serde_json::json!({"environment": "prod", "remote_path": p}),
                &ctx(),
            ).await;
            assert!(!out.success, "path {p} must be rejected");
            assert_eq!(out.data["error"], "invalid_params");
        }
        drop(tmp);
    }

    #[tokio::test]
    async fn test_download_duplicate_returns_existing_transfer_id() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools.clone());
        let args = serde_json::json!({"environment": "prod", "remote_path": "/tmp/friday-tools/dup.hprof"});
        let first = h.execute(args.clone(), &ctx()).await;
        assert!(first.success);
        let tid = first.data["transfer_id"].as_str().unwrap().to_string();
        let second = h.execute(args, &ctx()).await;
        assert!(!second.success);
        assert_eq!(second.data["error"], "duplicate_transfer");
        assert_eq!(second.data["transfer_id"].as_str().unwrap(), tid);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upload_requires_absolute_local_path() {
        let (tmp, tools) = setup().await;
        let h = FileUploadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "prod", "local_path": "relative.jar", "remote_path": "/tmp/x.jar"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upload_rejects_missing_local_file() {
        let (tmp, tools) = setup().await;
        let h = FileUploadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "prod", "local_path": "Z:/no/such/file.jar", "remote_path": "/tmp/x.jar"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert!(out.data["message"].as_str().unwrap().contains("不存在"));
        drop(tmp);
    }

    #[tokio::test]
    async fn test_unknown_environment() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools);
        let out = h.execute(
            serde_json::json!({"environment": "ghost", "remote_path": "/tmp/a.hprof"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "environment_not_found");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_status_unknown_transfer_id() {
        let (tmp, tools) = setup().await;
        let h = TransferStatusHandler(tools);
        let out = h.execute(
            serde_json::json!({"transfer_id": "nope"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        drop(tmp);
    }

    #[tokio::test]
    async fn test_status_empty_lists_session_transfers() {
        let (tmp, tools) = setup().await;
        let h = TransferStatusHandler(tools);
        let out = h.execute(serde_json::json!({}), &ctx()).await;
        assert!(out.success);
        assert_eq!(out.data["transfers"].as_array().unwrap().len(), 0);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_download_starts_and_returns_transfer_id() {
        let (tmp, tools) = setup().await;
        let h = FileDownloadHandler(tools.clone());
        let out = h.execute(
            serde_json::json!({"environment": "prod", "remote_path": "/tmp/friday-tools/a.hprof"}),
            &ctx(),
        ).await;
        assert!(out.success, "out: {}", out.data);
        let tid = out.data["transfer_id"].as_str().unwrap();
        assert!(!tid.is_empty());
        assert!(out.data["local_path"].as_str().unwrap().contains("a.hprof"));
        assert!(out.data["note"].as_str().unwrap().contains("轮询"));
        // 注册表里能查到
        assert!(tools.core.get(tid).await.is_some());
        // 状态查询能返回该条
        let sh = TransferStatusHandler(tools.clone());
        let s = sh.execute(serde_json::json!({"transfer_id": tid}), &ctx()).await;
        assert!(s.success);
        assert_eq!(s.data["transfer_id"], tid);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_upload_starts_and_returns_transfer_id() {
        let (tmp, tools) = setup().await;
        // 创建一个真实本地文件（上传校验存在性）
        let local = tmp.path().join("tool.jar");
        std::fs::write(&local, b"jar-bytes").unwrap();
        let h = FileUploadHandler(tools.clone());
        let out = h.execute(
            serde_json::json!({"environment": "prod", "local_path": local.to_string_lossy(), "remote_path": "/tmp/friday-tools/tool.jar"}),
            &ctx(),
        ).await;
        assert!(out.success, "out: {}", out.data);
        let tid = out.data["transfer_id"].as_str().unwrap();
        assert!(!tid.is_empty());
        assert!(out.data["note"].as_str().unwrap().contains("轮询"));
        assert!(tools.core.get(tid).await.is_some());
        drop(tmp);
    }

    #[tokio::test]
    async fn test_cancel_unknown_transfer_id() {
        let (tmp, tools) = setup().await;
        let h = TransferCancelHandler(tools);
        let out = h.execute(
            serde_json::json!({"transfer_id": "nope"}),
            &ctx(),
        ).await;
        assert!(!out.success);
        assert_eq!(out.data["cancelled"], false);
        drop(tmp);
    }

    #[tokio::test]
    async fn test_tool_def_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::infra::db::init(tmp.path().join("t.db")).await.unwrap();
        let mgr = Arc::new(TransferManager::new(db, crate::app::events::EventBus::disabled()));
        let defs = file_transfer_tool_defs(mgr, tmp.path().join("artifacts"));
        assert_eq!(defs.len(), 4);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"file_download"));
        assert!(names.contains(&"file_upload"));
        assert!(names.contains(&"transfer_status"));
        assert!(names.contains(&"transfer_cancel"));
        let upload = defs.iter().find(|d| d.name == "file_upload").unwrap();
        assert_eq!(upload.risk_level, RiskLevel::High);
        let status = defs.iter().find(|d| d.name == "transfer_status").unwrap();
        assert_eq!(status.risk_level, RiskLevel::ReadOnly);
        let download = defs.iter().find(|d| d.name == "file_download").unwrap();
        assert_eq!(download.risk_level, RiskLevel::Low);
        for d in &defs {
            assert!(!d.needs_channel);
        }
    }
}
