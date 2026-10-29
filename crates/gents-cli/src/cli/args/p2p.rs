use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use crate::cli::output_format::OutputFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum P2pTransportArg {
    None,
    Iroh,
}

impl P2pTransportArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Iroh => "iroh",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum P2pRelayModeArg {
    Default,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum P2pDiscoveryArg {
    #[value(name = "n0")]
    N0,
    Disabled,
}

#[derive(Subcommand)]
pub(crate) enum P2pCommand {
    #[command(about = "Show live P2P connectivity for the running runtime")]
    Status(P2pAccessArgs),
    #[command(about = "List connected peers for the running runtime")]
    Peers(P2pAccessArgs),
    #[command(about = "Run P2P HTTP endpoint diagnostics")]
    Diagnose(P2pAccessArgs),
    #[command(
        about = "Inspect authenticated enrollment-owned P2P pairings",
        after_help = "\
Pairing documents are authored only by authenticated enrollment. This command \
is observational and cannot grant, replace, or revoke enrollment authority."
    )]
    Pairings {
        #[command(subcommand)]
        command: P2pPairingsCommand,
    },
    #[command(about = "Operate authenticated enrollment authority")]
    Enrollment {
        #[command(subcommand)]
        command: P2pEnrollmentCommand,
    },
    #[command(
        about = "Register into and inspect the peer discovery registry",
        after_help = "\
The peer registry (PeerRegistry collection) is informational discovery metadata. \
It cannot authorize enrollment or materialize P2P routes.\n\
\n\
`p2p network register` — write this node's row (idempotent / upsert).\n\
`p2p network list`     — read all PeerRegistry rows with liveness and pairing annotations.\n\
`p2p network rm`       — deregister this node (delete its PeerRegistry row)."
    )]
    Network {
        #[command(subcommand)]
        command: P2pNetworkCommand,
    },
    #[command(
        about = "Low-level live P2P admin for diagnostics and repair",
        after_help = "\
These commands mutate live P2P state on the running runtime directly. The \
enrollment reconciler does not treat those changes as durable authority. Use \
them only for diagnostics, break-glass repair, \
and one-off document fetches. Authenticated enrollment is the sole route authority."
    )]
    Admin {
        #[command(subcommand)]
        command: P2pAdminCommand,
    },
    #[command(
        about = "Inspect the built-in scope-template catalog",
        after_help = "\
Scope templates are named pairing intents that bundle a fixed collection set, \
a per-collection scoping policy, and a delivery \
mode (push or replicate). Enrollment approvals select the authoritative route scope."
    )]
    Templates {
        #[command(subcommand)]
        command: P2pTemplatesCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum P2pNetworkCommand {
    #[command(about = "Register this node into the peer discovery registry (idempotent upsert)")]
    Register(P2pNetworkRegisterArgs),
    #[command(about = "List PeerRegistry rows with liveness and pairing annotations")]
    List(P2pNetworkListArgs),
    #[command(
        name = "rm",
        about = "Deregister this node from the peer discovery registry"
    )]
    Rm(P2pAccessArgs),
}

#[derive(Subcommand)]
pub(crate) enum P2pEnrollmentCommand {
    #[command(about = "List fresh pending authenticated enrollment requests")]
    Pending(P2pAccessArgs),
    #[command(about = "Approve a pending request with a bounded lease")]
    Approve(P2pEnrollmentDecisionArgs),
    #[command(about = "Deny a pending request")]
    Deny(P2pEnrollmentDecisionArgs),
    #[command(about = "Revoke the current authorization for a request")]
    Revoke(P2pEnrollmentDecisionArgs),
}

#[derive(clap::Args)]
pub(crate) struct P2pNetworkRegisterArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    /// Human-readable display name for this node in the registry.
    #[arg(long, value_name = "NAME")]
    pub(crate) display_name: Option<String>,
    /// Scope template this node offers (repeatable). A node advertises the
    /// templates it is willing to replicate; a discovering peer materializes a
    /// scoped pairing from one of them. Defaults to `conversation` (filtered
    /// push of the peer's session slice) when none are given.
    #[arg(long = "template", value_name = "TEMPLATE")]
    pub(crate) templates: Vec<String>,
    /// Network / fleet id. Defaults to "default".
    #[arg(long = "network", value_name = "NETWORK_ID")]
    pub(crate) network_id: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pNetworkListArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(Debug, clap::Args)]
pub(crate) struct P2pEnrollmentDecisionArgs {
    /// Immutable enrollment request ID shown by the operator pending queue.
    pub(crate) request_id: String,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    /// Approval lease in seconds; ignored for deny/revoke.
    #[arg(long, default_value_t = gents_protocol::enrollment::DEFAULT_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS)]
    pub(crate) lease_seconds: u64,
}

#[derive(Subcommand)]
pub(crate) enum P2pTemplatesCommand {
    #[command(about = "List all built-in scope templates")]
    List(P2pTemplatesListArgs),
}

#[derive(clap::Args)]
pub(crate) struct P2pTemplatesListArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(Subcommand)]
pub(crate) enum P2pAdminCommand {
    #[command(about = "Connect the running runtime to another peer")]
    Connect(P2pConnectArgs),
    #[command(about = "Manage live collection subscriptions on the running runtime")]
    Collections {
        #[command(subcommand)]
        command: P2pCollectionsCommand,
    },
    #[command(about = "Manage live push replicators on the running runtime")]
    Replicators {
        #[command(subcommand)]
        command: P2pReplicatorsCommand,
    },
    #[command(about = "Manage live document subscriptions and document sync")]
    Documents {
        #[command(subcommand)]
        command: P2pDocumentsCommand,
    },
}

#[derive(clap::Args)]
pub(crate) struct P2pAccessArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pConnectArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) peer: String,
}

#[derive(Subcommand)]
pub(crate) enum P2pCollectionsCommand {
    #[command(about = "List subscribed P2P collections")]
    List(P2pAccessArgs),
    #[command(about = "Subscribe collections or collection profiles for P2P replication")]
    Add(P2pCollectionsMutateArgs),
    #[command(about = "Remove subscribed P2P collections")]
    Remove(P2pCollectionsMutateArgs),
    #[command(about = "Fetch a branchable collection DAG from connected peers")]
    SyncBranchable(P2pSyncBranchableArgs),
    #[command(about = "Fetch collection-version DAG blocks from connected peers")]
    SyncVersions(P2pSyncVersionsArgs),
}

#[derive(Subcommand)]
pub(crate) enum P2pReplicatorsCommand {
    #[command(about = "List configured P2P replicators")]
    List(P2pAccessArgs),
    #[command(about = "Configure a peer replicator for collections or profiles")]
    Add(P2pReplicatorAddArgs),
    #[command(about = "Remove a peer replicator for collections or profiles")]
    Remove(P2pReplicatorRemoveArgs),
}

#[derive(Subcommand)]
pub(crate) enum P2pDocumentsCommand {
    #[command(about = "List document subscriptions for P2P replication")]
    List(P2pAccessArgs),
    #[command(about = "Subscribe documents for P2P replication")]
    Add(P2pDocumentsMutateArgs),
    #[command(about = "Remove document subscriptions from P2P replication")]
    Remove(P2pDocumentsMutateArgs),
    #[command(about = "Fetch documents from connected peers")]
    Sync(P2pDocumentsSyncArgs),
}

#[derive(Subcommand)]
pub(crate) enum P2pPairingsCommand {
    #[command(about = "List enrollment-owned pairings annotated with live health")]
    List(P2pPairingsListArgs),
}

#[derive(clap::Args)]
pub(crate) struct P2pPairingsListArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct P2pCollectionsMutateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "collection", value_name = "COLLECTION")]
    pub(crate) collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    pub(crate) profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
pub(crate) struct P2pSyncBranchableArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "collection-id", value_name = "COLLECTION_ID")]
    pub(crate) collection_id: String,
}

#[derive(clap::Args)]
pub(crate) struct P2pSyncVersionsArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "version-id", value_name = "VERSION_ID")]
    pub(crate) version_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pReplicatorAddArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) peer: String,
    #[arg(long = "collection", value_name = "COLLECTION")]
    pub(crate) collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    pub(crate) profiles: Vec<P2pCollectionProfileArg>,
    /// Per-collection field-equality filter for filtered replication (repeatable).
    /// Format: `<collection>:<field>=<value>`, e.g.
    /// `AgentRequest:node_did=did:key:alice`. Forwarded to the node as the
    /// replicator's `Filters`, which installs a filtered (push-only) replicator
    /// that sends only matching documents. The filter field must be `@immutable`.
    #[arg(long = "filter", value_name = "COLLECTION:FIELD=VALUE")]
    pub(crate) filters: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pReplicatorRemoveArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) peer: String,
    #[arg(long = "collection", value_name = "COLLECTION")]
    pub(crate) collections: Vec<String>,
    #[arg(long = "profile", value_enum, value_name = "PROFILE")]
    pub(crate) profiles: Vec<P2pCollectionProfileArg>,
}

#[derive(clap::Args)]
pub(crate) struct P2pDocumentsMutateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long = "doc-id", value_name = "DOC_ID")]
    pub(crate) doc_ids: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct P2pDocumentsSyncArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_name = "COLLECTION")]
    pub(crate) collection: String,
    #[arg(long = "doc-id", value_name = "DOC_ID")]
    pub(crate) doc_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum P2pCollectionProfileArg {
    Runtime,
    Agent,
    DesktopConfig,
    ChatRequests,
    ToolServices,
    Discovery,
}
