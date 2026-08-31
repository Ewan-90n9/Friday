use crate::exec::ssh::shell_quote_single;

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
pub fn attach_command(java: &str, home: &str, pid: i64) -> String {
    format!(
        "cd {home} && nohup {java} -jar arthas-boot.jar --pid {pid} < /dev/null >> {home}/arthas-attach-{pid}.log 2>&1 & echo attach-started"
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
