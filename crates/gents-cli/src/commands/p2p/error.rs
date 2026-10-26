use std::io;
use std::path::PathBuf;

use gents::agent::p2p_reconcile::error::P2pReconcileError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum P2pCommandError {
    #[error(transparent)]
    Cli(#[from] crate::CliError),
    #[error(transparent)]
    HomeState(#[from] crate::home_state::HomeStateError),
    #[error(transparent)]
    ConfigClient(#[from] gents::config_client::ConfigClientError),
    #[error(transparent)]
    RequestHelpers(#[from] crate::request_helpers::RequestHelpersError),
    #[error(transparent)]
    OutputFormat(#[from] crate::cli::output_format::OutputFormatError),
    #[error(transparent)]
    Reconcile(#[from] P2pReconcileError),
    #[error("provide --collection-id")]
    MissingCollectionId,
    #[error("provide --collection")]
    MissingCollection,
    #[error("runtime reported an empty shareable P2P address")]
    EmptyShareableAddress,
    #[error("runtime reported a shareable P2P address but no usable peer id")]
    NoUsablePeerId,
    #[error(transparent)]
    ReplicationFilters(#[from] gents::agent::p2p_reconcile::UnsupportedReplicationFilter),
    #[error("parsing --filter arguments for p2p admin replicators add")]
    ReplicatorFilter(#[source] super::replicators::ReplicatorFilterError),
    #[error("no init config found in {}; run `gents init` first", .home_dir.display())]
    NoInitConfig { home_dir: PathBuf },
    #[error("writing p2p network table row")]
    NetworkTableRow(#[source] io::Error),
    #[error("flushing p2p network table")]
    NetworkTableFlush(#[source] io::Error),
    #[error("writing p2p pairings table row")]
    PairingsTableRow(#[source] io::Error),
    #[error("flushing p2p pairings table")]
    PairingsTableFlush(#[source] io::Error),
    #[error("writing p2p templates table row")]
    TemplatesTableRow(#[source] io::Error),
    #[error("flushing p2p templates table")]
    TemplatesTableFlush(#[source] io::Error),
    #[error("request_id must not be empty")]
    EmptyRequestId,
    #[error("loading operator identity for {purpose}")]
    LoadOperatorIdentity {
        purpose: &'static str,
        #[source]
        source: Box<P2pCommandError>,
    },
    #[error("parsing runtime GraphQL endpoint")]
    ParseGraphqlEndpoint(#[source] url::ParseError),
    #[error("signing {what}")]
    SignOperatorCommand {
        what: &'static str,
        #[source]
        source: gents::identity::IdentityError,
    },
    #[error("{what}")]
    OperatorRequest {
        what: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("runtime rejected {what} ({status}): {message}")]
    OperatorRejected {
        what: &'static str,
        status: reqwest::StatusCode,
        message: String,
    },
    #[error(transparent)]
    Identity(#[from] gents::identity::IdentityError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub(super) type Result<T, E = P2pCommandError> = std::result::Result<T, E>;
