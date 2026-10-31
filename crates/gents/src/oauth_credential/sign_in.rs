use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use super::*;

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

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

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
        .transact_write("oauth_credential.sign_in", |txn| {
            let credential = credential.clone();
            let label = label.clone();
            Box::pin(async move { store_sign_in_in_txn(txn, credential, label).await })
        })
        .await
}

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

async fn original_account_profiles(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    credential: &OAuthCredential,
) -> Result<Vec<String>> {
    use crate::backend_provider::BackendProviderOauthExt;
    let backends: Vec<_> =
        crate::config_client::list_inference_backends_in_txn(txn, &credential.node_did)
            .await?
            .into_iter()
            .filter(|backend| {
                matches!(
                    backend.auth,
                    crate::document_config::BackendAuth::NodeOAuth { account_ref: None }
                ) && backend.provider_kind.oauth_provider() == Some(credential.provider.as_str())
            })
            .map(|backend| backend.backend_id)
            .collect();
    Ok(
        crate::config_client::list_inference_profiles_in_txn(txn, &credential.node_did)
            .await?
            .into_iter()
            .filter(|profile| backends.contains(&profile.backend_id))
            .map(|profile| profile.profile_id)
            .collect(),
    )
}
