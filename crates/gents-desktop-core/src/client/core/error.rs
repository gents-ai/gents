use defra_p2p_adapter::P2PError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientCoreError {
    #[error(transparent)]
    PeerDirectory(#[from] crate::client::peer_directory::PeerDirectoryError),
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
