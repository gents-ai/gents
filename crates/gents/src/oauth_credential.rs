//! Provider-agnostic OAuth credential documents and owner-only bearer refresh.
//!
//! ChatGPT Codex and Grok / xAI subscription OAuth both store tokens as
//! `OAuthCredential` rows and share this cache + refresh-lock shell. Provider
//! differences live in [`OAuthRefreshKind`] and product-specific HTTP clients.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::row::OAuthCredentialRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

const OAUTH_CREDENTIAL_FIELDS: &str = "_docID credential_id agent_did provider access_token refresh_token id_token account_id chatgpt_plan_type is_fedramp access_token_expires_at last_refresh enabled account_ref connected_at provider_account_key label";
/// The fields [`pick_oauth_credential`] reads: no token.
const OAUTH_PICK_FIELDS: &str = "credential_id agent_did provider enabled account_ref connected_at";

/// Display metadata only. Never use decoded, unverified claims for authorization
/// or overwrite the account ID used by a provider's authentication headers.
pub fn account_display_label(credential: &OAuthCredential) -> Option<String> {
    let claims = credential
        .id_token
        .as_deref()
        .and_then(crate::chatgpt_oauth_refresh::jwt_payload)
        .or_else(|| crate::chatgpt_oauth_refresh::jwt_payload(&credential.access_token));
    let field = |key: &str| {
        claims
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    };
    field("email")
        .or_else(|| credential.account_id.clone())
        .or_else(|| field("sub"))
}
const REFRESH_SKEW: Duration = Duration::minutes(5);
/// After a failed refresh the bearer serves that failure again for this long
/// instead of POSTing the provider's token endpoint on every request. While
/// the cooldown holds every call returns the classified error: no provider
/// round-trip and no fast-path token. A re-login is picked up once the
/// cooldown lapses, because the forced slow path re-reads the document first.
const REFRESH_FAILURE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// Product copy used when classifying auth failures for operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthProduct {
    pub name: &'static str,
    pub backend_label: &'static str,
    pub login_command: &'static str,
    /// What to do about a valid-grant-but-tier-gated account (HTTP 403).
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

/// Which token endpoint / claim mapping to use when rotating credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthRefreshKind {
    ChatGpt,
    Claude,
    Xai,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthAuthProblem {
    Missing,
    WrongMode {
        found_mode: String,
    },
    Expired,
    /// Valid grant, but the account is not entitled to this OAuth surface (tier gate).
    NotEntitled,
    Other(String),
}

pub fn classify_oauth_auth_error(
    product: &OAuthProduct,
    agent_did: &str,
    provider: &str,
    problem: &OAuthAuthProblem,
) -> String {
    match problem {
        OAuthAuthProblem::Missing => format!(
            "No OAuthCredential document found for agent {agent_did} and provider {provider}.\n\
             To use the {backend}, run \
             `gents {login} --agent-did {agent_did}`.",
            backend = product.backend_label,
            login = product.login_command,
        ),
        OAuthAuthProblem::WrongMode { found_mode } => format!(
            "OAuthCredential for agent {agent_did} and provider {provider} is {found_mode}, \
             but the {backend} needs an enabled {name} OAuth credential.\n\
             Run `gents {login} --agent-did {agent_did}` or select an API-key backend.",
            backend = product.backend_label,
            name = product.name,
            login = product.login_command,
        ),
        OAuthAuthProblem::Expired => format!(
            "{name} OAuth credential for agent {agent_did} and provider {provider} is expired or revoked.\n\
             Re-authenticate with `gents {login} --agent-did {agent_did}`.",
            name = product.name,
            login = product.login_command,
        ),
        OAuthAuthProblem::NotEntitled => format!(
            "{name} OAuth credential for agent {agent_did} and provider {provider} is valid, \
             but this account is not entitled to subscription OAuth inference (HTTP 403 tier gate).\n\
             Re-login will not fix this. {guidance}",
            name = product.name,
            guidance = product.not_entitled_guidance,
        ),
        OAuthAuthProblem::Other(detail) => {
            format!(
                "{name} OAuth credential for agent {agent_did} and provider {provider} could not be used: {detail}",
                name = product.name,
            )
        }
    }
}

pub fn classify_chatgpt_auth_error(
    agent_did: &str,
    provider: &str,
    problem: &OAuthAuthProblem,
) -> String {
    classify_oauth_auth_error(&CHATGPT_OAUTH_PRODUCT, agent_did, provider, problem)
}

/// Single owner of the OAuth access-token expiry fallback chain xAI's
/// device-code login and refresh both used to duplicate: prefer the JWT's own
/// `exp` claim (authoritative when present), fall back to a positive
/// `expires_in` (seconds) added to `now`, and default to a conservative
/// 15-minute window when neither is present or valid.
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
    pub agent_did: String,
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
    /// Which of the provider's accounts this row is; `None` is the provider's
    /// original account (the row upgraded in place).
    #[serde(default)]
    pub account_ref: Option<String>,
    /// When the account was first stored; `None` for rows from before the
    /// field existed.
    #[serde(default)]
    pub connected_at: Option<DateTime<Utc>>,
    /// The provider's stable id for the signed-in account, scoped by
    /// `provider`; `None` when the tokens did not show it.
    #[serde(default)]
    pub provider_account_key: Option<String>,
    /// The user-chosen name shown in every view instead of the sign-in email;
    /// `None` shows the product name.
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
            .field("agent_did", &self.agent_did)
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

pub fn oauth_credential_id(agent_did: &str, provider: &str) -> String {
    format!("{provider}:{agent_did}")
}

pub fn oauth_credential_by_id_query(credential_id: &str) -> String {
    let credential_id = crate::graphql::escape_graphql_string(credential_id);
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

/// Fields a token write never sends to a stored row.
const SET_ONCE_FIELDS: &[&str] = &["agent_did", "account_ref", "connected_at", "label"];

pub fn oauth_credential_upsert_mutation(credential: &OAuthCredential) -> String {
    let fields = oauth_credential_input_fields(credential);
    let add_input = render_oauth_input(&fields, &[]);
    // `agent_did` is `@immutable` and `credential_id` is the unique key. On this DefraDB pin the
    // immutability check rejects re-sending an immutable field on a pre-existing document, so the
    // `update` branch (re-login and per-request token rotation both land here) must omit it.
    // `credential_id` is likewise only ever written in `add`. Mirrors session/observations.rs.
    // `account_ref`, `connected_at` and `label` are set once, when a row is first stored; a
    // refresh or re-sign-in never rewrites them (a label changes only through its own update).
    let update_input = render_oauth_input(&fields, SET_ONCE_FIELDS);
    let credential_id = crate::graphql::escape_graphql_string(&credential.credential_id);
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

/// Which of a provider's accounts a caller needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPick<'a> {
    /// The provider's default account, for callers with no backend.
    ProviderDefault,
    /// The account a backend names; `None` is the provider's original account
    /// (the row with no `account_ref`), never another enabled account.
    Reference(Option<&'a str>),
}

/// The fixed account order: earliest `connected_at` first, rows without one
/// (stored before the field existed) before any time, ties by `credential_id`.
fn resolver_order(a: &OAuthCredential, b: &OAuthCredential) -> std::cmp::Ordering {
    (a.connected_at, &a.credential_id).cmp(&(b.connected_at, &b.credential_id))
}

/// The single credential pick: the first enabled row of `agent_did` and
/// `provider` in [`resolver_order`] that `pick` names. Every reader that
/// chooses a row goes through here.
pub fn pick_oauth_credential<'c>(
    rows: impl IntoIterator<Item = &'c OAuthCredential>,
    agent_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Option<&'c OAuthCredential> {
    rows.into_iter()
        .filter(|row| row.agent_did == agent_did && row.provider == provider && row.enabled)
        .filter(|row| match pick {
            AccountPick::ProviderDefault => true,
            AccountPick::Reference(account_ref) => row.account_ref.as_deref() == account_ref,
        })
        .min_by(|a, b| resolver_order(a, b))
}

fn enabled_oauth_credentials_query(agent_did: &str, provider: &str, fields: &str) -> String {
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    let provider = crate::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    provider: {{ _eq: "{provider}" }},
                    enabled: {{ _eq: true }}
                }}
            ) {{
                {fields}
            }}
        }}"#
    )
}

/// Read `agent_did`'s enabled rows for `provider` through `access` and pick one
/// with [`pick_oauth_credential`].
pub async fn resolve_oauth_credential(
    access: &crate::config_client::ConfigAccess,
    agent_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let response = access
        .execute(&enabled_oauth_credentials_query(
            agent_did,
            provider,
            OAUTH_CREDENTIAL_FIELDS,
        ))
        .await?;
    pick_from_response(&response, agent_did, provider, pick)
}

/// The provider's default account for `agent_did` (Lean `SelfConfig` `dflt`),
/// read inside a self-config transaction. `None` means both "the original
/// account is the default" and "no account is enabled"; the latter lets a move
/// onto the provider's no-reference backend through, which then fails at run
/// time with the missing-credential guidance, as with one account today.
pub(crate) async fn provider_default_account_ref(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    provider: &str,
) -> Result<Option<String>> {
    let response = txn
        .execute(&enabled_oauth_credentials_query(
            agent_did,
            provider,
            OAUTH_PICK_FIELDS,
        ))
        .await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "OAuthCredential")
        .into_iter()
        .map(pick_row_from_value)
        .collect::<Result<Vec<_>>>()?;
    Ok(
        pick_oauth_credential(&rows, agent_did, provider, AccountPick::ProviderDefault)
            .and_then(|row| row.account_ref.clone()),
    )
}

/// Decode a row read with [`OAUTH_PICK_FIELDS`] like
/// [`oauth_credential_from_value`], its token fields left blank. The pick
/// takes whole rows; this keeps a model-driven transaction off the tokens.
fn pick_row_from_value(value: Value) -> Result<OAuthCredential> {
    let row: OAuthCredentialRow =
        serde_json::from_value(value).context("decoding OAuthCredential row")?;
    Ok(OAuthCredential {
        doc_id: None,
        credential_id: row.credential_id,
        agent_did: required(row.agent_did, "agent_did")?,
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
        provider_account_key: None,
        label: None,
    })
}

fn pick_from_response(
    response: &Value,
    agent_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<OAuthCredential>> {
    let rows = oauth_credentials_from_response(response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    Ok(pick_oauth_credential(&rows, agent_did, provider, pick).cloned())
}

pub async fn lookup_oauth_credential_by_id(
    node: &EmbeddedNode,
    credential_id: &str,
) -> Result<Option<OAuthCredential>> {
    let response = node
        .execute(&oauth_credential_by_id_query(credential_id))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying OAuthCredential returned errors: {:?}",
            response.errors
        );
    }
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub fn oauth_credentials_for_agent_query(agent_did: &str) -> String {
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    format!(
        r#"query {{
            OAuthCredential(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

pub fn oauth_credential_by_doc_id_query(doc_id: &str) -> String {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
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
    agent_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = node
        .execute(&oauth_credentials_for_agent_query(agent_did))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying OAuthCredential returned errors: {:?}",
            response.errors
        );
    }
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .collect()
}

/// Read agent-scoped credentials through the selected canonical control-plane
/// access. Desktop-managed runtimes use their operator GraphQL endpoint here;
/// callers must not fall back to a replicated client copy for private tokens.
/// Rows come back in resolver order, so a list's first enabled row of a
/// provider is the provider's default account.
pub async fn list_oauth_credentials_on(
    access: &crate::config_client::ConfigAccess,
    agent_did: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = access
        .execute(&oauth_credentials_for_agent_query(agent_did))
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
    let response = node
        .execute(&oauth_credential_by_doc_id_query(doc_id))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying OAuthCredential returned errors: {:?}",
            response.errors
        );
    }
    let response = json!({ "data": response.data.unwrap_or(Value::Null) });
    oauth_credentials_from_response(&response)
        .into_iter()
        .next()
        .transpose()
}

pub async fn upsert_oauth_credential(
    node: &EmbeddedNode,
    credential: &OAuthCredential,
) -> Result<String> {
    let mutation = oauth_credential_upsert_mutation(credential);
    let response =
        crate::config_client::ConfigAccess::write_local(node, "oauth_credential.upsert", &mutation)
            .await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")
}

pub async fn upsert_oauth_credential_on(
    access: &crate::config_client::ConfigAccess,
    credential: &OAuthCredential,
) -> Result<String> {
    let mutation = oauth_credential_upsert_mutation(credential);
    let response = access.write("oauth_credential.upsert", &mutation).await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "OAuthCredential")
}

/// Whether a sign-in stored a new account or refreshed a stored one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInResult {
    Added,
    Refreshed,
}

/// A stored sign-in: the row as written and how it was matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    pub doc_id: String,
    pub credential: OAuthCredential,
    pub result: SignInResult,
    /// The sign-in refreshed a row that knew an identity field (key or
    /// `account_id`), as opposed to a row with no identity at all.
    pub identity_matched: bool,
    /// An add at the provider's original slot: the profiles on the
    /// provider's backends with no account reference, which now run on it.
    pub profiles: Vec<String>,
}

impl SignIn {
    /// After a sign-in that refreshed an account already stored: how to reach
    /// another account instead (a signed-in browser returns the same one).
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

    /// After an add at the original slot: which profiles now use it.
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

/// The product of a sign-in provider; `None` for a provider no backend kind
/// reads (a sign-in there would be stored and never used).
fn sign_in_product(provider: &str) -> Option<OAuthProduct> {
    match provider {
        crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER => Some(CHATGPT_OAUTH_PRODUCT),
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER => {
            Some(crate::claude_oauth::CLAUDE_OAUTH_PRODUCT)
        }
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER => Some(XAI_OAUTH_PRODUCT),
        _ => None,
    }
}

fn oauth_credentials_of_provider_query(agent_did: &str, provider: &str) -> String {
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    let provider = crate::graphql::escape_graphql_string(provider);
    format!(
        r#"query {{
            OAuthCredential(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                provider: {{ _eq: "{provider}" }}
            }}) {{
                {OAUTH_CREDENTIAL_FIELDS}
            }}
        }}"#
    )
}

/// `agent_did`'s rows of `provider`, enabled or not, in resolver order.
async fn provider_rows_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    provider: &str,
) -> Result<Vec<OAuthCredential>> {
    let response = txn
        .execute(&oauth_credentials_of_provider_query(agent_did, provider))
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

/// The backend a provider's added account runs on: the provider's preset
/// connection, named by the account's label, referencing the account.
fn account_backend(
    credential: &OAuthCredential,
    account_ref: &str,
) -> Result<crate::InferenceBackend> {
    use crate::inference_setup::{InferenceAuthMethod as Auth, InferenceProviderId as Id};
    let (provider, auth) = match credential.provider.as_str() {
        crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER => (Id::OpenAi, Auth::ChatGptOauth),
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER => (Id::Anthropic, Auth::ClaudeOauth),
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER => (Id::Grok, Auth::GrokOauth),
        other => anyhow::bail!("no backend reads sign-ins of provider {other:?}"),
    };
    let spec = crate::inference_setup::connection_spec(provider, auth, "")?;
    Ok(crate::InferenceBackend {
        agent_did: credential.agent_did.clone(),
        backend_id: format!("{}-{account_ref}", credential.provider),
        name: effective_account_label(credential),
        provider_kind: spec.provider_kind,
        openai_wire_api: spec.openai_wire_api,
        endpoint: spec.endpoint,
        auth: crate::document_config::BackendAuth::PrincipalOAuth {
            account_ref: Some(account_ref.to_owned()),
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
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    backend: &crate::InferenceBackend,
) -> Result<()> {
    crate::config_client::write_inference_backend_in_txn(txn, backend)
        .await
        .context(
            "the account's backend could not be stored, so the sign-in was not saved; \
             fix the agent's configuration and sign in again",
        )
}

/// Give an added account its backend when none references it (a lost one
/// comes back), and carry a label change to a backend still named by the
/// old label.
async fn sync_account_backend(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
    old_label: Option<&str>,
) -> Result<()> {
    let Some(account_ref) = credential.account_ref.as_deref() else {
        return Ok(());
    };
    let serving: Vec<_> =
        crate::config_client::list_inference_backends_in_txn(txn, &credential.agent_did)
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

/// Longest account label, in characters.
const MAX_ACCOUNT_LABEL_CHARS: usize = 64;

/// A label as stored: trimmed, 1 to 64 characters, no control characters.
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

/// The name an account shows: its label, else its product's name.
pub fn effective_account_label(credential: &OAuthCredential) -> String {
    credential.label.clone().unwrap_or_else(|| {
        sign_in_product(&credential.provider)
            .map_or(credential.provider.as_str(), |product| product.name)
            .to_owned()
    })
}

/// `label` unless another of the provider's accounts shows it.
fn ensure_label_free(others: &[&OAuthCredential], label: &str) -> Result<()> {
    anyhow::ensure!(
        !others
            .iter()
            .any(|row| effective_account_label(row) == label),
        "the label {label:?} is already used by another account of this provider"
    );
    Ok(())
}

/// The first of `Name`, `Name 2`, `Name 3`, ... no account shows.
fn next_free_label(rows: &[OAuthCredential], provider: &str) -> String {
    let name = sign_in_product(provider).map_or(provider, |product| product.name);
    let taken: std::collections::HashSet<_> = rows.iter().map(effective_account_label).collect();
    std::iter::once(name.to_owned())
        .chain((2..).map(|n| format!("{name} {n}")))
        .find(|label| !taken.contains(label))
        .expect("an unbounded sequence has a free label")
}

/// Store a sign-in: refresh the stored account it shows, or add it.
///
/// A refresh keeps the row's id, reference, connection time and label and
/// enables it. An add takes the provider's original slot
/// (`{provider}:{did}`, no reference) when the provider has no row on this
/// node, else mints an opaque, DID-free `account_ref`.
pub async fn store_sign_in(
    access: &crate::config_client::ConfigAccess,
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
        .transact("oauth_credential.sign_in", |txn| {
            let credential = credential.clone();
            let label = label.clone();
            Box::pin(async move { store_sign_in_in_txn(txn, credential, label).await })
        })
        .await
}

/// The key forms `credential`'s own tokens show, whole values trimmed. ChatGPT
/// and Grok keys are recomputed from the tokens (a stored key written beside
/// other tokens is not trusted); a Claude access token is opaque, so its
/// stored key stands.
fn identity_keys(credential: &OAuthCredential) -> Vec<String> {
    let keys = match credential.provider.as_str() {
        crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER => {
            crate::chatgpt_oauth_refresh::chatgpt_account_keys(
                &credential.access_token,
                credential.id_token.as_deref(),
                credential.account_id.as_deref(),
            )
        }
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER => {
            crate::xai_oauth_login::xai_account_key(&credential.access_token)
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

/// How a stored row matches a sign-in, best first: the sign-in shows every
/// identity field the row knows (a key in its key set, an equal
/// `account_id`), and the row knows a key, or only an `account_id`, or
/// nothing at all. `None` when the sign-in cannot confirm a known field.
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
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    mut credential: OAuthCredential,
    label: Option<String>,
) -> Result<SignIn> {
    let rows = provider_rows_in_txn(txn, &credential.agent_did, &credential.provider).await?;
    let sign_in_keys = identity_keys(&credential);
    let matched = rows
        .iter()
        .filter_map(|row| Some((sign_in_match(row, &sign_in_keys, &credential)?, row)))
        .min_by_key(|(tier, _)| *tier);
    if let Some((tier, row)) = matched {
        // Keep the stored key when this sign-in shows it in any form, so a
        // key moving between forms does not re-key the account.
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
            crate::graphql::escape_graphql_string(&doc_id),
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
        credential.credential_id = oauth_credential_id(&credential.agent_did, &credential.provider);
    } else {
        let account_ref = uuid::Uuid::new_v4().simple().to_string();
        credential.credential_id = format!(
            "{}:{}:{account_ref}",
            credential.provider, credential.agent_did
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
        crate::graphql::escape_graphql_string(&credential.credential_id),
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

/// Profiles on `credential`'s provider's backends with no account reference:
/// the ones its original account serves.
async fn original_account_profiles(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
) -> Result<Vec<String>> {
    use crate::backend_provider::BackendProviderOauthExt;
    let backends: Vec<_> =
        crate::config_client::list_inference_backends_in_txn(txn, &credential.agent_did)
            .await?
            .into_iter()
            .filter(|backend| {
                matches!(
                    backend.auth,
                    crate::document_config::BackendAuth::PrincipalOAuth { account_ref: None }
                ) && backend.provider_kind.oauth_provider() == Some(credential.provider.as_str())
            })
            .map(|backend| backend.backend_id)
            .collect();
    Ok(
        crate::config_client::list_inference_profiles_in_txn(txn, &credential.agent_did)
            .await?
            .into_iter()
            .filter(|profile| backends.contains(&profile.backend_id))
            .map(|profile| profile.profile_id)
            .collect(),
    )
}

/// One sign-in account as views show it: no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountSummary {
    pub credential_id: String,
    pub provider: String,
    pub account_ref: Option<String>,
    /// The effective label ([`effective_account_label`]).
    pub label: String,
    /// The sign-in's display identity ([`account_display_label`]); display only.
    pub identity: Option<String>,
    pub plan: Option<String>,
    pub enabled: bool,
    /// The provider's default account ([`AccountPick::ProviderDefault`]).
    pub default: bool,
    pub access_token_expires_at: DateTime<Utc>,
    /// When this sign-in took its slot; `None` for a row stored before the
    /// field existed.
    pub connected_at: Option<DateTime<Utc>>,
}

/// The account a principal OAuth backend runs on, among `accounts` (one
/// principal's): the one of the backend's provider with the backend's
/// reference, where no reference is the provider's original account. `None`
/// for a backend that uses no account; `Some(None)` when that account is not
/// on this node.
pub fn backend_account<'a>(
    backend: &crate::InferenceBackend,
    accounts: &'a [AccountSummary],
) -> Option<Option<&'a AccountSummary>> {
    use crate::backend_provider::BackendProviderOauthExt;
    let crate::document_config::BackendAuth::PrincipalOAuth { account_ref } = &backend.auth else {
        return None;
    };
    let provider = backend.provider_kind.oauth_provider()?;
    Some(
        accounts
            .iter()
            .find(|account| account.provider == provider && account.account_ref == *account_ref),
    )
}

/// Whether the account a backend runs on can serve it. `Missing` covers an
/// account removed from this node and one signed in on another node: stored
/// rows cannot tell them apart.
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
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The account a backend runs on, as views show it: a label, never an
/// identity or token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServingAccount {
    pub label: String,
    pub state: AccountState,
    /// The sign-in provider of a principal OAuth backend.
    #[serde(skip)]
    pub provider: Option<&'static str>,
}

/// [`backend_account`] as a label and state. A backend that uses no account
/// is its own account; one whose account is not on this node keeps its
/// backend name, which for an added account is its label.
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
    use crate::backend_provider::BackendProviderOauthExt;
    let provider = matches!(
        backend.auth,
        crate::document_config::BackendAuth::PrincipalOAuth { .. }
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

/// Where a profile can move instead: `provider`'s enabled accounts by label,
/// or how to sign one in when there is none.
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

/// The providers whose sign-ins are accounts: the ones a backend reads.
const ACCOUNT_PROVIDERS: [&str; 3] = [
    crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
    crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
    crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
];

/// A filter on `agent_did`'s accounts, optionally one `credential_id`.
fn account_filter(agent_did: &str, credential_id: Option<&str>) -> String {
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
        r#"{{ agent_did: {{ _eq: "{}" }}, provider: {{ _in: [{providers}] }}{credential} }}"#,
        crate::graphql::escape_graphql_string(agent_did)
    )
}

fn account_not_found(credential_id: &str) -> anyhow::Error {
    anyhow::anyhow!("account {credential_id:?} not found for this agent")
}

/// `agent_did`'s sign-in accounts in resolver order. Only providers a backend
/// reads are accounts; a cloud workspace token is not.
pub async fn list_accounts(
    access: &crate::config_client::ConfigAccess,
    agent_did: &str,
) -> Result<Vec<AccountSummary>> {
    let response = access
        .execute(&format!(
            "query {{ OAuthCredential(filter: {}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}",
            account_filter(agent_did, None)
        ))
        .await?;
    let mut rows = oauth_credentials_from_response(&response)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(resolver_order);
    let defaults: Vec<_> = ACCOUNT_PROVIDERS
        .iter()
        .filter_map(|provider| {
            pick_oauth_credential(&rows, agent_did, provider, AccountPick::ProviderDefault)
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

/// Rename an account; a backend still named by its old label follows.
pub async fn set_account_label(
    access: &crate::config_client::ConfigAccess,
    agent_did: &str,
    credential_id: &str,
    label: &str,
) -> Result<()> {
    let label = validate_account_label(label)?;
    access
        .transact("oauth_credential.label", |txn| {
            let label = label.clone();
            Box::pin(async move {
                let response = txn
                    .execute(&format!(
                        "query {{ OAuthCredential(filter: {}) {{ {OAUTH_CREDENTIAL_FIELDS} }} }}",
                        account_filter(agent_did, Some(credential_id))
                    ))
                    .await?;
                let row = oauth_credentials_from_response(&response)
                    .into_iter()
                    .next()
                    .ok_or_else(|| account_not_found(credential_id))??;
                let rows = provider_rows_in_txn(txn, agent_did, &row.provider).await?;
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

/// Enable or disable an account, keeping its tokens.
pub async fn set_account_enabled(
    access: &crate::config_client::ConfigAccess,
    agent_did: &str,
    credential_id: &str,
    enabled: bool,
) -> Result<()> {
    let response = access
        .write(
            "oauth_credential.enabled",
            &format!(
                "mutation {{ update_OAuthCredential(filter: {}, input: {{ enabled: {} }}) {{ _docID }} }}",
                account_filter(agent_did, Some(credential_id)),
                gents_protocol::graphql::graphql_bool_literal(enabled),
            ),
        )
        .await?;
    ensure_matched(&response, "update_OAuthCredential", credential_id)
}

/// Delete an account's row inside the caller's transaction.
pub async fn remove_account_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    credential_id: &str,
) -> Result<()> {
    let response = txn
        .execute(&format!(
            "mutation {{ delete_OAuthCredential(filter: {}) {{ _docID }} }}",
            account_filter(agent_did, Some(credential_id)),
        ))
        .await?;
    ensure_matched(&response, "delete_OAuthCredential", credential_id)
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
            crate::graphql::escape_graphql_string(value)
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
        ("agent_did", field("agent_did", &credential.agent_did)),
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
        agent_did: required(row.agent_did, "agent_did")?,
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

/// The provider account key `credential`'s own tokens show, by the bearer's
/// refresh kind (a provider name can be anything). Claude access tokens carry
/// no identity, so Claude rows fill at the next sign-in.
fn provider_account_key(kind: OAuthRefreshKind, credential: &OAuthCredential) -> Option<String> {
    match kind {
        OAuthRefreshKind::ChatGpt => crate::chatgpt_oauth_refresh::chatgpt_account_key(
            &credential.access_token,
            credential.id_token.as_deref(),
            credential.account_id.as_deref(),
        ),
        OAuthRefreshKind::Xai => crate::xai_oauth_login::xai_account_key(&credential.access_token),
        OAuthRefreshKind::Claude => None,
    }
}

pub(crate) fn get_or_insert_arc<T>(
    registry: &std::sync::Mutex<std::collections::HashMap<String, Arc<T>>>,
    key: &str,
    make: impl FnOnce() -> T,
) -> Arc<T> {
    let mut map = registry.lock().expect("bearer registry mutex poisoned");
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(make()))
        .clone()
}

fn bearer_registry(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<DbCredentialBearer>>> {
    static REGISTRY: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<DbCredentialBearer>>>,
    > = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
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
}

/// What a refresh's write found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshPersist {
    Written,
    /// The row was removed, replaced or signed in again since the refresh
    /// read it; the stored row stands.
    Superseded,
}

/// The fields a refresh changes, plus the key when this refresh filled it.
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
            crate::graphql::escape_graphql_string(doc_id)
        ),
        None => format!(
            r#"credential_id: {{ _eq: "{}" }}"#,
            crate::graphql::escape_graphql_string(&credential.credential_id)
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
        crate::graphql::escape_graphql_string(stored_refresh_token),
        render_oauth_input(&fields, &[]),
    )
}

const MOVE_OFF_ACCOUNT: &str =
    "Move a profile off it: gents config profile set-account <profile> <account> (gents accounts list).";

/// `<Product> account "<label>" is <state>.` A label that reads as a provider
/// limit or a context-length error is replaced by the product name: bearer
/// errors reach the limit classifiers, and a label is free text.
fn account_failure(product: &OAuthProduct, label: &str, state: &str) -> String {
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

pub struct DbCredentialBearer {
    node: Arc<EmbeddedNode>,
    agent_did: String,
    provider: String,
    credential_id: String,
    http: reqwest::Client,
    cache: Mutex<Option<OAuthCredential>>,
    /// Single-flight refresh lock. It also holds the last refresh failure, so
    /// the cooldown check and the refresh share one critical section.
    refresh_lock: Mutex<Option<(std::time::Instant, OAuthAuthProblem)>>,
    is_owner: bool,
    force_refresh: AtomicBool,
    refresh_kind: OAuthRefreshKind,
    product: OAuthProduct,
}

impl DbCredentialBearer {
    pub fn new(
        node: Arc<EmbeddedNode>,
        agent_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
        refresh_kind: OAuthRefreshKind,
        product: OAuthProduct,
    ) -> Self {
        Self::with_cache(
            node,
            agent_did,
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
        agent_did: impl Into<String>,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        is_owner: bool,
        cache_seed: Option<OAuthCredential>,
        refresh_kind: OAuthRefreshKind,
        product: OAuthProduct,
    ) -> Self {
        Self {
            node,
            agent_did: agent_did.into(),
            provider: provider.into(),
            credential_id: credential_id.into(),
            http: reqwest::Client::new(),
            cache: Mutex::new(cache_seed),
            refresh_lock: Mutex::new(None),
            is_owner,
            force_refresh: AtomicBool::new(false),
            refresh_kind,
            product,
        }
    }

    async fn load_credential(&self) -> Result<OAuthCredential> {
        let Some(credential) =
            lookup_oauth_credential_by_id(self.node.as_ref(), &self.credential_id).await?
        else {
            let label = self.cached().await.map_or_else(
                || self.product.name.to_owned(),
                |cached| effective_account_label(&cached),
            );
            anyhow::bail!(
                "{} {MOVE_OFF_ACCOUNT} Signing in again adds a new account; it does not restore \
                 this one.",
                account_failure(&self.product, &label, "removed from this node"),
            );
        };
        if !credential.enabled {
            anyhow::bail!(
                "{}\n{}\n{MOVE_OFF_ACCOUNT}",
                account_failure(
                    &self.product,
                    &effective_account_label(&credential),
                    "disabled"
                ),
                classify_oauth_auth_error(
                    &self.product,
                    &self.agent_did,
                    &self.provider,
                    &OAuthAuthProblem::WrongMode {
                        found_mode: "disabled".to_string(),
                    },
                )
            );
        }
        Ok(credential)
    }

    async fn cached(&self) -> Option<OAuthCredential> {
        self.cache.lock().await.clone()
    }

    async fn cache_credential(&self, credential: &OAuthCredential) {
        *self.cache.lock().await = Some(credential.clone());
    }

    /// Serve `stored` when the cache holds another document: the cached row
    /// was removed and its `credential_id` signed in again, so its token is
    /// not this account's. The same document keeps the cache.
    pub(crate) async fn adopt_document(&self, stored: &OAuthCredential) {
        let mut cache = self.cache.lock().await;
        if cache
            .as_ref()
            .is_some_and(|cached| cached.doc_id != stored.doc_id)
        {
            *cache = Some(stored.clone());
        }
    }

    /// Write a refresh's fields onto the row it read, only while that row
    /// still holds `stored_refresh_token`: a sign-in (always a new refresh
    /// token), remove or replacement since the read makes it match nothing,
    /// so a refresh never revives, re-enables or overwrites a row. Retried
    /// only on an error.
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
            match crate::config_client::ConfigAccess::write_local(
                self.node.as_ref(),
                "oauth_credential.refresh",
                &mutation,
            )
            .await
            {
                Ok(response) => {
                    let written = response
                        .get("data")
                        .and_then(|data| data.get("update_OAuthCredential"))
                        .is_some_and(crate::graphql::response_has_documents);
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
        Err(last_error.expect("persist_with_retry ran at least one failing attempt"))
    }

    /// `credential` is the row being refreshed, never the cache: after a
    /// remove and a new sign-in at this `credential_id` the cache can still
    /// hold the removed account.
    fn auth_error(
        &self,
        credential: &OAuthCredential,
        problem: &OAuthAuthProblem,
    ) -> anyhow::Error {
        let state = match problem {
            OAuthAuthProblem::Expired => "signed out (expired or revoked)",
            OAuthAuthProblem::NotEntitled => "not entitled",
            _ => "unusable",
        };
        anyhow::anyhow!(
            "{}\n{}\n{MOVE_OFF_ACCOUNT}",
            account_failure(&self.product, &effective_account_label(credential), state),
            classify_oauth_auth_error(&self.product, &self.agent_did, &self.provider, problem),
        )
    }

    async fn refresh_tokens(
        &self,
        refresh_token: &str,
    ) -> Result<RefreshedTokens, OAuthAuthProblem> {
        match self.refresh_kind {
            OAuthRefreshKind::ChatGpt => {
                crate::chatgpt_oauth_refresh::refresh_chatgpt_token(refresh_token, &self.http).await
            }
            OAuthRefreshKind::Claude => {
                crate::claude_oauth_refresh::refresh_claude_token(refresh_token, &self.http).await
            }
            OAuthRefreshKind::Xai => {
                crate::xai_oauth_refresh::refresh_xai_token(refresh_token, &self.http).await
            }
        }
    }
}

impl BearerSource for DbCredentialBearer {
    async fn current_bearer(&self) -> Result<String> {
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
        // Another document at this `credential_id` replaced the cached one
        // (remove, then a sign-in): never refresh the removed account.
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
                return Err(self.auth_error(&credential, problem));
            }
        }

        let refreshed = match self.refresh_tokens(&credential.refresh_token).await {
            Ok(refreshed) => refreshed,
            Err(problem) => {
                let error = self.auth_error(&credential, &problem);
                *last_failure = Some((std::time::Instant::now(), problem));
                return Err(error);
            }
        };
        *last_failure = None;
        apply_refreshed_tokens(&mut credential, refreshed);
        // The refresh owner is the single writer of existing rows: an empty key
        // is filled here, from the tokens persisted beside it, and never
        // recomputed once present.
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
                // The next call loads the stored row instead of refreshing this one.
                *self.cache.lock().await = None;
                tracing::debug!(
                    agent_did = %self.agent_did,
                    credential_id = %self.credential_id,
                    product = self.product.name,
                    "the stored account changed while its token refreshed; the stored row stands \
                     and this refreshed token is served once"
                )
            }
            Err(error) => tracing::error!(
                agent_did = %self.agent_did,
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

    async fn invalidate(&self) {
        self.force_refresh.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicUsize;

    fn sample_credential() -> OAuthCredential {
        OAuthCredential {
            doc_id: Some("doc-1".to_string()),
            credential_id: oauth_credential_id("did:key:zAgent", "chatgpt-codex"),
            agent_did: "did:key:zAgent".to_string(),
            provider: "chatgpt-codex".to_string(),
            access_token: "access-tok".to_string(),
            refresh_token: "refresh-tok".to_string(),
            id_token: Some("id-tok".to_string()),
            account_id: Some("acct-1".to_string()),
            chatgpt_plan_type: Some("pro".to_string()),
            is_fedramp: false,
            access_token_expires_at: DateTime::<Utc>::from_timestamp(1_900_000_000, 0).unwrap(),
            last_refresh: Some(DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap()),
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: None,
            label: None,
        }
    }

    #[test]
    fn debug_redacts_every_token() {
        let mut credential = sample_credential();
        credential.provider_account_key = Some("pak-sentinel".to_string());
        let refreshed = RefreshedTokens {
            access_token: credential.access_token.clone(),
            refresh_token: credential.refresh_token.clone(),
            id_token: credential.id_token.clone(),
            account_id: credential.account_id.clone(),
            is_fedramp: false,
            plan_type: None,
            access_token_expires_at: credential.access_token_expires_at,
        };
        let debug = format!("{credential:?} {credential:#?} {refreshed:?} {refreshed:#?}");
        for token in ["access-tok", "refresh-tok", "id-tok", "pak-sentinel"] {
            assert!(!debug.contains(token), "{token} leaked: {debug}");
        }
        assert!(debug.contains("acct-1"), "{debug}");
        for field in [
            "account_ref",
            "connected_at",
            "provider_account_key",
            "label",
        ] {
            assert!(debug.contains(field), "{field} missing: {debug}");
        }
    }

    #[test]
    fn account_label_is_display_only_and_prefers_email() {
        use base64::Engine;
        let mut credential = sample_credential();
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"email":"person@example.test","sub":"provider-user"}"#);
        credential.id_token = Some(format!("header.{payload}.signature"));
        assert_eq!(
            account_display_label(&credential).as_deref(),
            Some("person@example.test")
        );
        assert_eq!(credential.account_id.as_deref(), Some("acct-1"));
        credential.id_token = None;
        assert_eq!(
            account_display_label(&credential).as_deref(),
            Some("acct-1")
        );
        credential.account_id = None;
        assert_eq!(account_display_label(&credential), None);
    }

    #[test]
    fn shared_registry_returns_one_arc_per_key_and_constructs_once() {
        use std::collections::HashMap;
        use std::sync::Mutex as StdMutex;

        let registry: StdMutex<HashMap<String, Arc<u32>>> = StdMutex::new(HashMap::new());
        let calls = AtomicUsize::new(0);

        let a = get_or_insert_arc(&registry, "k1", || {
            calls.fetch_add(1, Ordering::SeqCst);
            7u32
        });
        let b = get_or_insert_arc(&registry, "k1", || {
            calls.fetch_add(1, Ordering::SeqCst);
            99u32
        });
        let c = get_or_insert_arc(&registry, "k2", || {
            calls.fetch_add(1, Ordering::SeqCst);
            7u32
        });

        assert!(Arc::ptr_eq(&a, &b), "same key must share one Arc");
        assert!(!Arc::ptr_eq(&a, &c), "different keys must be distinct Arcs");
        assert_eq!(*a, 7, "first construction wins for a key");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "construct exactly once per distinct key"
        );
    }

    #[test]
    fn token_is_fresh_respects_refresh_skew() {
        assert!(
            token_is_fresh(Utc::now() + Duration::hours(1)),
            "comfortably-future token is fresh"
        );
        assert!(
            !token_is_fresh(Utc::now() - Duration::minutes(1)),
            "expired token is stale"
        );
        assert!(
            !token_is_fresh(Utc::now() + Duration::minutes(2)),
            "token inside the 5-minute skew window is treated as stale"
        );
    }

    #[test]
    fn apply_refreshed_tokens_preserves_omitted_optionals_and_ors_fedramp() {
        let mut credential = sample_credential();
        credential.is_fedramp = true;
        let prior_id = credential.id_token.clone();

        apply_refreshed_tokens(
            &mut credential,
            RefreshedTokens {
                access_token: "new-access".to_string(),
                refresh_token: "new-refresh".to_string(),
                id_token: None,
                account_id: None,
                is_fedramp: false,
                plan_type: None,
                access_token_expires_at: DateTime::<Utc>::from_timestamp(2_000_000_000, 0).unwrap(),
            },
        );

        assert_eq!(credential.access_token, "new-access");
        assert_eq!(credential.refresh_token, "new-refresh");
        assert_eq!(credential.id_token, prior_id, "omitted id_token preserved");
        assert_eq!(
            credential.account_id.as_deref(),
            Some("acct-1"),
            "omitted account_id preserved"
        );
        assert_eq!(
            credential.chatgpt_plan_type.as_deref(),
            Some("pro"),
            "omitted plan preserved"
        );
        assert!(credential.is_fedramp, "is_fedramp stays true via OR");
        assert_eq!(
            credential.access_token_expires_at.timestamp(),
            2_000_000_000
        );
        assert!(credential.last_refresh.is_some());
    }

    #[test]
    fn oauth_credential_from_value_applies_defaults_and_cleans_blanks() {
        let row = json!({
            "_docID": "doc-9",
            "credential_id": "chatgpt-codex:did:key:zA",
            "agent_did": "did:key:zA",
            "provider": "chatgpt-codex",
            "access_token": "acc",
            "refresh_token": "ref",
            "id_token": null,
            "account_id": "",
            "chatgpt_plan_type": null,
            "access_token_expires_at": "2030-01-01T00:00:00Z",
            "last_refresh": null,
        });

        let credential = oauth_credential_from_value(row).expect("row parses");

        assert_eq!(credential.doc_id.as_deref(), Some("doc-9"));
        assert_eq!(credential.access_token, "acc");
        assert_eq!(credential.id_token, None, "explicit null -> None");
        assert_eq!(credential.account_id, None, "blank string cleaned to None");
        assert!(!credential.is_fedramp, "missing is_fedramp defaults false");
        assert!(credential.enabled, "missing enabled defaults true");
        assert_eq!(credential.last_refresh, None);
        assert_eq!(
            credential.access_token_expires_at,
            DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn oauth_credential_from_value_rejects_a_blank_refresh_token() {
        let row = json!({
            "_docID": "doc-9",
            "credential_id": "gents-cloud:did:key:zA",
            "agent_did": "did:key:zA",
            "provider": "gents-cloud",
            "access_token": "workspace-token",
            "refresh_token": "  ",
            "access_token_expires_at": "2030-01-01T00:00:00Z",
        });
        let error = oauth_credential_from_value(row).expect_err("blank refresh token");
        assert!(
            error.to_string().contains("missing refresh_token"),
            "{error}"
        );
        let rows = oauth_credentials_from_response(&json!({
            "data": {
                "OAuthCredential": [{
                    "_docID": "doc-9",
                    "credential_id": "gents-cloud:did:key:zA",
                    "agent_did": "did:key:zA",
                    "provider": "gents-cloud",
                    "access_token": "workspace-token",
                    "refresh_token": "",
                    "access_token_expires_at": "2030-01-01T00:00:00Z",
                }]
            }
        }));
        let listed = rows.into_iter().collect::<Result<Vec<_>, _>>();
        assert!(listed
            .expect_err("one blank row fails the list")
            .to_string()
            .contains("missing refresh_token"));
    }

    #[test]
    fn upsert_update_block_omits_immutable_agent_did() {
        let mutation = oauth_credential_upsert_mutation(&sample_credential());

        let update_idx = mutation
            .find("update:")
            .expect("mutation has an update block");
        let (add_block, update_block) = mutation.split_at(update_idx);

        assert!(
            !update_block.contains("agent_did:"),
            "update block must omit immutable agent_did: {update_block}"
        );
        assert!(
            update_block.contains("access_token:") && update_block.contains("refresh_token:"),
            "update block must still rotate token fields: {update_block}"
        );
        assert!(
            add_block.contains("agent_did:") && add_block.contains("credential_id:"),
            "add block must set agent_did and credential_id: {add_block}"
        );
    }

    #[test]
    fn account_fields_are_only_in_the_add_block() {
        let mut credential = sample_credential();
        credential.account_ref = Some("acct-b".to_string());
        credential.connected_at = Some(DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap());
        let mutation = oauth_credential_upsert_mutation(&credential);
        let (add_block, update_block) = mutation.split_at(mutation.find("update:").unwrap());

        assert!(
            add_block.contains(r#"account_ref: "acct-b""#)
                && add_block.contains(r#"connected_at: "2023-11-14T22:13:20Z""#),
            "add block must set the account fields: {add_block}"
        );
        assert!(
            !update_block.contains("account_ref") && !update_block.contains("connected_at"),
            "update block must never rewrite the account fields: {update_block}"
        );
    }

    #[tokio::test]
    async fn account_fields_are_kept_when_a_row_is_stored_again() {
        let node = super::test_support::test_node().await;
        let connected_at = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut credential = sample_credential();
        credential.doc_id = None;
        credential.account_ref = Some("acct-b".to_string());
        credential.connected_at = Some(connected_at);
        upsert_oauth_credential(&node, &credential).await.unwrap();

        let stored = lookup_oauth_credential_by_id(&node, &credential.credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert_eq!(stored.account_ref.as_deref(), Some("acct-b"));
        assert_eq!(stored.connected_at, Some(connected_at));

        // A refresh or re-sign-in stores the row again without the account fields.
        credential.account_ref = None;
        credential.connected_at = None;
        credential.access_token = "access-rotated".to_string();
        upsert_oauth_credential(&node, &credential).await.unwrap();

        let stored = lookup_oauth_credential_by_id(&node, &credential.credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert_eq!(stored.access_token, "access-rotated");
        assert_eq!(stored.account_ref.as_deref(), Some("acct-b"));
        assert_eq!(stored.connected_at, Some(connected_at));
    }

    #[test]
    fn not_entitled_copy_is_product_specific() {
        let msg = classify_oauth_auth_error(
            &CHATGPT_OAUTH_PRODUCT,
            "did:key:zAgent",
            "chatgpt-codex",
            &OAuthAuthProblem::NotEntitled,
        );
        assert!(
            !msg.contains("api.x.ai") && !msg.contains("SuperGrok"),
            "ChatGPT tier-gate copy must not carry xAI guidance: {msg}"
        );
        assert!(msg.contains("ChatGPT"), "{msg}");
    }

    #[test]
    fn classifies_not_entitled_without_relogin_guidance() {
        let msg = classify_oauth_auth_error(
            &XAI_OAUTH_PRODUCT,
            "did:key:zAgent",
            "xai-oauth",
            &OAuthAuthProblem::NotEntitled,
        );
        assert!(msg.contains("not entitled"), "{msg}");
        assert!(
            !msg.contains("gents grok-login"),
            "tier gate should not push re-login as the fix: {msg}"
        );
        assert!(msg.contains("api.x.ai"), "{msg}");
    }
}

#[cfg(test)]
mod serving_account_tests {
    use super::*;
    use crate::document_config::BackendAuth;

    fn account(account_ref: Option<&str>, label: &str, enabled: bool) -> AccountSummary {
        AccountSummary {
            credential_id: format!(
                "claude-subscription:did:key:z6MkTest{}",
                account_ref.unwrap_or("")
            ),
            provider: crate::claude_oauth::CLAUDE_OAUTH_PROVIDER.into(),
            account_ref: account_ref.map(str::to_owned),
            label: label.into(),
            identity: Some("IDENTITY".into()),
            plan: None,
            enabled,
            default: account_ref.is_none(),
            access_token_expires_at: Utc::now(),
            connected_at: None,
        }
    }

    fn backend(
        name: &str,
        provider_kind: &str,
        auth: BackendAuth,
        enabled: bool,
    ) -> crate::InferenceBackend {
        serde_json::from_value(json!({
            "agent_did": "did:key:z6MkTest", "backend_id": name, "name": name,
            "provider_kind": provider_kind, "endpoint": "http://127.0.0.1:1/v1",
            "auth": auth, "enabled": enabled,
        }))
        .unwrap()
    }

    fn oauth(account_ref: Option<&str>) -> BackendAuth {
        BackendAuth::PrincipalOAuth {
            account_ref: account_ref.map(str::to_owned),
        }
    }

    fn accounts() -> Vec<AccountSummary> {
        vec![
            account(None, "Claude", true),
            account(Some("acct-b"), "label-b", false),
        ]
    }

    #[test]
    fn an_account_backend_names_its_account() {
        let original = backend("claude", "ClaudeCliSubscription", oauth(None), true);
        assert_eq!(
            serving_account(&original, &accounts()),
            ServingAccount {
                label: "Claude".into(),
                state: AccountState::Enabled,
                provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            }
        );
        let b = backend(
            "label-b",
            "ClaudeCliSubscription",
            oauth(Some("acct-b")),
            true,
        );
        assert_eq!(
            serving_account(&b, &accounts()),
            ServingAccount {
                label: "label-b".into(),
                state: AccountState::Disabled,
                provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            }
        );
    }

    #[test]
    fn a_backend_whose_account_is_gone_is_missing_by_its_name() {
        let gone = backend(
            "label-gone",
            "ClaudeCliSubscription",
            oauth(Some("acct-other")),
            true,
        );
        assert_eq!(
            serving_account(&gone, &accounts()),
            ServingAccount {
                label: "label-gone".into(),
                state: AccountState::Missing,
                provider: Some(crate::claude_oauth::CLAUDE_OAUTH_PROVIDER),
            }
        );
    }

    #[test]
    fn an_api_key_backend_is_its_own_account() {
        let key = BackendAuth::ApiKey {
            key: "key-SECRET".into(),
        };
        for (enabled, state) in [
            (true, AccountState::Enabled),
            (false, AccountState::Disabled),
        ] {
            let api = backend("OpenAI", "OpenAiCompatible", key.clone(), enabled);
            assert_eq!(
                serving_account(&api, &accounts()),
                ServingAccount {
                    label: "OpenAI".into(),
                    state,
                    provider: None,
                }
            );
        }
    }

    #[test]
    fn the_serving_account_holds_no_identity() {
        for backend in [
            backend("claude", "ClaudeCliSubscription", oauth(None), true),
            backend(
                "label-gone",
                "ClaudeCliSubscription",
                oauth(Some("acct-other")),
                true,
            ),
        ] {
            let json = serde_json::to_value(serving_account(&backend, &accounts())).unwrap();
            let text = json.to_string();
            assert!(
                !text.contains("IDENTITY") && !text.contains("SECRET"),
                "{text}"
            );
            assert_eq!(
                json["state"],
                serving_account(&backend, &accounts()).state.as_str()
            );
        }
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::test_support::test_node;
    use super::*;

    const DID: &str = "did:key:zResolver";
    const PROVIDER: &str = "xai-oauth";

    fn row(
        agent_did: &str,
        provider: &str,
        credential_id: &str,
        account_ref: Option<&str>,
        connected_at: Option<i64>,
        enabled: bool,
    ) -> OAuthCredential {
        OAuthCredential {
            doc_id: None,
            credential_id: credential_id.to_string(),
            agent_did: agent_did.to_string(),
            provider: provider.to_string(),
            access_token: "access-TEST".to_string(),
            refresh_token: "refresh-TEST".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: DateTime::<Utc>::from_timestamp(1_900_000_000, 0).unwrap(),
            last_refresh: None,
            enabled,
            account_ref: account_ref.map(str::to_string),
            connected_at: connected_at
                .map(|secs| DateTime::<Utc>::from_timestamp(secs, 0).unwrap()),
            provider_account_key: None,
            label: None,
        }
    }

    fn picked<'c>(rows: &'c [OAuthCredential], pick: AccountPick<'_>) -> Option<&'c str> {
        pick_oauth_credential(rows, DID, PROVIDER, pick).map(|row| row.credential_id.as_str())
    }

    #[test]
    fn resolver_picks_the_earliest_connected_enabled_account() {
        let rows = [
            row(DID, PROVIDER, "late", Some("late"), Some(2_000), true),
            row(DID, PROVIDER, "early", Some("early"), Some(1_000), true),
        ];
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("early"));
    }

    #[test]
    fn resolver_skips_a_disabled_account() {
        let rows = [
            row(DID, PROVIDER, "off", Some("off"), Some(500), false),
            row(DID, PROVIDER, "on", Some("on"), Some(1_000), true),
        ];
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("on"));
    }

    #[test]
    fn resolver_sorts_a_missing_connection_time_first() {
        let rows = [
            row(DID, PROVIDER, "timed", Some("timed"), Some(1_000), true),
            row(DID, PROVIDER, "untimed", None, None, true),
        ];
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("untimed"));
    }

    #[test]
    fn resolver_breaks_equal_times_by_credential_id() {
        let rows = [
            row(DID, PROVIDER, "b", Some("b"), Some(1_000), true),
            row(DID, PROVIDER, "a", Some("a"), Some(1_000), true),
        ];
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("a"));
    }

    #[test]
    fn resolver_never_returns_another_principal_or_provider() {
        let rows = [
            row("did:key:zOther", PROVIDER, "other-did", None, None, true),
            row(DID, "chatgpt-codex", "other-provider", None, None, true),
            row(DID, PROVIDER, "mine", None, Some(1_000), true),
        ];
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("mine"));
        let foreign = [row("did:key:zOther", PROVIDER, "b", Some("b"), None, true)];
        assert_eq!(picked(&foreign, AccountPick::Reference(Some("b"))), None);
    }

    #[test]
    fn resolver_returns_nothing_when_no_account_is_enabled() {
        let rows = [row(DID, PROVIDER, "off", None, None, false)];
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), None);
        assert_eq!(picked(&rows, AccountPick::Reference(None)), None);
    }

    #[test]
    fn resolver_reference_names_exactly_one_account() {
        let rows = [
            row(DID, PROVIDER, "earlier", Some("earlier"), Some(1_000), true),
            row(DID, PROVIDER, "original", None, Some(2_000), true),
            row(DID, PROVIDER, "b", Some("b"), Some(3_000), true),
        ];
        assert_eq!(
            picked(&rows, AccountPick::Reference(None)),
            Some("original")
        );
        assert_eq!(picked(&rows, AccountPick::Reference(Some("b"))), Some("b"));
        assert_eq!(picked(&rows, AccountPick::ProviderDefault), Some("earlier"));

        let disabled = [
            row(DID, PROVIDER, "original", None, None, true),
            row(DID, PROVIDER, "b", Some("b"), Some(1_000), false),
        ];
        assert_eq!(picked(&disabled, AccountPick::Reference(Some("b"))), None);
    }

    #[tokio::test]
    async fn resolver_reads_through_config_access() {
        let node = std::sync::Arc::new(test_node().await);
        for credential in [
            row(DID, PROVIDER, "xai-oauth:b", Some("b"), Some(1_000), true),
            row(DID, PROVIDER, "xai-oauth:original", None, None, true),
        ] {
            upsert_oauth_credential(&node, &credential).await.unwrap();
        }
        let access = crate::config_client::ConfigAccess::Local(node);
        for (pick, expected) in [
            (AccountPick::Reference(Some("b")), "xai-oauth:b"),
            (AccountPick::Reference(None), "xai-oauth:original"),
            (AccountPick::ProviderDefault, "xai-oauth:original"),
        ] {
            let resolved = resolve_oauth_credential(&access, DID, PROVIDER, pick)
                .await
                .unwrap()
                .map(|row| row.credential_id);
            assert_eq!(resolved.as_deref(), Some(expected), "{pick:?}");
        }
    }

    #[tokio::test]
    async fn resolver_missing_account_keeps_the_missing_guidance() {
        let node = std::sync::Arc::new(test_node().await);
        let missing = classify_oauth_auth_error(
            &XAI_OAUTH_PRODUCT,
            DID,
            PROVIDER,
            &OAuthAuthProblem::Missing,
        );
        let bootstrap = |pick| {
            crate::oauth_http::bootstrap_oauth_client(
                node.clone(),
                DID,
                PROVIDER,
                OAuthRefreshKind::Xai,
                XAI_OAUTH_PRODUCT,
                pick,
            )
        };
        let Err(error) = bootstrap(AccountPick::ProviderDefault).await else {
            panic!("no account is stored");
        };
        assert_eq!(error.to_string(), missing);

        upsert_oauth_credential(
            &node,
            &row(DID, PROVIDER, "xai-oauth:original", None, None, true),
        )
        .await
        .unwrap();
        let Err(error) = bootstrap(AccountPick::Reference(Some("b"))).await else {
            panic!("a reference never falls back to the original account");
        };
        assert_eq!(error.to_string(), missing);
    }
}

/// Every sign-in writer stores through `upsert_oauth_credential_on`, which
/// writes the key in both upsert branches: a sign-in always leaves its own key.
#[cfg(test)]
mod sign_in_tests {
    use super::test_support::{test_node, unsigned_jwt};
    use super::*;
    use crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
    use crate::config_client::ConfigAccess;

    const DID: &str = "did:key:zSignIn";

    async fn sign_in(node: &Arc<EmbeddedNode>, credential: &OAuthCredential) -> OAuthCredential {
        let doc_id = upsert_oauth_credential_on(&ConfigAccess::Local(node.clone()), credential)
            .await
            .unwrap();
        let stored = lookup_oauth_credential_by_id(node, &credential.credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert_eq!(stored.doc_id.as_deref(), Some(doc_id.as_str()));
        stored
    }

    fn chatgpt(member: Option<&str>, refresh_token: &str) -> OAuthCredential {
        let mut auth = json!({ "chatgpt_account_id": "acct-ws" });
        if let Some(member) = member {
            auth["chatgpt_account_user_id"] = json!(member);
        }
        OAuthCredential::from_login_tokens(
            DID,
            CHATGPT_CODEX_PROVIDER,
            &unsigned_jwt(
                json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" } }),
            ),
            unsigned_jwt(json!({ "https://api.openai.com/auth": auth })),
            refresh_token.to_string(),
            Utc::now(),
        )
    }

    fn claude(label: &str, account_uuid: &str, refresh_token: &str) -> OAuthCredential {
        crate::claude_oauth::credential_from_login_tokens(
            DID,
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            &crate::claude_oauth::ClaudeLoginTokens {
                access_token: "access-TEST".into(),
                refresh_token: refresh_token.into(),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(label.into()),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(account_uuid.into()),
            },
            Utc::now(),
        )
    }

    fn grok(access_claims: Value, refresh_token: &str) -> OAuthCredential {
        crate::xai_oauth_login::credential_from_login_tokens(
            DID,
            crate::xai_grok_oauth::XAI_OAUTH_PROVIDER,
            &crate::xai_oauth_login::XaiLoginTokens {
                access_token: unsigned_jwt(access_claims),
                refresh_token: refresh_token.into(),
                id_token: None,
                expires_in: Some(900),
            },
            Utc::now(),
        )
    }

    #[tokio::test]
    async fn a_matching_sign_in_fills_an_empty_key() {
        let node = Arc::new(test_node().await);
        let before = sign_in(&node, &chatgpt(None, "refresh-1")).await;
        assert_eq!(before.provider_account_key, None);

        let after = sign_in(&node, &chatgpt(Some("member-a"), "refresh-2")).await;
        assert_eq!(after.provider_account_key.as_deref(), Some("member-a"));
        assert_eq!(after.credential_id, before.credential_id);
        assert_eq!(after.doc_id, before.doc_id);
    }

    #[tokio::test]
    async fn the_raw_upsert_of_another_account_replaces_tokens_and_key() {
        let node = Arc::new(test_node().await);
        sign_in(&node, &chatgpt(Some("member-a"), "refresh-1")).await;
        let stored = sign_in(&node, &chatgpt(Some("member-b"), "refresh-2")).await;
        assert_eq!(stored.provider_account_key.as_deref(), Some("member-b"));
        assert_eq!(stored.refresh_token, "refresh-2");

        let first = sign_in(&node, &claude("account-1", "account-1", "refresh-1")).await;
        assert_eq!(
            first.provider_account_key.as_deref(),
            Some("org-1:account-1")
        );
        let stored = sign_in(&node, &claude("account-2", "account-2", "refresh-2")).await;
        assert_eq!(
            stored.provider_account_key.as_deref(),
            Some("org-1:account-2")
        );
        assert_eq!(stored.account_id.as_deref(), Some("account-2"));
        assert_eq!(stored.refresh_token, "refresh-2");
    }

    #[tokio::test]
    async fn a_keyless_sign_in_clears_the_stored_key() {
        let node = Arc::new(test_node().await);
        let principal = json!({ "principal_type": "user", "principal_id": "principal-1" });
        let first = sign_in(&node, &grok(principal, "refresh-1")).await;
        assert_eq!(
            first.provider_account_key.as_deref(),
            Some("user:principal-1")
        );
        let stored = sign_in(&node, &grok(json!({ "sub": "user-a" }), "refresh-2")).await;
        assert_eq!(stored.provider_account_key, None);
        assert_eq!(stored.refresh_token, "refresh-2");

        sign_in(&node, &chatgpt(Some("member-a"), "refresh-1")).await;
        let stored = sign_in(&node, &chatgpt(None, "refresh-2")).await;
        assert_eq!(stored.provider_account_key, None);
        assert_eq!(stored.account_id.as_deref(), Some("acct-ws"));
        assert_eq!(stored.refresh_token, "refresh-2");
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::test_support::{test_node, unsigned_jwt};
    use super::*;
    use crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
    use crate::claude_oauth::CLAUDE_OAUTH_PROVIDER;
    use crate::config_client::ConfigAccess;
    use crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;

    #[derive(Debug, Clone, Copy)]
    enum Product {
        ChatGpt,
        Claude,
        Grok,
    }
    const PRODUCTS: [Product; 3] = [Product::ChatGpt, Product::Claude, Product::Grok];

    impl Product {
        fn provider(self) -> &'static str {
            match self {
                Self::ChatGpt => CHATGPT_CODEX_PROVIDER,
                Self::Claude => CLAUDE_OAUTH_PROVIDER,
                Self::Grok => XAI_OAUTH_PROVIDER,
            }
        }

        /// A sign-in of account `who` (key `member-{who}`, `org-1:account-{who}`
        /// or `user:principal-{who}`; Claude `account_id` `label-{who}`).
        fn sign_in(self, did: &str, who: &str, refresh_token: &str) -> OAuthCredential {
            match self {
                Self::ChatGpt => {
                    let auth = json!({
                        "chatgpt_account_id": "acct-ws",
                        "chatgpt_account_user_id": format!("member-{who}"),
                    });
                    OAuthCredential::from_login_tokens(
                        did,
                        CHATGPT_CODEX_PROVIDER,
                        &unsigned_jwt(
                            json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" } }),
                        ),
                        unsigned_jwt(json!({ "https://api.openai.com/auth": auth })),
                        refresh_token.to_string(),
                        Utc::now(),
                    )
                }
                Self::Claude => crate::claude_oauth::credential_from_login_tokens(
                    did,
                    CLAUDE_OAUTH_PROVIDER,
                    &crate::claude_oauth::ClaudeLoginTokens {
                        access_token: "access-TEST".into(),
                        refresh_token: refresh_token.into(),
                        expires_in: Some(3600),
                        scope: None,
                        account_id: Some(format!("label-{who}")),
                        organization_uuid: Some("org-1".into()),
                        account_uuid: Some(format!("account-{who}")),
                    },
                    Utc::now(),
                ),
                Self::Grok => crate::xai_oauth_login::credential_from_login_tokens(
                    did,
                    XAI_OAUTH_PROVIDER,
                    &crate::xai_oauth_login::XaiLoginTokens {
                        access_token: unsigned_jwt(json!({
                            "principal_type": "user",
                            "principal_id": format!("principal-{who}"),
                        })),
                        refresh_token: refresh_token.into(),
                        id_token: None,
                        expires_in: Some(900),
                    },
                    Utc::now(),
                ),
            }
        }
    }

    async fn access() -> ConfigAccess {
        ConfigAccess::Local(Arc::new(test_node().await))
    }

    async fn store(access: &ConfigAccess, credential: OAuthCredential) -> SignIn {
        store_sign_in(access, credential, None).await.unwrap()
    }

    async fn rows(access: &ConfigAccess, did: &str, provider: &str) -> Vec<OAuthCredential> {
        list_oauth_credentials_on(access, did)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.provider == provider)
            .collect()
    }

    async fn set_enabled(access: &ConfigAccess, credential_id: &str, enabled: bool) {
        let mutation = format!(
            r#"mutation {{ update_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}, input: {{ enabled: {enabled} }}) {{ _docID }} }}"#
        );
        access.write("test.set_enabled", &mutation).await.unwrap();
    }

    fn did(test: &str, product: Product) -> String {
        format!("did:key:z6MkTest{test}{product:?}")
    }

    #[tokio::test]
    async fn a_first_sign_in_is_the_original_account() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("First", product);
            let signed = store(&access, product.sign_in(&did, "a", "refresh-1")).await;
            assert_eq!(signed.result, SignInResult::Added, "{product:?}");
            let stored = rows(&access, &did, product.provider()).await;
            assert_eq!(stored.len(), 1, "{product:?}");
            assert_eq!(
                stored[0].credential_id,
                oauth_credential_id(&did, product.provider())
            );
            assert_eq!(stored[0].account_ref, None, "{product:?}");
            assert!(stored[0].connected_at.is_some(), "{product:?}");
        }
    }

    #[tokio::test]
    async fn another_account_is_added_beside_the_first() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("Another", product);
            let provider = product.provider();
            store(&access, product.sign_in(&did, "a", "refresh-a")).await;
            let first = rows(&access, &did, provider).await.remove(0);

            let signed = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
            assert_eq!(signed.result, SignInResult::Added, "{product:?}");
            let stored = rows(&access, &did, provider).await;
            assert_eq!(stored.len(), 2, "{product:?}");
            let kept = stored
                .iter()
                .find(|row| row.doc_id == first.doc_id)
                .expect("the first row is kept");
            assert_eq!(kept, &first, "{product:?}: the first account is untouched");
            let added = stored
                .iter()
                .find(|row| row.doc_id != first.doc_id)
                .unwrap();
            assert_eq!(added.refresh_token, "refresh-b");
            let reference = added.account_ref.clone().expect("a minted reference");
            assert!(!reference.is_empty() && !reference.contains("did:"));
            assert_ne!(added.account_ref, first.account_ref);
            assert_eq!(added.credential_id, format!("{provider}:{did}:{reference}"));
            assert!(added.connected_at.unwrap() >= first.connected_at.unwrap());

            let resolve = |pick| resolve_oauth_credential(&access, &did, provider, pick);
            let by_ref = resolve(AccountPick::Reference(Some(&reference)))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(by_ref.credential_id, added.credential_id);
            let original = resolve(AccountPick::Reference(None))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(original.credential_id, first.credential_id);
        }
    }

    #[tokio::test]
    async fn the_same_account_refreshes_in_place() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("Same", product);
            let provider = product.provider();
            let first = store(&access, product.sign_in(&did, "a", "refresh-1")).await;
            assert_eq!(first.result, SignInResult::Added, "{product:?}");
            let before = rows(&access, &did, provider).await.remove(0);
            assert!(before.connected_at.is_some(), "{product:?}");
            set_enabled(&access, &before.credential_id, false).await;

            let again = store(&access, product.sign_in(&did, "a", "refresh-2")).await;
            assert_eq!(again.result, SignInResult::Refreshed, "{product:?}");
            let stored = rows(&access, &did, provider).await;
            assert_eq!(stored.len(), 1, "{product:?}");
            let after = &stored[0];
            assert_eq!(after.doc_id, before.doc_id);
            assert_eq!(after.credential_id, before.credential_id);
            assert_eq!(after.account_ref, before.account_ref);
            assert_eq!(after.connected_at, before.connected_at);
            assert_eq!(after.refresh_token, "refresh-2");
            assert!(after.enabled, "{product:?}");
        }
    }

    #[tokio::test]
    async fn two_concurrent_first_sign_ins_both_store() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("Concurrent", product);
            let (a, b) = tokio::join!(
                store_sign_in(&access, product.sign_in(&did, "a", "refresh-a"), None),
                store_sign_in(&access, product.sign_in(&did, "b", "refresh-b"), None),
            );
            a.unwrap();
            b.unwrap();
            let stored = rows(&access, &did, product.provider()).await;
            assert_eq!(stored.len(), 2, "{product:?}");
            assert_eq!(
                stored
                    .iter()
                    .filter(|row| row.account_ref.is_none())
                    .count(),
                1,
                "{product:?}"
            );
        }
    }

    fn chatgpt_with(
        did: &str,
        access_auth: Value,
        id_claims: Value,
        refresh_token: &str,
    ) -> OAuthCredential {
        OAuthCredential::from_login_tokens(
            did,
            CHATGPT_CODEX_PROVIDER,
            &unsigned_jwt(id_claims),
            unsigned_jwt(json!({ "https://api.openai.com/auth": access_auth })),
            refresh_token.to_string(),
            Utc::now(),
        )
    }

    fn claude(
        did: &str,
        label: &str,
        account: Option<&str>,
        refresh_token: &str,
    ) -> OAuthCredential {
        crate::claude_oauth::credential_from_login_tokens(
            did,
            CLAUDE_OAUTH_PROVIDER,
            &crate::claude_oauth::ClaudeLoginTokens {
                access_token: "access-TEST".into(),
                refresh_token: refresh_token.into(),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(label.into()),
                organization_uuid: account.map(|_| "org-1".into()),
                account_uuid: account.map(str::to_owned),
            },
            Utc::now(),
        )
    }

    fn grok(did: &str, principal_id: &str, refresh_token: &str) -> OAuthCredential {
        crate::xai_oauth_login::credential_from_login_tokens(
            did,
            XAI_OAUTH_PROVIDER,
            &crate::xai_oauth_login::XaiLoginTokens {
                access_token: unsigned_jwt(
                    json!({ "principal_type": "user", "principal_id": principal_id }),
                ),
                refresh_token: refresh_token.into(),
                id_token: None,
                expires_in: Some(900),
            },
            Utc::now(),
        )
    }

    /// A row written raw, as older clients or pre-#2116 nodes left it; the
    /// input goes through a variable, so no value is trimmed on the way in.
    async fn seed_raw(access: &ConfigAccess, input: Value) {
        access
            .transact("test.seed_raw", |txn| {
                let input = input.clone();
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($input: OAuthCredentialMutationInputArg!) { create_OAuthCredential(input: $input) { _docID } }",
                        &json!({ "input": input }),
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }

    fn raw_row(did: &str, provider: &str) -> Value {
        json!({
            "credential_id": oauth_credential_id(did, provider),
            "agent_did": did,
            "provider": provider,
            "access_token": "placeholder-access",
            "refresh_token": "placeholder-refresh",
            "is_fedramp": false,
            "access_token_expires_at": "2020-01-01T00:00:00Z",
            "enabled": true,
        })
    }

    #[tokio::test]
    async fn a_chatgpt_account_matches_across_key_forms() {
        let access = access().await;
        let did = "did:key:z6MkTestKeyForms";
        let id = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" }, "chatgpt_user_id": "user-a" });
        let fallback_only = chatgpt_with(
            did,
            json!({ "chatgpt_account_id": "acct-ws" }),
            id.clone(),
            "refresh-1",
        );
        assert_eq!(
            fallback_only.provider_account_key.as_deref(),
            Some("acct-ws:user-a")
        );
        store(&access, fallback_only).await;

        let both = chatgpt_with(
            did,
            json!({ "chatgpt_account_id": "acct-ws", "chatgpt_account_user_id": "member-a" }),
            id,
            "refresh-2",
        );
        let signed = store(&access, both).await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        let stored = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].refresh_token, "refresh-2");
        assert_eq!(
            stored[0].provider_account_key.as_deref(),
            Some("acct-ws:user-a")
        );
    }

    #[tokio::test]
    async fn a_key_form_the_sign_in_does_not_compute_is_rewritten() {
        let access = access().await;
        let did = "did:key:z6MkTestKeyRewrite";
        let id = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-ws" }, "chatgpt_user_id": "user-a" });
        let both = chatgpt_with(
            did,
            json!({ "chatgpt_account_id": "acct-ws", "chatgpt_account_user_id": "member-a" }),
            id.clone(),
            "refresh-1",
        );
        assert_eq!(both.provider_account_key.as_deref(), Some("member-a"));
        store(&access, both).await;

        let fallback_only = chatgpt_with(
            did,
            json!({ "chatgpt_account_id": "acct-ws" }),
            id,
            "refresh-2",
        );
        let signed = store(&access, fallback_only).await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        let stored = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].provider_account_key.as_deref(),
            Some("acct-ws:user-a")
        );
    }

    #[tokio::test]
    async fn an_upgraded_claude_row_with_its_own_account_id_is_filled() {
        let access = access().await;
        let did = "did:key:z6MkTestUpgradedClaude";
        let mut input = raw_row(did, CLAUDE_OAUTH_PROVIDER);
        input["account_id"] = json!("label-a");
        seed_raw(&access, input).await;

        let signed = store(
            &access,
            claude(did, "label-a", Some("account-a"), "refresh-1"),
        )
        .await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert!(signed.identity_matched);
        let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].credential_id,
            oauth_credential_id(did, CLAUDE_OAUTH_PROVIDER)
        );
        assert_eq!(
            stored[0].provider_account_key.as_deref(),
            Some("org-1:account-a")
        );
    }

    #[tokio::test]
    async fn an_upgraded_claude_row_with_another_account_id_is_not_overwritten() {
        let access = access().await;
        let did = "did:key:z6MkTestUpgradedOther";
        let mut input = raw_row(did, CLAUDE_OAUTH_PROVIDER);
        input["account_id"] = json!("label-a");
        seed_raw(&access, input).await;
        let before = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.remove(0);

        let signed = store(
            &access,
            claude(did, "label-b", Some("account-b"), "refresh-1"),
        )
        .await;
        assert_eq!(signed.result, SignInResult::Added);
        let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
        assert_eq!(stored.len(), 2);
        assert!(stored.contains(&before));
    }

    #[tokio::test]
    async fn a_row_with_no_identity_is_filled() {
        let access = access().await;
        let did = "did:key:z6MkTestNoIdentity";
        seed_raw(&access, raw_row(did, CHATGPT_CODEX_PROVIDER)).await;

        let signed = store(&access, Product::ChatGpt.sign_in(did, "a", "refresh-1")).await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert!(!signed.identity_matched);
        let stored = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].provider_account_key.as_deref(), Some("member-a"));
    }

    #[tokio::test]
    async fn a_keyless_sign_in_adds_beside_a_keyed_row() {
        let access = access().await;
        let did = "did:key:z6MkTestKeyless";
        store(
            &access,
            claude(did, "label-a", Some("account-a"), "refresh-1"),
        )
        .await;
        let before = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.remove(0);

        let signed = store(&access, claude(did, "label-a", None, "refresh-2")).await;
        assert_eq!(signed.result, SignInResult::Added);
        let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
        assert_eq!(stored.len(), 2);
        assert!(stored.contains(&before));
    }

    #[tokio::test]
    async fn a_stored_key_beside_other_tokens_is_not_trusted() {
        let access = access().await;
        let did = "did:key:z6MkTestStaleKey";
        let mut stale = grok(did, "principal-2", "refresh-1");
        stale.provider_account_key = Some("user:principal-1".into());
        seed_raw(
            &access,
            json!({
                "credential_id": oauth_credential_id(did, XAI_OAUTH_PROVIDER),
                "agent_did": did,
                "provider": XAI_OAUTH_PROVIDER,
                "access_token": stale.access_token,
                "refresh_token": "refresh-1",
                "is_fedramp": false,
                "access_token_expires_at": "2030-01-01T00:00:00Z",
                "enabled": true,
                "provider_account_key": "user:principal-1",
            }),
        )
        .await;
        let signed = store(&access, grok(did, "principal-2", "refresh-2")).await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert_eq!(rows(&access, did, XAI_OAUTH_PROVIDER).await.len(), 1);

        let member_b = Product::ChatGpt.sign_in(did, "b", "refresh-1");
        seed_raw(
            &access,
            json!({
                "credential_id": oauth_credential_id(did, CHATGPT_CODEX_PROVIDER),
                "agent_did": did,
                "provider": CHATGPT_CODEX_PROVIDER,
                "access_token": member_b.access_token,
                "refresh_token": "refresh-1",
                "id_token": member_b.id_token,
                "account_id": "acct-ws",
                "is_fedramp": false,
                "access_token_expires_at": "2030-01-01T00:00:00Z",
                "enabled": true,
                "provider_account_key": "member-a",
            }),
        )
        .await;
        let signed = store(&access, Product::ChatGpt.sign_in(did, "b", "refresh-2")).await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert_eq!(rows(&access, did, CHATGPT_CODEX_PROVIDER).await.len(), 1);
    }

    #[tokio::test]
    async fn a_claude_row_with_a_stale_key_is_not_overwritten() {
        let access = access().await;
        let did = "did:key:z6MkTestClaudeSkew";
        // A pre-PR4 client replaced tokens and label but kept the key.
        store(
            &access,
            claude(did, "label-b", Some("account-1"), "refresh-1"),
        )
        .await;
        let before = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.remove(0);

        let signed = store(
            &access,
            claude(did, "label-a", Some("account-1"), "refresh-2"),
        )
        .await;
        assert_eq!(signed.result, SignInResult::Added);
        let signed = store(
            &access,
            claude(did, "label-b", Some("account-2"), "refresh-3"),
        )
        .await;
        assert_eq!(signed.result, SignInResult::Added);
        assert!(rows(&access, did, CLAUDE_OAUTH_PROVIDER)
            .await
            .contains(&before));
    }

    #[tokio::test]
    async fn a_claude_row_matches_on_its_stored_key() {
        let access = access().await;
        let did = "did:key:z6MkTestClaudeKeys";
        store(
            &access,
            claude(did, "label-1", Some("account-1"), "refresh-1"),
        )
        .await;
        let second = store(
            &access,
            claude(did, "label-2", Some("account-2"), "refresh-2"),
        )
        .await;

        let signed = store(
            &access,
            claude(did, "label-2", Some("account-2"), "refresh-3"),
        )
        .await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert_eq!(signed.doc_id, second.doc_id);
        assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 2);
    }

    #[tokio::test]
    async fn padded_values_compare_trimmed() {
        let access = access().await;
        let did = "did:key:z6MkTestPadded";
        store(&access, grok(did, "principal-1", "refresh-1")).await;
        let padded = grok(did, "principal-1 ", "refresh-2");
        assert_eq!(
            padded.provider_account_key.as_deref(),
            Some("user:principal-1 ")
        );
        let signed = store(&access, padded).await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert_eq!(rows(&access, did, XAI_OAUTH_PROVIDER).await.len(), 1);

        let mut input = raw_row(did, CLAUDE_OAUTH_PROVIDER);
        input["account_id"] = json!("label-a");
        input["provider_account_key"] = json!(" org-1:account-1 ");
        seed_raw(&access, input).await;
        let signed = store(
            &access,
            claude(did, "label-a", Some("account-1"), "refresh-3"),
        )
        .await;
        assert_eq!(signed.result, SignInResult::Refreshed);
        assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 1);
    }

    async fn labeled(access: &ConfigAccess, credential: OAuthCredential, label: &str) -> SignIn {
        store_sign_in(access, credential, Some(label))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_new_account_gets_the_next_free_name() {
        let access = access().await;
        for (product, name) in [
            (Product::ChatGpt, "ChatGPT"),
            (Product::Claude, "Claude"),
            (Product::Grok, "Grok"),
        ] {
            let did = did("NextFree", product);
            let a = store(&access, product.sign_in(&did, "a", "refresh-a")).await;
            assert_eq!(
                a.credential.label, None,
                "{product:?}: the original shows {name}"
            );
            let b = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
            assert_eq!(
                b.credential.label.as_deref(),
                Some(format!("{name} 2").as_str())
            );
            let c = store(&access, product.sign_in(&did, "c", "refresh-c")).await;
            assert_eq!(
                c.credential.label.as_deref(),
                Some(format!("{name} 3").as_str())
            );
            labeled(&access, product.sign_in(&did, "b", "refresh-b2"), "Work").await;
            let d = store(&access, product.sign_in(&did, "d", "refresh-d")).await;
            assert_eq!(
                d.credential.label.as_deref(),
                Some(format!("{name} 2").as_str())
            );
            let stored = rows(&access, &did, product.provider()).await;
            let mut labels: Vec<_> = stored.iter().map(|row| row.label.clone()).collect();
            labels.sort();
            let expected = |n: &str| Some(format!("{name}{n}"));
            assert_eq!(
                labels,
                {
                    let mut expected = vec![
                        None,
                        expected(" 2"),
                        expected(" 3"),
                        Some("Work".to_string()),
                    ];
                    expected.sort();
                    expected
                },
                "{product:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_label_names_and_a_refresh_renames() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("Rename", product);
            let signed = labeled(&access, product.sign_in(&did, "a", "refresh-1"), "Work").await;
            assert_eq!(signed.credential.label.as_deref(), Some("Work"));
            let signed = store(&access, product.sign_in(&did, "a", "refresh-2")).await;
            assert_eq!(signed.credential.label.as_deref(), Some("Work"));
            assert_eq!(
                rows(&access, &did, product.provider()).await[0]
                    .label
                    .as_deref(),
                Some("Work")
            );
            let signed =
                labeled(&access, product.sign_in(&did, "a", "refresh-3"), "Personal").await;
            assert_eq!(signed.result, SignInResult::Refreshed);
            let stored = rows(&access, &did, product.provider()).await;
            assert_eq!(stored.len(), 1);
            assert_eq!(stored[0].label.as_deref(), Some("Personal"), "{product:?}");
        }
    }

    #[tokio::test]
    async fn labels_are_unique_per_provider_and_clean() {
        let access = access().await;
        let did = "did:key:z6MkTestLabels";
        labeled(
            &access,
            Product::Claude.sign_in(did, "a", "refresh-1"),
            "Work",
        )
        .await;
        for taken in ["Work", " Work "] {
            let error = store_sign_in(
                &access,
                Product::Claude.sign_in(did, "b", "refresh-2"),
                Some(taken),
            )
            .await
            .expect_err("a duplicate label");
            assert!(error.to_string().contains("Work"), "{error}");
        }
        assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 1);
        labeled(
            &access,
            Product::Grok.sign_in(did, "a", "refresh-1"),
            "Work",
        )
        .await;

        let long = "x".repeat(65);
        for bad in ["", "   ", long.as_str(), "Work\nmore", "tab\there"] {
            assert!(
                store_sign_in(
                    &access,
                    Product::ChatGpt.sign_in(did, "a", "refresh-1"),
                    Some(bad)
                )
                .await
                .is_err(),
                "{bad:?}"
            );
        }
        assert!(rows(&access, did, CHATGPT_CODEX_PROVIDER).await.is_empty());
        let max = "x".repeat(64);
        labeled(
            &access,
            Product::ChatGpt.sign_in(did, "a", "refresh-1"),
            &max,
        )
        .await;
    }

    fn node(access: &ConfigAccess) -> &EmbeddedNode {
        match access {
            ConfigAccess::Local(node) => node,
            ConfigAccess::Graphql(_) => unreachable!("tests run on a local node"),
        }
    }

    async fn backends(access: &ConfigAccess, did: &str) -> Vec<crate::InferenceBackend> {
        crate::backend_registry::list_all_backends(node(access))
            .await
            .unwrap()
            .into_iter()
            .filter(|backend| backend.agent_did == did)
            .collect()
    }

    fn preset(product: Product) -> crate::inference_setup::InferenceConnectionSpec {
        use crate::inference_setup::{InferenceAuthMethod as Auth, InferenceProviderId as Id};
        let (provider, auth) = match product {
            Product::ChatGpt => (Id::OpenAi, Auth::ChatGptOauth),
            Product::Claude => (Id::Anthropic, Auth::ClaudeOauth),
            Product::Grok => (Id::Grok, Auth::GrokOauth),
        };
        crate::inference_setup::connection_spec(provider, auth, "").unwrap()
    }

    #[tokio::test]
    async fn only_added_accounts_create_a_backend() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("Backend", product);
            store(&access, product.sign_in(&did, "a", "refresh-a")).await;
            assert!(backends(&access, &did).await.is_empty(), "{product:?}");

            let b = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
            let reference = b.credential.account_ref.clone().unwrap();
            let stored = backends(&access, &did).await;
            assert_eq!(stored.len(), 1, "{product:?}");
            let backend = &stored[0];
            let spec = preset(product);
            assert_eq!(
                backend.backend_id,
                format!("{}-{reference}", product.provider())
            );
            assert_eq!(Some(backend.name.clone()), b.credential.label);
            assert_eq!(
                backend.auth,
                crate::document_config::BackendAuth::PrincipalOAuth {
                    account_ref: Some(reference.clone())
                }
            );
            assert_eq!(backend.provider_kind, spec.provider_kind);
            assert_eq!(backend.openai_wire_api, spec.openai_wire_api);
            assert_eq!(backend.endpoint, spec.endpoint);
            assert!(backend.enabled);

            store(&access, product.sign_in(&did, "b", "refresh-b2")).await;
            let ids = |backends: Vec<crate::InferenceBackend>| {
                backends
                    .into_iter()
                    .map(|backend| (backend.backend_id, backend.name))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                ids(backends(&access, &did).await),
                ids(stored),
                "{product:?}"
            );
            labeled(
                &access,
                product.sign_in(&did, "b", "refresh-b3"),
                "Personal",
            )
            .await;
            let renamed = backends(&access, &did).await;
            assert_eq!(renamed.len(), 1);
            assert_eq!(renamed[0].name, "Personal", "{product:?}");
        }
    }

    #[tokio::test]
    async fn each_accounts_backend_keeps_its_own_models() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("Models", product);
            let spec = preset(product);
            let original = crate::InferenceBackend {
                agent_did: did.clone(),
                backend_id: format!("{}-original", product.provider()),
                name: "Original".into(),
                provider_kind: spec.provider_kind,
                openai_wire_api: spec.openai_wire_api,
                endpoint: spec.endpoint.clone(),
                auth: crate::document_config::BackendAuth::PrincipalOAuth { account_ref: None },
                connect_timeout_secs: None,
                discovery_timeout_secs: None,
                max_concurrent: None,
                max_queue_depth: None,
                enabled: true,
                tags: Vec::new(),
            };
            crate::config_client::write_inference_backend_document(&access, &original)
                .await
                .unwrap();
            store(&access, product.sign_in(&did, "a", "refresh-a")).await;
            store(&access, product.sign_in(&did, "b", "refresh-b")).await;
            let added = backends(&access, &did)
                .await
                .into_iter()
                .find(|backend| backend.backend_id != original.backend_id)
                .expect("the added account's backend");
            for (backend, model) in [(&original, "model-a"), (&added, "model-b")] {
                let advertised: crate::document_config::AdvertisedModel =
                    serde_json::from_value(json!({
                        "model_name": model,
                        "display_name": null,
                        "context_window": null,
                        "max_context_window": null,
                        "max_output_tokens": null,
                        "reasoning_efforts": null,
                    }))
                    .unwrap();
                crate::backend_registry::record_discovered_catalog_on(
                    &access,
                    backend,
                    vec![advertised],
                )
                .await
                .unwrap();
            }
            for (backend, model) in [(&original, "model-a"), (&added, "model-b")] {
                let observation = crate::backend_registry::lookup_backend_observation(
                    node(&access),
                    &did,
                    &backend.backend_id,
                )
                .await
                .unwrap()
                .unwrap();
                let models: Vec<_> = observation
                    .catalogs
                    .iter()
                    .flat_map(|catalog| catalog.models.iter().map(|m| m.model_name.as_str()))
                    .collect();
                assert_eq!(models, [model], "{product:?}");
            }
        }
    }

    #[tokio::test]
    async fn a_lost_backend_comes_back_at_the_next_sign_in() {
        let access = access().await;
        for product in PRODUCTS {
            let did = did("LostBackend", product);
            store(&access, product.sign_in(&did, "a", "refresh-a")).await;
            let b = store(&access, product.sign_in(&did, "b", "refresh-b")).await;
            let reference = b.credential.account_ref.clone().unwrap();
            let serving = |backends: Vec<crate::InferenceBackend>| {
                backends
                    .into_iter()
                    .filter(|backend| backend.auth.oauth_account_ref() == Some(reference.as_str()))
                    .count()
            };
            let backend_id = format!("{}-{reference}", product.provider());
            access
                .write(
                    "test.backend_rm",
                    &format!(
                        r#"mutation {{ delete_InferenceBackend(filter: {{ backend_id: {{ _eq: "{backend_id}" }} }}) {{ _docID }} }}"#
                    ),
                )
                .await
                .unwrap();
            assert_eq!(serving(backends(&access, &did).await), 0);

            let again = store(&access, product.sign_in(&did, "b", "refresh-b2")).await;
            assert_eq!(again.result, SignInResult::Refreshed);
            assert_eq!(serving(backends(&access, &did).await), 1, "{product:?}");
            store(&access, product.sign_in(&did, "b", "refresh-b3")).await;
            assert_eq!(serving(backends(&access, &did).await), 1, "{product:?}");
        }
    }

    #[tokio::test]
    async fn an_invalid_config_fails_an_add_with_a_clear_error() {
        let access = access().await;
        let did = "did:key:z6MkTestInvalidConfig";
        store(&access, Product::Claude.sign_in(did, "a", "refresh-a")).await;
        access
            .write(
                "test.seed_invalid_profile",
                &format!(
                    r#"mutation {{ create_InferenceProfile(input: {{ agent_did: "{did}", profile_id: "profile-x", backend_id: "missing-backend", model_name: "model-x" }}) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();

        let error = store_sign_in(
            &access,
            Product::Claude.sign_in(did, "b", "refresh-b"),
            None,
        )
        .await
        .expect_err("an invalid config fails the add");
        let text = format!("{error:#}");
        assert!(text.contains("missing-backend"), "{text}");
        assert!(text.contains("sign in again"), "{text}");
        assert_eq!(rows(&access, did, CLAUDE_OAUTH_PROVIDER).await.len(), 1);
        assert!(backends(&access, did).await.is_empty());
    }

    async fn remove(access: &ConfigAccess, did: &str, credential_id: &str) -> Result<()> {
        access
            .transact("test.remove_account", |txn| {
                Box::pin(async move { remove_account_in_txn(txn, did, credential_id).await })
            })
            .await
    }

    #[tokio::test]
    async fn list_accounts_is_ordered_token_free_and_marks_the_default() {
        let access = access().await;
        let did = "did:key:z6MkTestListAccounts";
        let a = store(
            &access,
            Product::Claude.sign_in(did, "a", "refresh-SECRET-a"),
        )
        .await;
        store(
            &access,
            Product::Claude.sign_in(did, "b", "refresh-SECRET-b"),
        )
        .await;
        store(
            &access,
            Product::Claude.sign_in(did, "c", "refresh-SECRET-c"),
        )
        .await;
        store(&access, Product::Grok.sign_in(did, "a", "refresh-SECRET-g")).await;
        set_enabled(&access, &a.credential.credential_id, false).await;

        let listed = list_accounts(&access, did).await.unwrap();
        let rows = list_oauth_credentials_on(&access, did).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|account| &account.credential_id)
                .collect::<Vec<_>>(),
            rows.iter()
                .map(|row| &row.credential_id)
                .collect::<Vec<_>>()
        );
        for provider in [CLAUDE_OAUTH_PROVIDER, XAI_OAUTH_PROVIDER] {
            let default =
                resolve_oauth_credential(&access, did, provider, AccountPick::ProviderDefault)
                    .await
                    .unwrap()
                    .unwrap();
            let marked: Vec<_> = listed
                .iter()
                .filter(|account| account.provider == provider && account.default)
                .map(|account| account.credential_id.clone())
                .collect();
            assert_eq!(marked, [default.credential_id], "{provider}");
        }
        let claude: Vec<_> = listed
            .iter()
            .filter(|account| account.provider == CLAUDE_OAUTH_PROVIDER)
            .collect();
        let mut labels: Vec<_> = claude
            .iter()
            .map(|account| account.label.as_str())
            .collect();
        labels.sort();
        assert_eq!(labels, ["Claude", "Claude 2", "Claude 3"]);
        let original = claude
            .iter()
            .find(|account| account.account_ref.is_none())
            .unwrap();
        assert!(!original.enabled);
        assert_eq!(original.identity.as_deref(), Some("label-a"));
        let text = serde_json::to_string(&listed).unwrap();
        assert!(
            !text.contains("SECRET") && !text.contains("access-TEST"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_cloud_workspace_token_is_not_an_account() {
        let access = access().await;
        let did = "did:key:z6MkTestCloudToken";
        let mut cloud = Product::Grok.sign_in(did, "a", "refresh-1");
        cloud.provider = "gents-cloud".into();
        cloud.credential_id = oauth_credential_id(did, "gents-cloud");
        upsert_oauth_credential_on(&access, &cloud).await.unwrap();
        let before = list_oauth_credentials_on(&access, did).await.unwrap();

        assert!(list_accounts(&access, did).await.unwrap().is_empty());
        let id = cloud.credential_id.as_str();
        for error in [
            set_account_label(&access, did, id, "Work")
                .await
                .unwrap_err(),
            set_account_enabled(&access, did, id, false)
                .await
                .unwrap_err(),
            remove(&access, did, id).await.unwrap_err(),
        ] {
            assert!(error.to_string().contains("not found"), "{error}");
        }
        assert_eq!(
            list_oauth_credentials_on(&access, did).await.unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn a_label_change_is_validated_unique_and_renames_the_backend() {
        let access = access().await;
        let did = "did:key:z6MkTestSetLabel";
        store(&access, Product::Claude.sign_in(did, "a", "refresh-a")).await;
        let b = store(&access, Product::Claude.sign_in(did, "b", "refresh-b")).await;
        let id = b.credential.credential_id.as_str();

        set_account_label(&access, did, id, " Work ").await.unwrap();
        let row = rows(&access, did, CLAUDE_OAUTH_PROVIDER)
            .await
            .into_iter()
            .find(|row| row.credential_id == id)
            .unwrap();
        assert_eq!(row.label.as_deref(), Some("Work"));
        let backend = backends(&access, did).await.remove(0);
        assert_eq!(backend.name, "Work");

        for bad in ["Claude", "", "a\u{7}b"] {
            assert!(
                set_account_label(&access, did, id, bad).await.is_err(),
                "{bad:?}"
            );
        }

        let mut custom = backend.clone();
        custom.name = "Custom".into();
        crate::config_client::write_inference_backend_document(&access, &custom)
            .await
            .unwrap();
        set_account_label(&access, did, id, "Home").await.unwrap();
        assert_eq!(backends(&access, did).await.remove(0).name, "Custom");
    }

    #[tokio::test]
    async fn disable_keeps_the_tokens() {
        let access = access().await;
        let did = "did:key:z6MkTestDisable";
        let a = store(&access, Product::Grok.sign_in(did, "a", "refresh-a")).await;
        let before = rows(&access, did, XAI_OAUTH_PROVIDER).await.remove(0);
        set_account_enabled(&access, did, &a.credential.credential_id, false)
            .await
            .unwrap();
        let after = rows(&access, did, XAI_OAUTH_PROVIDER).await.remove(0);
        assert!(!after.enabled);
        assert_eq!(after.access_token, before.access_token);
        assert_eq!(after.refresh_token, before.refresh_token);
        set_account_enabled(&access, did, &a.credential.credential_id, true)
            .await
            .unwrap();
        assert!(rows(&access, did, XAI_OAUTH_PROVIDER).await[0].enabled);
    }

    #[tokio::test]
    async fn remove_deletes_the_row() {
        let access = access().await;
        let did = "did:key:z6MkTestRemove";
        let a = store(&access, Product::ChatGpt.sign_in(did, "a", "refresh-a")).await;
        let b = store(&access, Product::ChatGpt.sign_in(did, "b", "refresh-b")).await;
        remove(&access, did, &b.credential.credential_id)
            .await
            .unwrap();
        let left = rows(&access, did, CHATGPT_CODEX_PROVIDER).await;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].credential_id, a.credential.credential_id);
        let error = remove(&access, did, &b.credential.credential_id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not found"), "{error}");
    }

    /// Q1 (i): with no row left, a sign-in takes the original slot again and
    /// the backends with no account reference run on it, deliberately.
    #[tokio::test]
    async fn a_sign_in_after_removing_the_only_account_takes_the_original_slot() {
        let access = access().await;
        let did = "did:key:z6MkTestSlotReuse";
        for product in PRODUCTS {
            let spec = preset(product);
            let original = crate::InferenceBackend {
                agent_did: did.to_owned(),
                backend_id: format!("{}-original", product.provider()),
                name: format!("{product:?}"),
                provider_kind: spec.provider_kind,
                openai_wire_api: spec.openai_wire_api,
                endpoint: spec.endpoint,
                auth: crate::document_config::BackendAuth::PrincipalOAuth { account_ref: None },
                connect_timeout_secs: None,
                discovery_timeout_secs: None,
                max_concurrent: None,
                max_queue_depth: None,
                enabled: true,
                tags: Vec::new(),
            };
            crate::config_client::write_inference_backend_document(&access, &original)
                .await
                .unwrap();
            let profile = serde_json::from_value(json!({
                "agent_did": did,
                "profile_id": format!("{product:?}-profile"),
                "backend_id": original.backend_id,
                "model_name": "model-x",
            }))
            .unwrap();
            crate::config_client::write_inference_profile_document(&access, &profile)
                .await
                .unwrap();
        }
        let a = store(&access, Product::Claude.sign_in(did, "a", "refresh-a")).await;
        remove(&access, did, &a.credential.credential_id)
            .await
            .unwrap();
        let b = store(&access, Product::Claude.sign_in(did, "b", "refresh-b")).await;
        assert_eq!(b.result, SignInResult::Added);
        assert_eq!(b.credential.credential_id, a.credential.credential_id);
        assert_eq!(b.credential.account_ref, None);
        assert_ne!(b.doc_id, a.doc_id);
        let stored = rows(&access, did, CLAUDE_OAUTH_PROVIDER).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].refresh_token, "refresh-b");
        assert_eq!(b.profiles, ["Claude-profile"]);
        let note = b.profiles_note().expect("the sign-in names its profiles");
        assert!(note.contains("Claude-profile"), "{note}");

        let added = store(&access, Product::Claude.sign_in(did, "c", "refresh-c")).await;
        assert!(added.profiles.is_empty());
        assert_eq!(added.profiles_note(), None);
    }

    #[tokio::test]
    async fn another_principals_account_is_not_touched() {
        let access = access().await;
        let mine = "did:key:z6MkTestMine";
        let theirs = "did:key:z6MkTestTheirs";
        let signed = store(&access, Product::Claude.sign_in(theirs, "a", "refresh-a")).await;
        let id = signed.credential.credential_id.as_str();
        let before = rows(&access, theirs, CLAUDE_OAUTH_PROVIDER).await;
        for error in [
            set_account_label(&access, mine, id, "Work")
                .await
                .unwrap_err(),
            set_account_enabled(&access, mine, id, false)
                .await
                .unwrap_err(),
            remove(&access, mine, id).await.unwrap_err(),
        ] {
            assert!(error.to_string().contains("not found"), "{error}");
        }
        assert_eq!(rows(&access, theirs, CLAUDE_OAUTH_PROVIDER).await, before);
    }

    #[tokio::test]
    async fn a_provider_no_backend_reads_is_refused() {
        let access = access().await;
        let did = "did:key:z6MkTestOtherProvider";
        let mut credential = Product::Grok.sign_in(did, "a", "refresh-1");
        credential.provider = "other-provider".into();
        credential.credential_id = oauth_credential_id(did, "other-provider");
        assert!(store_sign_in(&access, credential, None).await.is_err());
        assert!(list_oauth_credentials_on(&access, did)
            .await
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod backfill_tests {
    use super::test_support::{one_shot_token_server, test_node, unsigned_jwt, TOKEN_URL_ENV};
    use super::*;
    use crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
    use crate::config_client::ConfigAccess;
    use crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;

    const XAI_ENV: &str = crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV;
    const CHATGPT_ENV: &str = gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;

    /// A token response whose access token carries `access_claims`.
    fn token_response(access_claims: Value) -> &'static str {
        let body = json!({
            "access_token": unsigned_jwt(access_claims),
            "refresh_token": "refresh-rotated",
            "expires_in": 900
        });
        Box::leak(body.to_string().into_boxed_str())
    }

    async fn seed_expired_grok(node: &EmbeddedNode, did: &str, key: Option<&str>) {
        let credential = OAuthCredential {
            doc_id: None,
            credential_id: oauth_credential_id(did, XAI_OAUTH_PROVIDER),
            agent_did: did.to_string(),
            provider: XAI_OAUTH_PROVIDER.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: Utc::now() - Duration::minutes(1),
            last_refresh: None,
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: key.map(str::to_owned),
            label: None,
        };
        upsert_oauth_credential(node, &credential).await.unwrap();
    }

    /// Refresh `did`'s `provider` row once through an owner bearer built
    /// directly (never the shared registry) and read the stored row back.
    async fn refresh_once(
        node: &Arc<EmbeddedNode>,
        did: &str,
        provider: &str,
        kind: OAuthRefreshKind,
        body: &'static str,
    ) -> OAuthCredential {
        let (env, product) = match kind {
            OAuthRefreshKind::ChatGpt => (CHATGPT_ENV, CHATGPT_OAUTH_PRODUCT),
            _ => (XAI_ENV, XAI_OAUTH_PRODUCT),
        };
        let credential_id = oauth_credential_id(did, provider);
        let _env = TOKEN_URL_ENV.lock().await;
        let (url, handle) = one_shot_token_server(200, body).await;
        std::env::set_var(env, &url);
        let bearer = DbCredentialBearer::new(
            node.clone(),
            did,
            provider,
            credential_id.clone(),
            true,
            kind,
            product,
        );
        let refreshed = bearer.current_bearer().await;
        std::env::remove_var(env);
        handle.await.expect("server");
        refreshed.expect("refresh");
        lookup_oauth_credential_by_id(node, &credential_id)
            .await
            .unwrap()
            .expect("stored row")
    }

    #[tokio::test]
    async fn refresh_fills_an_empty_key_from_the_refreshed_claims() {
        let node = Arc::new(test_node().await);
        let did = "did:key:zBackfillFill";
        seed_expired_grok(&node, did, None).await;
        let body =
            token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
        let stored =
            refresh_once(&node, did, XAI_OAUTH_PROVIDER, OAuthRefreshKind::Xai, body).await;
        assert_eq!(stored.refresh_token, "refresh-rotated");
        assert_eq!(
            stored.provider_account_key.as_deref(),
            Some("user:principal-1")
        );
    }

    #[tokio::test]
    async fn refresh_never_changes_a_present_key() {
        let node = Arc::new(test_node().await);
        let did = "did:key:zBackfillPresent";
        seed_expired_grok(&node, did, Some("user:principal-old")).await;
        let body =
            token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
        let stored =
            refresh_once(&node, did, XAI_OAUTH_PROVIDER, OAuthRefreshKind::Xai, body).await;
        assert_eq!(stored.refresh_token, "refresh-rotated");
        assert_eq!(
            stored.provider_account_key.as_deref(),
            Some("user:principal-old")
        );
    }

    #[tokio::test]
    async fn refresh_leaves_the_key_empty_without_the_claim() {
        let node = Arc::new(test_node().await);
        let did = "did:key:zBackfillNoClaim";
        seed_expired_grok(&node, did, None).await;
        let body = token_response(json!({ "sub": "user-a" }));
        let stored =
            refresh_once(&node, did, XAI_OAUTH_PROVIDER, OAuthRefreshKind::Xai, body).await;
        assert_eq!(stored.refresh_token, "refresh-rotated");
        assert_eq!(stored.provider_account_key, None);
    }

    /// Refresh `did`'s expired Grok row through an owner bearer while
    /// `change` runs between the owner's pre-refresh read and its persist.
    /// Returns the owner bearer.
    async fn refresh_racing<F>(node: &Arc<EmbeddedNode>, did: &str, change: F) -> DbCredentialBearer
    where
        F: std::future::Future<Output = ()>,
    {
        let body =
            token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
        let _env = TOKEN_URL_ENV.lock().await;
        let (received_tx, received) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let (url, handle) =
            super::test_support::gated_token_server(200, body, Some((received_tx, release_rx)))
                .await;
        std::env::set_var(XAI_ENV, &url);
        let bearer = DbCredentialBearer::new(
            node.clone(),
            did,
            XAI_OAUTH_PROVIDER,
            oauth_credential_id(did, XAI_OAUTH_PROVIDER),
            true,
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        );
        let (refreshed, ()) = tokio::join!(bearer.current_bearer(), async {
            received.await.expect("the owner asked for a refresh");
            change.await;
            release.send(()).expect("server waits");
        });
        std::env::remove_var(XAI_ENV);
        handle.await.expect("server");
        refreshed.expect("the owner still serves its refreshed token");
        bearer
    }

    #[tokio::test]
    async fn a_refresh_racing_a_remove_does_not_revive_the_row() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestRaceRemove";
        seed_expired_grok(&node, did, None).await;
        let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
        refresh_racing(&node, did, async {
            ConfigAccess::write_local(
                &node,
                "test.remove",
                &format!(
                    r#"mutation {{ delete_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();
        })
        .await;
        assert_eq!(
            lookup_oauth_credential_by_id(&node, &credential_id)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_refresh_racing_a_disable_keeps_it_disabled() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestRaceDisable";
        seed_expired_grok(&node, did, None).await;
        let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
        refresh_racing(&node, did, async {
            ConfigAccess::write_local(
                &node,
                "test.disable",
                &format!(
                    r#"mutation {{ update_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}, input: {{ enabled: false }}) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();
        })
        .await;
        let stored = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert!(!stored.enabled);
        assert_eq!(stored.refresh_token, "refresh-rotated");
        assert_eq!(
            stored.provider_account_key.as_deref(),
            Some("user:principal-1")
        );
    }

    #[tokio::test]
    async fn a_refresh_racing_a_sign_in_keeps_the_sign_in() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestRaceSignIn";
        seed_expired_grok(&node, did, None).await;
        let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
        let sign_in = crate::xai_oauth_login::credential_from_login_tokens(
            did,
            XAI_OAUTH_PROVIDER,
            &crate::xai_oauth_login::XaiLoginTokens {
                access_token: unsigned_jwt(
                    json!({ "principal_type": "user", "principal_id": "principal-1" }),
                ),
                refresh_token: "refresh-signed-in".into(),
                id_token: None,
                expires_in: Some(900),
            },
            Utc::now(),
        );
        let access = ConfigAccess::Local(node.clone());
        let bearer = refresh_racing(&node, did, async {
            let signed = store_sign_in(&access, sign_in.clone(), None).await.unwrap();
            assert_eq!(signed.result, SignInResult::Refreshed);
        })
        .await;
        let stored = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert_eq!(stored.refresh_token, "refresh-signed-in");
        assert_eq!(stored.access_token, sign_in.access_token);
        assert_eq!(
            stored.provider_account_key.as_deref(),
            Some("user:principal-1")
        );

        // The next refresh refreshes the sign-in, not the superseded session.
        let _env = TOKEN_URL_ENV.lock().await;
        let body =
            token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
        let (url, handle) = one_shot_token_server(200, body).await;
        std::env::set_var(XAI_ENV, &url);
        bearer.invalidate().await;
        let refreshed = bearer.current_bearer().await;
        std::env::remove_var(XAI_ENV);
        let request = handle.await.expect("server");
        refreshed.expect("second refresh");
        assert!(request.contains("refresh-signed-in"), "{request}");
    }

    /// Q1 (i): remove-all then a first sign-in re-creates the row at the same
    /// `credential_id` while a refresh of the removed row is in flight.
    #[tokio::test]
    async fn a_refresh_racing_a_replaced_sign_in_keeps_the_new_row() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestRaceReplaced";
        seed_expired_grok(&node, did, None).await;
        let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
        let sign_in = crate::xai_oauth_login::credential_from_login_tokens(
            did,
            XAI_OAUTH_PROVIDER,
            &crate::xai_oauth_login::XaiLoginTokens {
                access_token: unsigned_jwt(
                    json!({ "principal_type": "user", "principal_id": "principal-2" }),
                ),
                refresh_token: "refresh-replaced".into(),
                id_token: None,
                expires_in: Some(900),
            },
            Utc::now(),
        );
        let access = ConfigAccess::Local(node.clone());
        refresh_racing(&node, did, async {
            ConfigAccess::write_local(
                &node,
                "test.remove",
                &format!(
                    r#"mutation {{ delete_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();
            let signed = store_sign_in(&access, sign_in.clone(), None).await.unwrap();
            assert_eq!(signed.result, SignInResult::Added);
            assert_eq!(signed.credential.credential_id, credential_id);
        })
        .await;
        let stored = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .expect("stored row");
        assert_eq!(stored.refresh_token, "refresh-replaced");
        assert_eq!(stored.access_token, sign_in.access_token);
        assert_eq!(
            stored.provider_account_key.as_deref(),
            Some("user:principal-2")
        );
    }

    /// A cached removed document is never refreshed once its slot holds
    /// another document, even when the cached expiry is the later one.
    #[tokio::test]
    async fn a_refresh_adopts_a_replaced_document() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestRefreshReplaced";
        seed_expired_grok(&node, did, None).await;
        let credential_id = oauth_credential_id(did, XAI_OAUTH_PROVIDER);
        let mut removed = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .expect("stored row");
        removed.doc_id = Some("doc-removed".into());
        removed.refresh_token = "refresh-removed".into();
        removed.access_token_expires_at = Utc::now() - Duration::seconds(30);
        let _env = TOKEN_URL_ENV.lock().await;
        let body =
            token_response(json!({ "principal_type": "user", "principal_id": "principal-1" }));
        let (url, handle) = one_shot_token_server(200, body).await;
        std::env::set_var(XAI_ENV, &url);
        let bearer = DbCredentialBearer::with_cache(
            node.clone(),
            did,
            XAI_OAUTH_PROVIDER,
            credential_id,
            true,
            Some(removed),
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        );
        let refreshed = bearer.current_bearer().await;
        std::env::remove_var(XAI_ENV);
        let request = handle.await.expect("server");
        refreshed.expect("refresh");
        assert!(request.contains("refresh-TEST"), "{request}");
    }

    /// Rows stored before #2116, with none of the account fields.
    async fn seed_upgraded_row(
        node: &Arc<EmbeddedNode>,
        did: &str,
        provider: &str,
        account_id: &str,
    ) {
        let credential_id = oauth_credential_id(did, provider);
        let mutation = format!(
            r#"mutation {{ create_OAuthCredential(input: {{
                credential_id: "{credential_id}"
                agent_did: "{did}"
                provider: "{provider}"
                access_token: "placeholder-access"
                refresh_token: "placeholder-refresh"
                {account_id}
                is_fedramp: false
                access_token_expires_at: "2020-01-01T00:00:00Z"
                last_refresh: "2019-12-31T00:00:00Z"
                enabled: true
            }}) {{ _docID }} }}"#
        );
        ConfigAccess::write_local(node, "test.seed_upgraded_row", &mutation)
            .await
            .expect("seed upgraded row");
    }

    #[tokio::test]
    async fn upgraded_rows_keep_their_identity_and_fill_the_key_on_first_refresh() {
        let node = Arc::new(test_node().await);
        let did = "did:key:zUpgradedRows";
        let access = ConfigAccess::Local(node.clone());
        seed_upgraded_row(
            &node,
            did,
            CHATGPT_CODEX_PROVIDER,
            r#"account_id: "acct-ws""#,
        )
        .await;
        seed_upgraded_row(&node, did, XAI_OAUTH_PROVIDER, "").await;
        let cases = [
            (
                CHATGPT_CODEX_PROVIDER,
                OAuthRefreshKind::ChatGpt,
                CHATGPT_OAUTH_PRODUCT,
                token_response(json!({ "https://api.openai.com/auth": {
                    "chatgpt_account_id": "acct-ws",
                    "chatgpt_account_user_id": "member-a"
                } })),
                "member-a",
            ),
            (
                XAI_OAUTH_PROVIDER,
                OAuthRefreshKind::Xai,
                XAI_OAUTH_PRODUCT,
                token_response(json!({ "principal_type": "user", "principal_id": "principal-1" })),
                "user:principal-1",
            ),
        ];
        for (provider, kind, product, body, key) in cases {
            let resolve = |pick| resolve_oauth_credential(&access, did, provider, pick);
            let before = resolve(AccountPick::Reference(None))
                .await
                .unwrap()
                .expect("an upgraded row resolves");
            assert_eq!(before.provider_account_key, None);
            let default = resolve(AccountPick::ProviderDefault).await.unwrap();
            assert_eq!(default.as_ref(), Some(&before));
            let (_, bootstrapped) = crate::oauth_http::bootstrap_oauth_client(
                node.clone(),
                did,
                provider,
                kind,
                product,
                AccountPick::ProviderDefault,
            )
            .await
            .expect("an upgraded row builds a client");
            assert_eq!(bootstrapped.credential_id, before.credential_id);
            let credential_id = oauth_credential_id(did, provider);
            let stored = lookup_oauth_credential_by_id(&node, &credential_id)
                .await
                .unwrap()
                .expect("stored row");
            assert_eq!(stored, before, "{provider}");
            assert_eq!(stored.credential_id, credential_id);
            assert_eq!(stored.access_token, "placeholder-access");
            assert_eq!(stored.refresh_token, "placeholder-refresh");
            assert!(stored.enabled);

            let after = refresh_once(&node, did, provider, kind, body).await;
            assert_eq!(
                after.provider_account_key.as_deref(),
                Some(key),
                "{provider}"
            );
            assert_eq!(after.credential_id, before.credential_id);
            assert_eq!(after.doc_id, before.doc_id);
            assert!(after.enabled);
            assert_eq!(after.account_ref, None);
            assert_eq!(after.connected_at, None);
            assert_eq!(after.account_id, before.account_id);
            let resolved = resolve(AccountPick::ProviderDefault).await.unwrap();
            assert_eq!(resolved.as_ref(), Some(&after));
        }
    }
}

#[cfg(test)]
mod cooldown_tests {
    use super::test_support::{
        one_shot_token_server, seed_credential, seed_credential_with_refresh_token, test_node,
        TOKEN_URL_ENV,
    };
    use super::*;

    /// A failed refresh is served from the cooldown on the next call instead
    /// of POSTing the token endpoint again. The one-shot server accepts one
    /// request and is gone before the second call, so a second POST would
    /// surface as a transport error rather than the cached "expired or
    /// revoked" text.
    #[tokio::test]
    async fn failed_refresh_is_not_retried_during_the_cooldown() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkRevoked";
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
        let (url, handle) = one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
        // Process-global; hold TOKEN_URL_ENV while it is set.
        let _env = TOKEN_URL_ENV.lock().await;
        std::env::set_var(
            crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV,
            &url,
        );
        let bearer = DbCredentialBearer::new(
            node,
            did,
            provider,
            oauth_credential_id(did, provider),
            true,
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        );
        let first = bearer.current_bearer().await.expect_err("revoked");
        let request = handle.await.expect("server");
        let second = bearer.current_bearer().await.expect_err("cooldown");
        std::env::remove_var(crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV);

        assert!(request.contains("grant_type=refresh_token"), "{request}");
        assert!(
            first.to_string().contains("is expired or revoked"),
            "{first}"
        );
        assert_eq!(first.to_string(), second.to_string());
    }

    /// After `invalidate()` (a provider 401) a failed refresh must keep the
    /// bearer forced: while the cooldown holds every call returns the
    /// classified error instead of taking the cache fast path and re-serving
    /// the unexpired but rejected access token.
    #[tokio::test]
    async fn failed_refresh_keeps_the_bearer_forced_and_never_reserves_the_bad_token() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkRejected";
        let provider = crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
        seed_credential(&node, did, provider, Utc::now() + Duration::hours(1)).await;
        let (url, handle) = one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
        // Process-global; hold TOKEN_URL_ENV while it is set.
        let _env = TOKEN_URL_ENV.lock().await;
        std::env::set_var(
            gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
            &url,
        );
        let bearer = DbCredentialBearer::new(
            node,
            did,
            provider,
            oauth_credential_id(did, provider),
            true,
            OAuthRefreshKind::ChatGpt,
            CHATGPT_OAUTH_PRODUCT,
        );
        bearer.invalidate().await;
        let first = bearer.current_bearer().await;
        handle.await.expect("server");
        let second = bearer.current_bearer().await;
        std::env::remove_var(gents_protocol::chatgpt_oauth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR);

        let first = first.expect_err("rejected refresh");
        let second = second.expect_err("cooldown must not re-serve the rejected token");
        assert!(
            first.to_string().contains("is expired or revoked"),
            "{first}"
        );
        assert_eq!(first.to_string(), second.to_string());
    }

    fn grok_bearer(node: Arc<EmbeddedNode>, did: &str) -> DbCredentialBearer {
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        DbCredentialBearer::new(
            node,
            did,
            provider,
            oauth_credential_id(did, provider),
            true,
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        )
    }

    async fn set_label(node: &EmbeddedNode, did: &str, label: Option<&str>) {
        let credential_id = oauth_credential_id(did, crate::xai_grok_oauth::XAI_OAUTH_PROVIDER);
        let mutation = format!(
            r#"mutation {{ update_OAuthCredential(filter: {{ credential_id: {{ _eq: "{credential_id}" }} }}, input: {{ {} }}) {{ _docID }} }}"#,
            gents_protocol::graphql::nullable_string_field("label", label),
        );
        crate::config_client::ConfigAccess::write_local(node, "test.label", &mutation)
            .await
            .unwrap();
    }

    /// A refresh against a one-shot 401 `invalid_grant` endpoint, then a
    /// second call inside the cooldown; returns both errors and the request.
    async fn revoked_twice(bearer: &DbCredentialBearer) -> (String, String, String) {
        let (url, handle) = one_shot_token_server(401, r#"{"error":"invalid_grant"}"#).await;
        let _env = TOKEN_URL_ENV.lock().await;
        std::env::set_var(
            crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV,
            &url,
        );
        let first = bearer.current_bearer().await.expect_err("revoked");
        let request = handle.await.expect("server");
        let second = bearer.current_bearer().await.expect_err("cooldown");
        std::env::remove_var(crate::xai_oauth_refresh::XAI_OAUTH_TOKEN_URL_OVERRIDE_ENV);
        (first.to_string(), second.to_string(), request)
    }

    #[tokio::test]
    async fn a_revoked_refresh_names_the_account() {
        for (label, named) in [(Some("label-a"), "label-a"), (None, "Grok")] {
            let node = Arc::new(test_node().await);
            let did = "did:key:z6MkTestRevokedNamed";
            let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
            seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
            set_label(&node, did, label).await;
            let (first, second, _) = revoked_twice(&grok_bearer(node, did)).await;
            for error in [&first, &second] {
                assert!(
                    error.starts_with(&format!(
                        "Grok account \"{named}\" is signed out (expired or revoked)"
                    )),
                    "{error}"
                );
                assert!(error.contains("is expired or revoked"), "{error}");
                assert!(
                    error.contains("gents config profile set-account"),
                    "{error}"
                );
                assert!(
                    !error.contains("SECRET") && !error.contains("IDENTITY"),
                    "{error}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_revoked_refresh_never_uses_another_account() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestRevokedOther";
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
        let mut other = lookup_oauth_credential_by_id(&node, &oauth_credential_id(did, provider))
            .await
            .unwrap()
            .unwrap();
        other.doc_id = None;
        other.credential_id = format!("{}:acct-b", other.credential_id);
        other.account_ref = Some("acct-b".into());
        other.refresh_token = "refresh-B".into();
        other.access_token_expires_at = Utc::now() + Duration::hours(1);
        upsert_oauth_credential(&node, &other).await.unwrap();
        let (_, _, request) = revoked_twice(&grok_bearer(node, did)).await;
        assert!(request.contains("refresh-TEST"), "{request}");
        assert!(!request.contains("refresh-B"), "{request}");
    }

    #[tokio::test]
    async fn a_reused_slot_names_the_new_account() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestReusedSlot";
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        let credential_id = oauth_credential_id(did, provider);
        seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
        set_label(&node, did, Some("label-a")).await;
        let cached = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .unwrap();
        let bearer = DbCredentialBearer::with_cache(
            node.clone(),
            did,
            provider,
            credential_id.clone(),
            true,
            Some(cached),
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        );
        let access = crate::config_client::ConfigAccess::Local(node.clone());
        access
            .transact("test.remove", |txn| {
                let credential_id = credential_id.clone();
                Box::pin(async move { remove_account_in_txn(txn, did, &credential_id).await })
            })
            .await
            .unwrap();
        seed_credential_with_refresh_token(
            &node,
            did,
            provider,
            Utc::now() - Duration::minutes(1),
            "refresh-NEW",
        )
        .await;
        set_label(&node, did, Some("label-new")).await;
        let (first, _, request) = revoked_twice(&bearer).await;
        assert!(request.contains("refresh-NEW"), "{request}");
        assert!(first.contains("\"label-new\""), "{first}");
        assert!(
            !first.contains("label-a") && !first.contains("removed"),
            "{first}"
        );
    }

    #[tokio::test]
    async fn a_disabled_or_removed_row_names_the_account() {
        let node = Arc::new(test_node().await);
        let did = "did:key:z6MkTestDisabledNamed";
        let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
        let credential_id = oauth_credential_id(did, provider);
        seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
        set_label(&node, did, Some("label-a")).await;
        let cached = lookup_oauth_credential_by_id(&node, &credential_id)
            .await
            .unwrap()
            .unwrap();
        let bearer = DbCredentialBearer::with_cache(
            node.clone(),
            did,
            provider,
            credential_id.clone(),
            true,
            Some(cached),
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        );
        let access = crate::config_client::ConfigAccess::Local(node.clone());
        set_account_enabled(&access, did, &credential_id, false)
            .await
            .unwrap();
        let disabled = bearer
            .current_bearer()
            .await
            .expect_err("disabled")
            .to_string();
        assert!(
            disabled.starts_with("Grok account \"label-a\" is disabled"),
            "{disabled}"
        );
        assert!(disabled.contains("set-account"), "{disabled}");

        access
            .transact("test.remove", |txn| {
                let credential_id = credential_id.clone();
                Box::pin(async move { remove_account_in_txn(txn, did, &credential_id).await })
            })
            .await
            .unwrap();
        let removed = bearer
            .current_bearer()
            .await
            .expect_err("removed")
            .to_string();
        assert!(
            removed.starts_with("Grok account \"label-a\" is removed from this node"),
            "{removed}"
        );
        assert!(removed.contains("set-account"), "{removed}");
        assert!(removed.contains("adds a new account"), "{removed}");
        assert!(
            !removed.contains(XAI_OAUTH_PRODUCT.login_command),
            "{removed}"
        );
    }

    #[tokio::test]
    async fn a_limit_like_label_is_not_a_limit() {
        for label in ["usage limit", "maximum context length"] {
            let node = Arc::new(test_node().await);
            let did = "did:key:z6MkTestLimitLabel";
            let provider = crate::xai_grok_oauth::XAI_OAUTH_PROVIDER;
            seed_credential(&node, did, provider, Utc::now() - Duration::minutes(1)).await;
            set_label(&node, did, Some(label)).await;
            let (first, _, _) = revoked_twice(&grok_bearer(node, did)).await;
            assert!(first.starts_with("Grok account \"Grok\""), "{first}");
            assert!(!first.contains(label), "{first}");
            assert!(
                gents_loop::provider_limit::classify_provider_limit(&first, Utc::now()).is_none(),
                "{first}"
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use chrono::{DateTime, Utc};
    use defra_node::EmbeddedNode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serializes lib tests that set a process-global token-URL override.
    pub(crate) static TOKEN_URL_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// In-memory node with the gents schemas loaded.
    pub(crate) async fn test_node() -> EmbeddedNode {
        let node = EmbeddedNode::builder()
            .build()
            .await
            .expect("embedded node");
        crate::schema::ensure_runtime_schemas(&node)
            .await
            .expect("schemas");
        node
    }

    /// The bearer a client build bound for `credential_id`, without binding one.
    pub(crate) fn bound_bearer(
        credential_id: &str,
    ) -> Option<std::sync::Arc<super::DbCredentialBearer>> {
        super::bearer_registry()
            .lock()
            .expect("bearer registry mutex poisoned")
            .get(credential_id)
            .cloned()
    }

    pub(crate) async fn seed_credential(
        node: &EmbeddedNode,
        agent_did: &str,
        provider: &str,
        expires_at: DateTime<Utc>,
    ) {
        seed_credential_with_refresh_token(node, agent_did, provider, expires_at, "refresh-TEST")
            .await;
    }

    pub(crate) async fn seed_credential_with_refresh_token(
        node: &EmbeddedNode,
        agent_did: &str,
        provider: &str,
        expires_at: DateTime<Utc>,
        refresh_token: &str,
    ) {
        let credential = crate::oauth_credential::OAuthCredential {
            doc_id: None,
            credential_id: crate::oauth_credential::oauth_credential_id(agent_did, provider),
            agent_did: agent_did.to_string(),
            provider: provider.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: refresh_token.into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: expires_at,
            last_refresh: Some(Utc::now()),
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        crate::oauth_credential::upsert_oauth_credential(node, &credential)
            .await
            .expect("seed credential");
    }

    /// A JWT with an `alg: none` header. The signature segment stays non-empty:
    /// `jwt_payload` decodes nothing from a token whose signature is empty.
    pub(crate) fn unsigned_jwt(payload: serde_json::Value) -> String {
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        format!("{header}.{payload}.sig")
    }

    /// Accepts exactly one HTTP request, returns its body, and answers with `status` + `body`.
    pub(crate) async fn one_shot_token_server(
        status: u16,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        gated_token_server(status, body, None).await
    }

    /// [`one_shot_token_server`] that, with a gate, signals once it has read
    /// the request and answers only after the gate's release fires.
    pub(crate) async fn gated_token_server(
        status: u16,
        body: &'static str,
        gate: Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/v1/oauth/token", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.expect("read");
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(idx) = text.find("\r\n\r\n") {
                    let headers = &text[..idx];
                    let content_length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.trim().parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= idx + 4 + content_length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buf).into_owned();
            if let Some((received, release)) = gate {
                received.send(()).ok();
                release.await.ok();
            }
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.expect("write");
            socket.shutdown().await.ok();
            request
        });
        (url, handle)
    }
}
