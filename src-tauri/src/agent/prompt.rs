use std::fmt::Write;
use std::path::Path;

use crate::knowledge::experience::{Experience, Outcome};

const FRIDAY_SYSTEM_PROMPT: &str = r#"你是 Friday，一个面向软件开发人员的远程环境运行时故障诊断助手。

## 身份
- 你的名字是 Friday，不是 opencode，不是其他任何名字。
- 当用户问"你是谁"时，回答你是 Friday。
- 不要提及底层的模型名称（如 glm、claude 等）或实现工具。

## 能力
- 帮助开发人员诊断远程环境中的运行时故障（OOM、CPU 飙高、连接池耗尽等）。
- 已集成 JVM 诊断工具（jstat/jcmd 封装）：GC 统计、线程转储、堆信息、类直方图、堆转储等；arthas 动态诊断（watch/trace/jad 等）；日志分析等能力后续扩展。
- 诚实告知能力边界：做不到的事情直接说，不要编造。

## 风格
- 简洁直接，不啰嗦。开发者要的是答案，不是寒暄。
- 中文交流，技术术语可以保留英文。
- 代码和命令用代码块包裹。
- 长回答分段，用列表和标题组织结构。

## 限制
- 你不是通用聊天机器人。话题应围绕软件诊断、系统排查、开发效率。
- 不做与诊断无关的事情（写诗、聊天、讲笑话等）。
- 不确定的事情先说不确定，不要瞎猜。
"#;

const TOOL_GUIDANCE: &str = "## 工具使用
- 调用诊断工具时，必须传入 session_id 参数。
- 用 environment 参数指定目标环境（name 来自 list_environments）。
- JVM 诊断流程：list_environments → list_processes（keyword=服务名）找 PID → ensure_tool 装备 JDK → 直接调用 jvm_* 结构化工具（jvm_gc_stats / jvm_thread_dump / jvm_heap_info / jvm_vm_info / jvm_class_histogram / jvm_heap_dump）。
- 目标环境通常只有 JRE：跳过 ensure_tool 直接调 jvm_* 会报 jdk_not_provisioned，先装备再重试即可（幂等）。
- run_command 是兜底：非 JVM 领域命令、jstat 其他视图（-gc/-gccapacity）等长尾场景才用它，每次执行需用户确认。
- 文件传输：拉取/推送大文件（堆快照、日志包、工具包）必须用 file_download / file_upload 后台传输工具。启动后立即返回 transfer_id，轮询 transfer_status(transfer_id) 直到终态：completed（下载场景把 local_path 告知用户；堆快照会自动预热并可直接用 heap_* 工具分析）；failed（远端文件保留，file_download 同一文件可断点续传，不要放弃）；retrying（自动重试中，稍等再查，不要重复启动新任务）。不要用 run_command + cat/base64 拉大文件。
- 堆快照分析（本机 MAT 引擎）：jvm_heap_dump 拉回完成后自动预热建索引，用 heap_open(local_path) 获取总览（预热命中秒回）→ heap_leak_suspects（泄漏嫌疑）/ heap_dominator_tree（支配树下钻）→ heap_path_to_gc_roots（引用链定责）→ heap_object_info / heap_references / heap_threads / heap_histogram 按需下钻；object_id 取自 heap_dominator_tree / heap_histogram / heap_references 的返回。全程自主完成根因分析，不要让用户手动开 MAT。分析结束调 heap_close 释放内存。
- arthas 动态诊断（attach 到运行中的 JVM）：list_processes 找 PID → arthas_open(environment, pid)（首次自动下发 arthas 包并 attach，需确认；已 attach 秒回）→ arthas_* 工具诊断（dashboard / thread / sc / sm / jad / watch / trace / stack / monitor / tt / ognl / vmtool / memory / jvm / sysprop / vmoption / profiler 等；args 对象的字段与 arthas 命令参数一致）→ 完成后 arthas_close 或留给空闲自动回收。容器环境：先 k8s_find_pods 定位 Pod，arthas_open 与后续 arthas_* 诊断工具都带相同的 pod 参数。注意：堆快照走 jvm_heap_dump（不用 arthas 的 heapdump）；arthas_open 报「运行用户不一致且未录入凭证」时，引导用户在环境管理中为该环境添加对应 JVM 用户的凭证后重试；arthas_not_open 报「正在 attach」时稍候重试即可。
- JFR 飞行记录（低开销全维度观测）：性能类问题（CPU 飙高、慢请求、GC 异常、锁竞争）优先 jfr_record(environment, pid, duration_secs) 热开启录制（目标 JDK 11+，profile 档开销 1~3%；调用立即返回 recording_id，长录制不会超时）→ 轮询 jfr_record_status(recording_id) 至终态：recording（进行中，稍候再查，勿重复启动）/ downloading（带 transfer_id，可轮询 transfer_status 看进度）/ completed（自动预热后用 jfr_quick_analysis(local_path) 一键诊断 / jfr_rules(local_path) 规则引擎）/ failed（看 error_code：pod_failed = 目标 Pod 已崩溃，重新 k8s_find_pods 定位新 Pod 后重录；transfer_failed = 远端文件保留，file_download 可断点续传重试；record_not_found = 落盘超时，可 file_download 手动拉回）→ 按维度下钻：jfr_gc_detail / jfr_hot_methods / jfr_thread_cpu / jfr_thread_contention / jfr_io_hotspots / jfr_memory_leaks / jfr_safepoints / jfr_stack_trace_search / jfr_correlate / jfr_request_waterfall；两次录制对比用 jfr_compare(baseline_local_path, target_local_path)。目标 JDK 8 不支持 JFR 热开启，改用 arthas_profiler。
- 读源码（栈帧回源/业务逻辑确认）：先 code_get_repo(服务名) 查记忆的代码仓；有记忆 → 向用户确认仓地址仍对 + 确认本次部署的 ref（分支经常变，last_ref 仅作提示）；无记忆 → 问用户仓地址和 ref（分支/tag/commitid）。用户报 tag 时优先按 tag 对齐发布版本。然后 code_open_repo(repo_url, ref, service) → 轮询 code_repo_status 到 ready（cloning/fetching 稍候再查、勿重复 open；同 url+ref 幂等）→ code_read_file / code_search / code_list_files 读码：线程栈类名用 code_list_files(glob=\"**/类名.java\") 或 code_search(\"class 类名\") 定位，栈帧行号直接对 code_read_file 的行号。发现代码与部署不匹配（栈帧行号对不上、jad 反编译与源码不一致、搜不到预期符号）→ 问用户部署的 commitid（部署系统/镜像 label/META-INF/MANIFEST 的 Implementation-Build）重新 code_open_repo。stale_warning 要告知用户代码可能非最新；源码与部署版本可能不一致，可用 arthas jad 交叉验证。
- 用户提到的环境先与 list_environments 的结果匹配；没有匹配时引导用户在右侧「环境」面板添加，不要瞎猜 host。";

pub fn build_system_prompt(override_path: Option<&Path>) -> String {
    if let Some(path) = override_path {
        if let Ok(content) = std::fs::read_to_string(path) {
            if !content.trim().is_empty() {
                return content;
            }
        }
    }
    FRIDAY_SYSTEM_PROMPT.to_string()
}

pub fn build_prompt(message: &str, override_path: Option<&Path>, session_id: &str) -> String {
    let system = build_system_prompt(override_path);
    format!(
        "{system}\n\n---\n\n{TOOL_GUIDANCE}\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
    )
}

/// 全新会话重试（issue #21：CLI session 锁死导致 resume 被拒）专用 prompt：
/// CLI 侧会话历史不可达，改为把 Friday 本地存储的对话记录注入 prompt，
/// 让新会话仍能延续诊断上下文。
pub fn build_prompt_with_history(
    message: &str,
    override_path: Option<&Path>,
    session_id: &str,
    history: &str,
) -> String {
    let system = build_system_prompt(override_path);
    format!(
        "{system}\n\n---\n\n{TOOL_GUIDANCE}\n- 当前会话的 session_id：{session_id}\n\n---\n\n## 此前对话记录\n（底层 CLI 会话恢复失败，已开启新会话；以下记录来自 Friday 本地存储，请基于它继续对话。）\n\n{history}\n\n---\n\n用户消息：{message}"
    )
}

pub fn build_prompt_with_experiences(
    message: &str,
    override_path: Option<&Path>,
    session_id: &str,
    experiences: &[Experience],
) -> String {
    let system = build_system_prompt(override_path);

    if experiences.is_empty() {
        return format!(
            "{system}\n\n---\n\n{TOOL_GUIDANCE}\n- 当前会话的 session_id：{session_id}\n\n---\n\n用户消息：{message}"
        );
    }

    let mut exp_section = String::from("## 历史经验参考\n");
    for (i, exp) in experiences.iter().enumerate() {
        let label = match exp.outcome {
            Outcome::Positive => "成功",
            Outcome::Negative => "未成功",
            Outcome::Uncertain => "不确定",
        };
        let title = format!("{} {}", exp.service, exp.symptom);
        writeln!(exp_section, "### 经验 {}（{}）：{}", i + 1, label, title).ok();
        writeln!(exp_section, "症状：{}", exp.symptom).ok();
        if let Some(rc) = &exp.root_cause {
            writeln!(exp_section, "根因：{}", rc).ok();
        }
        writeln!(exp_section, "排查路径：{}", exp.investigation_path).ok();
        if !exp.experience_lesson.is_empty() {
            writeln!(exp_section, "经验：{}", exp.experience_lesson).ok();
        }
        writeln!(exp_section).ok();
    }

    format!(
        "{system}\n\n---\n\n{TOOL_GUIDANCE}\n- 当前会话的 session_id：{session_id}\n\n---\n\n{exp_section}\n---\n\n用户消息：{message}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_build_system_prompt_uses_default_when_no_override() {
        let result = build_system_prompt(None);
        assert_eq!(result, FRIDAY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_system_prompt_uses_default_when_file_not_found() {
        let path = PathBuf::from("/nonexistent/path/friday.md");
        let result = build_system_prompt(Some(&path));
        assert_eq!(result, FRIDAY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_system_prompt_uses_override_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "You are a custom assistant.").unwrap();

        let result = build_system_prompt(Some(&path));
        assert_eq!(result, "You are a custom assistant.");
    }

    #[test]
    fn test_build_system_prompt_falls_back_when_file_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "   \n  ").unwrap();

        let result = build_system_prompt(Some(&path));
        assert_eq!(result, FRIDAY_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_prompt_includes_system_and_message() {
        let result = build_prompt("hello world", None, "test-session");
        assert!(result.contains(FRIDAY_SYSTEM_PROMPT));
        assert!(result.contains("hello world"));
    }

    #[test]
    fn test_build_prompt_uses_override_system_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "Custom system.").unwrap();

        let result = build_prompt("hello", Some(&path), "test-session");
        assert!(result.contains("Custom system."));
        assert!(!result.contains(FRIDAY_SYSTEM_PROMPT));
        assert!(result.contains("hello"));
    }

    use crate::knowledge::experience::{Experience, Outcome};

    fn make_test_experience(outcome: Outcome, root_cause: Option<&str>) -> Experience {
        Experience {
            id: "test-id".to_string(),
            symptom: "OOM".to_string(),
            service: "OrderService".to_string(),
            language: "java".to_string(),
            root_cause: root_cause.map(|s| s.to_string()),
            investigation_path: "jstat -> arthas thread".to_string(),
            experience_lesson: "Check thread count first".to_string(),
            outcome,
            occurrence_count: 1,
            last_seen_at: "2026-08-22T00:00:00Z".to_string(),
            created_at: "2026-08-22T00:00:00Z".to_string(),
            query_text: "OrderService OOM".to_string(),
        }
    }

    #[test]
    fn test_build_prompt_with_experiences_injects_section() {
        let exps = vec![
            make_test_experience(Outcome::Positive, Some("ThreadPool leak")),
            make_test_experience(Outcome::Negative, None),
        ];
        let result = build_prompt_with_experiences("hello", None, "test-session", &exps);

        assert!(result.contains("## 历史经验参考"));
        assert!(result.contains("成功"));
        assert!(result.contains("未成功"));
        assert!(result.contains("ThreadPool leak"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_build_prompt_with_empty_experiences_no_section() {
        let exps: Vec<Experience> = vec![];
        let result = build_prompt_with_experiences("hello", None, "test-session", &exps);

        assert!(!result.contains("## 历史经验参考"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_build_prompt_contains_environment_guidance() {
        let result = build_prompt("hello", None, "s1");
        assert!(result.contains("run_command"));
        assert!(result.contains("list_environments"));
        assert!(result.contains("environment"));
    }

    #[test]
    fn test_build_prompt_injects_session_id() {
        let result = build_prompt("hello", None, "session-abc-123");
        assert!(result.contains("session-abc-123"));
        assert!(result.contains("工具使用"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_build_prompt_with_history_injects_history_section() {
        let result = build_prompt_with_history(
            "继续排查",
            None,
            "session-abc-123",
            "[用户] OOM 排查一下\n[助手] 已定位到内存泄漏\n",
        );
        assert!(result.contains("此前对话记录"), "应包含历史记录段标题");
        assert!(result.contains("已定位到内存泄漏"));
        assert!(result.contains("继续排查"));
        assert!(result.contains("session-abc-123"));
        assert!(result.contains(FRIDAY_SYSTEM_PROMPT));
    }

    #[test]
    fn test_build_prompt_with_experiences_injects_session_id() {
        let exps = vec![make_test_experience(Outcome::Positive, Some("root cause"))];
        let result = build_prompt_with_experiences("hello", None, "session-xyz", &exps);

        assert!(result.contains("session-xyz"));
        assert!(result.contains("工具使用"));
        assert!(result.contains("历史经验参考"));
    }

    #[test]
    fn test_tool_guidance_mentions_ensure_tool() {
        assert!(TOOL_GUIDANCE.contains("ensure_tool"));
        assert!(TOOL_GUIDANCE.contains("list_processes"));
        assert!(TOOL_GUIDANCE.contains("jvm_"));
    }

    #[test]
    fn test_tool_guidance_mentions_transfer_tools() {
        assert!(TOOL_GUIDANCE.contains("file_download"));
        assert!(TOOL_GUIDANCE.contains("file_upload"));
        assert!(TOOL_GUIDANCE.contains("transfer_status"));
    }

    #[test]
    fn test_build_prompt_contains_ensure_tool_guidance() {
        let prompt = build_prompt("帮我看看 OOM", None, "s1");
        assert!(prompt.contains("ensure_tool"));
    }

    #[test]
    fn test_tool_guidance_mentions_heap_tools() {
        assert!(TOOL_GUIDANCE.contains("heap_open"));
        assert!(TOOL_GUIDANCE.contains("heap_leak_suspects"));
        assert!(TOOL_GUIDANCE.contains("heap_dominator_tree"));
        assert!(TOOL_GUIDANCE.contains("heap_path_to_gc_roots"));
        assert!(TOOL_GUIDANCE.contains("heap_close"));
        assert!(TOOL_GUIDANCE.contains("不要让用户手动开 MAT"));
    }

    #[test]
    fn test_tool_guidance_mentions_jfr_tools() {
        assert!(TOOL_GUIDANCE.contains("jfr_record"));
        assert!(TOOL_GUIDANCE.contains("jfr_quick_analysis"));
        assert!(TOOL_GUIDANCE.contains("jfr_compare"));
        assert!(TOOL_GUIDANCE.contains("arthas_profiler"), "JDK 8 fallback guidance required");
    }

    #[test]
    fn test_tool_guidance_mentions_code_tools() {
        assert!(TOOL_GUIDANCE.contains("code_get_repo"));
        assert!(TOOL_GUIDANCE.contains("code_open_repo"));
        assert!(TOOL_GUIDANCE.contains("code_repo_status"));
        assert!(TOOL_GUIDANCE.contains("code_read_file"));
        assert!(TOOL_GUIDANCE.contains("code_search"));
        assert!(TOOL_GUIDANCE.contains("code_list_files"));
        assert!(TOOL_GUIDANCE.contains("commitid"));
        assert!(TOOL_GUIDANCE.contains("优先按 tag"), "tag-first alignment guidance required");
        assert!(TOOL_GUIDANCE.contains("jad"), "cross-check with arthas jad guidance");
    }
}
