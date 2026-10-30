use anyhow::Result;

use super::*;

/// Display metadata only. Decoded, unverified claims must not authorize requests or replace
/// `account_id` used by provider authentication headers.
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

pub(super) fn sign_in_product(provider: &str) -> Option<OAuthProduct> {
    match provider {
        crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER => Some(CHATGPT_OAUTH_PRODUCT),
        crate::claude_oauth::CLAUDE_OAUTH_PROVIDER => {
            Some(crate::claude_oauth::CLAUDE_OAUTH_PRODUCT)
        }
        crate::xai_grok_oauth::XAI_OAUTH_PROVIDER => Some(XAI_OAUTH_PRODUCT),
        _ => None,
    }
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

pub(super) fn ensure_label_free(others: &[&OAuthCredential], label: &str) -> Result<()> {
    anyhow::ensure!(
        !others
            .iter()
            .any(|row| effective_account_label(row) == label),
        "the label {label:?} is already used by another account of this provider"
    );
    Ok(())
}

pub(super) fn next_free_label(rows: &[OAuthCredential], provider: &str) -> String {
    let name = sign_in_product(provider).map_or(provider, |product| product.name);
    let taken: rapidhash::RapidHashSet<_> = rows.iter().map(effective_account_label).collect();
    std::iter::once(name.to_owned())
        .chain((2..).map(|n| format!("{name} {n}")))
        .find(|label| !taken.contains(label))
        .expect("an unbounded sequence has a free label")
}

pub fn provider_name(provider: &str) -> &str {
    sign_in_product(provider).map_or(provider, |product| product.name)
}
