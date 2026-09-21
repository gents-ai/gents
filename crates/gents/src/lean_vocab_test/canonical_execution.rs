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
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalExecutionSeed {
    pub(crate) request_id: u64,
    pub(crate) session_id: u64,
    pub(crate) principal: u64,
    pub(crate) remote_routes: Vec<LeanCanonicalRemoteRoute>,
    pub(crate) lease: LeanRequestExecutionWorld,
    pub(crate) transcript_session_id: u64,
    pub(crate) next_sequence: u64,
    pub(crate) segments: Vec<LeanCanonicalSegment>,
    pub(crate) messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    pub(crate) tool_calls: Vec<u64>,
    pub(crate) in_flight: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalRemoteRoute {
    pub(crate) call: u64,
    pub(crate) target: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalRemoteTarget {
    pub(crate) call: u64,
    pub(crate) coordinator: u64,
    pub(crate) target: u64,
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
            | Self::DeliverForegroundResult { actor, now, .. }
            | Self::TerminalizeCompleted { actor, now, .. }
            | Self::RecoverExpiredGeneration { actor, now, .. } => (*actor, *now),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanCanonicalExecutionObservation {
    pub(crate) accepted: bool,
    pub(crate) generation: Option<u64>,
    pub(crate) request_state: String,
    pub(crate) tool_state: Option<String>,
    pub(crate) tool_stuck_since: Option<u64>,
    pub(crate) tool_cancel_intent_at: Option<u64>,
    pub(crate) in_flight: bool,
    pub(crate) message_count: usize,
    pub(crate) next_sequence: u64,
    pub(crate) accepted_sequence: Option<u64>,
    pub(crate) physical_tool_request: Option<u64>,
    pub(crate) lease_deadline: Option<u64>,
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
        if &actual != expected {
            return Err(format!(
                "{name} step {index} ({:?}): expected {expected:?}, got {actual:?}",
                action
            ));
        }
    }
    Ok(())
}
