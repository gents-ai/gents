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
    pub(crate) fn endpoint_unreachable(
        endpoint: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
            ..Self::new(BridgeErrorCode::EndpointUnreachable, message)
        }
    }

    /// Classify a lower-level transport failure from its typed error chain.
    /// Connection, timeout, and response-body transport failures become
    /// `EndpointUnreachable`, with the endpoint taken from the underlying
    /// `reqwest::Error`; anything else (a non-2xx response, a JSON decode
    /// failure, or unrelated prose containing words such as "timeout") stays
    /// untyped. Single owner for the classification shared by
    /// `tauri_commands::peers::desktop_peer_status_fetch` and the
    /// local-runtime discovery path in `tauri_commands::lifecycle`, so
    /// neither call site — nor the TS client — needs its own copy (#1339).
    pub(crate) fn classify_transport_error(error: anyhow::Error) -> Self {
        let transport = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .filter(|error| error.is_connect() || error.is_timeout() || error.is_body());
        let message = error.to_string();
        let Some(transport) = transport else {
            return Self::untyped(message);
        };
        match transport.url() {
            Some(url) => Self::endpoint_unreachable(url.origin().ascii_serialization(), message),
            None => Self::new(BridgeErrorCode::EndpointUnreachable, message),
        }
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

    #[tokio::test]
    async fn classify_transport_error_uses_reqwest_source_and_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind temporary port");
        let endpoint = format!("http://{}", listener.local_addr().expect("local address"));
        drop(listener);

        let error = gents_desktop_core::local_runtime::fetch_runtime_connection_payload(&endpoint)
            .await
            .expect_err("closed temporary port must refuse the request");
        let err = BridgeError::classify_transport_error(error);
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(err.endpoint.as_deref(), Some(endpoint.as_str()));
    }

    #[test]
    fn classify_transport_error_leaves_application_level_failures_untyped() {
        let err = BridgeError::classify_transport_error(anyhow::anyhow!(
            "GET http://127.0.0.1:9181/status failed with 504 Gateway Timeout: request refused"
        ));
        assert_eq!(err.code, BridgeErrorCode::Unknown);
        assert_eq!(err.endpoint, None);

        let err = BridgeError::classify_transport_error(anyhow::anyhow!(
            "decoding JSON response from http://127.0.0.1:9181/status"
        ));
        assert_eq!(err.code, BridgeErrorCode::Unknown);
        assert_eq!(err.endpoint, None);
    }

    /// `tauri_commands::peers::desktop_peer_enroll_status` and
    /// `desktop_peer_status_fetch` both map a `fetch_runtime_connection_payload`
    /// failure through `classify_transport_error`. A display string alone is
    /// never sufficient evidence of an unreachable endpoint (#1339).
    #[test]
    fn classify_transport_error_rejects_message_only_transport_lookalikes() {
        let lookalike = BridgeError::classify_transport_error(anyhow::anyhow!(
            "no gents server found at http://198.51.100.7:9191/status: connection refused"
        ));
        assert_eq!(lookalike.code, BridgeErrorCode::Unknown);
        assert_eq!(lookalike.endpoint, None);
    }
}
