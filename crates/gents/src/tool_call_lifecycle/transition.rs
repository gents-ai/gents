use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::toolset::CommandPolicyDenial;

use super::{AwaitMode, CancelCause, FailureClass, ToolCallLifecycle, ToolCallState, ToolRowKind};

type Result<T, E = TransitionError> = std::result::Result<T, E>;

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum IllegalToolCallTransition {
    #[error(
        "illegal tool call transition: cannot {method} from state {from:?} (allowed: {allowed:?})"
    )]
    BadState {
        method: &'static str,
        from: ToolCallState,
        allowed: Vec<ToolCallState>,
    },
    #[error("await_mode flip rejected: tool already Background")]
    ModeAlreadyBackground,
    #[error("await_mode flip rejected: tool already Foreground")]
    ModeAlreadyForeground,
    #[error("AgentRequest parent linkage incoherent: must set both or neither parent fields")]
    ParentLinkageIncoherent,
    #[error("foreground rejected: an agent_new/agent_message row is background-only")]
    SessionMessageIsBackgroundOnly,
    #[error("foreground rejected: a plugin job row is background-only")]
    PluginJobIsBackgroundOnly,
}

#[derive(Debug, thiserror::Error)]
pub enum TransitionError {
    #[error(transparent)]
    Illegal(#[from] IllegalToolCallTransition),
    #[error("lost compare lifecycle missing physical identity")]
    LostCompareMissingIdentity,
    #[error("{method} compare failed and AgentToolCall row disappeared for session_id={session_id} tool_call_id={tool_call_id}")]
    LostCompareRowDisappeared {
        method: &'static str,
        session_id: String,
        tool_call_id: String,
    },
    #[error("{method} compare failed but AgentToolCall row is still running for session_id={session_id} tool_call_id={tool_call_id}")]
    LostCompareStillRunning {
        method: &'static str,
        session_id: String,
        tool_call_id: String,
    },
    #[error("{method} compare failed but AgentToolCall row is still running in {await_mode:?} for session_id={session_id} tool_call_id={tool_call_id}")]
    LostModeCompareStillRunning {
        method: &'static str,
        await_mode: AwaitMode,
        session_id: String,
        tool_call_id: String,
    },
    #[error("session-message settlement requires a background agent_new/agent_message row")]
    SessionMessageSettlementNotBackground,
    #[error("plugin job settlement requires a row with a job pin")]
    PluginJobSettlementNotPinned,
    #[error("plugin job row lacks physical identity")]
    PluginJobLacksPhysicalIdentity,
    #[error("a paused or awaiting job row has no stored progress to update")]
    PluginJobLacksStoredProgress,
    #[error("dispatch generation differs from accepted physical tool")]
    DispatchGenerationDiffers,
    #[error("start_running cannot re-dispatch an already-running physical tool; recover its registered executor instead")]
    StartRunningAlreadyRunning,
    #[error("spawn_failed lost its pending compare for tool_call_id={tool_call_id}")]
    SpawnFailedLostCompare { tool_call_id: String },
    #[error("tool has no physical row")]
    ToolHasNoPhysicalRow,
    #[error("tool has not started")]
    ToolNotStarted,
    #[error("only a running direct foreground tool can be backgrounded")]
    NotDirectForegroundForBackground,
    #[error("foreground tool changed before background handoff")]
    ForegroundChangedBeforeHandoff,
    #[error("plugin effects cannot detach from their parent invocation")]
    PluginEffectDetach,
    #[error("only a running foreground call can move to the background")]
    NotForegroundForBackground,
    #[error("background called before start_running persisted a row")]
    BackgroundBeforeStart,
    #[error("background called without started_at set")]
    BackgroundWithoutStartedAt,
    #[error("background called without parent request document")]
    BackgroundWithoutRequestDoc,
    #[error("foreground called before start_running persisted a row")]
    ForegroundBeforeStart,
    #[error("foreground called without started_at set")]
    ForegroundWithoutStartedAt,
    #[error(transparent)]
    Query(#[from] super::query::ToolCallQueryError),
    #[error(transparent)]
    Delivery(#[from] super::delivery::DeliveryError),
    #[error(transparent)]
    Lifecycle(#[from] super::ToolCallLifecycleError),
    #[error(transparent)]
    Access(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    DispatchRejected(#[from] super::delivery::ToolDispatchRejection),
    #[error(transparent)]
    Hook(Box<crate::hook::HookError>),
    #[error("foreground mutation")]
    ForegroundMutation(#[source] crate::config_client::ConfigClientError),
    #[error(transparent)]
    JobRecords(#[from] crate::plugin_job::records::JobRecordsError),
}

impl TransitionError {
    #[cfg(test)]
    pub(crate) fn illegal(&self) -> Option<&IllegalToolCallTransition> {
        match self {
            Self::Illegal(error) => Some(error),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn dispatch_rejection(&self) -> Option<&super::delivery::ToolDispatchRejection> {
        match self {
            Self::DispatchRejected(rejection) => Some(rejection),
            Self::Delivery(super::delivery::DeliveryError::DispatchRejected(rejection)) => {
                Some(rejection)
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn background_capacity(
        &self,
    ) -> Option<&crate::hook::persistence::BackgroundCapacityExceeded> {
        match self {
            Self::Delivery(error) => error.background_capacity(),
            Self::Hook(error) => error.background_capacity(),
            _ => None,
        }
    }

    pub(crate) fn reconstruction(&self) -> Option<&gents_protocol::output::ReconstructionError> {
        match self {
            Self::Delivery(error) => error.reconstruction(),
            Self::Lifecycle(error) => error.reconstruction(),
            _ => None,
        }
    }

    pub(crate) fn record_refusal(&self) -> Option<&crate::plugin_job::records::RecordError> {
        match self {
            Self::Delivery(error) => error.record_refusal(),
            Self::JobRecords(error) => error.refusal(),
            _ => None,
        }
    }
}

impl From<crate::hook::HookError> for TransitionError {
    fn from(error: crate::hook::HookError) -> Self {
        Self::Hook(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for TransitionError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Query(error) => error.config_client_error(),
            Self::Delivery(error) => error.config_client_error(),
            Self::Lifecycle(error) => error.config_client_error(),
            Self::Access(error) => Some(error),
            Self::Graphql(error) => error.config_client_error(),
            Self::Hook(error) => error.config_client_error(),
            Self::ForegroundMutation(error) => Some(error),
            Self::JobRecords(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl ToolCallLifecycle {
    async fn sync_after_lost_running_compare(&mut self, method: &'static str) -> Result<()> {
        let current = ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            self.doc_id
                .as_deref()
                .ok_or(TransitionError::LostCompareMissingIdentity)?,
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        )
        .await?
        .ok_or_else(|| TransitionError::LostCompareRowDisappeared {
            method,
            session_id: self.session_id.clone(),
            tool_call_id: self.tool_call_id.clone(),
        })?;

        if current.state == ToolCallState::Running {
            return Err(TransitionError::LostCompareStillRunning {
                method,
                session_id: self.session_id.clone(),
                tool_call_id: self.tool_call_id.clone(),
            });
        }

        self.doc_id = current.doc_id;
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        self.await_mode = current.await_mode;
        self.spawned_by_tool_call_doc_id = current.spawned_by_tool_call_doc_id;
        self.plugin_effect = current.plugin_effect;
        Ok(())
    }

    async fn sync_after_lost_mode_compare(
        &mut self,
        method: &'static str,
        target_mode: AwaitMode,
    ) -> Result<()> {
        let current = ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            self.doc_id
                .as_deref()
                .ok_or(TransitionError::LostCompareMissingIdentity)?,
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        )
        .await?
        .ok_or_else(|| TransitionError::LostCompareRowDisappeared {
            method,
            session_id: self.session_id.clone(),
            tool_call_id: self.tool_call_id.clone(),
        })?;

        if current.state == ToolCallState::Running && current.await_mode != target_mode {
            return Err(TransitionError::LostModeCompareStillRunning {
                method,
                await_mode: current.await_mode,
                session_id: self.session_id.clone(),
                tool_call_id: self.tool_call_id.clone(),
            });
        }

        self.doc_id = current.doc_id;
        self.deadline_at = current.deadline_at;
        self.state = current.state;
        self.started_at = current.started_at;
        self.failure_class = current.failure_class;
        self.cancel_cause = current.cancel_cause;
        self.await_mode = current.await_mode;
        self.spawned_by_tool_call_doc_id = current.spawned_by_tool_call_doc_id;
        self.plugin_effect = current.plugin_effect;
        Ok(())
    }

    pub(crate) fn ensure_state(
        &self,
        allowed: &[ToolCallState],
        method: &'static str,
    ) -> Result<(), TransitionError> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            Err(IllegalToolCallTransition::BadState {
                method,
                from: self.state,
                allowed: allowed.to_vec(),
            }
            .into())
        }
    }
}

mod cancel;
mod session_message;
pub use session_message::CausedRequestTerminal;
mod mode_policy;
mod native;
mod plugin_job;
pub(crate) use plugin_job::PluginJobTerminal;

#[cfg(test)]
mod tests;
