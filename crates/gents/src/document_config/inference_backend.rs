use serde::{Deserialize, Serialize};

use crate::backend_provider::BackendProviderKind;
use crate::config::ReasoningEffort;
use crate::openai_wire::OpenAiWireApi;
pub use gents_loop::claude_platform::ClaudePlatform;

/// Shared provider connection. Authentication must be compatible with the
/// provider adapter; OAuth credentials resolve through the invoking principal's DID.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceBackend {
    pub node_did: String,
    pub backend_id: String,
    pub name: String,
    pub provider_kind: BackendProviderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub openai_wire_api: Option<OpenAiWireApi>,
    pub endpoint: String,
    /// Missing/invalid credentials never fall back to unauthenticated access.
    pub auth: BackendAuth,
    /// Where a Claude platform provider kind is served from; `endpoint` stays
    /// the base URL and defaults from these settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub claude_platform: Option<ClaudePlatform>,
    /// Connection establishment only; default 10s, bounded by the operation deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub connect_timeout_secs: Option<i64>,
    /// Total model-discovery request timeout; default 10s.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub discovery_timeout_secs: Option<i64>,
    /// Shared backend capacity across profiles; absent resolves to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional = nullable))]
    pub max_concurrent: Option<i64>,
    /// Absent resolves to 100; zero disables queueing.
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
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[cfg_attr(feature = "typescript", ts(as = "Option<Vec<String>>", optional = nullable))]
    pub tags: Vec<String>,
}

/// Backend-owned observation projection, excluded from desired config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceBackendObservation {
    pub backend_id: String,
    #[serde(default, deserialize_with = "deserialize_backend_catalogs")]
    pub catalogs: Vec<BackendModelCatalog>,
    /// Runtime-owned; configuration apply must not overwrite it.
    pub probe_status: Option<String>,
    pub last_probe: Option<String>,
}

fn deserialize_backend_catalogs<'de, D>(
    deserializer: D,
) -> Result<Vec<BackendModelCatalog>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredCatalogs {
        Legacy(Vec<BackendModelCatalog>),
        Envelope { entries: Vec<BackendModelCatalog> },
    }

    Ok(match Option::<StoredCatalogs>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(StoredCatalogs::Legacy(catalogs)) => catalogs,
        Some(StoredCatalogs::Envelope { entries }) => entries,
    })
}

/// Authentication selection on a backend, not a second credential store.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum BackendAuth {
    /// Deliberately unauthenticated endpoint, such as a local model server.
    Unauthenticated,
    /// Operator-managed API key stored in the backend document under DefraDB ACP.
    ApiKey { key: String },
    /// Key from the runtime host's environment; missing/blank is an error.
    Environment { variable: String },
    /// Resolve the existing OAuthCredential using the node's DID; existing
    /// login, refresh, expiry, and credential ownership rules remain
    /// authoritative. `account_ref` is opaque and DID-free; absent means the
    /// provider's original account.
    #[serde(rename = "node_oauth")]
    NodeOAuth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional))]
        account_ref: Option<String>,
    },
    /// Rotate every call over all of the node's enabled OAuthCredential rows
    /// for the backend's provider (Lean `nodeOAuthPool`, `AccountRotation`).
    /// `allow_overage` lets a call draw paid extra usage once every account
    /// is past its limit; it is off unless the operator turns it on.
    #[serde(rename = "node_oauth_pool")]
    NodeOAuthPool {
        #[serde(default)]
        allow_overage: bool,
    },
    /// Static AWS credentials stored in the backend document under DefraDB ACP,
    /// for the AWS Claude platforms. Secret like an API key: a self-configuration
    /// patch can keep them but never set them.
    AwsCredentials {
        access_key_id: String,
        secret_access_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional))]
        session_token: Option<String>,
    },
    /// A Google service account or authorized-user credentials JSON stored in
    /// the backend document under DefraDB ACP, for the Google Claude platforms.
    /// Secret like an API key.
    GoogleCredentials { credentials_json: String },
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
            Self::NodeOAuth { account_ref } => f
                .debug_struct("NodeOAuth")
                .field("account_ref", account_ref)
                .finish(),
            Self::NodeOAuthPool { allow_overage } => f
                .debug_struct("NodeOAuthPool")
                .field("allow_overage", allow_overage)
                .finish(),
            Self::AwsCredentials { access_key_id, .. } => f
                .debug_struct("AwsCredentials")
                .field("access_key_id", access_key_id)
                .field("secret_access_key", &"[redacted]")
                .field("session_token", &"[redacted]")
                .finish(),
            Self::GoogleCredentials { .. } => f
                .debug_struct("GoogleCredentials")
                .field("credentials_json", &"[redacted]")
                .finish(),
        }
    }
}

impl BackendAuth {
    /// Credentials come from the executing node's OAuthCredential rows
    /// (Lean `oauthLookupOwner` is the execution scope).
    pub fn uses_node_oauth(&self) -> bool {
        matches!(self, Self::NodeOAuth { .. } | Self::NodeOAuthPool { .. })
    }

    /// How a node OAuth backend resolves one account for readiness, discovery
    /// and resume: its named account, or for a pool the earliest-connected
    /// enabled account, so a pool resolves while any of its accounts does.
    pub fn account_pick(&self) -> Option<crate::oauth_credential::AccountPick<'_>> {
        match self {
            Self::NodeOAuth { account_ref } => Some(
                crate::oauth_credential::AccountPick::Reference(account_ref.as_deref()),
            ),
            Self::NodeOAuthPool { .. } => {
                Some(crate::oauth_credential::AccountPick::ProviderDefault)
            }
            _ => None,
        }
    }

    /// Whether this credential form can authenticate `kind`. The Claude
    /// platforms take a static key or token, or their cloud's credentials; a
    /// cloud credential is refused anywhere else. Other kinds keep their own
    /// rules (node OAuth is checked against `is_node_scoped_oauth`).
    pub fn suits_provider(&self, kind: BackendProviderKind) -> bool {
        use BackendProviderKind as Kind;
        match self {
            Self::AwsCredentials { .. } => {
                matches!(
                    kind,
                    Kind::ClaudeBedrock | Kind::ClaudeMantle | Kind::ClaudeAnthropicAws
                )
            }
            Self::GoogleCredentials { .. } => {
                matches!(kind, Kind::ClaudeVertex | Kind::ClaudeGoogleCloud)
            }
            Self::Unauthenticated => !kind.is_claude_platform(),
            _ => true,
        }
    }

    /// A raw secret stored in the backend document: the self-configuration
    /// guard lets a patch keep it but never set or change it (Lean
    /// `authPatchAllowed`).
    pub fn is_stored_secret(&self) -> bool {
        matches!(
            self,
            Self::ApiKey { .. } | Self::AwsCredentials { .. } | Self::GoogleCredentials { .. }
        )
    }

    /// Whether a pool may draw paid extra usage; `None` for any other auth.
    pub fn pool_allow_overage(&self) -> Option<bool> {
        match self {
            Self::NodeOAuthPool { allow_overage } => Some(*allow_overage),
            _ => None,
        }
    }

    /// The account a node OAuth backend names (Lean `oauthRef`); a pool names none.
    pub fn oauth_account_ref(&self) -> Option<&str> {
        match self {
            Self::NodeOAuth { account_ref } => account_ref.as_deref(),
            _ => None,
        }
    }
}

/// One backend catalog observed under a particular authentication scope; a
/// node's catalog must not be treated as another node's entitlement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendModelCatalog {
    /// None: the backend's shared credential scope; Some: the node whose
    /// OAuthCredential was used. Never contains secrets.
    pub node_did: Option<String>,
    /// Latest successful discovery in public observations. The stored payload
    /// retains its content timestamp; the observation owner overlays the
    /// independently persisted freshness watermark.
    pub observed_at: String,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    pub models: Vec<AdvertisedModel>,
}

/// Server-advertised model option, scoped by its containing backend catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct AdvertisedModel {
    pub model_name: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    /// Provider-advertised ceiling for an explicit context override; must
    /// never become a default itself.
    pub max_context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    /// None unknown; Some(empty) means no configurable reasoning effort.
    pub reasoning_efforts: Option<Vec<ReasoningEffort>>,
}
