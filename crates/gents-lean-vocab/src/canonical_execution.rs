use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;

use super::canonical_output::{
    LeanCanonicalMessage, LeanCanonicalSegment, LeanPayloadSpec, LeanTerminalSelection,
};
use super::request_execution_lease::LeanRequestExecutionWorld;
use super::{required_nullable, LeanRequestPurpose};

pub type ExecutionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchObservationCase {
    pub name: String,
    pub inputs: Vec<LeanDispatchObservationInput>,
    pub expected: Vec<LeanDispatchObservationResult>,
    pub completion_probe_outcome: String,
    pub completion_probe_accepted: bool,
    pub parent_outcome: String,
    pub expected_after_parent_failure: LeanDispatchParentFailure,
    pub expected_after_policy_settlement: Option<LeanDispatchPolicySettlement>,
    pub expected_after_parent_recovery: Option<LeanDispatchParentRecovery>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchParentRecovery {
    pub state: String,
    pub dispatchable: bool,
    pub terminalized: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchPolicySettlement {
    pub failed: bool,
    pub running: bool,
    pub started: bool,
    pub failure_class: Option<String>,
    pub completion_accepted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSpawnedTargetRejectionCase {
    pub name: String,
    pub parent_tool: String,
    pub completion_probe_outcome: String,
    pub expected: LeanSpawnedTargetRejection,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSpawnedTargetRejection {
    pub failed: bool,
    pub started: bool,
    pub failure_class: Option<String>,
    pub spawned_admitted: bool,
    pub completion_accepted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchParentFailure {
    pub running: bool,
    pub needs_recovery: bool,
    pub message_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchObservationInput {
    pub acknowledged: bool,
    pub policy_allows: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanDispatchObservationResult {
    pub observation: String,
    pub may_invoke: bool,
    pub running: bool,
    pub in_flight: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalExecutionCase {
    TraceSummary {
        name: String,
        completed: bool,
    },
    NativeExecution {
        name: String,
        seed: LeanCanonicalExecutionSeed,
        query_document: u64,
        operations: Vec<LeanCanonicalExecutionOperation>,
        expected_observations: Vec<LeanCanonicalExecutionObservation>,
        native_gap: Option<String>,
    },
    ModelExecution {
        name: String,
        seed: LeanCanonicalExecutionSeed,
        query_document: u64,
        operations: Vec<LeanCanonicalExecutionOperation>,
        expected_observations: Vec<LeanCanonicalExecutionObservation>,
        native_gap: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalExecutionSeed {
    pub purpose: LeanRequestPurpose,
    #[serde(deserialize_with = "required_nullable")]
    pub title_binding: Option<LeanCanonicalTitleBinding>,
    pub request_id: u64,
    pub session_id: u64,
    pub node_did: u64,
    pub lease: LeanRequestExecutionWorld,
    pub transcript_session_id: u64,
    pub next_sequence: u64,
    pub segments: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub tool_calls: Vec<u64>,
    pub in_flight: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalTitleBinding {
    pub purpose: LeanRequestPurpose,
    pub physical_request: u64,
    pub logical_request: u64,
    pub parent_physical: u64,
    pub parent_logical: u64,
    pub node: u64,
    pub session: u64,
    pub authenticated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalToolAdmission {
    pub document: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub state: String,
    pub operation: String,
    pub deadline: u64,
    pub started_at: Option<u64>,
    pub current_time: u64,
    pub failure_class: Option<String>,
    pub persistence: String,
    pub await_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalSpawnedToolAdmission {
    pub document: u64,
    pub parent_tool_document: u64,
    pub call_id: u64,
    pub request_id: u64,
    pub state: String,
    pub operation: String,
    pub deadline: u64,
    pub started_at: Option<u64>,
    pub current_time: u64,
    pub failure_class: Option<String>,
    pub persistence: String,
    pub await_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanCanonicalExecutionOperation {
    AcceptForeground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    AcceptBackground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    Dispatch {
        actor: u64,
        now: u64,
        generation: u64,
        call: u64,
        cancellation_allows: bool,
        tool_policy_allows: bool,
    },
    CloseForegroundTool {
        actor: u64,
        now: u64,
        document: u64,
        authority_outcome: String,
        record: LeanCanonicalSegment,
    },
    CompleteForegroundTool {
        actor: u64,
        now: u64,
        document: u64,
        authority_outcome: String,
        record: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    DeliverForegroundResult {
        actor: u64,
        now: u64,
        document: u64,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    TerminalizeCompleted {
        actor: u64,
        now: u64,
        generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
    },
    RecoverExpiredGeneration {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        deadline: u64,
        items: Vec<LeanCanonicalRecoveryItem>,
    },
    RecoverExpiredTerminal {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
        items: Vec<LeanCanonicalRecoveryItem>,
    },
    ClosePartial {
        actor: u64,
        now: u64,
        generation: u64,
        item: LeanCanonicalRecoveryItem,
    },
    CloseAuxiliary {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
    },
    RetractBeforeRetry {
        actor: u64,
        now: u64,
        generation: u64,
        record: LeanCanonicalSegment,
    },
    RenewLease {
        actor: u64,
        now: u64,
        generation: u64,
        expected_deadline: u64,
    },
    AppendOutput {
        actor: u64,
        now: u64,
        generation: u64,
        record: LeanCanonicalSegment,
    },
    AppendToolOutput {
        actor: u64,
        now: u64,
        document: u64,
        record: LeanCanonicalSegment,
    },
    AppendOutputWhileSiblingWaits {
        actor: u64,
        now: u64,
        generation: u64,
        record: LeanCanonicalSegment,
    },
    AcceptTurn {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    BackgroundTool {
        actor: u64,
        now: u64,
        generation: u64,
        document: u64,
        action: String,
    },
    PublishBackgroundReceipt {
        actor: u64,
        now: u64,
        parent_document: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    AdmitSpawnedBackground {
        actor: u64,
        now: u64,
        generation: u64,
        admission: LeanCanonicalSpawnedToolAdmission,
    },
    PublishAuthored {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
    },
    AdvanceCompactionCursor {
        actor: u64,
        now: u64,
        cursor: u64,
    },
    RevokeCorrupt {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
    },
    DeliverReplicatedSegment {
        actor: u64,
        now: u64,
        record: LeanCanonicalSegment,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalRecoveryItem {
    pub closing: LeanCanonicalSegment,
    pub message: Option<LeanCanonicalMessage<LeanPayloadSpec>>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalExecutionObservation {
    pub accepted: bool,
    pub generation: Option<u64>,
    pub terminal_generation: Option<u64>,
    pub request_state: String,
    #[serde(deserialize_with = "required_nullable")]
    pub terminal_selection: Option<LeanTerminalSelection>,
    pub tool_state: Option<String>,
    pub tool_stuck_since: Option<u64>,
    pub in_flight: bool,
    pub next_sequence: u64,
    pub accepted_sequence: Option<u64>,
    pub physical_tool_request: Option<u64>,
    pub lease_deadline: Option<u64>,
    pub compaction_cursor: Option<u64>,
    pub segments: Vec<LeanCanonicalSegment>,
    pub messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
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
}
