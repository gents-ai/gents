use serde::{Deserialize, Serialize};

use crate::inference::backends::provider::BackendProviderKind;
use crate::inference::resolved::ReasoningEffort;
use crate::openai_wire::OpenAiWireApi;
pub use gents_loop::claude_platform::ClaudePlatform;

/// Secret-free authentication selection stored in InferenceBackendMetadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendAuthSelection {
    Unauthenticated,
    Credential { id: String, version: String },
    Environment { variable: String },
    NodeOAuth { account_ref: Option<String> },
    NodeOAuthPool { allow_overage: bool },
}

/// Scalar wire fields prevent direct writers from storing an inline auth JSON
/// object in the Agent-readable metadata collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendAuthSelectionFields {
    pub auth_kind: String,
    pub credential_id: Option<String>,
    pub credential_version: Option<String>,
    pub environment_variable: Option<String>,
    pub oauth_account_ref: Option<String>,
    pub oauth_allow_overage: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid backend authentication selection")]
pub struct InvalidBackendAuthSelection;

impl TryFrom<BackendAuthSelectionFields> for BackendAuthSelection {
    type Error = InvalidBackendAuthSelection;

    fn try_from(fields: BackendAuthSelectionFields) -> Result<Self, Self::Error> {
        let BackendAuthSelectionFields {
            auth_kind,
            credential_id,
            credential_version,
            environment_variable,
            oauth_account_ref,
            oauth_allow_overage,
        } = fields;
        match (
            auth_kind.as_str(),
            credential_id,
            credential_version,
            environment_variable,
            oauth_account_ref,
            oauth_allow_overage,
        ) {
            ("unauthenticated", None, None, None, None, None) => Ok(Self::Unauthenticated),
            ("credential", Some(id), Some(version), None, None, None)
                if !id.is_empty() && !version.is_empty() =>
            {
                Ok(Self::Credential { id, version })
            }
            ("environment", None, None, Some(variable), None, None) if !variable.is_empty() => {
                Ok(Self::Environment { variable })
            }
            ("node_oauth", None, None, None, account_ref, None)
                if account_ref.as_ref().is_none_or(|value| !value.is_empty()) =>
            {
                Ok(Self::NodeOAuth { account_ref })
            }
            ("node_oauth_pool", None, None, None, None, Some(allow_overage)) => {
                Ok(Self::NodeOAuthPool { allow_overage })
            }
            _ => Err(InvalidBackendAuthSelection),
        }
    }
}

impl From<BackendAuthSelection> for BackendAuthSelectionFields {
    fn from(selection: BackendAuthSelection) -> Self {
        let mut fields = Self {
            auth_kind: String::new(),
            credential_id: None,
            credential_version: None,
            environment_variable: None,
            oauth_account_ref: None,
            oauth_allow_overage: None,
        };
        match selection {
            BackendAuthSelection::Unauthenticated => fields.auth_kind = "unauthenticated".into(),
            BackendAuthSelection::Credential { id, version } => {
                fields.auth_kind = "credential".into();
                fields.credential_id = Some(id);
                fields.credential_version = Some(version);
            }
            BackendAuthSelection::Environment { variable } => {
                fields.auth_kind = "environment".into();
                fields.environment_variable = Some(variable);
            }
            BackendAuthSelection::NodeOAuth { account_ref } => {
                fields.auth_kind = "node_oauth".into();
                fields.oauth_account_ref = account_ref;
            }
            BackendAuthSelection::NodeOAuthPool { allow_overage } => {
                fields.auth_kind = "node_oauth_pool".into();
                fields.oauth_allow_overage = Some(allow_overage);
            }
        }
        fields
    }
}

/// Canonical fresh backend configuration; no raw credential is a writable field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceBackendMetadata {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub physical_doc_id: Option<String>,
    pub node_did: String,
    pub backend_id: String,
    pub name: String,
    pub provider_kind: BackendProviderKind,
    pub openai_wire_api: Option<OpenAiWireApi>,
    pub endpoint: String,
    pub auth_kind: String,
    pub credential_id: Option<String>,
    pub credential_version: Option<String>,
    pub environment_variable: Option<String>,
    pub oauth_account_ref: Option<String>,
    pub oauth_allow_overage: Option<bool>,
    pub claude_platform: Option<ClaudePlatform>,
    pub connect_timeout_secs: Option<i64>,
    pub discovery_timeout_secs: Option<i64>,
    pub max_concurrent: Option<i64>,
    pub max_queue_depth: Option<i64>,
    #[serde(
        default = "super::serde_helpers::default_enabled",
        deserialize_with = "super::serde_helpers::deserialize_enabled"
    )]
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_default_on_null"
    )]
    pub tags: Vec<String>,
}

impl InferenceBackendMetadata {
    /// A raw authoring credential may only become an opaque reference. The
    /// credential record and this metadata must be committed together.
    pub fn from_backend(
        backend: InferenceBackend,
        selection: BackendAuthSelection,
    ) -> Result<Self, InvalidBackendAuthSelection> {
        let consistent = match (&backend.auth, &selection) {
            (BackendAuth::Unauthenticated, BackendAuthSelection::Unauthenticated) => true,
            (
                BackendAuth::Credential { id: a, version: av },
                BackendAuthSelection::Credential { id: b, version: bv },
            ) => a == b && av == bv && !av.is_empty(),
            (
                BackendAuth::Environment { variable: a },
                BackendAuthSelection::Environment { variable: b },
            ) => a == b,
            (
                BackendAuth::NodeOAuth { account_ref: a },
                BackendAuthSelection::NodeOAuth { account_ref: b },
            ) => a == b,
            (
                BackendAuth::NodeOAuthPool { allow_overage: a },
                BackendAuthSelection::NodeOAuthPool { allow_overage: b },
            ) => a == b,
            (
                BackendAuth::ApiKey { .. }
                | BackendAuth::AwsCredentials { .. }
                | BackendAuth::GoogleCredentials { .. },
                BackendAuthSelection::Credential { .. },
            ) => true,
            _ => false,
        };
        if !consistent {
            return Err(InvalidBackendAuthSelection);
        }
        let fields = BackendAuthSelectionFields::from(selection);
        BackendAuthSelection::try_from(fields.clone())?;
        Ok(Self {
            physical_doc_id: None,
            node_did: backend.node_did,
            backend_id: backend.backend_id,
            name: backend.name,
            provider_kind: backend.provider_kind,
            openai_wire_api: backend.openai_wire_api,
            endpoint: backend.endpoint,
            auth_kind: fields.auth_kind,
            credential_id: fields.credential_id,
            credential_version: fields.credential_version,
            environment_variable: fields.environment_variable,
            oauth_account_ref: fields.oauth_account_ref,
            oauth_allow_overage: fields.oauth_allow_overage,
            claude_platform: backend.claude_platform,
            connect_timeout_secs: backend.connect_timeout_secs,
            discovery_timeout_secs: backend.discovery_timeout_secs,
            max_concurrent: backend.max_concurrent,
            max_queue_depth: backend.max_queue_depth,
            enabled: backend.enabled,
            tags: backend.tags,
        })
    }

    pub fn auth_selection(&self) -> Result<BackendAuthSelection, InvalidBackendAuthSelection> {
        BackendAuthSelectionFields {
            auth_kind: self.auth_kind.clone(),
            credential_id: self.credential_id.clone(),
            credential_version: self.credential_version.clone(),
            environment_variable: self.environment_variable.clone(),
            oauth_account_ref: self.oauth_account_ref.clone(),
            oauth_allow_overage: self.oauth_allow_overage,
        }
        .try_into()
    }

    pub fn into_backend(self) -> Result<InferenceBackend, InvalidBackendAuthSelection> {
        let auth = match self.auth_selection()? {
            BackendAuthSelection::Unauthenticated => BackendAuth::Unauthenticated,
            BackendAuthSelection::Credential { id, version } => {
                BackendAuth::Credential { id, version }
            }
            BackendAuthSelection::Environment { variable } => BackendAuth::Environment { variable },
            BackendAuthSelection::NodeOAuth { account_ref } => {
                BackendAuth::NodeOAuth { account_ref }
            }
            BackendAuthSelection::NodeOAuthPool { allow_overage } => {
                BackendAuth::NodeOAuthPool { allow_overage }
            }
        };
        Ok(InferenceBackend {
            physical_doc_id: self.physical_doc_id,
            node_did: self.node_did,
            backend_id: self.backend_id,
            name: self.name,
            provider_kind: self.provider_kind,
            openai_wire_api: self.openai_wire_api,
            endpoint: self.endpoint,
            auth,
            claude_platform: self.claude_platform,
            connect_timeout_secs: self.connect_timeout_secs,
            discovery_timeout_secs: self.discovery_timeout_secs,
            max_concurrent: self.max_concurrent,
            max_queue_depth: self.max_queue_depth,
            enabled: self.enabled,
            tags: self.tags,
        })
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn metadata_selection_rejects_mixed_or_missing_fields() {
        let selection = BackendAuthSelectionFields {
            auth_kind: "credential".into(),
            credential_id: Some("id".into()),
            credential_version: Some("version".into()),
            environment_variable: None,
            oauth_account_ref: None,
            oauth_allow_overage: None,
        };
        assert!(matches!(
            BackendAuthSelection::try_from(selection.clone()),
            Ok(BackendAuthSelection::Credential { .. })
        ));
        let mut mixed = selection.clone();
        mixed.environment_variable = Some("API_KEY".into());
        assert!(BackendAuthSelection::try_from(mixed).is_err());
        let mut missing = selection;
        missing.credential_version = None;
        assert!(BackendAuthSelection::try_from(missing).is_err());
    }

    #[test]
    fn private_auth_is_never_serialized_into_metadata() {
        let backend: InferenceBackend = serde_json::from_value(serde_json::json!({
            "node_did": "node", "backend_id": "backend", "name": "provider",
            "provider_kind": "OpenAiCompatible", "endpoint": "https://provider.example",
            "auth": {"kind": "api_key", "key": "private-secret"}
        }))
        .unwrap();
        let metadata = InferenceBackendMetadata::from_backend(
            backend.clone(),
            BackendAuthSelection::Credential {
                id: "id".into(),
                version: "rotation".into(),
            },
        )
        .unwrap();
        let encoded = serde_json::to_value(metadata).unwrap();
        assert!(encoded.get("auth").is_none());
        assert!(!encoded.to_string().contains("private-secret"));
        let mut injected = encoded;
        injected["auth"] = serde_json::json!({"kind":"api_key", "key":"private-secret"});
        assert!(serde_json::from_value::<InferenceBackendMetadata>(injected).is_err());
        assert!(InferenceBackendMetadata::from_backend(
            backend,
            BackendAuthSelection::Unauthenticated
        )
        .is_err());
    }

    #[test]
    fn metadata_cannot_rewrite_a_credential_reference_version() {
        let backend: InferenceBackend = serde_json::from_value(serde_json::json!({
            "node_did":"node", "backend_id":"backend", "name":"backend",
            "provider_kind":"OpenAiCompatible", "endpoint":"https://provider.example",
            "auth":{"kind":"credential", "id":"id", "version":"approved"}
        }))
        .unwrap();
        let cases = crate::lean_vocab_test::lean_identity_household_cases();
        let modeled = cases
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap()
            ._backend_credential_cases
            .iter()
            .find(|case| case["name"] == "rotated-credential")
            .unwrap()["may_use"]
            .as_bool()
            .unwrap();
        assert_eq!(
            InferenceBackendMetadata::from_backend(
                backend,
                BackendAuthSelection::Credential {
                    id: "id".into(),
                    version: "other".into()
                },
            )
            .is_ok(),
            modeled
        );
    }
}

/// Shared provider connection. Authentication must be compatible with the
/// provider adapter; OAuth credentials resolve through the invoking principal's DID.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct InferenceBackend {
    #[serde(skip)]
    #[cfg_attr(feature = "typescript", ts(skip))]
    pub physical_doc_id: Option<String>,
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
    #[serde(default)]
    pub metadata_doc_id: Option<String>,
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
    /// An operator-supplied key written to a private BackendCredential record.
    ApiKey { key: String },
    /// A private BackendCredential, resolved only by the runtime credential owner.
    Credential { id: String, version: String },
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
            Self::Credential { id, version } => f
                .debug_struct("Credential")
                .field("id", id)
                .field("version", version)
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

    /// An operator-supplied secret; self-configuration cannot set or change it
    /// (Lean `authPatchAllowed`).
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
