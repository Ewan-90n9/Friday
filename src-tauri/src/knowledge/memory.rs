use crate::agent::spawn::spawn_one_shot;
use crate::app::session::get_session_messages;
use crate::knowledge::embedding::EmbeddingService;
use crate::knowledge::experience::{self, Experience, Outcome};
use crate::knowledge::parsing::parse_llm_output;
use crate::knowledge::summary;
use crate::knowledge::vec_store::VecStore;
use sqlx::SqlitePool;
use std::sync::Arc;

const SIMILARITY_THRESHOLD: f32 = 0.5;

fn distance_to_similarity(distance: f32) -> f32 {
    1.0 / (1.0 + distance)
}

pub async fn recall_experiences(
    pool: &SqlitePool,
    embedding: &EmbeddingService,
    vec_store: &VecStore,
    query_text: &str,
) -> Vec<Experience> {
    let query_vec = match embedding.embed(query_text) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(?e, "failed to embed query for experience recall");
            return Vec::new();
        }
    };

    let positive_ids: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences WHERE outcome = 'positive'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let positive_ids: Vec<String> = positive_ids.into_iter().map(|(id,)| id).collect();

    let negative_ids: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM experiences WHERE outcome = 'negative'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let negative_ids: Vec<String> = negative_ids.into_iter().map(|(id,)| id).collect();

    let positive_results = if positive_ids.is_empty() {
        Vec::new()
    } else {
        vec_store.query_filtered(&query_vec, 2, &positive_ids).unwrap_or_default()
    };

    let negative_results = if negative_ids.is_empty() {
        Vec::new()
    } else {
        vec_store.query_filtered(&query_vec, 1, &negative_ids).unwrap_or_default()
    };

    let mut experiences = Vec::new();

    for (id, distance) in &positive_results {
        if distance_to_similarity(*distance) < SIMILARITY_THRESHOLD {
            continue;
        }
        if let Ok(Some(exp)) = experience::get_experience(pool, id).await {
            experiences.push(exp);
        }
    }

    for (id, distance) in &negative_results {
        if distance_to_similarity(*distance) < SIMILARITY_THRESHOLD {
            continue;
        }
        if let Ok(Some(exp)) = experience::get_experience(pool, id).await {
            experiences.push(exp);
        }
    }

    experiences
}

fn compress_session_data(messages: &[crate::app::session::MessageRow]) -> String {
    use std::fmt::Write;
    let mut output = String::new();
    for msg in messages {
        let role_label = match msg.role.as_str() {
            "user" => "用户",
            "agent" => "Friday",
            other => other,
        };
        writeln!(output, "\n## {} (seq={})", role_label, msg.seq).ok();

        if let Some(content) = &msg.content {
            if !content.is_empty() {
                writeln!(output, "{}", content).ok();
            }
        }

        for part in &msg.parts {
            match part.part_type.as_str() {
                "text" => {
                    if let Some(text) = &part.text {
                        if !text.is_empty() {
                            writeln!(output, "{}", text).ok();
                        }
                    }
                }
                "tool" => {
                    writeln!(output, "\n[工具调用: {}]", part.tool_name.as_deref().unwrap_or("?")).ok();
                    if let Some(args) = &part.tool_args {
                        writeln!(output, "参数: {}", args).ok();
                    }
                    if let Some(tool_output) = &part.tool_output {
                        let lines: Vec<&str> = tool_output.lines().collect();
                        if lines.len() > 20 {
                            writeln!(output, "输出:\n{}\n... ({} 行已截断)", lines[..20].join("\n"), lines.len() - 20).ok();
                        } else {
                            writeln!(output, "输出:\n{}", tool_output).ok();
                        }
                    }
                }
                _ => {}
            }
        }
    }
    output
}

fn build_one_shot_prompt(session_data: &str, fallback_outcome: &str) -> String {
    format!(
        r#"请分析以下诊断会话记录，提取结构化信息。

## 会话记录

{session_data}

## 输出要求

请输出一个 JSON 代码块，包含以下字段：

```json
{{
    "summary": "会话摘要（2-3句话概括诊断过程和结论）",
    "symptom": "症状关键词（如 OOM、CPU飙高、连接池耗尽）",
    "service": "服务名称",
    "language": "编程语言（java/cpp/go/python/unknown，从工具使用推断）",
    "root_cause": "根因（如果定位到，否则 null）",
    "investigation_path": "排查路径（自然语言描述，如 jstat显示GC频繁 → arthas thread发现2000+线程 → 定位ThreadPoolExecutor）",
    "experience_lesson": "经验提炼（可复用的经验，如 OOM先查线程数）",
    "outcome": "positive（成功定位根因）| negative（未定位根因）| uncertain（不确定）"
}}
```

注意：
- outcome 为 {fallback_outcome} 时，说明诊断未正常完成，请据此判断。
- 如果无法确定某个字段，使用空字符串或 null。
- 只输出 JSON 代码块，不要其他文字。"#
    )
}

pub async fn generate_memory(
    pool: SqlitePool,
    session_id: String,
    fallback_outcome: Outcome,
    embedding: Arc<EmbeddingService>,
    vec_store: Arc<VecStore>,
) {
    tracing::info!(session_id = %session_id, "generate_memory started");

    let messages = match get_session_messages(&pool, &session_id).await {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::error!(?e, session_id = %session_id, "failed to read session messages");
            return;
        }
    };

    if messages.is_empty() {
        tracing::warn!(session_id = %session_id, "no messages to summarize");
        return;
    }

    let session_data = compress_session_data(&messages);
    let fallback_str = fallback_outcome.as_str();
    let prompt = build_one_shot_prompt(&session_data, fallback_str);

    let stdout = match spawn_one_shot(&pool, prompt).await {
        Ok(text) => text,
        Err(e) => {
            tracing::error!(?e, session_id = %session_id, "spawn_one_shot failed in generate_memory");
            return;
        }
    };

    let parsed = parse_llm_output(&stdout, fallback_outcome);

    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = summary::insert_summary(&pool, &session_id, &parsed.summary, &now).await {
        tracing::error!(?e, session_id = %session_id, "failed to store session summary");
    }

    if let (Some(symptom), Some(service), Some(language)) =
        (&parsed.symptom, &parsed.service, &parsed.language)
    {
        let _ = sqlx::query(
            "UPDATE sessions SET symptom = ?, service = ?, language = ? WHERE id = ?",
        )
        .bind(symptom)
        .bind(service)
        .bind(language)
        .bind(&session_id)
        .execute(&pool)
        .await;
    }

    let query_text = messages
        .iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();

    if query_text.is_empty() {
        tracing::warn!(session_id = %session_id, "no user message found for vectorization");
        return;
    }

    let exp = Experience {
        id: uuid::Uuid::new_v4().to_string(),
        symptom: parsed.symptom.clone().unwrap_or_default(),
        service: parsed.service.clone().unwrap_or_default(),
        language: parsed.language.clone().unwrap_or("unknown".to_string()),
        root_cause: parsed.root_cause.clone(),
        investigation_path: parsed.investigation_path.clone().unwrap_or_default(),
        experience_lesson: parsed.experience_lesson.clone().unwrap_or_default(),
        outcome: parsed.outcome.clone(),
        occurrence_count: 1,
        last_seen_at: now.clone(),
        created_at: now.clone(),
        query_text: query_text.clone(),
    };

    if let Err(e) = upsert_experience(&pool, &exp, &embedding, &vec_store).await {
        tracing::error!(?e, session_id = %session_id, "failed to upsert experience");
    }

    tracing::info!(session_id = %session_id, "generate_memory completed");
}

pub async fn upsert_experience(
    pool: &SqlitePool,
    exp: &Experience,
    embedding: &EmbeddingService,
    vec_store: &VecStore,
) -> Result<(), String> {
    let query_vec = embedding.embed(&exp.query_text)?;

    let candidates = vec_store.query(&query_vec, 5)?;

    let candidate_ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();

    if exp.outcome == Outcome::Positive {
        for cid in &candidate_ids {
            if let Ok(Some(existing)) = experience::get_experience(pool, cid).await {
                if existing.outcome == Outcome::Positive
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                    && existing.root_cause == exp.root_cause
                {
                    tracing::info!(existing_id = %existing.id, "dedup match, incrementing");
                    experience::update_experience_increment(
                        pool,
                        &existing.id,
                        &exp.investigation_path,
                        &exp.experience_lesson,
                        &exp.last_seen_at,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    return Ok(());
                }

                if existing.outcome == Outcome::Negative
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                {
                    tracing::info!(existing_id = %existing.id, "replacing negative with positive");
                    experience::replace_experience(pool, &existing.id, exp)
                        .await
                        .map_err(|e| e.to_string())?;
                    vec_store.upsert_vector(&existing.id, &query_vec)?;
                    return Ok(());
                }
            }
        }
    } else if exp.outcome == Outcome::Negative {
        for cid in &candidate_ids {
            if let Ok(Some(existing)) = experience::get_experience(pool, cid).await {
                if existing.outcome == Outcome::Negative
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                {
                    tracing::info!(existing_id = %existing.id, "negative dedup match, merging lesson");
                    experience::update_experience_increment(
                        pool,
                        &existing.id,
                        &exp.investigation_path,
                        &exp.experience_lesson,
                        &exp.last_seen_at,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    return Ok(());
                }

                if existing.outcome == Outcome::Positive
                    && existing.symptom == exp.symptom
                    && existing.language == exp.language
                    && existing.service == exp.service
                {
                    tracing::info!(existing_id = %existing.id, "keeping positive, appending negative lesson");
                    experience::update_experience_increment(
                        pool,
                        &existing.id,
                        &exp.investigation_path,
                        &exp.experience_lesson,
                        &exp.last_seen_at,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    return Ok(());
                }
            }
        }
    }

    tracing::info!(exp_id = %exp.id, "inserting new experience");
    experience::insert_experience(pool, exp)
        .await
        .map_err(|e| e.to_string())?;
    vec_store.upsert_vector(&exp.id, &query_vec)?;

    Ok(())
}
