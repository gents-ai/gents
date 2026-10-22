use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum BridgeErrorCode {
    ClientNotRunning,
    ClientStartFailed,
    LocalHistoryLocked,
    /// The read could not publish against the current observation fence.
    ProjectionPending,
    NotFound,
    InvalidArgument,
    Unsupported,
    EndpointUnreachable,
    PathEscapesRoot,
    Backend,
    IncompatibleLocalStore,
    Pairing,
    CredentialNotSaved,
    RuntimeStillBooting,
    /// The session belongs to another requester, so core would refuse the send.
    SessionNotOwned,
    Unknown,
}

impl BridgeErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientNotRunning => "clientNotRunning",
            Self::ClientStartFailed => "clientStartFailed",
            Self::LocalHistoryLocked => "localHistoryLocked",
            Self::ProjectionPending => "projectionPending",
            Self::NotFound => "notFound",
            Self::InvalidArgument => "invalidArgument",
            Self::Unsupported => "unsupported",
            Self::EndpointUnreachable => "endpointUnreachable",
            Self::PathEscapesRoot => "pathEscapesRoot",
            Self::Backend => "backend",
            Self::IncompatibleLocalStore => "incompatibleLocalStore",
            Self::Pairing => "pairing",
            Self::CredentialNotSaved => "credentialNotSaved",
            Self::RuntimeStillBooting => "runtimeStillBooting",
            Self::SessionNotOwned => "sessionNotOwned",
            Self::Unknown => "unknown",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::ProjectionPending
                | Self::ClientStartFailed
                | Self::EndpointUnreachable
                | Self::Backend
                | Self::Pairing
                | Self::CredentialNotSaved
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BridgeError {
    pub code: BridgeErrorCode,
    pub message: String,
    pub retryable: bool,
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

    pub fn endpoint_unreachable(endpoint: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
            ..Self::new(BridgeErrorCode::EndpointUnreachable, message)
        }
    }

    pub fn classify_transport_error(error: anyhow::Error) -> Self {
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
    fn an_unpublished_read_fence_remains_a_retryable_observation() {
        let error = BridgeError::new(BridgeErrorCode::ProjectionPending, "read fence changed");
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["code"], "projectionPending");
        assert_eq!(value["retryable"], true);
        assert!(value["endpoint"].is_null());
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

        let error = crate::local_runtime::fetch_runtime_connection_payload(&endpoint)
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

    #[test]
    fn classify_transport_error_rejects_message_only_transport_lookalikes() {
        let lookalike = BridgeError::classify_transport_error(anyhow::anyhow!(
            "no gents server found at http://198.51.100.7:9191/status: connection refused"
        ));
        assert_eq!(lookalike.code, BridgeErrorCode::Unknown);
        assert_eq!(lookalike.endpoint, None);
    }
}
