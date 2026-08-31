use crate::app::events::{AppEvent, EventBus};
use crate::exec::channel::ExecChannel;
use crate::exec::pool::ExecChannelPool;
use crate::exec::ssh::shell_quote_single;
use crate::provision::package::ToolPackage;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::manager::{ArthasClient, ArthasStopHandle, AttachFactory, AttachRequest, AttachedSession, ManagerError};

/// 远端 arthas HTTP 端口分配起点（顺序向上探测）
pub const ARTHAS_PORT_START: u16 = 18563;
pub const ARTHAS_PORT_CANDIDATES: u16 = 10;

/// arthas.properties 内容（MCP endpoint 开启 / telnet 关闭 / Friday 分配端口 / 随机 Bearer）。
/// 内容不含单引号/美元符，可安全嵌入 shell 单引号（见测试）。
pub fn arthas_properties_content(http_port: u16, token: &str) -> String {
    format!(
        "arthas.mcpEndpoint=/mcp\narthas.telnetPort=-1\narthas.httpPort={http_port}\narthas.password={token}\n"
    )
}

/// 用户对齐 pre-flight：目标进程属主 + 当前 SSH 用户
pub fn check_user_command(pid: i64) -> String {
    format!("ps -o user= -p {pid} 2>/dev/null; echo '---'; id -un")
}

/// 解析 check_user_command 输出 → (jvm_user, ssh_user)。
/// jvm_user 为空 = 进程不存在（或已被回收）。
pub fn parse_user_check(stdout: &str) -> Result<(String, String), String> {
    let mut parts = stdout.splitn(2, "---");
    let jvm_raw = parts.next().unwrap_or_default();
    let ssh_raw = parts.next().unwrap_or_default();
    let jvm_user = jvm_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default();
    let ssh_user = ssh_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or_default();
    if jvm_user.is_empty() {
        return Err("目标进程不存在或已退出（ps 无属主输出）".to_string());
    }
    if ssh_user.is_empty() {
        return Err("无法确定当前 SSH 用户（id -un 无输出）".to_string());
    }
    Ok((jvm_user.to_string(), ssh_user.to_string()))
}

/// 目标机端口占用探测（bash /dev/tcp）：busy = 可连（占用）；free = 连不上
pub fn port_probe_command(port: u16) -> String {
    format!(
        "if (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then exec 3>&- 3<&-; echo busy; else echo free; fi"
    )
}

/// 从 start 起找第一个空闲端口（探测命令，候选 count 个）
pub fn find_free_port_command(start: u16, count: u16) -> String {
    let end = start + count - 1;
    format!(
        "for p in $(seq {start} {end}); do \
         if (exec 3<>/dev/tcp/127.0.0.1/$p) 2>/dev/null; then exec 3>&- 3<&-; else echo $p; exit 0; fi; \
         done; echo none"
    )
}

/// 解析 find_free_port_command 输出
pub fn parse_free_port(stdout: &str) -> Result<u16, String> {
    let first = stdout.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first == "none" || first.is_empty() {
        return Err(format!(
            "端口 {ARTHAS_PORT_START}~{} 均被占用，请减少同机并发 attach 的 JVM 数或稍后重试",
            ARTHAS_PORT_START + ARTHAS_PORT_CANDIDATES - 1
        ));
    }
    first
        .parse::<u16>()
        .map_err(|_| format!("端口探测输出无法解析: {stdout:?}"))
}

/// 写 arthas.properties（内容经单引号转义；chmod 644 保证 jvm_user 可读）
pub fn write_properties_command(home: &str, content: &str) -> String {
    format!(
        "printf '%s' {} > {home}/arthas.properties && chmod 644 {home}/arthas.properties",
        shell_quote_single(content)
    )
}

/// attach 命令：cd 到 arthas home（arthas-boot 从当前目录读 arthas.properties），
/// nohup 后台驻留，stdin 接 /dev/null 防交互等待。java 为可执行文件完整路径（已做字符集校验）。
/// 日志重定向到 /tmp（跨用户 attach 时 home 目录对 jvm_user 不可写，/tmp 才能保证可写）。
pub fn attach_command(java: &str, home: &str, pid: i64) -> String {
    format!(
        "cd {home} && nohup {java} -jar arthas-boot.jar --pid {pid} < /dev/null >> /tmp/arthas-friday-{pid}.log 2>&1 & echo attach-started"
    )
}

/// HTTP stop（best-effort）：arthas HTTP API 执行 stop 命令，卸载 agent。
/// curl 缺失时 wget 兜底，再失败吞掉（stop 尽力而为）。
pub fn stop_command(port: u16, token: &str) -> String {
    format!(
        "curl -s -m 10 -X POST -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' \
         -d '{{\"action\":\"exec\",\"command\":\"stop\"}}' http://127.0.0.1:{port}/api \
         || wget -q -O /dev/null --header='Authorization: Bearer {token}' \
         --post-data='{{\"action\":\"exec\",\"command\":\"stop\"}}' http://127.0.0.1:{port}/api \
         || true"
    )
}

/// 生成 Bearer token（32 位十六进制，无 shell 特殊字符）
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

// ─────────────────────────── 生产编排 ───────────────────────────

/// 生产 attach 依赖集
#[derive(Clone)]
pub struct AttachDeps {
    pub db: sqlx::SqlitePool,
    pub exec_pool: Arc<Mutex<ExecChannelPool>>,
    pub tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    pub jdk_cache: Arc<crate::tools::builtin::jvm::jdk_cache::JdkCache>,
    pub cache_dir: PathBuf,
    pub bus: EventBus,
}

pub fn production_attach_factory(deps: AttachDeps) -> AttachFactory {
    Arc::new(move |req| {
        let deps = deps.clone();
        Box::pin(attach_arthas(deps, req))
    })
}

/// attach 命令执行通道。关键正确性约束：Shared 是连接池里的连接，用后**绝不能**
/// disconnect（会拆掉池连接）；Temp 是 jvm_user 临时连接，attach 完即断开。
enum AttachExecKind {
    Shared(Arc<dyn ExecChannel>),
    Temp(TempAttachTransport),
}

async fn attach_arthas(deps: AttachDeps, req: AttachRequest) -> Result<AttachedSession, ManagerError> {
    let progress = |stage: &str, detail: String| {
        tracing::info!(session_id = %req.session_id, env_id = %req.env_id, stage, detail = %detail, "arthas attach progress");
        deps.bus.emit(
            &req.session_id,
            AppEvent::ProvisionProgress {
                session_id: req.session_id.clone(),
                tool: "arthas_open".to_string(),
                stage: stage.to_string(),
                detail,
            },
        );
    };

    // 0. 默认连接（连接池）
    let channel = get_default_channel(&deps, &req.env_id).await?;

    // 1. 确保 arthas 工具包（幂等，cached 快路径）
    progress("ensure_package", "确保 arthas 工具包".to_string());
    let pctx = provision_context(&deps, &req, channel.clone()).await?;
    let arthas_pkg = crate::provision::arthas::ArthasPackage;
    let arthas_result = arthas_pkg
        .ensure(&pctx, "java")
        .await
        .map_err(|e| ManagerError::Attach(format!("arthas 工具包下发失败: {}", e.message)))?;
    let arthas_home = arthas_result.tool_home;

    // 2. 解析 attach 用 java（JdkCache → PATH java → ensure JDK），返回可执行文件完整路径
    let java = resolve_attach_java(&deps, &req, &pctx).await?;

    // 3. 用户对齐 pre-flight
    progress("check_user", "检查目标 JVM 运行用户".to_string());
    let (jvm_user, ssh_user) = check_users(channel.as_ref(), req.pid).await?;
    let attach_exec_kind = if jvm_user == ssh_user || ssh_user == "root" {
        AttachExecKind::Shared(channel.clone())
    } else {
        progress(
            "check_user",
            format!("SSH 用户 {ssh_user} ≠ JVM 用户 {jvm_user}，使用 {jvm_user} 凭证临时连接"),
        );
        match crate::app::env_credentials::find_credential_by_username(&deps.db, &req.env_id, &jvm_user).await {
            Ok(Some(cred)) => AttachExecKind::Temp(build_temp_transport(&deps, &req.env_id, &cred).await?),
            _ => {
                return Err(ManagerError::Attach(format!(
                    "目标 JVM 运行用户为 {jvm_user}，当前 SSH 用户为 {ssh_user} 且未录入 {jvm_user} 的凭证。\
                     请让用户在环境管理中为该环境添加用户 {jvm_user} 的凭证后重试"
                )))
            }
        }
    };

    // 4. 分配远端 HTTP 端口 + 写 arthas.properties（配置只走 properties，不传 CLI 端口参数）
    progress("allocate_port", "分配 arthas HTTP 端口".to_string());
    let port = find_free_remote_port(channel.as_ref()).await?;
    let token = generate_token();
    progress("write_config", format!("写入 arthas.properties（httpPort={port}）"));
    write_properties(channel.as_ref(), &arthas_home, &arthas_properties_content(port, &token)).await?;

    // 5. attach（nohup 后台驻留；临时连接场景执行完即断开）
    progress("attach", format!("attach arthas 到 PID {}（java={java}）", req.pid));
    let temp_disconnect: Option<TempAttachTransport> = match attach_exec_kind {
        AttachExecKind::Shared(shared) => {
            run_attach_command(shared.as_ref(), &java, &arthas_home, req.pid).await?;
            None
        }
        AttachExecKind::Temp(t) => {
            run_attach_command(&t, &java, &arthas_home, req.pid).await?;
            Some(t)
        }
    };
    if let Some(t) = temp_disconnect {
        tokio::spawn(async move { t.disconnect().await; });
    }

    // 6. 探活（端口可连 = arthas HTTP server 就绪）
    progress("probe", "等待 arthas HTTP 服务就绪".to_string());
    wait_http_ready(channel.as_ref(), port, std::time::Duration::from_secs(60)).await?;

    // 7. 隧道 + MCP 握手（失败要拆隧道）
    progress("tunnel", "建立 SSH 隧道".to_string());
    let lease = deps
        .tunnels
        .open(&req.env_id, "127.0.0.1", port)
        .await
        .map_err(|e| ManagerError::Attach(format!("SSH 隧道建立失败: {e}")))?;
    let url = format!("http://127.0.0.1:{}/mcp", lease.local_port);
    progress("handshake", format!("MCP 握手（{url}）"));
    let client: Arc<dyn ArthasClient> = match crate::arthas::client::connect_arthas_client(&url, &token).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            deps.tunnels.close(&req.env_id, "127.0.0.1", port).await;
            return Err(ManagerError::Attach(format!("arthas MCP 握手失败: {e}")));
        }
    };

    progress("ready", format!("arthas 就绪（远端端口 {port}，本地隧道端口 {}）", lease.local_port));
    let stop_handle: Arc<dyn ArthasStopHandle> = Arc::new(ProductionStopHandle {
        db: deps.db.clone(),
        exec_pool: deps.exec_pool.clone(),
        tunnels: deps.tunnels.clone(),
        env_id: req.env_id.clone(),
        remote_port: port,
        token,
        client: client.clone(),
    });
    Ok(AttachedSession { client, stop_handle })
}

/// best-effort stop：HTTP stop arthas（卸载 agent）+ 拆隧道 + 关 MCP client
struct ProductionStopHandle {
    db: sqlx::SqlitePool,
    exec_pool: Arc<Mutex<ExecChannelPool>>,
    tunnels: Arc<crate::exec::tunnel::TunnelManager>,
    env_id: String,
    remote_port: u16,
    token: String,
    client: Arc<dyn ArthasClient>,
}

#[async_trait::async_trait]
impl ArthasStopHandle for ProductionStopHandle {
    async fn stop(&self) {
        // HTTP stop（尽力而为，失败仅告警——残留 agent 由用户 arthas_close 重试或目标机重启解决）
        match get_default_channel_raw(&self.db, &self.exec_pool, &self.env_id).await {
            Ok(channel) => match run_with_timeout(channel.as_ref(), &stop_command(self.remote_port, &self.token), 15).await {
                Ok(_) => tracing::info!(env_id = %self.env_id, port = self.remote_port, "arthas stopped via http api"),
                Err(e) => tracing::warn!(env_id = %self.env_id, port = self.remote_port, error = %e, "arthas http stop failed (best-effort)"),
            },
            Err(e) => tracing::warn!(env_id = %self.env_id, port = self.remote_port, error = %e, "failed to get exec channel for arthas http stop (best-effort skip)"),
        }
        self.tunnels.close(&self.env_id, "127.0.0.1", self.remote_port).await;
        self.client.shutdown().await;
    }
}

// ── 编排子步骤 ──

async fn get_default_channel(deps: &AttachDeps, env_id: &str) -> Result<Arc<dyn ExecChannel>, ManagerError> {
    get_default_channel_raw(&deps.db, &deps.exec_pool, env_id).await
}

async fn get_default_channel_raw(
    db: &sqlx::SqlitePool,
    exec_pool: &Arc<Mutex<ExecChannelPool>>,
    env_id: &str,
) -> Result<Arc<dyn ExecChannel>, ManagerError> {
    let mut pool = exec_pool.lock().await;
    pool.get_or_create(env_id, db)
        .await
        .map_err(|e| ManagerError::Attach(format!("SSH 连接失败: {e}")))
}

async fn provision_context(
    deps: &AttachDeps,
    req: &AttachRequest,
    channel: Arc<dyn ExecChannel>,
) -> Result<crate::provision::package::ProvisionContext, ManagerError> {
    let base = crate::app::settings::artifactory_base_url(&deps.db)
        .await
        .map_err(|e| ManagerError::Attach(format!("读取 Artifactory 设置失败: {e}")))?;
    if base.trim().is_empty() {
        return Err(ManagerError::Attach(
            "Artifactory 地址未配置，请在设置中配置后重试".to_string(),
        ));
    }
    Ok(crate::provision::package::ProvisionContext {
        session_id: req.session_id.clone(),
        env_id: req.env_id.clone(),
        channel,
        cache_dir: deps.cache_dir.clone(),
        artifactory_base_url: base,
        timeouts: crate::provision::package::StageTimeouts::default(),
        bus: deps.bus.clone(),
    })
}

/// attach 用 java 可执行文件解析：JdkCache → PATH java → ensure JDK（结果回写 JdkCache）。
/// 返回可执行文件完整路径（已做字符集校验，可安全嵌入 shell 命令）。
async fn resolve_attach_java(
    deps: &AttachDeps,
    req: &AttachRequest,
    pctx: &crate::provision::package::ProvisionContext,
) -> Result<String, ManagerError> {
    if let Some(layout) = deps.jdk_cache.get(&req.env_id).await {
        return Ok(format!("{}/bin/java", layout.tool_home));
    }
    // PATH 上有 java：直接用（JRE 也够跑 arthas-boot）
    if let Ok(out) = run_with_timeout(pctx.channel.as_ref(), "command -v java", 15).await {
        let java = out.stdout.trim().to_string();
        if out.exit_code == 0 && !java.is_empty() {
            // 字符集校验（防 shell 注入，与 ensure_tool 的 java_bin 同款规则）
            if crate::provision::jdk::validate_java_bin(&java).is_ok() {
                return Ok(java);
            }
            tracing::warn!(java = %java, "PATH java path failed charset validation, ignoring");
        }
    }
    // 兜底：ensure JDK（依赖 java_bin 参数指向可用 java；目标机无 java 时给 agent 可行动的错误）
    let jdk = crate::provision::jdk::JdkPackage;
    match jdk.ensure(pctx, &req.java_bin).await {
        Ok(result) => {
            deps.jdk_cache
                .set(
                    &req.env_id,
                    crate::tools::builtin::jvm::jdk_cache::JdkLayout {
                        tool_home: result.tool_home.clone(),
                        bins: result.bins.clone(),
                    },
                )
                .await;
            Ok(format!("{}/bin/java", result.tool_home))
        }
        Err(e) => Err(ManagerError::Attach(format!(
            "目标机找不到可用的 java（{}）。可用 run_command 确认目标服务的 java 路径后，\
             用 java_bin 参数重试 arthas_open",
            e.message
        ))),
    }
}

/// 用户对齐检查（jvm_user, ssh_user）
async fn check_users(
    channel: &dyn ExecChannel,
    pid: i64,
) -> Result<(String, String), ManagerError> {
    let out = run_with_timeout(channel, &check_user_command(pid), 15).await?;
    parse_user_check(&out.stdout).map_err(|e| ManagerError::Attach(format!("用户对齐检查失败: {e}; stderr: {}", out.stderr)))
}

/// 分配远端空闲端口（18563 起顺序探测）
async fn find_free_remote_port(channel: &dyn ExecChannel) -> Result<u16, ManagerError> {
    let cmd = find_free_port_command(ARTHAS_PORT_START, ARTHAS_PORT_CANDIDATES);
    let out = run_with_timeout(channel, &cmd, 20).await?;
    parse_free_port(&out.stdout).map_err(|e| ManagerError::Attach(e))
}

/// 写 arthas.properties（经默认连接执行；chmod 644 保证 jvm_user 可读）
async fn write_properties(
    channel: &dyn ExecChannel,
    home: &str,
    content: &str,
) -> Result<(), ManagerError> {
    let out = run_with_timeout(channel, &write_properties_command(home, content), 15).await?;
    if out.exit_code != 0 {
        return Err(ManagerError::Attach(format!(
            "写入 arthas.properties 失败（exit {}）: {}",
            out.exit_code, out.stderr
        )));
    }
    Ok(())
}

/// 执行 attach 命令（临时连接场景用后即断）
async fn run_attach_command(
    exec: &dyn ExecChannel,
    java: &str,
    arthas_home: &str,
    pid: i64,
) -> Result<(), ManagerError> {
    let out = run_with_timeout(exec, &attach_command(java, arthas_home, pid), 30).await?;
    if out.exit_code != 0 {
        return Err(ManagerError::Attach(format!(
            "arthas attach 命令失败（exit {}）: {}",
            out.exit_code, out.stderr
        )));
    }
    Ok(())
}

/// 探活循环：端口可连即认为 arthas HTTP server 就绪（bash /dev/tcp，无 curl 依赖）
async fn wait_http_ready(
    channel: &dyn ExecChannel,
    port: u16,
    budget: std::time::Duration,
) -> Result<(), ManagerError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let out = run_with_timeout(channel, &port_probe_command(port), 15)
            .await
            .map_err(|e| ManagerError::Attach(format!("arthas 探活失败: {e}")))?;
        if out.stdout.trim() == "busy" {
            tracing::info!(port, attempt, "arthas http server ready");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ManagerError::Attach(format!(
                "arthas HTTP 服务在 {}s 内未就绪（端口 {port}）。\
                 可能原因：attach 失败（用户权限/attach 机制被禁用）、目标 JVM 拒绝 attach。\
                 可用 run_command 查看 {ARTHAS_LOG_HINT} 日志",
                budget.as_secs()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// attach 日志位置提示（错误消息用）
const ARTHAS_LOG_HINT: &str = "/tmp/arthas-friday-<pid>.log";

/// 临时 attach 连接：jvm_user 凭证 → 独立 SshTransport（用后由调用方 disconnect）
async fn build_temp_transport(
    deps: &AttachDeps,
    env_id: &str,
    cred: &crate::app::env_credentials::EnvCredentialRow,
) -> Result<TempAttachTransport, ManagerError> {
    let env = crate::exec::pool::fetch_environment(&deps.db, env_id)
        .await
        .map_err(|e| ManagerError::Attach(format!("环境查询失败: {e}")))?;
    let auth = crate::exec::ssh::SshAuth::from_row(&cred.auth_type, cred.private_key_path.as_deref())
        .ok_or_else(|| ManagerError::Attach(format!("用户 {} 的认证配置无效", cred.username)))?;
    let secret = crate::app::credentials::load_cred_secret(env_id, &cred.id)
        .await
        .map_err(|e| ManagerError::Attach(format!("读取用户 {} 密钥失败: {e}", cred.username)))?;
    let secret = match (&auth, secret) {
        // 密码认证但未存储密码（None/空串）→ 明确报错（不能回落到默认用户的旧密钥）；
        // 私钥认证的 None 合法（无口令私钥），原样透传
        (crate::exec::ssh::SshAuth::Password, s) if s.as_deref().map_or(true, |v| v.trim().is_empty()) => {
            return Err(ManagerError::Attach(format!(
                "用户 {} 的凭证未存储密码，请在环境管理中补录该用户的密码后重试",
                cred.username
            )));
        }
        (_, s) => s,
    };
    let transport = crate::exec::ssh::SshTransport::with_secret(
        env_id,
        env.host.as_deref().unwrap_or_default(),
        env.port.unwrap_or(22),
        &cred.username,
        auth,
        secret,
    );
    transport
        .connect()
        .await
        .map_err(|e| ManagerError::Attach(format!("以用户 {} 建立连接失败: {e}", cred.username)))?;
    Ok(TempAttachTransport { inner: transport })
}

/// 临时连接包装：断开由调用方处理（russh 无 async Drop，用后台 spawn）
struct TempAttachTransport {
    inner: crate::exec::ssh::SshTransport,
}

#[async_trait::async_trait]
impl ExecChannel for TempAttachTransport {
    async fn run(&self, cmd: &str) -> Result<crate::exec::channel::ExecOutput, Box<dyn std::error::Error + Send + Sync>> {
        self.inner.run(cmd).await
    }
    async fn connect(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.inner.connect().await
    }
    async fn disconnect(&self) {
        self.inner.disconnect().await;
    }
    async fn is_alive(&self) -> bool {
        self.inner.is_alive().await
    }
}

/// 统一的带超时远端执行（命令本身都应秒级返回）
async fn run_with_timeout(
    channel: &dyn ExecChannel,
    cmd: &str,
    secs: u64,
) -> Result<crate::exec::channel::ExecOutput, ManagerError> {
    match tokio::time::timeout(std::time::Duration::from_secs(secs), channel.run(cmd)).await {
        Err(_) => Err(ManagerError::Attach(format!("远端命令执行超时（{secs}s）: {cmd}"))),
        Ok(Err(e)) => Err(ManagerError::Attach(format!("远端命令执行失败: {e}（命令: {cmd}）"))),
        Ok(Ok(out)) => {
            if !out.stderr.trim().is_empty() {
                tracing::debug!(cmd, stderr = %out.stderr, "remote command stderr");
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arthas_properties_content() {
        let content = arthas_properties_content(18563, "abc123");
        assert!(content.contains("arthas.mcpEndpoint=/mcp\n"));
        assert!(content.contains("arthas.telnetPort=-1\n"));
        assert!(content.contains("arthas.httpPort=18563\n"));
        assert!(content.contains("arthas.password=abc123\n"));
        // 无单引号/美元符（安全嵌入 shell 单引号）
        assert!(!content.contains('\''));
        assert!(!content.contains('$'));
    }

    #[test]
    fn test_check_user_command() {
        assert_eq!(check_user_command(123), "ps -o user= -p 123 2>/dev/null; echo '---'; id -un");
    }

    #[test]
    fn test_parse_user_check() {
        let (jvm, ssh) = parse_user_check("svcapp\n---\nopc\n").unwrap();
        assert_eq!(jvm, "svcapp");
        assert_eq!(ssh, "opc");
        // ps 输出带空白
        let (jvm, ssh) = parse_user_check("  svcapp \n---\n opc \n").unwrap();
        assert_eq!(jvm, "svcapp");
        assert_eq!(ssh, "opc");
    }

    #[test]
    fn test_parse_user_check_pid_gone() {
        assert!(parse_user_check("\n---\nopc\n").is_err());
    }

    #[test]
    fn test_find_free_port_command_and_parse() {
        let cmd = find_free_port_command(18563, 3);
        assert!(cmd.contains("seq 18563 18565"));
        assert_eq!(parse_free_port("18563\n").unwrap(), 18563);
        assert!(parse_free_port("none\n").is_err());
    }

    #[test]
    fn test_port_probe_command() {
        assert!(port_probe_command(8563).contains("/dev/tcp/127.0.0.1/8563"));
    }

    #[test]
    fn test_attach_command() {
        let cmd = attach_command("/tmp/friday-tools/jdk-21/bin/java", "/tmp/friday-tools/arthas-4.3.5", 123);
        assert!(cmd.contains("cd /tmp/friday-tools/arthas-4.3.5"));
        assert!(cmd.contains("nohup /tmp/friday-tools/jdk-21/bin/java -jar arthas-boot.jar --pid 123"));
        assert!(cmd.contains("< /dev/null"));
        assert!(cmd.contains("&"));
        // 日志重定向到 /tmp（跨用户 attach 时 home 目录对 jvm_user 不可写）
        assert!(cmd.contains(">> /tmp/arthas-friday-123.log 2>&1"));
        assert!(!cmd.contains("{home}/arthas-attach-"));
    }

    #[test]
    fn test_write_properties_command_quotes_content() {
        let content = arthas_properties_content(18563, "tok123");
        let cmd = write_properties_command("/tmp/friday-tools/arthas-4.3.5", &content);
        assert!(cmd.starts_with("printf '%s' 'arthas."));
        assert!(cmd.contains("> /tmp/friday-tools/arthas-4.3.5/arthas.properties"));
        assert!(cmd.contains("chmod 644 /tmp/friday-tools/arthas-4.3.5/arthas.properties"));
    }

    #[test]
    fn test_stop_command_contains_auth_and_payload() {
        let cmd = stop_command(18563, "tok123");
        assert!(cmd.contains("Authorization: Bearer tok123"));
        assert!(cmd.contains("http://127.0.0.1:18563/api"));
        assert!(cmd.contains("\"command\":\"stop\""));
    }

    #[test]
    fn test_generate_token_charset() {
        for _ in 0..10 {
            let t = generate_token();
            assert_eq!(t.len(), 32);
            assert!(t.chars().all(|c| c.is_ascii_alphanumeric()));
        }
    }
}
