use anyhow::Result;
use serde_json::Value;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPick<'a> {
    ProviderDefault,
    Reference(Option<&'a str>),
}

impl crate::documents::BackendAuth {
    /// How a node OAuth backend resolves one account for readiness, discovery
    /// and resume: its named account, or for a pool the earliest-connected
    /// enabled account, so a pool resolves while any of its accounts does.
    pub fn account_pick(&self) -> Option<AccountPick<'_>> {
        match self {
            Self::NodeOAuth { account_ref } => Some(AccountPick::Reference(account_ref.as_deref())),
            Self::NodeOAuthPool { .. } => Some(AccountPick::ProviderDefault),
            _ => None,
        }
    }
}

pub(super) fn resolver_order(a: &OAuthCredential, b: &OAuthCredential) -> std::cmp::Ordering {
    (a.connected_at, &a.credential_id).cmp(&(b.connected_at, &b.credential_id))
}

pub fn pick_oauth_credential<'c>(
    rows: impl IntoIterator<Item = &'c OAuthCredential>,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Option<&'c OAuthCredential> {
    rows.into_iter()
        .filter(|row| row.node_did == node_did && row.provider == provider && row.enabled)
        .filter(|row| match pick {
            AccountPick::ProviderDefault => true,
            AccountPick::Reference(account_ref) => row.account_ref.as_deref() == account_ref,
        })
        .min_by(|a, b| resolver_order(a, b))
}

/// The accounts a pool backend rotates over (Lean `oauthPool`): every enabled
/// row of `provider` the node owns, in the resolver's order.
pub fn pick_oauth_pool<'c>(
    rows: impl IntoIterator<Item = &'c OAuthCredential>,
    node_did: &str,
    provider: &str,
) -> Vec<&'c OAuthCredential> {
    let mut pool: Vec<_> = rows
        .into_iter()
        .filter(|row| row.node_did == node_did && row.provider == provider && row.enabled)
        .collect();
    pool.sort_by(|a, b| resolver_order(a, b));
    pool
}

pub async fn resolve_oauth_credential(
    access: &crate::store::config_client::ConfigAccess,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let response = access
        .execute(&enabled_oauth_credentials_query(
            node_did,
            provider,
            OAUTH_CREDENTIAL_FIELDS,
        ))
        .await?;
    pick_from_response(&response, node_did, provider, pick)
}

pub async fn resolve_oauth_pool(
    access: &crate::store::config_client::ConfigAccess,
    node_did: &str,
    provider: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = access
        .execute(&enabled_oauth_credentials_query(
            node_did,
            provider,
            OAUTH_CREDENTIAL_FIELDS,
        ))
        .await?;
    let rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    Ok(pick_oauth_pool(&rows, node_did, provider)
        .into_iter()
        .cloned()
        .collect())
}

pub(crate) async fn provider_default_account_ref(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
) -> Result<Option<String>> {
    if txn.managed_agent_signer().is_some() {
        return txn
            .runtime_default_oauth_account_ref(node_did, provider)
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
    pub(crate) doc_id: String,
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
                doc_id: row
                    .doc_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("selected account has no document ID"))?,
                account_ref: row.account_ref.clone(),
                connected_at: row.connected_at,
            })
        })
        .transpose()
}

pub(crate) async fn resolve_oauth_credential_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let response = txn
        .execute(&enabled_oauth_credentials_query(
            node_did,
            provider,
            OAUTH_PICK_FIELDS,
        ))
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
