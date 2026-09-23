use serde::Deserialize;

use super::canonical_execution::ExecutionFuture;
use super::canonical_output::{
    LeanCanonicalMessage, LeanCanonicalSegment, LeanPayloadSpec, LeanReconstructedMessage,
};

/// Concrete canonical facts plus controlled projection/estimation observations.
/// The token observations are injectable native-boundary inputs, not measured
/// provider serialization or an alternate tokenizer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalCompactionInput {
    pub(crate) session: u64,
    pub(crate) request: u64,
    pub(crate) segments: Vec<LeanCanonicalSegment>,
    pub(crate) messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub(crate) source: Vec<u64>,
    pub(crate) context_window: u64,
    pub(crate) threshold_basis_points: u64,
    pub(crate) configured_max_output_tokens: u64,
    pub(crate) can_fit: bool,
    pub(crate) prefix_length: u64,
    pub(crate) checkpoint: u64,
    pub(crate) initial_estimate_tokens: u64,
    pub(crate) rebuilt_estimate_tokens: u64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalCompactionObservation {
    /// None means reconstruction failed before the projection callback ran.
    /// Some(empty) is a successful callback with an empty native list.
    pub(crate) initial_native: Option<Vec<LeanReconstructedMessage>>,
    pub(crate) rebuilt_checkpoint: Option<u64>,
    pub(crate) rebuilt_native: Option<Vec<LeanReconstructedMessage>>,
    pub(crate) result: String,
    pub(crate) output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalCompactionCase {
    pub(crate) name: String,
    pub(crate) input: LeanCanonicalCompactionInput,
    pub(crate) expected: LeanCanonicalCompactionObservation,
}

pub(crate) trait CanonicalCompactionAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: &'a LeanCanonicalCompactionInput,
    ) -> ExecutionFuture<'a, Result<LeanCanonicalCompactionObservation, Self::Error>>;
}

pub(crate) async fn assert_canonical_compaction_cases<A: CanonicalCompactionAdapter>(
    cases: &[LeanCanonicalCompactionCase],
    adapter: &mut A,
) -> Result<(), String> {
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
pub(crate) struct LeanRepairedProjectionInput {
    pub(crate) source: Vec<u64>,
    pub(crate) project_succeeds: bool,
    pub(crate) estimate_tokens: Option<u64>,
    pub(crate) effective_input_budget: u64,
    pub(crate) context_window: u64,
    pub(crate) configured_max_output_tokens: u64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRepairedProjectionObservation {
    pub(crate) result: String,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) projected_source: Option<Vec<u64>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRepairedProjectionCase {
    pub(crate) name: String,
    pub(crate) input: LeanRepairedProjectionInput,
    pub(crate) expected: LeanRepairedProjectionObservation,
}

pub(crate) trait RepairedProjectionAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: &'a LeanRepairedProjectionInput,
    ) -> ExecutionFuture<'a, Result<LeanRepairedProjectionObservation, Self::Error>>;
}

pub(crate) async fn assert_repaired_projection_cases<A: RepairedProjectionAdapter>(
    cases: &[LeanRepairedProjectionCase],
    adapter: &mut A,
) -> Result<(), String> {
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
