//! CodeRepoManager：clone 缓存（完整克隆）+ 每 (repo, ref) 一个 worktree +
//! 异步 open 管线（同 jfr_record 模式：立即返回、后台任务、轮询状态）。
//!
//! 生命周期不做定时回收：clone/worktree 落盘持久、跨会话复用（重开秒级），
//! 磁盘增长出口在设置页缓存管理（delete_cache）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use super::git;

pub const CLONE_TIMEOUT_SECS: u64 = 1800;
pub const FETCH_TIMEOUT_SECS: u64 = 300;
pub const GIT_TIMEOUT_SECS: u64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoPhase {
    Cloning,
    Fetching,
    Ready,
    Failed,
}

impl RepoPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoPhase::Cloning => "cloning",
            RepoPhase::Fetching => "fetching",
            RepoPhase::Ready => "ready",
            RepoPhase::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, RepoPhase::Ready | RepoPhase::Failed)
    }
}

#[derive(Clone, Debug)]
pub struct RepoState {
    pub repo_id: String,
    pub url_hash: String,
    pub repo_url: String,
    pub git_ref: String,
    pub phase: RepoPhase,
    /// git 进度（如 "Receiving objects: 45%"）
    pub progress: Option<String>,
    pub resolved_commit: Option<String>,
    /// fetch 失败但本地有该 ref：降级用本地缓存
    pub stale_warning: Option<String>,
    pub error_code: Option<String>,
    pub error: Option<String>,
    /// Some = worktree 已检出（读工具可用的最低条件）
    pub worktree_path: Option<PathBuf>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RepoCacheEntry {
    pub url_hash: String,
    pub repo_url: String,
    pub disk_bytes: u64,
    pub worktrees: usize,
}

struct ManagerInner {
    repos_dir: PathBuf,
    states: Mutex<HashMap<String, RepoState>>,
    /// 同 URL（共享主 clone）管线串行化锁：并发 open 不同 ref 时防
    /// clone/fetch/worktree 互踩（如双 clone 同目录 "already exists"）。
    /// 条目按 url_hash 只增不减：上界为打开过的不同 URL 数（量级极小），不回收。
    pipeline_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

#[derive(Clone)]
pub struct CodeRepoManager {
    inner: Arc<ManagerInner>,
}

impl CodeRepoManager {
    pub fn new(repos_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                repos_dir,
                states: Mutex::new(HashMap::new()),
                pipeline_locks: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn repo_root(&self, url_hash: &str) -> PathBuf {
        self.inner.repos_dir.join(url_hash)
    }

    async fn url_lock(&self, url_hash: &str) -> Arc<Mutex<()>> {
        let mut locks = self.inner.pipeline_locks.lock().await;
        locks
            .entry(url_hash.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn clone_dir(&self, url_hash: &str) -> PathBuf {
        self.repo_root(url_hash).join("clone")
    }

    fn worktree_dir(&self, url_hash: &str, git_ref: &str) -> PathBuf {
        self.repo_root(url_hash).join("wt").join(git::ref_hash(git_ref))
    }

    /// 打开仓（幂等）：进行中直接返回同 id；已就绪仍触发一次 fetch 刷新。
    /// 返回 repo_id，状态经 status() 轮询。
    pub async fn open(&self, repo_url: &str, git_ref: &str) -> String {
        let id = git::repo_id(repo_url, git_ref);
        let uh = git::url_hash(repo_url);
        {
            // in-flight 去重 + 占位在同一 states 锁内原子完成：并发同
            // (url,ref) open 不再有双 spawn 窗口（Ready 刷新 respawn 仍
            // 允许——管线幂等，串行重跑无害）
            let mut states = self.inner.states.lock().await;
            if let Some(st) = states.get(&id) {
                if st.phase == RepoPhase::Cloning || st.phase == RepoPhase::Fetching {
                    return id; // in-flight 去重
                }
            }
            states.insert(
                id.clone(),
                RepoState {
                    repo_id: id.clone(),
                    url_hash: uh.clone(),
                    repo_url: repo_url.to_string(),
                    git_ref: git_ref.to_string(),
                    phase: RepoPhase::Cloning,
                    progress: None,
                    resolved_commit: None,
                    stale_warning: None,
                    error_code: None,
                    error: None,
                    worktree_path: Some(self.worktree_dir(&uh, git_ref)).filter(|p| p.exists()),
                    created_at: chrono::Utc::now(),
                },
            );
        }
        let mgr = self.clone();
        let rid = id.clone();
        tokio::spawn(async move {
            run_pipeline(mgr, rid).await;
        });
        id
    }

    pub async fn status(&self, repo_id: &str) -> Option<RepoState> {
        self.inner.states.lock().await.get(repo_id).cloned()
    }

    /// 读工具入口：worktree 已检出即可读，不阻塞并发读。读取期间若发生刷新
    /// checkout 切换，可能观察到单文件 old-or-new、跨文件可能混合版本——
    /// 设计接受的权衡。
    pub async fn worktree(&self, repo_id: &str) -> Result<PathBuf, String> {
        let st = self.inner.states.lock().await.get(repo_id).cloned();
        match st {
            None => Err("repo_not_found".to_string()),
            Some(st) => match st.worktree_path {
                Some(p) if p.exists() => Ok(p),
                _ => Err(format!("repo_not_ready: {}", st.phase.as_str())),
            },
        }
    }

    /// 缓存仓库列表（设置页）：目录扫描 + url 标记 + 磁盘占用
    pub async fn list_cache(&self) -> Vec<RepoCacheEntry> {
        let mut entries = Vec::new();
        let Ok(dir) = std::fs::read_dir(&self.inner.repos_dir) else {
            return entries;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let url_hash = entry.file_name().to_string_lossy().to_string();
            let repo_url = std::fs::read_to_string(path.join("url.txt"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| format!("<unknown {}>", url_hash));
            let worktrees = std::fs::read_dir(path.join("wt"))
                .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
                .unwrap_or(0);
            // 递归磁盘扫描放阻塞线程池：大仓缓存可达数 GB，避免卡异步运行时
            let scan_path = path.clone();
            let disk_bytes = tokio::task::spawn_blocking(move || dir_size(&scan_path))
                .await
                .unwrap_or(0);
            entries.push(RepoCacheEntry {
                url_hash,
                repo_url,
                disk_bytes,
                worktrees,
            });
        }
        entries.sort_by(|a, b| b.disk_bytes.cmp(&a.disk_bytes));
        entries
    }

    /// 删除某仓全部缓存（主 clone + worktrees）。有进行中任务时拒绝。
    pub async fn delete_cache(&self, url_hash: &str) -> Result<(), String> {
        // url_hash 会拼进路径：先做格式校验，防 `..` 等路径穿越触达仓缓存目录之外
        if url_hash.len() != 16 || !url_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("invalid url_hash: {url_hash}"));
        }
        // 快路径：明显忙直接拒绝，不取管线锁（免无谓等待）
        if self.url_busy(url_hash).await {
            return Err("repo busy (cloning/fetching in progress); retry later".to_string());
        }
        // 与管线互斥：持 URL 管线锁删除，杜绝 remove_dir_all 与 clone/fetch
        // 交错产生半截主 clone（.git 在、内容缺 → 后续 open 跳过 clone 直接
        // fetch_failed，且无法自愈）。锁等待期间新 spawn 的管线会在锁释放后
        // clone 到干净目录；其非 terminal 状态在持锁复查中可见 → 此时拒绝删除
        let url_lock = self.url_lock(url_hash).await;
        let _guard = url_lock.lock().await;
        self.take_url_states_if_idle(url_hash).await?;
        let root = self.repo_root(url_hash);
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|e| format!("failed to delete cache: {e}"))?;
            tracing::info!(url_hash, ?root, "repo cache deleted");
        }
        Ok(())
    }

    /// 该 URL 是否有进行中（非 terminal）open 管线
    async fn url_busy(&self, url_hash: &str) -> bool {
        let states = self.inner.states.lock().await;
        states.values().any(|st| st.url_hash == url_hash && !st.phase.is_terminal())
    }

    /// busy 复查 + 状态清除（单次 states 锁内原子完成）：该 URL 存在非
    /// terminal 状态 → Err(busy)；否则移除该 URL 全部状态（含 terminal 的）
    async fn take_url_states_if_idle(&self, url_hash: &str) -> Result<(), String> {
        let mut states = self.inner.states.lock().await;
        if states.values().any(|st| st.url_hash == url_hash && !st.phase.is_terminal()) {
            return Err("repo busy (cloning/fetching in progress); retry later".to_string());
        }
        states.retain(|_, st| st.url_hash != url_hash);
        Ok(())
    }

    async fn set_phase(&self, repo_id: &str, phase: RepoPhase) {
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.phase = phase;
            st.progress = None;
        }
    }

    async fn set_progress(&self, repo_id: &str, progress: String) {
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.progress = Some(progress);
        }
    }

    async fn fail(&self, repo_id: &str, error_code: &str, error: String) {
        tracing::warn!(repo_id, error_code, error = %error, "code repo open failed");
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.phase = RepoPhase::Failed;
            st.error_code = Some(error_code.to_string());
            st.error = Some(error);
            st.progress = None;
        }
    }

    async fn set_ready(
        &self,
        repo_id: &str,
        resolved_commit: String,
        worktree: PathBuf,
        stale_warning: Option<String>,
    ) {
        if let Some(st) = self.inner.states.lock().await.get_mut(repo_id) {
            st.phase = RepoPhase::Ready;
            st.resolved_commit = Some(resolved_commit);
            st.worktree_path = Some(worktree);
            st.stale_warning = stale_warning;
            st.progress = None;
            st.error_code = None;
            st.error = None;
        }
    }
}

/// open 三分支管线：①无主 clone → 后台 clone；②有 clone → fetch 刷新（best
/// effort，失败降级本地缓存）；③ref 解析（分支/tag/commitid）→ worktree 建/检出。
async fn run_pipeline(mgr: CodeRepoManager, repo_id: String) {
    let st = mgr.inner.states.lock().await.get(&repo_id).cloned();
    let Some(st) = st else {
        tracing::debug!(repo_id, "pipeline aborted: state removed before start");
        return;
    };
    tracing::info!(repo_id, url = %st.repo_url, git_ref = %st.git_ref, "code repo open pipeline started");
    // 同 URL 管线全程序串行：clone/fetch/resolve/worktree 均作用于共享主
    // clone，无锁并发会互踩（clone 同目录冲突、worktree 元数据竞争）；
    // 不同 URL 互不影响。锁在管线结束时释放。
    let url_lock = mgr.url_lock(&st.url_hash).await;
    let _url_guard = url_lock.lock().await;
    let url = st.repo_url;
    let git_ref = st.git_ref;
    let uh = st.url_hash;
    let root = mgr.repo_root(&uh);
    let clone_dir = mgr.clone_dir(&uh);
    let wt_dir = mgr.worktree_dir(&uh, &git_ref);
    if let Err(e) = std::fs::create_dir_all(root.join("wt")) {
        tracing::debug!(repo_id, error = %e, "wt dir create failed (best-effort)");
    }
    // url 标记（缓存列表反查仓地址；clone 前写，半截 clone 也能识别）
    if let Err(e) = std::fs::write(root.join("url.txt"), &url) {
        tracing::debug!(repo_id, error = %e, "url.txt write failed (best-effort)");
    }
    let clone_str = clone_dir.display().to_string();
    let wt_str = wt_dir.display().to_string();

    // ① 主 clone 不存在 → 完整克隆
    let mut cloned_now = false;
    if !clone_dir.join(".git").exists() {
        cloned_now = true;
        mgr.set_phase(&repo_id, RepoPhase::Cloning).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let fwd = {
            let mgr = mgr.clone();
            let rid = repo_id.clone();
            tokio::spawn(async move {
                while let Some(p) = rx.recv().await {
                    mgr.set_progress(&rid, p).await;
                }
            })
        };
        let res = git::run_git_streaming(
            &["clone", &url, &clone_str],
            None,
            Duration::from_secs(CLONE_TIMEOUT_SECS),
            tx,
        )
        .await;
        // 等进度转发任务排空收尾（tx 已随 run_git_streaming 返回关闭 channel，
        // fwd 很快退出）：防止缓冲中的残留进度事件落在终态之后
        let _ = fwd.await;
        if !res.success {
            mgr.fail(&repo_id, "clone_failed", res.stderr.trim().to_string()).await;
            return;
        }
    }

    // ② fetch 刷新（分支经常变；best effort）。刚完成 clone 或 full-40-hex
    // commitid 跳过：前者缓存必新；后者对象不可变，本地 resolve 即答案
    // （https 上 fetch <sha> 几乎必被拒——unadvertised object，会产生误导
    // 性 stale 告警）
    let mut stale_warning: Option<String> = None;
    let mut fetch_error: Option<String> = None;
    if should_fetch(&git_ref, cloned_now) {
        mgr.set_phase(&repo_id, RepoPhase::Fetching).await;
        let fetch = git::run_git(
            &["-C", &clone_str, "fetch", "--force", "origin", &git_ref],
            Some(&clone_dir),
            Duration::from_secs(FETCH_TIMEOUT_SECS),
        )
        .await;
        if !fetch.success {
            fetch_error = Some(fetch.stderr.trim().to_string());
            stale_warning = Some(format!(
                "fetch failed ({}); using locally cached code which may be stale",
                fetch_error.as_deref().unwrap_or("").lines().last().unwrap_or("network error")
            ));
        }
    }

    // ③ ref 解析：分支（本地或 origin/）→ tag → commitid
    let Some(sha) = git::resolve_ref(&clone_dir, &git_ref).await else {
        // 失败语义区分：fetch 跑过且远端确实没有该 ref（couldn't find
        // remote ref）→ invalid_ref；fetch 跑过但网络/认证失败 →
        // fetch_failed；fetch 被跳过（刚 clone 完 / commitid，连通性未知）
        // → invalid_ref
        let (code, detail) = match &fetch_error {
            None => ("invalid_ref", "absent on remote".to_string()),
            Some(err) if err.to_lowercase().contains("couldn't find remote ref") => {
                ("invalid_ref", "absent on remote".to_string())
            }
            Some(err) => ("fetch_failed", format!("fetch error: {err}")),
        };
        mgr.fail(
            &repo_id,
            code,
            format!(
                "ref '{git_ref}' not found ({detail}); \
                 confirm branch/tag/commitid with the user, or check network/credentials"
            ),
        )
        .await;
        return;
    };

    // ④ worktree：残损缓存自动重建
    let ok = if wt_dir.exists() {
        let res = git::run_git(
            &["-C", &wt_str, "checkout", "--force", &sha],
            Some(&wt_dir),
            Duration::from_secs(GIT_TIMEOUT_SECS),
        )
        .await;
        if res.success {
            true
        } else {
            tracing::warn!(repo_id, ?wt_dir, "worktree checkout failed, rebuilding");
            let _ = std::fs::remove_dir_all(&wt_dir);
            let _ = git::run_git(
                &["-C", &clone_str, "worktree", "prune"],
                Some(&clone_dir),
                Duration::from_secs(GIT_TIMEOUT_SECS),
            )
            .await;
            git::run_git(
                &["-C", &clone_str, "worktree", "add", "--detach", &wt_str, &sha],
                Some(&clone_dir),
                Duration::from_secs(GIT_TIMEOUT_SECS),
            )
            .await
            .success
        }
    } else {
        let _ = git::run_git(
            &["-C", &clone_str, "worktree", "prune"],
            Some(&clone_dir),
            Duration::from_secs(GIT_TIMEOUT_SECS),
        )
        .await;
        git::run_git(
            &["-C", &clone_str, "worktree", "add", "--detach", &wt_str, &sha],
            Some(&clone_dir),
            Duration::from_secs(GIT_TIMEOUT_SECS),
        )
        .await
        .success
    };
    if !ok {
        mgr.fail(&repo_id, "worktree_failed", format!("failed to prepare worktree at {wt_str}")).await;
        return;
    }

    mgr.set_ready(&repo_id, sha, wt_dir, stale_warning).await;
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(dir) = std::fs::read_dir(path) {
        for entry in dir.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// fetch 刷新决策：刚完成完整 clone → 缓存必新，跳过；full-40-hex
/// commitid → 对象不可变，本地 resolve 即答案（https 上 fetch <sha> 几乎
/// 必被拒——unadvertised object，会产生误导性 stale 告警），跳过；命名 ref
/// （分支/tag）且 clone 已存在 → 刷新（分支经常变）
fn should_fetch(git_ref: &str, cloned_now: bool) -> bool {
    if cloned_now {
        return false;
    }
    let is_full_sha = git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit());
    !is_full_sha
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_terminal(mgr: &CodeRepoManager, repo_id: &str) -> RepoState {
        for _ in 0..1200 {
            if let Some(st) = mgr.status(repo_id).await {
                if st.phase.is_terminal() {
                    return st;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("pipeline did not reach terminal state in 60s");
    }

    #[tokio::test]
    async fn test_open_main_ready_and_worktree_content() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "main").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert!(st.stale_warning.is_none());
        assert!(st.resolved_commit.as_deref().unwrap().len() >= 7);
        let wt = mgr.worktree(&rid).await.unwrap();
        assert!(wt.join("src").join("Bar.java").exists());
        assert!(wt.join("src").join("Baz.java").exists(), "main HEAD has Baz");
    }

    #[tokio::test]
    async fn test_open_tag_and_branch_isolated_worktrees() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid_main = mgr.open(&url, "main").await;
        let rid_tag = mgr.open(&url, "v1.0").await;
        let rid_feat = mgr.open(&url, "feature-x").await;
        assert_ne!(rid_main, rid_tag);
        assert_ne!(rid_main, rid_feat);

        let st = wait_terminal(&mgr, &rid_tag).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        let wt_tag = mgr.worktree(&rid_tag).await.unwrap();
        assert!(!wt_tag.join("src").join("Baz.java").exists(), "v1.0 predates Baz");

        let st = wait_terminal(&mgr, &rid_feat).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        let wt_feat = mgr.worktree(&rid_feat).await.unwrap();
        assert!(wt_feat.join("src").join("Feature.java").exists());

        wait_terminal(&mgr, &rid_main).await;
    }

    #[test]
    fn test_should_fetch_decision() {
        // 刚完成完整 clone：缓存必新，任何 ref 都跳过
        assert!(!should_fetch("main", true));
        assert!(!should_fetch("0123456789abcdef0123456789abcdef01234567", true));
        // full 40-hex commitid：对象不可变，本地 resolve 即答案
        assert!(!should_fetch("0123456789abcdef0123456789abcdef01234567", false));
        // 40 位但含非 hex 字符：不是 commitid，按命名 ref 走 fetch
        assert!(should_fetch("zzzz456789abcdef0123456789abcdef01234567", false));
        // 命名 ref（分支/tag）+ clone 已存在：fetch 刷新（分支经常变）
        assert!(should_fetch("main", false));
        assert!(should_fetch("v1.0", false));
        assert!(should_fetch("feature-x", false));
    }

    #[tokio::test]
    async fn test_open_commitid() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let sha = crate::code::testutil::head_sha(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, &sha).await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert_eq!(st.resolved_commit.as_deref(), Some(sha.as_str()));
        assert!(st.stale_warning.is_none(), "commitid open must not stamp stale warning");
    }

    #[tokio::test]
    async fn test_invalid_ref_fails() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "no-such-branch").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Failed);
        assert_eq!(st.error_code.as_deref(), Some("invalid_ref"));
        assert!(mgr.worktree(&rid).await.is_err());
    }

    #[tokio::test]
    async fn test_fetch_failure_degrades_to_stale_ready() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "main").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert!(st.stale_warning.is_none());

        // 源仓消失 → 重开（Ready 触发 fetch 刷新）→ fetch 失败但本地有 ref
        std::fs::remove_dir_all(src.path()).unwrap();
        let rid2 = mgr.open(&url, "main").await;
        assert_eq!(rid, rid2, "same (url, ref) is idempotent");
        let st = wait_terminal(&mgr, &rid2).await;
        assert_eq!(st.phase, RepoPhase::Ready);
        assert!(st.stale_warning.is_some(), "degraded to local cache with warning");
    }

    #[tokio::test]
    async fn test_list_cache_and_delete() {
        let src = tempfile::tempdir().unwrap();
        let url = crate::code::testutil::make_source_repo(src.path());
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());

        let rid = mgr.open(&url, "main").await;
        wait_terminal(&mgr, &rid).await;

        let entries = mgr.list_cache().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].repo_url, url);
        assert!(entries[0].disk_bytes > 0);
        assert!(entries[0].worktrees >= 1);

        mgr.delete_cache(&entries[0].url_hash).await.unwrap();
        assert!(mgr.list_cache().await.is_empty());
        // 重开后可再次 clone
        let rid = mgr.open(&url, "main").await;
        let st = wait_terminal(&mgr, &rid).await;
        assert_eq!(st.phase, RepoPhase::Ready);
    }

    #[tokio::test]
    async fn test_delete_cache_rejects_inflight() {
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());
        // 直接注入一个进行中状态（不真跑 clone）；url_hash 用合法 16 位 hex，
        // 确保命中的是 busy 拒绝而非格式校验
        mgr.inner.states.lock().await.insert(
            "fake-rid".to_string(),
            RepoState {
                repo_id: "fake-rid".to_string(),
                url_hash: "cafebabe00000000".to_string(),
                repo_url: "https://example.com/x.git".to_string(),
                git_ref: "main".to_string(),
                phase: RepoPhase::Cloning,
                progress: None,
                resolved_commit: None,
                stale_warning: None,
                error_code: None,
                error: None,
                worktree_path: None,
                created_at: chrono::Utc::now(),
            },
        );
        let err = mgr.delete_cache("cafebabe00000000").await.unwrap_err();
        assert!(err.contains("busy"));
    }

    /// 审查修复 #2：delete_cache 的 url_hash 格式校验。`..` 会拼成
    /// repos_dir/..（父目录！）——修复前直接跑 RED 会真的删系统临时目录，
    /// 故只对修复后的拒绝行为做 GREEN 断言（非法输入一律 Err）。
    #[tokio::test]
    async fn test_delete_cache_rejects_invalid_url_hash() {
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());
        // ".." = 路径穿越（删除目标会逃出仓缓存目录）；"abc" = 长度不足；
        // "zzzzzzzzzzzzzzzz" = 16 位但非 hex；"a/b" = 分隔符；"" = 空
        for bad in ["..", "abc", "zzzzzzzzzzzzzzzz", "a/b", ""] {
            let err = mgr.delete_cache(bad).await.unwrap_err();
            assert!(err.contains("invalid url_hash"), "expected invalid url_hash for {bad:?}, got: {err}");
        }
        // 安全性复核：`..` 尝试后仓缓存目录（及其父）必须安然无恙
        assert!(repos.path().exists(), "repos dir must survive a '..' attempt");
        // 合法格式但不存在的 hash：无目录可删 → Ok（既有行为不变）
        mgr.delete_cache("0123456789abcdef").await.unwrap();
    }

    /// 审查修复 #7：并发同 (url,ref) open 去重——注入 Cloning 态后再次
    /// open，必须原位早退（同 repo_id 且 created_at 不变），不得重插状态。
    #[tokio::test]
    async fn test_open_inflight_dedup_no_reinsert() {
        let repos = tempfile::tempdir().unwrap();
        let mgr = CodeRepoManager::new(repos.path().to_path_buf());
        let url = "file:///C:/friday-no-such-repo";
        let git_ref = "main";
        let rid = git::repo_id(url, git_ref);
        let uh = git::url_hash(url);
        // 注入 1 小时前创建的 Cloning 态（同 test_delete_cache_rejects_inflight 模式，
        // 不真跑 clone）：若早退失效被重插，created_at 会变成 now，断言必挂
        let created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        mgr.inner.states.lock().await.insert(
            rid.clone(),
            RepoState {
                repo_id: rid.clone(),
                url_hash: uh,
                repo_url: url.to_string(),
                git_ref: git_ref.to_string(),
                phase: RepoPhase::Cloning,
                progress: None,
                resolved_commit: None,
                stale_warning: None,
                error_code: None,
                error: None,
                worktree_path: None,
                created_at,
            },
        );
        let rid2 = mgr.open(url, git_ref).await;
        assert_eq!(rid, rid2, "in-flight dedup must return the same repo_id");
        let st = mgr.status(&rid).await.unwrap();
        assert_eq!(st.created_at, created_at, "in-flight early-return must not re-insert the state");
    }
}
