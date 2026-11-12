use std::error::Error;

use serde::Serialize;

use super::agent::NextCall;

/// A named grouping of ACP grants an operator gives one Agent. DefraDB
/// decides whether the caller may write the relation behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermission {
    /// `config_writer` on the Agent's own document.
    SelfConfig,
    /// `agent_manager` on the Node: the agent catalog and nothing else.
    Catalog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionChange {
    Grant,
    Revoke,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPermissionInput {
    pub node_did: String,
    pub agent_did: String,
    pub permission: AgentPermission,
    pub change: PermissionChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentPermissionReceipt {
    pub node_did: String,
    pub agent_did: String,
    pub permission: AgentPermission,
    pub granted: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentPermissionError {
    #[error("{0:?} is not a DID; name the Agent by its agent_did")]
    InvalidAgentDid(String, #[source] Box<dyn Error + Send + Sync>),
    #[error("agent {0:?} is missing or not visible to this actor")]
    AgentUnavailable(String),
    #[error("Node {0:?} is missing or not visible to this actor")]
    NodeUnavailable(String),
    /// Another owner's failure (the store, ACP), unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl AgentPermissionError {
    /// The call that lets the caller correct this failure.
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::InvalidAgentDid(..) | Self::AgentUnavailable(_) => {
                NextCall::List { resource: "agent" }
            }
            Self::NodeUnavailable(_) | Self::Passthrough(_) => {
                NextCall::Inspect { resource: "node" }
            }
        }
    }
}
