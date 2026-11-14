use crate::generated::*;
use std::future::Future;
use std::pin::Pin;

pub type ExecutionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl LeanCanonicalExecutionOperation {
    pub fn actor_and_now(&self) -> (u64, u64) {
        match self {
            Self::AcceptForeground { actor, now, .. }
            | Self::AcceptBackground { actor, now, .. }
            | Self::Dispatch { actor, now, .. }
            | Self::CloseForegroundTool { actor, now, .. }
            | Self::CompleteForegroundTool { actor, now, .. }
            | Self::DeliverForegroundResult { actor, now, .. }
            | Self::TerminalizeCompleted { actor, now, .. }
            | Self::RecoverExpiredGeneration { actor, now, .. }
            | Self::RecoverExpiredTerminal { actor, now, .. }
            | Self::ClosePartial { actor, now, .. }
            | Self::CloseAuxiliary { actor, now, .. }
            | Self::RetractBeforeRetry { actor, now, .. }
            | Self::RenewLease { actor, now, .. }
            | Self::AppendOutput { actor, now, .. }
            | Self::AppendToolOutput { actor, now, .. }
            | Self::AppendOutputWhileSiblingWaits { actor, now, .. }
            | Self::AcceptTurn { actor, now, .. }
            | Self::BackgroundTool { actor, now, .. }
            | Self::PublishBackgroundReceipt { actor, now, .. }
            | Self::AdmitSpawnedBackground { actor, now, .. }
            | Self::PublishAuthored { actor, now, .. }
            | Self::AdvanceCompactionCursor { actor, now, .. }
            | Self::RevokeCorrupt { actor, now, .. }
            | Self::DeliverReplicatedSegment { actor, now, .. } => (*actor, *now),
        }
    }
}

fn exact_multiset_eq<T>(left: &[T], right: &[T]) -> bool
where
    T: PartialEq,
{
    if left.len() != right.len() {
        return false;
    }
    let mut matched = vec![false; right.len()];
    left.iter().all(|item| {
        let Some(index) = right
            .iter()
            .enumerate()
            .position(|(index, candidate)| !matched[index] && item == candidate)
        else {
            return false;
        };
        matched[index] = true;
        true
    })
}

fn observations_match(
    actual: &LeanCanonicalExecutionObservation,
    expected: &LeanCanonicalExecutionObservation,
) -> bool {
    if !exact_multiset_eq(&actual.segments, &expected.segments)
        || !exact_multiset_eq(&actual.messages, &expected.messages)
    {
        return false;
    }
    let mut normalized = actual.clone();
    normalized.segments.clone_from(&expected.segments);
    normalized.messages.clone_from(&expected.messages);
    &normalized == expected
}

/// Asynchronous boundary for a stateful native execution fixture. The adapter
/// receives the seed and operations, but never the expected observations.
pub trait CanonicalExecutionAdapter {
    type Error: std::fmt::Display;
    type Native;

    fn initialize<'a>(
        &'a mut self,
        seed: &'a LeanCanonicalExecutionSeed,
    ) -> ExecutionFuture<'a, Result<Self::Native, Self::Error>>;

    fn apply<'a>(
        &'a mut self,
        native: &'a mut Self::Native,
        query_document: u64,
        operation: &'a LeanCanonicalExecutionOperation,
    ) -> ExecutionFuture<'a, Result<LeanCanonicalExecutionObservation, Self::Error>>;
}

pub async fn assert_native_execution_case<A>(
    case: &LeanCanonicalExecutionCase,
    adapter: &mut A,
) -> Result<(), String>
where
    A: CanonicalExecutionAdapter,
{
    let LeanCanonicalExecutionCase::NativeExecution {
        name,
        seed,
        query_document,
        operations,
        expected_observations,
        ..
    } = case
    else {
        return Err("trace summaries are witnesses, not native execution inputs".to_owned());
    };

    if operations.len() != expected_observations.len() {
        return Err(format!(
            "{name}: {} operations but {} expected observations",
            operations.len(),
            expected_observations.len()
        ));
    }
    if operations.is_empty() {
        return Err(format!("{name}: native execution script is empty"));
    }

    let mut native = adapter
        .initialize(seed)
        .await
        .map_err(|error| format!("{name}: native fixture initialization failed: {error}"))?;
    for (index, (action, expected)) in operations.iter().zip(expected_observations).enumerate() {
        let actual = adapter
            .apply(&mut native, *query_document, action)
            .await
            .map_err(|error| format!("{name} step {index}: native adapter failed: {error:#}"))?;
        if !observations_match(&actual, expected) {
            return Err(format!(
                "{name} step {index} ({:?}): expected {expected:?}, got {actual:?}",
                action
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LeanCanonicalExecutionCase, LeanCanonicalExecutionOperation};

    #[test]
    fn generated_background_accept_cases_decode_background_admissions() {
        let cases = &super::super::lean_contract_snapshot().canonical_execution_gate_cases;
        let admissions = cases
            .iter()
            .filter_map(|case| match case {
                LeanCanonicalExecutionCase::NativeExecution { operations, .. }
                | LeanCanonicalExecutionCase::ModelExecution { operations, .. } => Some(operations),
                LeanCanonicalExecutionCase::TraceSummary { .. } => None,
            })
            .flatten()
            .filter_map(|operation| match operation {
                LeanCanonicalExecutionOperation::AcceptBackground { admissions, .. } => {
                    Some(admissions)
                }
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert!(
            !admissions.is_empty(),
            "Lean exports background accept admissions"
        );
        assert!(admissions
            .iter()
            .all(|admission| admission.await_mode == "background"));
    }

    #[test]
    fn generated_title_execution_inventory_keeps_external_premises_explicit() {
        use crate::{LeanCanonicalExecutionCase, LeanRequestPurpose};
        let mut native = 0;
        let mut gaps = Vec::new();
        for case in &crate::lean_contract_snapshot().canonical_execution_gate_cases {
            match case {
                LeanCanonicalExecutionCase::NativeExecution { name, seed, .. }
                    if name.starts_with("title_")
                        || name == "normal_request_rejects_title_source" =>
                {
                    native += 1;
                    assert_eq!(
                        seed.title_binding.is_some(),
                        seed.purpose == LeanRequestPurpose::TitleAudit
                    );
                }
                LeanCanonicalExecutionCase::ModelExecution {
                    name, native_gap, ..
                } if name.starts_with("title_") => {
                    assert!(!native_gap.trim().is_empty(), "{name}");
                    gaps.push(name.as_str());
                }
                _ => {}
            }
        }
        assert_eq!(native, 11);
        assert_eq!(
            gaps,
            [
                "title_recovery_accepts_closed_partial_before_future_dated_raw",
                "title_closed_extent_allows_terminal_after_late_raw",
                "title_terminal_replay_ignores_late_raw",
                "title_rejects_publication_and_tool_dispatch",
            ]
        );
    }
}
