pub use gents_protocol::database_error::AccessCheck;

use super::ConfigClientError;

/// `DecisionRead.Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Complete,
    Unavailable,
    Failed,
}

impl From<AccessCheck> for Outcome {
    /// `DecisionRead.Outcome.ofCheck`.
    fn from(check: AccessCheck) -> Self {
        match check {
            AccessCheck::Unavailable => Self::Unavailable,
            AccessCheck::Failed => Self::Failed,
        }
    }
}

/// `DecisionRead.Step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStep {
    Decide,
    Wait,
    Fail,
}

/// `DecisionRead.step`.
pub fn step(outcome: Outcome) -> DecisionStep {
    match outcome {
        Outcome::Complete => DecisionStep::Decide,
        Outcome::Unavailable => DecisionStep::Wait,
        Outcome::Failed => DecisionStep::Fail,
    }
}

/// What a strict read a decision is taken on returned, decided
/// (`DecisionRead.decides_only_on_a_complete_read`): an access check without
/// an answer becomes a typed error, so no caller acts on a partial read.
pub(crate) fn decided<T>(read: Result<T, ConfigClientError>) -> Result<T, ConfigClientError> {
    let error = match read {
        Ok(rows) => return Ok(rows),
        Err(error) => error,
    };
    match error.access_check().map(|check| step(check.into())) {
        Some(DecisionStep::Wait) => Err(ConfigClientError::DecisionReadUnanswered),
        Some(DecisionStep::Fail) => Err(ConfigClientError::DecisionReadFailed),
        Some(DecisionStep::Decide) | None => Err(error),
    }
}

#[cfg(test)]
mod conformance {
    #[test]
    fn decision_reads_match_the_lean_model() {
        use super::{step, AccessCheck, DecisionStep, Outcome};
        gents_lean_vocab::families::decision_read_step::replay::<Outcome, DecisionStep>(|input| {
            step(input.outcome)
        });
        gents_lean_vocab::families::decision_read_access_check::replay(|input| {
            AccessCheck::from_code(&input.code)
        });
    }
}
