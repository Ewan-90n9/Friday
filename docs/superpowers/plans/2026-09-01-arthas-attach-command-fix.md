# arthas attach 命令修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 attach 命令的三个问题（pid 位置参数 / telnet 预检端口 / stop 认证），arthas attach 全链路可用（issue #7 第二轮）。

**Architecture:** 全部改动集中在 `src-tauri/src/arthas/attach.rs`：`attach_command`/`arthas_properties_content`/`find_free_port_command`/`parse_free_port` 四个纯函数改造 + 编排层 `attach_arthas` 两端口传递。TDD：先改测试断言新形态，RED 后改实现。

**Tech Stack:** Rust（纯函数 + 编排，无资源/前端改动）。

**约定：**
- 测试：`cargo test --manifest-path src-tauri/Cargo.toml arthas::attach`；全量 `cargo test --manifest-path src-tauri/Cargo.toml`
- spec：docs/superpowers/specs/2026-09-01-arthas-attach-command-fix-design.md（含根因证据）
- main 分支直接干
- 本地已用 math-game JVM + arthas 4.3.5 验证过修复方案（attach 成功 / MCP 握手 / stop 关闭），实现只需对齐该形态

---

### Task 1: 四个纯函数 + 编排改造（TDD，单任务完成）

**Files:**
- Modify: `src-tauri/src/arthas/attach.rs`

- [ ] **Step 1.1: 改测试到新形态（RED）**

`mod tests` 中修改/新增：

```rust
    #[test]
    fn test_arthas_properties_content() {
        let content = arthas_properties_content(18563, "abc123");
        assert!(content.contains("arthas.config.overrideAll=true\n"));
        assert!(content.contains("arthas.mcpEndpoint=/mcp\n"));
        assert!(content.contains("arthas.telnetPort=-1\n"));
        assert!(content.contains("arthas.httpPort=18563\n"));
        assert!(content.contains("arthas.password=abc123\n"));
        assert!(content.contains("arthas.localConnectionNonAuth=true\n"));
        assert!(content.contains("arthas.ip=127.0.0.1\n"));
        // 无单引号/美元符（安全嵌入 shell 单引号）
        assert!(!content.contains('\''));
        assert!(!content.contains('$'));
    }

    #[test]
    fn test_attach_command() {
        let cmd = attach_command(
            "/tmp/friday-tools/jdk-21/bin/java",
            "/tmp/friday-tools/arthas-4.3.5",
            18563,      // http_port
            19563,      // telnet_det_port
            123,        // pid
        );
        assert!(cmd.contains("cd /tmp/friday-tools/arthas-4.3.5"));
        assert!(cmd.contains("--attach-only"));
        assert!(cmd.contains("--http-port 18563"));
        assert!(cmd.contains("--telnet-port 19563"));
        // pid 是位置参数（arthas-boot 无 --pid 选项），放在最后
        assert!(cmd.contains("arthas-boot.jar --attach-only --http-port 18563 --telnet-port 19563 123"));
        assert!(!cmd.contains("--pid"));
        assert!(cmd.contains("< /dev/null"));
        assert!(cmd.contains("&"));
        assert!(cmd.contains(">> /tmp/arthas-friday-123.log 2>&1"));
    }

    #[test]
    fn test_find_free_port_command_and_parse() {
        let cmd = find_free_port_command(18563, 10);
        assert!(cmd.contains("seq 18563 18572"));
        // 返回两个空闲端口：第一行 http，第二行 telnet 预检
        assert_eq!(parse_free_port("18563\n18564\n").unwrap(), (18563, 18564));
        assert!(parse_free_port("none\n").is_err());
        // 只有一个空闲端口：不够用，报错
        assert!(parse_free_port("18563\nnone\n").is_err());
    }
```

- [ ] **Step 1.2: 跑测试确认 RED**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas::attach`
Expected: 3 个改过的测试 FAIL（编译错误也算：签名不匹配），其余 PASS。观察记录。

- [ ] **Step 1.3: 实现改造**

```rust
/// arthas.properties 内容。overrideAll=true 使 properties 覆盖 CLI（agent 侧 telnetPort=-1
/// 真正禁用 telnet，CLI 传的 telnet 端口仅用于骗过 boot 的预检）；localConnectionNonAuth +
/// ip=127.0.0.1 为官方包默认（stop 走 /api 本地免密、只绑回环），此前覆盖时丢失。
/// 内容不含单引号/美元符，可安全嵌入 shell 单引号（见测试）。
pub fn arthas_properties_content(http_port: u16, token: &str) -> String {
    format!(
        "arthas.config.overrideAll=true\narthas.mcpEndpoint=/mcp\narthas.telnetPort=-1\narthas.httpPort={http_port}\narthas.password={token}\narthas.localConnectionNonAuth=true\narthas.ip=127.0.0.1\n"
    )
}

/// 从 start 起找两个空闲端口（http + telnet 预检用）：探测候选 count 个，输出两行
pub fn find_free_port_command(start: u16, count: u16) -> String {
    let end = start + count - 1;
    format!(
        "for p in $(seq {start} {end}); do \
         if (exec 3<>/dev/tcp/127.0.0.1/$p) 2>/dev/null; then exec 3>&- 3<&-; else echo $p; fi; \
         done | head -2; true"
    )
}
```

注意：原命令找到 1 个即 `exit 0`，新命令遍历全部候选输出空闲端口再 `head -2`（`| head -2` 管道下循环 SIGPIPE 提前退出，行为正确）；末尾 `; true` 保证管道整体 exit 0（探活命令的退出码不被 head 影响）。

```rust
/// 解析 find_free_port_command 输出 → (http_port, telnet_det_port)
pub fn parse_free_port(stdout: &str) -> Result<(u16, u16), String> {
    let ports: Vec<u16> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.parse::<u16>().ok())
        .collect();
    if ports.len() >= 2 {
        return Ok((ports[0], ports[1]));
    }
    if ports.len() == 1 {
        return Err(format!(
            "端口 {ARTHAS_PORT_START}~{} 中只有 1 个空闲（需要 2 个：HTTP + telnet 预检），请稍后重试",
            ARTHAS_PORT_START + ARTHAS_PORT_CANDIDATES - 1
        ));
    }
    Err(format!(
        "端口 {ARTHAS_PORT_START}~{} 均被占用，请减少同机并发 attach 的 JVM 数或稍后重试",
        ARTHAS_PORT_START + ARTHAS_PORT_CANDIDATES - 1
    ))
}
```

（实现时可斟酌单端口场景的错误信息是否合理——10 个候选只空 1 个的概率极低，明确报错即可。）

```rust
/// attach 命令：pid 是位置参数（arthas-boot 无 --pid 选项）；--attach-only 使 boot 进程
/// attach 后即退出（telnet 已禁用，不启动交互 client）；--telnet-port 传有效空闲端口
/// 骗过 boot 的预检（对 -1 会抛 port out of range 退出），agent 侧实际不绑（overrideAll）。
pub fn attach_command(java: &str, home: &str, http_port: u16, telnet_det_port: u16, pid: i64) -> String {
    format!(
        "cd {home} && nohup {java} -jar arthas-boot.jar --attach-only --http-port {http_port} --telnet-port {telnet_det_port} {pid} < /dev/null >> /tmp/arthas-friday-{pid}.log 2>&1 & echo attach-started"
    )
}
```

- [ ] **Step 1.4: 编排层适配（编译驱动）**

`attach_arthas` 中：

```rust
    // 4. 分配远端端口（http + telnet 预检各一）+ 写 arthas.properties
    progress("allocate_port", "分配 arthas 端口".to_string());
    let (port, telnet_det_port) = find_free_remote_port(channel.as_ref()).await?;
```

`find_free_remote_port` 返回类型改 `Result<(u16, u16), ManagerError>`；`run_attach_command` 增加 `http_port: u16, telnet_det_port: u16` 参数透传给 `attach_command`；调用点：

```rust
    progress("attach", format!("attach arthas 到 PID {}（java={java}）", req.pid));
    // ... AttachExecKind 分支内：
    run_attach_command(shared.as_ref(), &java, &arthas_home, port, telnet_det_port, req.pid).await?;
```

`write_properties(channel.as_ref(), &arthas_home, &arthas_properties_content(port, &token))` 不变（port 即 http_port）。

grep 确认 `attach_command`/`parse_free_port` 无其他调用方（纯函数仅本文件使用）。

- [ ] **Step 1.5: GREEN + 全量**

Run: `cargo test --manifest-path src-tauri/Cargo.toml arthas::attach` → 全 PASS
Run: `cargo test --manifest-path src-tauri/Cargo.toml` → 全绿（510 基线，无新增用例数变化时保持 510±1，以实际为准）
Run: `cargo check --manifest-path src-tauri/Cargo.toml` → 无新警告

- [ ] **Step 1.6: 提交**

```bash
git add src-tauri/src/arthas/attach.rs
git commit -m "fix: arthas attach uses positional pid, valid telnet probe port and local-auth properties"
```

---

### Task 2: 回归 + 发布准备

- [ ] **Step 2.1: 全量验证**

`cargo check` / `cargo test`（全绿）/ `pnpm typecheck`（前端未动，防意外）。

- [ ] **Step 2.2: AGENTS.md 不需要更新**（attach 编排细节不在此文档粒度；确认 Arthas 段落无与新命令形态冲突的表述——若有 "attach 命令" 字样则保持模糊即可，grep 确认）。

- [ ] **Step 2.3: 提交（如有文档改动）**

## Self-Review 记录

- Spec 覆盖：#1 pid 位置参数（Task 1 attach_command）/ #2 telnet 预检端口（find_free_port 两端口 + properties overrideAll）/ #3 stop 认证（localConnectionNonAuth）✓
- 类型一致性：`(u16, u16)` 元组贯穿 find_free_remote_port → attach_arthas → run_attach_command → attach_command ✓
- 占位符：无 ✓
