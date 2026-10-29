use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum BridgeErrorCode {
    ClientNotRunning,
    ClientStartFailed,
    LocalHistoryLocked,
    /// Unlock the existing managed-runtime keys through an explicit user action.
    RuntimeKeyAccess,
    /// Storage recovery preserves retained history and unresolved submission identity.
    StorageFull,
    /// The read could not publish against the current observation fence.
    ProjectionPending,
    /// A verified runtime Goal-control rejection, never transport or message classification.
    GoalControlRejected,
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
            Self::RuntimeKeyAccess => "runtimeKeyAccess",
            Self::StorageFull => "storageFull",
            Self::ProjectionPending => "projectionPending",
            Self::GoalControlRejected => "goalControlRejected",
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BridgeError {
    pub code: BridgeErrorCode,
    pub message: String,
    pub retryable: bool,
    pub endpoint: Option<String>,
    #[serde(skip)]
    #[ts(skip)]
    transport_source: Option<std::sync::Arc<reqwest::Error>>,
}

impl PartialEq for BridgeError {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code
            && self.message == other.message
            && self.retryable == other.retryable
            && self.endpoint == other.endpoint
    }
}
impl Eq for BridgeError {}

impl BridgeError {
    pub fn new(code: BridgeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            endpoint: None,
            transport_source: None,
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

    pub fn storage_full() -> Self {
        Self::new(BridgeErrorCode::StorageFull,
            "Storage is full. Free disk space, then restart Gents to reopen the same store. Your saved chats and pending message are kept.")
    }

    pub fn classify_transport_error(error: anyhow::Error) -> Self {
        if gents::graphql::is_storage_full(error.as_ref()) {
            return Self::storage_full();
        }
        if let Some(classified) = error.chain().find_map(|cause| cause.downcast_ref::<Self>()) {
            return classified.clone();
        }
        let transport = gents::config_client::TransactionFailure::config_client_error(&error)
            .and_then(gents::config_client::ConfigClientError::transport_error)
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

    pub(crate) fn runtime_http_error(error: reqwest::Error, message: String) -> anyhow::Error {
        if error.is_connect() || error.is_timeout() || error.is_body() {
            let mut bridge = match error.url() {
                Some(url) => {
                    Self::endpoint_unreachable(url.origin().ascii_serialization(), message.clone())
                }
                None => Self::new(BridgeErrorCode::EndpointUnreachable, message.clone()),
            };
            bridge.transport_source = Some(std::sync::Arc::new(error));
            anyhow::Error::from(bridge).context(message)
        } else {
            anyhow::Error::from(error).context(message)
        }
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for BridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.transport_source.as_deref().map(|source| source as _)
    }
}

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
        assert!(error.chain().any(|cause| cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_connect)));
        let err = BridgeError::classify_transport_error(error);
        assert_eq!(err.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(err.endpoint.as_deref(), Some(endpoint.as_str()));
    }

    #[tokio::test]
    async fn classify_config_transaction_transport_uses_typed_access_source() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let access =
            gents::config_client::ConfigAccess::graphql(format!("{endpoint}/api/v0/graphql"));
        let error = access
            .transact_readonly("test.desktop_transport", |_txn| {
                Box::pin(async { Ok::<_, anyhow::Error>(()) })
            })
            .await
            .unwrap_err();
        let error = BridgeError::classify_transport_error(error);
        assert_eq!(error.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(error.endpoint.as_deref(), Some(endpoint.as_str()));
    }

    #[tokio::test]
    async fn classify_config_query_transport_uses_typed_protocol_source() {
        let endpoint =
            gents::config_client::GraphqlEndpoint::anonymous("http://127.0.0.1:0/api/v0/graphql");
        let result = gents::config_client::query_graphql_with_options(
            &endpoint,
            "{__typename}",
            gents_protocol::graphql::GraphqlRequestOptions {
                timeout: std::time::Duration::from_secs(5),
                max_attempts: 1,
                retry_backoff: std::time::Duration::ZERO,
            },
        )
        .await
        .unwrap_err();
        let error = BridgeError::classify_transport_error(result.into());
        assert_eq!(error.code, BridgeErrorCode::EndpointUnreachable);
        assert_eq!(error.endpoint.as_deref(), Some("http://127.0.0.1:0"));
    }

    #[test]
    fn typed_bridge_context_survives_transport_classification() {
        let bridge = BridgeError::new(
            BridgeErrorCode::ProjectionPending,
            "waiting for the exact row",
        );
        let error = anyhow::Error::from(bridge.clone()).context("row not replicated");
        assert_eq!(BridgeError::classify_transport_error(error), bridge);
    }

    #[test]
    fn wrapped_bridge_error_keeps_its_code_and_endpoint() {
        #[derive(Debug, thiserror::Error)]
        #[error("outer operation")]
        struct Wrapped(#[source] BridgeError);
        let bridge = BridgeError::endpoint_unreachable("http://127.0.0.1:9191", "not reachable");
        let error = anyhow::Error::new(Wrapped(bridge.clone())).context("caller");
        assert_eq!(BridgeError::classify_transport_error(error), bridge);
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

#[cfg(test)]
mod storage_failure_tests {
    use super::*;

    #[test]
    fn full_storage_is_actionable_and_does_not_authorize_retry() {
        let error = anyhow::Error::new(gents_protocol::database_error::StorageFull)
            .context("a database operation");
        let error = BridgeError::classify_transport_error(error);
        assert_eq!(error.code, BridgeErrorCode::StorageFull);
        assert!(!error.retryable);
        assert!(error.message.contains("Free disk space"));
        assert!(error.message.contains("pending message"));
    }
}
