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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InterruptDisposition {
    Cancel,
    Retain,
}

impl InterruptDisposition {
    pub fn of(state: ToolCallState, await_mode: AwaitMode) -> Self {
        match (state, await_mode) {
            (ToolCallState::Pending, _) => Self::Cancel,
            (ToolCallState::Running, AwaitMode::Foreground) => Self::Cancel,
            _ => Self::Retain,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Retain => "retain",
        }
    }
}

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

use gents_protocol::error_chain::ErrorChain;
use std::sync::Arc;

use defra_node::EmbeddedNode;

pub(crate) mod delivery;
pub(crate) use delivery::publish_background_receipt_in_txn;
mod file_review_access;
pub(crate) mod query;
pub use file_review_access::VerifiedFileImageReceipt;
pub use query::{
    load_tool_call_arguments, load_tool_call_file_changes, load_tool_call_presentation,
    load_tool_call_result, load_tool_file_change_observation, render_tool_result,
    CanonicalToolCallPresentation, ToolFileChangeRead,
};
pub(crate) mod plugin_receipt;
pub use plugin_receipt::ReceiptDecision;
mod recovery;
pub(crate) use recovery::ToolCallRecoveryError;
pub(crate) mod runtime;
mod transition;

pub use gents_loop::tool_call_lifecycle::{FailureClass, ToolOutcome};
#[cfg(test)]
pub(crate) mod admission_fixture;
#[cfg(test)]
mod background_conformance;
#[cfg(test)]
mod background_hook_conformance;
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
#[cfg(test)]
mod session_activity_conformance;
#[cfg(test)]
mod source_read_conformance;

mod plugin_effect;
pub(crate) use crate::streaming::AcceptedToolCall;
pub use recovery::{
    deadline_at_is_expired, deadline_is_expired, BackgroundCompletionSideEffectReport,
    OrphanedBackgroundToolReport, TerminalParentToolReport, ToolCallRecoveryReport,
};
pub use transition::CausedRequestTerminal;
pub use transition::IllegalToolCallTransition;
pub use transition::TransitionError;
pub(crate) use transition::{PluginJobFinish, PluginJobTerminal};

#[derive(Debug, thiserror::Error)]
pub enum ToolCallLifecycleError {
    #[error("invalid plugin effect provenance")]
    InvalidPluginEffectProvenance,
    #[error("dispatch await mode conflicts with the accepted background publication")]
    DispatchAwaitModeConflict,
    #[error("plugin effect lacks generation")]
    PluginEffectLacksGeneration,
    #[error("plugin effects require a running direct parent")]
    PluginEffectRequiresRunningDirectParent,
    #[error("plugin effect exceeds its invocation bounds")]
    PluginEffectExceedsInvocationBounds,
    #[error("plugin effect arguments must be a JSON object")]
    PluginEffectArgumentsNotObject {
        #[source]
        source: serde_json::Error,
    },
    #[error("plugin parent lacks accepted header")]
    PluginParentLacksAcceptedHeader,
    #[error("plugin parent lacks generation")]
    PluginParentLacksGeneration,
    #[error("plugin effect lookup omitted rows")]
    PluginEffectLookupOmittedRows,
    #[error("plugin effect has conflicting physical identities")]
    PluginEffectConflictingIdentities,
    #[error("plugin effect lacks physical identity")]
    PluginEffectLacksPhysicalIdentity,
    #[error("plugin effect replay changed immutable admission")]
    PluginEffectReplayChangedAdmission,
    #[error("plugin effect replay changed arguments")]
    PluginEffectReplayChangedArguments,
    #[error("admitted plugin effect disappeared")]
    PluginEffectAdmittedDisappeared,
    #[error("plugin effect lacks provenance")]
    PluginEffectLacksProvenance,
    #[error("plugin effect lacks request binding")]
    PluginEffectLacksRequestBinding,
    #[error("plugin effect lacks parent header")]
    PluginEffectLacksParentHeader,
    #[error("plugin effect is no longer pending")]
    PluginEffectNoLongerPending,
    #[error("plugin parent lookup omitted rows")]
    PluginParentLookupOmittedRows,
    #[error("plugin parent is missing or ambiguous")]
    PluginParentMissingOrAmbiguous,
    #[error("plugin parent lacks deadline")]
    PluginParentLacksDeadline,
    #[error("plugin effect deadline exceeds parent or has expired")]
    PluginEffectDeadlineExceedsParent,
    #[error("plugin effect has no exact accepted direct parent")]
    PluginEffectNoExactAcceptedParent,
    #[error("plugin request lookup omitted rows")]
    PluginRequestLookupOmittedRows,
    #[error("plugin request is missing or ambiguous")]
    PluginRequestMissingOrAmbiguous,
    #[error("plugin effect lost live request authority")]
    PluginEffectLostRequestAuthority,
    #[error("plugin request lacks logical identity")]
    PluginRequestLacksLogicalIdentity,
    #[error("plugin arguments lookup omitted segments")]
    PluginArgumentsLookupOmittedSegments,
    #[error("plugin effect requires one immutable arguments source")]
    PluginEffectArgumentsSourceNotSingle,
    #[error("invalid plugin arguments closure")]
    PluginEffectInvalidArgumentsClosure,
    #[error("plugin argument declaration changed tool identity")]
    PluginEffectArgumentDeclarationChanged,
    #[error("plugin arguments exceed their payload bound")]
    PluginEffectArgumentsExceedBound,
    #[error(transparent)]
    CanonicalRows(#[from] crate::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    Reconstruction(#[from] gents_protocol::output::ReconstructionError),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    SessionOutput(#[from] crate::session::SessionOutputError),
    #[error(transparent)]
    Timestamp(#[from] chrono::ParseError),
    #[error(transparent)]
    SegmentBytes(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Query(Box<query::ToolCallQueryError>),
    #[error(transparent)]
    Delivery(Box<delivery::DeliveryError>),
}

impl From<delivery::DeliveryError> for ToolCallLifecycleError {
    fn from(error: delivery::DeliveryError) -> Self {
        Self::Delivery(Box::new(error))
    }
}

impl ToolCallLifecycleError {
    pub(crate) fn reconstruction(&self) -> Option<&gents_protocol::output::ReconstructionError> {
        match self {
            Self::Reconstruction(error) => Some(error),
            Self::SessionOutput(error) => error.reconstruction(),
            Self::Delivery(error) => error.reconstruction(),
            Self::Query(error) => error.reconstruction(),
            _ => None,
        }
    }
}

impl From<query::ToolCallQueryError> for ToolCallLifecycleError {
    fn from(error: query::ToolCallQueryError) -> Self {
        Self::Query(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for ToolCallLifecycleError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Graphql(error) => error.config_client_error(),
            Self::SessionOutput(error) => error.config_client_error(),
            Self::Query(error) => error.config_client_error(),
            Self::Delivery(error) => error.config_client_error(),
            _ => None,
        }
    }
}

/// Which owner terminates a tool-call row. A [`Self::SessionMessage`] row
/// (Lean `ToolOperation.sessionMessage`) has no host process: its terminal
/// is the terminal output of the request it caused. A [`Self::PluginJob`] row
/// (Lean `ToolOperation.pluginJob`) is the row whose `job_pin` is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRowKind {
    Native,
    /// An `agent_new`/`agent_message` row. It has no deadline (Lean
    /// `session_message_row_left_running`): the stored `deadline_at` is the
    /// accepted dispatch deadline, never a terminal cause, so the caused
    /// request's result always reaches the calling session.
    SessionMessage,
    /// A plugin job: a background row pinned by `job_pin`. The restart sweep
    /// leaves it running and it never returns to the foreground.
    PluginJob,
}

impl ToolRowKind {
    pub(crate) fn of(tool_name: &str) -> Self {
        if crate::toolset::is_session_message_tool(tool_name) {
            Self::SessionMessage
        } else {
            Self::Native
        }
    }

    /// The kind of a stored row: pluginJob exactly when `job_pin` is present.
    pub(crate) fn of_row(tool_name: &str, has_job_pin: bool) -> Self {
        if has_job_pin {
            Self::PluginJob
        } else {
            Self::of(tool_name)
        }
    }

    /// Whether the orphan sweep may settle a row of this kind and await mode.
    pub fn in_orphan_sweep_scope(self, await_mode: AwaitMode) -> bool {
        await_mode == AwaitMode::Background && self == Self::Native
    }

    /// Whether a running background row of this kind may flip to foreground.
    pub fn may_foreground(self) -> bool {
        !matches!(self, Self::SessionMessage | Self::PluginJob)
    }
}

#[derive(Clone)]
pub struct ToolCallLifecycle {
    node: Arc<EmbeddedNode>,
    request_id: String,
    request_doc_id: Option<String>,
    session_id: String,
    /// Immutable scope key used by filtered replication.
    node_did: String,
    requester_did: Option<String>,
    tool_call_id: String,
    /// Immutable accepted-header provenance used to pair the native result.
    call_id: Option<String>,
    message_sequence: u32,
    tool_name: String,
    kind: ToolRowKind,
    /// Exact accepted assistant header that introduced this physical call.
    accepted_header_doc_id: Option<String>,
    arguments: Option<gents_protocol::output::PayloadRef>,
    execution_generation: Option<String>,
    plugin_receipt: Option<gents_protocol::plugin::PluginExecutionReceipt>,
    /// Data writes a plugin tool returned beside its output; committed with a
    /// Completed terminal and discarded by any other.
    plugin_writes: Option<delivery::PluginToolWrites>,
    file_changes: Option<crate::toolset::FileChanges>,
    /// Immutable physical provenance for dispatch, recovery, and completion routing.
    spawned_by_tool_call_doc_id: Option<String>,
    plugin_effect: Option<PluginEffectBinding>,
    doc_id: Option<String>,
    deadline_at: chrono::DateTime<chrono::Utc>,
    state: ToolCallState,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    failure_class: Option<FailureClass>,
    cancel_cause: Option<CancelCause>,
    selected_tool_identity: Option<SelectedToolIdentity>,
    pub(crate) await_mode: AwaitMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginEffectBinding {
    pub(crate) parent_tool_call_doc_id: String,
    pub(crate) ordinal: u32,
}

impl PluginEffectBinding {
    pub(crate) fn internal_id(parent_tool_call_doc_id: &str, ordinal: u32) -> String {
        format!("plugin-effect:{parent_tool_call_doc_id}:{ordinal}")
    }

    pub(crate) fn decode(
        parent_tool_call_doc_id: Option<&str>,
        ordinal: Option<u32>,
        spawned: bool,
        tool_call_id: &str,
    ) -> Result<Option<Self>, ToolCallLifecycleError> {
        match (parent_tool_call_doc_id, ordinal) {
            (None, None) => Ok(None),
            (Some(parent), Some(ordinal))
                if !parent.is_empty()
                    && (1..=crate::plugin::tool_calls::MAX_EFFECTS).contains(&ordinal)
                    && !spawned
                    && tool_call_id == Self::internal_id(parent, ordinal) =>
            {
                Ok(Some(Self {
                    parent_tool_call_doc_id: parent.to_owned(),
                    ordinal,
                }))
            }
            _ => Err(ToolCallLifecycleError::InvalidPluginEffectProvenance),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedToolIdentity {
    service_id: String,
    tool_name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SpawnedBackgroundToolAdmission {
    pub(crate) tool_name: String,
    pub(crate) deadline_at: chrono::DateTime<chrono::Utc>,
    /// Remote target identity recorded at creation for later wait-policy lookup.
    pub(crate) selected_tool_identity: Option<(String, String)>,
}

impl ToolCallLifecycle {
    /// Adopt a pending row atomically published with an accepted provider header.
    pub(crate) fn from_accepted(
        node: Arc<EmbeddedNode>,
        node_did: String,
        requester_did: Option<String>,
        accepted: AcceptedToolCall,
        deadline_at: chrono::DateTime<chrono::Utc>,
        await_mode: AwaitMode,
    ) -> Result<Self, ToolCallLifecycleError> {
        if accepted.await_mode != AwaitMode::Foreground && await_mode != AwaitMode::Background {
            return Err(ToolCallLifecycleError::DispatchAwaitModeConflict);
        }
        Ok(Self {
            node,
            request_id: String::new(),
            request_doc_id: Some(accepted.request_doc_id),
            session_id: accepted.session_id,
            node_did,
            requester_did: requester_did.and_then(|did| {
                let did = did.trim();
                (!did.is_empty()).then(|| did.to_owned())
            }),
            tool_call_id: accepted.id,
            call_id: accepted.call_id,
            message_sequence: accepted.message_sequence,
            kind: ToolRowKind::of(&accepted.tool_name),
            tool_name: accepted.tool_name,
            accepted_header_doc_id: Some(accepted.accepted_header_doc_id),
            arguments: Some(accepted.arguments),
            execution_generation: Some(accepted.execution_generation),
            plugin_receipt: None,
            plugin_writes: None,
            file_changes: None,
            spawned_by_tool_call_doc_id: None,
            plugin_effect: None,
            doc_id: Some(accepted.tool_call_doc_id),
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode,
        })
    }

    pub(crate) fn stage_plugin_receipt(
        &mut self,
        receipt: Option<gents_protocol::plugin::PluginExecutionReceipt>,
    ) {
        if receipt.is_some() {
            self.plugin_receipt = receipt;
        }
    }

    pub(crate) fn stage_deferred_writes(
        &mut self,
        writes: Option<gents_loop::live_output::DeferredToolWrites>,
    ) {
        if let Some(writes) = writes.as_ref() {
            if let Some(pending) = writes.downcast_ref::<delivery::PluginToolWrites>() {
                self.plugin_writes = Some(pending.clone());
            }
            if let Some(changes) = writes.downcast_ref::<crate::toolset::FileChanges>() {
                self.file_changes = Some(changes.clone());
            }
        }
    }

    pub fn doc_id(&self) -> Option<&str> {
        self.doc_id.as_deref()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node: Arc<EmbeddedNode>,
        request_id: String,
        session_id: String,
        node_did: String,
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
            node_did,
            requester_did: None,
            tool_call_id,
            call_id: None,
            message_sequence,
            kind: ToolRowKind::of(&tool_name),
            tool_name,
            accepted_header_doc_id: None,
            arguments: None,
            execution_generation: None,
            plugin_receipt: None,
            plugin_writes: None,
            file_changes: None,
            spawned_by_tool_call_doc_id: None,
            plugin_effect: None,
            doc_id: None,
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode: AwaitMode::Foreground,
        }
    }

    pub fn with_requester_did(mut self, requester_did: Option<String>) -> Self {
        self.requester_did = requester_did.and_then(|did| {
            let did = did.trim();
            (!did.is_empty()).then(|| did.to_string())
        });
        self
    }

    pub fn with_request_doc_id(mut self, request_doc_id: Option<String>) -> Self {
        self.request_doc_id = request_doc_id.filter(|doc_id| !doc_id.trim().is_empty());
        self
    }

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

    #[allow(clippy::too_many_arguments)]
    pub fn new_background_tool(
        node: Arc<EmbeddedNode>,
        request_id: String,
        session_id: String,
        node_did: String,
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
            node_did,
            requester_did: None,
            tool_call_id,
            call_id: None,
            message_sequence,
            kind: ToolRowKind::of(&tool_name),
            tool_name,
            accepted_header_doc_id: None,
            arguments: None,
            execution_generation: None,
            plugin_receipt: None,
            plugin_writes: None,
            file_changes: None,
            spawned_by_tool_call_doc_id: None,
            plugin_effect: None,
            doc_id: None,
            deadline_at,
            state: ToolCallState::Pending,
            started_at: None,
            failure_class: None,
            cancel_cause: None,
            selected_tool_identity: None,
            await_mode: AwaitMode::Background,
        }
    }

    /// Deadline expiry is inclusive; fresh output admission uses `now > deadline`.
    pub(crate) fn is_deadline_expired(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.deadline_at <= now
    }

    /// The owner that terminates this row; see [`ToolRowKind`].
    pub(crate) fn kind(&self) -> ToolRowKind {
        self.kind
    }

    pub(crate) fn interrupt_disposition(&self) -> InterruptDisposition {
        InterruptDisposition::of(self.state, self.await_mode)
    }

    pub(crate) fn is_background_tool_bridge(&self) -> bool {
        self.await_mode == AwaitMode::Background
            && self.spawned_by_tool_call_doc_id.is_none()
            && self.plugin_effect.is_none()
    }

    pub(crate) fn is_spawned_background(&self) -> bool {
        self.spawned_by_tool_call_doc_id.is_some()
    }

    pub(crate) fn terminal_persistence_status(&self, completion_reason: Option<&str>) -> String {
        if (self.is_background_tool_bridge() || self.is_spawned_background())
            && self.state != ToolCallState::Pending
        {
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

    pub(crate) fn started_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.started_at
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

    pub(crate) fn node_did(&self) -> &str {
        &self.node_did
    }

    pub(crate) fn requester_did(&self) -> Option<&str> {
        self.requester_did.as_deref()
    }

    pub(crate) fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub(crate) async fn output_budget(&self) -> usize {
        let agent_id = match &self.request_doc_id {
            Some(doc_id) => {
                match crate::request_binding::load_agent_request_by_doc_id(&self.node, doc_id).await
                {
                    Ok(request) => request.map(|request| request.agent_id),
                    Err(error) => {
                        tracing::warn!(
                            request_doc_id = %doc_id,
                            tool_call_id = %self.tool_call_id,
                            error = %ErrorChain(&error),
                            "output budget uses the default: owning request did not load"
                        );
                        None
                    }
                }
            }
            None => None,
        };
        match agent_id {
            Some(agent_id) => {
                crate::tool_surface::configured_output_budget(
                    &self.node,
                    &self.node_did,
                    &agent_id,
                    &self.tool_name,
                )
                .await
            }
            None => crate::toolset::DEFAULT_MAX_COMMAND_CHARS,
        }
    }

    pub(crate) fn selected_service_id(&self) -> Option<&str> {
        self.selected_tool_identity
            .as_ref()
            .map(|selected| selected.service_id.as_str())
    }

    pub(crate) fn tool_call_id(&self) -> &str {
        &self.tool_call_id
    }

    #[cfg(test)]
    pub(crate) fn execution_generation(&self) -> Option<&str> {
        self.execution_generation.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn accepted_header_doc_id(&self) -> Option<&str> {
        self.accepted_header_doc_id.as_deref()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tool_row_kind_is_session_message_for_agent_new_and_agent_message() {
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        let deadline = chrono::Utc::now() + chrono::Duration::minutes(1);
        let kind_of = |tool_name: &str| {
            ToolCallLifecycle::new(
                node.clone(),
                "request".into(),
                "session".into(),
                "did:test:owner".into(),
                "call".into(),
                0,
                tool_name.into(),
                "{}".into(),
                deadline,
            )
            .kind()
        };
        assert_eq!(
            kind_of(crate::toolset::AGENT_NEW_TOOL_NAME),
            ToolRowKind::SessionMessage
        );
        assert_eq!(
            kind_of(crate::toolset::AGENT_MESSAGE_TOOL_NAME),
            ToolRowKind::SessionMessage
        );
        assert_eq!(kind_of("tool"), ToolRowKind::Native);
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
