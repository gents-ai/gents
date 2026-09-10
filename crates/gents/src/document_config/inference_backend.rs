use serde::{Deserialize, Serialize};

use crate::backend_provider::BackendProviderKind;
use crate::config::ReasoningEffort;
use crate::openai_wire::OpenAiWireApi;

/// Shared provider connection. Model and generation choices live in profiles.
/// Authentication is explicit and must be compatible with the provider adapter.
/// OAuth credentials remain resolved through the invoking principal's DID.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceBackend {
    pub agent_did: String,
    pub backend_id: String,
    pub name: String,
    pub provider_kind: BackendProviderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub openai_wire_api: Option<OpenAiWireApi>,
    pub endpoint: String,
    /// Required explicit choice; missing or invalid credentials never fall back
    /// to unauthenticated access. The same selection governs discovery and calls.
    pub auth: BackendAuth,
    /// Connection establishment only, not the total streaming response duration.
    /// Default 10s, bounded by the active operation's deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub connect_timeout_secs: Option<i64>,
    /// Total model-discovery request timeout. Default 10s for all discovery callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub discovery_timeout_secs: Option<i64>,
    /// Shared backend capacity across all profiles and models using this connection.
    /// Absent/null resolves to 1; an explicit value must be positive.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_concurrent: Option<i64>,
    /// Absent/null resolves to 100; zero disables queueing, negatives are invalid.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_queue_depth: Option<i64>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled",
        skip_serializing_if = "super::serde_helpers::is_enabled"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional = nullable))]
    pub enabled: bool,
    /// Optional UI/discovery labels. References, never tags, determine execution.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Backend-owned observations exposed by the server, excluded from desired config.
/// This is an observation projection, not a separately configured model registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceBackendObservation {
    pub backend_id: String,
    /// Runtime-owned catalog observations exposed by the server. Entries are
    /// embedded advertisements, not independently addressable model documents.
    /// Discovery never creates profiles or changes their selected model/effort.
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    pub catalogs: Vec<BackendModelCatalog>,
    /// Runtime-owned observation; configuration apply must not overwrite it.
    pub probe_status: Option<String>,
    pub last_probe: Option<String>,
}

/// Authentication selection on a backend, not a second credential store.
/// A tagged value prevents competing raw-key and environment-key settings.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum BackendAuth {
    /// Deliberately unauthenticated endpoint, such as a local model server.
    Unauthenticated,
    /// Operator-managed API key stored in the backend document under DefraDB ACP.
    ApiKey { key: String },
    /// Read the key from the runtime host's environment. Missing/blank is an error.
    Environment { variable: String },
    /// Resolve the existing OAuthCredential using the invoking principal's DID
    /// and the provider adapter's OAuth provider key. Existing login, refresh,
    /// expiry, and credential ownership rules remain authoritative. No tokens
    /// or fixed principal DID are copied into the shared backend.
    #[serde(rename = "principal_o_auth")]
    PrincipalOAuth,
}

impl std::fmt::Debug for BackendAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthenticated => f.write_str("Unauthenticated"),
            Self::ApiKey { .. } => f
                .debug_struct("ApiKey")
                .field("key", &"[redacted]")
                .finish(),
            Self::Environment { variable } => f
                .debug_struct("Environment")
                .field("variable", variable)
                .finish(),
            Self::PrincipalOAuth => f.write_str("PrincipalOAuth"),
        }
    }
}

/// One backend catalog observed under a particular authentication scope.
/// A principal's catalog must not be treated as another principal's entitlement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendModelCatalog {
    /// None denotes the backend's shared credential scope; Some identifies the
    /// principal whose existing OAuthCredential was used. Never contains secrets.
    pub agent_did: Option<String>,
    /// Successful observation time. A failed refresh must not erase a prior
    /// catalog or claim it was freshly observed. Health remains separately owned.
    pub observed_at: String,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    pub models: Vec<AdvertisedModel>,
}

/// Server-advertised model option, scoped by its containing backend catalog.
/// Optional capabilities remain unknown unless discovery or an explicit adapter
/// contract establishes them. No separate document identity or model profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvertisedModel {
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    /// None means unknown; Some(empty) means no configurable reasoning effort.
    /// The server advertises these choices without materializing effort profiles.
    pub reasoning_efforts: Option<Vec<ReasoningEffort>>,
}
