//! Tool-call lifecycle state machine.
//!
//! Mirrors `crates/gents/src/lifecycle.rs` (`RequestLifecycle`) for tool
//! calls. Defines the persisted vocabulary, failure-class enum, and the
//! `ToolCallLifecycle` struct that owns every persistence write.
//!
//! Lifecycle is daemon-visible only; subprocess kill mechanics, output
//! streaming, and persistent processes are out of scope.
//!
//! ## R2 maintenance obligations
//!
//! This module implements R2 ("Rust subagent data plane"):
//!
//! - SubagentSource (R3) consumes `create_subagent_request` and the bridge methods.
//! - Agent-facing tools (R4) are routed via hook integration that uses
//!   `new_subagent` and recognizes spawn_subagent / wait_task / etc. tool names.
//! - Cross-reference validation (target resolution, parent existence) is wired
//!   by R3's `SubagentSource` work.
//! - Cross-principal delegation (R6) lands with source-inc/gents#9.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

impl ToolCallState {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 6] = [
        Self::Pending,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::TimedOut,
        Self::Cancelled,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "timedOut" => Some(Self::TimedOut),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }
}

// FailureClass and ToolOutcome moved to gents-loop (G-1): the loop's tool
// dispatch classifies outcomes with no DefraDB dependency. Re-exported below
// (with the rest of this module's `pub use` block) so `crate::tool_call_lifecycle`
// keeps every symbol this crate's callers already use.

/// Whether the parent's narrative is blocked on this tool's terminal state.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum AwaitMode {
    #[default]
    Foreground,
    Background,
}

impl AwaitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AwaitMode::Foreground => "foreground",
            AwaitMode::Background => "background",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "foreground" => Some(AwaitMode::Foreground),
            "background" => Some(AwaitMode::Background),
            _ => None,
        }
    }

    pub const ALL: &'static [AwaitMode] = &[AwaitMode::Foreground, AwaitMode::Background];
}

/// Whether parent termination drives the linked child request to .interrupted
/// (cascade) or detaches the child to its own deadline.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CancelPolicy {
    Cascade,
    Detach,
}

impl CancelPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelPolicy::Cascade => "cascade",
            CancelPolicy::Detach => "detach",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "cascade" => Some(CancelPolicy::Cascade),
            "detach" => Some(CancelPolicy::Detach),
            _ => None,
        }
    }

    pub const ALL: &'static [CancelPolicy] = &[CancelPolicy::Cascade, CancelPolicy::Detach];
}

/// Why a tool-call cancellation was requested at the state-machine boundary.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CancelCause {
    Interrupted,
    Deadline,
    UserCancelled,
}

impl CancelCause {
    pub fn as_str(self) -> &'static str {
        match self {
            CancelCause::Interrupted => "interrupted",
            CancelCause::Deadline => "deadline",
            CancelCause::UserCancelled => "userCancelled",
        }
    }

    pub fn from_persisted(s: &str) -> Option<Self> {
        match s {
            "interrupted" => Some(CancelCause::Interrupted),
            "deadline" => Some(CancelCause::Deadline),
            "userCancelled" => Some(CancelCause::UserCancelled),
            _ => None,
        }
    }

    pub const ALL: &'static [CancelCause] = &[
        CancelCause::Interrupted,
        CancelCause::Deadline,
        CancelCause::UserCancelled,
    ];
}

/// The four non-.completed terminal states a child AgentRequest can reach.
/// Used as the argument shape to bridge_failure to project the child terminal
/// onto a parent ToolCallState (.failed for most, .cancelled for .interrupted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChildTerminal {
    Failed {
        reason: String,
        failure_class: FailureClass,
    },
    Dead,
    Interrupted,
    Superseded,
}

impl ChildTerminal {
    /// Lean B2 projection: .interrupted → .cancelled, all others → .failed.
    pub fn projected_state(&self) -> ToolCallState {
        match self {
            ChildTerminal::Interrupted => ToolCallState::Cancelled,
            _ => ToolCallState::Failed,
        }
    }

    /// Persisted vocabulary names for conformance enumeration.
    pub const ALL_KIND: &'static [&'static str] = &["failed", "dead", "interrupted", "superseded"];
}

/// Returned by `bridge_cancel_cascade` (wrapped in Option). The caller — typically
/// R3's daemon interrupt dispatcher — performs the actual write to the child
/// AgentRequest's interrupt_requested_at field. Returning None from
/// bridge_cancel_cascade means no cascade is required: the bridge tool is
/// native (no child link), detached (no cascade), or not in .cancelled state.
#[derive(Clone, Debug)]
pub struct CascadeIntent {
    pub child_request_id: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug)]
pub enum CascadeDispatch {
    Local {
        intent: CascadeIntent,
        child: gents_protocol::row::AgentRequestRow,
    },
    RemoteIntentWritten,
}

use std::sync::Arc;

use defra_node::EmbeddedNode;

pub(crate) mod delivery;
pub(crate) mod query;
// Consumers reconstruct invocation replies through the same physical-identity
// owner as the runtime; admission and mutation internals remain private.
pub use query::{
    load_tool_call_arguments, load_tool_call_presentation, load_tool_call_result,
    render_tool_result, CanonicalToolCallPresentation,
};
mod recovery;
pub(crate) mod runtime;
pub mod subagent_request;
pub(crate) mod subagent_workspace;
mod transition;

pub use gents_loop::tool_call_lifecycle::{FailureClass, ToolOutcome};
#[cfg(test)]
pub(crate) mod admission_fixture;
#[cfg(test)]
mod background_conformance;
#[cfg(test)]
mod background_hook_conformance;
#[cfg(test)]
mod cascade_source_conformance;
#[cfg(test)]
pub(crate) mod completion_owner_conformance;
#[cfg(test)]
mod composed_conformance;
#[cfg(test)]
mod recovery_closeout_conformance;
#[cfg(test)]
mod recovery_conformance;
#[cfg(test)]
mod recovery_orphan_conformance;
#[cfg(test)]
mod request_scope_conformance;

pub(crate) use crate::streaming::AcceptedToolCall;
pub use recovery::{
    deadline_at_is_expired, deadline_is_expired, BackgroundCompletionSideEffectReport,
    OrphanedBackgroundToolReport, SubagentLivenessReport, TerminalParentToolReport,
    ToolCallRecoveryReport,
};
#[cfg(test)]
pub(crate) use subagent_request::create_subagent_request_with_request_id_and_workspace;
pub use subagent_request::{
    create_subagent_request, create_subagent_request_with_request_id,
    create_subagent_request_with_trusted_parent_request_id, MAX_SUBAGENT_DEPTH,
};
pub use transition::IllegalToolCallTransition;

/// State machine struct for an individual tool call. Mirrors `RequestLifecycle`
/// from `lifecycle.rs:189-204`. Owns every persistence write for a single
/// AgentToolCall row.
pub struct ToolCallLifecycle {
    node: Arc<EmbeddedNode>,
    request_id: String,
    request_doc_id: Option<String>,
    session_id: String,
    /// DID of the agent that owns the session this tool call belongs to. Stamped
    /// onto the AgentToolCall row at create so filtered replication can scope the
    /// collection to one agent (`@immutable` scope key).
    agent_did: String,
    requester_did: Option<String>,
    tool_call_id: String,
    /// The provider's optional secondary call identity.  This is immutable
    /// accepted-header provenance and is used to pair the native result.
    call_id: Option<String>,
    message_sequence: u32,
    tool_name: String,
    /// Exact accepted assistant header that introduced this physical call.
    /// It is intentionally absent only on old in-memory constructors that can
    /// no longer dispatch under the canonical schema.
    accepted_header_doc_id: Option<String>,
    arguments: Option<gents_protocol::output::PayloadRef>,
    execution_generation: Option<String>,
    /// A native background execution is admitted by an already accepted
    /// `spawn_process` call.  It is deliberately not an `AcceptedToolCall` of
    /// its own: this is the immutable physical provenance used for dispatch,
    /// recovery and completion notification routing.
    spawned_by_tool_call_doc_id: Option<String>,
    doc_id: Option<String>,
    deadline_at: chrono::DateTime<chrono::Utc>,
    state: ToolCallState,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    failure_class: Option<FailureClass>,
    cancel_cause: Option<CancelCause>,
    selected_tool_identity: Option<SelectedToolIdentity>,
    pub(crate) await_mode: AwaitMode,
    pub(crate) cancel_policy: CancelPolicy,
    pub(crate) child_request_id: Option<String>,
    pub(crate) spawn_target_did: Option<String>,
    pub(crate) spawn_behavior_id: Option<String>,
    pub(crate) unclaimed_deadline_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedToolIdentity {
    service_id: String,
    tool_name: String,
}

/// Input to the one `spawn_process` admission transaction.  The stable
/// `tool_call_id` is derived from the accepted parent document, not generated
/// by a retrying hook invocation; the returned lifecycle always carries the
/// newly-created child's physical document ID separately.
#[derive(Clone, Debug)]
pub(crate) struct SpawnedBackgroundToolAdmission {
    pub(crate) tool_name: String,
    pub(crate) deadline_at: chrono::DateTime<chrono::Utc>,
}

impl ToolCallLifecycle {
    pub(crate) fn execution_generation(&self) -> Option<&str> {
        self.execution_generation.as_deref()
    }
    /// Adopt a pending row that was atomically published with an accepted
    /// provider header.  This is the only constructor for direct canonical
    /// dispatch: it deliberately has no create transition.
    pub(crate) fn from_accepted(
        node: Arc<EmbeddedNode>,
        agent_did: String,
        requester_did: Option<String>,
        accepted: AcceptedToolCall,
        deadline_at: chrono::DateTime<chrono::Utc>,
        await_mode: AwaitMode,
        cancel_policy: CancelPolicy,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            accepted.delegated_input.is_none(),
            "delegated tool admission requires the remote-host lifecycle owner"
        );
        if let Some(plan) = accepted.spawn_admission.as_ref() {
            anyhow::ensure!(
                plan.await_mode == await_mode,
                "dispatch await mode conflicts with immutable spawn admission"
            );
        }
        Ok(Self {
            node,
            request_id: String::new(),
            request_doc_id: Some(accepted.request_doc_id),
            session_id: accepted.session_id,
            agent_did,
            requester_did: requester_did.and_then(|did| {
                let did = did.trim();
                (!did.is_empty()).then(|| did.to_owned())
            }),
            tool_call_id: accepted.id,
            call_id: accepted.call_id,
            message_sequence: accepted.message_sequence,
            tool_name: accepted.tool_name,
            accepted_header_doc_id: Some(accepted.accepted_header_doc_id),
            arguments: Some(accepted.arguments),
            execution_generation: Some(accepted.execution_generation),
            spawned_by_tool_call_doc_id: None,
            doc_id: Some(accepted.tool_call_doc_id),
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode,
            cancel_policy,
            child_request_id: accepted
                .spawn_admission
                .as_ref()
                .map(|plan| plan.child_request_id.clone()),
            spawn_target_did: accepted
                .spawn_admission
                .as_ref()
                .map(|plan| plan.spawn_target_did.clone()),
            spawn_behavior_id: accepted
                .spawn_admission
                .as_ref()
                .map(|plan| plan.spawn_behavior_id.clone()),
            unclaimed_deadline_at: None,
        })
    }

    /// Exact DefraDB document identifier after the first persisted transition.
    pub fn doc_id(&self) -> Option<&str> {
        self.doc_id.as_deref()
    }

    /// Construct an unbound lifecycle value. It cannot dispatch: canonical
    /// dispatch requires `from_accepted` with an already-published tool row.
    /// Remaining callers are being migrated off this legacy constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node: Arc<EmbeddedNode>,
        request_id: String,
        session_id: String,
        agent_did: String,
        tool_call_id: String,
        message_sequence: u32,
        tool_name: String,
        _args: String,
        deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            node,
            request_id,
            request_doc_id: None,
            session_id,
            agent_did,
            requester_did: None,
            tool_call_id,
            call_id: None,
            message_sequence,
            tool_name,
            accepted_header_doc_id: None,
            arguments: None,
            execution_generation: None,
            spawned_by_tool_call_doc_id: None,
            doc_id: None,
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode: AwaitMode::Foreground,
            cancel_policy: CancelPolicy::Cascade,
            child_request_id: None,
            spawn_target_did: None,
            spawn_behavior_id: None,
            unclaimed_deadline_at: None,
        }
    }

    pub fn with_requester_did(mut self, requester_did: Option<String>) -> Self {
        self.requester_did = requester_did.and_then(|did| {
            let did = did.trim();
            (!did.is_empty()).then(|| did.to_string())
        });
        self
    }

    /// Attach the exact owning AgentRequest document to the persisted tool call.
    pub fn with_request_doc_id(mut self, request_doc_id: Option<String>) -> Self {
        self.request_doc_id = request_doc_id.filter(|doc_id| !doc_id.trim().is_empty());
        self
    }

    /// Attach the concrete MCP dispatch identity to the row created by the
    /// first lifecycle transition. Later transitions deliberately never write
    /// these fields, so retries and recovery cannot retarget a persisted call.
    pub(crate) fn with_selected_tool_identity(
        mut self,
        selected: Option<(String, String)>,
    ) -> Self {
        if let Some((service_id, tool_name)) = selected {
            self.selected_tool_identity = Some(SelectedToolIdentity {
                service_id,
                tool_name,
            });
        }
        self
    }

    /// Add the child edge to an already accepted direct invocation before its
    /// one pending-to-running transition. The provider header remains the
    /// only source of the parent tool intent; this only supplies the runtime
    /// child allocation owned by the subagent bridge.
    pub(crate) fn with_subagent_bridge(
        mut self,
        await_mode: AwaitMode,
        cancel_policy: CancelPolicy,
        child_request_id: String,
        spawn_target_did: String,
    ) -> Self {
        self.await_mode = await_mode;
        self.cancel_policy = cancel_policy;
        self.child_request_id = Some(child_request_id);
        self.spawn_target_did = Some(spawn_target_did);
        self
    }

    /// Construct an unbound subagent lifecycle value, not dispatch authority.
    /// Canonical invocation must adopt the immutable published spawn admission
    /// through `from_accepted`; this constructor does not create a tool row.
    #[allow(clippy::too_many_arguments)]
    pub fn new_subagent(
        node: Arc<EmbeddedNode>,
        request_id: String,
        session_id: String,
        agent_did: String,
        tool_call_id: String,
        message_sequence: u32,
        tool_name: String,
        _args: String,
        deadline_at: chrono::DateTime<chrono::Utc>,
        await_mode: AwaitMode,
        cancel_policy: CancelPolicy,
        child_request_id: String,
        spawn_target_did: String,
    ) -> Self {
        Self {
            node,
            request_id,
            request_doc_id: None,
            session_id,
            agent_did,
            requester_did: None,
            tool_call_id,
            call_id: None,
            message_sequence,
            tool_name,
            accepted_header_doc_id: None,
            arguments: None,
            execution_generation: None,
            spawned_by_tool_call_doc_id: None,
            doc_id: None,
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode,
            cancel_policy,
            child_request_id: Some(child_request_id),
            spawn_target_did: Some(spawn_target_did),
            spawn_behavior_id: None,
            unclaimed_deadline_at: None,
        }
    }

    /// Construct an unbound legacy background lifecycle value. Canonical
    /// background processes use `admit_spawned_background` on their accepted
    /// spawn invocation instead; this constructor does not create a tool row.
    #[allow(clippy::too_many_arguments)]
    pub fn new_background_tool(
        node: Arc<EmbeddedNode>,
        request_id: String,
        session_id: String,
        agent_did: String,
        tool_call_id: String,
        message_sequence: u32,
        tool_name: String,
        _args: String,
        deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            node,
            request_id,
            request_doc_id: None,
            session_id,
            agent_did,
            requester_did: None,
            tool_call_id,
            call_id: None,
            message_sequence,
            tool_name,
            accepted_header_doc_id: None,
            arguments: None,
            execution_generation: None,
            spawned_by_tool_call_doc_id: None,
            doc_id: None,
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode: AwaitMode::Background,
            cancel_policy: CancelPolicy::Cascade,
            child_request_id: None,
            spawn_target_did: None,
            spawn_behavior_id: None,
            unclaimed_deadline_at: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_doc_id(&mut self, doc_id: Option<String>) {
        self.doc_id = doc_id;
    }

    /// True once `deadline_at` has passed (inclusive) relative to `now`. Single
    /// owner for the deadline-expiry check shared by the parent-deadline sweep
    /// (`hook.rs::timeout_expired_tool_calls`) and the `wait_process` deadline
    /// check (`hook/persistence/background_tools.rs::persist_wait_tool_call`).
    /// Sweep eligibility is inclusive; fresh output admission instead follows
    /// `ToolExecution.ToolCallContext.deadlineExceeded` (`now > deadline`).
    /// At equality, output is admissible only until the sweep terminalizes the row.
    pub(crate) fn is_deadline_expired(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.deadline_at <= now
    }

    pub(crate) fn is_subagent_bridge(&self) -> bool {
        self.child_request_id.is_some()
    }

    pub(crate) fn is_background_tool_bridge(&self) -> bool {
        self.child_request_id.is_none()
            && self.await_mode == AwaitMode::Background
            && self.spawned_by_tool_call_doc_id.is_none()
    }

    pub(crate) fn is_spawned_background(&self) -> bool {
        self.spawned_by_tool_call_doc_id.is_some()
    }

    pub(crate) fn spawned_by_tool_call_doc_id(&self) -> Option<&str> {
        self.spawned_by_tool_call_doc_id.as_deref()
    }

    pub(crate) fn is_bridge(&self) -> bool {
        self.is_subagent_bridge() || self.is_background_tool_bridge()
    }

    pub(crate) fn terminal_persistence_status(&self, completion_reason: Option<&str>) -> String {
        if self.is_background_tool_bridge() || self.is_spawned_background() {
            completion_reason
                .map(|reason| format!("completionPending:{reason}"))
                .unwrap_or_else(|| "completionPending".to_string())
        } else {
            "completed".to_string()
        }
    }

    pub(crate) fn await_mode(&self) -> AwaitMode {
        self.await_mode
    }

    pub(crate) fn state(&self) -> ToolCallState {
        self.state
    }

    pub(crate) fn request_doc_id(&self) -> Option<&str> {
        self.request_doc_id.as_deref()
    }

    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub(crate) fn requester_did(&self) -> Option<&str> {
        self.requester_did.as_deref()
    }

    pub(crate) fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub(crate) fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    pub(crate) fn accepted_header_doc_id(&self) -> Option<&str> {
        self.accepted_header_doc_id.as_deref()
    }

    pub(crate) fn accepted_arguments(&self) -> Option<&gents_protocol::output::PayloadRef> {
        self.arguments.as_ref()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state == ToolCallState::Running
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state == ToolCallState::Cancelled
    }

    #[cfg(test)]
    pub(crate) fn set_state(&mut self, state: ToolCallState) {
        self.state = state;
    }

    #[cfg(test)]
    pub(crate) fn set_started_at(&mut self, t: Option<chrono::DateTime<chrono::Utc>>) {
        self.started_at = t;
    }

    pub(crate) fn set_unclaimed_deadline_at(
        &mut self,
        deadline_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        self.unclaimed_deadline_at = deadline_at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_persisted_vocabulary() {
        for state in ToolCallState::ALL {
            assert_eq!(ToolCallState::from_persisted(state.as_str()), Some(state));
        }
        assert_eq!(ToolCallState::from_persisted("called"), None);
        assert_eq!(ToolCallState::from_persisted("unknown"), None);
    }

    #[test]
    fn failure_class_round_trip_persisted_vocabulary() {
        for fc in FailureClass::ALL {
            assert_eq!(FailureClass::from_persisted(fc.as_str()), Some(fc));
        }
        assert_eq!(FailureClass::from_persisted("unknown"), None);
    }

    #[tokio::test]
    async fn constructors_preserve_bridge_classification_and_terminal_status() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        let deadline = chrono::Utc::now() + chrono::Duration::minutes(1);
        let check = |tool: &ToolCallLifecycle, subagent, background, plain: &str, reason: &str| {
            assert_eq!(tool.is_subagent_bridge(), subagent);
            assert_eq!(tool.is_background_tool_bridge(), background);
            assert_eq!(tool.is_bridge(), subagent || background);
            assert_eq!(tool.terminal_persistence_status(None), plain);
            assert_eq!(
                tool.terminal_persistence_status(Some("tool_failed")),
                reason
            );
        };
        let mut native = ToolCallLifecycle::new(
            node.clone(),
            "request".into(),
            "session".into(),
            "did:test:owner".into(),
            "native".into(),
            0,
            "tool".into(),
            "{}".into(),
            deadline,
        );
        check(&native, false, false, "completed", "completed");
        for (input, expected) in [
            (Some("  did:test:requester  "), Some("did:test:requester")),
            (Some(""), None),
            (Some("  "), None),
            (None, None),
        ] {
            native = native.with_requester_did(input.map(str::to_string));
            assert_eq!(native.requester_did.as_deref(), expected);
        }
        let background = ToolCallLifecycle::new_background_tool(
            node.clone(),
            "request".into(),
            "session".into(),
            "did:test:owner".into(),
            "background".into(),
            0,
            "tool".into(),
            "{}".into(),
            deadline,
        );
        // Recovery selects completionPending rows to redrive native-tool effects.
        check(
            &background,
            false,
            true,
            "completionPending",
            "completionPending:tool_failed",
        );
        for mode in [AwaitMode::Foreground, AwaitMode::Background] {
            let subagent = ToolCallLifecycle::new_subagent(
                node.clone(),
                "request".into(),
                "session".into(),
                "did:test:owner".into(),
                "subagent".into(),
                0,
                "spawn_agent".into(),
                "{}".into(),
                deadline,
                mode,
                CancelPolicy::Cascade,
                "child".into(),
                "did:test:target".into(),
            );
            check(&subagent, true, false, "completed", "completed");
        }
        node.shutdown().await;
    }

    use crate::lean_vocab_test::{
        assert_lean_contract_vocabulary_matches, assert_state_machine_contract_is_complete,
        lean_state_machine_contract, LeanContractVocabulary,
    };

    #[test]
    fn rust_tool_call_state_vocabulary_matches_lean_model() {
        let rust_states = ToolCallState::ALL
            .iter()
            .copied()
            .map(ToolCallState::as_str)
            .collect::<Vec<_>>();
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "ToolCallState",
            rust_source: "ToolCallState::ALL",
            rust_values: &rust_states,
        });
    }

    #[test]
    fn rust_cancel_cause_vocabulary_matches_lean_model() {
        let rust_causes = CancelCause::ALL
            .iter()
            .copied()
            .map(CancelCause::as_str)
            .collect::<Vec<_>>();
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "CancelCause",
            rust_source: "CancelCause::ALL",
            rust_values: &rust_causes,
        });
    }

    #[test]
    fn rust_failure_class_vocabulary_matches_lean_model() {
        let rust_classes = FailureClass::ALL
            .iter()
            .copied()
            .map(FailureClass::as_str)
            .collect::<Vec<_>>();
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "ToolFailureClass",
            rust_source: "FailureClass::ALL",
            rust_values: &rust_classes,
        });
    }

    #[test]
    fn tool_call_state_machine_contract_is_complete() {
        assert_state_machine_contract_is_complete("ToolCall");
    }

    #[test]
    fn tool_call_terminal_partition_matches_lean_contract() {
        let machine = lean_state_machine_contract("ToolCall");
        let terminal = ToolCallState::ALL
            .iter()
            .copied()
            .filter(|s| s.is_terminal())
            .map(ToolCallState::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            terminal,
            machine
                .terminal_states
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod subagent_vocabulary {
    use super::*;

    #[test]
    fn await_mode_round_trip_via_persisted_vocab() {
        for &mode in AwaitMode::ALL {
            assert_eq!(AwaitMode::from_persisted(mode.as_str()), Some(mode));
        }
        assert_eq!(AwaitMode::from_persisted("unknown"), None);
    }

    #[test]
    fn cancel_policy_round_trip_via_persisted_vocab() {
        for &policy in CancelPolicy::ALL {
            assert_eq!(CancelPolicy::from_persisted(policy.as_str()), Some(policy));
        }
        assert_eq!(CancelPolicy::from_persisted("unknown"), None);
    }

    #[test]
    fn cancel_cause_round_trip_via_persisted_vocab() {
        for &cause in CancelCause::ALL {
            assert_eq!(CancelCause::from_persisted(cause.as_str()), Some(cause));
        }
        assert_eq!(CancelCause::from_persisted("unknown"), None);
    }

    #[test]
    fn child_terminal_projection_partition() {
        // .interrupted → .cancelled; everything else → .failed
        assert_eq!(
            ChildTerminal::Failed {
                reason: "x".to_string(),
                failure_class: FailureClass::External
            }
            .projected_state(),
            ToolCallState::Failed
        );
        assert_eq!(ChildTerminal::Dead.projected_state(), ToolCallState::Failed);
        assert_eq!(
            ChildTerminal::Interrupted.projected_state(),
            ToolCallState::Cancelled
        );
        assert_eq!(
            ChildTerminal::Superseded.projected_state(),
            ToolCallState::Failed
        );
    }
}
