//! The one typed client for TypeSafe Jev's SystemOne decision API.
//!
//! Every Lightagent component that consults Jev — today the advisory model
//! router, later the task-classification coordinator and the explicit
//! classification tool — goes through [`JevClient`]. HTTP, response
//! validation, retries, secrets and error classification therefore exist once.
//!
//! The contract is the one TypeSafe documents (`docs.typesafe.ai/api`):
//!
//! * `POST {root}/v1/systemone` with `Authorization: Bearer <key>` and a body of
//!   `model`, `state` and a map of typed `questions` ([`EvaluationRequest`]).
//! * Answers come back under the same question ids. `choice` and `score`
//!   answers carry `probabilities` and a `confidence`; a `noul` answer carries
//!   only its probability, and [`Answer::confidence`] reports `None` for it —
//!   no confidence is ever manufactured.
//! * The top-level `model` names the versioned model that answered (an alias
//!   such as `jev-latest` moves between releases) and `usage` reports tokens.
//!   Both are surfaced on [`Evaluation`] for version pinning and cost.
//!
//! A second, non-TypeSafe `output` envelope is still accepted for local
//! adapters that predate the typed API. It is identified explicitly as
//! [`Response::LegacyOutput`] so no caller can mistake it for a typed answer.
//!
//! Jev is advisory. Nothing here grants a tool, relaxes an approval or
//! authorizes a model; callers validate every answer against their own policy.

mod answer;
mod client;
mod error;
mod question;
mod tls;

pub use answer::{
    Answer, Evaluation, LegacyOutput, Response, ResponseFormat, Usage, parse_response,
};
pub use client::{
    DEFAULT_TIMEOUT, JevClient, Limits, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, MAX_RETRIES,
    RetryPolicy, systemone_endpoint,
};
pub use error::JevError;
pub use question::{EvaluationRequest, MAX_CHOICE_OPTIONS, MAX_QUESTIONS, NoulCriteria, Question};
pub use tls::ensure_provider;
