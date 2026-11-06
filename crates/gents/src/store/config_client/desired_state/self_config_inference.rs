use std::collections::VecDeque;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{read::read_record, ConfigApplyTxn};
use crate::documents::{references_of, RequiredReference};
use crate::Collection;

/// Each physical link follows the revocable Agent self-configuration role.
/// Independent actor grants are not changed by this publication. The walk is
/// the Agent's InferenceProfile and the canonical references below it.
pub(super) async fn grant_selected_inference(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_doc: &str,
    agent_document: &Value,
) -> Result<()> {
    let mut pending = selected(Collection::Agent, node_did, agent_document)?
        .filter(|reference| reference.collection == Collection::InferenceProfile)
        .collect::<VecDeque<_>>();
    while let Some(reference) = pending.pop_front() {
        if reference.collection == Collection::InferenceBackend {
            let (backend_doc, _) =
                read_record(txn, Collection::InferenceBackend, node_did, &reference.id)
                    .await?
                    .context("selected InferenceBackendMetadata is unavailable")?;
            txn.add_entity_set_relationship(
                Collection::InferenceBackend.graphql_type(),
                &backend_doc,
                "selection_manager",
                "agents",
                agent_doc,
                "config_writer",
            )
            .await?;
            continue;
        }
        let document = grant(
            txn,
            node_did,
            agent_doc,
            reference.collection,
            &reference.id,
        )
        .await?;
        pending.extend(selected(reference.collection, node_did, &document)?);
    }
    Ok(())
}

fn selected(
    collection: Collection,
    node_did: &str,
    document: &Value,
) -> Result<impl Iterator<Item = RequiredReference>> {
    Ok(references_of(collection, document, node_did)?
        .into_iter()
        .filter(|reference| !reference.id.is_empty()))
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

    use super::super::selection::tests::canonical;
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
    async fn user_grants_selected_backend_manager_without_backend_write() -> Result<()> {
        let (db, user, owner, binding) =
            crate::test_support::signed_test_node("backend-selection-manager").await;
        let runtime = db.node_identity.did().to_owned();
        let agent_identity =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)?;
        let actor = identity::Did::new(agent_identity.did())?;
        let agent = ConfigAccess::local_managed_agent(
            db.node.clone(),
            actor.clone(),
            agent_identity.as_ref(),
        )?;
        let backend_id = format!("{}:backend", binding.agent_did);
        let backend_query = format!(
            r#"{{ InferenceBackendMetadata(filter: {{backend_id:{{_eq:"{}"}}}}) {{_docID name}} }}"#,
            crate::store::graphql::escape_graphql_string(&backend_id)
        );
        let before = owner.execute(&backend_query).await?;
        let backend = before["data"]["InferenceBackendMetadata"][0]["_docID"]
            .as_str()
            .context("User-owned backend missing")?
            .to_owned();
        let owners = owner
            .transact_readonly("test.backend.owner", |txn| {
                let backend = &backend;
                Box::pin(async move {
                    txn.document_owners("InferenceBackendMetadata", std::slice::from_ref(backend))
                        .await
                })
            })
            .await?;
        assert_eq!(
            owners[0].owner.as_ref().map(|did| did.as_str()),
            Some(user.did())
        );
        let input = serde_json::json!({"node_did":runtime,"profile_id":"other-profile","backend_id":backend_id,"model_name":"test-model"});
        let created = owner.transact_write("test.backend.other_profile", |txn| {
            let input = &input;
            Box::pin(async move { txn.execute_client_signed_create(
                "mutation($input: InferenceProfileMutationInputArg!) { create_InferenceProfile(input:$input) {_docID} }",
                &serde_json::json!({"input":input}), gents_protocol::schemas::INFERENCE_PROFILE,
            ).await })
        }).await?;
        let profile = crate::store::graphql::created_doc_id(&created, "InferenceProfile")?;
        let case = crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .unwrap();
        let expected = |phase: &str| {
            case.configuration_inference_self_config_cases
                .iter()
                .find(|value| {
                    value["phase"] == phase
                        && value["actor"] == "agent"
                        && value["document"] == "backend-metadata"
                })
                .unwrap()["manager_expected"]
                .as_bool()
                .unwrap()
        };
        let link = || {
            agent.transact_write("test.backend.selected_by", |txn| {
                let backend = &backend;
                let profile = &profile;
                Box::pin(async move {
                    txn.add_entity_set_relationship(
                        "InferenceBackendMetadata",
                        backend,
                        "selected_by",
                        "configuration",
                        profile,
                        "selected_by",
                    )
                    .await
                })
            })
        };
        assert_eq!(link().await.is_ok(), expected("before"));
        owner
            .transact_write("test.backend.grant", |txn| {
                let runtime = &runtime;
                let agent_did = &binding.agent_did;
                Box::pin(async move {
                    super::super::grant_selected_self_configuration(txn, runtime, agent_did).await
                })
            })
            .await?;
        assert_eq!(link().await.is_ok(), expected("granted"));
        let node_rows = owner.execute("{ Node {_docID node_did} }").await?;
        let node_doc = node_rows["data"]["Node"]
            .as_array()
            .context("Node rows")?
            .iter()
            .find(|row| row["node_did"] == runtime)
            .and_then(|row| row["_docID"].as_str())
            .context("User-owned Node missing")?;
        let node_binding = super::super::ConfigurationNodeBinding {
            node_did: identity::Did::new(&runtime)?,
            doc_id: node_doc.to_owned(),
            enabled: true,
            default_agent_did: Some(binding.agent_did.clone()),
        };
        let value = serde_json::json!({"node_did":runtime,"profile_id":"fresh-profile",
            "backend_id":backend_id,"model_name":"test-model"});
        let plan = super::super::plan::DesiredStateApplyPlan::new(vec![
            super::super::plan::DesiredStateApplyDocument {
                collection: Collection::InferenceProfile,
                add: value.clone(),
                update: value,
            },
        ])?;
        agent
            .transact_write("test.backend.create_profile", |txn| {
                let (plan, node_binding) = (&plan, &node_binding);
                Box::pin(async move {
                    super::super::apply_desired_state_plan_with_node_binding(
                        txn,
                        plan,
                        Some(node_binding),
                    )
                    .await
                })
            })
            .await?;
        let created = agent
            .execute(
                r#"{ InferenceProfile(filter: {profile_id: {_eq: "fresh-profile"}}) {_docID} }"#,
            )
            .await?;
        let created_doc = created["data"]["InferenceProfile"][0]["_docID"]
            .as_str()
            .context("Agent-created profile missing")?
            .to_owned();
        let created_owners = agent
            .transact_readonly("test.backend.profile_owner", |txn| {
                let created_doc = &created_doc;
                Box::pin(async move {
                    txn.document_owners("InferenceProfile", std::slice::from_ref(created_doc))
                        .await
                })
            })
            .await?;
        assert_eq!(created_owners[0].owner.as_ref(), Some(&actor));
        let mutation = format!(
            r#"mutation {{ update_InferenceBackendMetadata(docID:"{}",input:{{name:"forged"}}) {{_docID}} }}"#,
            crate::store::graphql::escape_graphql_string(&backend)
        );
        match agent.write("test.backend.write_denied", &mutation).await {
            Ok(response) => assert!(!updated(
                &response,
                "update_InferenceBackendMetadata",
                &backend
            )?),
            Err(error) => assert!(error.is_graphql_rejection(), "{error}"),
        }
        assert_eq!(owner.execute(&backend_query).await?, before);
        owner
            .set_agent_self_config_permission(
                &runtime,
                &binding.agent_did,
                crate::store::config_client::AgentSelfConfigPermission::Revoke,
            )
            .await?;
        assert_eq!(link().await.is_ok(), expected("revoked"));
        db.node.shutdown().await?;
        Ok(())
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
        let actor = identity::Did::new(&binding.agent_did)?;
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        crate::store::identity::GrantSet::AgentBaseline
            .grant(&owner, &actor)
            .await?;
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
        owner.write("test.inference.backend", &format!(r#"mutation {{ create_InferenceBackendMetadata(input: {{node_did:"{node_did}",backend_id:"backend",name:"backend",provider_kind:"OpenAiCompatible",endpoint:"http://localhost:8000/v1",auth_kind:"unauthenticated"}}) {{_docID}} }}"#)).await?;
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
            before: Some(canonical(
                Collection::InferenceProfile,
                &runtime_did,
                serde_json::json!({"execution_id":"execution"}),
            )),
            after: canonical(
                Collection::InferenceProfile,
                &runtime_did,
                serde_json::json!({"execution_id":"new-execution"}),
            ),
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

    #[tokio::test]
    async fn self_config_grant_reads_only_agents_with_a_profile() -> Result<()> {
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
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        crate::store::identity::GrantSet::AgentBaseline
            .grant(&owner, &identity::Did::new(&binding.agent_did)?)
            .await?;
        let node_did = crate::store::graphql::escape_graphql_string(runtime.did());
        let agent_did = crate::store::graphql::escape_graphql_string(&binding.agent_did);
        let nonce = crate::store::graphql::escape_graphql_string(&binding.creation_nonce);
        owner.write("test.profileless.context", &format!(r#"mutation {{ create_AgentContext(input: {{node_did:"{node_did}",context_id:"context"}}) {{_docID}} }}"#)).await?;
        owner.write("test.profileless.agent", &format!(r#"mutation {{ create_Agent(input: {{node_did:"{node_did}",agent_did:"{agent_did}",creation_nonce:"{nonce}",context_id:"context"}}) {{_docID}} }}"#)).await?;
        let runtime_did = runtime.did().to_owned();
        let read = owner
            .transact_readonly("test.profileless.read", |txn| {
                let agent_did = &binding.agent_did;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    read_record(txn, Collection::Agent, runtime_did, agent_did).await
                })
            })
            .await;
        assert!(read.is_err(), "{read:?}");
        let grant = owner
            .transact_write("test.profileless.grant", |txn| {
                let agent_did = &binding.agent_did;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    super::super::grant_selected_self_configuration(txn, runtime_did, agent_did)
                        .await
                })
            })
            .await;
        assert!(grant.is_err());
        node.shutdown().await?;
        Ok(())
    }
}
