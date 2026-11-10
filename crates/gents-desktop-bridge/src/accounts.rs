use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
#[cfg(feature = "tauri-host")]
pub(crate) mod login;
#[cfg(feature = "tauri-host")]
mod provider;
#[cfg(feature = "tauri-host")]
pub(crate) use provider::*;
pub mod usage;
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSignInRequest {
    pub node_did: String,
    pub method: gents::inference::backends::setup::InferenceAuthMethod,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendAuthenticationRequest {
    pub node_did: String,
    pub backend_id: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendAuthenticationSaveRequest {
    pub node_did: String,
    pub backend_id: String,
    pub revision: gents_desktop_core::client::ConfigEditorRevision,
    pub auth: gents::documents::BackendAuth,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BackendAuthenticationView {
    pub revision: gents_desktop_core::client::ConfigEditorRevision,
    pub backend_id: String,
    pub auth_kind: String,
    pub environment_variable: Option<String>,
    pub account_ref: Option<String>,
    pub allow_overage: Option<bool>,
    pub oauth_provider: Option<String>,
}

impl BackendAuthenticationView {
    pub(crate) fn from_backend(
        revision: gents_desktop_core::client::ConfigEditorRevision,
        backend: &gents::InferenceBackend,
    ) -> Self {
        use gents::documents::BackendAuth;
        use gents::inference::backends::provider::BackendProviderOauthExt;
        let (kind, variable, account_ref) = match &backend.auth {
            BackendAuth::Unauthenticated => ("unauthenticated", None, None),
            BackendAuth::ApiKey { .. } => ("api_key", None, None),
            BackendAuth::Credential { .. } => ("credential", None, None),
            BackendAuth::Environment { variable } => ("environment", Some(variable.clone()), None),
            BackendAuth::NodeOAuth { account_ref } => ("node_oauth", None, account_ref.clone()),
            BackendAuth::NodeOAuthPool { .. } => ("node_oauth_pool", None, None),
            BackendAuth::AwsCredentials { .. } => ("aws_credentials", None, None),
            BackendAuth::GoogleCredentials { .. } => ("google_credentials", None, None),
        };
        Self {
            revision,
            backend_id: backend.backend_id.clone(),
            auth_kind: kind.to_owned(),
            environment_variable: variable,
            account_ref,
            allow_overage: backend.auth.pool_allow_overage(),
            oauth_provider: backend.provider_kind.oauth_provider().map(str::to_owned),
        }
    }
}

#[cfg(test)]
mod authentication_tests {
    use super::*;

    #[test]
    fn authentication_metadata_and_command_debug_never_contain_the_api_key() {
        let backend: gents::InferenceBackend = serde_json::from_value(serde_json::json!({
            "node_did":"did:test:backend", "backend_id":"backend", "name":"Backend",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"api_key","key":"private-authentication-fixture"}
        }))
        .unwrap();
        let revision = gents_desktop_core::client::ConfigEditorRevision {
            document_id: "physical".into(),
            composite_cid: "version".into(),
        };
        let view = BackendAuthenticationView::from_backend(revision.clone(), &backend);
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("private-authentication-fixture"));
        assert_eq!(view.auth_kind, "api_key");
        let command = BackendAuthenticationSaveRequest {
            node_did: backend.node_did,
            backend_id: backend.backend_id,
            revision,
            auth: backend.auth,
        };
        assert!(!format!("{command:?}").contains("private-authentication-fixture"));
    }

    #[test]
    fn authentication_metadata_reports_an_opaque_stored_credential() {
        let backend: gents::InferenceBackend = serde_json::from_value(serde_json::json!({
            "node_did":"did:test:backend", "backend_id":"backend", "name":"Backend",
            "provider_kind":"ClaudeBedrock", "endpoint":"https://example.test",
            "auth":{"kind":"credential", "id":"private-reference-fixture", "version":"private-version-fixture"}
        }))
        .unwrap();
        let revision = gents_desktop_core::client::ConfigEditorRevision {
            document_id: "physical".into(),
            composite_cid: "revision".into(),
        };
        let view = BackendAuthenticationView::from_backend(revision, &backend);
        let serialized = serde_json::to_value(&view).unwrap();
        assert_eq!(serialized["authKind"], "credential");
        assert!(serialized["environmentVariable"].is_null());
        assert!(serialized["accountRef"].is_null());
        assert!(serialized["allowOverage"].is_null());
        assert!(serialized["oauthProvider"].is_null());
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("private-reference-fixture"));
        assert!(!serialized.contains("private-version-fixture"));
    }

    #[test]
    fn rotation_requests_decode_the_operator_choice_and_reject_unknown_fields() {
        use gents::inference::backends::rotation::BackendRotation;
        let decode = |rotation: serde_json::Value| {
            serde_json::from_value::<crate::types::BackendRotationSetRequest>(
                serde_json::json!({"nodeDid":"did:test:backend","backendId":"claude","rotation":rotation}),
            )
        };
        assert_eq!(
            decode(serde_json::json!({"kind":"pool"})).unwrap().rotation,
            BackendRotation::Pool {
                allow_overage: None
            }
        );
        assert_eq!(
            decode(serde_json::json!({"kind":"pool","allow_overage":true}))
                .unwrap()
                .rotation,
            BackendRotation::Pool {
                allow_overage: Some(true)
            }
        );
        assert_eq!(
            decode(serde_json::json!({"kind":"single"}))
                .unwrap()
                .rotation,
            BackendRotation::Single
        );
        assert!(decode(serde_json::json!({"kind":"pool","allowOverage":true})).is_err());
        assert!(decode(serde_json::json!({"kind":"split"})).is_err());
    }
}

type CredentialKey = (String, String, String);

pub struct IssuedOAuthCredential {
    sequence: u64,
    credential: gents::inference::auth::credential::OAuthCredential,
}

impl IssuedOAuthCredential {
    pub fn credential(&self) -> &gents::inference::auth::credential::OAuthCredential {
        &self.credential
    }

    fn key(&self) -> CredentialKey {
        let identity = [
            &self.credential.provider_account_key,
            &self.credential.account_id,
        ]
        .into_iter()
        .flatten()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_default();
        (
            self.credential.node_did.clone(),
            self.credential.provider.clone(),
            identity.to_owned(),
        )
    }
}

pub enum CredentialSave<T> {
    Saved(T),
    Superseded,
    Failed(anyhow::Error),
}

#[derive(Default)]
struct CredentialSlot {
    write_gate: Arc<tokio::sync::Mutex<()>>,
    newest: u64,
    held: Option<IssuedOAuthCredential>,
}

/// Issued credentials awaiting storage stay in memory and never cross the client boundary.
/// Saves for one account are ordered so an older sign-in cannot replace a newer one.
#[derive(Default)]
pub struct PendingOAuthCredentials {
    issued: std::sync::atomic::AtomicU64,
    slots: Mutex<rapidhash::RapidHashMap<CredentialKey, CredentialSlot>>,
}

impl PendingOAuthCredentials {
    pub fn issue(
        &self,
        credential: gents::inference::auth::credential::OAuthCredential,
    ) -> IssuedOAuthCredential {
        IssuedOAuthCredential {
            sequence: self
                .issued
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1,
            credential,
        }
    }

    fn slots(
        &self,
    ) -> std::sync::MutexGuard<'_, rapidhash::RapidHashMap<CredentialKey, CredentialSlot>> {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_gate(&self, key: &CredentialKey) -> Arc<tokio::sync::Mutex<()>> {
        self.slots()
            .entry(key.clone())
            .or_default()
            .write_gate
            .clone()
    }

    pub async fn save<T, F, Fut>(
        &self,
        issued: IssuedOAuthCredential,
        write: F,
    ) -> CredentialSave<T>
    where
        F: FnOnce(gents::inference::auth::credential::OAuthCredential) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let key = issued.key();
        let gate = self.write_gate(&key);
        let _ordered = gate.lock().await;
        {
            let mut slots = self.slots();
            let slot = slots.entry(key.clone()).or_default();
            if issued.sequence < slot.newest {
                return CredentialSave::Superseded;
            }
            slot.newest = issued.sequence;
        }
        let written = write(issued.credential.clone()).await;
        let mut slots = self.slots();
        let slot = slots.entry(key).or_default();
        match written {
            Ok(value) => {
                slot.held = None;
                CredentialSave::Saved(value)
            }
            Err(error) => {
                slot.held = Some(issued);
                CredentialSave::Failed(error)
            }
        }
    }

    pub async fn retry<T, F, Fut>(
        &self,
        node_did: &str,
        provider: &str,
        write: F,
    ) -> Option<(
        gents::inference::auth::credential::OAuthCredential,
        anyhow::Result<T>,
    )>
    where
        F: FnOnce(gents::inference::auth::credential::OAuthCredential) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let key = self
            .slots()
            .iter()
            .filter(|((node, held_provider, _), slot)| {
                node == node_did && held_provider == provider && slot.held.is_some()
            })
            .max_by_key(|(_, slot)| slot.held.as_ref().map(|held| held.sequence))
            .map(|(key, _)| key.clone())?;
        let gate = self.write_gate(&key);
        let _ordered = gate.lock().await;
        let credential = self
            .slots()
            .get(&key)?
            .held
            .as_ref()
            .map(|held| held.credential.clone())?;
        let written = write(credential.clone()).await;
        if written.is_ok() {
            if let Some(slot) = self.slots().get_mut(&key) {
                slot.held = None;
            }
        }
        Some((credential, written))
    }

    pub fn held_for(
        &self,
        node_did: &str,
    ) -> Vec<gents::inference::auth::credential::OAuthCredential> {
        let mut held: Vec<_> = self
            .slots()
            .iter()
            .filter(|((node, _, _), _)| node == node_did)
            .filter_map(|(_, slot)| slot.held.as_ref().map(|held| held.credential.clone()))
            .collect();
        held.sort_by(|left, right| left.provider.cmp(&right.provider));
        held
    }
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountsRequest {
    pub node_did: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountDisconnectRequest {
    pub node_did: String,
    pub credential_id: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountRenameRequest {
    pub node_did: String,
    pub credential_id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountRemoveRequest {
    pub node_did: String,
    pub credential_id: String,
}

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct ProviderAccountRetrySaveRequest {
    pub node_did: String,
    pub provider: String,
}
