use anyhow::{Context, Result};
use serde_json::Value;

use super::{read::read_record, ConfigApplyTxn};
use crate::Collection;

/// An explicit authenticated grant covers these physical documents; changing
/// a selection does not revoke or recreate an independently managed grant.
pub(super) async fn grant_selected_inference(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent: &identity::Did,
    agent_document: &Value,
) -> Result<()> {
    let Some(profile_id) = reference(agent_document, "inference_profile_id") else {
        return Ok(());
    };
    let profile = grant(
        txn,
        node_did,
        agent,
        Collection::InferenceProfile,
        profile_id,
    )
    .await?;
    if let Some(id) = reference(&profile, "sampling_id") {
        grant(txn, node_did, agent, Collection::InferenceSampling, id).await?;
    }
    if let Some(id) = reference(&profile, "execution_id") {
        let execution = grant(txn, node_did, agent, Collection::InferenceExecution, id).await?;
        if let Some(id) = reference(&execution, "retry_policy_id") {
            grant(txn, node_did, agent, Collection::InferenceRetryPolicy, id).await?;
        }
    }
    Ok(())
}

fn reference<'a>(document: &'a Value, field: &str) -> Option<&'a str> {
    document
        .get(field)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

async fn grant(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent: &identity::Did,
    collection: Collection,
    id: &str,
) -> Result<Value> {
    let (doc_id, value) = read_record(txn, collection, node_did, id)
        .await?
        .with_context(|| format!("selected {} is unavailable", collection.graphql_type()))?;
    txn.add_actor_relationship(collection.graphql_type(), &doc_id, "writer", agent)
        .await?;
    txn.add_actor_relationship(
        collection.graphql_type(),
        &doc_id,
        "selection_manager",
        agent,
    )
    .await?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::config_client::{ConfigAccess, NodePermission};
    use crate::store::identity::NodeIdentity;

    #[tokio::test]
    async fn selected_inference_self_config_requires_explicit_user_grant_and_preserves_revoke(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime");
        let runtime = crate::KeyIdentity::load_or_create(&path, None)?;
        let key = crate::store::identity::load_file_identity(&path)?.private_key_bytes();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        let signer = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
        crate::store::identity::grant_agent_node_capabilities(&node, &runtime, &binding).await?;
        let actor = identity::Did::new(&binding.agent_did)?;
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        let agent =
            ConfigAccess::local_managed_agent(node.clone(), actor.clone(), signer.as_ref())?;
        let node_did = crate::store::graphql::escape_graphql_string(runtime.did());
        let agent_did = crate::store::graphql::escape_graphql_string(&binding.agent_did);
        let nonce = crate::store::graphql::escape_graphql_string(&binding.creation_nonce);
        let execution = owner.write("test.inference.execution", &format!(r#"mutation {{ create_InferenceExecution(input: {{node_did:"{node_did}",execution_id:"execution"}}) {{_docID}} }}"#)).await?;
        let execution = crate::store::graphql::created_doc_id(&execution, "InferenceExecution")?;
        let profile = owner.write("test.inference.profile", &format!(r#"mutation {{ create_InferenceProfile(input: {{node_did:"{node_did}",profile_id:"profile",backend_id:"backend",model_name:"model",execution_id:"execution"}}) {{_docID}} }}"#)).await?;
        let profile = crate::store::graphql::created_doc_id(&profile, "InferenceProfile")?;
        owner.write("test.inference.agent", &format!(r#"mutation {{ create_Agent(input: {{node_did:"{node_did}",agent_did:"{agent_did}",creation_nonce:"{nonce}",inference_profile_id:"profile"}}) {{_docID}} }}"#)).await?;
        owner
            .grant_node_permission(&actor, NodePermission::DacRelationDelete)
            .await?;
        owner
            .transact_write("test.inference.select", |txn| {
                let execution = &execution;
                let profile = &profile;
                Box::pin(async move {
                    txn.add_entity_set_relationship(
                        "InferenceExecution",
                        execution,
                        "selected_by",
                        "configuration",
                        profile,
                        "selected_by",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let case = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let modeled = |phase: &str| {
            case.configuration_inference_self_config_cases
                .iter()
                .find(|value| {
                    value["phase"] == phase
                        && value["actor"] == "agent"
                        && value["document"] == "execution"
                })
                .unwrap()["expected"]
                .as_bool()
                .unwrap()
        };
        let doc_value = crate::store::graphql::escape_graphql_string(&execution);
        let update = format!(
            r#"mutation {{ update_InferenceExecution(docID:"{doc_value}", input:{{max_turns:20}}) {{_docID}} }}"#
        );
        assert_eq!(
            agent.write("test.inference.before", &update).await.is_ok(),
            modeled("before")
        );
        let runtime_did = runtime.did().to_owned();
        owner
            .transact_write("test.inference.grant", |txn| {
                let agent_did = &binding.agent_did;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    super::super::grant_selected_self_configuration(txn, runtime_did, agent_did)
                        .await
                })
            })
            .await?;
        assert_eq!(
            agent.write("test.inference.after", &update).await.is_ok(),
            modeled("granted")
        );
        agent.write("test.inference.new_execution", &format!(r#"mutation {{ create_InferenceExecution(input: {{node_did:"{node_did}",execution_id:"new-execution"}}) {{_docID}} }}"#)).await?;
        let profile_value = crate::store::graphql::escape_graphql_string(&profile);
        let profile_update = format!(
            r#"mutation {{ update_InferenceProfile(docID:"{profile_value}",input:{{execution_id:"new-execution"}}) {{_docID}} }}"#
        );
        let change = super::super::selection::Change {
            collection: Collection::InferenceProfile,
            node_did: runtime_did.clone(),
            doc_id: profile.clone(),
            before: Some(serde_json::json!({"execution_id":"execution"})),
            after: serde_json::json!({"execution_id":"new-execution"}),
        };
        agent
            .transact_write("test.inference.rebind", |txn| {
                let profile_update = &profile_update;
                let change = &change;
                Box::pin(async move {
                    txn.execute(profile_update).await?;
                    super::super::selection::apply_selection_changes(
                        txn,
                        std::slice::from_ref(change),
                        |_| true,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let profile_after = agent
            .execute(&format!(
                r#"{{ InferenceProfile(docID:"{profile_value}") {{ execution_id }} }}"#
            ))
            .await?;
        assert_eq!(
            profile_after["data"]["InferenceProfile"][0]["execution_id"],
            "new-execution"
        );
        owner
            .delete_actor_relationship("InferenceExecution", &execution, "writer", &actor)
            .await?;
        assert_eq!(
            agent.write("test.inference.revoked", &update).await.is_ok(),
            modeled("revoked")
        );
        assert!(
            node.nac_manager()
                .check_permission(&actor, NodePermission::DocumentUpdate)
                .await?
        );
        node.shutdown().await?;
        Ok(())
    }
}
