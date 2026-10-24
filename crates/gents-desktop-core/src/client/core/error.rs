use defra_p2p_adapter::P2PError;
use gents_protocol::peer_schema::ReplicatedSchemaSkew;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientCoreError {
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
    #[error("P2P transport reported an empty peer id")]
    P2pEmptyPeerId,
    #[error("P2P transport reported no listen addresses")]
    P2pNoListenAddresses,
    #[error("too many client observation attachments")]
    TooManyObservationAttachments,
}
