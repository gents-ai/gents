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
    ClientCore, ClientCoreOptions, ClientObservationAttachment, ClientPeerStatus,
    ClientRouteStatus, ClientStoreKeyProvider, ClientSyncStateSnapshot, EnrollmentRequestResult,
    P2PHealth, P2PHealthStatus, PairingCollectionStatus, RefreshedRequestProjection,
    RuntimeSchemaObservation, STUCK_THRESHOLD_ATTEMPTS,
};
pub use gents::P2pSyncStatusSnapshot;
pub use mutations::{ConfigEditorDocument, ConfigEditorRevision};
pub use mutations::{DeliveryUnconfirmed, SubmissionOutcomeUnknown, UnresolvedSubmission};
pub use mutations::{PeerMutationResult, SubmitRequestOptions, SubmittedRequest};
pub use observe::{
    ObservedStore, ObserverHandle, ObserverMetricsSnapshot, StoreProjectionRevision,
    StoreUpdateNotice,
};
pub use paths::{DesktopPaths, DESKTOP_HOME_ENV};
pub(crate) use peer_directory::PeerDirectory;
pub use peer_directory::{initialize_local_standard_peer, load_peer_records, PeerRecord};
pub use principal_identity::PrincipalIdentity;
pub(crate) use query::load_request_documents_on;
pub use query::{
    fetch_doc_patch, load_node_scoped_snapshot, load_request_prompt_ownership,
    load_request_prompt_ownership_on, load_session_compaction_entries,
    load_session_compaction_entries_on, load_session_context_store, load_session_context_store_on,
    load_session_diagnostics_store, load_session_live_store, load_session_live_store_on,
    load_session_tip_store, load_session_tip_store_on, load_session_transcript_page,
    load_session_transcript_page_on, session_transcript_requester_scope, session_unreadable_reason,
    session_unsendable_reason, CanonicalTranscriptDependencies, RequestPromptFact,
    RequestPromptOwnership, SessionTranscriptQueryPage, DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE,
    MAX_SESSION_TRANSCRIPT_PAGE_SIZE,
};
pub use schema::subscribed_collection_names;
pub use store::{
    folded_requests, queued_behind_turn, request_answer_owner, session_display_request,
    session_turn_request, source_matches_node, ClientStore, ClientStoreRows, TaskRecentRuns,
    TranscriptView,
};
pub use sync_projection::{project_sync_health, SyncHealth, SyncHealthState};
