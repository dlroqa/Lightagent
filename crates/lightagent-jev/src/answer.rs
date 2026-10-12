//! What comes back, validated against what was asked.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::error::JevError;
use crate::question::{EvaluationRequest, MAX_CHOICE_OPTIONS, Question};

/// Probabilities must sum to one within this tolerance.
const SUM_TOLERANCE: f64 = 0.02;
/// The reported model id is metadata; anything longer is not a model id.
const MAX_MODEL_ID_CHARS: usize = 128;

/// A validated answer to one question.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// The selected option, its full distribution, and TypeSafe's confidence.
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// The probability-weighted level (0-based), the per-level distribution
    /// keyed by level index, and TypeSafe's confidence.
    Score {
        score: f64,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// The probability that the answer is yes. TypeSafe reports no confidence
    /// for a Noul, and none is derived here.
    Noul { probability: f64 },
}

impl Answer {
    /// TypeSafe's reported confidence, or `None` for a Noul, which has none.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Choice { confidence, .. } | Self::Score { confidence, .. } => Some(*confidence),
            Self::Noul { .. } => None,
        }
    }
}

/// Token usage reported with an answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// A typed SystemOne response.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    /// The versioned model that answered, e.g. `jev-1.13.0`.
    pub model: Option<String>,
    /// One validated answer per question asked, under the same ids.
    pub answers: BTreeMap<String, Answer>,
    pub usage: Option<Usage>,
}

/// The compatibility `output` envelope accepted from local adapters that
/// predate the typed API. It names a route and/or profile with a confidence;
/// it is not a TypeSafe response and carries no probabilities, model id or
/// usage.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyOutput {
    /// The first of `output.model`, `output.route`, `output.choice`.
    pub route: Option<String>,
    pub profile: Option<String>,
    /// `output.confidence`, else `output.score`.
    pub confidence: f64,
}

/// A parsed response in one of the two accepted formats.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    TypeSafe(Evaluation),
    LegacyOutput(LegacyOutput),
}

/// Which wire format a [`Response`] arrived in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseFormat {
    /// The documented TypeSafe `answers` schema.
    TypeSafe,
    /// The compatibility `output` envelope.
    LegacyOutput,
}

impl Response {
    pub fn format(&self) -> ResponseFormat {
        match self {
            Self::TypeSafe(_) => ResponseFormat::TypeSafe,
            Self::LegacyOutput(_) => ResponseFormat::LegacyOutput,
        }
    }
}

/// Validate `body` as the answer to `request`.
///
/// A body with `answers` is parsed only as TypeSafe: every question asked must
/// be answered with its own type, a distribution over exactly the options or
/// levels asked, and in-range values. Answers to questions not asked are
/// ignored. A body without `answers` but with `output` is the compatibility
/// envelope. Anything else is malformed.
pub fn parse_response(body: &Value, request: &EvaluationRequest) -> Result<Response, JevError> {
    if let Some(answers) = body.get("answers") {
        let answers = answers
            .as_object()
            .ok_or(JevError::Malformed("answers is not an object"))?;
        let mut parsed = BTreeMap::new();
        for (id, question) in &request.questions {
            let answer = answers
                .get(id)
                .ok_or(JevError::Malformed("a question was not answered"))?;
            parsed.insert(id.clone(), parse_answer(answer, question)?);
        }
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty() && model.chars().count() <= MAX_MODEL_ID_CHARS)
            .map(str::to_owned);
        let usage = body
            .get("usage")
            .and_then(Value::as_object)
            .map(|usage| Usage {
                input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
                output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            });
        return Ok(Response::TypeSafe(Evaluation {
            model,
            answers: parsed,
            usage,
        }));
    }
    if body.get("output").is_some() {
        return legacy_output(body).map(Response::LegacyOutput);
    }
    Err(JevError::Malformed("no answers"))
}

fn parse_answer(answer: &Value, question: &Question) -> Result<Answer, JevError> {
    if answer.get("type").and_then(Value::as_str) != Some(question.kind()) {
        return Err(JevError::Malformed("an answer has the wrong type"));
    }
    match question {
        Question::Choice { criteria, .. } => {
            let choice = answer
                .get("choice")
                .and_then(Value::as_str)
                .ok_or(JevError::Malformed("a choice answer has no choice"))?;
            let confidence = unit_field(answer, "confidence")?;
            let options: BTreeSet<String> = criteria.keys().cloned().collect();
            let probabilities = distribution(answer, &options)?;
            let selected = *probabilities
                .get(choice)
                .ok_or(JevError::Malformed("the choice is not an option asked"))?;
            if probabilities
                .values()
                .any(|probability| *probability > selected + 1e-9)
            {
                return Err(JevError::Malformed(
                    "the choice is not the most probable option",
                ));
            }
            Ok(Answer::Choice {
                choice: choice.to_owned(),
                probabilities,
                confidence,
            })
        }
        Question::Score { criteria, .. } => {
            let score = answer
                .get("score")
                .and_then(Value::as_f64)
                .filter(|score| {
                    score.is_finite()
                        && *score >= -1e-9
                        && *score <= criteria.len().saturating_sub(1) as f64 + 1e-9
                })
                .ok_or(JevError::Malformed("a score is out of range"))?;
            let confidence = unit_field(answer, "confidence")?;
            let levels: BTreeSet<String> =
                (0..criteria.len()).map(|level| level.to_string()).collect();
            let probabilities = distribution(answer, &levels)?;
            Ok(Answer::Score {
                score,
                probabilities,
                confidence,
            })
        }
        Question::Noul { .. } => Ok(Answer::Noul {
            probability: unit_field(answer, "noul")?,
        }),
    }
}

fn unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn unit_field(answer: &Value, field: &str) -> Result<f64, JevError> {
    answer
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| unit(*value))
        .ok_or(JevError::Malformed(
            "a probability or confidence is out of range",
        ))
}

/// A probability distribution over exactly `expected` keys that sums to one.
fn distribution(
    answer: &Value,
    expected: &BTreeSet<String>,
) -> Result<BTreeMap<String, f64>, JevError> {
    let object: &Map<String, Value> = answer
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or(JevError::Malformed("an answer has no probabilities"))?;
    if object.is_empty()
        || object.len() > MAX_CHOICE_OPTIONS
        || object.len() != expected.len()
        || object.keys().any(|key| !expected.contains(key))
    {
        return Err(JevError::Malformed(
            "probabilities do not match the options asked",
        ));
    }
    let mut total = 0.0;
    let mut probabilities = BTreeMap::new();
    for (key, value) in object {
        let probability = value
            .as_f64()
            .filter(|probability| unit(*probability))
            .ok_or(JevError::Malformed("a probability is out of range"))?;
        total += probability;
        probabilities.insert(key.clone(), probability);
    }
    if (total - 1.0).abs() > SUM_TOLERANCE {
        return Err(JevError::Malformed("probabilities do not sum to one"));
    }
    Ok(probabilities)
}

/// The compatibility envelope, parsed exactly as the original router did:
/// every naming field present must be a string, at least one of route and
/// profile must be named and non-empty, and the confidence must be in range.
fn legacy_output(body: &Value) -> Result<LegacyOutput, JevError> {
    let malformed = JevError::Malformed("the output envelope is invalid");
    let Some(result) = body.get("output") else {
        return Err(malformed);
    };
    for field in ["model", "route", "choice", "profile"] {
        if result.get(field).is_some_and(|value| !value.is_string()) {
            return Err(malformed);
        }
    }
    let route = ["model", "route", "choice"]
        .iter()
        .find_map(|field| result.get(*field).and_then(Value::as_str))
        .map(str::trim);
    let profile = result.get("profile").and_then(Value::as_str).map(str::trim);
    let confidence = ["confidence", "score"]
        .iter()
        .find_map(|field| result.get(*field).and_then(Value::as_f64));
    let Some(confidence) = confidence else {
        return Err(malformed);
    };
    if (route.is_none() && profile.is_none())
        || route.is_some_and(str::is_empty)
        || profile.is_some_and(str::is_empty)
        || !unit(confidence)
    {
        return Err(malformed);
    }
    Ok(LegacyOutput {
        route: route.map(str::to_owned),
        profile: profile.map(str::to_owned),
        confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> EvaluationRequest {
        EvaluationRequest::new(
            "jev-latest",
            "state",
            BTreeMap::from([
                (
                    "task".to_owned(),
                    Question::choice(
                        "Which task?",
                        [
                            ("coding".to_owned(), String::new()),
                            ("research".to_owned(), String::new()),
                        ],
                    ),
                ),
                (
                    "complexity".to_owned(),
                    Question::score(
                        "How complex?",
                        ["low".to_owned(), "medium".to_owned(), "high".to_owned()],
                    ),
                ),
                ("tools".to_owned(), Question::noul("Are tools needed?")),
            ]),
        )
    }

    fn typed() -> Value {
        json!({
            "model": "jev-1.13.0",
            "answers": {
                "task": {"type": "choice", "choice": "coding", "probabilities": {"coding": 0.9, "research": 0.1}, "confidence": 0.8},
                "complexity": {"type": "score", "score": 1.05, "legend": {"0": "low", "1": "medium", "2": "high"}, "probabilities": {"0": 0.0, "1": 0.95, "2": 0.05}, "confidence": 0.92},
                "tools": {"type": "noul", "noul": 0.7, "confidence": 0.99},
                "unasked": {"type": "noul", "noul": 1.0}
            },
            "usage": {"input_tokens": 318, "output_tokens": 34}
        })
    }

    #[test]
    fn parses_every_typed_answer_with_version_and_usage() {
        let Response::TypeSafe(evaluation) = parse_response(&typed(), &request()).unwrap() else {
            panic!("expected the TypeSafe format");
        };
        assert_eq!(evaluation.model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(
            evaluation.usage,
            Some(Usage {
                input_tokens: Some(318),
                output_tokens: Some(34)
            })
        );
        assert_eq!(evaluation.answers.len(), 3, "unasked answers are ignored");
        assert_eq!(evaluation.answers["task"].confidence(), Some(0.8));
        assert_eq!(evaluation.answers["complexity"].confidence(), Some(0.92));
        assert_eq!(
            evaluation.answers["tools"],
            Answer::Noul { probability: 0.7 }
        );
        assert_eq!(
            evaluation.answers["tools"].confidence(),
            None,
            "a noul has no confidence, even if a server sends one"
        );
    }

    #[test]
    fn rejects_answers_that_do_not_fit_the_question() {
        let cases: [(&str, Value); 9] = [
            ("/answers/task/type", json!("noul")),
            ("/answers/task/choice", json!("cooking")),
            ("/answers/task/choice", json!("research")),
            ("/answers/task/confidence", json!(1.5)),
            ("/answers/task/probabilities", json!({"coding": 1.0})),
            (
                "/answers/task/probabilities",
                json!({"coding": 0.5, "research": 0.1}),
            ),
            ("/answers/complexity/score", json!(2.5)),
            (
                "/answers/complexity/probabilities",
                json!({"0": 0.5, "1": 0.5}),
            ),
            ("/answers/tools/noul", json!(-0.1)),
        ];
        for (pointer, value) in cases {
            let mut body = typed();
            *body.pointer_mut(pointer).unwrap() = value;
            assert!(
                matches!(
                    parse_response(&body, &request()),
                    Err(JevError::Malformed(_))
                ),
                "{pointer} should be rejected"
            );
        }
        let mut missing = typed();
        missing["answers"].as_object_mut().unwrap().remove("tools");
        assert!(parse_response(&missing, &request()).is_err());
        assert!(parse_response(&json!({"model": "jev-1.13.0"}), &request()).is_err());
    }

    #[test]
    fn identifies_the_legacy_output_envelope() {
        let response = parse_response(
            &json!({"output": {"choice": "careful", "score": 0.9}}),
            &request(),
        )
        .unwrap();
        assert_eq!(response.format(), ResponseFormat::LegacyOutput);
        assert_eq!(
            response,
            Response::LegacyOutput(LegacyOutput {
                route: Some("careful".to_owned()),
                profile: None,
                confidence: 0.9,
            })
        );
        let profile = parse_response(
            &json!({"output": {"profile": " careful ", "confidence": 1}}),
            &request(),
        )
        .unwrap();
        assert_eq!(
            profile,
            Response::LegacyOutput(LegacyOutput {
                route: None,
                profile: Some("careful".to_owned()),
                confidence: 1.0,
            })
        );
    }

    #[test]
    fn rejects_invalid_legacy_envelopes() {
        for output in [
            json!({"model": "fast"}),
            json!({"model": "fast", "confidence": 1.1}),
            json!({"model": "", "confidence": 0.8}),
            json!({"profile": 42, "model": "fast", "confidence": 1}),
            json!({"confidence": 1}),
            json!("fast"),
        ] {
            assert!(
                parse_response(&json!({ "output": output }), &request()).is_err(),
                "{output} should be rejected"
            );
        }
    }

    #[test]
    fn a_typed_body_is_never_read_as_the_legacy_envelope() {
        let body = json!({"answers": {}, "output": {"model": "fast", "confidence": 1}});
        assert!(parse_response(&body, &request()).is_err());
    }
}
