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
}
