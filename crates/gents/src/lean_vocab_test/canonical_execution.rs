use std::future::Future;
use std::pin::Pin;

use serde::Deserialize;

use super::canonical_output::{
    LeanCanonicalMessage, LeanCanonicalSegment, LeanPayloadSpec, LeanTerminalSelection,
};
use super::request_execution_lease::LeanRequestExecutionWorld;

pub(crate) type ExecutionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanCanonicalExecutionCase {
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
pub(crate) struct LeanCanonicalExecutionSeed {
    pub(crate) request_id: u64,
    pub(crate) session_id: u64,
    pub(crate) principal: u64,
    pub(crate) subagent_depth: u64,
    pub(crate) workspace: Option<LeanCanonicalDelegatedWorkspace>,
    pub(crate) remote_routes: Vec<LeanCanonicalRemoteRoute>,
    pub(crate) lease: LeanRequestExecutionWorld,
    pub(crate) transcript_session_id: u64,
    pub(crate) next_sequence: u64,
    pub(crate) segments: Vec<LeanCanonicalSegment>,
    pub(crate) messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub(crate) tool_calls: Vec<u64>,
    pub(crate) in_flight: Vec<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalRemoteRoute {
    pub(crate) call: u64,
    pub(crate) target: u64,
    pub(crate) behavior: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalRemoteTarget {
    pub(crate) call: u64,
    pub(crate) coordinator: u64,
    pub(crate) target: u64,
    pub(crate) behavior: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalToolAdmission {
    pub(crate) document: u64,
    pub(crate) call_id: u64,
    pub(crate) request_id: u64,
    pub(crate) state: String,
    pub(crate) operation: String,
    pub(crate) deadline: u64,
    pub(crate) started_at: Option<u64>,
    pub(crate) current_time: u64,
    pub(crate) failure_class: Option<String>,
    pub(crate) persistence: String,
    pub(crate) await_mode: String,
    pub(crate) cancel_policy: String,
    pub(crate) child_request_id: Option<u64>,
    pub(crate) spawn_behavior_id: Option<u64>,
    pub(crate) delegated_workspace: Option<LeanCanonicalDelegatedWorkspace>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalDelegatedWorkspace {
    pub(crate) workspace_id: u64,
    pub(crate) workspace_owner_agent_did: u64,
    pub(crate) workspace_seal_hash: Option<u64>,
    pub(crate) workspace_authority: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDelegatedChildResolutionCase {
    pub(crate) name: String,
    pub(crate) parent_depth: u32,
    pub(crate) parent_agent: u64,
    pub(crate) child_agent: u64,
    pub(crate) delegated_input: Option<LeanDelegatedChildInput>,
    pub(crate) parent_workspace: Option<LeanCanonicalDelegatedWorkspace>,
    pub(crate) choice: LeanDelegatedChildChoice,
    pub(crate) expected: Option<LeanDelegatedChildResult>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanDelegatedChildChoice {
    None,
    Inherit {
        workspace: LeanObservedChildWorkspace,
    },
    Bind {
        workspace: LeanObservedChildWorkspace,
        requested_authority: Option<String>,
    },
    Provision {
        observed_parent: LeanObservedChildWorkspace,
        parent_path_exact: bool,
        created_child: Option<LeanObservedChildWorkspace>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanObservedChildWorkspace {
    pub(crate) workspace_id: u64,
    pub(crate) workspace_owner_agent_did: u64,
    pub(crate) workspace_seal_hash: Option<u64>,
    pub(crate) state: String,
    pub(crate) available: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDelegatedChildInput {
    pub(crate) source_close_doc_id: u64,
    pub(crate) source_stream: u64,
    pub(crate) arguments: String,
    pub(crate) parent_subagent_depth: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDelegatedChildResult {
    pub(crate) child_depth: u32,
    pub(crate) child_workspace: Option<LeanCanonicalDelegatedWorkspace>,
}

/// A background tool spawned by an already running parent tool. It shares the
/// lifecycle context fields of [`LeanCanonicalToolAdmission`] and additionally
/// names the physical parent tool document that owns it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalSpawnedToolAdmission {
    pub(crate) document: u64,
    pub(crate) parent_tool_document: u64,
    pub(crate) call_id: u64,
    pub(crate) request_id: u64,
    pub(crate) state: String,
    pub(crate) operation: String,
    pub(crate) deadline: u64,
    pub(crate) started_at: Option<u64>,
    pub(crate) current_time: u64,
    pub(crate) failure_class: Option<String>,
    pub(crate) persistence: String,
    pub(crate) await_mode: String,
    pub(crate) cancel_policy: String,
    pub(crate) child_request_id: Option<u64>,
    pub(crate) spawn_behavior_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanCanonicalExecutionOperation {
    AcceptForeground {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        targets: Vec<LeanCanonicalRemoteTarget>,
        admissions: Vec<LeanCanonicalToolAdmission>,
    },
    AcceptRemote {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        targets: Vec<LeanCanonicalRemoteTarget>,
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
    /// Explicit due-only deadline CAS. Output and dispatch never extend the lease.
    RenewLease {
        actor: u64,
        now: u64,
        generation: u64,
        expected_deadline: u64,
    },
    /// Guarded raw output insert. It checks the live lease but never updates it.
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
    /// The same guarded insert attempted by a same-task holder whose sibling
    /// awaits the gate. The holder is unpollable, so nothing may commit.
    AppendOutputWhileSiblingWaits {
        actor: u64,
        now: u64,
        generation: u64,
        record: LeanCanonicalSegment,
    },
    /// Accepted provider turn with caller-supplied closure, header and admissions.
    AcceptTurn {
        actor: u64,
        now: u64,
        generation: u64,
        closing: LeanCanonicalSegment,
        message: LeanCanonicalMessage<LeanPayloadSpec>,
        targets: Vec<LeanCanonicalRemoteTarget>,
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
    /// Advance the compaction watermark across a provider-stable published prefix.
    AdvanceCompactionCursor { actor: u64, now: u64, cursor: u64 },
    /// Integrity revocation: terminate conflicted work without reconstructing it
    /// and without discarding either conflicting fact.
    RevokeCorrupt {
        actor: u64,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: String,
        selection: LeanTerminalSelection,
    },
    /// A distinct immutable fact arriving by replication. It bypasses the local
    /// mutation gate, so the native fixture must insert it as a remote merge and
    /// never route it through the execution owner.
    DeliverReplicatedSegment {
        actor: u64,
        now: u64,
        record: LeanCanonicalSegment,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalRecoveryItem {
    pub(crate) closing: LeanCanonicalSegment,
    pub(crate) message: Option<LeanCanonicalMessage<LeanPayloadSpec>>,
}

impl LeanCanonicalExecutionOperation {
    pub(crate) fn actor_and_now(&self) -> (u64, u64) {
        match self {
            Self::AcceptForeground { actor, now, .. }
            | Self::AcceptRemote { actor, now, .. }
            | Self::Dispatch { actor, now, .. }
            | Self::CloseForegroundTool { actor, now, .. }
            | Self::CompleteForegroundTool { actor, now, .. }
            | Self::DeliverForegroundResult { actor, now, .. }
            | Self::TerminalizeCompleted { actor, now, .. }
            | Self::RecoverExpiredGeneration { actor, now, .. }
            | Self::RecoverExpiredTerminal { actor, now, .. }
            | Self::ClosePartial { actor, now, .. }
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
pub(crate) struct LeanCanonicalExecutionObservation {
    pub(crate) accepted: bool,
    pub(crate) generation: Option<u64>,
    pub(crate) terminal_generation: Option<u64>,
    pub(crate) request_state: String,
    pub(crate) tool_state: Option<String>,
    pub(crate) tool_stuck_since: Option<u64>,
    pub(crate) tool_cancel_intent_at: Option<u64>,
    pub(crate) in_flight: bool,
    pub(crate) next_sequence: u64,
    pub(crate) accepted_sequence: Option<u64>,
    pub(crate) physical_tool_request: Option<u64>,
    pub(crate) lease_deadline: Option<u64>,
    pub(crate) compaction_cursor: Option<u64>,
    /// Exact durable rows. Fixture output is deterministically ordered, while
    /// harness comparison treats native rows as multisets with multiplicity.
    pub(crate) segments: Vec<LeanCanonicalSegment>,
    pub(crate) messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
}

fn exact_multiset_eq<T: PartialEq>(left: &[T], right: &[T]) -> bool {
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
pub(crate) trait CanonicalExecutionAdapter {
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

/// Initializes one native fixture and drives the entire generated script on
/// that same handle. The adapter cannot inspect the Lean expectations.
pub(crate) async fn assert_native_execution_case<A>(
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
            .map_err(|error| format!("{name} step {index}: native adapter failed: {error}"))?;
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
    use super::{exact_multiset_eq, LeanCanonicalExecutionCase, LeanCanonicalExecutionOperation};

    #[test]
    fn exact_multiset_comparison_is_order_independent_and_multiplicity_sensitive() {
        assert!(exact_multiset_eq(&[1, 2, 1], &[2, 1, 1]));
        assert!(!exact_multiset_eq(&[1, 2], &[1, 3]));
        assert!(!exact_multiset_eq(&[1, 1], &[1, 2]));
    }

    #[test]
    fn generated_remote_route_cases_decode_behavior_and_workspace() {
        let cases = &super::super::lean_contract_snapshot().canonical_execution_gate_cases;
        let case = cases
            .iter()
            .find(|case| {
                matches!(case,
                LeanCanonicalExecutionCase::NativeExecution { name, .. }
                    if name == "real_spawn_route_workspace_drift_rejected_on_replay")
            })
            .expect("Lean exports immutable remote workspace replay case");
        let LeanCanonicalExecutionCase::NativeExecution {
            seed, operations, ..
        } = case
        else {
            unreachable!()
        };
        assert_eq!(seed.remote_routes[0].behavior, 8);
        let LeanCanonicalExecutionOperation::AcceptRemote {
            targets,
            admissions,
            ..
        } = &operations[0]
        else {
            panic!("first step must accept remote admission")
        };
        assert_eq!(targets[0].behavior, 8);
        let workspace = admissions[0]
            .delegated_workspace
            .as_ref()
            .expect("remote admission carries workspace source");
        assert_eq!(workspace.workspace_authority, "readOnly");
    }
}
