#[derive(Debug, thiserror::Error)]
pub enum TriggerEngineError {
    #[error("task {task_id} is not in the active snapshot (check the task exists, is enabled, and its agent is available)")]
    TaskNotActive { task_id: String },
    #[error("manual trigger channel is closed; engine has shut down")]
    ManualChannelClosed,
    #[error("schedule Task changed before admission")]
    ScheduleTaskChanged,
    #[error("event trigger disappeared")]
    EventTriggerMissing,
    #[error("trigger agent unavailable")]
    TriggerAgentUnavailable,
    #[error("fire lacks source document ID")]
    FireSourceDocMissing,
    #[error("session template rendered an empty ID")]
    SessionTemplateEmpty,
    #[error("trigger session template disagrees with pinned graph session")]
    SessionTemplateMismatch,
    #[error("emit_outcome requires a source handoff_id")]
    OutcomeHandoffMissing,
    #[error(
        "emit_outcome requires document-triggered delivery or explicit CLI/desktop Task admission"
    )]
    EmitOutcomeNeedsDelivery,
    #[error("queued_serial requires an explicit session_id_template for scheduled delivery")]
    QueuedSerialScheduleNeedsSession,
    #[error("queued_serial requires a document event source")]
    QueuedSerialNeedsEventSource,
    #[error("session_id_template requires a document event source")]
    SessionTemplateNeedsEventSource,
    #[error(transparent)]
    Template(#[from] crate::template::TemplateError),
    #[error("source document is missing runtime-filled field `{field}`")]
    CapturedFieldMissing { field: String },
    #[error("runtime-filled source field `{field}` must be a string or integral number")]
    CapturedFieldType { field: String },
    #[error("runtime-filled source field `requester_did` must be non-empty")]
    RequesterDidEmpty,
    #[error("native event source has no {field}")]
    NativeSourceFieldMissing { field: String },
    #[error("introspection returned no fields for {collection}")]
    IntrospectionEmpty { collection: String },
    #[error("event source scan omitted a document ID")]
    ScanDocumentIdMissing,
    #[error("group timeout must be positive")]
    GroupTimeoutNotPositive,
    #[error("callback source is not grouped")]
    CallbackSourceNotGrouped,
    #[error("minimum group count must be positive")]
    MinimumCountNotPositive,
    #[error("minimum group count exceeds bounds")]
    MinimumCountOutOfBounds,
    #[error("group has two expected count sources")]
    TwoExpectedCountSources,
    #[error("group member missing expected-count field")]
    ExpectedCountFieldMissing,
    #[error("expected count must be a canonical positive bounded integer")]
    ExpectedCountNotCanonical,
    #[error("group members disagree on expected count")]
    ExpectedCountConflict,
    #[error("ambiguous durable event group")]
    AmbiguousGroup,
    #[error("group record missing physical ID")]
    GroupRecordMissingId,
    #[error("group ownership/identity mismatch")]
    GroupIdentityMismatch,
    #[error(transparent)]
    IntegerRange(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("goal continuation reconciliation rejected a persisted child")]
    GoalContinuationReconciliationRejectedPersistedChild,
    #[error("goal continuation reconciliation produced an invalid phase")]
    GoalContinuationReconciliationProducedInvalidPhase,
    #[error("active Goal candidate has an unknown status")]
    ActiveGoalCandidateHasUnknownStatus,
    #[error("claimed Goal has an unknown status")]
    ClaimedGoalHasUnknownStatus,
    #[error("usage-limit transition must be legal from active")]
    UsageLimitTransitionMustLegalActive,
    #[error("refreshed Goal row disappeared")]
    RefreshedGoalRowDisappeared,
    #[error("refreshed Goal candidate has an unknown status")]
    RefreshedGoalCandidateHasUnknownStatus,
    #[error("claimed continuation sequence exhausted")]
    ClaimedContinuationSequenceExhausted,
    #[error("decode goal continuation parent AgentRequest")]
    DecodeGoalContinuationParentAgentRequest(#[source] crate::watcher::WatcherError),
    #[error("unavailable-agent transition must be legal for a publishing Goal")]
    UnavailableAgentTransitionMustLegalPublishing,
    #[error(transparent)]
    BlockedTurn(Box<crate::blocked_turn::BlockedTurnError>),
    #[error(transparent)]
    Goal(Box<crate::goal::GoalError>),
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    GraphqlResponse(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    MaterializeDuplicate(#[from] crate::trigger_engine::MaterializeDuplicate),
    #[error(transparent)]
    Materializer(Box<crate::trigger_engine::production_materializer::MaterializerError>),
    #[error(transparent)]
    WorkspaceRuntime(Box<crate::workspace::WorkspaceRuntimeError>),
    #[error(transparent)]
    GraphPipeline(Box<crate::graph_pipeline::GraphPipelineError>),
    #[error(transparent)]
    Lifecycle(Box<crate::lifecycle::LifecycleError>),
    #[error(transparent)]
    Materialize(Box<crate::lifecycle::materialize::MaterializeError>),
    #[error(transparent)]
    Session(Box<crate::session::SessionError>),
    #[error(transparent)]
    GoalSource(#[from] crate::trigger_engine::goal_source::GoalSourceError),
}

impl From<crate::lifecycle::materialize::MaterializeError> for TriggerEngineError {
    fn from(error: crate::lifecycle::materialize::MaterializeError) -> Self {
        Self::Materialize(Box::new(error))
    }
}

impl From<crate::session::SessionError> for TriggerEngineError {
    fn from(error: crate::session::SessionError) -> Self {
        Self::Session(Box::new(error))
    }
}

impl From<crate::lifecycle::LifecycleError> for TriggerEngineError {
    fn from(error: crate::lifecycle::LifecycleError) -> Self {
        Self::Lifecycle(Box::new(error))
    }
}

impl From<crate::workspace::WorkspaceRuntimeError> for TriggerEngineError {
    fn from(error: crate::workspace::WorkspaceRuntimeError) -> Self {
        Self::WorkspaceRuntime(Box::new(error))
    }
}

impl From<crate::trigger_engine::production_materializer::MaterializerError> for TriggerEngineError {
    fn from(error: crate::trigger_engine::production_materializer::MaterializerError) -> Self {
        Self::Materializer(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for TriggerEngineError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::DecodeGoalContinuationParentAgentRequest(error) => error.config_client_error(),
            Self::BlockedTurn(error) => error.config_client_error(),
            Self::Goal(error) => error.config_client_error(),
            Self::ConfigClient(error) => Some(error),
            Self::GraphqlResponse(error) => error.config_client_error(),
            Self::Materializer(error) => error.config_client_error(),
            Self::GraphPipeline(error) => error.config_client_error(),
            Self::Lifecycle(error) => error.config_client_error(),
            Self::Materialize(error) => error.config_client_error(),
            Self::Session(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl From<crate::graph_pipeline::GraphPipelineError> for TriggerEngineError {
    fn from(error: crate::graph_pipeline::GraphPipelineError) -> Self {
        Self::GraphPipeline(Box::new(error))
    }
}

impl From<crate::goal::GoalError> for TriggerEngineError {
    fn from(error: crate::goal::GoalError) -> Self {
        Self::Goal(Box::new(error))
    }
}

impl From<crate::blocked_turn::BlockedTurnError> for TriggerEngineError {
    fn from(error: crate::blocked_turn::BlockedTurnError) -> Self {
        Self::BlockedTurn(Box::new(error))
    }
}
