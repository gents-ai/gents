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
    /// Bearer pairing / peer-directory pairing family failures.
    Pairing,
    /// Catch-all for uncategorized legacy string errors during migration.
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

    pub fn classify_legacy_message(message: &str) -> Self {
        fn contains_pairing_word(message: &str) -> bool {
            message
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|word| matches!(word, "pair" | "paired" | "pairing"))
        }

        let lower = message.to_ascii_lowercase();
        if lower.contains("desktop client is not running")
            || lower.contains("desktop bridge not initialized")
            || lower.contains("desktop bridge core not initialized")
            || lower.contains("desktop client not initialized")
            || lower.contains("desktop bridge has not finished bootstrapping")
        {
            return Self::ClientNotRunning;
        }
        if lower.contains("startup thread panicked") || lower.contains("client start") {
            return Self::ClientStartFailed;
        }
        if lower.contains("was not found")
            || lower.contains("not found")
            || lower.contains("no online")
            || lower.contains("unknown tool")
        {
            return Self::NotFound;
        }
        if lower.contains("stale preview")
            || lower.contains("expectedpreviewsignature")
            || lower.contains("expected preview signature")
            || lower.contains("signature")
                && (lower.contains("mismatch")
                    || lower.contains("drift")
                    || lower.contains("missing"))
        {
            return Self::StalePreview;
        }
        if lower.contains("cascade depth exceeded") {
            return Self::CascadeDepthExceeded;
        }
        if lower.contains("path escapes") {
            return Self::PathEscapesRoot;
        }
        if lower.contains("not yet supported")
            || lower.contains("not supported")
            || lower.contains("unsupported")
        {
            return Self::Unsupported;
        }
        if lower.contains("sending get request to")
            || lower.contains("reading get request to")
            || lower.contains("connection refused")
            || lower.contains("timed out")
            || lower.contains("error sending request")
        {
            return Self::EndpointUnreachable;
        }
        if lower.contains("is required")
            || lower.contains("must be")
            || lower.contains("must not")
            || lower.contains("does not exist")
            || lower.contains("unrecognized")
        {
            return Self::InvalidArgument;
        }
        if contains_pairing_word(&lower)
            || lower.contains("bearer")
            || lower.contains("invite")
            || lower.contains("ticket")
            || lower.contains("network-admin")
            || lower.contains("network admin")
        {
            return Self::Pairing;
        }
        if lower.contains("graphql")
            || lower.contains("query returned")
            || lower.contains("mutation")
            || lower.contains("parsing ")
        {
            return Self::Backend;
        }
        Self::Unknown
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

    pub fn from_legacy_message(message: impl Into<String>) -> Self {
        let message = message.into();
        let code = BridgeErrorCode::classify_legacy_message(&message);
        Self::new(code, message)
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
        Self::from_legacy_message(message)
    }
}

impl From<&str> for BridgeError {
    fn from(message: &str) -> Self {
        Self::from_legacy_message(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_client_not_running() {
        assert_eq!(
            BridgeErrorCode::classify_legacy_message("desktop client is not running"),
            BridgeErrorCode::ClientNotRunning
        );
    }

    #[test]
    fn classifies_endpoint_urls_from_peer_errors() {
        assert_eq!(
            BridgeErrorCode::classify_legacy_message(
                "error sending GET request to http://127.0.0.1:9181/status"
            ),
            BridgeErrorCode::EndpointUnreachable
        );
    }

    #[test]
    fn classifies_required_fields() {
        assert_eq!(
            BridgeErrorCode::classify_legacy_message("content is required"),
            BridgeErrorCode::InvalidArgument
        );
    }

    #[test]
    fn classifies_not_found() {
        assert_eq!(
            BridgeErrorCode::classify_legacy_message("task demo was not found"),
            BridgeErrorCode::NotFound
        );
    }

    #[test]
    fn pairing_word_match_does_not_classify_repair_as_pairing() {
        assert_eq!(
            BridgeErrorCode::classify_legacy_message("P2P repair was requested"),
            BridgeErrorCode::Unknown
        );
        assert_eq!(
            BridgeErrorCode::classify_legacy_message("peer pairing failed"),
            BridgeErrorCode::Pairing
        );
    }

    #[test]
    fn bridge_error_serializes_camel_case() {
        let err = BridgeError::new(BridgeErrorCode::StalePreview, "preview drifted");
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(json["code"], "stalePreview");
        assert_eq!(json["retryable"], true);
        assert_eq!(json["message"], "preview drifted");
    }
}
