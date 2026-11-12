use super::ExecutionFuture;
use crate::generated::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionProjectionJoinInput<'a> {
    pub source: &'a [usize],
    pub context_window: usize,
    pub threshold_basis_points: usize,
    pub configured_max_output_tokens: usize,
    pub can_fit: bool,
    pub prefix_length: usize,
    pub checkpoint: usize,
    pub initial_projection_succeeds: bool,
    pub initial_estimate_tokens: Option<usize>,
    pub rebuild_succeeds: bool,
    pub rebuilt_estimate_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionProjectionJoinObservation {
    pub initial_projection_input: Vec<usize>,
    pub rebuilt_projection_input: Option<LeanRebuiltProjectionInput>,
    pub result: String,
    pub output_tokens: Option<usize>,
    pub committed_reductions: usize,
}

pub trait CompactionProjectionJoinAdapter {
    type Error: std::fmt::Display;

    fn observe<'a>(
        &'a mut self,
        input: CompactionProjectionJoinInput<'a>,
    ) -> ExecutionFuture<'a, Result<CompactionProjectionJoinObservation, Self::Error>>;
}

pub async fn assert_compaction_projection_join_cases<A>(
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
            committed_reductions: case.committed_reductions,
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
