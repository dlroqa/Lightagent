//! What is sent: a state and a map of typed questions.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::error::JevError;

/// TypeSafe accepts at most this many options in one Choice.
pub const MAX_CHOICE_OPTIONS: usize = 255;
/// Questions per request. TypeSafe documents no count limit; this bounds what
/// one Lightagent call may ask so a request stays small and cheap.
pub const MAX_QUESTIONS: usize = 64;
/// TypeSafe accepts between 2 and 10 Score levels.
const SCORE_LEVELS: std::ops::RangeInclusive<usize> = 2..=10;

/// One SystemOne request: `state` evaluated against each named question.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EvaluationRequest {
    /// The Jev model or alias, e.g. `jev-latest` or a pinned `jev-1.13.0`.
    pub model: String,
    /// The content evaluated. Callers decide what may leave the machine.
    pub state: Value,
    /// Questions keyed by caller-chosen ids; answers return under the same ids.
    pub questions: BTreeMap<String, Question>,
}

impl EvaluationRequest {
    /// A request asking `questions` about `state`.
    pub fn new(
        model: impl Into<String>,
        state: impl Into<Value>,
        questions: BTreeMap<String, Question>,
    ) -> Self {
        Self {
            model: model.into(),
            state: state.into(),
            questions,
        }
    }

    /// Refuse a request TypeSafe would reject, before anything is sent.
    pub fn validate(&self) -> Result<(), JevError> {
        if self.questions.is_empty() {
            return Err(JevError::InvalidRequest("no questions"));
        }
        if self.questions.len() > MAX_QUESTIONS {
            return Err(JevError::InvalidRequest("too many questions"));
        }
        for question in self.questions.values() {
            match question {
                Question::Choice { criteria, .. }
                    if criteria.is_empty() || criteria.len() > MAX_CHOICE_OPTIONS =>
                {
                    return Err(JevError::InvalidRequest(
                        "a choice needs between 1 and 255 options",
                    ));
                }
                Question::Score { criteria, .. } if !SCORE_LEVELS.contains(&criteria.len()) => {
                    return Err(JevError::InvalidRequest(
                        "a score needs between 2 and 10 levels",
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// A typed SystemOne question.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Select one option from `criteria` (option → description or `null`).
    Choice {
        instructions: Value,
        criteria: BTreeMap<String, Value>,
    },
    /// Rate along ordered levels.
    Score {
        instructions: Value,
        criteria: Vec<Value>,
    },
    /// A yes/no question answered with the probability of yes.
    Noul {
        instructions: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

impl Question {
    /// A Choice over `criteria`, each option described in plain text.
    pub fn choice(
        instructions: impl Into<String>,
        criteria: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self::Choice {
            instructions: Value::String(instructions.into()),
            criteria: criteria
                .into_iter()
                .map(|(option, description)| (option, Value::String(description)))
                .collect(),
        }
    }

    /// A Score over ordered `levels`, lowest first.
    pub fn score(
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::Score {
            instructions: Value::String(instructions.into()),
            criteria: levels.into_iter().map(Value::String).collect(),
        }
    }

    /// A yes/no question.
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: Value::String(instructions.into()),
            criteria: None,
        }
    }

    /// The wire `type` of this question.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
            Self::Noul { .. } => "noul",
        }
    }
}

/// Optional descriptions of what a Noul's yes and no mean.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: Value,
    #[serde(rename = "false")]
    pub no: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_the_documented_wire_shape() {
        let request = EvaluationRequest::new(
            "jev-latest",
            "Help! My payouts have been failing.",
            BTreeMap::from([
                (
                    "department".to_owned(),
                    Question::choice(
                        "Which team?",
                        [("billing".to_owned(), "Payments".to_owned())],
                    ),
                ),
                (
                    "frustration".to_owned(),
                    Question::score("How frustrated?", ["Calm".to_owned(), "Angry".to_owned()]),
                ),
                ("urgent".to_owned(), Question::noul("Is it urgent?")),
            ]),
        );
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({
                "model": "jev-latest",
                "state": "Help! My payouts have been failing.",
                "questions": {
                    "department": {"type": "choice", "instructions": "Which team?", "criteria": {"billing": "Payments"}},
                    "frustration": {"type": "score", "instructions": "How frustrated?", "criteria": ["Calm", "Angry"]},
                    "urgent": {"type": "noul", "instructions": "Is it urgent?"}
                }
            })
        );
        assert!(request.validate().is_ok());
    }

    #[test]
    fn refuses_requests_typesafe_would_reject() {
        let empty = EvaluationRequest::new("jev-latest", "x", BTreeMap::new());
        assert_eq!(
            empty.validate(),
            Err(JevError::InvalidRequest("no questions"))
        );
        let options = (0..=MAX_CHOICE_OPTIONS).map(|index| (index.to_string(), String::new()));
        let wide = EvaluationRequest::new(
            "jev-latest",
            "x",
            BTreeMap::from([("route".to_owned(), Question::choice("pick", options))]),
        );
        assert!(wide.validate().is_err());
        let flat = EvaluationRequest::new(
            "jev-latest",
            "x",
            BTreeMap::from([("s".to_owned(), Question::score("rate", ["one".to_owned()]))]),
        );
        assert!(flat.validate().is_err());
        let many = EvaluationRequest::new(
            "jev-latest",
            "x",
            (0..=MAX_QUESTIONS)
                .map(|index| (index.to_string(), Question::noul("yes?")))
                .collect(),
        );
        assert!(many.validate().is_err());
    }
}
