use std::sync::Arc;

use defra_node::QueryRequest;

use super::apply_txn::TxnBackend;
use super::native::execute_owned_native;
use super::{retry, ConfigApplyTxn, ConfigClientError};
use crate::store::config_client::write_telemetry::ConflictSource;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeDefaultAccountError {
    #[error("default account lookup requires the bound runtime and a managed Agent transaction")]
    RuntimeScope,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error("decoding runtime-visible default account metadata")]
    Decode,
    #[error("runtime default account read was not authorized")]
    ReadNotAuthorized,
}

impl ConfigApplyTxn<'_> {
    /// Resolve only the provider default account reference as the configured
    /// runtime, on this Agent transaction's native snapshot. DefraDB filters
    /// unreadable rows; the resolver uses this same runtime-visible set.
    pub(crate) async fn runtime_default_oauth_account_ref(
        &self,
        node_did: &str,
        provider: &str,
    ) -> Result<Option<String>, RuntimeDefaultAccountError> {
        let TxnBackend::Embedded {
            node,
            handle,
            agent_signer: Some(_),
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(RuntimeDefaultAccountError::RuntimeScope);
        };
        if node.node_identity_did() != Some(node_did) {
            return Err(RuntimeDefaultAccountError::RuntimeScope);
        }
        let runtime =
            identity::Did::new(node_did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let query =
            crate::inference::auth::credential::runtime_default_account_query(node_did, provider);
        let node = Arc::clone(node);
        let handle = handle.clone();
        let request = QueryRequest::new(&query).with_identity(Some(runtime.clone()));
        let response = execute_owned_native(
            async move { node.execute_request_in_txn(request, &handle).await },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?;
        if response.is_transaction_conflict() {
            return Err(retry::transaction_conflict(ConflictSource::StructuredCode).into());
        }
        if response.has_errors() {
            return Err(retry::transaction_storage_failure(
                ConfigClientError::EmbeddedGraphqlErrors {
                    errors: response.errors,
                },
            )
            .into());
        }
        let response = crate::store::graphql::into_data_envelope(response.data);
        let selected = crate::inference::auth::credential::runtime_default_account_from_response(
            &response, node_did, provider,
        )
        .map_err(|_| RuntimeDefaultAccountError::Decode)?;
        let Some((doc_id, account_ref)) = selected else {
            return Ok(None);
        };
        let decisions = self
            .observe_document_access(
                &runtime,
                "OAuthCredential",
                &[doc_id],
                defra_node::DocumentAccessMode::DelegatedRead,
            )
            .await?;
        match decisions.as_slice() {
            [defra_node::DocumentAccessDecision::Allowed] => Ok(account_ref),
            _ => Err(RuntimeDefaultAccountError::ReadNotAuthorized),
        }
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
        let metadata_query = crate::inference::auth::credential::runtime_default_account_query(
            runtime.did(),
            provider,
        );
        for secret_field in ["access_token", "refresh_token", "id_token"] {
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
            connected_at: Some(chrono::Utc::now()),
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

        let result = agent
            .transact_write("test.default_account.agent_write", |txn| {
                let agent_did = &binding.agent_did;
                Box::pin(async move {
                    let selected = txn
                        .runtime_default_oauth_account_ref(runtime.did(), provider)
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
                        txn.runtime_default_oauth_account_ref("did:key:foreign", provider)
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
                        txn.runtime_default_oauth_account_ref(runtime.did(), provider)
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
        Ok(())
    }
}
