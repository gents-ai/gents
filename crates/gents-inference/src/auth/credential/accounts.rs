use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

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

/// A stored row without its tokens: what every account listing reads, for
/// any client may list a node's accounts while only the runtime that holds an
/// account reads its tokens (Lean `AccountListing.Summary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCredentialSummary {
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub account_id: Option<String>,
    /// The email the tokens showed when they were last stored or refreshed.
    pub account_email: Option<String>,
    pub chatgpt_plan_type: Option<String>,
    pub access_token_expires_at: DateTime<Utc>,
    pub last_refresh: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub account_ref: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    pub provider_account_key: Option<String>,
    pub label: Option<String>,
}

impl OAuthCredentialSummary {
    /// How a listing names the account: its email, else its account id, else
    /// the provider's account key, each only when not blank (Lean
    /// `AccountListing.identityOf`).
    pub fn identity(&self) -> Option<String> {
        let shown = |value: &Option<String>| value.clone().filter(|text| !text.trim().is_empty());
        shown(&self.account_email)
            .or_else(|| shown(&self.account_id))
            .or_else(|| shown(&self.provider_account_key))
    }

    pub fn effective_label(&self) -> String {
        label_or_product(self.label.as_deref(), &self.provider)
    }
}

/// A held credential as a listing would show it once stored.
impl From<&OAuthCredential> for OAuthCredentialSummary {
    fn from(credential: &OAuthCredential) -> Self {
        Self {
            credential_id: credential.credential_id.clone(),
            node_did: credential.node_did.clone(),
            provider: credential.provider.clone(),
            account_id: credential.account_id.clone(),
            account_email: token_account_email(credential),
            chatgpt_plan_type: credential.chatgpt_plan_type.clone(),
            access_token_expires_at: credential.access_token_expires_at,
            last_refresh: credential.last_refresh,
            enabled: credential.enabled,
            account_ref: credential.account_ref.clone(),
            connected_at: credential.connected_at,
            provider_account_key: credential.provider_account_key.clone(),
            label: credential.label.clone(),
        }
    }
}

/// A stored row as account management shows it: any provider, no tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccount {
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub account_ref: Option<String>,
    pub label: String,
    pub identity: Option<String>,
    pub plan: Option<String>,
    pub enabled: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub last_refresh: Option<DateTime<Utc>>,
}

impl From<&OAuthCredentialSummary> for StoredAccount {
    fn from(row: &OAuthCredentialSummary) -> Self {
        Self {
            credential_id: row.credential_id.clone(),
            node_did: row.node_did.clone(),
            provider: row.provider.clone(),
            account_ref: row.account_ref.clone(),
            label: row.effective_label(),
            identity: row.identity(),
            plan: row.chatgpt_plan_type.clone(),
            enabled: row.enabled,
            access_token_expires_at: row.access_token_expires_at,
            last_refresh: row.last_refresh,
        }
    }
}

impl From<&OAuthCredential> for StoredAccount {
    fn from(credential: &OAuthCredential) -> Self {
        Self::from(&OAuthCredentialSummary::from(credential))
    }
}

/// The query every account listing reads: the rows of `providers` the node
/// owns, or every row it owns, and no token field.
fn account_listing_query(node_did: &str, providers: Option<&[&str]>) -> String {
    let filter = match providers {
        Some(providers) => rows_filter(node_did, providers, None),
        None => format!(
            r#"{{ node_did: {{ _eq: "{}" }} }}"#,
            gents_store::graphql::escape_graphql_string(node_did)
        ),
    };
    format!("query {{ OAuthCredential(filter: {filter}) {{ {OAUTH_SUMMARY_FIELDS} }} }}")
}

async fn list_summaries(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    providers: Option<&[&str]>,
) -> Result<Vec<OAuthCredentialSummary>> {
    let response = access
        .execute(&account_listing_query(node_did, providers))
        .await?;
    let mut rows = oauth_credential_summaries_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    /* the resolver's order */
    rows.sort_by(|a, b| {
        (a.connected_at, &a.credential_id).cmp(&(b.connected_at, &b.credential_id))
    });
    Ok(rows)
}

/// Every row the node owns, in the resolver's order.
pub async fn list_stored_accounts(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> Result<Vec<StoredAccount>> {
    let rows = list_summaries(access, node_did, None).await?;
    Ok(rows.iter().map(StoredAccount::from).collect())
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
    crate::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
    crate::auth::claude::CLAUDE_OAUTH_PROVIDER,
    crate::clients::xai_grok::XAI_OAUTH_PROVIDER,
];

fn account_filter(node_did: &str, credential_id: Option<&str>) -> String {
    rows_filter(node_did, &ACCOUNT_PROVIDERS, credential_id)
}

fn rows_filter(node_did: &str, providers: &[&str], credential_id: Option<&str>) -> String {
    let providers = providers
        .iter()
        .map(|provider| format!("\"{provider}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let credential = credential_id
        .map(|id| {
            format!(
                r#", credential_id: {{ _eq: "{}" }}"#,
                gents_store::graphql::escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    format!(
        r#"{{ node_did: {{ _eq: "{}" }}, provider: {{ _in: [{providers}] }}{credential} }}"#,
        gents_store::graphql::escape_graphql_string(node_did)
    )
}

fn account_not_found(credential_id: &str) -> anyhow::Error {
    anyhow::anyhow!("account {credential_id:?} not found for this node")
}

pub async fn list_accounts(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> Result<Vec<AccountSummary>> {
    let rows = list_summaries(access, node_did, Some(&ACCOUNT_PROVIDERS)).await?;
    /* each provider's default is its first enabled row in the resolver's order */
    let defaults: Vec<_> = ACCOUNT_PROVIDERS
        .iter()
        .filter_map(|provider| {
            rows.iter()
                .find(|row| row.provider == *provider && row.enabled)
        })
        .map(|row| row.credential_id.clone())
        .collect();
    Ok(rows
        .iter()
        .map(|row| AccountSummary {
            credential_id: row.credential_id.clone(),
            provider: row.provider.clone(),
            account_ref: row.account_ref.clone(),
            label: row.effective_label(),
            identity: row.identity(),
            plan: row.chatgpt_plan_type.clone(),
            enabled: row.enabled,
            default: defaults.contains(&row.credential_id),
            access_token_expires_at: row.access_token_expires_at,
            connected_at: row.connected_at,
        })
        .collect())
}

pub async fn set_account_label(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    credential_id: &str,
    label: &str,
) -> Result<()> {
    crate::operations::admit_account_operation_on(
        access,
        node_did,
        gents_protocol::operations::inference::AccountOperation::Label,
    )
    .await?;
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
                super::rows::update_credential_patch_in_txn(
                    txn,
                    doc_id,
                    serde_json::json!({"label": label}),
                )
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
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    credential_id: &str,
    enabled: bool,
) -> Result<()> {
    crate::operations::admit_account_operation_on(
        access,
        node_did,
        gents_protocol::operations::inference::AccountOperation::Disable,
    )
    .await?;
    access
        .transact_write("oauth_credential.enabled", |txn| {
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "query {{ OAuthCredential(filter: {}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}",
                        account_filter(node_did, Some(credential_id)),
                    ))
                    .await?;
                let row = oauth_credentials_from_response(&response)
                    .into_iter()
                    .next()
                    .ok_or_else(|| account_not_found(credential_id))??;
                let doc_id = row
                    .doc_id
                    .as_deref()
                    .context("stored OAuthCredential has no _docID")?;
                super::rows::update_credential_patch_in_txn(
                    txn,
                    doc_id,
                    serde_json::json!({"enabled": enabled}),
                )
                .await
            })
        })
        .await
}

pub async fn remove_account_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    credential_id: &str,
) -> Result<()> {
    let filter = account_filter(node_did, Some(credential_id));
    let response = txn
        .execute(&format!(
            "query {{ OAuthCredential(filter: {filter}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}"
        ))
        .await?;
    let row = oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .ok_or_else(|| account_not_found(credential_id))??;
    let doc_id = row
        .doc_id
        .as_deref()
        .context("stored OAuthCredential has no _docID")?;
    txn.delete_configuration_document(gents_protocol::schemas::OAUTH_CREDENTIAL_NAME, doc_id)
        .await?;
    Ok(())
}

#[cfg(test)]
mod listing_tests {
    use super::*;

    /// What a listing selects, and how it names the account, are the Lean
    /// model's (`AccountListing.Contracts.listing`).
    #[test]
    fn a_listing_matches_the_lean_model() {
        use gents_lean_vocab::families::account_listing_contracts_listing as family;
        family::replay(|input: &family::Input| {
            let summary = OAuthCredentialSummary {
                credential_id: "credential".to_owned(),
                node_did: "did:key:node".to_owned(),
                provider: "chatgpt-codex".to_owned(),
                account_id: input.account_id.clone(),
                account_email: input.account_email.clone(),
                chatgpt_plan_type: None,
                access_token_expires_at: DateTime::default(),
                last_refresh: None,
                enabled: true,
                account_ref: None,
                connected_at: None,
                provider_account_key: input.key.clone(),
                label: None,
            };
            family::Listing {
                selected: OAUTH_SUMMARY_FIELDS
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect(),
                identity: summary.identity(),
            }
        });
    }

    /// Both listing queries select exactly those fields.
    #[test]
    fn every_listing_query_selects_the_summary_fields() {
        for query in [
            account_listing_query("did:key:node", None),
            account_listing_query("did:key:node", Some(&ACCOUNT_PROVIDERS)),
        ] {
            assert!(
                query.contains(&format!("{{ {OAUTH_SUMMARY_FIELDS} }}")),
                "{query}"
            );
        }
    }
}
