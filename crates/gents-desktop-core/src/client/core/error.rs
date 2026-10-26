use defra_p2p_adapter::P2PError;
use gents_protocol::peer_schema::ReplicatedSchemaSkew;

/// A native store-key host's failure. Core cannot name the host's error type,
/// so it carries the host's error unchanged; the host classifies its own
/// failure through [`ClientCoreError::cause`].
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ClientStoreKeyError(Box<dyn std::error::Error + Send + Sync>);

impl ClientStoreKeyError {
    pub fn new<E>(error: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self(Box::new(error))
    }

    pub fn host_error(&self) -> &(dyn std::error::Error + 'static) {
        self.0.as_ref()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientCoreError {
    #[error(transparent)]
    PeerDirectory(#[from] crate::client::peer_directory::PeerDirectoryError),
    #[error(transparent)]
    ReplicatedSchemaSkew(#[from] ReplicatedSchemaSkew),
    #[error("runtime status belongs to {}, not {runtime_did}", status_did.as_deref().unwrap_or("an unidentified runtime"))]
    RuntimeStatusWrongNode {
        status_did: Option<String>,
        runtime_did: String,
    },
    #[error("runtime {runtime_did} has not published its replicated schema yet")]
    RuntimeSchemaUnpublished { runtime_did: String },
    #[error("timed out connecting bootstrap peer {label} at {addr}: {error}")]
    ConnectBootstrapPeerTimedOut {
        label: String,
        addr: String,
        error: Box<ClientCoreError>,
    },
    #[error("timed out force-connecting bootstrap peer {label} at {addr}: {error}")]
    ForceConnectBootstrapPeerTimedOut {
        label: String,
        addr: String,
        error: Box<ClientCoreError>,
    },
    #[error("file review lookup omitted demand rows")]
    FileReviewLookupOmittedRows,
    #[error("file review demand has conflicting rows")]
    FileReviewConflictingRows,
    #[error("file review demand {field} differs from the selected tool")]
    FileReviewDemandDiffers { field: String },
    #[error("timed out waiting for peer {peer_id} to disconnect")]
    DisconnectTimedOut { peer_id: String },
    #[error("transport disconnected but replicator cleanup failed for {label} at {addr}: {replicator_error}")]
    ReplicatorCleanupAfterDisconnect {
        label: String,
        addr: String,
        replicator_error: String,
    },
    #[error("replicator removed but transport disconnect failed for {label} at {addr}: {disconnect_error}")]
    DisconnectAfterReplicatorRemoval {
        label: String,
        addr: String,
        disconnect_error: Box<ClientCoreError>,
    },
    #[error("replicator cleanup failed for {label} at {addr}: {replicator_error}; transport disconnect also failed: {disconnect_error}")]
    ReplicatorAndDisconnectFailed {
        label: String,
        addr: String,
        replicator_error: String,
        disconnect_error: Box<ClientCoreError>,
    },
    #[error("timed out waiting for bootstrap peer {peer_id} to connect for {label}")]
    WaitBootstrapPeerTimedOut { peer_id: String, label: String },
    #[error("{name} is missing from the current peer authority")]
    PeerAuthorityFieldMissing { name: String },
    #[error("{field} must not be empty")]
    EmptyRequiredField { field: String },
    #[error("a simulated peer route requires ClientCoreOptions::local_simulated_route()")]
    SimulatedRouteRequiresLocalOptions,
    #[error("desktop P2P supervisor is shutting down")]
    P2pSupervisorShuttingDown,
    #[error("queueing desktop P2P supervisor command")]
    QueueP2pSupervisorCommand {
        #[source]
        source: tokio::sync::mpsc::error::SendError<super::P2PSupervisorCommand>,
    },
    #[error("request {request_id} is ambiguous across node scopes")]
    RequestAmbiguousAcrossNodeScopes { request_id: String },
    #[error("request {request_id} not found")]
    RequestNotFound { request_id: String },
    #[error("invalid enrollment protocol version")]
    InvalidEnrollmentProtocolVersion {
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("unsupported enrollment protocol version {version}")]
    UnsupportedEnrollmentProtocolVersion { version: u8 },
    #[error("decode enrollment {kind} signature")]
    DecodeEnrollmentSignature {
        kind: String,
        #[source]
        source: bs58::decode::Error,
    },
    #[error("invalid enrollment {kind} signature")]
    InvalidEnrollmentSignature { kind: String },
    #[error("unknown enrollment decision {decision:?}")]
    UnknownEnrollmentDecision { decision: String },
    #[error("negative enrollment decision sequence")]
    NegativeEnrollmentDecisionSequence {
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("negative enrollment route receipt sequence")]
    NegativeEnrollmentRouteReceiptSequence {
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("unknown enrollment route receipt direction {direction:?}")]
    UnknownEnrollmentRouteReceiptDirection { direction: String },
    #[error("negative enrollment revision sequence")]
    NegativeEnrollmentRevisionSequence {
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("unknown enrollment revision kind {kind:?}")]
    UnknownEnrollmentRevisionKind { kind: String },
    #[error("{operation}")]
    P2pOperation {
        operation: String,
        #[source]
        source: P2PError,
    },
    #[error("timed out {operation}")]
    P2pOperationTimedOut { operation: String },
    #[error("desktop P2P transport has no dialable shareable address")]
    P2pNoDialableAddress,
    #[error("P2P transport reported an empty peer id")]
    P2pEmptyPeerId,
    #[error("P2P transport reported no listen addresses")]
    P2pNoListenAddresses,
    #[error("too many client observation attachments")]
    TooManyObservationAttachments,
    #[error("pending edit request patch crossed its node, session, or requester scope")]
    PendingEditCrossedScope,
    #[error("accepted edit projection read became stale before publication")]
    AcceptedEditProjectionStale,
    #[error("the selected deployment route is not ready; no request was saved (wait for pairing repair or inspect pairing status)")]
    DeploymentRouteNotReady,
    #[error("no saved deployment route owns node {node_did}; no request was saved")]
    NoDeploymentRoute { node_did: String },
    #[error("no saved deployment route owns this node")]
    NoRouteOwnsNode,
    #[error("request output crossed the selected session scope")]
    RequestOutputCrossedSession,
    #[error("request answer crossed its admitted scope")]
    RequestAnswerCrossedScope,
    #[error("local store access is unavailable")]
    LocalStoreAccessUnavailable,
    #[error("timed out loading timeline for {request_id}")]
    TimelineTimedOut { request_id: String },
    #[error("{detail}")]
    LoadTimeline { detail: String },
    #[error("persisting expired enrollment write fence")]
    PersistExpiredEnrollmentFence {
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("chat target has no current owned peer route")]
    NoOwnedPeerRoute,
    #[error("enrollment authorization sequence is missing")]
    EnrollmentAuthorizationSequenceMissing,
    #[error("chat target is not owned by enrollment or a local standard runtime")]
    ChatTargetNotOwned,
    #[error("local request signer does not own target node")]
    SignerDoesNotOwnNode,
    #[error("message_key is required")]
    MessageKeyRequired,
    #[error("Editing a message requires a direct runtime connection. Reconnect the local runtime and try again.")]
    DirectRuntimeRequiredForEdit,
    #[error("no Skill document with skill_id {skill_id:?} for {node_did}")]
    NoSkillDeleted { skill_id: String, node_did: String },
    #[error("no {collection} document with {field} {id:?}")]
    NoDocumentDeleted {
        collection: &'static str,
        field: &'static str,
        id: String,
    },
    #[error("stale request has no node_did")]
    StaleRequestNoNodeDid,
    #[error("request {request_id} is absent from the selected node")]
    RequestAbsentFromSelectedNode { request_id: String },
    #[error("request {request_id} is ambiguous across the selected node scope")]
    RequestAmbiguousInSelectedNode { request_id: String },
    #[error("request has no node_did")]
    RequestNoNodeDid,
    #[error("retry parent has no node_did")]
    RetryParentNoNodeDid,
    #[error("peer {peer_id} not found")]
    PeerNotFound { peer_id: String },
    #[error("peer {peer_id} changed while it was being renamed")]
    PeerChangedDuringRename { peer_id: String },
    #[error("no runtime record for this node")]
    NoRuntimeRecord,
    #[error("local standard runtime has no operator GraphQL endpoint")]
    LocalStandardNoOperatorEndpoint,
    #[error("managed node {node_did} has no operator GraphQL endpoint; refusing a desktop-replica configuration fallback")]
    ManagedNodeNoOperatorEndpoint { node_did: String },
    #[error("Connect this local runtime before editing its configuration.")]
    ConnectBeforeEditingConfiguration,
    #[error("Connect this local runtime before changing authentication.")]
    ConnectBeforeChangingAuthentication,
    #[error("Connect this runtime before setting up access review.")]
    ConnectBeforeAccessReview,
    #[error("Open Runtime Settings to connect this runtime's configuration before choosing Approve for me.")]
    AccessReviewRequiresConfiguration,
    #[error("Access review setup requires this runtime's operator endpoint.")]
    AccessReviewRequiresOperatorEndpoint,
    #[error("desktop node DID is not ACP-addressable")]
    NodeDidNotAcpAddressable(#[source] identity::Error),
    #[error("{reason}")]
    SessionUnreadable { reason: &'static str },
    #[error("session hydration retry requires a failed attempt for the selected session")]
    HydrationRetryRequiresFailedAttempt,
    #[error("passive hydration admission requires local start evidence")]
    PassiveHydrationRequiresEvidence,
    #[error("explicit hydration retry does not accept passive start evidence")]
    ExplicitHydrationRetryWithEvidence,
    #[error("session hydration request key resolved to multiple rows")]
    HydrationRequestKeyAmbiguous,
    #[error("unknown session hydration request status {status:?}")]
    UnknownHydrationStatus { status: String },
    #[error("session hydration request is missing status")]
    HydrationStatusMissing,
    #[error("initial session hydration request create returned no document")]
    HydrationCreateReturnedNoDocument,
    #[error("session hydration request {field} does not match the selected target")]
    HydrationTargetMismatch { field: &'static str },
    #[error("hydration admission transaction omitted response data")]
    HydrationResponseMissingData,
    #[error("hydration admission transaction omitted {field}")]
    HydrationResponseMissingField { field: String },
    #[error("decode {field} transaction rows")]
    DecodeHydrationRows {
        field: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("session hydration served_doc_count must be non-negative")]
    HydrationServedCountNegative(#[source] std::num::TryFromIntError),
    #[error("terminal hydration request is missing {field}")]
    HydrationFieldMissing { field: &'static str },
    #[error("session hydration served_doc_count does not match signed manifest")]
    HydrationServedCountMismatch,
    #[error("decode session hydration receipt signature")]
    DecodeHydrationSignature(#[source] bs58::decode::Error),
    #[error("session hydration receipt signature is invalid")]
    HydrationSignatureInvalid,
    #[error("runtime cannot sync with this app")]
    RuntimeCannotSync {
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("server does not advertise authenticated status enrollment")]
    StatusEnrollmentNotAdvertised,
    #[error("decoding server enrollment offer")]
    DecodeServerEnrollmentOffer(#[source] gents_protocol::enrollment::EnrollmentError),
    #[error("refusing to enroll with an incompatible runtime")]
    IncompatibleEnrollmentRuntime {
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("enrollment request commit was not observably recovered after: {error}")]
    EnrollmentCommitNotRecovered { error: String },
    #[error("network has no unique durable admin pin")]
    NoUniqueAdminPin,
    #[error("request admin does not match the durable network pin")]
    RequestAdminPinMismatch,
    #[error("decoding persisted enrollment offer")]
    DecodePersistedEnrollmentOffer(#[source] gents_protocol::enrollment::EnrollmentError),
    #[error("validating persisted enrollment request against offer")]
    ValidatePersistedEnrollmentRequest(#[source] gents_protocol::enrollment::EnrollmentError),
    #[error("persisted enrollment offer has an incompatible schema")]
    PersistedOfferIncompatibleSchema,
    #[error("persisted enrollment offer signature is invalid")]
    PersistedOfferSignatureInvalid,
    #[error("persisted enrollment request signature is invalid")]
    PersistedRequestSignatureInvalid,
    #[error("parsing enrollment request expiry")]
    ParseEnrollmentExpiry(#[source] chrono::ParseError),
    #[error("enrollment request has multiple authenticated decisions")]
    MultipleEnrollmentDecisions,
    #[error("no local enrollment request {request_id}")]
    NoLocalEnrollmentRequest { request_id: String },
    #[error("enrollment request {request_id} is not the request persisted for its offer")]
    EnrollmentRequestNotPersistedForOffer { request_id: String },
    #[error("enrollment request {request_id} for offer {offer_id} was removed on this desktop; fetch a fresh offer from the server's /status to enroll again")]
    EnrollmentRequestRetired {
        request_id: String,
        offer_id: String,
    },
    #[error("persisted enrollment request embeds a different signed offer")]
    PersistedRequestOfferDiffers,
    #[error("offer {offer_id} has multiple local enrollment requests")]
    MultipleLocalEnrollmentRequests { offer_id: String },
    #[error("enrolled server has no authenticated transport identity")]
    EnrolledServerUnauthenticated,
    #[error("timed out re-authenticating enrolled server while {stage} after {elapsed_ms} ms")]
    ReauthenticateEnrolledServerTimedOut {
        stage: &'static str,
        elapsed_ms: u128,
        #[source]
        source: tokio::time::error::Elapsed,
    },
    #[error("managed enrollment has no local home")]
    ManagedEnrollmentNoHome,
    #[error("managed runtime has an invalid P2P address: {error}")]
    ManagedRuntimeInvalidP2pAddress { error: p2p::Error },
    #[error("discovered managed runtime identity does not match approved enrollment")]
    ManagedRuntimeIdentityMismatch,
    #[error("enrollment request is not owned by this desktop identity and transport")]
    EnrollmentRequestNotOwned,
    #[error("persisted enrollment offer contains an invalid server ticket")]
    PersistedOfferInvalidTicket(#[source] p2p::Error),
    #[error("persisted server ticket is bound to another transport peer")]
    PersistedTicketOtherPeer,
    #[error("client shutdown owner ended without a result")]
    ShutdownOwnerEnded(#[source] tokio::sync::watch::error::RecvError),
    #[error("starting embedded desktop node")]
    StartEmbeddedNode(#[source] Box<defra_node::Error>),
    #[error("desktop node started without P2P support")]
    NodeWithoutP2p,
    #[error("reading desktop P2P peer id")]
    ReadLocalPeerId {
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("reading desktop P2P listen addresses")]
    ReadListenAddresses {
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("decoding {}", path.display())]
    DecodeStoreEncryption {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("reading {}", path.display())]
    ReadStoreEncryption {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("client encryption metadata does not match its pending upgrade")]
    StoreEncryptionUpgradeMismatch,
    #[error(transparent)]
    StoreKeyProvider(#[from] ClientStoreKeyError),
    #[error(transparent)]
    StoreRecord(#[from] super::bootstrap::ClientStoreRecordError),
    #[error("the local runtime deployment cannot be removed")]
    LocalRuntimeNotRemovable,
    #[error("queueing peer {peer_id} for durable route teardown")]
    QueuePeerRemoval {
        peer_id: String,
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("peer {peer_id} not found while queueing removal")]
    PeerNotFoundWhileQueueing { peer_id: String },
    #[error("enrollment route configuration requires enrollment-owned peer")]
    EnrollmentRouteRequiresEnrollmentPeer,
    #[error("authenticated enrollment is required to configure a remote client route")]
    RemoteRouteRequiresEnrollment,
    #[error("manual pairing desired authority is disabled")]
    ManualPairingDesiredDisabled,
    #[error("enrollment record has no {field}")]
    EnrollmentRecordMissing { field: &'static str },
    #[error("remote GraphQL endpoint is required to teardown the return route")]
    TeardownRequiresGraphql,
    #[error("{failures}")]
    RemovalCleanupFailed { failures: String },
    #[error("cleanup exceeded {seconds}s timeout")]
    CleanupTimedOut { seconds: u64 },
    #[error("pending removal intent changed while route lifecycle owned {peer_id}")]
    RemovalIntentChanged { peer_id: String },
    #[error("manual PeerPairingDesired authoring is disabled")]
    ManualPairingDesiredAuthoringDisabled,
    #[error("file changes crossed the selected request scope")]
    FileChangesCrossedScope,
    #[error("request lacks physical identity")]
    RequestLacksPhysicalIdentity,
    #[error("file review lacks its requester")]
    FileReviewLacksRequester,
    #[error("file review requester differs from the enrolled client")]
    FileReviewRequesterDiffers,
    #[error("file review demand disappeared")]
    FileReviewDemandDisappeared,
    #[error("Runtime rejected file review: {detail}")]
    FileReviewRejected { detail: String },
    #[error("file review observation differs from the signed receipt")]
    FileReviewObservationDiffers,
    #[error("local file-image collection is unavailable")]
    FileImageCollectionUnavailable,
    #[error("Reconnect this runtime to load new file versions.")]
    FileReviewReconnect {
        #[source]
        source: Box<ClientCoreError>,
    },
    #[error("file review observation subscription closed")]
    FileReviewSubscriptionClosed,
    #[error("file review route observation closed")]
    FileReviewRouteObservationClosed(#[source] tokio::sync::watch::error::RecvError),
    #[error("File review is waiting for the runtime. Open the changes again when the connection is ready.")]
    FileReviewWaiting(#[source] tokio::time::error::Elapsed),
    #[error("file review demand omitted its request key")]
    FileReviewDemandOmittedKey,
    #[error("file review receipts selected conflicting images")]
    FileReviewConflictingImages,
    #[error("file review lookup returned a rejected demand")]
    FileReviewLookupRejected,
    #[error("file review receipt omitted {field}")]
    FileReviewReceiptOmitted { field: String },
    #[error("rejected file review selected an image")]
    RejectedFileReviewSelectedImage,
    #[error("file review receipt has an invalid observation identity")]
    FileReviewInvalidObservationIdentity,
    #[error("file review receipt has an unknown status")]
    FileReviewUnknownStatus,
    #[error("The retained request belongs to another session")]
    RetainedRequestOtherSession,
    #[error("The retained request contains different attachments")]
    RetainedRequestDifferentAttachments,
    #[error("Attachment delivery is still unknown; reconnect to recover the original request before retrying")]
    AttachmentDeliveryUnknown,
    #[error("unresolved submission {request_id} belongs to another session")]
    UnresolvedSubmissionOtherSession { request_id: String },
    #[error("unresolved submission {request_id} is a different message")]
    UnresolvedSubmissionDifferentMessage { request_id: String },
    #[error(transparent)]
    ConfigClient(#[from] gents::config_client::ConfigClientError),
    #[error(transparent)]
    GraphqlResponse(#[from] gents::graphql::GraphqlResponseError),
    #[error(transparent)]
    P2pReconcile(Box<gents::agent::p2p_reconcile::error::P2pReconcileError>),
    #[error(transparent)]
    LocalRuntime(#[from] crate::local_runtime::LocalRuntimeError),
    #[error(transparent)]
    Identity(#[from] gents::identity::IdentityError),
    #[error(transparent)]
    ClientQuery(Box<crate::client::query::ClientQueryError>),
    #[error(transparent)]
    Goal(Box<gents::goal::GoalError>),
    #[error(transparent)]
    StoreKeyUpgrade(#[from] gents::store_key::upgrade::UpgradeError),
    #[error(transparent)]
    StoreKey(#[from] gents::store_key::StoreKeyError),
    #[error(transparent)]
    Queue(Box<gents::lifecycle::QueueError>),
    #[error(transparent)]
    SessionHydration(#[from] gents_protocol::session_hydration::SessionHydrationError),
    #[error(transparent)]
    Enrollment(#[from] gents_protocol::enrollment::EnrollmentError),
    #[error(transparent)]
    ToolCallQuery(Box<gents::tool_call_lifecycle::ToolCallQueryError>),
    #[error(transparent)]
    FileReviewAccess(Box<gents::tool_call_lifecycle::FileReviewAccessError>),
    #[error(transparent)]
    Paths(#[from] crate::client::paths::PathsError),
    #[error(transparent)]
    SessionFork(#[from] gents::session::ForkError),
    #[error(transparent)]
    FileImageHydration(#[from] gents_protocol::file_image_hydration::FileImageHydrationError),
    #[error(transparent)]
    Uuid(#[from] uuid::Error),
    #[error(transparent)]
    StorageBackend(#[from] gents::storage_backend::StorageBackendError),
    #[error(transparent)]
    RequestOutput(#[from] gents::session::RequestOutputError),
    #[error(transparent)]
    RemoteP2pAdmin(#[from] crate::remote_admin::RemoteP2pAdminError),
    #[error(transparent)]
    PrincipalIdentity(#[from] crate::client::principal_identity::PrincipalIdentityError),
    #[error(transparent)]
    Mailbox(#[from] gents::mailbox::MailboxError),
    #[error(transparent)]
    Interrupt(#[from] gents::interrupt::InterruptError),
    #[error(transparent)]
    Home(#[from] gents::home::HomeError),
    #[error(transparent)]
    SubscriptionSource(#[from] gents::SubscriptionSourceError),
    #[error(transparent)]
    Schema(#[from] crate::client::schema::SchemaError),
    #[error(transparent)]
    RuntimeMigration(
        Box<gents::storage_backend::ClassifiedStoreError<gents::migration::RuntimeMigrationError>>,
    ),
    #[error(transparent)]
    Bs58(#[from] bs58::decode::Error),
    #[error(transparent)]
    ReceiptJournal(Box<crate::client::mutations::submission_journal::ReceiptJournalError>),
    #[error(transparent)]
    P2p(#[from] P2PError),
    #[error(transparent)]
    Bridge(#[from] crate::error::BridgeError),
    #[error(transparent)]
    Receipt(#[from] crate::client::mutations::ReceiptError),
    #[error(transparent)]
    ChatMutation(Box<crate::client::mutations::ChatMutationError>),
    #[error(transparent)]
    ManageMutation(Box<crate::client::mutations::ManageMutationError>),
    #[error(transparent)]
    TaskMutation(Box<crate::client::mutations::TaskMutationError>),
}

/// A receipt failure stays at the top level so submit classifiers (unknown
/// owner outcome, unconfirmed delivery) see it without unwrapping the chat
/// mutation that produced it.
impl From<crate::client::mutations::ChatMutationError> for ClientCoreError {
    fn from(error: crate::client::mutations::ChatMutationError) -> Self {
        match error {
            crate::client::mutations::ChatMutationError::Receipt(error) => Self::Receipt(error),
            error => Self::ChatMutation(Box::new(error)),
        }
    }
}

impl gents::config_client::TransactionFailure for ClientCoreError {
    fn config_client_error(&self) -> Option<&gents::config_client::ConfigClientError> {
        match self {
            Self::ConfigClient(error) => Some(error),
            Self::GraphqlResponse(error) => error.config_client_error(),
            Self::P2pReconcile(error) => error.config_client_error(),
            Self::Goal(error) => error.config_client_error(),
            Self::Queue(error) => error.config_client_error(),
            Self::ClientQuery(error) => error.config_client_error(),
            Self::ManageMutation(error) => error.config_client_error(),
            Self::TaskMutation(error) => error.config_client_error(),
            Self::ChatMutation(error) => error.config_client_error(),
            Self::Mailbox(error) => error.config_client_error(),
            Self::Interrupt(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl ClientCoreError {
    /// The error a transparent variant forwards. Its `source()` skips that
    /// error, so source-chain classifiers (store lock, store compatibility,
    /// native host failures) search from here.
    pub fn cause(&self) -> &(dyn std::error::Error + 'static) {
        match self {
            Self::StoreKeyProvider(error) => error.host_error(),
            Self::ChatMutation(error) => error.as_ref(),
            Self::PeerDirectory(error) => error,
            Self::ReplicatedSchemaSkew(error) => error,
            Self::StoreRecord(error) => error,
            Self::ConfigClient(error) => error,
            Self::GraphqlResponse(error) => error,
            Self::P2pReconcile(error) => error.as_ref(),
            Self::LocalRuntime(error) => error,
            Self::Identity(error) => error,
            Self::ClientQuery(error) => error.as_ref(),
            Self::Goal(error) => error.as_ref(),
            Self::StoreKeyUpgrade(error) => error,
            Self::StoreKey(error) => error,
            Self::Queue(error) => error.as_ref(),
            Self::SessionHydration(error) => error,
            Self::Enrollment(error) => error,
            Self::ToolCallQuery(error) => error.as_ref(),
            Self::FileReviewAccess(error) => error.as_ref(),
            Self::Paths(error) => error,
            Self::SessionFork(error) => error,
            Self::FileImageHydration(error) => error,
            Self::Uuid(error) => error,
            Self::StorageBackend(error) => error,
            Self::RequestOutput(error) => error,
            Self::RemoteP2pAdmin(error) => error,
            Self::PrincipalIdentity(error) => error,
            Self::Mailbox(error) => error,
            Self::Interrupt(error) => error,
            Self::Home(error) => error,
            Self::SubscriptionSource(error) => error,
            Self::Schema(error) => error,
            Self::RuntimeMigration(error) => error.as_ref(),
            Self::Bs58(error) => error,
            Self::ReceiptJournal(error) => error.as_ref(),
            Self::P2p(error) => error,
            Self::Bridge(error) => error,
            Self::Receipt(error) => error,
            Self::ManageMutation(error) => error.as_ref(),
            Self::TaskMutation(error) => error.as_ref(),
            _ => self,
        }
    }
}

impl From<crate::client::mutations::TaskMutationError> for ClientCoreError {
    fn from(error: crate::client::mutations::TaskMutationError) -> Self {
        Self::TaskMutation(Box::new(error))
    }
}

impl From<crate::client::mutations::submission_journal::ReceiptJournalError> for ClientCoreError {
    fn from(error: crate::client::mutations::submission_journal::ReceiptJournalError) -> Self {
        Self::ReceiptJournal(Box::new(error))
    }
}

impl From<crate::client::mutations::ManageMutationError> for ClientCoreError {
    fn from(error: crate::client::mutations::ManageMutationError) -> Self {
        Self::ManageMutation(Box::new(error))
    }
}

impl From<crate::client::query::ClientQueryError> for ClientCoreError {
    fn from(error: crate::client::query::ClientQueryError) -> Self {
        Self::ClientQuery(Box::new(error))
    }
}

impl From<gents::storage_backend::ClassifiedStoreError<gents::migration::RuntimeMigrationError>>
    for ClientCoreError
{
    fn from(
        error: gents::storage_backend::ClassifiedStoreError<
            gents::migration::RuntimeMigrationError,
        >,
    ) -> Self {
        Self::RuntimeMigration(Box::new(error))
    }
}

impl From<gents::agent::p2p_reconcile::error::P2pReconcileError> for ClientCoreError {
    fn from(error: gents::agent::p2p_reconcile::error::P2pReconcileError) -> Self {
        Self::P2pReconcile(Box::new(error))
    }
}

impl From<gents::tool_call_lifecycle::ToolCallQueryError> for ClientCoreError {
    fn from(error: gents::tool_call_lifecycle::ToolCallQueryError) -> Self {
        Self::ToolCallQuery(Box::new(error))
    }
}

impl From<gents::tool_call_lifecycle::FileReviewAccessError> for ClientCoreError {
    fn from(error: gents::tool_call_lifecycle::FileReviewAccessError) -> Self {
        Self::FileReviewAccess(Box::new(error))
    }
}

impl From<gents::lifecycle::QueueError> for ClientCoreError {
    fn from(error: gents::lifecycle::QueueError) -> Self {
        Self::Queue(Box::new(error))
    }
}

impl From<gents::goal::GoalError> for ClientCoreError {
    fn from(error: gents::goal::GoalError) -> Self {
        Self::Goal(Box::new(error))
    }
}
