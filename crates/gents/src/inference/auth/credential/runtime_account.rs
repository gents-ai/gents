use super::{
    runtime_account_from_response, runtime_account_query, AccountPick, RuntimeAccountSelection,
};
use crate::store::config_client::{ConfigApplyTxn, ConfigClientError};
use crate::store::identity::NodeIdentity;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeAccountMetadataError {
    #[error("account lookup requires the bound runtime and a managed Agent transaction")]
    RuntimeScope,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error("decoding runtime-visible account metadata")]
    Decode,
    #[error("runtime account read was not authorized")]
    ReadNotAuthorized,
}

/// Resolve only the provider default account reference as the configured
/// runtime, on this Agent transaction's native snapshot. DefraDB filters
/// unreadable rows; the resolver uses this same runtime-visible set.
pub(crate) async fn runtime_default_oauth_account_ref(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
) -> Result<Option<String>, RuntimeAccountMetadataError> {
    Ok(
        runtime_oauth_account_selection(txn, node_did, provider, AccountPick::ProviderDefault)
            .await?
            .and_then(|selected| selected.account_ref),
    )
}

/// Reset scheduling observes only whether the selected enabled account was
/// connected at the failed call. The held Node is bound to this managed
/// Agent transaction, and native ACP still authorizes its metadata read.
pub(crate) async fn runtime_oauth_account_served_before(
    txn: &ConfigApplyTxn<'_>,
    runtime: &dyn NodeIdentity,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
    started: chrono::DateTime<chrono::Utc>,
) -> Result<bool, RuntimeAccountMetadataError> {
    if runtime.did() != node_did || runtime.agent_derivation_root().is_none() {
        return Err(RuntimeAccountMetadataError::RuntimeScope);
    }
    Ok(
        runtime_oauth_account_selection(txn, node_did, provider, pick)
            .await?
            .is_some_and(|selected| selected.connected_at.is_none_or(|since| since <= started)),
    )
}

async fn runtime_oauth_account_selection(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    provider: &str,
    pick: AccountPick<'_>,
) -> Result<Option<RuntimeAccountSelection>, RuntimeAccountMetadataError> {
    let runtime =
        identity::Did::new(node_did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
    let response = txn
        .execute_runtime_account_metadata_query(
            node_did,
            &runtime_account_query(node_did, provider),
        )
        .await
        .map_err(|error| match error {
            ConfigClientError::EmbeddedAccessRequired
            | ConfigClientError::TransactionActorMismatch => {
                RuntimeAccountMetadataError::RuntimeScope
            }
            error => RuntimeAccountMetadataError::Access(error),
        })?;
    let response = crate::store::graphql::into_data_envelope(response.data);
    let selected = runtime_account_from_response(&response, node_did, provider, pick)
        .map_err(|_| RuntimeAccountMetadataError::Decode)?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let decisions = txn
        .observe_document_access(
            &runtime,
            "OAuthCredential",
            std::slice::from_ref(&selected.doc_id),
            defra_node::DocumentAccessMode::DelegatedRead,
        )
        .await?;
    match decisions.as_slice() {
        [defra_node::DocumentAccessDecision::Allowed] => Ok(Some(selected)),
        _ => Err(RuntimeAccountMetadataError::ReadNotAuthorized),
    }
}

#[cfg(test)]
mod tests {
    use crate::store::config_client::{ConfigAccess, NodePermission};
    use crate::store::identity::NodeIdentity;

    #[tokio::test]
    async fn runtime_default_read_shares_agent_transaction_and_preserves_secret_acl(
    ) -> anyhow::Result<()> {
        let (db, user, user_access, binding) =
            crate::test_support::signed_test_node("runtime-default-account").await;
        let runtime = db.node_identity.as_ref();
        let signer = crate::store::identity::resolve_agent_identity(runtime, &binding)?;
        let agent = ConfigAccess::local_managed_agent(
            db.node.clone(),
            identity::Did::new(&binding.agent_did)?,
            signer.as_ref(),
        )?;
        user_access
            .transact_write("test.default_account.grant_agent", |txn| {
                let binding = &binding;
                Box::pin(async move {
                    crate::store::config_client::grant_selected_self_configuration(
                        txn,
                        runtime.did(),
                        &binding.agent_did,
                    )
                    .await
                })
            })
            .await?;
        let model = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let case = |name: &str| {
            model
                .runtime_default_account_cases
                .iter()
                .find(|case| case["name"] == name)
                .unwrap()
        };
        let provider = "default-account-test";
        let metadata_query = super::super::runtime_account_query(runtime.did(), provider);
        for secret_field in [
            "access_token",
            "refresh_token",
            "id_token",
            "provider_account_key",
        ] {
            assert!(!metadata_query.contains(secret_field));
        }
        let credential = crate::inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: "test-default-account".into(),
            node_did: runtime.did().into(),
            provider: provider.into(),
            access_token: "private-access-token".into(),
            refresh_token: "private-refresh-token".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: Some("named".into()),
            connected_at: Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()),
            provider_account_key: None,
            label: None,
        };
        let doc = crate::inference::auth::credential::upsert_oauth_credential_on(
            &user_access,
            &credential,
        )
        .await?;
        let hidden = agent
            .execute("{ OAuthCredential { _docID account_ref access_token refresh_token } }")
            .await?;
        assert!(hidden["data"]["OAuthCredential"]
            .as_array()
            .unwrap()
            .is_empty());

        let connected = credential.connected_at.unwrap();
        for (pick, started, expected) in [
            (
                crate::inference::auth::credential::AccountPick::ProviderDefault,
                connected,
                true,
            ),
            (
                crate::inference::auth::credential::AccountPick::Reference(Some("named")),
                connected,
                true,
            ),
            (
                crate::inference::auth::credential::AccountPick::Reference(Some("named")),
                connected - chrono::Duration::seconds(1),
                false,
            ),
            (
                crate::inference::auth::credential::AccountPick::Reference(Some("missing")),
                connected,
                false,
            ),
            (
                crate::inference::auth::credential::AccountPick::Reference(None),
                connected,
                false,
            ),
        ] {
            let observed = agent
                .transact_readonly("test.reset_account.selection", |txn| {
                    Box::pin(async move {
                        Ok::<_, anyhow::Error>(
                            super::runtime_oauth_account_served_before(
                                txn,
                                runtime,
                                runtime.did(),
                                provider,
                                pick,
                                started,
                            )
                            .await?,
                        )
                    })
                })
                .await?;
            assert_eq!(observed, expected);
        }
        let foreign = agent
            .transact_readonly("test.reset_account.foreign_runtime", |txn| {
                let user = &user;
                Box::pin(async move {
                    super::runtime_oauth_account_served_before(
                        txn,
                        user,
                        runtime.did(),
                        provider,
                        crate::inference::auth::credential::AccountPick::ProviderDefault,
                        connected,
                    )
                    .await
                    .map_err(anyhow::Error::from)
                })
            })
            .await;
        assert!(foreign.is_err());

        let mut disabled = credential.clone();
        disabled.enabled = false;
        crate::inference::auth::credential::upsert_oauth_credential_on(&user_access, &disabled)
            .await?;
        let disabled = agent
            .transact_readonly("test.reset_account.disabled", |txn| {
                Box::pin(async move {
                    super::runtime_oauth_account_served_before(
                        txn,
                        runtime,
                        runtime.did(),
                        provider,
                        crate::inference::auth::credential::AccountPick::ProviderDefault,
                        connected,
                    )
                    .await
                    .map_err(anyhow::Error::from)
                })
            })
            .await?;
        assert!(!disabled);
        crate::inference::auth::credential::upsert_oauth_credential_on(&user_access, &credential)
            .await?;

        let result = agent
            .transact_write("test.default_account.agent_write", |txn| {
                let agent_did = &binding.agent_did;
                Box::pin(async move {
                    let selected =
                        super::runtime_default_oauth_account_ref(txn, runtime.did(), provider)
                            .await?;
                    let escaped = crate::store::graphql::escape_graphql_string(agent_did);
                    let changed = txn.execute(&format!(r#"mutation {{ update_Agent(filter: {{agent_did: {{_eq: "{escaped}"}}}}, input: {{display_name: "Agent-signed update"}}) {{ _docID }} }}"#)).await?;
                    anyhow::ensure!(changed["data"]["update_Agent"].as_array().is_some_and(|rows| rows.len() == 1));
                    Ok::<_, anyhow::Error>(selected)
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            case("visible-named")["allowed"].as_bool().unwrap()
        );
        assert_eq!(
            result?,
            case("visible-named")["visible_default"]
                .as_str()
                .map(str::to_owned)
        );
        let stored = user_access
            .execute("{ Agent { agent_did display_name } }")
            .await?;
        assert!(stored["data"]["Agent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["agent_did"] == binding.agent_did
                && row["display_name"] == "Agent-signed update"));
        let foreign = agent
            .transact_readonly("test.default_account.foreign", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        super::runtime_default_oauth_account_ref(txn, "did:key:foreign", provider)
                            .await?,
                    )
                })
            })
            .await;
        assert_eq!(
            foreign.is_ok(),
            case("foreign-node")["allowed"].as_bool().unwrap()
        );

        let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did())?);
        admin
            .grant_node_permission(
                &identity::Did::new(user.did())?,
                NodePermission::DacRelationDelete,
            )
            .await?;
        let node = user_access.execute("{ Node { _docID } }").await?;
        let node_doc = node["data"]["Node"][0]["_docID"].as_str().unwrap();
        let runtime_did = identity::Did::new(runtime.did())?;
        user_access
            .delete_actor_relationship("Node", node_doc, "runtime_reader", &runtime_did)
            .await?;
        user_access
            .delete_actor_relationship("OAuthCredential", &doc, "runtime_writer", &runtime_did)
            .await?;
        let visible = admin
            .execute("{ OAuthCredential { _docID access_token } }")
            .await?;
        assert!(visible["data"]["OAuthCredential"]
            .as_array()
            .unwrap()
            .is_empty());
        let empty = agent
            .transact_readonly("test.default_account.hidden", |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        super::runtime_default_oauth_account_ref(txn, runtime.did(), provider)
                            .await?,
                    )
                })
            })
            .await;
        assert_eq!(
            empty.is_ok(),
            case("visible-empty")["allowed"].as_bool().unwrap()
        );
        assert_eq!(empty?, None);
        let unavailable = agent
            .transact_readonly("test.reset_account.revoked", |txn| {
                Box::pin(async move {
                    super::runtime_oauth_account_served_before(
                        txn,
                        runtime,
                        runtime.did(),
                        provider,
                        crate::inference::auth::credential::AccountPick::ProviderDefault,
                        connected,
                    )
                    .await
                    .map_err(anyhow::Error::from)
                })
            })
            .await?;
        assert!(!unavailable);
        Ok(())
    }
}
