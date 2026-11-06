use anyhow::{Context, Result};
use serde_json::Value;

use super::{read::read_record, ConfigApplyTxn};
use crate::Collection;

/// Each physical link follows the revocable Agent self-configuration role.
/// Independent actor grants are not changed by this publication.
pub(super) async fn grant_selected_inference(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_doc: &str,
    agent_document: &Value,
) -> Result<()> {
    let Some(profile_id) = reference(agent_document, "inference_profile_id") else {
        return Ok(());
    };
    let profile = grant(
        txn,
        node_did,
        agent_doc,
        Collection::InferenceProfile,
        profile_id,
    )
    .await?;
    if let Some(id) = reference(&profile, "sampling_id") {
        grant(txn, node_did, agent_doc, Collection::InferenceSampling, id).await?;
    }
    if let Some(id) = reference(&profile, "execution_id") {
        let execution = grant(txn, node_did, agent_doc, Collection::InferenceExecution, id).await?;
        if let Some(id) = reference(&execution, "retry_policy_id") {
            grant(
                txn,
                node_did,
                agent_doc,
                Collection::InferenceRetryPolicy,
                id,
            )
            .await?;
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
    agent_doc: &str,
    collection: Collection,
    id: &str,
) -> Result<Value> {
    let (doc_id, value) = read_record(txn, collection, node_did, id)
        .await?
        .with_context(|| format!("selected {} is unavailable", collection.graphql_type()))?;
    txn.add_entity_set_relationship(
        collection.graphql_type(),
        &doc_id,
        "writer",
        "agents",
        agent_doc,
        "config_writer",
    )
    .await?;
    txn.add_entity_set_relationship(
        collection.graphql_type(),
        &doc_id,
        "selection_manager",
        "agents",
        agent_doc,
        "config_writer",
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

    fn updated(response: &Value, field: &str, doc_id: &str) -> Result<bool> {
        let rows = crate::store::graphql::mutation_documents(response, field)?
            .context("mutation response missing update field")?;
        match rows {
            [] => Ok(false),
            [row] => {
                anyhow::ensure!(
                    row["_docID"] == doc_id,
                    "mutation returned a different document"
                );
                Ok(true)
            }
            _ => anyhow::bail!("mutation returned multiple documents"),
        }
    }

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
        let tools = owner.write("test.inference.tools", &format!(r#"mutation {{ create_Tools(input: {{node_did:"{node_did}",tools_id:"tools"}}) {{_docID}} }}"#)).await?;
        let tools = crate::store::graphql::created_doc_id(&tools, "Tools")?;
        let context = owner.write("test.inference.context", &format!(r#"mutation {{ create_AgentContext(input: {{node_did:"{node_did}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#)).await?;
        let context = crate::store::graphql::created_doc_id(&context, "AgentContext")?;
        let profile = owner.write("test.inference.profile", &format!(r#"mutation {{ create_InferenceProfile(input: {{node_did:"{node_did}",profile_id:"profile",backend_id:"backend",model_name:"model",execution_id:"execution"}}) {{_docID}} }}"#)).await?;
        let profile = crate::store::graphql::created_doc_id(&profile, "InferenceProfile")?;
        let agent_doc = owner.write("test.inference.agent", &format!(r#"mutation {{ create_Agent(input: {{node_did:"{node_did}",agent_did:"{agent_did}",creation_nonce:"{nonce}",context_id:"context",inference_profile_id:"profile"}}) {{_docID}} }}"#)).await?;
        let agent_doc = crate::store::graphql::created_doc_id(&agent_doc, "Agent")?;
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
        let modeled = |phase: &str, document: &str| {
            case.configuration_inference_self_config_cases
                .iter()
                .find(|value| {
                    value["phase"] == phase
                        && value["actor"] == "agent"
                        && value["document"] == document
                })
                .unwrap()["expected"]
                .as_bool()
                .unwrap()
        };
        let modeled_manager = |phase: &str, document: &str| {
            case.configuration_inference_self_config_cases
                .iter()
                .find(|value| {
                    value["phase"] == phase
                        && value["actor"] == "agent"
                        && value["document"] == document
                })
                .unwrap()["manager_expected"]
                .as_bool()
                .unwrap()
        };
        let doc_value = crate::store::graphql::escape_graphql_string(&execution);
        let update = format!(
            r#"mutation {{ update_InferenceExecution(docID:"{doc_value}", input:{{max_turns:20}}) {{_docID}} }}"#
        );
        let execution_query =
            format!(r#"{{ InferenceExecution(docID:"{doc_value}") {{ max_turns }} }}"#);
        let before = owner.execute(&execution_query).await?;
        assert_eq!(
            updated(
                &agent.write("test.inference.before", &update).await?,
                "update_InferenceExecution",
                &execution,
            )?,
            modeled("before", "execution")
        );
        assert_eq!(owner.execute(&execution_query).await?, before);
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
            updated(
                &agent.write("test.inference.after", &update).await?,
                "update_InferenceExecution",
                &execution,
            )?,
            modeled("granted", "execution")
        );
        let context_id = crate::store::graphql::escape_graphql_string(&context);
        let tools_id = crate::store::graphql::escape_graphql_string(&tools);
        let context_update = format!(
            r#"mutation {{ update_AgentContext(docID:"{context_id}",input:{{display_name:"updated"}}) {{_docID}} }}"#
        );
        let tools_update = format!(
            r#"mutation {{ update_Tools(docID:"{tools_id}",input:{{display_name:"updated"}}) {{_docID}} }}"#
        );
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.context_granted", &context_update)
                    .await?,
                "update_AgentContext",
                &context,
            )?,
            modeled("granted", "context")
        );
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.tools_granted", &tools_update)
                    .await?,
                "update_Tools",
                &tools,
            )?,
            modeled("granted", "tools")
        );
        let new_execution = owner.write("test.inference.new_execution", &format!(r#"mutation {{ create_InferenceExecution(input: {{node_did:"{node_did}",execution_id:"new-execution"}}) {{_docID}} }}"#)).await?;
        let new_execution =
            crate::store::graphql::created_doc_id(&new_execution, "InferenceExecution")?;
        owner
            .transact_write("test.inference.grant_new_execution", |txn| {
                let agent_doc = &agent_doc;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    super::grant(
                        txn,
                        runtime_did,
                        agent_doc,
                        Collection::InferenceExecution,
                        "new-execution",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let new_doc = crate::store::graphql::escape_graphql_string(&new_execution);
        let new_update = format!(
            r#"mutation {{ update_InferenceExecution(docID:"{new_doc}",input:{{max_turns:20}}) {{_docID}} }}"#
        );
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.new_granted", &new_update)
                    .await?,
                "update_InferenceExecution",
                &new_execution,
            )?,
            modeled("granted", "new-execution")
        );
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
        let rebind = agent
            .transact_write("test.inference.rebind", |txn| {
                let profile_update = &profile_update;
                let change = &change;
                Box::pin(async move {
                    let response = txn.execute(profile_update).await?;
                    anyhow::ensure!(
                        updated(&response, "update_InferenceProfile", &change.doc_id)?,
                        "profile rebind did not update the selected document"
                    );
                    super::super::selection::apply_selection_changes(
                        txn,
                        std::slice::from_ref(change),
                        |_| true,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        assert_eq!(rebind.is_ok(), modeled_manager("granted", "execution"));
        rebind?;
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
            .add_actor_relationship("InferenceExecution", &execution, "writer", &actor)
            .await?;
        owner
            .set_agent_self_config_permission(
                &runtime_did,
                &binding.agent_did,
                crate::store::config_client::AgentSelfConfigPermission::Revoke,
            )
            .await?;
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.direct_after_revoke", &update)
                    .await?,
                "update_InferenceExecution",
                &execution,
            )?,
            modeled("direct-after-revoke", "execution")
        );
        let revoked_new_update = new_update.replace("max_turns:20", "max_turns:21");
        let revoked_profile_update = profile_update.replace("new-execution", "execution");
        let revoked_context_update = context_update.replace("updated", "revoked");
        let revoked_tools_update = tools_update.replace("updated", "revoked");
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.new_revoked", &revoked_new_update)
                    .await?,
                "update_InferenceExecution",
                &new_execution,
            )?,
            modeled("revoked", "new-execution")
        );
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.profile_revoked", &revoked_profile_update)
                    .await?,
                "update_InferenceProfile",
                &profile,
            )?,
            modeled("revoked", "profile")
        );
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.context_revoked", &revoked_context_update)
                    .await?,
                "update_AgentContext",
                &context,
            )?,
            modeled("revoked", "context")
        );
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.tools_revoked", &revoked_tools_update)
                    .await?,
                "update_Tools",
                &tools,
            )?,
            modeled("revoked", "tools")
        );
        let observed = owner.execute(&format!(
            r#"{{ InferenceExecution(docID:"{new_doc}") {{ max_turns }} InferenceProfile(docID:"{profile_value}") {{ execution_id }} AgentContext(docID:"{context_id}") {{ display_name }} Tools(docID:"{tools_id}") {{ display_name }} }}"#
        )).await?;
        assert_eq!(observed["data"]["InferenceExecution"][0]["max_turns"], 20);
        assert_eq!(
            observed["data"]["InferenceProfile"][0]["execution_id"],
            "new-execution"
        );
        assert_eq!(
            observed["data"]["AgentContext"][0]["display_name"],
            "updated"
        );
        assert_eq!(observed["data"]["Tools"][0]["display_name"], "updated");
        let relation_revoke = agent
            .transact_write("test.inference.revoke_denied", |txn| {
                let new_execution = &new_execution;
                let profile = &profile;
                Box::pin(async move {
                    txn.delete_entity_set_relationship(
                        "InferenceExecution",
                        new_execution,
                        "selected_by",
                        "configuration",
                        profile,
                        "selected_by",
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        assert_eq!(
            relation_revoke.is_ok(),
            modeled_manager("revoked", "new-execution")
        );
        owner
            .delete_actor_relationship("InferenceExecution", &execution, "writer", &actor)
            .await?;
        let revoked_update = update.replace("max_turns:20", "max_turns:21");
        assert_eq!(
            updated(
                &agent
                    .write("test.inference.revoked", &revoked_update)
                    .await?,
                "update_InferenceExecution",
                &execution,
            )?,
            modeled("revoked", "execution")
        );
        assert_eq!(
            owner.execute(&execution_query).await?["data"]["InferenceExecution"][0]["max_turns"],
            20
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
