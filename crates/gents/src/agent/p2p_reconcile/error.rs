use crate::agent::p2p_reconcile::session_hydration_closure::CanonicalClosureError;
use defra_p2p_adapter::P2PError;

use super::enrollment_offer::MAX_OFFER_VALIDITY;
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
    #[error("enrollment offer validity must be between 1 and {} seconds", MAX_OFFER_VALIDITY.num_seconds())]
    OfferValidityOutOfRange,
    #[error("reading live server peer ID for enrollment offer")]
    ReadServerPeerId { source: P2PError },
    #[error("reading live server ticket for enrollment offer")]
    ReadServerTicket { source: P2PError },
    #[error("server has no shareable P2P address for enrollment")]
    ServerHasNoShareableAddress,
    #[error("server produced an invalid shareable P2P ticket")]
    InvalidServerTicket { source: p2p::Error },
    #[error("server shareable ticket does not match its live peer ID")]
    ServerTicketPeerMismatch,
    #[error("signing authenticated enrollment offer")]
    SignEnrollmentOffer {
        source: crate::identity::IdentityError,
    },
    #[error(transparent)]
    EnrollmentProtocol(#[from] gents_protocol::enrollment::EnrollmentError),
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error("client route collection {name} is not registered")]
    ClientRouteCollectionUnregistered { name: String },
    #[error("local_peer_id: {error}")]
    LocalPeerIdFailed { error: P2PError },
    #[error("listen_addresses: {error}")]
    ListenAddressesFailed { error: P2PError },
    #[error("shareable_address: {error}")]
    ShareableAddressFailed { error: P2PError },
    #[error("P2P node reported no shareable address for PeerEndpoint")]
    NoPeerEndpointAddress,
    #[error("signing PeerEndpoint binding")]
    SignPeerEndpoint {
        source: crate::identity::IdentityError,
    },
    #[error("server enrollment schema {schema_fingerprint} is incompatible with {enrollment_schema_fingerprint}")]
    ServerEnrollmentSchemaIncompatible {
        schema_fingerprint: String,
        enrollment_schema_fingerprint: String,
    },
    #[error("unsupported enrollment profile")]
    UnsupportedEnrollmentProfile,
    #[error("server enrollment ticket does not match its signed peer ID")]
    ServerEnrollmentTicketDoesNotMatch,
    #[error("enrollment offer signature is invalid")]
    EnrollmentOfferSignatureInvalid,
    #[error("network {network_id} is pinned to admin {admin_did}; refusing conflicting admin {admin_did2}")]
    NetworkPinnedAdminRefusingConflictingAdmin {
        network_id: String,
        admin_did: String,
        admin_did2: String,
    },
    #[error("network {network_id} has {pins_len} local admin pins; refusing enrollment")]
    NetworkHasLocalAdminPinsRefusing {
        network_id: String,
        pins_len: String,
    },
    #[error("admin pin commit was not observably recovered after: {detail}")]
    AdminPinCommitWasNotObservably { detail: String },
    #[error("offer {offer_id} has multiple local enrollment requests")]
    OfferHasMultipleLocalEnrollmentRequests { offer_id: String },
    #[error("persisted enrollment request embeds a different signed offer")]
    PersistedEnrollmentRequestEmbedsDifferentSigned,
    #[error("persisted enrollment request signature is invalid")]
    PersistedEnrollmentRequestSignatureInvalid,
    #[error("this node has no P2P transport")]
    NodeHasNoP2PTransport,
    #[error("decoding server enrollment offer")]
    DecodingServerEnrollmentOffer(#[source] gents_protocol::enrollment::EnrollmentError),
    #[error("enrollment request commit was not observably recovered after: {detail}")]
    EnrollmentRequestCommitWasNotObservably { detail: String },
    #[error("server enrollment offer contains an invalid Iroh ticket")]
    ServerEnrollmentOfferContainsInvalidIroh(#[source] p2p::Error),
    #[error("timed out connecting to enrollment server")]
    TimedOutConnectingEnrollmentServer(#[source] tokio::time::error::Elapsed),
    #[error("connecting to enrollment server")]
    ConnectingEnrollmentServer(#[source] TransportFailure),
    #[error("validating enrollment server peer ID")]
    ValidatingEnrollmentServerPeerID(#[source] TransportFailure),
    #[error("timed out authenticating enrollment server identity")]
    TimedOutAuthenticatingEnrollmentServerIdentity(#[source] tokio::time::error::Elapsed),
    #[error("enrollment server has no configured authenticated identity")]
    EnrollmentServerHasNoConfiguredAuthenticated,
    #[error("validating authored enrollment request")]
    ValidatingAuthoredEnrollmentRequest(#[source] gents_protocol::enrollment::EnrollmentError),
    #[error("enrollment request mutation returned no document ID")]
    EnrollmentRequestMutationReturnedNoDocument(#[source] gents_protocol::graphql::GraphqlError),
    #[error("enrollment authority owner stopped")]
    EnrollmentAuthorityOwnerStopped(
        #[source]
        tokio::sync::mpsc::error::SendError<
            super::enrollment_reconcile::EnrollmentAuthorityCommand,
        >,
    ),
    #[error("enrollment authority owner dropped fresh authorization response")]
    EnrollmentAuthorityOwnerDroppedFreshAuthorization(
        #[source] tokio::sync::oneshot::error::RecvError,
    ),
    #[error("enrollment authority owner dropped fresh peer authorization response")]
    EnrollmentAuthorityOwnerDroppedFreshPeer(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("enrollment authority owner dropped fresh member response")]
    EnrollmentAuthorityOwnerDroppedFreshMember(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("enrollment authority owner dropped peer admission response")]
    EnrollmentAuthorityOwnerDroppedPeerAdmission(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("reload enrollment authority for authorization fence")]
    ReloadEnrollmentAuthorityAuthorizationFence(#[source] Box<P2pReconcileError>),
    #[error("reload enrollment authority for peer admission")]
    ReloadEnrollmentAuthorityPeerAdmission(#[source] Box<P2pReconcileError>),
    #[error("reload enrollment authority for transport fence")]
    ReloadEnrollmentAuthorityTransportFence(#[source] Box<P2pReconcileError>),
    #[error("reload enrollment authority for request admission")]
    ReloadEnrollmentAuthorityRequestAdmission(#[source] Box<P2pReconcileError>),
    #[error("client template is missing")]
    ClientTemplateMissing,
    #[error("reload enrollment authority before route publication")]
    ReloadEnrollmentAuthorityBeforeRoutePublication(#[source] Box<P2pReconcileError>),
    #[error("session command schema missing")]
    SessionCommandSchemaMissing,
    #[error("goal observation schema missing")]
    GoalObservationSchemaMissing,
    #[error("goal control command is missing or ambiguous")]
    GoalControlCommandMissingAmbiguous,
    #[error("goal control immutable scope disagrees with signed intent")]
    GoalControlImmutableScopeDisagreesSigned,
    #[error("goal control receipt lost its command")]
    GoalControlReceiptLostCommand,
    #[error("missing goal command identity query")]
    MissingGoalCommandIdentityQuery,
    #[error("missing desired routes")]
    MissingDesiredRoutes,
    #[error("missing applied routes")]
    MissingAppliedRoutes,
    #[error("selected file image has not been acknowledged by the peer")]
    SelectedFileImageHasNotBeen,
    #[error("session hydration served commit lost its pending-row compare-and-set")]
    SessionHydrationServedCommitLostPending,
    #[error("session hydration rejected commit lost its pending-row compare-and-set")]
    SessionHydrationRejectedCommitLostPending,
    #[error("terminal hydration selection does not belong to the exact request")]
    TerminalHydrationSelectionDoesNotBelong,
    #[error("terminal hydration selection is not an eligible assistant header")]
    TerminalHydrationSelectionNotEligibleAssistant,
    #[error("hydration origin is missing, denied, or conflicting")]
    HydrationOriginMissingDeniedConflicting,
    #[error("hydration provenance is missing, denied, or conflicting")]
    HydrationProvenanceMissingDeniedConflicting,
    #[error("hydration reconciler cannot sign for another agent")]
    HydrationReconcilerCannotSignAnotherAgent,
    #[error("injected terminal write failure")]
    InjectedTerminalWriteFailure,
    #[error("temporary transport failure")]
    TemporaryTransportFailure,
    #[error("transport outcome unknown after dispatch")]
    TransportOutcomeUnknownAfterDispatch,
    #[error("load pending session hydration requests")]
    LoadPendingSessionHydrationRequests(#[source] Box<P2pReconcileError>),
    #[error("reconcile session hydration {request_key}")]
    ReconcileSessionHydration {
        request_key: String,
        #[source]
        source: Box<P2pReconcileError>,
    },
    #[error("load hydration admission")]
    LoadHydrationAdmission(#[source] Box<P2pReconcileError>),
    #[error("mark session hydration rejected")]
    MarkSessionHydrationRejected(#[source] Box<P2pReconcileError>),
    #[error("admitted hydration request has no authenticated enrollment fence")]
    AdmittedHydrationRequestHasNoAuthenticated,
    #[error("load hydration selection")]
    LoadHydrationSelection(#[source] Box<P2pReconcileError>),
    #[error("revalidate hydration authorization generation")]
    RevalidateHydrationAuthorizationGeneration(#[source] Box<P2pReconcileError>),
    #[error("revalidate hydration authorization at terminal commit")]
    RevalidateHydrationAuthorizationTerminalCommit(#[source] Box<P2pReconcileError>),
    #[error("mark session hydration served")]
    MarkSessionHydrationServed(#[source] Box<P2pReconcileError>),
    #[error("terminal hydration request omitted physical id")]
    TerminalHydrationRequestOmittedPhysicalId,
    #[error("terminal hydration request omitted terminal_output")]
    TerminalHydrationRequestOmittedTerminalOutput,
    #[error("terminal hydration selection is missing its exact header")]
    TerminalHydrationSelectionMissingExactHeader,
    #[error("build authorized canonical hydration closure")]
    BuildAuthorizedCanonicalHydrationClosure(#[source] CanonicalClosureError),
    #[error("terminal hydration request missing from authorized base rows")]
    TerminalHydrationRequestMissingAuthorizedBase,
    #[error("hydration provenance omitted physical id")]
    HydrationProvenanceOmittedPhysicalId,
    #[error("load fresh authenticated enrollment authority for hydration")]
    LoadFreshAuthenticatedEnrollmentAuthorityHydration(#[source] Box<P2pReconcileError>),
    #[error("enrollment authority is conflicted: {detail}")]
    EnrollmentAuthorityConflicted { detail: String },
    #[error("request {request_id} is already approved")]
    RequestAlreadyApproved { request_id: String },
    #[error("request {request_id} is already denied")]
    RequestAlreadyDenied { request_id: String },
    #[error("authorization sequence exhausted the DefraDB Int range for this member")]
    AuthorizationSequenceExhaustedDefraDBIntRange,
    #[error("authorization revocation sequence is invalid or exhausted")]
    AuthorizationRevocationSequenceInvalidExhausted,
    #[error("enrollment request targets a different server transport peer")]
    EnrollmentRequestTargetsDifferentServerTransport,
    #[error("candidate transport identity does not match its signed request DID")]
    CandidateTransportIdentityDoesNotMatch,
    #[error("enrollment request is not yet valid")]
    EnrollmentRequestNotYetValid,
    #[error("enrollment request expired during approval")]
    EnrollmentRequestExpiredDuringApproval,
    #[error("authorization generation expired or changed before route receipt publication")]
    AuthorizationGenerationExpiredChangedBeforeRoute,
    #[error("expected exactly one Network, found {network_rows_len}")]
    ExpectedExactlyOneNetworkFound { network_rows_len: String },
    #[error("Network admin does not match the local runtime identity")]
    NetworkAdminDoesNotMatchLocal,
    #[error("Network signature is invalid")]
    NetworkSignatureInvalid,
    #[error("enrollment decision signature is invalid")]
    EnrollmentDecisionSignatureInvalid,
    #[error("authorization revision signature is invalid")]
    AuthorizationRevisionSignatureInvalid,
    #[error("enrollment route receipt signature is invalid")]
    EnrollmentRouteReceiptSignatureInvalid,
    #[error("enrollment offer schema fingerprint mismatch")]
    EnrollmentOfferSchemaFingerprintMismatch,
    #[error("offer network mismatch")]
    OfferNetworkMismatch,
    #[error("offer admin mismatch")]
    OfferAdminMismatch,
    #[error("offer server ticket peer mismatch")]
    OfferServerTicketPeerMismatch,
    #[error("request candidate ticket peer mismatch")]
    RequestCandidateTicketPeerMismatch,
    #[error("enrollment terminal commit was not observably recovered after: {detail}")]
    EnrollmentTerminalCommitWasNotObservably {
        detail: String,
        #[source]
        source: Box<P2pReconcileError>,
    },
    #[error("request {request_id} has no current approval to revoke")]
    RequestHasNoCurrentApprovalRevoke { request_id: String },
    #[error("revocation commit was not observably recovered after: {detail}")]
    RevocationCommitWasNotObservablyRecovered { detail: String },
    #[error("runtime P2P is unavailable")]
    RuntimeP2PUnavailable,
    #[error("timed out reading the enrollment server peer identity")]
    TimedOutReadingEnrollmentServerPeer(#[source] tokio::time::error::Elapsed),
    #[error("timed out connecting to enrollment candidate")]
    TimedOutConnectingEnrollmentCandidate(#[source] tokio::time::error::Elapsed),
    #[error("timed out resolving enrollment candidate identity")]
    TimedOutResolvingEnrollmentCandidateIdentity(#[source] tokio::time::error::Elapsed),
    #[error("enrollment candidate has no authenticated identity")]
    EnrollmentCandidateHasNoAuthenticatedIdentity,
    #[error("parsing enrollment request issued_at")]
    ParsingEnrollmentRequestIssued(#[source] chrono::ParseError),
    #[error("parsing enrollment request expires_at")]
    ParsingEnrollmentRequestExpires(#[source] chrono::ParseError),
    #[error("exact committed approval pair is absent")]
    ExactCommittedApprovalPairAbsent,
    #[error("exact committed denial is absent")]
    ExactCommittedDenialAbsent,
    #[error("route receipt commit was not observably recovered after: {detail}")]
    RouteReceiptCommitWasNotObservably { detail: String },
    #[error("parsing enrollment server ticket")]
    ParsingEnrollmentServerTicket(#[source] p2p::Error),
    #[error("parsing enrollment candidate ticket")]
    ParsingEnrollmentCandidateTicket(#[source] p2p::Error),
    #[error("file-image demand omitted physical identity")]
    FileImageDemandOmittedPhysicalIdentity,
    #[error("runtime file-image collection is unavailable")]
    RuntimeFileImageCollectionUnavailable,
    #[error("file-image reconciler cannot sign for another node")]
    FileImageReconcilerCannotSignAnother,
    #[error("file-image receipt changed its recorded outcome")]
    FileImageReceiptChangedRecordedOutcome,
    #[error("file-image receipt lost its pending-row compare-and-set")]
    FileImageReceiptLostPendingRow,
    #[error("recorded file-image outcome is ambiguous")]
    RecordedFileImageOutcomeAmbiguous,
    #[error("recorded file-image receipt crossed source or demand scope")]
    RecordedFileImageReceiptCrossedSource,
    #[error("recorded file-image receipt has an invalid source signature")]
    RecordedFileImageReceiptHasInvalid,
    #[error("image demand has no authenticated enrollment fence")]
    ImageDemandHasNoAuthenticatedEnrollment,
    #[error("commit file-image hydration receipt")]
    CommitFileImageHydrationReceipt(#[source] Box<P2pReconcileError>),
    #[error("file-image transaction did not select an outcome")]
    FileImageTransactionDidNotSelect,
    #[error("file-image receipt query omitted its rows")]
    FileImageReceiptQueryOmittedRows,
    #[error("edit command is missing or ambiguous")]
    EditCommandMissingAmbiguous,
    #[error("edit immutable scope disagrees with signed intent")]
    EditImmutableScopeDisagreesSignedIntent,
    #[error("edit receipt write lost its command")]
    EditReceiptWriteLostCommand,
    #[error("missing session scope")]
    MissingSessionScope,
    #[error("missing command identity query")]
    MissingCommandIdentityQuery,
    #[error("checking enrollment authority for {raw_peer_id}")]
    CheckingEnrollmentAuthority {
        raw_peer_id: String,
        #[source]
        source: Box<P2pReconcileError>,
    },
    #[error("connect pairing peer")]
    ConnectPairingPeer(#[source] RemoteP2pAdminError),
    #[error("reload live pairing state after enrollment authority closed")]
    ReloadLivePairingStateAfterEnrollment(#[source] Box<P2pReconcileError>),
    #[error("verify live pairing state after reconcile")]
    VerifyLivePairingStateAfterReconcile(#[source] Box<P2pReconcileError>),
    #[error(transparent)]
    Transport(#[from] TransportFailure),
    #[error(transparent)]
    CanonicalRows(#[from] crate::session::canonical_rows::CanonicalRowsError),
    #[error(transparent)]
    GoalControl(#[from] gents_protocol::goal_control::GoalControlError),
    #[error(transparent)]
    GraphqlResponse(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    #[error(transparent)]
    RemoteP2pAdmin(#[from] crate::agent::p2p_reconcile::trait_def::RemoteP2pAdminError),
    #[error(transparent)]
    SessionInputEdit(#[from] gents_protocol::session_input_edit::SessionInputEditError),
    #[error(transparent)]
    SubscriptionSource(#[from] crate::trigger_engine::subscription_source::SubscriptionSourceError),
    #[error(transparent)]
    ToolCallQuery(#[from] crate::tool_call_lifecycle::query::ToolCallQueryError),
    #[error(transparent)]
    Error(#[from] defra_node::Error),
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error(transparent)]
    Goal(Box<crate::goal::GoalError>),
    #[error(transparent)]
    Queue(Box<crate::lifecycle::QueueError>),
    #[error(transparent)]
    OutOfRange(#[from] chrono::OutOfRangeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<crate::lifecycle::QueueError> for P2pReconcileError {
    fn from(error: crate::lifecycle::QueueError) -> Self {
        Self::Queue(Box::new(error))
    }
}

impl From<crate::goal::GoalError> for P2pReconcileError {
    fn from(error: crate::goal::GoalError) -> Self {
        Self::Goal(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for P2pReconcileError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::ConfigClient(error) => Some(error),
            Self::ReloadEnrollmentAuthorityAuthorizationFence(error) => error.config_client_error(),
            Self::ReloadEnrollmentAuthorityPeerAdmission(error) => error.config_client_error(),
            Self::ReloadEnrollmentAuthorityTransportFence(error) => error.config_client_error(),
            Self::ReloadEnrollmentAuthorityRequestAdmission(error) => error.config_client_error(),
            Self::ReloadEnrollmentAuthorityBeforeRoutePublication(error) => {
                error.config_client_error()
            }
            Self::LoadPendingSessionHydrationRequests(error) => error.config_client_error(),
            Self::LoadHydrationAdmission(error) => error.config_client_error(),
            Self::MarkSessionHydrationRejected(error) => error.config_client_error(),
            Self::LoadHydrationSelection(error) => error.config_client_error(),
            Self::RevalidateHydrationAuthorizationGeneration(error) => error.config_client_error(),
            Self::RevalidateHydrationAuthorizationTerminalCommit(error) => {
                error.config_client_error()
            }
            Self::MarkSessionHydrationServed(error) => error.config_client_error(),
            Self::CommitFileImageHydrationReceipt(error) => error.config_client_error(),
            Self::ReloadLivePairingStateAfterEnrollment(error) => error.config_client_error(),
            Self::VerifyLivePairingStateAfterReconcile(error) => error.config_client_error(),
            Self::GraphqlResponse(error) => error.config_client_error(),
            Self::ToolCallQuery(error) => error.config_client_error(),
            Self::Goal(error) => error.config_client_error(),
            Self::Queue(error) => error.config_client_error(),
            _ => None,
        }
    }
}

/// A failure the P2P transport reported as text.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TransportFailure(pub String);

pub(crate) type Result<T, E = P2pReconcileError> = std::result::Result<T, E>;
