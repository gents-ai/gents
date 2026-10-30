use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountSummary {
    pub credential_id: String,
    pub provider: String,
    pub account_ref: Option<String>,
    pub label: String,
    pub identity: Option<String>,
    pub plan: Option<String>,
    pub enabled: bool,
    pub default: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub connected_at: Option<DateTime<Utc>>,
}

pub fn enabled_accounts_note(provider: &str, accounts: &[AccountSummary]) -> String {
    let product = sign_in_product(provider);
    let name = product.as_ref().map_or(provider, |product| product.name);
    let enabled: Vec<_> = accounts
        .iter()
        .filter(|account| account.provider == provider && account.enabled)
        .map(|account| format!("{:?}", account.label))
        .collect();
    match (enabled.is_empty(), product) {
        (false, _) => format!("enabled {name} accounts: {}", enabled.join(", ")),
        (true, Some(product)) => format!(
            "no enabled {name} account on this node; sign one in with `gents {}`",
            product.login_command
        ),
        (true, None) => format!("no enabled {name} account on this node"),
    }
}

pub fn no_switch_candidate_note(provider: &str, model: &str) -> String {
    match sign_in_product(provider) {
        Some(product) => format!(
            "no other {} account offers {model}; add one with `gents {} --label <label>`",
            product.name, product.login_command
        ),
        None => format!(
            "no other {provider} account offers {model}; add a backend with `gents config backend set`"
        ),
    }
}

const ACCOUNT_PROVIDERS: [&str; 3] = [
    crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
    crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
    crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
];

fn account_filter(node_did: &str, credential_id: Option<&str>) -> String {
    let providers = ACCOUNT_PROVIDERS
        .iter()
        .map(|provider| format!("\"{provider}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let credential = credential_id
        .map(|id| {
            format!(
                r#", credential_id: {{ _eq: "{}" }}"#,
                crate::graphql::escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    format!(
        r#"{{ node_did: {{ _eq: "{}" }}, provider: {{ _in: [{providers}] }}{credential} }}"#,
        crate::graphql::escape_graphql_string(node_did)
    )
}

fn account_not_found(credential_id: &str) -> anyhow::Error {
    anyhow::anyhow!("account {credential_id:?} not found for this node")
}

pub async fn list_accounts(
    access: &crate::config_client::ConfigAccess,
    node_did: &str,
) -> Result<Vec<AccountSummary>> {
    let response = access
        .execute(&format!(
            "query {{ OAuthCredential(filter: {}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}",
            account_filter(node_did, None)
        ))
        .await?;
    let mut rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(resolver_order);
    let defaults: Vec<_> = ACCOUNT_PROVIDERS
        .iter()
        .filter_map(|provider| {
            pick_oauth_credential(&rows, node_did, provider, AccountPick::ProviderDefault)
        })
        .map(|row| row.credential_id.clone())
        .collect();
    Ok(rows
        .iter()
        .map(|row| AccountSummary {
            credential_id: row.credential_id.clone(),
            provider: row.provider.clone(),
            account_ref: row.account_ref.clone(),
            label: effective_account_label(row),
            identity: account_display_label(row),
            plan: row.chatgpt_plan_type.clone(),
            enabled: row.enabled,
            default: defaults.contains(&row.credential_id),
            access_token_expires_at: row.access_token_expires_at,
            connected_at: row.connected_at,
        })
        .collect())
}

pub async fn set_account_label(
    access: &crate::config_client::ConfigAccess,
    node_did: &str,
    credential_id: &str,
    label: &str,
) -> Result<()> {
    let label = validate_account_label(label)?;
    access
        .transact_write("oauth_credential.label", |txn| {
            let label = label.clone();
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "query {{ OAuthCredential(filter: {}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}",
                        account_filter(node_did, Some(credential_id))
                    ))
                    .await?;
                let row = oauth_credentials_from_response(&response)
                    .into_iter()
                    .next()
                    .ok_or_else(|| account_not_found(credential_id))??;
                let rows = provider_rows_in_txn(txn, node_did, &row.provider).await?;
                let others: Vec<_> = rows
                    .iter()
                    .filter(|other| other.doc_id != row.doc_id)
                    .collect();
                ensure_label_free(&others, &label)?;
                let doc_id = row
                    .doc_id
                    .as_deref()
                    .context("stored OAuthCredential has no _docID")?;
                txn.execute(&format!(
                    r#"mutation {{ update_OAuthCredential(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {} }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(doc_id),
                    gents_protocol::graphql::nullable_string_field("label", Some(&label)),
                ))
                .await?;
                let old_label = effective_account_label(&row);
                let renamed = OAuthCredential {
                    label: Some(label),
                    ..row
                };
                sync_account_backend(txn, &renamed, Some(&old_label)).await
            })
        })
        .await
}

pub async fn set_account_enabled(
    access: &crate::config_client::ConfigAccess,
    node_did: &str,
    credential_id: &str,
    enabled: bool,
) -> Result<()> {
    let response = access
        .write(
            "oauth_credential.enabled",
            &format!(
                "mutation {{ update_OAuthCredential(filter: {}, input: {{ enabled: {} }}) {{ _docID }} }}",
                account_filter(node_did, Some(credential_id)),
                gents_protocol::graphql::graphql_bool_literal(enabled),
            ),
        )
        .await?;
    ensure_matched(&response, "update_OAuthCredential", credential_id)
}

pub async fn remove_account_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    credential_id: &str,
) -> Result<()> {
    let filter = account_filter(node_did, Some(credential_id));
    let response = txn
        .execute(&format!(
            "query {{ OAuthCredential(filter: {filter}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}"
        ))
        .await?;
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .ok_or_else(|| account_not_found(credential_id))??;
    let response = txn
        .execute(&format!(
            "mutation {{ delete_OAuthCredential(filter: {filter}) {{ _docID }} }}"
        ))
        .await?;
    ensure_matched(&response, "delete_OAuthCredential", credential_id)?;
    Ok(())
}

fn ensure_matched(response: &Value, field: &str, credential_id: &str) -> Result<()> {
    if response
        .get("data")
        .and_then(|data| data.get(field))
        .is_some_and(crate::graphql::response_has_documents)
    {
        Ok(())
    } else {
        Err(account_not_found(credential_id))
    }
}
