use anyhow::Result;
use rapidhash::{HashSetExt, RapidHashSet};
use serde_json::Value;

use super::*;
use gents_documents::BackendAuth;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPick<'a> {
    ProviderDefault,
    Reference(Option<&'a str>),
}

/// How a node OAuth backend resolves one account for readiness, discovery
/// and resume: its named account, or for a pool the earliest-connected
/// enabled account, so a pool resolves while any of its accounts does.
pub fn account_pick(auth: &BackendAuth) -> Option<AccountPick<'_>> {
    match auth {
        BackendAuth::NodeOAuth { account_ref } => {
            Some(AccountPick::Reference(account_ref.as_deref()))
        }
        BackendAuth::NodeOAuthPool { .. } => Some(AccountPick::ProviderDefault),
        _ => None,
    }
}

/// The fields the resolver picks an account by, which an account row has
/// with or without its tokens: a stored credential, or a listing's summary.
pub trait AccountRow {
    fn credential_id(&self) -> &str;
    fn node_did(&self) -> &str;
    fn provider(&self) -> &str;
    fn enabled(&self) -> bool;
    fn account_ref(&self) -> Option<&str>;
    fn connected_at(&self) -> Option<chrono::DateTime<chrono::Utc>>;
}

impl AccountRow for OAuthCredential {
    fn credential_id(&self) -> &str {
        &self.credential_id
    }
    fn node_did(&self) -> &str {
        &self.node_did
    }
    fn provider(&self) -> &str {
        &self.provider
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn account_ref(&self) -> Option<&str> {
        self.account_ref.as_deref()
    }
    fn connected_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.connected_at
    }
}

impl AccountRow for OAuthCredentialSummary {
    fn credential_id(&self) -> &str {
        &self.credential_id
    }
    fn node_did(&self) -> &str {
        &self.node_did
    }
    fn provider(&self) -> &str {
        &self.provider
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn account_ref(&self) -> Option<&str> {
        self.account_ref.as_deref()
    }
    fn connected_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.connected_at
    }
}

pub(super) fn resolver_order<R: AccountRow>(a: &R, b: &R) -> std::cmp::Ordering {
    (a.connected_at(), a.credential_id()).cmp(&(b.connected_at(), b.credential_id()))
}

pub fn pick_oauth_credential<'c, R: AccountRow + 'c>(
    rows: impl IntoIterator<Item = &'c R>,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Option<&'c R> {
    rows.into_iter()
        .filter(|row| row.node_did() == node_did && row.provider() == provider && row.enabled())
        .filter(|row| match pick {
            AccountPick::ProviderDefault => true,
            AccountPick::Reference(account_ref) => row.account_ref() == account_ref,
        })
        .min_by(|a, b| resolver_order(*a, *b))
}

/// The accounts a pool backend rotates over (Lean `oauthPool`): every enabled
/// row of `provider` the node owns, in the resolver's order.
pub fn pick_oauth_pool<'c, R: AccountRow + 'c>(
    rows: impl IntoIterator<Item = &'c R>,
    node_did: &str,
    provider: &str,
) -> Vec<&'c R> {
    let mut pool: Vec<_> = rows
        .into_iter()
        .filter(|row| row.node_did() == node_did && row.provider() == provider && row.enabled())
        .collect();
    pool.sort_by(|a, b| resolver_order(*a, *b));
    pool
}

/// One node's credential rows as a runtime view holds them between loads, so
/// readiness asks the resolver's picks of the rows it was resolved against
/// rather than of a later read.
#[derive(Debug, Clone, Default)]
pub struct NodeOAuthAccounts {
    rows: Vec<OAuthCredential>,
}

impl NodeOAuthAccounts {
    pub async fn load(
        access: &gents_store::config_client::ConfigAccess,
        node_did: &str,
    ) -> Result<Self> {
        let rows = super::list_oauth_credentials_on(access, node_did).await?;
        let mut seen = RapidHashSet::with_capacity(rows.len());
        for row in &rows {
            anyhow::ensure!(row.doc_id.is_some(), "OAuthCredential missing physical ID");
            anyhow::ensure!(row.node_did == node_did, "OAuthCredential owner mismatch");
            anyhow::ensure!(
                seen.insert(row.credential_id.as_str()),
                "duplicate OAuthCredential {}",
                row.credential_id
            );
        }
        Ok(Self { rows })
    }

    pub fn resolves(&self, node_did: &str, provider: &str, pick: AccountPick<'_>) -> bool {
        pick_oauth_credential(&self.rows, node_did, provider, pick).is_some()
    }

    /// The credential ids of [`pick_oauth_pool`], in rotation order.
    pub fn pool_ids(&self, node_did: &str, provider: &str) -> Vec<String> {
        pick_oauth_pool(&self.rows, node_did, provider)
            .into_iter()
            .map(|row| row.credential_id.clone())
            .collect()
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn rows_mut(&mut self) -> &mut Vec<OAuthCredential> {
        &mut self.rows
    }
}

pub async fn resolve_oauth_credential(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let response = access
        .execute_for_use(
            &enabled_oauth_credentials_query(node_did, provider, OAUTH_CREDENTIAL_FIELDS),
            "OAuthCredential",
        )
        .await?;
    pick_from_response(&response, node_did, provider, pick)
}

pub async fn resolve_oauth_pool(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    provider: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = access
        .execute_for_use(
            &enabled_oauth_credentials_query(node_did, provider, OAUTH_CREDENTIAL_FIELDS),
            "OAuthCredential",
        )
        .await?;
    let rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    Ok(pick_oauth_pool(&rows, node_did, provider)
        .into_iter()
        .cloned()
        .collect())
}

pub async fn provider_default_account_ref(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
) -> Result<Option<String>> {
    if txn.managed_agent_signer().is_some() {
        return super::runtime_default_oauth_account_ref(txn, node_did, provider)
            .await
            .map_err(Into::into);
    }
    Ok(
        resolve_oauth_credential_in_txn(txn, node_did, provider, AccountPick::ProviderDefault)
            .await?
            .and_then(|row| row.account_ref),
    )
}

pub(crate) fn runtime_account_query(node_did: &str, provider: &str) -> String {
    enabled_oauth_credentials_query(
        node_did,
        provider,
        "_docID credential_id node_did provider enabled account_ref connected_at",
    )
}

pub(crate) struct RuntimeAccountSelection {
    pub(crate) account_ref: Option<String>,
    pub(crate) connected_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) fn runtime_account_from_response(
    response: &Value,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<RuntimeAccountSelection>> {
    let rows = gents_protocol::graphql::graphql_rows_from_response(response, "OAuthCredential")
        .into_iter()
        .map(pick_row_from_value)
        .collect::<Result<Vec<_>>>()?;
    pick_oauth_credential(&rows, node_did, provider, pick)
        .map(|row| {
            Ok(RuntimeAccountSelection {
                account_ref: row.account_ref.clone(),
                connected_at: row.connected_at,
            })
        })
        .transpose()
}

pub async fn resolve_oauth_credential_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let response = txn
        .execute_for_use(
            &enabled_oauth_credentials_query(node_did, provider, OAUTH_PICK_FIELDS),
            "OAuthCredential",
        )
        .await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "OAuthCredential")
        .into_iter()
        .map(pick_row_from_value)
        .collect::<Result<Vec<_>>>()?;
    Ok(pick_oauth_credential(&rows, node_did, provider, pick).cloned())
}

fn pick_from_response(
    response: &Value,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let rows = oauth_credentials_from_response(response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    Ok(pick_oauth_credential(&rows, node_did, provider, pick).cloned())
}
