use rmcp::{model::ToolAnnotations, service::ServiceError};

/// Default per-call retry budget for retry-safe MCP `call_tool` calls.
pub const DEFAULT_MCP_CALL_MAX_RETRIES: usize = 1;

/// Global upper bound on retries per MCP `call_tool` invocation.
pub const MAX_ALLOWED_MCP_CALL_RETRIES: usize = 3;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolRetryMetadata {
    #[default]
    Absent,
    NonIdempotent,
    ReadOnly,
    Idempotent,
}

impl McpToolRetryMetadata {
    pub fn from_annotations(annotations: Option<&ToolAnnotations>) -> Self {
        match annotations {
            Some(annotations) => {
                Self::from_hints(annotations.read_only_hint, annotations.idempotent_hint)
            }
            None => Self::Absent,
        }
    }

    pub const fn from_hints(read_only_hint: Option<bool>, idempotent_hint: Option<bool>) -> Self {
        match (read_only_hint, idempotent_hint) {
            (_, Some(false)) => Self::NonIdempotent,
            (Some(true), None | Some(true)) => Self::ReadOnly,
            (None | Some(false), Some(true)) => Self::Idempotent,
            (None | Some(false), None) => Self::Absent,
        }
    }

    pub const fn allows_replay(self) -> bool {
        matches!(self, Self::ReadOnly | Self::Idempotent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpCallRetryPolicy {
    pub metadata: McpToolRetryMetadata,
    pub max_retries: usize,
}

impl Default for McpCallRetryPolicy {
    fn default() -> Self {
        Self {
            metadata: McpToolRetryMetadata::Absent,
            max_retries: DEFAULT_MCP_CALL_MAX_RETRIES,
        }
    }
}

impl McpCallRetryPolicy {
    pub const fn for_metadata(metadata: McpToolRetryMetadata) -> Self {
        Self {
            metadata,
            max_retries: DEFAULT_MCP_CALL_MAX_RETRIES,
        }
    }

    pub const fn with_max_retries(mut self, max_retries: usize) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub const fn effective_budget(self) -> usize {
        if self.max_retries < MAX_ALLOWED_MCP_CALL_RETRIES {
            self.max_retries
        } else {
            MAX_ALLOWED_MCP_CALL_RETRIES
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpCallFailureClass {
    TransportDropped,
    AuthRefused,
    ConnectFailed,
    ProtocolOrToolError,
    TimedOut,
    Cancelled,
}

impl McpCallFailureClass {
    pub const fn is_transient_transport(self) -> bool {
        matches!(self, Self::TransportDropped)
    }

    pub fn from_service_error(error: &ServiceError) -> Self {
        match error {
            ServiceError::TransportSend(_) | ServiceError::TransportClosed => {
                Self::TransportDropped
            }
            ServiceError::McpError(_) | ServiceError::UnexpectedResponse => {
                Self::ProtocolOrToolError
            }
            ServiceError::Timeout { .. } => Self::TimedOut,
            ServiceError::Cancelled { .. } => Self::Cancelled,
            _ => Self::ProtocolOrToolError,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct McpCallFailure {
    pub(crate) class: McpCallFailureClass,
    pub(crate) message: String,
}

impl McpCallFailure {
    pub(crate) fn from_service_error(error: ServiceError) -> Self {
        Self {
            class: McpCallFailureClass::from_service_error(&error),
            message: format!("call_tool: {error}"),
        }
    }

    #[cfg(test)]
    pub(crate) fn new(class: McpCallFailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpCallRetryDecision {
    FailFinal,
    EvictAndRetry,
}

pub const fn decide_mcp_call_retry(
    policy: McpCallRetryPolicy,
    failure: McpCallFailureClass,
    retries_used: usize,
) -> McpCallRetryDecision {
    if policy.metadata.allows_replay()
        && failure.is_transient_transport()
        && retries_used < policy.effective_budget()
    {
        McpCallRetryDecision::EvictAndRetry
    } else {
        McpCallRetryDecision::FailFinal
    }
}
