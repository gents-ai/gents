use std::collections::BTreeMap;

use anyhow::{Context, Result};

use super::ServingAccount;
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::Collection;

pub async fn serving_accounts(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<BTreeMap<String, ServingAccount>> {
    let accounts = super::list_accounts(access, node_did).await?;
    let backends = access
        .transact_readonly("config.serving_accounts", |txn| {
            Box::pin(async move {
                crate::store::config_client::list_inference_backends_in_txn(txn, node_did).await
            })
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
                let agent: crate::documents::Agent =
                    read(txn, Collection::Agent, node_did, agent_did)
                        .await?
                        .with_context(|| format!("agent {agent_did:?} not found"))?;
                let mut profile_ids = vec![agent.inference_profile_id];
                let context: Option<crate::documents::AgentContext> =
                    match agent.context_id.as_deref() {
                        Some(id) => read(txn, Collection::AgentContext, node_did, id).await?,
                        None => None,
                    };
                let compaction: Option<crate::documents::CompactionConfig> =
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
                    let profile: Option<crate::documents::InferenceProfile> =
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
    crate::store::config_client::read_desired_state_record_in_txn(txn, collection, node_did, id)
        .await?
        .map(|(_, value)| {
            serde_json::from_value(value).with_context(|| format!("decoding scoped {collection:?}"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::BackendAuth;
    use crate::inference::auth::credential::{set_account_enabled, store_sign_in, AccountState};
    use crate::InferenceBackend;
    use serde_json::json;
    use std::sync::Arc;

    fn serving_sign_in(
        did: &str,
        who: &str,
    ) -> crate::inference::auth::credential::OAuthCredential {
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
        let access = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(
                node.node_identity_did()
                    .expect("fixture node has an identity"),
            )
            .unwrap(),
        );
        crate::test_support::bootstrap_signed_test_node(&access, did).await;
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
            crate::documents::write_inference_backend_document(&access, &backend).await?;
        }
        store_sign_in(&access, serving_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, serving_sign_in(did, "b"), Some("label-b")).await?;
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

    fn claude_sign_in(did: &str, who: &str) -> crate::inference::auth::credential::OAuthCredential {
        crate::inference::auth::claude::credential_from_login_tokens(
            did,
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &crate::inference::auth::claude::ClaudeLoginTokens {
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
    async fn account_events_never_repoint_a_profile() -> Result<()> {
        use crate::inference::auth::credential::{
            remove_account_in_txn, resolve_oauth_credential, set_account_enabled, store_sign_in,
            AccountPick,
        };
        let did = "did:key:z6MkTestProfileEvents";
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(
                node.node_identity_did()
                    .expect("fixture node has an identity"),
            )
            .unwrap(),
        );
        crate::test_support::bootstrap_signed_test_node(&access, did).await;
        let spec = crate::inference::backends::setup::connection_spec(
            crate::inference::backends::setup::InferenceProviderId::Anthropic,
            crate::inference::backends::setup::InferenceAuthMethod::ClaudeOauth,
            "",
        )?;
        let original = crate::InferenceBackend {
            physical_doc_id: None,
            node_did: did.to_owned(),
            backend_id: "claude".into(),
            name: "Claude".into(),
            provider_kind: spec.provider_kind,
            openai_wire_api: spec.openai_wire_api,
            endpoint: spec.endpoint,
            claude_platform: None,
            auth: crate::documents::BackendAuth::NodeOAuth { account_ref: None },
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: true,
            tags: Vec::new(),
        };
        crate::documents::write_inference_backend_document(&access, &original).await?;
        let a = store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, claude_sign_in(did, "b"), None).await?;
        let b_backend = format!(
            "{}-{}",
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );
        for (profile_id, backend_id) in [
            ("on-a", "claude"),
            ("on-b", b_backend.as_str()),
            ("compaction", b_backend.as_str()),
        ] {
            let profile: crate::documents::InferenceProfile = serde_json::from_value(json!({
                "node_did": did, "profile_id": profile_id, "backend_id": backend_id,
                "model_name": "model-x",
            }))?;
            crate::documents::write_inference_profile_document(&access, &profile).await?;
        }
        let profiles = || async {
            access
                .transact_write("test.profiles", |txn| {
                    Box::pin(async move {
                        let mut records = Vec::new();
                        for id in ["on-a", "on-b", "compaction"] {
                            records.push(
                                crate::store::config_client::read_desired_state_record_in_txn(
                                    txn,
                                    Collection::InferenceProfile,
                                    did,
                                    id,
                                )
                                .await?
                                .expect("profile"),
                            );
                        }
                        Ok::<_, anyhow::Error>(records)
                    })
                })
                .await
                .unwrap()
        };
        let before = profiles().await;
        let remove = |credential_id: String| {
            let access = &access;
            async move {
                access
                    .transact_write("test.remove", |txn| {
                        let credential_id = credential_id.clone();
                        Box::pin(
                            async move { remove_account_in_txn(txn, did, &credential_id).await },
                        )
                    })
                    .await
                    .unwrap()
            }
        };

        store_sign_in(&access, claude_sign_in(did, "c"), None).await?;
        assert_eq!(profiles().await, before, "add C");
        store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        assert_eq!(profiles().await, before, "refresh A");
        store_sign_in(&access, claude_sign_in(did, "b"), Some("label-b2")).await?;
        assert_eq!(profiles().await, before, "relabel B");
        set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
        assert_eq!(profiles().await, before, "disable B");
        remove(a.credential.credential_id.clone()).await;
        assert_eq!(profiles().await, before, "remove A");
        let d = store_sign_in(&access, claude_sign_in(did, "d"), None).await?;
        assert!(
            d.credential.account_ref.is_some(),
            "B remains, so D is added"
        );
        assert_eq!(profiles().await, before, "sign in D");

        for account in crate::inference::auth::credential::list_accounts(&access, did).await? {
            remove(account.credential_id).await;
        }
        let reused = store_sign_in(&access, claude_sign_in(did, "e"), None).await?;
        assert_eq!(reused.credential.account_ref, None, "the original slot");
        assert_eq!(reused.profiles, ["on-a"]);
        assert_eq!(profiles().await, before, "remove all, then sign in");
        let resolved = resolve_oauth_credential(
            &access,
            did,
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            AccountPick::Reference(None),
        )
        .await?
        .expect("the original slot resolves");
        assert_eq!(resolved.doc_id.as_deref(), Some(reused.doc_id.as_str()));

        let backends = access
            .transact_write("test.backends", |txn| {
                Box::pin(async move {
                    crate::store::config_client::list_inference_backends_in_txn(txn, did).await
                })
            })
            .await?;
        for backend in &backends {
            crate::inference::backends::registry::record_discovered_catalog_on(
                &access,
                backend,
                Vec::new(),
            )
            .await?;
        }
        assert_eq!(profiles().await, before, "catalog refresh");
        let ConfigAccess::Local(local) = &access else {
            unreachable!()
        };
        let node = local.node();
        crate::runtime::agent::document_view::load_document_runtime_view(node, did).await?;
        assert_eq!(profiles().await, before, "view load");
        Ok(())
    }

    #[tokio::test]
    async fn agent_accounts_follow_profile_and_compaction() -> Result<()> {
        use crate::inference::auth::credential::{
            set_account_enabled, store_sign_in, AccountState,
        };
        let did = "did:key:z6MkTestAgentAccounts";
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(
                node.node_identity_did()
                    .expect("fixture node has an identity"),
            )
            .unwrap(),
        );
        crate::test_support::bootstrap_signed_test_node(&access, did).await;
        let spec = crate::inference::backends::setup::connection_spec(
            crate::inference::backends::setup::InferenceProviderId::Anthropic,
            crate::inference::backends::setup::InferenceAuthMethod::ClaudeOauth,
            "",
        )?;
        let original = json!({
            "node_did": did, "backend_id": "claude", "name": "Claude",
            "provider_kind": spec.provider_kind, "endpoint": spec.endpoint,
            "auth": {"kind": "node_oauth"},
        });
        crate::documents::write_inference_backend_document(
            &access,
            &serde_json::from_value(original)?,
        )
        .await?;
        store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, claude_sign_in(did, "b"), Some("label-b")).await?;
        let b_backend = format!(
            "{}-{}",
            crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );
        let documents = [
            (Collection::InferenceProfile, json!({"node_did": did, "profile_id": "profile-a", "backend_id": "claude", "model_name": "model-x"})),
            (Collection::InferenceProfile, json!({"node_did": did, "profile_id": "profile-b", "backend_id": b_backend, "model_name": "model-x"})),
            (Collection::Compaction, json!({"node_did": did, "compaction_id": "compaction-a", "inference_profile_id": "profile-a"})),
            (Collection::AgentContext, json!({"node_did": did, "context_id": "context-x", "compaction_id": "compaction-a"})),
            (Collection::Agent, json!({"node_did": did, "agent_did": "did:key:z6MkTestAgentX", "creation_nonce": "nonce-x", "context_id": "context-x", "inference_profile_id": "profile-b"})),
            (Collection::Agent, json!({"node_did": did, "agent_did": "did:key:z6MkTestAgentY", "creation_nonce": "nonce-y", "inference_profile_id": "profile-a"})),
        ]
        .into_iter()
        .map(|(collection, value)| crate::store::config_client::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
        let plan = crate::store::config_client::DesiredStateApplyPlan::new(documents)?;
        access
            .transact_write("test.agent_accounts", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    crate::documents::apply_desired_state_plan(txn, plan)
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
                    provider: Some(crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER),
                },
            )
        };

        assert_eq!(
            agent_accounts(&access, did, "did:key:z6MkTestAgentX").await?,
            [
                expected("profile-b", "label-b", AccountState::Enabled),
                expected("profile-a", "Claude", AccountState::Enabled),
            ]
        );
        set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
        assert_eq!(
            agent_accounts(&access, did, "did:key:z6MkTestAgentX").await?[0],
            expected("profile-b", "label-b", AccountState::Disabled)
        );
        assert_eq!(
            agent_accounts(&access, did, "did:key:z6MkTestAgentY").await?,
            [expected("profile-a", "Claude", AccountState::Enabled)]
        );
        assert!(agent_accounts(&access, did, "agent-unknown").await.is_err());
        Ok(())
    }
}
