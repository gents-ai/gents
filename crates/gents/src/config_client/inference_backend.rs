use super::{ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan};
use crate::inference::auth::credential::ServingAccount;
use crate::{Collection, InferenceBackend};
use anyhow::{Context, Result};
use std::collections::BTreeMap;

pub async fn load_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    backend_id: &str,
) -> Result<Option<InferenceBackend>> {
    super::desired_state::read::read_record(txn, Collection::InferenceBackend, node_did, backend_id)
        .await?
        .map(|(_, value)| serde_json::from_value(value).context("decoding scoped InferenceBackend"))
        .transpose()
}

pub async fn list_inference_backends_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
) -> Result<Vec<InferenceBackend>> {
    let fields = super::config_projection(Collection::InferenceBackend, None)?
        .0
        .join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceBackend(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            crate::graphql::escape_graphql_string(node_did)
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackend")
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding scoped InferenceBackend"))
        .collect()
}

pub(crate) async fn write_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
) -> Result<()> {
    backend.validate()?;
    let value = serde_json::to_value(backend)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_desired_state_plan(txn, &plan).await?;
    Ok(())
}

pub async fn write_inference_backend_document(
    access: &ConfigAccess,
    backend: &InferenceBackend,
) -> Result<String> {
    backend.validate()?;
    access
        .transact_write("config.inference_backend.upsert", |txn| {
            Box::pin(async move {
                write_inference_backend_in_txn(txn, backend).await?;
                super::desired_state::read::read_record(
                    txn,
                    Collection::InferenceBackend,
                    &backend.node_did,
                    &backend.backend_id,
                )
                .await?
                .map(|(id, _)| id)
                .context("replaced InferenceBackend missing")
            })
        })
        .await
}

pub async fn serving_accounts(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<BTreeMap<String, ServingAccount>> {
    let accounts = crate::inference::auth::credential::list_accounts(access, node_did).await?;
    let backends = access
        .transact_readonly("config.serving_accounts", |txn| {
            Box::pin(async move { list_inference_backends_in_txn(txn, node_did).await })
        })
        .await?;
    Ok(backends
        .iter()
        .map(|backend| {
            (
                backend.backend_id.clone(),
                crate::inference::auth::credential::serving_account(backend, &accounts),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::BackendAuth;
    use crate::inference::auth::credential::{set_account_enabled, store_sign_in, AccountState};
    use serde_json::json;
    use std::sync::Arc;

    fn claude_sign_in(did: &str, who: &str) -> crate::inference::auth::credential::OAuthCredential {
        crate::inference::auth::claude::credential_from_login_tokens(
            did,
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::inference::auth::claude::ClaudeLoginTokens {
                access_token: "access-SECRET".into(),
                refresh_token: format!("refresh-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some("IDENTITY".into()),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        )
    }

    fn backend(
        did: &str,
        backend_id: &str,
        provider_kind: &str,
        auth: BackendAuth,
    ) -> InferenceBackend {
        let endpoint = if provider_kind == "ClaudeCliSubscription" {
            "claude-cli://subscription"
        } else {
            "http://127.0.0.1:1/v1"
        };
        serde_json::from_value(json!({
            "node_did": did, "backend_id": backend_id, "name": format!("name-{backend_id}"),
            "provider_kind": provider_kind, "endpoint": endpoint, "auth": auth,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn serving_accounts_reads_backends_and_accounts() -> Result<()> {
        let did = "did:key:z6MkTestServing";
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local(node);
        let oauth = |account_ref: Option<&str>| BackendAuth::NodeOAuth {
            account_ref: account_ref.map(str::to_owned),
        };
        for backend in [
            backend(did, "claude", "ClaudeCliSubscription", oauth(None)),
            backend(
                did,
                "gone",
                "ClaudeCliSubscription",
                oauth(Some("acct-other")),
            ),
            backend(
                did,
                "openai",
                "OpenAiCompatible",
                BackendAuth::ApiKey {
                    key: "key-SECRET".into(),
                },
            ),
        ] {
            write_inference_backend_document(&access, &backend).await?;
        }
        store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, claude_sign_in(did, "b"), Some("label-b")).await?;
        set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
        let b_backend = format!(
            "{}-{}",
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );

        let serving = serving_accounts(&access, did).await?;
        let expected = |label: &str, state| ServingAccount {
            label: label.into(),
            state,
            provider: (label != "name-openai")
                .then_some(crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER),
        };
        assert_eq!(
            serving,
            BTreeMap::from([
                (
                    "claude".to_string(),
                    expected("Claude", AccountState::Enabled)
                ),
                (b_backend, expected("label-b", AccountState::Disabled)),
                (
                    "gone".to_string(),
                    expected("name-gone", AccountState::Missing)
                ),
                (
                    "openai".to_string(),
                    expected("name-openai", AccountState::Enabled)
                ),
            ])
        );
        Ok(())
    }
}
