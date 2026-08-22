use serde::{Deserialize, Serialize};

use super::experience::Outcome;

/// The structured output expected from spawn_one_shot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmOutput {
    pub summary: String,
    pub symptom: String,
    pub service: String,
    pub language: String,
    pub root_cause: Option<String>,
    pub investigation_path: String,
    pub experience_lesson: String,
    pub outcome: String,
}

/// Result of parsing LLM output. Fields may be None if extraction fell back.
#[derive(Clone, Debug)]
pub struct ParsedOutput {
    pub summary: String,
    pub symptom: Option<String>,
    pub service: Option<String>,
    pub language: Option<String>,
    pub root_cause: Option<String>,
    pub investigation_path: Option<String>,
    pub experience_lesson: Option<String>,
    pub outcome: Outcome,
    pub extraction_succeeded: bool,
}

/// Parse LLM stdout with layered fallback:
/// 1. Extract JSON code block (```json ... ```)
/// 2. Try raw JSON line scanning
/// 3. Partial field degradation
/// 4. Rule-based fallback (take first 500 chars as summary)
pub fn parse_llm_output(stdout: &str, fallback_outcome: Outcome) -> ParsedOutput {
    // Step 1: Try JSON code block extraction
    if let Some(json_str) = extract_json_code_block(stdout) {
        if let Ok(parsed) = serde_json::from_str::<LlmOutput>(&json_str) {
            return build_parsed_output(parsed, true);
        }
    }

    // Step 2: Try raw JSON line scanning
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(parsed) = serde_json::from_str::<LlmOutput>(trimmed) {
                return build_parsed_output(parsed, true);
            }
        }
    }

    // Step 3: Fallback — take first 500 chars as summary, use fallback outcome
    let summary: String = stdout.chars().take(500).collect();

    tracing::warn!("LLM output parsing failed, using fallback");
    ParsedOutput {
        summary,
        symptom: None,
        service: None,
        language: None,
        root_cause: None,
        investigation_path: None,
        experience_lesson: None,
        outcome: fallback_outcome,
        extraction_succeeded: false,
    }
}

fn extract_json_code_block(text: &str) -> Option<String> {
    let start = text.find("```json").or_else(|| text.find("```"))?;
    let after_fence = if text[start..].starts_with("```json") {
        start + 7
    } else {
        start + 3
    };

    // Skip whitespace (including newlines) after the fence marker.
    let content_start = after_fence
        + text[after_fence..]
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())?
            .0;

    // Find the closing fence.
    let rel_end = text[content_start..].find("```")?;
    Some(text[content_start..content_start + rel_end].trim().to_string())
}

fn build_parsed_output(parsed: LlmOutput, succeeded: bool) -> ParsedOutput {
    let outcome = Outcome::from_str(&parsed.outcome);

    let outcome = if outcome == Outcome::Positive && parsed.root_cause.is_none() {
        tracing::warn!("positive outcome but no root_cause, degrading to uncertain");
        Outcome::Uncertain
    } else {
        outcome
    };

    ParsedOutput {
        summary: parsed.summary,
        symptom: Some(parsed.symptom),
        service: Some(parsed.service),
        language: Some(if parsed.language.is_empty() {
            "unknown".to_string()
        } else {
            parsed.language
        }),
        root_cause: parsed.root_cause,
        investigation_path: Some(parsed.investigation_path),
        experience_lesson: Some(parsed.experience_lesson),
        outcome,
        extraction_succeeded: succeeded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_code_block() {
        let stdout = "Here is the summary:\n```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"thread leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"check threads\",\"outcome\":\"positive\"}\n```\nDone.";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert!(result.extraction_succeeded);
        assert_eq!(result.summary, "OOM");
        assert_eq!(result.symptom.as_deref(), Some("OOM"));
        assert_eq!(result.outcome, Outcome::Positive);
        assert_eq!(result.root_cause.as_deref(), Some("thread leak"));
    }

    #[test]
    fn test_parse_raw_json_line() {
        let stdout = "{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"thread leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"check threads\",\"outcome\":\"positive\"}";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert!(result.extraction_succeeded);
        assert_eq!(result.summary, "OOM");
    }

    #[test]
    fn test_parse_positive_without_root_cause_degrades_to_uncertain() {
        let stdout = "```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":null,\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"positive\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert_eq!(result.outcome, Outcome::Uncertain);
    }

    #[test]
    fn test_parse_missing_outcome_defaults_uncertain() {
        let stdout = "```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert_eq!(result.outcome, Outcome::Uncertain);
    }

    #[test]
    fn test_parse_empty_language_defaults_unknown() {
        let stdout = "```json\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"\",\"root_cause\":\"leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"positive\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert_eq!(result.language.as_deref(), Some("unknown"));
    }

    #[test]
    fn test_parse_fallback_takes_first_500_chars() {
        let long_text = "x".repeat(600);
        let result = parse_llm_output(&long_text, Outcome::Negative);
        assert!(!result.extraction_succeeded);
        assert_eq!(result.summary.len(), 500);
        assert_eq!(result.outcome, Outcome::Negative);
    }

    #[test]
    fn test_parse_fallback_short_text() {
        let result = parse_llm_output("some text", Outcome::Negative);
        assert!(!result.extraction_succeeded);
        assert_eq!(result.summary, "some text");
    }

    #[test]
    fn test_parse_json_block_without_json_tag() {
        let stdout = "```\n{\"summary\":\"OOM\",\"symptom\":\"OOM\",\"service\":\"OrderService\",\"language\":\"java\",\"root_cause\":\"leak\",\"investigation_path\":\"jstat\",\"experience_lesson\":\"\",\"outcome\":\"positive\"}\n```";
        let result = parse_llm_output(stdout, Outcome::Uncertain);
        assert!(result.extraction_succeeded);
        assert_eq!(result.summary, "OOM");
    }
}
