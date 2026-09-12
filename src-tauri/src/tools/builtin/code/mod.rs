//! code_* 工具：全部只读自主、needs_channel = false（纯本地，无环境也可用）。
//! 输出防护：单行 2000 字符截断 + lines/matches 数组 256KB 字节预算（尾部裁剪）。

use crate::app::service_repos;
use crate::code::{self, CodeRepoManager, RepoState};
use crate::tools::builtin::jvm::core::error_output;
use crate::tools::category::ToolCategory;
use crate::tools::registry::{ToolContext, ToolDef, ToolHandler, ToolOutput};
use crate::tools::risk::RiskLevel;
use async_trait::async_trait;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Instant;

/// 单行输出字符上限（防 minified 单行文件撑爆 payload）
const MAX_LINE_CHARS: usize = 2000;
/// 输出数组字节预算（read_file 的 lines / search 的 matches 序列化后上限）
const OUTPUT_BYTE_BUDGET: usize = 256 * 1024;
const BUDGET_EXCEEDED_REASON: &str =
    "output byte budget exceeded; narrow the request (offset/limit, pattern/glob, max_results)";

/// 单行截断：超 2000 字符截断 + 长度标记；返回 (文本, 是否截断)
fn cap_line(text: &str) -> (String, bool) {
    let chars = text.chars().count();
    if chars > MAX_LINE_CHARS {
        let capped: String = text.chars().take(MAX_LINE_CHARS).collect();
        (format!("{capped}… [line truncated, {chars} chars total]"), true)
    } else {
        (text.to_string(), false)
    }
}

/// 行数组 → JSON，单行截断标记累计进 any_capped
fn lines_json(lines: &[(usize, String)], any_capped: &mut bool) -> Vec<serde_json::Value> {
    lines
        .iter()
        .map(|(n, t)| {
            let (capped, was_capped) = cap_line(t);
            *any_capped |= was_capped;
            serde_json::json!({"line": n, "text": capped})
        })
        .collect()
}

/// 数组尾部裁剪进字节预算（紧凑序列化长度 = 2 括号 + 条目长度和 + 逗号数）；
/// 返回是否发生了裁剪。
fn trim_to_budget(entries: &mut Vec<serde_json::Value>) -> bool {
    let lens: Vec<usize> = entries
        .iter()
        .map(|v| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0))
        .collect();
    let mut sum: usize = lens.iter().sum();
    let mut n = lens.len();
    while n > 0 && 2 + (n - 1) + sum > OUTPUT_BYTE_BUDGET {
        sum -= lens[n - 1];
        n -= 1;
    }
    if n < lens.len() {
        entries.truncate(n);
        true
    } else {
        false
    }
}

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

fn clamp_usize(v: Option<i64>, default: usize, max: usize) -> usize {
    match v {
        Some(n) if n >= 1 => (n as usize).min(max),
        _ => default,
    }
}

#[async_trait]
impl ToolHandler for CodeToolHandler {
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let start = Instant::now();
        tracing::info!(session_id = %ctx.session_id, kind = ?self.kind, "code tool executing");
        let out = match self.kind {
            CodeToolKind::GetRepo => self.get_repo(&args, ctx).await,
            CodeToolKind::OpenRepo => self.open_repo(&args, ctx).await,
            CodeToolKind::RepoStatus => self.repo_status(&args, ctx).await,
            CodeToolKind::ReadFile => self.read_file(&args, ctx).await,
            CodeToolKind::Search => self.search(&args, ctx).await,
            CodeToolKind::ListFiles => self.list_files(&args, ctx).await,
        };
        tracing::info!(
            session_id = %ctx.session_id,
            kind = ?self.kind,
            success = out.success,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "code tool finished"
        );
        out
    }
}

impl CodeToolHandler {
    /// 错误统一出口：warn 留痕（session/kind/error_code）+ 结构化 error_output
    fn fail(&self, ctx: &ToolContext, code: &str, message: &str) -> ToolOutput {
        tracing::warn!(session_id = %ctx.session_id, kind = ?self.kind, error_code = code, "code tool failed");
        error_output(code, message)
    }

    /// search.rs 的路径/glob 错误 → 结构化错误码（glob 无效 / 逃逸 / 未找到区分）
    fn path_error(&self, ctx: &ToolContext, e: String) -> ToolOutput {
        if e.contains("invalid glob") {
            self.fail(ctx, "glob_invalid", &e)
        } else if e.contains("escapes") {
            self.fail(ctx, "path_outside_repo", &e)
        } else {
            self.fail(ctx, "path_not_found", &e)
        }
    }

    fn require_str(&self, ctx: &ToolContext, args: &serde_json::Value, key: &str) -> Result<String, ToolOutput> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| self.fail(ctx, "invalid_params", &format!("missing required parameter: {key}")))
    }

    async fn get_repo(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let service = match self.require_str(ctx, args, "service") {
            Ok(v) => v,
            Err(out) => return out,
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
            Err(e) => self.fail(ctx, "db_error", &e.to_string()),
        }
    }

    async fn open_repo(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let repo_url = match self.require_str(ctx, args, "repo_url") {
            Ok(v) => v,
            Err(out) => return out,
        };
        let git_ref = match self.require_str(ctx, args, "ref") {
            Ok(v) => v,
            Err(out) => return out,
        };
        if let Err(e) = code::git::validate_git_ref(&git_ref) {
            return self.fail(ctx, "invalid_params", &format!("invalid ref: {e}"));
        }
        if let Err(e) = code::git::validate_repo_url(&repo_url) {
            return self.fail(ctx, "invalid_repo_url", &e);
        }
        if !code::git::git_available() {
            return self.fail(
                ctx,
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

    async fn repo_status(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let repo_id = match self.require_str(ctx, args, "repo_id") {
            Ok(v) => v,
            Err(out) => return out,
        };
        match self.deps.manager.status(&repo_id).await {
            Some(st) => ToolOutput { success: true, data: state_json(&st), raw_stdout: None },
            None => self.fail(ctx, "repo_not_found", "unknown repo_id; call code_open_repo first"),
        }
    }

    async fn resolve_worktree(&self, ctx: &ToolContext, repo_id: &str) -> Result<std::path::PathBuf, ToolOutput> {
        match self.deps.manager.worktree(repo_id).await {
            Ok(p) => Ok(p),
            Err(code) if code == "repo_not_found" => {
                Err(self.fail(ctx, "repo_not_found", "unknown repo_id; call code_open_repo first"))
            }
            Err(not_ready) => {
                // manager 返回 "repo_not_ready: <phase>"；剥前缀取 phase（无前缀时防御性用整串）
                let phase = not_ready.strip_prefix("repo_not_ready: ").unwrap_or(&not_ready);
                if phase == "failed" {
                    // 终态 failed：轮询无意义，指引改为查错误码后换 ref 重开
                    Err(self.fail(
                        ctx,
                        "repo_failed",
                        "repo open failed; check code_repo_status error_code and re-open with a corrected ref (branch/tag/commit id)",
                    ))
                } else {
                    Err(self.fail(
                        ctx,
                        "repo_not_ready",
                        &format!("repo is {phase}; poll code_repo_status until ready, do NOT re-open"),
                    ))
                }
            }
        }
    }

    async fn read_file(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let repo_id = match self.require_str(ctx, args, "repo_id") {
            Ok(v) => v,
            Err(out) => return out,
        };
        let path = match self.require_str(ctx, args, "path") {
            Ok(v) => v,
            Err(out) => return out,
        };
        let offset = clamp_usize(args.get("offset").and_then(|v| v.as_i64()), 1, usize::MAX);
        let limit = clamp_usize(args.get("limit").and_then(|v| v.as_i64()), code::search::DEFAULT_READ_LINES, code::search::MAX_READ_LINES);
        let wt = match self.resolve_worktree(ctx, &repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::read_file(&wt, &path, offset, limit) {
            Ok(out) => {
                let mut lines_truncated = false;
                let mut lines = lines_json(&out.lines, &mut lines_truncated);
                let budget_exceeded = trim_to_budget(&mut lines);
                // limit 报告实际返回行数：预算裁剪后随之下调（search.rs 的 limit 本就 = 返回行数）
                let mut limit = out.limit;
                if budget_exceeded {
                    limit = lines.len();
                }
                let mut data = serde_json::json!({
                    "repo_id": repo_id,
                    "path": path,
                    "total_lines": out.total_lines,
                    "offset": out.offset,
                    "limit": limit,
                    "truncated": out.truncated || budget_exceeded,
                    "lines": lines,
                });
                if lines_truncated {
                    data["lines_truncated"] = serde_json::json!(true);
                }
                if budget_exceeded {
                    data["truncated_reason"] = serde_json::json!(BUDGET_EXCEEDED_REASON);
                }
                ToolOutput { success: true, data, raw_stdout: None }
            }
            Err(e) => self.path_error(ctx, e),
        }
    }

    async fn search(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let repo_id = match self.require_str(ctx, args, "repo_id") {
            Ok(v) => v,
            Err(out) => return out,
        };
        let pattern = match self.require_str(ctx, args, "pattern") {
            Ok(v) => v,
            Err(out) => return out,
        };
        let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        // context=0 是合法的 grep 式请求（不带上下文）；负数/缺省回默认
        let context = match args.get("context").and_then(|v| v.as_i64()) {
            Some(0) => 0,
            Some(n) if n >= 1 => (n as usize).min(code::search::MAX_CONTEXT_LINES),
            _ => code::search::DEFAULT_CONTEXT_LINES,
        };
        let max_results = clamp_usize(args.get("max_results").and_then(|v| v.as_i64()), code::search::DEFAULT_MAX_RESULTS, code::search::MAX_RESULTS_CAP);
        let wt = match self.resolve_worktree(ctx, &repo_id).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match code::search::search(&wt, &pattern, glob.as_deref(), context, max_results) {
            Ok(out) => {
                let mut lines_truncated = false;
                let mut matches = Vec::with_capacity(out.matches.len());
                for m in &out.matches {
                    let (text, text_capped) = cap_line(&m.text);
                    let mut capped = text_capped;
                    let before = lines_json(&m.context_before, &mut capped);
                    let after = lines_json(&m.context_after, &mut capped);
                    lines_truncated |= capped;
                    matches.push(serde_json::json!({
                        "file": m.file,
                        "line": m.line,
                        "text": text,
                        "context_before": before,
                        "context_after": after,
                    }));
                }
                let budget_exceeded = trim_to_budget(&mut matches);
                let mut data = serde_json::json!({
                    "repo_id": repo_id,
                    "pattern": pattern,
                    "truncated": out.truncated || budget_exceeded,
                    "matches": matches,
                });
                if lines_truncated {
                    data["lines_truncated"] = serde_json::json!(true);
                }
                if budget_exceeded {
                    data["truncated_reason"] = serde_json::json!(BUDGET_EXCEEDED_REASON);
                }
                ToolOutput { success: true, data, raw_stdout: None }
            }
            Err(e) if e.contains("invalid glob") => self.fail(ctx, "glob_invalid", &e),
            Err(e) => self.fail(ctx, "pattern_invalid", &e),
        }
    }

    async fn list_files(&self, args: &serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let repo_id = match self.require_str(ctx, args, "repo_id") {
            Ok(v) => v,
            Err(out) => return out,
        };
        let path = args.get("path").and_then(|v| v.as_str()).map(|s| s.to_string());
        let glob = args.get("glob").and_then(|v| v.as_str()).map(|s| s.to_string());
        let wt = match self.resolve_worktree(ctx, &repo_id).await {
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
            Err(e) => self.path_error(ctx, e),
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
        "轮询 code_open_repo 进度：status = cloning / fetching / ready / failed。progress 为 git 进度百分比；ready 的 resolved_commit 为检出 commit，stale_warning 非空表示 fetch 失败降级用本地缓存（要告知用户代码可能非最新）。failed 看 error_code：clone_failed / fetch_failed / worktree_failed / invalid_ref / git_not_found。",
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
        "读仓内源码文件，输出 1-based 行号。线程栈/堆栈帧行号可直接对上。大文件分页：offset 起始行（默认 1）、limit 行数（默认 2000 上限 5000）。定位类文件先用 code_list_files(glob) 或 code_search。单行超 2000 字符截断（置 lines_truncated）；输出超 256KB 预算时尾部裁剪并附 truncated_reason。",
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
        "仓内正则搜索（尊重 .gitignore，跳过二进制/超大文件），返回 文件+行号+匹配行+上下文。定位类定义用 pattern=\"class Foo\"；glob 过滤文件类型如 \"*.java\"。context 上下文行数默认 2（0 = 不带上下文），max_results 默认 100。默认跳过隐藏文件（dot 开头，同 ripgrep）；结果按文件遍历序返回，截断时收窄 pattern/glob 后重试。单行超 2000 字符截断（置 lines_truncated）；matches 超 256KB 预算时尾部裁剪并附 truncated_reason。",
        serde_json::json!({
            "type": "object",
            "properties": {
                "repo_id": { "type": "string" },
                "pattern": { "type": "string", "description": "正则表达式" },
                "glob": { "type": "string", "description": "文件白名单 glob（如 *.java、**/Bar.java）" },
                "context": { "type": "number", "description": "上下文行数（默认 2，0 = 无上下文，上限 10）" },
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
    use std::path::Path;
    use std::time::Duration;

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

    // ---- 审查修复：输出预算 / 单行截断 / clamp / 错误码（TDD：先红后绿） ----

    fn git_cmd(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git binary required for tests");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 自定义内容 fixture 仓（单 main 提交），供大文件预算/截断测试
    async fn repo_with_files(files: &[(&str, String)]) -> (Arc<CodeToolDeps>, tempfile::TempDir, tempfile::TempDir, String) {
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(src.path()).unwrap();
        git_cmd(src.path(), &["init", "-b", "main"]);
        git_cmd(src.path(), &["config", "user.email", "friday@test.local"]);
        git_cmd(src.path(), &["config", "user.name", "friday-test"]);
        git_cmd(src.path(), &["config", "commit.gpgsign", "false"]);
        for (name, content) in files {
            let p = src.path().join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, content).unwrap();
        }
        git_cmd(src.path(), &["add", "."]);
        git_cmd(src.path(), &["commit", "-m", "init"]);
        let url = crate::code::git::file_url(src.path());
        let repos = tempfile::tempdir().unwrap();
        let db_tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(db_tmp.path().join("friday.db")).await.unwrap();
        let deps = Arc::new(CodeToolDeps {
            manager: Arc::new(CodeRepoManager::new(repos.path().to_path_buf())),
            db: pool,
        });
        (deps, src, repos, url)
    }

    /// (a) 单行 50K 字符 → 截到 2000 + 标记，置 lines_truncated
    #[tokio::test]
    async fn test_read_file_caps_long_line() {
        let (deps, _src, _repos, url) = repo_with_files(&[("big.txt", format!("{}\n", "x".repeat(50_000)))]).await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "big.txt"}), &ctx())
            .await;
        assert!(out.success, "out: {:?}", out.data);
        let text = out.data["lines"][0]["text"].as_str().unwrap();
        assert!(text.starts_with(&"x".repeat(2000)), "text must start with the first 2000 chars");
        assert!(text.ends_with("… [line truncated, 50000 chars total]"));
        assert_eq!(out.data["lines_truncated"], true);
        assert_eq!(out.data["truncated"], false, "single capped line fits the byte budget");
    }

    /// (b) 5000 行超预算 → 尾部裁剪进 256KB，limit == 实际返回行数。
    /// 审查建议 5000×4KB 行，但 search.rs MAX_FILE_BYTES=5MB 拒读 >5MB 文件，
    /// 故缩至 1000 字符/行（≈5MB 内）；全量序列化 ≈5MB 仍远超 256KB，裁剪必然触发。
    #[tokio::test]
    async fn test_read_file_byte_budget_trims_to_256k() {
        let content = format!("{}\n", "y".repeat(1000)).repeat(5000);
        let (deps, _src, _repos, url) = repo_with_files(&[("wide.log", content)]).await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "wide.log", "limit": 5000}), &ctx())
            .await;
        assert!(out.success, "out: {:?}", out.data);
        let lines = out.data["lines"].as_array().unwrap();
        assert!(lines.len() < 5000, "byte budget must trim, got {} lines", lines.len());
        assert_eq!(out.data["truncated"], true);
        assert!(out.data["truncated_reason"].as_str().unwrap().contains("byte budget"));
        assert_eq!(out.data["limit"].as_u64().unwrap(), lines.len() as u64);
        assert_eq!(out.data["total_lines"], 5000);
        assert_eq!(lines[0]["line"], 1, "trim drops from the end, first line retained");
    }

    /// (c) 500 匹配 × 4096 字符行 → matches 尾部裁剪进 256KB
    #[tokio::test]
    async fn test_search_byte_budget_trims_matches() {
        let mut content = String::new();
        for i in 0..500 {
            content.push_str(&format!("MATCH{i:03} {}\n", "z".repeat(4090)));
        }
        let (deps, _src, _repos, url) = repo_with_files(&[("hits.log", content)]).await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::Search }
            .execute(serde_json::json!({"repo_id": repo_id, "pattern": "MATCH", "max_results": 500}), &ctx())
            .await;
        assert!(out.success, "out: {:?}", out.data);
        let matches = out.data["matches"].as_array().unwrap();
        assert!(matches.len() < 500, "byte budget must trim, got {} matches", matches.len());
        assert_eq!(out.data["truncated"], true);
        assert!(out.data["truncated_reason"].as_str().unwrap().contains("byte budget"));
        assert_eq!(matches[0]["line"], 1, "trim drops from the end, first match retained");
        assert_eq!(out.data["lines_truncated"], true, "4096-char lines and context must be capped");
    }

    /// context=0 是合法 grep 式请求：无上下文行
    #[tokio::test]
    async fn test_search_context_zero_returns_no_context_lines() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::Search }
            .execute(serde_json::json!({"repo_id": repo_id, "pattern": "class Bar", "context": 0}), &ctx())
            .await;
        assert!(out.success, "out: {:?}", out.data);
        assert_eq!(out.data["matches"][0]["line"], 2);
        assert_eq!(out.data["matches"][0]["context_before"].as_array().unwrap().len(), 0);
        assert_eq!(out.data["matches"][0]["context_after"].as_array().unwrap().len(), 0);
    }

    /// glob 语法错误 → glob_invalid（search 与 list_files 一致）
    #[tokio::test]
    async fn test_invalid_glob_reports_glob_invalid() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::Search }
            .execute(serde_json::json!({"repo_id": repo_id, "pattern": "class", "glob": "["}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "glob_invalid");

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ListFiles }
            .execute(serde_json::json!({"repo_id": repo_id, "glob": "["}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "glob_invalid");
    }

    /// 拆分后的缺失参数必须指名道姓（read_file 缺 path / search 缺 pattern）
    #[tokio::test]
    async fn test_read_file_missing_path_specific_message() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "invalid_params");
        assert_eq!(out.data["message"], "missing required parameter: path");

        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::Search }
            .execute(serde_json::json!({"repo_id": repo_id}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["message"], "missing required parameter: pattern");
    }

    /// clamp 边界：offset=0→1；limit=0→默认 2000；limit=99999→上限 5000；缺省/负数→默认
    #[test]
    fn test_clamp_usize_edges() {
        assert_eq!(clamp_usize(Some(0), 1, usize::MAX), 1);
        assert_eq!(clamp_usize(Some(0), code::search::DEFAULT_READ_LINES, code::search::MAX_READ_LINES), 2000);
        assert_eq!(clamp_usize(Some(99999), code::search::DEFAULT_READ_LINES, code::search::MAX_READ_LINES), 5000);
        assert_eq!(clamp_usize(None, 2, 10), 2);
        assert_eq!(clamp_usize(Some(-3), 2, 10), 2);
    }

    /// offset=0 经 handler 钳到 1（clamp 边界的端到端验证）
    #[tokio::test]
    async fn test_read_file_offset_zero_clamped_to_one() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "src/Bar.java", "offset": 0}), &ctx())
            .await;
        assert!(out.success, "out: {:?}", out.data);
        assert_eq!(out.data["offset"], 1);
        assert_eq!(out.data["lines"][0]["line"], 1);
        assert_eq!(out.data["total_lines"], 4);
    }

    /// 审查修复 #1：ref 以 `-` 开头会被 git 当选项解析（`--upload-pack=` 配
    /// file:// 远端可执行任意程序）→ invalid_params，不得进入 manager.open
    #[tokio::test]
    async fn test_open_rejects_option_injection_ref() {
        let repos = tempfile::tempdir().unwrap();
        let db_tmp = tempfile::tempdir().unwrap();
        let pool = crate::infra::db::init(db_tmp.path().join("friday.db")).await.unwrap();
        let deps = Arc::new(CodeToolDeps {
            manager: Arc::new(CodeRepoManager::new(repos.path().to_path_buf())),
            db: pool,
        });
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(
                serde_json::json!({"repo_url": "file:///C:/friday-no-such-repo", "ref": "--upload-pack=evil"}),
                &ctx(),
            )
            .await;
        assert!(!out.success, "option-injection ref must be rejected, got: {:?}", out.data);
        assert_eq!(out.data["error"], "invalid_params");
        assert!(out.data["message"].as_str().unwrap().contains("invalid ref"));
    }

    /// READY 仓上 `..` 逃逸 → path_outside_repo
    #[tokio::test]
    async fn test_read_file_escape_rejected_on_ready_repo() {
        let (deps, _src, _repos, url) = test_deps().await;
        let repo_id = open_until_ready(&deps, &url, None).await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "../"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "path_outside_repo");
    }

    /// get_repo 未命中 → found:false + hint
    #[tokio::test]
    async fn test_get_repo_not_found_returns_found_false() {
        let (deps, _src, _repos, _url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::GetRepo }
            .execute(serde_json::json!({"service": "NoSuchService"}), &ctx())
            .await;
        assert!(out.success);
        assert_eq!(out.data["found"], false);
        assert!(out.data["hint"].as_str().is_some());
    }

    /// repo_failed 确定性触发：无效 ref → 终态 failed（worktree_path None）→ 读工具报 failed + 换 ref 重开指引
    #[tokio::test]
    async fn test_read_file_on_failed_repo_reports_repo_failed() {
        let (deps, _src, _repos, url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(serde_json::json!({"repo_url": url, "ref": "no-such-branch"}), &ctx())
            .await;
        assert!(out.success, "open failed: {:?}", out.data);
        let repo_id = out.data["repo_id"].as_str().unwrap().to_string();
        let mut failed = false;
        for _ in 0..300 {
            let st = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::RepoStatus }
                .execute(serde_json::json!({"repo_id": repo_id}), &ctx())
                .await;
            let status = st.data["status"].as_str().unwrap_or("");
            if status == "failed" {
                failed = true;
                break;
            }
            assert_ne!(status, "ready", "invalid ref must not become ready");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(failed, "not failed in 15s");
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "src/Bar.java"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "repo_failed");
        assert!(out.data["message"].as_str().unwrap().contains("re-open with a corrected ref"));
    }

    /// cloning 阶段读工具 → repo_not_ready + 自然语言 phase（无 "repo_not_ready: " 前缀泄漏）。
    /// current_thread 运行时下 open 后无 yield 点，首个读请求必先于管线任务执行——确定性非竞态。
    #[tokio::test]
    async fn test_read_file_during_cloning_reports_repo_not_ready() {
        let (deps, _src, _repos, url) = test_deps().await;
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::OpenRepo }
            .execute(serde_json::json!({"repo_url": url, "ref": "main"}), &ctx())
            .await;
        assert!(out.success, "open failed: {:?}", out.data);
        let repo_id = out.data["repo_id"].as_str().unwrap().to_string();
        // 不 sleep 直接读：phase 必为 cloning
        let out = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::ReadFile }
            .execute(serde_json::json!({"repo_id": repo_id, "path": "src/Bar.java"}), &ctx())
            .await;
        assert!(!out.success);
        assert_eq!(out.data["error"], "repo_not_ready");
        let message = out.data["message"].as_str().unwrap();
        assert!(message.contains("repo is cloning"), "message should read naturally, got: {message}");
        assert!(!message.contains("repo_not_ready:"), "manager prefix must not leak, got: {message}");
        // 等终态再结束，避免 tempdir 删除与后台 clone 竞争
        let mut ready = false;
        for _ in 0..300 {
            let st = CodeToolHandler { deps: deps.clone(), kind: CodeToolKind::RepoStatus }
                .execute(serde_json::json!({"repo_id": repo_id}), &ctx())
                .await;
            let status = st.data["status"].as_str().unwrap_or("");
            if status == "ready" {
                ready = true;
                break;
            }
            if status == "failed" {
                panic!("clone failed: {:?}", st.data);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ready, "not ready in 15s");
    }
}
