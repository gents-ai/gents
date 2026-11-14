use std::collections::BTreeMap;

use anyhow::{Context, Result};

use super::ServingAccount;
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

pub async fn serving_accounts(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<BTreeMap<String, ServingAccount>> {
    let accounts = super::list_accounts(access, node_did).await?;
    let backends = access
        .transact_readonly("config.serving_accounts", |txn| {
            Box::pin(
                async move { gents_documents::list_inference_backends_in_txn(txn, node_did).await },
            )
        })
        .await?;
    Ok(backends
        .iter()
        .map(|backend| {
            (
                backend.backend_id.clone(),
                super::serving_account(backend, &accounts),
            )
        })
        .collect())
}

pub async fn agent_accounts(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<Vec<(String, ServingAccount)>> {
    let profile_backends = access
        .transact_readonly("config.agent_accounts", |txn| {
            Box::pin(async move {
                let agent: gents_documents::Agent =
                    read(txn, Collection::Agent, node_did, agent_did)
                        .await?
                        .with_context(|| format!("agent {agent_did:?} not found"))?;
                let mut profile_ids = vec![agent.inference_profile_id];
                let context: Option<gents_documents::AgentContext> =
                    match agent.context_id.as_deref() {
                        Some(id) => read(txn, Collection::AgentContext, node_did, id).await?,
                        None => None,
                    };
                let compaction: Option<gents_documents::CompactionConfig> =
                    match context.and_then(|context| context.compaction_id) {
                        Some(id) => read(txn, Collection::Compaction, node_did, &id).await?,
                        None => None,
                    };
                if let Some(id) = compaction.and_then(|compaction| compaction.inference_profile_id)
                {
                    if !profile_ids.contains(&id) {
                        profile_ids.push(id);
                    }
                }
                let mut profile_backends = Vec::new();
                for profile_id in &profile_ids {
                    let profile: Option<gents_documents::InferenceProfile> =
                        read(txn, Collection::InferenceProfile, node_did, profile_id).await?;
                    if let Some(profile) = profile {
                        profile_backends.push((profile_id.clone(), profile.backend_id));
                    }
                }
                Ok::<_, anyhow::Error>(profile_backends)
            })
        })
        .await?;
    let serving = serving_accounts(access, node_did).await?;
    Ok(profile_backends
        .into_iter()
        .filter_map(|(profile_id, backend_id)| {
            serving
                .get(&backend_id)
                .map(|account| (profile_id, account.clone()))
        })
        .collect())
}

async fn read<T>(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    node_did: &str,
    id: &str,
) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    gents_documents::read_desired_state_record_in_txn(txn, collection, node_did, id)
        .await?
        .map(|(_, value)| {
            serde_json::from_value(value).with_context(|| format!("decoding scoped {collection:?}"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{set_account_enabled, store_sign_in, AccountState};
    use gents_documents::BackendAuth;
    use gents_documents::InferenceBackend;
    use gents_store::identity::NodeIdentity;
    use serde_json::json;

    fn serving_sign_in(did: &str, who: &str) -> crate::auth::credential::OAuthCredential {
        crate::auth::claude::credential_from_login_tokens(
            did,
            crate::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::auth::claude::ClaudeLoginTokens {
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
        // Only a User changes an account, so the fixture writes through the
        // enrolled User's client access on a protected node.
        let (db, _user, access) = crate::test_support::signed_test_node_without_agent(
            "serving-accounts",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let did = db.node_identity.did();
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
            gents_documents::write_inference_backend_document(
                &access,
                &backend,
                &crate::resolved::ProviderProfilePolicy,
            )
            .await?;
        }
        store_sign_in(&access, serving_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, serving_sign_in(did, "b"), Some("label-b")).await?;
        set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
        let b_backend = format!(
            "{}-{}",
            crate::auth::claude::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );

        let serving = serving_accounts(&access, did).await?;
        let expected = |label: &str, state| ServingAccount {
            label: label.into(),
            state,
            provider: (label != "name-openai")
                .then_some(crate::auth::claude::CLAUDE_OAUTH_PROVIDER),
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

    fn claude_sign_in(did: &str, who: &str) -> crate::auth::credential::OAuthCredential {
        crate::auth::claude::credential_from_login_tokens(
            did,
            crate::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::auth::claude::ClaudeLoginTokens {
                access_token: "access-TEST".into(),
                refresh_token: format!("refresh-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("label-{who}")),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        )
    }

    #[tokio::test]
    async fn agent_accounts_follow_profile_and_compaction() -> Result<()> {
        use crate::auth::credential::{set_account_enabled, store_sign_in, AccountState};
        // Only a User changes an account, so the fixture writes through the
        // enrolled User's client access on a protected node.
        let (db, user, access) = crate::test_support::signed_test_node_without_agent(
            "agent-accounts",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let did = db.node_identity.did();
        let spec = crate::backends::setup::connection_spec(
            crate::backends::setup::InferenceProviderId::Anthropic,
            crate::backends::setup::InferenceAuthMethod::ClaudeOauth,
            "",
        )?;
        let original = json!({
            "node_did": did, "backend_id": "claude", "name": "Claude",
            "provider_kind": spec.provider_kind, "endpoint": spec.endpoint,
            "auth": {"kind": "node_oauth"},
        });
        gents_documents::write_inference_backend_document(
            &access,
            &serde_json::from_value(original)?,
            &crate::resolved::ProviderProfilePolicy,
        )
        .await?;
        store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, claude_sign_in(did, "b"), Some("label-b")).await?;
        let b_backend = format!(
            "{}-{}",
            crate::auth::claude::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );
        let user_did = identity::Did::new(user.did())?;
        let x = gents_store::identity::provision_agent_identity_authorized(
            &db.node,
            db.node_identity.as_ref(),
            &user_did,
        )
        .await?;
        let y = gents_store::identity::provision_agent_identity_authorized(
            &db.node,
            db.node_identity.as_ref(),
            &user_did,
        )
        .await?;
        let documents = [
            (Collection::InferenceProfile, json!({"node_did": did, "profile_id": "profile-a", "backend_id": "claude", "model_name": "model-x"})),
            (Collection::InferenceProfile, json!({"node_did": did, "profile_id": "profile-b", "backend_id": b_backend, "model_name": "model-x"})),
            (Collection::Compaction, json!({"node_did": did, "compaction_id": "compaction-a", "inference_profile_id": "profile-a"})),
            (Collection::AgentContext, json!({"node_did": did, "context_id": "context-x", "compaction_id": "compaction-a"})),
            (Collection::Agent, json!({"node_did": did, "agent_did": x.agent_did, "creation_nonce": x.creation_nonce, "context_id": "context-x", "inference_profile_id": "profile-b"})),
            (Collection::Agent, json!({"node_did": did, "agent_did": y.agent_did, "creation_nonce": y.creation_nonce, "inference_profile_id": "profile-a"})),
        ]
        .into_iter()
        .map(|(collection, value)| gents_documents::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
        let plan = gents_documents::DesiredStateApplyPlan::new(documents)?;
        access
            .transact_write("test.agent_accounts", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &crate::resolved::ProviderProfilePolicy,
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await?;
        let expected = |profile: &str, label: &str, state| {
            (
                profile.to_string(),
                ServingAccount {
                    label: label.into(),
                    state,
                    provider: Some(crate::auth::claude::CLAUDE_OAUTH_PROVIDER),
                },
            )
        };

        assert_eq!(
            agent_accounts(&access, did, &x.agent_did).await?,
            [
                expected("profile-b", "label-b", AccountState::Enabled),
                expected("profile-a", "Claude", AccountState::Enabled),
            ]
        );
        set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
        assert_eq!(
            agent_accounts(&access, did, &x.agent_did).await?[0],
            expected("profile-b", "label-b", AccountState::Disabled)
        );
        assert_eq!(
            agent_accounts(&access, did, &y.agent_did).await?,
            [expected("profile-a", "Claude", AccountState::Enabled)]
        );
        assert!(agent_accounts(&access, did, "agent-unknown").await.is_err());
        Ok(())
    }
}
