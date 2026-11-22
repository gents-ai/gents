use gents_store::config_client::{PackEvidenceError, PackMutationDifference};

/// Why a hosted pack session could not go on. No variant carries a plan
/// value or the runtime's message bytes.
#[derive(Debug, thiserror::Error)]
pub enum PackSessionError {
    #[error(
        "runtime pack graph differs from the client's installation plan: {collection} {}: {}",
        .doc_id.as_deref().unwrap_or("<new document>"),
        Listed(.differences)
    )]
    GraphDiffers {
        collection: String,
        doc_id: Option<String>,
        differences: Vec<PackMutationDifference>,
    },
    #[error(
        "runtime pack relationship differs from the client's installation plan: {collection} \
         {doc_id}: {}",
        Listed(.differences)
    )]
    RelationshipDiffers {
        collection: String,
        doc_id: String,
        differences: Vec<PackMutationDifference>,
    },
    #[error(
        "runtime asked for a `{proposed}` timestamp where the client's installation plan has \
         `{expected}`"
    )]
    TimestampDiffers {
        expected: &'static str,
        proposed: String,
    },
    #[error("the runtime answered the client's {step} with `{received}`")]
    Unexpected {
        step: SessionStep,
        received: &'static str,
    },
    #[error(transparent)]
    RuntimeFailed(#[from] gents_pack::pack::HostedRefusal),
    #[error("remote pack install scope does not match the reviewed artifacts and node")]
    ScopeMismatch,
    #[error("runtime proposed an unexpected extra pack effect")]
    ExtraEffect,
    #[error("the runtime answered an applied relationship with a result")]
    RelationshipResult,
    #[error("pack install request ID exhausted")]
    IdsExhausted,
    #[error("pack session is closed")]
    Closed,
    #[error("remote pack install has no confirmed result")]
    NoResult,
    #[error(transparent)]
    Receive(#[from] ReceiveError),
    #[error(transparent)]
    Send(#[from] SendError),
    #[error(transparent)]
    Evidence(#[from] PackEvidenceError),
    #[error("could not read which policies the runtime's home uses")]
    HomePolicies(#[source] gents_protocol::error_chain::PassedThrough),
    #[error(fmt = gents_protocol::error_chain::held)]
    Endpoint(#[source] anyhow::Error),
}

/// The step of the client's plan a runtime message answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStep {
    Start,
    Read,
    Graph,
    Relationship,
    Effect,
    Timestamp,
    Finish,
}

impl std::fmt::Display for SessionStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Start => "start request",
            Self::Read => "read",
            Self::Graph => "graph approval",
            Self::Relationship => "relationship approval",
            Self::Effect => "applied effect",
            Self::Timestamp => "timestamp",
            Self::Finish => "finish",
        })
    }
}

/// Why no runtime message arrived.
#[derive(Debug, thiserror::Error)]
pub enum ReceiveError {
    #[error(
        "the runtime sent an unreadable pack install message ({bytes} bytes, line {line} column \
         {column})"
    )]
    Unreadable {
        bytes: usize,
        line: usize,
        column: usize,
    },
    #[error("remote pack install connection closed; inspect the runtime before retrying")]
    Closed,
    #[error("remote pack install connection lost; inspect the runtime before retrying")]
    Lost(#[source] tokio_tungstenite::tungstenite::Error),
    #[error("decoding a remote pack install message stopped")]
    Decode(#[source] tokio::task::JoinError),
}

/// Why a client message did not reach the runtime.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("encoding a remote pack install message failed")]
    Encode(#[source] serde_json::Error),
    #[error("encoding a remote pack install message stopped")]
    Stopped(#[source] tokio::task::JoinError),
    #[error("remote pack install connection lost; inspect the runtime before retrying")]
    Lost(#[source] tokio_tungstenite::tungstenite::Error),
}

struct Listed<'a>(&'a [PackMutationDifference]);

impl std::fmt::Display for Listed<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            [] => f.write_str("the plans agree field by field"),
            [only] => write!(f, "{only}"),
            [first, rest @ ..] => write!(
                f,
                "{first} (and {} more difference{})",
                rest.len(),
                if rest.len() == 1 { "" } else { "s" }
            ),
        }
    }
}
