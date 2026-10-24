use defra_p2p_adapter::P2PError;

use super::trait_def::RemoteP2pAdminError;

#[derive(Debug, thiserror::Error)]
pub enum P2pReconcileError {
    #[error("unknown pairing direction {direction:?}")]
    UnknownPairingDirection { direction: String },
    #[error("client pairing id must not be blank")]
    BlankClientPairingId,
    #[error("client pairing id has a blank directory identity")]
    BlankClientPairingDirectory,
    #[error("client pairing id has no recognized direction suffix")]
    UnrecognizedClientPairingDirection,
    #[error("pairing address is not a dialable Iroh endpoint")]
    PairingAddressNotDialable(#[source] p2p::Error),
    #[error("pairing address does not contain a valid Iroh transport peer id")]
    InvalidPairingPeerId,
    #[error("pairing address has no dialable Iroh transport address")]
    NoDialablePairingAddress,
    #[error("client route directory id, requester DID, and owner DID must not be blank")]
    BlankClientRouteIdentity,
    #[error("unknown P2P collection profile {id:?}")]
    UnknownCollectionProfile { id: String },
    #[error("provide at least one --collection or --profile")]
    NoCollectionsOrProfiles,
    #[error("offered scope template must not be blank")]
    BlankOfferedScopeTemplate,
    #[error("unknown offered scope template {id:?}")]
    UnknownOfferedScopeTemplate { id: String },
    #[error("pairing row is missing its scope template")]
    PairingRowMissingScopeTemplate,
    #[error("unknown pairing scope template {template_id:?}")]
    UnknownPairingScopeTemplate { template_id: String },
    #[error(
        "pairing row for peer-DID-dependent template {template_id:?} has a blank \
             node_did; refusing to install an unscoped replicator (skipping peer)"
    )]
    PeerDidDependentTemplateBlank { template_id: String },
    #[error("client pairing row is missing its durable route key")]
    ClientPairingRowMissingRouteKey,
    #[error("data-plane pairing row is missing its scope template")]
    DataPlanePairingRowMissingScopeTemplate,
    #[error("unknown data-plane pairing scope template {template_id:?}")]
    UnknownDataPlaneScopeTemplate { template_id: String },
    #[error("client data-plane pairing row is missing its durable route key")]
    ClientDataPlanePairingRowMissingRouteKey,
    #[error("DataPlanePairingDesired for peer {peer_id} scopes node_did {row_did} but this node is {self_did}; \
             refusing to install a data-plane replicator for a foreign DID")]
    DataPlaneForeignDid {
        peer_id: String,
        row_did: String,
        self_did: String,
    },
    #[error(
        "DataPlanePairingDesired for peer {peer_id} uses template {template_id:?} but the signed \
             PeerEndpoint has a blank node_did"
    )]
    DataPlaneBlankPeerDid {
        peer_id: String,
        template_id: String,
    },
    #[error("list replicators for owned route teardown")]
    ListReplicatorsForTeardown { source: RemoteP2pAdminError },
    #[error("teardown owned client replicator {id}")]
    TeardownOwnedClientReplicator {
        id: String,
        source: RemoteP2pAdminError,
    },
    #[error("verify owned route teardown")]
    VerifyOwnedRouteTeardown { source: RemoteP2pAdminError },
    #[error("owned client replicator at transport endpoint {peer_id} survived teardown")]
    OwnedReplicatorSurvivedTeardown { peer_id: String },
    #[error("list remote P2P collections")]
    ListRemoteP2pCollections { source: RemoteP2pAdminError },
    #[error("resolve collection name for id {id}")]
    ResolveCollectionNameForId {
        id: String,
        source: RemoteP2pAdminError,
    },
    #[error("list remote P2P replicators")]
    ListRemoteP2pReplicators { source: RemoteP2pAdminError },
    #[error("resolve replicator filter collection {collection_id}")]
    ResolveReplicatorFilterCollection {
        collection_id: String,
        source: RemoteP2pAdminError,
    },
    #[error("resolve replicator collection name for id {id}")]
    ResolveReplicatorCollectionNameForId {
        id: String,
        source: RemoteP2pAdminError,
    },
    #[error("install P2P collection {collection}")]
    InstallP2pCollection {
        collection: String,
        source: RemoteP2pAdminError,
    },
    #[error("teardown P2P collection {collection}")]
    TeardownP2pCollection {
        collection: String,
        source: RemoteP2pAdminError,
    },
    #[error("install P2P replicator {address}")]
    InstallP2pReplicator {
        address: String,
        source: RemoteP2pAdminError,
    },
    #[error("teardown P2P replicator {address}")]
    TeardownP2pReplicator {
        address: String,
        source: RemoteP2pAdminError,
    },
    #[error("remove P2P replicator {address} for reconnect replay")]
    RemoveP2pReplicatorForReconnectReplay {
        address: String,
        source: RemoteP2pAdminError,
    },
    #[error("reinstall P2P replicator {address} for reconnect replay")]
    ReinstallP2pReplicatorForReconnectReplay {
        address: String,
        source: RemoteP2pAdminError,
    },
    #[error("embedded node has no P2P transport for hydration delivery")]
    NoHydrationP2pTransport,
    #[error("push hydration documents to peer {peer_id} timed out")]
    PushHydrationDocumentsTimedOut {
        peer_id: String,
        source: tokio::time::error::Elapsed,
    },
    #[error("push hydration documents to peer {peer_id}")]
    PushHydrationDocuments { peer_id: String, source: P2PError },
    #[error("authenticated server DID does not match the signed enrollment admin")]
    AuthenticatedServerDidMismatch,
    #[error("parsing offer issued_at")]
    ParseOfferIssuedAt { source: chrono::ParseError },
    #[error("parsing offer expires_at")]
    ParseOfferExpiresAt { source: chrono::ParseError },
    #[error("enrollment offer is from the future")]
    OfferFromFuture,
    #[error("enrollment offer has expired")]
    OfferExpired,
    #[error("enrollment offer expires before issuance")]
    OfferExpiresBeforeIssuance,
    #[error("enrollment offer validity window is too long")]
    OfferValidityTooLong,
    #[error("timed out reading local enrollment ticket")]
    ReadLocalEnrollmentTicketTimedOut { source: tokio::time::error::Elapsed },
    #[error("enrollment candidate has no shareable P2P address")]
    ShareableP2pAddressUnreadable { source: P2PError },
    #[error("enrollment candidate has no shareable P2P address")]
    NoShareableP2pAddress,
    #[error("enrollment candidate produced an invalid shareable Iroh ticket")]
    InvalidShareableIrohTicket { source: p2p::Error },
    #[error("enrollment candidate shareable ticket does not match its local peer ID")]
    ShareableTicketPeerMismatch,
    #[error("installing authenticated enrollment bootstrap route")]
    InstallBootstrapRoute { source: P2PError },
    #[error("timed out installing authenticated enrollment bootstrap route")]
    InstallBootstrapRouteTimedOut,
    #[error("pushing enrollment request to server")]
    PushEnrollmentRequest { source: P2PError },
    #[error("timed out pushing enrollment request to server")]
    PushEnrollmentRequestTimedOut,
    #[error("removing authenticated enrollment bootstrap route")]
    RemoveBootstrapRoute { source: P2PError },
    #[error("timed out removing authenticated enrollment bootstrap route")]
    RemoveBootstrapRouteTimedOut,
    #[error("enrollment bootstrap route cleanup also failed: {cleanup}")]
    BootstrapRouteCleanupAlsoFailed {
        #[source]
        operation: Box<P2pReconcileError>,
        cleanup: String,
    },
    #[error("enrollment request {request_id} is conflicted: {reason}")]
    EnrollmentRequestConflicted { request_id: String, reason: String },
    #[error("no fresh pending enrollment request {request_id}")]
    NoFreshPendingEnrollmentRequest { request_id: String },
    #[error("unattributable {row_kind} has no {field}")]
    UnattributableRowMissingField { row_kind: String, field: String },
    #[error(transparent)]
    ParseRfc3339(#[from] chrono::ParseError),
    #[error("request {request_id} has {rows} conflicting rows")]
    ConflictingRequestRows { request_id: String, rows: usize },
    #[error("request {request_id} is missing")]
    RequestMissing { request_id: String },
    #[error("request {request_id} has no unique approval ({terminal_rows} terminal rows)")]
    NoUniqueApproval {
        request_id: String,
        terminal_rows: usize,
    },
    #[error("request {request_id} has no approval")]
    NoApproval { request_id: String },
    #[error("unknown enrollment decision {decision:?}")]
    UnknownEnrollmentDecision { decision: String },
    #[error("negative enrollment authorization sequence")]
    NegativeEnrollmentAuthorizationSequence { source: std::num::TryFromIntError },
    #[error("negative route receipt authorization sequence")]
    NegativeRouteReceiptAuthorizationSequence { source: std::num::TryFromIntError },
    #[error("unknown enrollment route receipt direction {direction:?}")]
    UnknownRouteReceiptDirection { direction: String },
    #[error("negative authorization sequence")]
    NegativeAuthorizationSequence { source: std::num::TryFromIntError },
    #[error("unknown authorization revision kind {kind:?}")]
    UnknownAuthorizationRevisionKind { kind: String },
    #[error("invalid enrollment protocol version")]
    InvalidEnrollmentProtocolVersion { source: std::num::TryFromIntError },
    #[error("unsupported enrollment protocol version {version}")]
    UnsupportedEnrollmentProtocolVersion { version: u8 },
    #[error("decoding {field}")]
    DecodeSignature {
        field: String,
        source: bs58::decode::Error,
    },
    #[error("{field} must contain a 64-byte signature")]
    SignatureLength { field: String },
    #[error("enrollment authority has not completed its first projection")]
    EnrollmentAuthorityPending,
    #[error("enrollment authority projection failed: {error}")]
    EnrollmentAuthorityProjectionFailed { error: String },
    #[error("enrollment root authority is conflicted")]
    EnrollmentRootAuthorityConflicted,
    #[error("enrollment authority has multiple active generations for member")]
    MultipleActiveGenerationsForMember,
    #[error("enrollment authority has multiple active generations for member DID")]
    MultipleActiveGenerationsForMemberDid,
    #[error("enrollment authority has multiple active generations for transport peer")]
    MultipleActiveGenerationsForTransportPeer,
    #[error(transparent)]
    SessionHydration(#[from] gents_protocol::session_hydration::SessionHydrationError),
    #[error("file-image receipt crossed demand scope")]
    FileImageReceiptScope,
    #[error(transparent)]
    FileImageReceipt(#[from] gents_protocol::file_image_hydration::FileImageHydrationError),
}

pub(crate) type Result<T, E = P2pReconcileError> = std::result::Result<T, E>;
