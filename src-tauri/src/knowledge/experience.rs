use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Outcome {
    Positive,
    Negative,
    Uncertain,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Positive => "positive",
            Outcome::Negative => "negative",
            Outcome::Uncertain => "uncertain",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "positive" => Outcome::Positive,
            "negative" => Outcome::Negative,
            _ => Outcome::Uncertain,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Experience {
    pub id: String,
    pub symptom: String,
    pub service: String,
    pub language: String,
    pub root_cause: Option<String>,
    pub investigation_path: String,
    pub experience_lesson: String,
    pub outcome: Outcome,
    pub occurrence_count: i64,
    pub last_seen_at: String,
    pub created_at: String,
    pub query_text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outcome_round_trip() {
        for outcome in [Outcome::Positive, Outcome::Negative, Outcome::Uncertain] {
            let s = outcome.as_str();
            assert_eq!(Outcome::from_str(s), outcome);
        }
    }

    #[test]
    fn test_outcome_from_str_unknown_defaults_uncertain() {
        assert_eq!(Outcome::from_str("garbage"), Outcome::Uncertain);
    }
}
