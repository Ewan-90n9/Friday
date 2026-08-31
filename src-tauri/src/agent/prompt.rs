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
- arthas 动态诊断（attach 到运行中的 JVM）：list_processes 找 PID → arthas_open(environment, pid)（首次自动下发 arthas 包并 attach，需确认；已 attach 秒回）→ arthas_* 工具诊断（dashboard / thread / sc / sm / jad / watch / trace / stack / monitor / tt / ognl / vmtool / memory / jvm / sysprop / vmoption / profiler 等；args 对象的字段与 arthas 命令参数一致）→ 完成后 arthas_close 或留给空闲自动回收。注意：堆快照走 jvm_heap_dump（不用 arthas 的 heapdump）；arthas_open 报「运行用户不一致且未录入凭证」时，引导用户在环境管理中为该环境添加对应 JVM 用户的凭证后重试；arthas_not_open 报「正在 attach」时稍候重试即可。
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
}
