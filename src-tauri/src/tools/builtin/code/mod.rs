//! code_* 工具：全部只读自主、needs_channel = false（纯本地，无环境也可用）。

use crate::app::service_repos;
use crate::code::{self, CodeRepoManager, RepoState};
use crate::tools::builtin::jvm::core::error_output;
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;

pub struct CodeToolDeps {
    pub manager: Arc<CodeRepoManager>,
    pub db: SqlitePool,
}

#[derive(Clone, Copy, Debug)]
enum CodeToolKind {
    GetRepo,
    OpenRepo,
    RepoStatus,
    ReadFile,
    Search,
    ListFiles,
}

struct CodeToolHandler {
    deps: Arc<CodeToolDeps>,
    kind: CodeToolKind,
}

fn state_json(st: &RepoState) -> serde_json::Value {
    serde_json::json!({
        "repo_id": st.repo_id,
        "repo_url": st.repo_url,
        "ref": st.git_ref,
        "status": st.phase.as_str(),
        "progress": st.progress,
        "resolved_commit": st.resolved_commit,
        "stale_warning": st.stale_warning,
        "error_code": st.error_code,
        "error": st.error,
    })
}

fn require_str(args: &serde_json::Value, key: &str) -> Result<String, ToolOutput> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error_output("invalid_params", &format!("missing required parameter: {key}")))
}

fn clamp_usize(v: Option<i64>, default: usize, max: usize) -> usize {
    match v {
        Some(n) if n >= 1 => (n as usize).min(max),
        _ => default,
    }
}

#[async_trait]
impl ToolHandler for CodeToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, "code tool executing");
        match self.kind {
            CodeToolKind::GetRepo => self.get_repo(&args).await,
            CodeToolKind::OpenRepo => self.open_repo(&args).await,
            CodeToolKind::RepoStatus => self.repo_status(&args).await,
            CodeToolKind::ReadFile => self.read_file(&args).await,
            CodeToolKind::Search => self.search(&args).await,
            CodeToolKind::ListFiles => self.list_files(&args).await,
        }
    }
}

impl CodeToolHandler {
    /// search.rs 的路径错误 → 结构化错误码（逃逸/未找到区分）
    fn path_error(e: String) -> ToolOutput {
        if e.contains("escapes") {
            error_output("path_outside_repo", &e)
        } else {
            error_output("path_not_found", &e)
        }
    }

    async fn get_repo(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(service) = require_str(args, "service") else {
            return error_output("invalid_params", "missing required parameter: service");
        };
        match service_repos::get_service_repo(&self.deps.db, &service).await {
            Ok(Some(row)) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "service": row.service,
                    "found": true,
                    "repo_url": row.repo_url,
                    "last_ref": row.last_ref,
                    "hint": "repo_url is the remembered address; always confirm the ref (branch changes often, last_ref is only a hint)",
                }),
                raw_stdout: None,
            },
            Ok(None) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "service": service,
                    "found": false,
                    "hint": "ask the user for the repo URL and the ref (branch/tag/commit id) deployed on the target",
                }),
                raw_stdout: None,
            },
            Err(e) => error_output("db_error", &e.to_string()),
        }
    }

    async fn open_repo(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(repo_url) = require_str(args, "repo_url") else {
            return error_output("invalid_params", "missing required parameter: repo_url");
        };
        let Ok(git_ref) = require_str(args, "ref") else {
            return error_output("invalid_params", "missing required parameter: ref (branch/tag/commit id)");
        };
        if let Err(e) = code::git::validate_repo_url(&repo_url) {
            return error_output("invalid_repo_url", &e);
        }
        if !code::git::git_available() {
            return error_output(
                "git_not_found",
                "git binary not found on PATH; install git (https://git-scm.com) and retry",
            );
        }
        let service = args.get("service").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let repo_id = self.deps.manager.open(&repo_url, &git_ref).await;
        if let Some(svc) = &service {
            if let Err(e) = service_repos::upsert_service_repo(&self.deps.db, svc, &repo_url, Some(&git_ref)).await {
                tracing::warn!(service = %svc, error = ?e, "failed to persist service repo mapping");
            }
        }
        let st = self.deps.manager.status(&repo_id).await;
        let mut data = st.as_ref().map(state_json).unwrap_or_else(|| serde_json::json!({"repo_id": repo_id, "status": "cloning"}));
        if service.is_some() {
            data["service"] = serde_json::json!(service);
        }
        ToolOutput { success: true, data, raw_stdout: None }
    }

    async fn repo_status(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(repo_id) = require_str(args, "repo_id") else {
            return error_output("invalid_params", "missing required parameter: repo_id");
        };
        match self.deps.manager.status(&repo_id).await {
            Some(st) => ToolOutput { success: true, data: state_json(&st), raw_stdout: None },
            None => error_output("repo_not_found", "unknown repo_id; call code_open_repo first"),
        }
    }

    async fn resolve_worktree(&self, repo_id: &str) -> Result<std::path::PathBuf, ToolOutput> {
        match self.deps.manager.worktree(repo_id).await {
            Ok(p) => Ok(p),
            Err(code) if code == "repo_not_found" => {
                Err(error_output("repo_not_found", "unknown repo_id; call code_open_repo first"))
            }
            Err(not_ready) => Err(error_output(
                "repo_not_ready",
                &format!("repo is {not_ready}; poll code_repo_status until ready, do NOT re-open"),
            )),
        }
    }

    async fn read_file(&self, args: &serde_json::Value) -> ToolOutput {
        let (Ok(repo_id), Ok(path)) = (require_str(args, "repo_id"), require_str(args, "path")) else {
            return error_output("invalid_params", "missing required parameter: repo_id / path");
        };
        let offset = clamp_usize(args.get("offset").and_then(|v| v.as_i64()), 1, usize::MAX);
        let limit = clamp_usize(args.get("limit").and_then(|v| v.as_i64()), code::search::DEFAULT_READ_LINES, code::search::MAX_READ_LINES);
        let wt = match self.resolve_worktree(&repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::read_file(&wt, &path, offset, limit) {
            Ok(out) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "repo_id": repo_id,
                    "path": path,
                    "total_lines": out.total_lines,
                    "offset": out.offset,
                    "limit": out.limit,
                    "truncated": out.truncated,
                    "lines": out.lines.iter().map(|(n, t)| serde_json::json!({"line": n, "text": t})).collect::<Vec<_>>(),
                }),
                raw_stdout: None,
            },
            Err(e) => Self::path_error(e),
        }
    }

    async fn search(&self, args: &serde_json::Value) -> ToolOutput {
        let (Ok(repo_id), Ok(pattern)) = (require_str(args, "repo_id"), require_str(args, "pattern")) else {
            return error_output("invalid_params", "missing required parameter: repo_id / pattern");
        };
        let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        let context = clamp_usize(args.get("context").and_then(|v| v.as_i64()), code::search::DEFAULT_CONTEXT_LINES, code::search::MAX_CONTEXT_LINES);
        let max_results = clamp_usize(args.get("max_results").and_then(|v| v.as_i64()), code::search::DEFAULT_MAX_RESULTS, code::search::MAX_RESULTS_CAP);
        let wt = match self.resolve_worktree(&repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::search(&wt, &pattern, glob.as_deref(), context, max_results) {
            Ok(out) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "repo_id": repo_id,
                    "pattern": pattern,
                    "truncated": out.truncated,
                    "matches": out.matches.iter().map(|m| serde_json::json!({
                        "file": m.file,
                        "line": m.line,
                        "text": m.text,
                        "context_before": m.context_before.iter().map(|(n, t)| serde_json::json!({"line": n, "text": t})).collect::<Vec<_>>(),
                        "context_after": m.context_after.iter().map(|(n, t)| serde_json::json!({"line": n, "text": t})).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                }),
                raw_stdout: None,
            },
            Err(e) => error_output("pattern_invalid", &e),
        }
    }

    async fn list_files(&self, args: &serde_json::Value) -> ToolOutput {
        let Ok(repo_id) = require_str(args, "repo_id") else {
            return error_output("invalid_params", "missing required parameter: repo_id");
        };
        let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
        let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        let wt = match self.resolve_worktree(&repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::list_files(&wt, path.as_deref(), glob.as_deref()) {
            Ok((files, truncated)) => ToolOutput {
                success: true,
                data: serde_json::json!({
                    "repo_id": repo_id,
                    "files": files,
                    "truncated": truncated,
                }),
                raw_stdout: None,
            },
            Err(e) => Self::path_error(e),
        }
    }
}

fn code_tool_def(name: &str, description: &str, schema: serde_json::Value, kind: CodeToolKind, deps: &Arc<CodeToolDeps>) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: schema,
        risk_level: RiskLevel::ReadOnly,
        category: ToolCategory::Code,
        needs_channel: false,
        handler: Arc::new(CodeToolHandler { deps: deps.clone(), kind }),
    }
}

pub fn register_all(registry: &mut crate::tools::registry::ToolRegistry, deps: Arc<CodeToolDeps>) {
    registry.register(code_tool_def(
        "code_get_repo",
        "查询某服务记住的代码仓地址与上次使用的 ref（分支/tag/commitid）。需要读源码时先调本工具：命中后仍须向用户确认仓地址仍有效、并确认本次部署的 ref（分支经常变，last_ref 仅作提示）；未命中则直接问用户仓地址和 ref。",
        serde_json::json!({
            "type": "object",
            "properties": { "service": { "type": "string", "description": "服务名（与用户输入/进程关键词一致）" } },
            "required": ["service"]
        }),
        CodeToolKind::GetRepo,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_open_repo",
        "打开代码仓用于读取（幂等）：后台完整 clone / fetch + 检出到指定 ref（分支/tag/commitid 均可），立即返回 repo_id 与状态。大仓首次 clone 需数分钟，轮询 code_repo_status 直到 ready；cloning/fetching 期间勿重复调用（同 url+ref 幂等返回同 id）。带 service 参数会记住 服务→仓 映射。认证依赖本机 git 配置（credential manager / .netrc / URL 内嵌 token），公司仓走 https。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_url": { "type": "string", "description": "git 仓地址（https:// 或 file://，公司强制 https）" },
                "ref": { "type": "string", "description": "分支名 / tag 名 / commitid" },
                "service": { "type": "string", "description": "服务名（可选，提供则记住 服务→仓 映射，下次免问仓地址）" }
            },
            "required": ["repo_url", "ref"]
        }),
        CodeToolKind::OpenRepo,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_repo_status",
        "轮询 code_open_repo 进度：status = cloning / fetching / ready / failed。progress 为 git 进度百分比；ready 的 resolved_commit 为检出 commit，stale_warning 非空表示 fetch 失败降级用本地缓存（要告知用户代码可能非最新）。failed 看 error_code：clone_failed / fetch_failed / invalid_ref / git_not_found。",
        serde_json::json!({
            "type": "object",
            "properties": { "repo_id": { "type": "string", "description": "code_open_repo 返回的 repo_id" } },
            "required": ["repo_id"]
        }),
        CodeToolKind::RepoStatus,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_read_file",
        "读仓内源码文件，输出 1-based 行号。线程栈/堆栈帧行号可直接对上。大文件分页：offset 起始行（默认 1）、limit 行数（默认 2000 上限 5000）。定位类文件先用 code_list_files(glob) 或 code_search。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "path": { "type": "string", "description": "仓内相对路径（如 src/main/java/com/foo/Bar.java）" },
                "offset": { "type": "number", "description": "起始行（1-based，默认 1）" },
                "limit": { "type": "number", "description": "读取行数（默认 2000，上限 5000）" }
            },
            "required": ["repo_id", "path"]
        }),
        CodeToolKind::ReadFile,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_search",
        "仓内正则搜索（尊重 .gitignore，跳过二进制/超大文件），返回 文件+行号+匹配行+上下文。定位类定义用 pattern=\"class Foo\"；glob 过滤文件类型如 \"*.java\"。context 上下文行数默认 2，max_results 默认 100。默认跳过隐藏文件（dot 开头，同 ripgrep）；结果按文件遍历序返回，截断时收窄 pattern/glob 后重试。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "pattern": { "type": "string", "description": "正则表达式" },
                "glob": { "type": "string", "description": "文件白名单 glob（如 *.java、**/Bar.java）" },
                "context": { "type": "number", "description": "上下文行数（默认 2，上限 10）" },
                "max_results": { "type": "number", "description": "匹配上限（默认 100，上限 500）" }
            },
            "required": ["repo_id", "pattern"]
        }),
        CodeToolKind::Search,
        &deps,
    ));
    registry.register(code_tool_def(
        "code_list_files",
        "列仓内文件（尊重 .gitignore）：glob 匹配（如 **/Bar.java，线程栈类名定位）或 path 列子目录。上限 500 条，超出置 truncated。默认跳过隐藏文件（dot 开头，同 ripgrep）。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "path": { "type": "string", "description": "子目录前缀（可选）" },
                "glob": { "type": "string", "description": "文件 glob 白名单（可选）" }
            },
            "required": ["repo_id"]
        }),
        CodeToolKind::ListFiles,
        &deps,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::ToolHandler;

    async fn test_deps() -> (Arc<CodeToolDeps>, tempfile::TempDir, tempfile::TempDir, String) {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let db_tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(db_tmp.path().join("friday.db")).await.unwrap();
        let deps = Arc::new(CodeToolDeps {
            manager: Arc::new(CodeRepoManager::new(repos.path().to_path_buf())),
            db: pool,
        });
        (deps, src, repos, url)
    }

    fn ctx() -> ToolContext {
        ToolContext { session_id: "s-test".to_string(), channel: None }
    }

    async fn open_until_ready(deps: &Arc<CodeToolDeps>, url: &str, service: Option<&str>) -> String {
        let mut args = serde_json::json!({"repo_url": url, "ref": "main"});
        if let Some(s) = service {
            args["service"] = serde_json::json!(s);
        }
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }.execute(args, &ctx()).await;
        assert!(out.success, "open failed: {:?}", out.data);
        let repo_id = out.data["repo_id"].as_str().unwrap().to_string();
        for _ in 0..300 {
            let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::RepoStatus }
                .execute(serde_json::json!({"repo_id": repo_id}), &ctx())
                .await;
            if out.data["status"] == "ready" {
                return repo_id;
            }
            if out.data["status"] == "failed" {
                panic!("open failed: {:?}", out.data);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("not ready in 15s");
    }

    #[tokio::test]
    async fn test_open_get_read_search_list_roundtrip() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, Some("BarService")).await;

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "src/Bar.java"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["total_lines"], 4);
        assert_eq!(out.data["lines"][1]["line"], 2);
        assert_eq!(out.data["lines"][1]["text"], "public class Bar {");

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::Search }
            .execute(serde_json::json!({"repo_id": repo_id, "pattern": "class Bar", "glob": "*.java"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["matches"][0]["file"], "src/Bar.java");
        assert_eq!(out.data["matches"][0]["line"], 2);

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ListFiles }
            .execute(serde_json::json!({"repo_id": repo_id, "glob": "**/B*.java"}), &ctx())
            .await;
        assert!(out.success);
        let files: Vec<&str> = out.data["files"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(files.contains(&"src/Bar.java"));

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::GetRepo }
            .execute(serde_json::json!({"service": "BarService"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["found"], true);
        assert_eq!(out.data["repo_url"], url);
        assert_eq!(out.data["last_ref"], "main");
    }

    #[tokio::test]
    async fn test_read_unknown_repo_errors() {
        let (deps, _src, _repos, _url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": "nope", "path": "x"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "repo_not_found");
    }

    #[tokio::test]
    async fn test_open_rejects_ssh_url_and_missing_params() {
        let (deps, _src, _repos, _url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(serde_json::json!({"repo_url": "git@example.com:a/b.git", "ref": "main"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_repo_url");

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(serde_json::json!({"repo_url": "https://x.git"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
    }

    #[tokio::test]
    async fn test_register_all_six_tools() {
        let mut registry = crate::tools::registry::ToolRegistry::new();
        let repos = tempfile::tempdir().unwrap();
        let db_tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(db_tmp.path().join("friday.db")).await.unwrap();
        let deps = Arc::new(CodeToolDeps {
            manager: Arc::new(CodeRepoManager::new(repos.path().to_path_buf())),
            db: pool,
        });
        register_all(&mut registry, deps);
        let names: Vec<&str> = registry.list().iter().map(|d| d.name.as_str()).collect();
        for expected in ["code_get_repo", "code_open_repo", "code_repo_status", "code_read_file", "code_search", "code_list_files"] {
            assert!(names.contains(&expected), "missing {expected}");
            let def = registry.get(expected).unwrap();
            assert_eq!(def.category, ToolCategory::Code);
            assert_eq!(def.risk_level, RiskLevel::ReadOnly);
            assert!(!def.needs_channel);
        }
    }
}
