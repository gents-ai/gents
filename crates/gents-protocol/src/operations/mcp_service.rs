use std::error::Error;

use serde::{Deserialize, Serialize};

use super::document::{DocumentDeleteError, DocumentOperationError};
use super::NextCall;
use super::StringUpdate;

pub const RESOURCE: &str = "mcp-service";

/// Lean `DocumentOperations.Toggle`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    #[default]
    Omitted,
    On,
    Off,
}

impl Toggle {
    pub fn requested(self) -> Option<bool> {
        match self {
            Self::Omitted => None,
            Self::On => Some(true),
            Self::Off => Some(false),
        }
    }
}

/// Lean `DocumentOperations.StoredToggle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredToggle {
    Absent,
    On,
    Off,
}

/// Lean `DocumentOperations.EndpointScheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointScheme {
    Http,
    Https,
}

/// Lean `DocumentOperations.EndpointHost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointHost {
    Ipv4,
    Name,
}

/// Lean `DocumentOperations.EndpointStorage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointStorage {
    VerbatimUrl,
    LanIp,
    Hostname,
}

/// Registers an MCP service at `endpoint`, or moves a registered one there.
/// Everything the call omits keeps its stored value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpRegisterInput {
    pub service_id: String,
    pub endpoint: String,
    pub display_name: StringUpdate,
    pub description: StringUpdate,
    pub send_node_did: Toggle,
    pub enabled: Toggle,
}

#[derive(Debug, thiserror::Error)]
pub enum McpServiceOperationError {
    #[error("the endpoint is not usable: {source}")]
    Endpoint {
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error(transparent)]
    Document(#[from] DocumentOperationError),
    #[error(transparent)]
    Delete(#[from] DocumentDeleteError),
}

impl McpServiceOperationError {
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::Endpoint { .. } => NextCall::VerbHelp {
                resource: RESOURCE,
                verb: "register",
            },
            Self::Document(error) => error.recovery(RESOURCE),
            Self::Delete(error) => error.recovery(RESOURCE),
        }
    }
}
