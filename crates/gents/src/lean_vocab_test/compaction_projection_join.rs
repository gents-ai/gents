use serde::Deserialize;

use super::ExecutionFuture;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRebuiltProjectionInput {
    pub(crate) checkpoint: usize,
    pub(crate) retained_suffix: Vec<usize>,
}

/// Model fixture for the join between canonical projection, estimation,
/// reduction, rebuilt projection, and dispatch authorization. Token counts are
/// abstract observations supplied by an adapter; they are not serialization
/// or tokenizer proofs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCompactionProjectionJoinCase {
    pub(crate) name: String,
    pub(crate) source: Vec<usize>,
    pub(crate) context_window: usize,
    pub(crate) threshold_basis_points: usize,
    pub(crate) configured_max_output_tokens: usize,
    pub(crate) can_fit: bool,
    pub(crate) prefix_length: usize,
    pub(crate) checkpoint: usize,
    pub(crate) initial_projection_succeeds: bool,
    pub(crate) initial_estimate_tokens: Option<usize>,
    pub(crate) rebuild_succeeds: bool,
    pub(crate) rebuilt_estimate_tokens: Option<usize>,
    pub(crate) initial_projection_input: Vec<usize>,
    pub(crate) rebuilt_projection_input: Option<LeanRebuiltProjectionInput>,
    pub(crate) result: String,
    pub(crate) output_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionProjectionJoinInput<'a> {
    pub(crate) source: &'a [usize],
    pub(crate) context_window: usize,
    pub(crate) threshold_basis_points: usize,
    pub(crate) configured_max_output_tokens: usize,
    pub(crate) can_fit: bool,
    pub(crate) prefix_length: usize,
    pub(crate) checkpoint: usize,
    pub(crate) initial_projection_succeeds: bool,
    pub(crate) initial_estimate_tokens: Option<usize>,
    pub(crate) rebuild_succeeds: bool,
    pub(crate) rebuilt_estimate_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionProjectionJoinObservation {
    pub(crate) initial_projection_input: Vec<usize>,
    pub(crate) rebuilt_projection_input: Option<LeanRebuiltProjectionInput>,
    pub(crate) result: String,
    pub(crate) output_tokens: Option<usize>,
}

/// Run a native composed owner against model inputs and compare all externally
/// observable callback invocations and the final decision centrally. The
/// adapter never receives the model's expected result.
pub(crate) trait CompactionProjectionJoinAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: CompactionProjectionJoinInput<'a>,
    ) -> ExecutionFuture<'a, Result<CompactionProjectionJoinObservation, Self::Error>>;
}

pub(crate) async fn assert_compaction_projection_join_cases<A>(
    cases: &[LeanCompactionProjectionJoinCase],
    adapter: &mut A,
) -> Result<(), String>
where
    A: CompactionProjectionJoinAdapter,
{
    for case in cases {
        let actual = adapter
            .observe(CompactionProjectionJoinInput {
                source: &case.source,
                context_window: case.context_window,
                threshold_basis_points: case.threshold_basis_points,
                configured_max_output_tokens: case.configured_max_output_tokens,
                can_fit: case.can_fit,
                prefix_length: case.prefix_length,
                checkpoint: case.checkpoint,
                initial_projection_succeeds: case.initial_projection_succeeds,
                initial_estimate_tokens: case.initial_estimate_tokens,
                rebuild_succeeds: case.rebuild_succeeds,
                rebuilt_estimate_tokens: case.rebuilt_estimate_tokens,
            })
            .await
            .map_err(|error| format!("{}: native join adapter failed: {error}", case.name))?;
        let expected = CompactionProjectionJoinObservation {
            initial_projection_input: case.initial_projection_input.clone(),
            rebuilt_projection_input: case.rebuilt_projection_input.clone(),
            result: case.result.clone(),
            output_tokens: case.output_tokens,
        };
        if actual != expected {
            return Err(format!(
                "{}: compaction projection join mismatch: expected {expected:?}, got {actual:?}",
                case.name
            ));
        }
    }
    Ok(())
}
