use anyhow::Result;
use serde_json::Value;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPick<'a> {
    ProviderDefault,
    Reference(Option<&'a str>),
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

pub async fn resolve_oauth_credential(
    access: &crate::config_client::ConfigAccess,
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

pub(crate) async fn provider_default_account_ref(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
) -> Result<Option<String>> {
    Ok(
        resolve_oauth_credential_in_txn(txn, node_did, provider, AccountPick::ProviderDefault)
            .await?
            .and_then(|row| row.account_ref),
    )
}

pub(crate) async fn resolve_oauth_credential_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
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
