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
    /// The origin/address that could not be reached, for
    /// `EndpointUnreachable` errors. `None` for every other code. Structured
    /// so callers don't need to regex it back out of `message` (#1339).
    pub endpoint: Option<String>,
}

impl BridgeError {
    pub fn new(code: BridgeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            endpoint: None,
        }
    }

    pub fn untyped(message: impl Into<String>) -> Self {
        Self::new(BridgeErrorCode::Unknown, message)
    }

    /// An `EndpointUnreachable` error carrying the endpoint that could not be
    /// reached as a structured field.
    pub fn endpoint_unreachable(endpoint: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
            ..Self::new(BridgeErrorCode::EndpointUnreachable, message)
        }
    }

    /// Classify a lower-level transport failure message. Connection
    /// failures (refused, timed out, DNS, or a failed send/read) become
    /// `EndpointUnreachable` with the endpoint recovered from the message
    /// when one is present; anything else (a non-2xx response, a JSON
    /// decode failure, …) stays untyped, exactly as before. Single owner
    /// for the classification shared by
    /// `tauri_commands::peers::desktop_peer_status_fetch` and the
    /// local-runtime discovery path in `tauri_commands::lifecycle`, so
    /// neither call site — nor the TS client — needs its own copy (#1339).
    pub fn classify_transport_error(message: impl Into<String>) -> Self {
        let message = message.into();
        if !looks_like_unreachable_endpoint(&message) {
            return Self::untyped(message);
        }
        match transport_endpoint_from_message(&message) {
            Some(endpoint) => Self::endpoint_unreachable(endpoint, message),
            None => Self::new(BridgeErrorCode::EndpointUnreachable, message),
        }
    }
}

/// True when `message` describes a connection-level transport failure
/// (refused, timed out, DNS, or a failed send/read) rather than an
/// application-level failure (a non-2xx response, a JSON decode error, …)
/// from an endpoint that was in fact reachable.
fn looks_like_unreachable_endpoint(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("connection refused")
        || lower.contains("refused")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("no gents server found at")
        || lower.contains("sending get request to")
        || lower.contains("sending post request to")
        || lower.contains("sending delete request to")
        || lower.contains("reading get response body from")
        || lower.contains("error trying to connect")
        || lower.contains("dns error")
}

/// Best-effort recovery of the origin a transport-failure message names
/// (e.g. "...sending GET request to http://127.0.0.1:9181/status" ->
/// "http://127.0.0.1:9181"). `None` when the message names no URL.
fn transport_endpoint_from_message(message: &str) -> Option<String> {
    let start = message
        .find("http://")
        .or_else(|| message.find("https://"))?;
    let candidate = &message[start..];
    // Capture the whole non-whitespace token first (a host/IP legitimately
    // contains '.'), then trim only trailing punctuation the token picked up
    // from surrounding prose (e.g. a sentence-ending period).
    let end = candidate
        .find(char::is_whitespace)
        .unwrap_or(candidate.len());
    let token = candidate[..end].trim_end_matches(['.', ',', ';', ')', ']']);
    reqwest::Url::parse(token)
        .ok()
        .map(|parsed| parsed.origin().ascii_serialization())
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
        assert!(json["endpoint"].is_null());
    }

    #[test]
    fn endpoint_unreachable_carries_the_endpoint_as_a_structured_field() {
        let err = BridgeError::endpoint_unreachable(
            "http://127.0.0.1:9181",
            "sending GET request to http://127.0.0.1:9181/api/v0/p2p/shareable-address",
        );
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
        assert!(err.retryable);
        assert_eq!(err.endpoint.as_deref(), Some("http://127.0.0.1:9181"));
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(json["code"], "endpointUnreachable");
        assert_eq!(json["endpoint"], "http://127.0.0.1:9181");
    }

    #[test]
    fn classify_transport_error_flags_connection_failures_with_endpoint() {
        let err = BridgeError::classify_transport_error(
            "sending GET request to http://127.0.0.1:9181/api/v0/p2p/shareable-address",
        );
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(err.endpoint.as_deref(), Some("http://127.0.0.1:9181"));

        let err = BridgeError::classify_transport_error(
            "no gents server found at http://127.0.0.1:9191/status. Start one first with              `gents server` or `gents demo`.",
        );
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(err.endpoint.as_deref(), Some("http://127.0.0.1:9191"));

        let err = BridgeError::classify_transport_error("connection refused (os error 61)");
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(err.endpoint, None);

        let err = BridgeError::classify_transport_error("request timed out after 5s");
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
    }

    #[test]
    fn classify_transport_error_leaves_application_level_failures_untyped() {
        let err = BridgeError::classify_transport_error(
            "GET http://127.0.0.1:9181/status failed with 500 Internal Server Error: boom",
        );
        assert_eq!(err.code, BridgeErrorCode::Unknown);
        assert_eq!(err.endpoint, None);

        let err = BridgeError::classify_transport_error(
            "decoding JSON response from http://127.0.0.1:9181/status",
        );
        assert_eq!(err.code, BridgeErrorCode::Unknown);
        assert_eq!(err.endpoint, None);
    }
}
