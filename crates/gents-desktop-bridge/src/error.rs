use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Closed set of bridge error codes. Additive codes bump contract MINOR;
/// rename/removal/meaning change bumps MAJOR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum BridgeErrorCode {
    /// Client store / bridge state is not started.
    ClientNotRunning,
    /// Client start / bootstrap failed.
    ClientStartFailed,
    /// Requested resource was not found (task, schedule, tool call, …).
    NotFound,
    /// Caller-supplied argument failed validation.
    InvalidArgument,
    /// Operation is not supported in this host/policy configuration.
    Unsupported,
    /// Peer / runtime HTTP endpoint was unreachable.
    EndpointUnreachable,
    /// Cascade preview signature was missing or drifted.
    StalePreview,
    /// Cascade walk exceeded the safety depth limit.
    CascadeDepthExceeded,
    /// Path escaped an authorized workspace root.
    PathEscapesRoot,
    /// Underlying store / GraphQL / runtime I/O failed.
    Backend,
    /// Enrollment-owned peer pairing and route-actuation failures.
    Pairing,
    /// Catch-all for failures whose producer has not assigned a typed code.
    Unknown,
}

impl BridgeErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientNotRunning => "clientNotRunning",
            Self::ClientStartFailed => "clientStartFailed",
            Self::NotFound => "notFound",
            Self::InvalidArgument => "invalidArgument",
            Self::Unsupported => "unsupported",
            Self::EndpointUnreachable => "endpointUnreachable",
            Self::StalePreview => "stalePreview",
            Self::CascadeDepthExceeded => "cascadeDepthExceeded",
            Self::PathEscapesRoot => "pathEscapesRoot",
            Self::Backend => "backend",
            Self::Pairing => "pairing",
            Self::Unknown => "unknown",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::ClientStartFailed
                | Self::EndpointUnreachable
                | Self::Backend
                | Self::StalePreview
                | Self::Pairing
        )
    }
}

/// Serialized error shape returned by bridge commands after phase 3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BridgeError {
    pub code: BridgeErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl BridgeError {
    pub fn new(code: BridgeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
        }
    }

    pub fn untyped(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::Unknown, message)
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for BridgeError {}

impl From<BridgeError> for String {
    fn from(value: BridgeError) -> Self {
        value.message
    }
}

impl From<String> for BridgeError {
    fn from(message: String) -> Self {
        Self::untyped(message)
    }
}

impl From<&str> for BridgeError {
    fn from(message: &str) -> Self {
        Self::untyped(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_error_serializes_camel_case() {
        let err = BridgeError::new(BridgeErrorCode::StalePreview, "preview drifted");
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(json["code"], "stalePreview");
        assert_eq!(json["retryable"], true);
        assert_eq!(json["message"], "preview drifted");
    }
}
