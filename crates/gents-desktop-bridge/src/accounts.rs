use crate::error::{BridgeError, BridgeErrorCode};
use gents::inference::auth::credential::{
    list_oauth_credentials_on, OAuthCredential, SignIn, SignInResult,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
pub(crate) mod login;
pub mod usage;
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSignInRequest {
    pub node_did: String,
    pub method: gents::inference_setup::InferenceAuthMethod,
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
    pub auth: gents::document_config::BackendAuth,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct BackendAuthenticationView {
    pub revision: gents_desktop_core::client::ConfigEditorRevision,
    pub backend_id: String,
    pub auth_kind: String,
    pub environment_variable: Option<String>,
    pub account_ref: Option<String>,
    pub oauth_provider: Option<String>,
}

impl BackendAuthenticationView {
    pub(crate) fn from_backend(
        revision: gents_desktop_core::client::ConfigEditorRevision,
        backend: &gents::InferenceBackend,
    ) -> Self {
        use gents::backend_provider::BackendProviderOauthExt;
        use gents::document_config::BackendAuth;
        let (kind, variable, account_ref) = match &backend.auth {
            BackendAuth::Unauthenticated => ("unauthenticated", None, None),
            BackendAuth::ApiKey { .. } => ("api_key", None, None),
            BackendAuth::Environment { variable } => ("environment", Some(variable.clone()), None),
            BackendAuth::NodeOAuth { account_ref } => ("node_oauth", None, account_ref.clone()),
        };
        Self {
            revision,
            backend_id: backend.backend_id.clone(),
            auth_kind: kind.to_owned(),
            environment_variable: variable,
            account_ref,
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
}
const LOG_TARGET: &str = "gents_desktop_bridge::inference_setup";

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

pub(crate) fn provider_label(provider: &str) -> &'static str {
    match provider {
        gents::chatgpt_codex::CHATGPT_CODEX_PROVIDER => "ChatGPT",
        gents::inference::auth::claude::CLAUDE_OAUTH_PROVIDER => "Claude",
        gents::xai_grok_oauth::XAI_OAUTH_PROVIDER => "Grok",
        _ => "provider",
    }
}

pub(crate) async fn require_reachable_configuration(
    access: anyhow::Result<gents::ConfigAccess>,
    node_did: &str,
    provider: &str,
) -> Result<(), BridgeError> {
    let probe = match access {
        Ok(access) => list_oauth_credentials_on(&access, node_did)
            .await
            .map(|_| ()),
        Err(error) => Err(error),
    };
    probe.map_err(|error| {
        tracing::warn!(
            target: LOG_TARGET,
            node_did,
            provider,
            error = %format!("{error:#}"),
            "node configuration is not reachable; not starting provider sign-in"
        );
        BridgeError::new(
            BridgeErrorCode::EndpointUnreachable,
            format!(
                "The node is not running, so {} sign-in was not started. Start the node and try again.",
                provider_label(provider)
            ),
        )
    })
}

pub(crate) async fn upsert_through(
    access: anyhow::Result<gents::ConfigAccess>,
    credential: OAuthCredential,
) -> anyhow::Result<SignIn> {
    let access = access?;
    let label = credential.label.clone();
    let mut signed =
        gents::inference::auth::credential::store_sign_in(&access, credential, None).await?;
    if let (SignInResult::Added, Some(label)) = (&signed.result, label) {
        let stored = &signed.credential;
        match gents::inference::auth::credential::set_account_label(
            &access,
            &stored.node_did,
            &stored.credential_id,
            &label,
        )
        .await
        {
            Ok(()) => signed.credential.label = Some(label),
            Err(error) => tracing::warn!(
                target: LOG_TARGET,
                node_did = %stored.node_did,
                provider = %stored.provider,
                error = %format!("{error:#}"),
                "labelling the added account failed; it keeps its default label"
            ),
        }
    }
    Ok(signed)
}

pub(crate) fn sign_in_label(label: Option<&str>) -> Result<Option<String>, BridgeError> {
    label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(gents::inference::auth::credential::validate_account_label)
        .transpose()
        .map_err(|error| BridgeError::new(BridgeErrorCode::InvalidArgument, error.to_string()))
}

fn credential_not_saved(credential: &OAuthCredential) -> BridgeError {
    tracing::warn!(
        target: LOG_TARGET,
        node_did = %credential.node_did,
        provider = %credential.provider,
        "saving the issued provider credential failed; holding it for retry"
    );
    BridgeError::new(
        BridgeErrorCode::CredentialNotSaved,
        format!(
            "You are signed in to {}, but Gents could not save the sign-in to the node. Make sure the node is running, then retry saving.",
            provider_label(&credential.provider)
        ),
    )
}

pub(crate) async fn save_issued_credential(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    issued: IssuedOAuthCredential,
) -> Result<SignIn, BridgeError> {
    let credential = issued.credential().clone();
    match pending
        .save(issued, |credential| upsert_through(access, credential))
        .await
    {
        CredentialSave::Saved(signed) => Ok(signed),
        CredentialSave::Superseded => Err(BridgeError::untyped(format!(
            "A newer {} sign-in for this node replaced this one.",
            provider_label(&credential.provider)
        ))),
        CredentialSave::Failed(_) => Err(credential_not_saved(&credential)),
    }
}

pub(crate) async fn retry_pending_credential(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    node_did: &str,
    provider: &str,
) -> Result<OAuthCredential, BridgeError> {
    let (credential, saved) = pending
        .retry(node_did, provider, |credential| {
            upsert_through(access, credential)
        })
        .await
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::NotFound,
                "There is no unsaved sign-in to retry. Sign in again.",
            )
        })?;
    match saved {
        Ok(signed) => Ok(signed.credential),
        Err(_) => Err(credential_not_saved(&credential)),
    }
}

pub(crate) async fn observe_provider_accounts(
    pending: &PendingOAuthCredentials,
    access: anyhow::Result<gents::ConfigAccess>,
    node_did: &str,
) -> Result<Vec<ProviderAccountView>, BridgeError> {
    let held: Vec<ProviderAccountView> = pending
        .held_for(node_did)
        .iter()
        .map(ProviderAccountView::pending_save)
        .collect();
    let stored = match access {
        Ok(access) => list_oauth_credentials_on(&access, node_did).await,
        Err(error) => Err(error),
    };
    match stored {
        Ok(stored) => Ok(stored
            .iter()
            .map(ProviderAccountView::from)
            .chain(held)
            .collect()),
        Err(error) if !held.is_empty() => {
            tracing::warn!(
                target: LOG_TARGET,
                node_did,
                error = %format!("{error:#}"),
                "reading stored provider accounts failed; reporting held sign-ins only"
            );
            Ok(held)
        }
        Err(error) => Err(BridgeError::untyped(error.to_string())),
    }
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderAccountView {
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub account_id: Option<String>,
    pub plan_type: Option<String>,
    pub access_token_expires_at: String,
    pub last_refresh: Option<String>,
    pub enabled: bool,
    pub pending_save: bool,
    pub account_ref: Option<String>,
    pub label: String,
}

impl ProviderAccountView {
    pub(crate) fn pending_save(credential: &OAuthCredential) -> Self {
        Self {
            pending_save: true,
            ..Self::from(credential)
        }
    }
}

impl From<&OAuthCredential> for ProviderAccountView {
    fn from(credential: &OAuthCredential) -> Self {
        Self {
            credential_id: credential.credential_id.clone(),
            node_did: credential.node_did.clone(),
            provider: credential.provider.clone(),
            account_id: gents::inference::auth::credential::account_display_label(credential),
            plan_type: credential.chatgpt_plan_type.clone(),
            access_token_expires_at: credential.access_token_expires_at.to_rfc3339(),
            last_refresh: credential.last_refresh.map(|value| value.to_rfc3339()),
            enabled: credential.enabled,
            pending_save: false,
            account_ref: credential.account_ref.clone(),
            label: gents::inference::auth::credential::effective_account_label(credential),
        }
    }
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SignInView {
    pub result: String,
    pub label: String,
    pub account_ref: Option<String>,
    pub hint: Option<String>,
}

impl From<&SignIn> for SignInView {
    fn from(signed: &SignIn) -> Self {
        Self {
            result: match signed.result {
                SignInResult::Added => "added",
                SignInResult::Refreshed => "refreshed",
            }
            .to_string(),
            label: gents::inference::auth::credential::effective_account_label(&signed.credential),
            account_ref: signed.credential.account_ref.clone(),
            hint: signed.account_chooser_hint(),
        }
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
