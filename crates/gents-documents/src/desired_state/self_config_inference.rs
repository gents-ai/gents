use std::collections::VecDeque;

use anyhow::{Context, Result};
use serde_json::Value;

use super::read::read_record;
use crate::{references_of, RequiredReference};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigApplyTxn;

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

/// Link one selected document to the Agent's revocable self-configuration
/// role: `writer` to edit it and `selection_manager` to select or unselect it
/// (Lean `Identity.Household.agentConfigWriterFixture`).
pub(super) async fn grant(
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
    use gents_store::config_client::{ConfigAccess, NodePermission};
    use gents_store::identity::NodeIdentity;

    fn updated(response: &Value, field: &str, doc_id: &str) -> Result<bool> {
        let rows = gents_store::graphql::mutation_documents(response, field)?
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
        let runtime = gents_store::identity::KeyIdentity::load_or_create(&path, None)?;
        let key = gents_store::identity::load_file_identity(&path)?.private_key_bytes();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .build()
                .await?,
        );
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        let binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
        let actor = identity::Did::new(&binding.agent_did)?;
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        gents_store::identity::GrantSet::AgentBaseline
            .grant(&owner, &actor)
            .await?;
        let agent =
            ConfigAccess::local_managed_agent(node.clone(), actor.clone(), signer.as_ref())?;
        let node_did = gents_store::graphql::escape_graphql_string(runtime.did());
        let agent_did = gents_store::graphql::escape_graphql_string(&binding.agent_did);
        let nonce = gents_store::graphql::escape_graphql_string(&binding.creation_nonce);
        let execution = owner.write("test.inference.execution", &format!(r#"mutation {{ create_InferenceExecution(input: {{node_did:"{node_did}",execution_id:"execution"}}) {{_docID}} }}"#)).await?;
        let execution = gents_store::graphql::created_doc_id(&execution, "InferenceExecution")?;
        let tools = owner.write("test.inference.tools", &format!(r#"mutation {{ create_Tools(input: {{node_did:"{node_did}",tools_id:"tools"}}) {{_docID}} }}"#)).await?;
        let tools = gents_store::graphql::created_doc_id(&tools, "Tools")?;
        let context = owner.write("test.inference.context", &format!(r#"mutation {{ create_AgentContext(input: {{node_did:"{node_did}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#)).await?;
        let context = gents_store::graphql::created_doc_id(&context, "AgentContext")?;
        let backend = owner.write("test.inference.backend", &format!(r#"mutation {{ create_InferenceBackendMetadata(input: {{node_did:"{node_did}",backend_id:"backend",name:"backend",provider_kind:"OpenAiCompatible",endpoint:"http://localhost:8000/v1",auth_kind:"unauthenticated"}}) {{_docID}} }}"#)).await?;
        let backend = gents_store::graphql::created_doc_id(&backend, "InferenceBackendMetadata")?;
        let profile = owner.write("test.inference.profile", &format!(r#"mutation {{ create_InferenceProfile(input: {{node_did:"{node_did}",profile_id:"profile",backend_id:"backend",model_name:"model",execution_id:"execution"}}) {{_docID}} }}"#)).await?;
        let profile = gents_store::graphql::created_doc_id(&profile, "InferenceProfile")?;
        let agent_doc = owner.write("test.inference.agent", &format!(r#"mutation {{ create_Agent(input: {{node_did:"{node_did}",agent_did:"{agent_did}",creation_nonce:"{nonce}",context_id:"context",inference_profile_id:"profile"}}) {{_docID}} }}"#)).await?;
        let agent_doc = gents_store::graphql::created_doc_id(&agent_doc, "Agent")?;
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
        let sibling_binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let sibling_signer =
            gents_store::identity::resolve_agent_identity(&runtime, &sibling_binding)?;
        let sibling_actor = identity::Did::new(&sibling_binding.agent_did)?;
        gents_store::identity::GrantSet::AgentBaseline
            .grant(&owner, &sibling_actor)
            .await?;
        let sibling = ConfigAccess::local_managed_agent(
            node.clone(),
            sibling_actor,
            sibling_signer.as_ref(),
        )?;
        let mut updates = std::collections::BTreeMap::<String, bool>::new();
        let mut managers = std::collections::BTreeMap::<String, bool>::new();
        let backend_value = gents_store::graphql::escape_graphql_string(&backend);
        let backend_update = format!(
            r#"mutation {{ update_InferenceBackendMetadata(docID:"{backend_value}",input:{{name:"backend"}}) {{_docID}} }}"#
        );
        let write_backend = |access: &ConfigAccess, operation: &'static str| {
            let access = access.clone();
            let backend_update = backend_update.clone();
            let backend = backend.clone();
            async move {
                match access.write(operation, &backend_update).await {
                    Ok(response) => updated(&response, "update_InferenceBackendMetadata", &backend),
                    Err(error) if error.is_graphql_rejection() || error.is_permission_denied() => {
                        Ok(false)
                    }
                    Err(error) => Err(error.into()),
                }
            }
        };
        let doc_value = gents_store::graphql::escape_graphql_string(&execution);
        let update = format!(
            r#"mutation {{ update_InferenceExecution(docID:"{doc_value}", input:{{max_turns:20}}) {{_docID}} }}"#
        );
        let execution_query =
            format!(r#"{{ InferenceExecution(docID:"{doc_value}") {{ max_turns }} }}"#);
        let before = owner.execute(&execution_query).await?;
        updates.insert(
            "before/agent/execution".to_owned(),
            updated(
                &agent.write("test.inference.before", &update).await?,
                "update_InferenceExecution",
                &execution,
            )?,
        );
        updates.insert(
            "before/agent/backend-metadata".to_owned(),
            write_backend(&agent, "test.inference.backend_before").await?,
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
        updates.insert(
            "granted/agent/execution".to_owned(),
            updated(
                &agent.write("test.inference.after", &update).await?,
                "update_InferenceExecution",
                &execution,
            )?,
        );
        updates.insert(
            "granted/sibling/execution".to_owned(),
            updated(
                &sibling.write("test.inference.sibling", &update).await?,
                "update_InferenceExecution",
                &execution,
            )?,
        );
        updates.insert(
            "granted/agent/backend-metadata".to_owned(),
            write_backend(&agent, "test.inference.backend_granted").await?,
        );
        updates.insert(
            "granted/sibling/backend-metadata".to_owned(),
            write_backend(&sibling, "test.inference.backend_sibling").await?,
        );
        let context_id = gents_store::graphql::escape_graphql_string(&context);
        let tools_id = gents_store::graphql::escape_graphql_string(&tools);
        let context_update = format!(
            r#"mutation {{ update_AgentContext(docID:"{context_id}",input:{{display_name:"updated"}}) {{_docID}} }}"#
        );
        let tools_update = format!(
            r#"mutation {{ update_Tools(docID:"{tools_id}",input:{{display_name:"updated"}}) {{_docID}} }}"#
        );
        updates.insert(
            "granted/agent/context".to_owned(),
            updated(
                &agent
                    .write("test.inference.context_granted", &context_update)
                    .await?,
                "update_AgentContext",
                &context,
            )?,
        );
        updates.insert(
            "granted/agent/tools".to_owned(),
            updated(
                &agent
                    .write("test.inference.tools_granted", &tools_update)
                    .await?,
                "update_Tools",
                &tools,
            )?,
        );
        let new_execution = owner.write("test.inference.new_execution", &format!(r#"mutation {{ create_InferenceExecution(input: {{node_did:"{node_did}",execution_id:"new-execution"}}) {{_docID}} }}"#)).await?;
        let new_execution =
            gents_store::graphql::created_doc_id(&new_execution, "InferenceExecution")?;
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
        let new_doc = gents_store::graphql::escape_graphql_string(&new_execution);
        let new_update = format!(
            r#"mutation {{ update_InferenceExecution(docID:"{new_doc}",input:{{max_turns:20}}) {{_docID}} }}"#
        );
        updates.insert(
            "granted/agent/new-execution".to_owned(),
            updated(
                &agent
                    .write("test.inference.new_granted", &new_update)
                    .await?,
                "update_InferenceExecution",
                &new_execution,
            )?,
        );
        let profile_value = gents_store::graphql::escape_graphql_string(&profile);
        updates.insert(
            "granted/agent/profile".to_owned(),
            updated(
                &agent
                    .write(
                        "test.inference.profile_granted",
                        &format!(
                            r#"mutation {{ update_InferenceProfile(docID:"{profile_value}",input:{{model_name:"model"}}) {{_docID}} }}"#
                        ),
                    )
                    .await?,
                "update_InferenceProfile",
                &profile,
            )?,
        );
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
                        false,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        managers.insert("granted/execution".to_owned(), rebind.is_ok());
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
                gents_store::config_client::AgentSelfConfigPermission::Revoke,
            )
            .await?;
        updates.insert(
            "direct-after-revoke/agent/execution".to_owned(),
            updated(
                &agent
                    .write("test.inference.direct_after_revoke", &update)
                    .await?,
                "update_InferenceExecution",
                &execution,
            )?,
        );
        let revoked_new_update = new_update.replace("max_turns:20", "max_turns:21");
        let revoked_profile_update = profile_update.replace("new-execution", "execution");
        let revoked_context_update = context_update.replace("updated", "revoked");
        let revoked_tools_update = tools_update.replace("updated", "revoked");
        updates.insert(
            "revoked/agent/new-execution".to_owned(),
            updated(
                &agent
                    .write("test.inference.new_revoked", &revoked_new_update)
                    .await?,
                "update_InferenceExecution",
                &new_execution,
            )?,
        );
        updates.insert(
            "revoked/agent/profile".to_owned(),
            updated(
                &agent
                    .write("test.inference.profile_revoked", &revoked_profile_update)
                    .await?,
                "update_InferenceProfile",
                &profile,
            )?,
        );
        updates.insert(
            "revoked/agent/context".to_owned(),
            updated(
                &agent
                    .write("test.inference.context_revoked", &revoked_context_update)
                    .await?,
                "update_AgentContext",
                &context,
            )?,
        );
        updates.insert(
            "revoked/agent/tools".to_owned(),
            updated(
                &agent
                    .write("test.inference.tools_revoked", &revoked_tools_update)
                    .await?,
                "update_Tools",
                &tools,
            )?,
        );
        updates.insert(
            "revoked/agent/backend-metadata".to_owned(),
            write_backend(&agent, "test.inference.backend_revoked").await?,
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
        managers.insert("revoked/new-execution".to_owned(), relation_revoke.is_ok());
        owner
            .delete_actor_relationship("InferenceExecution", &execution, "writer", &actor)
            .await?;
        let revoked_update = update.replace("max_turns:20", "max_turns:21");
        updates.insert(
            "revoked/agent/execution".to_owned(),
            updated(
                &agent
                    .write("test.inference.revoked", &revoked_update)
                    .await?,
                "update_InferenceExecution",
                &execution,
            )?,
        );
        for case in gents_lean_vocab::families::household_inference_self_config_update::cases() {
            let key = format!(
                "{}/{}/{}",
                case.input.phase, case.input.actor, case.input.document
            );
            let observed = *updates
                .get(&key)
                .unwrap_or_else(|| panic!("scenario never updated {key}"));
            case.check(observed);
        }
        for case in gents_lean_vocab::families::household_inference_self_config_manager::cases() {
            anyhow::ensure!(case.input.actor == "agent", "unmodeled manager actor");
            let key = format!("{}/{}", case.input.phase, case.input.document);
            let observed = *managers
                .get(&key)
                .unwrap_or_else(|| panic!("scenario never linked {key}"));
            case.check(observed);
        }
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
        let runtime = gents_store::identity::KeyIdentity::load_or_create(&path, None)?;
        let key = gents_store::identity::load_file_identity(&path)?.private_key_bytes();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(key))
                .with_node_acp_enabled()
                .build()
                .await?,
        );
        gents_store::schema::ensure_runtime_schemas(&node).await?;
        let binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
        gents_store::identity::GrantSet::AgentBaseline
            .grant(&owner, &identity::Did::new(&binding.agent_did)?)
            .await?;
        let node_did = gents_store::graphql::escape_graphql_string(runtime.did());
        let agent_did = gents_store::graphql::escape_graphql_string(&binding.agent_did);
        let nonce = gents_store::graphql::escape_graphql_string(&binding.creation_nonce);
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
