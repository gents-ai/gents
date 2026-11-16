//! Provider account sign-in and observation served by the Tauri commands.

use super::{CredentialSave, IssuedOAuthCredential, PendingOAuthCredentials};
use crate::error::{BridgeError, BridgeErrorCode};
use gents_inference::auth::credential::{
    list_stored_accounts, OAuthCredential, SignIn, SignInResult, StoredAccount,
};
use serde::Serialize;

const LOG_TARGET: &str = "gents_desktop_bridge::inference_setup";

pub(crate) fn provider_label(provider: &str) -> &'static str {
    match provider {
        gents_inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER => "ChatGPT",
        gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER => "Claude",
        gents_inference::clients::xai_grok::XAI_OAUTH_PROVIDER => "Grok",
        _ => "provider",
    }
}

pub(crate) async fn require_reachable_configuration(
    access: anyhow::Result<gents_store::config_client::ConfigAccess>,
    node_did: &str,
    provider: &str,
) -> Result<(), BridgeError> {
    let probe = match access {
        Ok(access) => list_stored_accounts(&access, node_did).await.map(|_| ()),
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
    access: anyhow::Result<gents_store::config_client::ConfigAccess>,
    credential: OAuthCredential,
) -> anyhow::Result<SignIn> {
    let access = access?;
    let label = credential.label.clone();
    let mut signed =
        gents_inference::auth::credential::store_sign_in(&access, credential, None).await?;
    if let (SignInResult::Added, Some(label)) = (&signed.result, label) {
        let stored = &signed.credential;
        match gents_inference::auth::credential::set_account_label(
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
        .map(gents_inference::auth::credential::validate_account_label)
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
    access: anyhow::Result<gents_store::config_client::ConfigAccess>,
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
    access: anyhow::Result<gents_store::config_client::ConfigAccess>,
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
    access: anyhow::Result<gents_store::config_client::ConfigAccess>,
    node_did: &str,
) -> Result<Vec<ProviderAccountView>, BridgeError> {
    let held: Vec<ProviderAccountView> = pending
        .held_for(node_did)
        .iter()
        .map(ProviderAccountView::pending_save)
        .collect();
    let stored = match access {
        Ok(access) => list_stored_accounts(&access, node_did).await,
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
        Self::from(&StoredAccount::from(credential))
    }
}

impl From<&StoredAccount> for ProviderAccountView {
    fn from(account: &StoredAccount) -> Self {
        Self {
            credential_id: account.credential_id.clone(),
            node_did: account.node_did.clone(),
            provider: account.provider.clone(),
            account_id: account.identity.clone(),
            plan_type: account.plan.clone(),
            access_token_expires_at: account.access_token_expires_at.to_rfc3339(),
            last_refresh: account.last_refresh.map(|value| value.to_rfc3339()),
            enabled: account.enabled,
            pending_save: false,
            account_ref: account.account_ref.clone(),
            label: account.label.clone(),
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
            label: gents_inference::auth::credential::effective_account_label(&signed.credential),
            account_ref: signed.credential.account_ref.clone(),
            hint: signed.account_chooser_hint(),
        }
    }
}
