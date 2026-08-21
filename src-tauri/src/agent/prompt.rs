use std::path::Path;

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

pub fn build_prompt(message: &str, override_path: Option<&Path>) -> String {
    let system = build_system_prompt(override_path);
    format!("{system}\n\n---\n\n用户消息：{message}")
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
        let result = build_prompt("hello world", None);
        assert!(result.contains(FRIDAY_SYSTEM_PROMPT));
        assert!(result.contains("hello world"));
    }

    #[test]
    fn test_build_prompt_uses_override_system_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("friday.md");
        std::fs::write(&path, "Custom system.").unwrap();

        let result = build_prompt("hello", Some(&path));
        assert!(result.contains("Custom system."));
        assert!(!result.contains(FRIDAY_SYSTEM_PROMPT));
        assert!(result.contains("hello"));
    }
}
