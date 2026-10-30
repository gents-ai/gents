use chrono::Utc;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthAuthProblem {
    Missing,
    WrongMode { found_mode: String },
    Expired,
    NotEntitled,
    Other(String),
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct OAuthCredentialAuthError {
    pub(crate) problem: OAuthAuthProblem,
    pub(crate) message: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OAuthCredentialError {
    #[error(transparent)]
    Auth(#[from] OAuthCredentialAuthError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub fn classify_oauth_auth_error(
    product: &OAuthProduct,
    node_did: &str,
    provider: &str,
    problem: &OAuthAuthProblem,
) -> String {
    match problem {
        OAuthAuthProblem::Missing => format!(
            "No OAuthCredential document found for node {node_did} and provider {provider}.\n\
             To use the {backend}, run \
             `gents {login} --node-did {node_did}`.",
            backend = product.backend_label,
            login = product.login_command,
        ),
        OAuthAuthProblem::WrongMode { found_mode } => format!(
            "OAuthCredential for node {node_did} and provider {provider} is {found_mode}, \
             but the {backend} needs an enabled {name} OAuth credential.\n\
             Run `gents {login} --node-did {node_did}` or select an API-key backend.",
            backend = product.backend_label,
            name = product.name,
            login = product.login_command,
        ),
        OAuthAuthProblem::Expired => format!(
            "{name} OAuth credential for node {node_did} and provider {provider} is expired or revoked.\n\
             Re-authenticate with `gents {login} --node-did {node_did}`.",
            name = product.name,
            login = product.login_command,
        ),
        OAuthAuthProblem::NotEntitled => format!(
            "{name} OAuth credential for node {node_did} and provider {provider} is valid, \
             but this account is not entitled to subscription OAuth inference (HTTP 403 tier gate).\n\
             Re-login will not fix this. {guidance}",
            name = product.name,
            guidance = product.not_entitled_guidance,
        ),
        OAuthAuthProblem::Other(detail) => {
            format!(
                "{name} OAuth credential for node {node_did} and provider {provider} could not be used: {detail}",
                name = product.name,
            )
        }
    }
}

pub fn classify_chatgpt_auth_error(
    node_did: &str,
    provider: &str,
    problem: &OAuthAuthProblem,
) -> String {
    classify_oauth_auth_error(&CHATGPT_OAUTH_PRODUCT, node_did, provider, problem)
}

pub(super) const MOVE_OFF_ACCOUNT: &str =
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
        crate::claude_oauth::CLAUDE_OAUTH_PRODUCT,
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
