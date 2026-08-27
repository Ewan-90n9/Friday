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
- 当前版本你的工具能力有限，主要依靠对话和分析。后续会集成 jstat、jcmd、arthas 等诊断工具。
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
- 远程命令一律通过 run_command 工具执行，并用 environment 参数指定目标环境（name 来自 list_environments）。
- 优先使用结构化诊断工具，run_command 是兜底。
- 诊断 JVM 相关问题（OOM、GC、线程、CPU 飙高等）时，先调用 ensure_tool 装备 JDK，再用返回的 bins 全路径通过 run_command 执行 jstat/jcmd 等工具（目标环境通常只有 JRE，直接执行 jstat 会失败）。
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
        assert!(TOOL_GUIDANCE.contains("jstat"));
    }

    #[test]
    fn test_build_prompt_contains_ensure_tool_guidance() {
        let prompt = build_prompt("帮我看看 OOM", None, "s1");
        assert!(prompt.contains("ensure_tool"));
    }
}
