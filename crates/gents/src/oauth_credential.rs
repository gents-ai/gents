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
pub use resolve::{pick_oauth_credential, resolve_oauth_credential, AccountPick};
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
