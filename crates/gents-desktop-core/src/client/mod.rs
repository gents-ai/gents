pub mod canonical_output;
mod collection_resolver;
mod core;
mod mutations;
mod observe;
mod paths;
mod peer_directory;
mod principal_identity;
mod query;
mod schema;
mod store;
mod sync_projection;

pub(crate) use query::load_inference_configuration;

pub use collection_resolver::CollectionResolver;
pub use core::{
    tool_file_changes_on, ChatRouteRefusal, ClientCore, ClientCoreOptions,
    ClientObservationAttachment, ClientPeerStatus, ClientRouteStatus, ClientStoreKeyProvider,
    ClientSyncStateSnapshot, EnrollmentActivationError, EnrollmentRequestResult,
    GoalSubmissionReference, InferenceActivity, InferenceObservation, InferenceTarget,
    LocalHomeAccessError, P2PHealth, P2PHealthStatus, PairingCollectionStatus,
    RefreshedRequestProjection, RetainedGoalSubmission, StoreRefresh, STUCK_THRESHOLD_ATTEMPTS,
};

pub use mutations::{apply_config_components_on, patch_config_components_on, set_default_agent_on};
pub use mutations::{
    delete_event_source_on, delete_schedule_on, delete_skill_on, delete_tool_service_registry_on,
    delete_trigger_on, resolve_schedule_now_on, upsert_event_source_on, upsert_node_on,
    upsert_schedule_on, upsert_skill_on, upsert_tool_service_registry_on, upsert_trigger_on,
    ManualTaskInvocation,
};
pub use mutations::{interrupt_request_on, RequestInterrupt};
pub use mutations::{ConfigEditorDocument, ConfigEditorRevision};
pub use mutations::{PeerMutationResult, SubmitRequestOptions, SubmittedRequest};
pub use observe::{
    ObservedStore, ObserverHandle, ObserverMetricsSnapshot, RuntimeConnectionStatus,
    StoreProjectionRevision, StoreUpdateNotice,
};
pub use paths::{DesktopPaths, DESKTOP_HOME_ENV};
pub(crate) use peer_directory::PeerDirectory;
pub use peer_directory::{
    initialize_local_standard_peer, load_peer_records, ChatNextAction, ChatRouteCause, EndedAccess,
    EndedAccessReason, PeerRecord,
};
pub use principal_identity::PrincipalIdentity;
pub(crate) use query::load_request_documents_on;
pub use query::{
    load_node_scoped_snapshot, load_node_snapshot_on, load_request_prompt_ownership,
    load_request_prompt_ownership_on, load_session_compaction_entries,
    load_session_compaction_entries_on, load_session_context_store, load_session_context_store_on,
    load_session_diagnostics_store, load_session_diagnostics_store_on, load_session_live_store,
    load_session_live_store_on, load_session_live_text_on, load_session_request_live_store_on,
    load_session_tip_store, load_session_tip_store_on, load_session_transcript_page,
    load_session_transcript_page_on, load_session_transcript_window,
    load_session_transcript_window_on, CanonicalTranscriptDependencies, RequestPromptFact,
    RequestPromptOwnership, SessionTranscriptQueryPage, SnapshotReadError, TranscriptSpan,
    TranscriptWindow, DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE, MAX_SESSION_TRANSCRIPT_PAGE_SIZE,
};
pub use schema::subscribed_collection_names;
pub use store::{
    folded_requests, queued_behind_turn, request_answer_owner, session_display_request,
    session_order_key, session_summary_entries, session_turn_request, source_matches_node,
    ClientStore, ClientStoreRows, SessionOrderKey, SessionSummaryEntry, TaskRecentRuns,
    TranscriptView,
};
pub use sync_projection::{
    project_sync_health, SyncHealth, SyncHealthCause, SyncHealthState, SyncNextAction,
};
