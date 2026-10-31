mod account_backend;
mod accounts;
mod auth_error;
mod bearer;
mod document;
mod labels;
mod resolve;
mod rows;
mod serving;
mod sign_in;
pub(crate) mod token_endpoint;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

pub(crate) use account_backend::preset_account_backend;
use account_backend::sync_account_backend;
pub use accounts::{
    enabled_accounts_note, list_accounts, no_switch_candidate_note, remove_account_in_txn,
    set_account_enabled, set_account_label, AccountSummary,
};
use auth_error::MOVE_OFF_ACCOUNT;
pub(crate) use auth_error::{account_failure, OAuthCredentialAuthError, OAuthCredentialError};
pub use auth_error::{
    account_failure_state, classify_chatgpt_auth_error, classify_oauth_auth_error, OAuthAuthProblem,
};
pub use bearer::{
    apply_refreshed_tokens, shared_bearer, token_is_fresh, BearerSource, DbCredentialBearer,
};
use document::{
    enabled_oauth_credentials_query, oauth_credential_input_fields,
    oauth_credentials_of_provider_query, pick_row_from_value, refresh_update_mutation,
    render_oauth_input, OAUTH_CREDENTIAL_FIELDS, OAUTH_PICK_FIELDS, SET_ONCE_FIELDS,
};
pub use document::{
    oauth_credential_by_doc_id_query, oauth_credential_by_id_query,
    oauth_credential_upsert_mutation, oauth_credentials_for_node_query,
    oauth_credentials_from_response,
};
pub use labels::{
    account_display_label, effective_account_label, provider_name, validate_account_label,
};
use labels::{ensure_label_free, next_free_label, sign_in_product};
use resolve::resolver_order;
pub use resolve::{
    pick_oauth_credential, pick_oauth_pool, resolve_oauth_credential, resolve_oauth_pool,
    AccountPick,
};
pub(crate) use resolve::{provider_default_account_ref, resolve_oauth_credential_in_txn};
use rows::provider_rows_in_txn;
pub use rows::{
    list_oauth_credentials, list_oauth_credentials_on, lookup_oauth_credential_by_doc_id,
    lookup_oauth_credential_by_id, upsert_oauth_credential, upsert_oauth_credential_on,
};
pub use serving::{backend_account, serving_account, AccountState, ServingAccount};
pub use sign_in::{store_sign_in, SignIn, SignInResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthProduct {
    pub name: &'static str,
    pub backend_label: &'static str,
    pub login_command: &'static str,
    pub not_entitled_guidance: &'static str,
}

pub const CHATGPT_OAUTH_PRODUCT: OAuthProduct = OAuthProduct {
    name: "ChatGPT",
    backend_label: "ChatGPT subscription backend",
    login_command: "codex-login",
    not_entitled_guidance: "Use an API-key backend, or check the ChatGPT plan's Codex eligibility.",
};

pub const XAI_OAUTH_PRODUCT: OAuthProduct = OAuthProduct {
    name: "Grok",
    backend_label: "Grok subscription backend",
    login_command: "grok-login",
    not_entitled_guidance: "Use an API key with an OpenAI-compatible backend against \
                            https://api.x.ai/v1, or check SuperGrok / X Premium+ eligibility.",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthRefreshKind {
    ChatGpt,
    Claude,
    Xai,
    /// A Claude Code cloud gateway sign-in; the credential's `account_id` names the gateway.
    Gateway,
}

pub fn resolve_access_token_expiry(
    access_token: &str,
    expires_in: Option<i64>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    crate::chatgpt_oauth_refresh::jwt_expiration(access_token)
        .or_else(|| {
            expires_in
                .filter(|seconds| *seconds > 0)
                .map(|seconds| now + Duration::seconds(seconds))
        })
        .unwrap_or_else(|| now + Duration::minutes(15))
}

#[derive(Clone, PartialEq, Eq)]
pub struct RefreshedTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
    pub is_fedramp: bool,
    pub plan_type: Option<String>,
    pub access_token_expires_at: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    #[serde(default)]
    pub doc_id: Option<String>,
    pub credential_id: String,
    pub node_did: String,
    pub provider: String,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub chatgpt_plan_type: Option<String>,
    pub is_fedramp: bool,
    pub access_token_expires_at: DateTime<Utc>,
    #[serde(default)]
    pub last_refresh: Option<DateTime<Utc>>,
    pub enabled: bool,
    #[serde(default)]
    pub account_ref: Option<String>,
    #[serde(default)]
    pub connected_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub provider_account_key: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

const REDACTED: &str = "[redacted]";

/// Token values never reach logs or error text through `Debug`.
impl std::fmt::Debug for RefreshedTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshedTokens")
            .field("access_token", &REDACTED)
            .field("refresh_token", &REDACTED)
            .field("id_token", &self.id_token.as_ref().map(|_| REDACTED))
            .field("account_id", &self.account_id)
            .field("is_fedramp", &self.is_fedramp)
            .field("plan_type", &self.plan_type)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .finish()
    }
}

/// Token values never reach logs or error text through `Debug`.
impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("doc_id", &self.doc_id)
            .field("credential_id", &self.credential_id)
            .field("node_did", &self.node_did)
            .field("provider", &self.provider)
            .field("access_token", &REDACTED)
            .field("refresh_token", &REDACTED)
            .field("id_token", &self.id_token.as_ref().map(|_| REDACTED))
            .field("account_id", &self.account_id)
            .field("chatgpt_plan_type", &self.chatgpt_plan_type)
            .field("is_fedramp", &self.is_fedramp)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .field("last_refresh", &self.last_refresh)
            .field("enabled", &self.enabled)
            .field("account_ref", &self.account_ref)
            .field("connected_at", &self.connected_at)
            .field(
                "provider_account_key",
                &self.provider_account_key.as_ref().map(|_| REDACTED),
            )
            .field("label", &self.label)
            .finish()
    }
}

pub fn oauth_credential_id(node_did: &str, provider: &str) -> String {
    format!("{provider}:{node_did}")
}

#[cfg(test)]
mod tests;
pub fn oauth_credential_by_id_query(credential_id: &str) -> String {
    let credential_id = crate::store::graphql::escape_graphql_string(credential_id);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{ credential_id: {{ _eq: "{credential_id}" }} }},
                limit: 1
            ) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

const SET_ONCE_FIELDS: &[&str] = &["node_did", "account_ref", "connected_at", "label"];

pub fn oauth_credential_upsert_mutation(credential: &OAuthCredential) -> String {
    let fields = oauth_credential_input_fields(credential);
    let add_input = render_oauth_input(&fields, &[]);
    // `node_did` is immutable, so the update branch must omit it on this DefraDB pin.
    let update_input = render_oauth_input(&fields, SET_ONCE_FIELDS);
    let credential_id = crate::store::graphql::escape_graphql_string(&credential.credential_id);
    format!(
        r#"mutation {{
            upsert_OAuthCredential(
                filter: {{ credential_id: {{ _eq: "{credential_id}" }} }},
                add: {{
                    credential_id: "{credential_id}",
                    {add_input}
                }},
                update: {{
                    {update_input}
                }}
            ) {{ _docID }}
        }}"#
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPick<'a> {
    ProviderDefault,
    Reference(Option<&'a str>),
}

fn resolver_order(a: &OAuthCredential, b: &OAuthCredential) -> std::cmp::Ordering {
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

fn enabled_oauth_credentials_query(node_did: &str, provider: &str, fields: &str) -> String {
    let node_did = crate::store::graphql::escape_graphql_string(node_did);
    let provider = crate::store::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{
                    node_did: {{ _eq: "{node_did}" }},
                    provider: {{ _eq: "{provider}" }},
                    enabled: {{ _eq: true }}
                }}
            ) {{
                {fields}
            }}
        }}"#
    )
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

pub(crate) async fn provider_default_account_ref(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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

fn pick_row_from_value(value: Value) -> Result<OAuthCredential> {
    let row: OAuthCredentialRow =
        serde_json::from_value(value).context("decoding OAuthCredential row")?;
    Ok(OAuthCredential {
        doc_id: row.doc_id,
        credential_id: row.credential_id,
        node_did: required(row.node_did, "node_did")?,
        provider: required(row.provider, "provider")?,
        access_token: String::new(),
        refresh_token: String::new(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: DateTime::default(),
        last_refresh: None,
        enabled: row.enabled.unwrap_or(true),
        account_ref: clean_optional(row.account_ref),
        connected_at: parse_optional_datetime(row.connected_at, "connected_at")?,
        provider_account_key: clean_optional(row.provider_account_key),
        label: None,
    })
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

pub async fn lookup_oauth_credential_by_id(
    node: &EmbeddedNode,
    credential_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &oauth_credential_by_id_query(credential_id),
        "lookup_oauth_credential_by_id",
    )
    .await?;
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub fn oauth_credentials_for_node_query(node_did: &str) -> String {
    let node_did = crate::store::graphql::escape_graphql_string(node_did);
    format!(
        r#"query {{
            OAuthCredential(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub fn oauth_credential_by_doc_id_query(doc_id: &str) -> String {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
    format!(
        r#"query {{
            OAuthCredential(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub async fn list_oauth_credentials(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &oauth_credentials_for_node_query(node_did),
        "list_oauth_credentials",
    )
    .await?;
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .collect()
}

pub async fn list_oauth_credentials_on(
    access: &crate::store::config_client::ConfigAccess,
    node_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = access
        .execute(&oauth_credentials_for_node_query(node_did))
        .await?;
    let mut rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(resolver_order);
    Ok(rows)
}

pub async fn lookup_oauth_credential_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &oauth_credential_by_doc_id_query(doc_id),
        "lookup_oauth_credential_by_doc_id",
    )
    .await?;
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn upsert_oauth_credential(
    node: &std::sync::Arc<EmbeddedNode>,
    credential: &OAuthCredential,
) -> Result<String> {
    let mutation = oauth_credential_upsert_mutation(credential);
    let response = crate::store::config_client::ConfigAccess::write_local(
        node,
        "oauth_credential.upsert",
        &mutation,
    )
    .await?;
    Ok(gents_protocol::graphql::extract_mutation_doc_id(
        &response,
        "OAuthCredential",
    )?)
}

pub async fn upsert_oauth_credential_on(
    access: &crate::store::config_client::ConfigAccess,
    credential: &OAuthCredential,
) -> Result<String> {
    let mutation = oauth_credential_upsert_mutation(credential);
    let response = access.write("oauth_credential.upsert", &mutation).await?;
    Ok(gents_protocol::graphql::extract_mutation_doc_id(
        &response,
        "OAuthCredential",
    )?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInResult {
    Added,
    Refreshed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    pub doc_id: String,
    pub credential: OAuthCredential,
    pub result: SignInResult,
    pub identity_matched: bool,
    pub profiles: Vec<String>,
}

impl SignIn {
    pub fn account_chooser_hint(&self) -> Option<String> {
        (self.result == SignInResult::Refreshed && self.identity_matched).then(|| {
            let name = sign_in_product(&self.credential.provider)
                .map_or(self.credential.provider.as_str(), |product| product.name);
            format!(
                "This is the account already stored as {}. To add a different {name} account, \
                 sign out of {name} in the browser first or use a private window.",
                effective_account_label(&self.credential)
            )
        })
    }

    pub fn profiles_note(&self) -> Option<String> {
        (!self.profiles.is_empty()).then(|| {
            format!(
                "These profiles now use this account (their backend has no account \
                 reference): {}",
                self.profiles.join(", ")
            )
        })
    }
}

fn sign_in_product(provider: &str) -> Option<OAuthProduct> {
    match provider {
        crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER => {
            Some(CHATGPT_OAUTH_PRODUCT)
        }
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER => {
            Some(crate::inference::auth::claude::CLAUDE_OAUTH_PRODUCT)
        }
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER => Some(XAI_OAUTH_PRODUCT),
        _ => None,
    }
}

fn oauth_credentials_of_provider_query(node_did: &str, provider: &str) -> String {
    let node_did = crate::store::graphql::escape_graphql_string(node_did);
    let provider = crate::store::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                provider: {{ _eq: "{provider}" }}
            }}) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

async fn provider_rows_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = txn
        .execute(&oauth_credentials_of_provider_query(node_did, provider))
        .await?;
    let mut rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(resolver_order);
    Ok(rows)
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn account_backend(
    credential: &OAuthCredential,
    account_ref: &str,
) -> Result<crate::InferenceBackend> {
    preset_account_backend(
        &credential.node_did,
        &credential.provider,
        Some(account_ref),
        effective_account_label(credential),
    )
}

pub(crate) fn preset_account_backend(
    node_did: &str,
    provider: &str,
    account_ref: Option<&str>,
    name: String,
) -> Result<crate::InferenceBackend> {
    use crate::inference::backends::setup::{
        InferenceAuthMethod as Auth, InferenceProviderId as Id,
    };
    let (provider_id, auth) = match provider {
        crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER => {
            (Id::OpenAi, Auth::ChatGptOauth)
        }
        crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER => (Id::Anthropic, Auth::ClaudeOauth),
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER => (Id::Grok, Auth::GrokOauth),
        other => anyhow::bail!("no backend reads sign-ins of provider {other:?}"),
    };
    let spec = crate::inference::backends::setup::connection_spec(provider_id, auth, "")?;
    Ok(crate::InferenceBackend {
        node_did: node_did.to_owned(),
        backend_id: match account_ref {
            Some(account_ref) => format!("{provider}-{account_ref}"),
            None => provider.to_owned(),
        },
        name,
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        endpoint: spec.endpoint,
        auth: crate::documents::BackendAuth::NodeOAuth {
            account_ref: account_ref.map(str::to_owned),
        },
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: None,
        max_queue_depth: None,
        enabled: true,
        tags: Vec::new(),
    })
}

async fn write_account_backend(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    backend: &crate::InferenceBackend,
) -> Result<()> {
    crate::store::config_client::write_inference_backend_in_txn(txn, backend)
        .await
        .context(
            "the account's backend could not be stored, so the sign-in was not saved; \
             fix the node's configuration and sign in again",
        )
}

async fn sync_account_backend(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
    old_label: Option<&str>,
) -> Result<()> {
    let Some(account_ref) = credential.account_ref.as_deref() else {
        return Ok(());
    };
    let serving: Vec<_> =
        crate::store::config_client::list_inference_backends_in_txn(txn, &credential.node_did)
            .await?
            .into_iter()
            .filter(|backend| backend.auth.oauth_account_ref() == Some(account_ref))
            .collect();
    if serving.is_empty() {
        return write_account_backend(txn, &account_backend(credential, account_ref)?).await;
    }
    let label = effective_account_label(credential);
    for mut backend in serving {
        if old_label.is_some_and(|old| old == backend.name && old != label) {
            backend.name = label.clone();
            write_account_backend(txn, &backend).await?;
        }
    }
    Ok(())
}

const MAX_ACCOUNT_LABEL_CHARS: usize = 64;

pub fn validate_account_label(label: &str) -> Result<String> {
    let label = label.trim();
    anyhow::ensure!(!label.is_empty(), "an account label cannot be empty");
    anyhow::ensure!(
        label.chars().count() <= MAX_ACCOUNT_LABEL_CHARS,
        "an account label is at most {MAX_ACCOUNT_LABEL_CHARS} characters"
    );
    anyhow::ensure!(
        !label.chars().any(char::is_control),
        "an account label cannot contain control characters"
    );
    Ok(label.to_owned())
}

pub fn effective_account_label(credential: &OAuthCredential) -> String {
    credential.label.clone().unwrap_or_else(|| {
        sign_in_product(&credential.provider)
            .map_or(credential.provider.as_str(), |product| product.name)
            .to_owned()
    })
}

fn ensure_label_free(others: &[&OAuthCredential], label: &str) -> Result<()> {
    anyhow::ensure!(
        !others
            .iter()
            .any(|row| effective_account_label(row) == label),
        "the label {label:?} is already used by another account of this provider"
    );
    Ok(())
}

fn next_free_label(rows: &[OAuthCredential], provider: &str) -> String {
    let name = sign_in_product(provider).map_or(provider, |product| product.name);
    let taken: rapidhash::RapidHashSet<_> = rows.iter().map(effective_account_label).collect();
    std::iter::once(name.to_owned())
        .chain((2..).map(|n| format!("{name} {n}")))
        .find(|label| !taken.contains(label))
        .expect("an unbounded sequence has a free label")
}

pub async fn store_sign_in(
    access: &crate::store::config_client::ConfigAccess,
    credential: OAuthCredential,
    label: Option<&str>,
) -> Result<SignIn> {
    let label = label.map(validate_account_label).transpose()?;
    anyhow::ensure!(
        sign_in_product(&credential.provider).is_some(),
        "no backend reads sign-ins of provider {:?}",
        credential.provider
    );
    access
        .transact_write("oauth_credential.sign_in", |txn| {
            let credential = credential.clone();
            let label = label.clone();
            Box::pin(async move { store_sign_in_in_txn(txn, credential, label).await })
        })
        .await
}

fn identity_keys(credential: &OAuthCredential) -> Vec<String> {
    let keys = match credential.provider.as_str() {
        crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER => {
            crate::inference::auth::chatgpt_refresh::chatgpt_account_keys(
                &credential.access_token,
                credential.id_token.as_deref(),
                credential.account_id.as_deref(),
            )
        }
        crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER => {
            crate::inference::auth::xai_login::xai_account_key(&credential.access_token)
                .into_iter()
                .collect()
        }
        _ => credential.provider_account_key.iter().cloned().collect(),
    };
    keys.iter()
        .filter_map(|key| trimmed(Some(key)))
        .map(str::to_owned)
        .collect()
}

fn sign_in_match(
    row: &OAuthCredential,
    sign_in_keys: &[String],
    sign_in: &OAuthCredential,
) -> Option<u8> {
    let row_keys = identity_keys(row);
    let row_account = trimmed(row.account_id.as_deref());
    if !row_keys.is_empty() && !sign_in_keys.iter().any(|key| row_keys.contains(key)) {
        return None;
    }
    if row_account.is_some() && trimmed(sign_in.account_id.as_deref()) != row_account {
        return None;
    }
    Some(match (row_keys.is_empty(), row_account) {
        (false, _) => 0,
        (true, Some(_)) => 1,
        (true, None) => 2,
    })
}

async fn store_sign_in_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    mut credential: OAuthCredential,
    label: Option<String>,
) -> Result<SignIn> {
    let rows = provider_rows_in_txn(txn, &credential.node_did, &credential.provider).await?;
    let sign_in_keys = identity_keys(&credential);
    let matched = rows
        .iter()
        .filter_map(|row| Some((sign_in_match(row, &sign_in_keys, &credential)?, row)))
        .min_by_key(|(tier, _)| *tier);
    if let Some((tier, row)) = matched {
        if trimmed(row.provider_account_key.as_deref())
            .is_some_and(|stored| sign_in_keys.iter().any(|key| key == stored))
        {
            credential.provider_account_key = row.provider_account_key.clone();
        }
        credential.doc_id = row.doc_id.clone();
        credential.credential_id = row.credential_id.clone();
        credential.account_ref = row.account_ref.clone();
        credential.connected_at = row.connected_at;
        credential.label = row.label.clone();
        credential.enabled = true;
        let old_label = effective_account_label(row);
        let mut set_once = SET_ONCE_FIELDS.to_vec();
        if let Some(label) = label {
            let others: Vec<_> = rows
                .iter()
                .filter(|other| other.doc_id != row.doc_id)
                .collect();
            ensure_label_free(&others, &label)?;
            credential.label = Some(label);
            set_once.retain(|field| *field != "label");
        }
        let doc_id = row
            .doc_id
            .clone()
            .context("stored OAuthCredential has no _docID")?;
        let fields = oauth_credential_input_fields(&credential);
        let mutation = format!(
            r#"mutation {{
                update_OAuthCredential(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{
                    {}
                }}) {{ _docID }}
            }}"#,
            crate::store::graphql::escape_graphql_string(&doc_id),
            render_oauth_input(&fields, &set_once),
        );
        txn.execute(&mutation).await?;
        sync_account_backend(txn, &credential, Some(&old_label)).await?;
        return Ok(SignIn {
            doc_id,
            credential,
            result: SignInResult::Refreshed,
            identity_matched: tier < 2,
            profiles: Vec::new(),
        });
    }
    let now = Utc::now();
    credential.connected_at = DateTime::from_timestamp(now.timestamp(), 0);
    if rows.is_empty() {
        credential.account_ref = None;
        credential.credential_id = oauth_credential_id(&credential.node_did, &credential.provider);
    } else {
        let account_ref = uuid::Uuid::new_v4().simple().to_string();
        credential.credential_id = format!(
            "{}:{}:{account_ref}",
            credential.provider, credential.node_did
        );
        credential.account_ref = Some(account_ref);
    }
    credential.enabled = true;
    credential.label = match label {
        Some(label) => {
            ensure_label_free(&rows.iter().collect::<Vec<_>>(), &label)?;
            Some(label)
        }
        None if rows.is_empty() => None,
        None => Some(next_free_label(&rows, &credential.provider)),
    };
    let fields = oauth_credential_input_fields(&credential);
    let mutation = format!(
        r#"mutation {{
            create_OAuthCredential(input: {{
                credential_id: "{}",
                {}
            }}) {{ _docID }}
        }}"#,
        crate::store::graphql::escape_graphql_string(&credential.credential_id),
        render_oauth_input(&fields, &[]),
    );
    let response = txn.execute(&mutation).await?;
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")?;
    credential.doc_id = Some(doc_id.clone());
    sync_account_backend(txn, &credential, None).await?;
    let profiles = if rows.is_empty() {
        original_account_profiles(txn, &credential).await?
    } else {
        Vec::new()
    };
    Ok(SignIn {
        doc_id,
        credential,
        result: SignInResult::Added,
        identity_matched: false,
        profiles,
    })
}

async fn original_account_profiles(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
) -> Result<Vec<String>> {
    use crate::inference::backends::provider::BackendProviderOauthExt;
    let backends: Vec<_> =
        crate::store::config_client::list_inference_backends_in_txn(txn, &credential.node_did)
            .await?
            .into_iter()
            .filter(|backend| {
                matches!(
                    backend.auth,
                    crate::documents::BackendAuth::NodeOAuth { account_ref: None }
                ) && backend.provider_kind.oauth_provider() == Some(credential.provider.as_str())
            })
            .map(|backend| backend.backend_id)
            .collect();
    Ok(
        crate::store::config_client::list_inference_profiles_in_txn(txn, &credential.node_did)
            .await?
            .into_iter()
            .filter(|profile| backends.contains(&profile.backend_id))
            .map(|profile| profile.profile_id)
            .collect(),
    )
}

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

pub fn backend_account<'a>(
    backend: &crate::InferenceBackend,
    accounts: &'a [AccountSummary],
) -> Option<Option<&'a AccountSummary>> {
    use crate::inference::backends::provider::BackendProviderOauthExt;
    let crate::documents::BackendAuth::NodeOAuth { account_ref } = &backend.auth else {
        return None;
    };
    let provider = backend.provider_kind.oauth_provider()?;
    Some(
        accounts
            .iter()
            .find(|account| account.provider == provider && account.account_ref == *account_ref),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    Enabled,
    Disabled,
    Missing,
}

impl AccountState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Missing => "account not on this node",
        }
    }
}

impl Serialize for AccountState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServingAccount {
    pub label: String,
    pub state: AccountState,
    #[serde(skip)]
    pub provider: Option<&'static str>,
}

pub fn serving_account(
    backend: &crate::InferenceBackend,
    accounts: &[AccountSummary],
) -> ServingAccount {
    let state = |enabled| {
        if enabled {
            AccountState::Enabled
        } else {
            AccountState::Disabled
        }
    };
    use crate::inference::backends::provider::BackendProviderOauthExt;
    let provider = matches!(
        backend.auth,
        crate::documents::BackendAuth::NodeOAuth { .. }
    )
    .then(|| backend.provider_kind.oauth_provider())
    .flatten();
    let (label, state) = match backend_account(backend, accounts) {
        None => (backend.name.clone(), state(backend.enabled)),
        Some(None) => (backend.name.clone(), AccountState::Missing),
        Some(Some(account)) => (account.label.clone(), state(account.enabled)),
    };
    ServingAccount {
        label,
        state,
        provider,
    }
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

pub fn provider_name(provider: &str) -> &str {
    sign_in_product(provider).map_or(provider, |product| product.name)
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
    crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
    crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
    crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER,
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
                crate::store::graphql::escape_graphql_string(id)
            )
        })
        .unwrap_or_default();
    format!(
        r#"{{ node_did: {{ _eq: "{}" }}, provider: {{ _in: [{providers}] }}{credential} }}"#,
        crate::store::graphql::escape_graphql_string(node_did)
    )
}

fn account_not_found(credential_id: &str) -> anyhow::Error {
    anyhow::anyhow!("account {credential_id:?} not found for this node")
}

pub async fn list_accounts(
    access: &crate::store::config_client::ConfigAccess,
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
    access: &crate::store::config_client::ConfigAccess,
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
                    crate::store::graphql::escape_graphql_string(doc_id),
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
    access: &crate::store::config_client::ConfigAccess,
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
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
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
        .is_some_and(crate::store::graphql::response_has_documents)
    {
        Ok(())
    } else {
        Err(account_not_found(credential_id))
    }
}

pub fn oauth_credentials_from_response(response: &Value) -> Vec<Result<OAuthCredential>> {
    gents_protocol::graphql::graphql_rows_from_response(response, "OAuthCredential")
        .into_iter()
        .map(oauth_credential_from_value)
        .collect()
}

fn oauth_credential_input_fields(credential: &OAuthCredential) -> Vec<(&'static str, String)> {
    let field = |name: &str, value: &str| {
        format!(
            r#"{name}: "{}""#,
            crate::store::graphql::escape_graphql_string(value)
        )
    };
    let datetime_field = |name: &str, value: Option<DateTime<Utc>>| {
        value
            .map(|value| {
                format!(
                    r#"{name}: "{}""#,
                    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                )
            })
            .unwrap_or_else(|| format!("{name}: null"))
    };
    vec![
        ("node_did", field("node_did", &credential.node_did)),
        ("provider", field("provider", &credential.provider)),
        (
            "access_token",
            field("access_token", &credential.access_token),
        ),
        (
            "refresh_token",
            field("refresh_token", &credential.refresh_token),
        ),
        (
            "id_token",
            gents_protocol::graphql::nullable_string_field(
                "id_token",
                credential.id_token.as_deref(),
            ),
        ),
        (
            "account_id",
            gents_protocol::graphql::nullable_string_field(
                "account_id",
                credential.account_id.as_deref(),
            ),
        ),
        (
            "chatgpt_plan_type",
            gents_protocol::graphql::nullable_string_field(
                "chatgpt_plan_type",
                credential.chatgpt_plan_type.as_deref(),
            ),
        ),
        (
            "is_fedramp",
            format!(
                "is_fedramp: {}",
                gents_protocol::graphql::graphql_bool_literal(credential.is_fedramp)
            ),
        ),
        (
            "access_token_expires_at",
            datetime_field(
                "access_token_expires_at",
                Some(credential.access_token_expires_at),
            ),
        ),
        (
            "last_refresh",
            datetime_field("last_refresh", credential.last_refresh),
        ),
        (
            "enabled",
            format!(
                "enabled: {}",
                gents_protocol::graphql::graphql_bool_literal(credential.enabled)
            ),
        ),
        (
            "account_ref",
            gents_protocol::graphql::nullable_string_field(
                "account_ref",
                credential.account_ref.as_deref(),
            ),
        ),
        (
            "connected_at",
            datetime_field("connected_at", credential.connected_at),
        ),
        (
            "provider_account_key",
            gents_protocol::graphql::nullable_string_field(
                "provider_account_key",
                credential.provider_account_key.as_deref(),
            ),
        ),
        (
            "label",
            gents_protocol::graphql::nullable_string_field("label", credential.label.as_deref()),
        ),
    ]
}

fn render_oauth_input(fields: &[(&'static str, String)], exclude: &[&str]) -> String {
    fields
        .iter()
        .filter(|(name, _)| !exclude.contains(name))
        .map(|(_, rendered)| rendered.as_str())
        .collect::<Vec<_>>()
        .join(",\n                    ")
}

pub(crate) fn oauth_credential_from_value(value: Value) -> Result<OAuthCredential> {
    let row: OAuthCredentialRow =
        serde_json::from_value(value).context("decoding OAuthCredential row")?;
    let access_token = required(row.access_token, "access_token")?;
    let refresh_token = required(row.refresh_token, "refresh_token")?;
    Ok(OAuthCredential {
        doc_id: row.doc_id,
        credential_id: row.credential_id,
        node_did: required(row.node_did, "node_did")?,
        provider: required(row.provider, "provider")?,
        access_token,
        refresh_token,
        id_token: clean_optional(row.id_token),
        account_id: clean_optional(row.account_id),
        chatgpt_plan_type: clean_optional(row.chatgpt_plan_type),
        is_fedramp: row.is_fedramp.unwrap_or(false),
        access_token_expires_at: parse_required_datetime(
            row.access_token_expires_at,
            "access_token_expires_at",
        )?,
        last_refresh: parse_optional_datetime(row.last_refresh, "last_refresh")?,
        enabled: row.enabled.unwrap_or(true),
        account_ref: clean_optional(row.account_ref),
        connected_at: parse_optional_datetime(row.connected_at, "connected_at")?,
        provider_account_key: clean_optional(row.provider_account_key),
        label: clean_optional(row.label),
    })
}

fn required(value: Option<String>, field: &str) -> Result<String> {
    clean_optional(value).ok_or_else(|| anyhow::anyhow!("OAuthCredential missing {field}"))
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(value)
    })
}

fn parse_required_datetime(value: Option<String>, field: &str) -> Result<DateTime<Utc>> {
    parse_optional_datetime(value, field)?
        .ok_or_else(|| anyhow::anyhow!("OAuthCredential missing {field}"))
}

fn parse_optional_datetime(value: Option<String>, field: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .with_context(|| format!("parsing OAuthCredential {field} timestamp {value}"))
        })
        .transpose()
}

pub fn token_is_fresh(expires_at: DateTime<Utc>) -> bool {
    Utc::now() + REFRESH_SKEW < expires_at
}

pub fn apply_refreshed_tokens(credential: &mut OAuthCredential, refreshed: RefreshedTokens) {
    credential.access_token = refreshed.access_token;
    credential.refresh_token = refreshed.refresh_token;
    if refreshed.id_token.is_some() {
        credential.id_token = refreshed.id_token;
    }
    if refreshed.account_id.is_some() {
        credential.account_id = refreshed.account_id;
    }
    if refreshed.plan_type.is_some() {
        credential.chatgpt_plan_type = refreshed.plan_type;
    }
    credential.is_fedramp = refreshed.is_fedramp || credential.is_fedramp;
    credential.access_token_expires_at = refreshed.access_token_expires_at;
    credential.last_refresh = Some(Utc::now());
}

fn provider_account_key(kind: OAuthRefreshKind, credential: &OAuthCredential) -> Option<String> {
    match kind {
        OAuthRefreshKind::ChatGpt => crate::inference::auth::chatgpt_refresh::chatgpt_account_key(
            &credential.access_token,
            credential.id_token.as_deref(),
            credential.account_id.as_deref(),
        ),
        OAuthRefreshKind::Xai => {
            crate::inference::auth::xai_login::xai_account_key(&credential.access_token)
        }
        OAuthRefreshKind::Claude => None,
    }
}

pub(crate) fn get_or_insert_arc<T>(
    registry: &std::sync::Mutex<rapidhash::RapidHashMap<String, Arc<T>>>,
    key: &str,
    make: impl FnOnce() -> T,
) -> Arc<T> {
    let mut map = registry.lock().expect("bearer registry mutex poisoned");
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(make()))
        .clone()
}

fn bearer_registry(
) -> &'static std::sync::Mutex<rapidhash::RapidHashMap<String, Arc<DbCredentialBearer>>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<rapidhash::RapidHashMap<String, Arc<DbCredentialBearer>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(rapidhash::RapidHashMap::default()))
}

pub fn shared_bearer(
    credential_id: &str,
    make: impl FnOnce() -> DbCredentialBearer,
) -> Arc<DbCredentialBearer> {
    get_or_insert_arc(bearer_registry(), credential_id, make)
}

pub trait BearerSource: Send + Sync {
    fn current_bearer(&self) -> impl Future<Output = Result<String>> + Send;

    fn invalidate(&self) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn is_oauth(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshPersist {
    Written,
    Superseded,
}

const REFRESH_FIELDS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "account_id",
    "chatgpt_plan_type",
    "is_fedramp",
    "access_token_expires_at",
    "last_refresh",
];

fn refresh_update_mutation(
    credential: &OAuthCredential,
    stored_refresh_token: &str,
    key_filled: bool,
) -> String {
    let document = match &credential.doc_id {
        Some(doc_id) => format!(
            r#"_docID: {{ _eq: "{}" }}"#,
            crate::store::graphql::escape_graphql_string(doc_id)
        ),
        None => format!(
            r#"credential_id: {{ _eq: "{}" }}"#,
            crate::store::graphql::escape_graphql_string(&credential.credential_id)
        ),
    };
    let fields: Vec<_> = oauth_credential_input_fields(credential)
        .into_iter()
        .filter(|(name, _)| {
            REFRESH_FIELDS.contains(name) || (key_filled && *name == "provider_account_key")
        })
        .collect();
    format!(
        r#"mutation {{
            update_OAuthCredential(
                filter: {{ {document}, refresh_token: {{ _eq: "{}" }} }},
                input: {{
                    {}
                }}
            ) {{ _docID }}
        }}"#,
        crate::store::graphql::escape_graphql_string(stored_refresh_token),
        render_oauth_input(&fields, &[]),
    )
}

const MOVE_OFF_ACCOUNT: &str =
    "Move a profile off it: gents config profile set-account <profile> <account> (gents accounts list).";

pub(crate) fn account_failure(product: &OAuthProduct, label: &str, state: &str) -> String {
    let lower = label.to_ascii_lowercase();
    let label = if gents_loop::provider_limit::classify_provider_limit(label, Utc::now()).is_some()
        || lower.contains("context_length_exceeded")
        || lower.contains("maximum context length")
    {
        product.name
    } else {
        label
    };
    format!("{} account \"{label}\" is {state}.", product.name)
}

/// The account state an `account_unusable_message` rendering reports.
///
/// External premise: a blocked turn is read back from the persisted
/// AgentToolCall `failure_reason`, the only durable record of the failure;
/// the schema carries no account-state field, so the state is recovered from
/// that rendering.
pub fn account_failure_state(text: &str) -> Option<&'static str> {
    const STATES: [&str; 5] = [
        "signed out (expired or revoked)",
        "not entitled",
        "unusable",
        "disabled",
        "removed from this node",
    ];
    [
        CHATGPT_OAUTH_PRODUCT,
        XAI_OAUTH_PRODUCT,
        crate::inference::auth::claude::CLAUDE_OAUTH_PRODUCT,
    ]
    .iter()
    .any(|product| text.contains(&format!("{} account \"", product.name)))
    .then(|| {
        STATES
            .into_iter()
            .find(|state| text.contains(&format!("\" is {state}.")))
    })
    .flatten()
}

/// Bounds each token-endpoint exchange, not the persist that follows it: a hung endpoint must
/// fail refresh instead of stalling callers, while cancelling after a response could lose a
/// rotated refresh token.
const REFRESH_REQUEST_TIMEOUT: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_secs(5)
} else {
    std::time::Duration::from_secs(30)
};

pub struct DbCredentialBearer {
    node: Arc<EmbeddedNode>,
    node_did: String,
    provider: String,
    credential_id: String,
    http: reqwest::Client,
    cache: Mutex<Option<OAuthCredential>>,
    refresh_lock: Mutex<Option<(std::time::Instant, OAuthAuthProblem)>>,
    is_owner: bool,
    force_refresh: AtomicBool,
    refresh_kind: OAuthRefreshKind,
    product: OAuthProduct,
}

impl DbCredentialBearer {
    pub fn new(
        node: Arc<EmbeddedNode>,
        node_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
        refresh_kind: OAuthRefreshKind,
        product: OAuthProduct,
    ) -> Self {
        Self::with_cache(
            node,
            node_did,
            provider,
            credential_id,
            is_owner,
            None,
            refresh_kind,
            product,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_cache(
        node: Arc<EmbeddedNode>,
        node_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
        cache_seed: Option<OAuthCredential>,
        refresh_kind: OAuthRefreshKind,
        product: OAuthProduct,
    ) -> Self {
        Self {
            node,
            node_did: node_did.into(),
            provider: provider.into(),
            credential_id: credential_id.into(),
            http: reqwest::Client::builder()
                .timeout(REFRESH_REQUEST_TIMEOUT)
                .build()
                .expect("reqwest client with a timeout builds like Client::new()"),
            cache: Mutex::new(cache_seed),
            refresh_lock: Mutex::new(None),
            is_owner,
            force_refresh: AtomicBool::new(false),
            refresh_kind,
            product,
        }
    }

    async fn load_credential(&self) -> std::result::Result<OAuthCredential, OAuthCredentialError> {
        let Some(credential) =
            lookup_oauth_credential_by_id(self.node.as_ref(), &self.credential_id).await?
        else {
            let label = self.cached().await.map_or_else(
                || self.product.name.to_owned(),
                |cached| effective_account_label(&cached),
            );
            return Err(OAuthCredentialAuthError {
                problem: OAuthAuthProblem::Missing,
                message: format!(
                    "{} {MOVE_OFF_ACCOUNT} Signing in again adds a new account; it does not restore \
                     this one.",
                    account_failure(&self.product, &label, "removed from this node"),
                ),
            }
            .into());
        };
        if !credential.enabled {
            return Err(OAuthCredentialAuthError {
                problem: OAuthAuthProblem::WrongMode {
                    found_mode: "disabled".into(),
                },
                message: format!(
                    "{}\n{}\n{MOVE_OFF_ACCOUNT}",
                    account_failure(
                        &self.product,
                        &effective_account_label(&credential),
                        "disabled"
                    ),
                    classify_oauth_auth_error(
                        &self.product,
                        &self.node_did,
                        &self.provider,
                        &OAuthAuthProblem::WrongMode {
                            found_mode: "disabled".to_string(),
                        },
                    )
                ),
            }
            .into());
        }
        Ok(credential)
    }

    async fn cached(&self) -> Option<OAuthCredential> {
        self.cache.lock().await.clone()
    }

    async fn cache_credential(&self, credential: &OAuthCredential) {
        *self.cache.lock().await = Some(credential.clone());
    }

    pub(crate) async fn adopt_document(&self, stored: &OAuthCredential) {
        let mut cache = self.cache.lock().await;
        if cache
            .as_ref()
            .is_some_and(|cached| cached.doc_id != stored.doc_id)
        {
            *cache = Some(stored.clone());
        }
    }

    /// Persist rotated fields only while the stored refresh token still matches the one read.
    /// A sign-in, removal, or replacement since that read must not be overwritten or revived.
    async fn persist_with_retry(
        &self,
        credential: &OAuthCredential,
        stored_refresh_token: &str,
        key_filled: bool,
    ) -> Result<RefreshPersist> {
        let mutation = refresh_update_mutation(credential, stored_refresh_token, key_filled);
        let mut last_error = None;
        let mut delay_ms = 200u64;
        for attempt in 0..3u32 {
            match crate::store::config_client::ConfigAccess::write_local(
                &self.node,
                "oauth_credential.refresh",
                &mutation,
            )
            .await
            {
                Ok(response) => {
                    let written = response
                        .get("data")
                        .and_then(|data| data.get("update_OAuthCredential"))
                        .is_some_and(crate::store::graphql::response_has_documents);
                    return Ok(if written {
                        RefreshPersist::Written
                    } else {
                        RefreshPersist::Superseded
                    });
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2;
                    }
                }
            }
        }
        Err(last_error
            .expect("persist_with_retry ran at least one failing attempt")
            .into())
    }

    fn auth_error(
        &self,
        credential: &OAuthCredential,
        problem: &OAuthAuthProblem,
    ) -> OAuthCredentialAuthError {
        let state = match problem {
            OAuthAuthProblem::Expired => "signed out (expired or revoked)",
            OAuthAuthProblem::NotEntitled => "not entitled",
            _ => "unusable",
        };
        OAuthCredentialAuthError {
            problem: problem.clone(),
            message: format!(
                "{}\n{}\n{MOVE_OFF_ACCOUNT}",
                account_failure(&self.product, &effective_account_label(credential), state),
                classify_oauth_auth_error(&self.product, &self.node_did, &self.provider, problem),
            ),
        }
    }

    async fn refresh_tokens(
        &self,
        refresh_token: &str,
    ) -> Result<RefreshedTokens, OAuthAuthProblem> {
        match self.refresh_kind {
            OAuthRefreshKind::ChatGpt => {
                crate::inference::auth::chatgpt_refresh::refresh_chatgpt_token(
                    refresh_token,
                    &self.http,
                )
                .await
            }
            OAuthRefreshKind::Claude => {
                crate::inference::auth::claude_refresh::refresh_claude_token(
                    refresh_token,
                    &self.http,
                )
                .await
            }
            OAuthRefreshKind::Xai => {
                crate::inference::auth::xai_refresh::refresh_xai_token(refresh_token, &self.http)
                    .await
            }
        }
    }
}

impl DbCredentialBearer {
    pub(crate) async fn current_bearer_typed(
        &self,
    ) -> std::result::Result<String, OAuthCredentialError> {
        let forced = self.force_refresh.load(Ordering::SeqCst);

        if !forced {
            if let Some(cred) = self.cached().await {
                if token_is_fresh(cred.access_token_expires_at) {
                    return Ok(cred.access_token);
                }
            }
        }

        let mut last_failure = self.refresh_lock.lock().await;

        let forced = self.force_refresh.load(Ordering::SeqCst);

        if !forced {
            if let Some(cred) = self.cached().await {
                if token_is_fresh(cred.access_token_expires_at) {
                    return Ok(cred.access_token);
                }
            }
        }

        let mut credential = match self.cached().await {
            Some(cred) => cred,
            None => {
                let cred = self.load_credential().await?;
                self.cache_credential(&cred).await;
                cred
            }
        };
        if !forced && token_is_fresh(credential.access_token_expires_at) {
            return Ok(credential.access_token);
        }

        if !self.is_owner {
            self.force_refresh.store(false, Ordering::SeqCst);
            return Ok(credential.access_token);
        }

        let db_credential = self.load_credential().await?;
        let stored_refresh_token = db_credential.refresh_token.clone();
        if db_credential.doc_id != credential.doc_id
            || db_credential.access_token_expires_at >= credential.access_token_expires_at
        {
            credential = db_credential;
            if !forced && token_is_fresh(credential.access_token_expires_at) {
                self.cache_credential(&credential).await;
                return Ok(credential.access_token);
            }
        }

        if let Some((failed_at, problem)) = last_failure.as_ref() {
            if failed_at.elapsed() < REFRESH_FAILURE_COOLDOWN {
                return Err(self.auth_error(&credential, problem).into());
            }
        }

        let refreshed = match self.refresh_tokens(&credential.refresh_token).await {
            Ok(refreshed) => refreshed,
            Err(problem) => {
                let error = self.auth_error(&credential, &problem);
                *last_failure = Some((std::time::Instant::now(), problem));
                return Err(error.into());
            }
        };
        *last_failure = None;
        apply_refreshed_tokens(&mut credential, refreshed);
        let key_filled = credential.provider_account_key.is_none() && {
            credential.provider_account_key = provider_account_key(self.refresh_kind, &credential);
            credential.provider_account_key.is_some()
        };

        self.cache_credential(&credential).await;
        self.force_refresh.store(false, Ordering::SeqCst);
        match self
            .persist_with_retry(&credential, &stored_refresh_token, key_filled)
            .await
        {
            Ok(RefreshPersist::Written) => {}
            Ok(RefreshPersist::Superseded) => {
                *self.cache.lock().await = None;
                tracing::debug!(
                    node_did = %self.node_did,
                    credential_id = %self.credential_id,
                    product = self.product.name,
                    "the stored account changed while its token refreshed; the stored row stands \
                     and this refreshed token is served once"
                )
            }
            Err(error) => tracing::error!(
                node_did = %self.node_did,
                credential_id = %self.credential_id,
                product = self.product.name,
                %error,
                "failed to persist rotated OAuth token to DefraDB after retries; serving \
                 the rotated token from memory. It must be re-persisted before this process exits \
                 or the rotated refresh token will be lost."
            ),
        }
        Ok(credential.access_token)
    }
}

impl BearerSource for DbCredentialBearer {
    async fn current_bearer(&self) -> Result<String> {
        self.current_bearer_typed().await.map_err(Into::into)
    }

    async fn invalidate(&self) {
        self.force_refresh.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod serving_account_tests;

#[cfg(test)]
mod resolver_tests;

#[cfg(test)]
mod sign_in_tests;

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod backfill_tests;

#[cfg(test)]
mod cooldown_tests;

#[cfg(test)]
pub(crate) mod test_support;
