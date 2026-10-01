use anyhow::{Context, Result};

use crate::collection::Collection;
use crate::document_config::InferenceProfile;
use crate::oauth_credential::ServingAccount;

use super::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};

/// Every profile of `agent_did` in this transaction's snapshot.
pub async fn list_inference_profiles_in_txn(
    txn: &super::ConfigApplyTxn<'_>,
    agent_did: &str,
) -> Result<Vec<InferenceProfile>> {
    let fields = super::config_projection(Collection::InferenceProfile, None)?
        .0
        .join(" ");
    let response = txn
        .execute(&format!(
            r#"{{ InferenceProfile(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
            crate::graphql::escape_graphql_string(agent_did)
        ))
        .await?;
    gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceProfile")
        .into_iter()
        .map(|row| serde_json::from_value(row).context("decoding scoped InferenceProfile"))
        .collect()
}

/// Replace a complete inference profile through the common configuration writer.
/// Sampling and execution settings are references, never a second set of profile
/// fields. Explicit sparse patches remain owned by the patch API.
pub async fn write_inference_profile_document(
    access: &ConfigAccess,
    profile: &InferenceProfile,
) -> Result<String> {
    profile.validate()?;
    access
        .transact("config.inference_profile.write", |txn| {
            Box::pin(async move {
                let value = serde_json::to_value(profile)?;
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: Collection::InferenceProfile,
                    add: value.clone(),
                    update: value,
                }])?;
                super::apply_desired_state_plan(txn, &plan).await?;
                super::desired_state::read_record(
                    txn,
                    Collection::InferenceProfile,
                    &profile.agent_did,
                    &profile.profile_id,
                )
                .await?
                .map(|(doc_id, _)| doc_id)
                .context("replaced InferenceProfile missing")
            })
        })
        .await
}

/// The accounts a behavior's turns need: its profile's, then its context's
/// compaction profile's, the chain readiness walks. A profile or backend
/// that is not stored is left out.
pub async fn behavior_accounts(
    access: &ConfigAccess,
    agent_did: &str,
    behavior_id: &str,
) -> Result<Vec<(String, ServingAccount)>> {
    let profile_backends = access
        .transact("config.behavior_accounts", |txn| {
            Box::pin(async move {
                let behavior: crate::document_config::AgentBehavior =
                    read(txn, Collection::AgentBehavior, agent_did, behavior_id)
                        .await?
                        .with_context(|| format!("behavior {behavior_id:?} not found"))?;
                let mut profile_ids = vec![behavior.inference_profile_id];
                let context: Option<crate::document_config::AgentContext> =
                    match behavior.context_id.as_deref() {
                        Some(id) => read(txn, Collection::AgentContext, agent_did, id).await?,
                        None => None,
                    };
                let compaction: Option<crate::document_config::CompactionConfig> =
                    match context.and_then(|context| context.compaction_id) {
                        Some(id) => read(txn, Collection::Compaction, agent_did, &id).await?,
                        None => None,
                    };
                if let Some(id) = compaction.and_then(|compaction| compaction.inference_profile_id)
                {
                    if !profile_ids.contains(&id) {
                        profile_ids.push(id);
                    }
                }
                let mut profile_backends = Vec::new();
                for profile_id in profile_ids {
                    let profile: Option<InferenceProfile> =
                        read(txn, Collection::InferenceProfile, agent_did, &profile_id).await?;
                    if let Some(profile) = profile {
                        profile_backends.push((profile_id, profile.backend_id));
                    }
                }
                Ok(profile_backends)
            })
        })
        .await?;
    let serving = super::serving_accounts(access, agent_did).await?;
    Ok(profile_backends
        .into_iter()
        .filter_map(|(profile_id, backend_id)| {
            serving
                .get(&backend_id)
                .map(|account| (profile_id, account.clone()))
        })
        .collect())
}

async fn read<T: serde::de::DeserializeOwned>(
    txn: &super::ConfigApplyTxn<'_>,
    collection: Collection,
    agent_did: &str,
    id: &str,
) -> Result<Option<T>> {
    super::desired_state::read_record(txn, collection, agent_did, id)
        .await?
        .map(|(_, value)| {
            serde_json::from_value(value).with_context(|| format!("decoding scoped {collection:?}"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use defra_node::EmbeddedNode;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn replacement_preserves_other_owner_and_clears_optional_selection() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node);
        for owner in ["did:key:owner", "did:key:other"] {
            let backend = json!({"agent_did":owner,"backend_id":"local","name":"Local",
                "provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:8000/v1",
                "auth":{"kind":"unauthenticated"}});
            let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                collection: Collection::InferenceBackend,
                add: backend.clone(),
                update: backend,
            }])?;
            access
                .transact("test.profile.seed", |txn| {
                    let plan = &plan;
                    Box::pin(async move {
                        super::super::apply_desired_state_plan(txn, plan)
                            .await
                            .map(|_| ())
                    })
                })
                .await?;
        }
        let mut initial: InferenceProfile = serde_json::from_value(json!({
            "agent_did":"did:key:owner","profile_id":"same","backend_id":"local",
            "model_name":"exact-model","reasoning_effort":"high","max_output_tokens":1234,"tags":["old"]
        }))?;
        let first_id = write_inference_profile_document(&access, &initial).await?;
        let mut foreign = initial.clone();
        foreign.agent_did = "did:key:other".to_owned();
        assert_ne!(
            write_inference_profile_document(&access, &foreign).await?,
            first_id
        );
        initial.reasoning_effort = None;
        initial.max_output_tokens = None;
        initial.tags.clear();
        assert_eq!(
            write_inference_profile_document(&access, &initial).await?,
            first_id
        );
        access
            .transact("test.profile.read", |txn| {
                let initial = &initial;
                let foreign = &foreign;
                Box::pin(async move {
                    for expected in [initial, foreign] {
                        let value = super::super::read_desired_state_document_in_txn(
                            txn,
                            Collection::InferenceProfile,
                            &expected.agent_did,
                            &expected.profile_id,
                        )
                        .await?
                        .unwrap();
                        let actual: InferenceProfile = serde_json::from_value(value)?;
                        assert_eq!(&actual, expected);
                    }
                    Ok(())
                })
            })
            .await?;
        Ok(())
    }

    fn claude_sign_in(did: &str, who: &str) -> crate::oauth_credential::OAuthCredential {
        crate::claude_oauth::credential_from_login_tokens(
            did,
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            &crate::claude_oauth::ClaudeLoginTokens {
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
        use crate::oauth_credential::{
            remove_account_in_txn, resolve_oauth_credential, set_account_enabled, store_sign_in,
            AccountPick,
        };
        let did = "did:key:z6MkTestProfileEvents";
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        crate::ensure_agent_principal(&node, did).await?;
        let access = ConfigAccess::Local(node);
        let spec = crate::inference_setup::connection_spec(
            crate::inference_setup::InferenceProviderId::Anthropic,
            crate::inference_setup::InferenceAuthMethod::ClaudeOauth,
            "",
        )?;
        let original = crate::InferenceBackend {
            agent_did: did.to_owned(),
            backend_id: "claude".into(),
            name: "Claude".into(),
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
        super::super::write_inference_backend_document(&access, &original).await?;
        let a = store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, claude_sign_in(did, "b"), None).await?;
        let b_backend = format!(
            "{}-{}",
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );
        for (profile_id, backend_id) in [
            ("on-a", "claude"),
            ("on-b", b_backend.as_str()),
            ("compaction", b_backend.as_str()),
        ] {
            let profile: InferenceProfile = serde_json::from_value(json!({
                "agent_did": did, "profile_id": profile_id, "backend_id": backend_id,
                "model_name": "model-x",
            }))?;
            write_inference_profile_document(&access, &profile).await?;
        }
        let profiles = || async {
            access
                .transact("test.profiles", |txn| {
                    Box::pin(async move {
                        let mut records = Vec::new();
                        for id in ["on-a", "on-b", "compaction"] {
                            records.push(
                                super::super::desired_state::read_record(
                                    txn,
                                    Collection::InferenceProfile,
                                    did,
                                    id,
                                )
                                .await?
                                .expect("profile"),
                            );
                        }
                        Ok(records)
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
                    .transact("test.remove", |txn| {
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

        for account in crate::oauth_credential::list_accounts(&access, did).await? {
            remove(account.credential_id).await;
        }
        let reused = store_sign_in(&access, claude_sign_in(did, "e"), None).await?;
        assert_eq!(reused.credential.account_ref, None, "the original slot");
        assert_eq!(reused.profiles, ["on-a"]);
        assert_eq!(profiles().await, before, "remove all, then sign in");
        let resolved = resolve_oauth_credential(
            &access,
            did,
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            AccountPick::Reference(None),
        )
        .await?
        .expect("the original slot resolves");
        assert_eq!(resolved.doc_id.as_deref(), Some(reused.doc_id.as_str()));

        let backends = access
            .transact("test.backends", |txn| {
                Box::pin(
                    async move { super::super::list_inference_backends_in_txn(txn, did).await },
                )
            })
            .await?;
        for backend in &backends {
            crate::backend_registry::record_discovered_catalog_on(&access, backend, Vec::new())
                .await?;
        }
        assert_eq!(profiles().await, before, "catalog refresh");
        let ConfigAccess::Local(node) = &access else {
            unreachable!()
        };
        crate::agent::document_view::load_document_runtime_view(node, did).await?;
        assert_eq!(profiles().await, before, "view load");
        Ok(())
    }

    #[tokio::test]
    async fn behavior_accounts_follow_profile_and_compaction() -> Result<()> {
        use crate::oauth_credential::{set_account_enabled, store_sign_in, AccountState};
        let did = "did:key:z6MkTestBehaviorAccounts";
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        crate::ensure_agent_principal(&node, did).await?;
        let access = ConfigAccess::Local(node);
        let spec = crate::inference_setup::connection_spec(
            crate::inference_setup::InferenceProviderId::Anthropic,
            crate::inference_setup::InferenceAuthMethod::ClaudeOauth,
            "",
        )?;
        let original = json!({
            "agent_did": did, "backend_id": "claude", "name": "Claude",
            "provider_kind": spec.provider_kind, "endpoint": spec.endpoint,
            "auth": {"kind": "principal_oauth"},
        });
        super::super::write_inference_backend_document(&access, &serde_json::from_value(original)?)
            .await?;
        store_sign_in(&access, claude_sign_in(did, "a"), None).await?;
        let b = store_sign_in(&access, claude_sign_in(did, "b"), Some("label-b")).await?;
        let b_backend = format!(
            "{}-{}",
            crate::claude_oauth::CLAUDE_OAUTH_PROVIDER,
            b.credential.account_ref.as_deref().unwrap()
        );
        let documents = [
            (Collection::InferenceProfile, json!({"agent_did": did, "profile_id": "profile-a", "backend_id": "claude", "model_name": "model-x"})),
            (Collection::InferenceProfile, json!({"agent_did": did, "profile_id": "profile-b", "backend_id": b_backend, "model_name": "model-x"})),
            (Collection::Compaction, json!({"agent_did": did, "compaction_id": "compaction-a", "inference_profile_id": "profile-a"})),
            (Collection::AgentContext, json!({"agent_did": did, "context_id": "context-x", "compaction_id": "compaction-a"})),
            (Collection::AgentBehavior, json!({"agent_did": did, "behavior_id": "behavior-x", "context_id": "context-x", "inference_profile_id": "profile-b"})),
            (Collection::AgentBehavior, json!({"agent_did": did, "behavior_id": "behavior-y", "inference_profile_id": "profile-a"})),
        ]
        .into_iter()
        .map(|(collection, value)| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
        .collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        access
            .transact("test.behavior_accounts", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    super::super::apply_desired_state_plan(txn, plan)
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
                },
            )
        };

        assert_eq!(
            behavior_accounts(&access, did, "behavior-x").await?,
            [
                expected("profile-b", "label-b", AccountState::Enabled),
                expected("profile-a", "Claude", AccountState::Enabled),
            ]
        );
        set_account_enabled(&access, did, &b.credential.credential_id, false).await?;
        assert_eq!(
            behavior_accounts(&access, did, "behavior-x").await?[0],
            expected("profile-b", "label-b", AccountState::Disabled)
        );
        assert_eq!(
            behavior_accounts(&access, did, "behavior-y").await?,
            [expected("profile-a", "Claude", AccountState::Enabled)]
        );
        assert!(behavior_accounts(&access, did, "behavior-unknown")
            .await
            .is_err());
        Ok(())
    }
}
