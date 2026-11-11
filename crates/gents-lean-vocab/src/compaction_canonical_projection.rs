use serde::Deserialize;

use super::canonical_execution::ExecutionFuture;
use super::canonical_output::{
    LeanCanonicalMessage, LeanCanonicalSegment, LeanPayloadSpec, LeanReconstructedMessage,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCompactionInput {
    pub session: u64,
    pub request: u64,
    pub segments: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub source: Vec<u64>,
    pub context_window: u64,
    pub threshold_basis_points: u64,
    pub configured_max_output_tokens: u64,
    pub can_fit: bool,
    pub prefix_length: u64,
    pub checkpoint: u64,
    pub initial_estimate_tokens: u64,
    pub rebuilt_estimate_tokens: u64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCompactionObservation {
    /// None: reconstruction failed before the projection callback; Some(empty):
    /// a successful callback with an empty native list.
    pub initial_native: Option<Vec<LeanReconstructedMessage>>,
    pub rebuilt_checkpoint: Option<u64>,
    pub rebuilt_native: Option<Vec<LeanReconstructedMessage>>,
    pub result: String,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalCompactionCase {
    pub name: String,
    pub input: LeanCanonicalCompactionInput,
    pub expected: LeanCanonicalCompactionObservation,
}

pub trait CanonicalCompactionAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: &'a LeanCanonicalCompactionInput,
    ) -> ExecutionFuture<'a, Result<LeanCanonicalCompactionObservation, Self::Error>>;
}

pub async fn assert_canonical_compaction_cases<A>(
    cases: &[LeanCanonicalCompactionCase],
    adapter: &mut A,
) -> Result<(), String>
where
    A: CanonicalCompactionAdapter,
{
    for case in cases {
        let actual = adapter.observe(&case.input).await.map_err(|error| {
            format!("{}: native canonical compaction failed: {error}", case.name)
        })?;
        if actual != case.expected {
            return Err(format!(
                "{}: canonical compaction mismatch: expected {:?}, got {actual:?}",
                case.name, case.expected
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRepairedProjectionInput {
    pub source: Vec<u64>,
    pub project_succeeds: bool,
    pub estimate_tokens: Option<u64>,
    pub effective_input_budget: u64,
    pub context_window: u64,
    pub configured_max_output_tokens: u64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRepairedProjectionObservation {
    pub result: String,
    pub output_tokens: Option<u64>,
    pub projected_source: Option<Vec<u64>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRepairedProjectionCase {
    pub name: String,
    pub input: LeanRepairedProjectionInput,
    pub expected: LeanRepairedProjectionObservation,
}

pub trait RepairedProjectionAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: &'a LeanRepairedProjectionInput,
    ) -> ExecutionFuture<'a, Result<LeanRepairedProjectionObservation, Self::Error>>;
}

pub async fn assert_repaired_projection_cases<A>(
    cases: &[LeanRepairedProjectionCase],
    adapter: &mut A,
) -> Result<(), String>
where
    A: RepairedProjectionAdapter,
{
    for case in cases {
        let actual = adapter.observe(&case.input).await.map_err(|error| {
            format!("{}: native repaired projection failed: {error}", case.name)
        })?;
        if actual != case.expected {
            return Err(format!(
                "{}: repaired projection mismatch: expected {:?}, got {actual:?}",
                case.name, case.expected
            ));
        }
    }
    Ok(())
}
