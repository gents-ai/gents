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

pub use collection_resolver::CollectionResolver;
pub use core::{
    ClientCore, ClientCoreOptions, ClientPeerStatus, ClientRouteStatus, ClientSyncStateSnapshot,
    EnrollmentRequestResult, P2PHealth, P2PHealthStatus, PairingCollectionStatus,
    STUCK_THRESHOLD_ATTEMPTS,
};
pub use mutations::{PeerMutationResult, SubmitRequestOptions, SubmittedRequest};
pub use observe::{
    ObservedStore, ObserverHandle, ObserverMetricsSnapshot, StoreProjectionRevision,
    StoreUpdateNotice,
};
pub use paths::DesktopPaths;
#[cfg(test)]
pub(crate) use peer_directory::load_peer_directory_snapshot;
pub(crate) use peer_directory::PeerDirectory;
pub use peer_directory::{initialize_local_standard_peer, load_peer_records, PeerRecord};
pub use principal_identity::PrincipalIdentity;
pub use query::{
    fetch_doc_patch, load_agent_scoped_snapshot, load_session_context_store,
    load_session_diagnostics_store, load_session_transcript_page, SessionTranscriptQueryPage,
    DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE, MAX_SESSION_TRANSCRIPT_PAGE_SIZE,
};
pub use store::{ClientStore, ClientStoreRows, TaskRecentRuns, TranscriptView};
pub use sync_projection::{project_sync_health, SyncHealth, SyncHealthState};
