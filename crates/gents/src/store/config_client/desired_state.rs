mod event_grants;
mod workspace_selection;
pub(super) mod plan;
pub(super) mod projection;
pub(super) mod read;
pub(super) mod schema_fields;
pub(super) mod selection;
pub(super) mod task_selection;
pub(super) mod validate;

use super::{mint_recreate_identity, ConfigApplyTxn, ConfigRelationshipGrant};
use crate::store::graphql::escape_graphql_string;
use crate::Collection;
use anyhow::{Context, Result};
use gents_protocol::graphql::extract_mutation_doc_id;
use plan::{
    document_identity, ensure_expectations_hold, validate_node_replacement,
    DesiredStateApplyCounts, DesiredStateApplyPlan,
};
use read::read_identity_records;
use schema_fields::{
    prefetch_declared_fields, validate_event_source_live_fields,
    validate_output_obligation_count_fields, IntrospectedFields,
};
use std::collections::BTreeSet;
use validate::{
    changed_configuration_keys, validate_advertised_profiles, validate_outcome_source_fields,
    validate_trigger_document_fields,
};

#[cfg(test)]
fn confirm_removal(response: &serde_json::Value, collection: &str, doc_id: &str) -> Result<()> {
    let field = format!("delete_{collection}");
    let rows = response
        .get("data")
        .and_then(|data| data.get(field.as_str()))
        .and_then(serde_json::Value::as_array);
    if !matches!(rows.map(Vec::as_slice), Some([row]) if row.get("_docID").and_then(serde_json::Value::as_str) == Some(doc_id))
    {
        return Err(super::ConfigClientError::DesiredStateRemovalUnconfirmed.into());
    }
    Ok(())
}

/// Documents commit in the caller's transaction. Managed Agent creates stage
/// their creator-managed relationships in that transaction; the client HTTP
/// publication route retains its separately observed post-commit grants.
/// Reference checks run after staged writes so cyclic configurations do not
/// depend on write order.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<DesiredStateApplyCounts> {
    apply_desired_state_plan_with_node_binding(txn, plan, None).await
}

/// Explicitly grant an Agent authority to configure its selected documents.
/// The transaction's authenticated actor must be allowed to manage each grant.
pub async fn grant_selected_self_configuration(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    agent_did: &str,
) -> Result<()> {
    let target = identity::Did::new(agent_did)?;
    let (agent_doc, agent) = read::read_record(txn, Collection::Agent, node_did, agent_did)
        .await?
        .context("selected Agent is unavailable")?;
    txn.add_actor_relationship("Agent", &agent_doc, "config_writer", &target)
        .await?;
    if let Some(context_id) = agent["context_id"].as_str() {
        let (context_doc, context) =
            read::read_record(txn, Collection::AgentContext, node_did, context_id)
                .await?
                .context("selected AgentContext is unavailable")?;
        txn.add_actor_relationship("AgentContext", &context_doc, "writer", &target)
            .await?;
        if let Some(tools_id) = context["tools_id"].as_str() {
            let (tools_doc, _) = read::read_record(txn, Collection::Tools, node_did, tools_id)
                .await?
                .context("selected Tools is unavailable")?;
            txn.add_actor_relationship("Tools", &tools_doc, "writer", &target)
                .await?;
        }
    }
    Ok(())
}

/// The runtime obtains this physical Node binding through its own native read
/// grant. It is passed with the agent operation; it never changes the actor.
#[derive(Clone)]
pub(crate) struct ConfigurationNodeBinding {
    pub(crate) node_did: identity::Did,
    pub(crate) doc_id: String,
    pub(crate) enabled: bool,
    pub(crate) default_agent_did: Option<String>,
}

pub(crate) async fn configuration_node_doc(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    binding: Option<&ConfigurationNodeBinding>,
) -> Result<String> {
    if let Some(binding) = binding {
        anyhow::ensure!(
            binding.node_did.as_str() == node_did && !binding.doc_id.is_empty(),
            "Node binding does not match configuration"
        );
        let observed = txn
            .document_owners_as(
                &binding.node_did,
                "Node",
                std::slice::from_ref(&binding.doc_id),
            )
            .await?;
        anyhow::ensure!(
            matches!(observed.as_slice(), [row] if row.visible && row.doc_id == binding.doc_id),
            "bound Node is unavailable to the runtime"
        );
        Ok(binding.doc_id.clone())
    } else {
        read::read_record(txn, Collection::Node, node_did, node_did)
            .await?
            .map(|(doc, _)| doc)
            .context("creating configuration requires its user-owned Node")
    }
}

pub(crate) async fn apply_desired_state_plan_with_node_binding(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    binding: Option<&ConfigurationNodeBinding>,
) -> Result<DesiredStateApplyCounts> {
    ensure_expectations_hold(txn, plan).await?;
    let event_grants = event_grants::capture(txn, plan).await?;
    let workspace_selection = workspace_selection::capture(txn, plan).await?;
    let mut introspected = IntrospectedFields::new();
    prefetch_declared_fields(txn, plan, &mut introspected).await?;
    let document_identities = plan
        .documents()
        .iter()
        .map(|document| {
            let (owner, id) = document_identity(document.collection, &document.add)?;
            Ok((document.collection, owner.to_owned(), id.to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut existing_documents = read_identity_records(txn, &document_identities).await?;
    let mut counts = DesiredStateApplyCounts::default();
    let mut created_agents = Vec::new();
    let mut created_configuration = Vec::new();
    let mut selection_changes = Vec::new();
    let mut created_callback_bindings = Vec::new();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        let existing =
            existing_documents.remove(&(document.collection, owner.to_owned(), id.to_owned()));
        let changed = existing
            .as_ref()
            .is_none_or(|(_, current)| current != &document.update);
        let created = existing.is_none();
        if document.collection == Collection::InferenceBackend {
            let backend: crate::InferenceBackend = serde_json::from_value(document.update.clone())?;
            backend.validate()?;
            let (metadata, credential) = super::inference_backend::prepare_backend_metadata_in_txn(
                txn,
                backend,
                existing.as_ref().map(|(_, value)| value),
            )
            .await?;
            let projected = serde_json::to_value(metadata.clone().into_backend()?)?;
            let normalized =
                super::config_projection(Collection::InferenceBackend, Some(&projected))?
                    .1
                    .context("backend metadata projection missing")?;
            if credential.is_some()
                || existing
                    .as_ref()
                    .is_none_or(|(_, value)| value != &normalized)
            {
                crate::OpenAiWireApi::warn_if_ignored(
                    metadata.provider_kind,
                    metadata.openai_wire_api,
                    &metadata.backend_id,
                );
                super::inference_backend::write_backend_metadata_in_txn(
                    txn,
                    &metadata,
                    credential.as_ref(),
                )
                .await?;
            }
            counts.increment(document.collection);
            continue;
        }
        let previous = existing.as_ref().map(|(_, current)| current.clone());
        if created && matches!(document.collection, Collection::Node | Collection::Agent) {
            if txn.actor_did().is_none() {
                return Err(super::ConfigClientError::RelationshipActorRequired.into());
            }
            identity::Did::new(if document.collection == Collection::Node {
                owner
            } else {
                id
            })?;
        }
        let name = document.collection.graphql_type();
        let (mutation, input) = if let Some((doc_id, current)) = existing {
            if document.collection == Collection::Node {
                validate_node_replacement(&current, &document.update)?;
            }
            let update = if txn.client_signed_configuration() {
                super::client_signed_agent::configuration_replacement_patch(
                    document.collection,
                    &current,
                    &document.update,
                )?
            } else {
                document.update.clone()
            };
            (
                format!(
                    r#"mutation($input: {name}MutationInputArg!) {{ update_{name}(docID: "{}", input: $input) {{ _docID }} }}"#,
                    escape_graphql_string(&doc_id)
                ),
                update,
            )
        } else {
            (
                format!(
                    "mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"
                ),
                mint_recreate_identity(&document.add),
            )
        };
        match document.collection {
            Collection::DatastoreToolSurface => {
                validate_output_obligation_count_fields(txn, &input, &mut introspected).await?;
            }
            Collection::EventSource => {
                validate_event_source_live_fields(txn, &input, &mut introspected).await?;
            }
            _ => {}
        }
        if changed {
            let response = txn
                .execute_desired_state_mutation(
                    &mutation,
                    &serde_json::json!({"input": input}),
                    document.collection,
                    created,
                )
                .await?;
            if !created
                && response["data"][format!("update_{name}")]
                    .as_array()
                    .is_some_and(Vec::is_empty)
            {
                return Err(super::ConfigClientError::ConfigurationMutationNotApplied {
                    collection: name,
                    node_did: owner.to_owned(),
                    id: id.to_owned(),
                }
                .into());
            }
            let doc_id = extract_mutation_doc_id(&response, name)?;
            if matches!(
                document.collection,
                Collection::Agent
                    | Collection::AgentContext
                    | Collection::Task
                    | Collection::Compaction
                    | Collection::InferenceProfile
                    | Collection::InferenceExecution
                    | Collection::Tools
            ) {
                selection_changes.push(selection::Change {
                    collection: document.collection,
                    node_did: owner.to_owned(),
                    doc_id: doc_id.clone(),
                    before: previous,
                    after: document.update.clone(),
                });
            }
            if created {
                match document.collection {
                    Collection::Node => {
                        txn.queue_relationship_grant(ConfigRelationshipGrant::Actor {
                            collection: name.to_owned(),
                            doc_id,
                            relation: "runtime_reader".to_owned(),
                            target: owner.to_owned(),
                        })?;
                    }
                    Collection::Agent => {
                        created_agents.push((owner.to_owned(), id.to_owned(), doc_id))
                    }
                    _ if gents_protocol::schemas::identity_policy_resource(name)
                        == Some("configuration") =>
                    {
                        if document.collection == Collection::CallbackBinding {
                            let binding: crate::documents::CallbackBinding =
                                serde_json::from_value(document.update.clone())?;
                            created_callback_bindings.push((
                                owner.to_owned(),
                                doc_id.clone(),
                                binding,
                            ));
                        }
                        created_configuration.push((owner.to_owned(), name, doc_id));
                    }
                    _ => {}
                }
            }
        }
        counts.increment(document.collection);
    }
    for (node_did, collection, doc_id) in created_configuration {
        let node_doc = configuration_node_doc(txn, &node_did, binding).await?;
        txn.add_resource_relationship(collection, &doc_id, "parent", "nodes", &node_doc)
            .await?;
    }
    for (node_did, binding_doc, binding) in created_callback_bindings {
        let agent = identity::Did::new(&binding.agent_did)?;
        let (callback_doc, _) =
            read::read_record(txn, Collection::Callback, &node_did, &binding.callback_id)
                .await?
                .context("selected Callback is unavailable")?;
        txn.add_actor_relationship("CallbackBinding", &binding_doc, "reader", &agent)
            .await?;
        txn.add_actor_relationship("Callback", &callback_doc, "reader", &agent)
            .await?;
    }
    for (node_did, agent_did, agent_doc) in created_agents {
        let node_doc = configuration_node_doc(txn, &node_did, binding).await?;
        if txn.managed_agent_signer().is_some() {
            txn.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
                .await?;
            txn.add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &identity::Did::new(&agent_did)?,
            )
            .await?;
        } else {
            txn.queue_relationship_grant(ConfigRelationshipGrant::Resource {
                collection: "Agent".to_owned(),
                doc_id: agent_doc.clone(),
                relation: "parent".to_owned(),
                target_resource: "nodes".to_owned(),
                target_doc_id: node_doc,
            })?;
            txn.queue_relationship_grant(ConfigRelationshipGrant::Actor {
                collection: "Agent".to_owned(),
                doc_id: agent_doc,
                relation: "executor".to_owned(),
                target: agent_did.clone(),
            })?;
        }
        let bound = |collection: Collection| {
            gents_protocol::schemas::IDENTITY_POLICY_BINDINGS
                .iter()
                .any(|(name, resource)| {
                    *name == collection.graphql_type() && *resource == "configuration"
                })
        };
        let (_, agent) = read::read_record(txn, Collection::Agent, &node_did, &agent_did)
            .await?
            .context("created Agent disappeared during configuration publication")?;
        if let Some(context_id) = agent["context_id"].as_str() {
            let (context_doc, context) =
                read::read_record(txn, Collection::AgentContext, &node_did, context_id)
                    .await?
                    .context("selected AgentContext is unavailable")?;
            if bound(Collection::AgentContext) && !selection::context_hierarchy_enabled(bound) {
                txn.add_actor_relationship(
                    Collection::AgentContext.graphql_type(),
                    &context_doc,
                    "reader",
                    &identity::Did::new(&agent_did)?,
                )
                .await?;
            }
            if let Some(tools_id) = context["tools_id"].as_str() {
                let (tools_doc, _) = read::read_record(txn, Collection::Tools, &node_did, tools_id)
                    .await?
                    .context("selected Tools is unavailable")?;
                if bound(Collection::Tools) && !selection::context_hierarchy_enabled(bound) {
                    txn.add_actor_relationship(
                        Collection::Tools.graphql_type(),
                        &tools_doc,
                        "reader",
                        &identity::Did::new(&agent_did)?,
                    )
                    .await?;
                }
            }
        }
    }
    selection::apply_selection_changes(txn, &selection_changes, |collection| {
        gents_protocol::schemas::IDENTITY_POLICY_BINDINGS
            .iter()
            .any(|(name, resource)| {
                *name == collection.graphql_type() && *resource == "configuration"
            })
    })
    .await?;
    if gents_protocol::schemas::identity_policy_resource("Task") == Some("configuration") {
        task_selection::apply_task_selections(txn, &selection_changes).await?;
    }
    let removal_identities = plan
        .removals()
        .iter()
        .map(|(collection, owner, id)| (*collection, owner.clone(), id.clone()))
        .collect::<Vec<_>>();
    let mut existing_removals = read_identity_records(txn, &removal_identities).await?;
    for (collection, owner, id) in plan.removals() {
        if let Some((doc_id, _)) =
            existing_removals.remove(&(*collection, owner.clone(), id.clone()))
        {
            if *collection == Collection::InferenceBackend {
                super::inference_backend::remove_backend_metadata_in_txn(txn, &doc_id, owner, id)
                    .await?;
            } else {
                txn.delete_configuration_document(collection.graphql_type(), &doc_id)
                    .await?;
            }
        }
    }
    event_grants::grant_changes(txn, event_grants).await?;
    workspace_selection::apply(txn, workspace_selection).await?;
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        match document.collection {
            Collection::EventSource => {
                super::event_source_cursor::seed_referencing_consumers(txn, owner, id).await?;
            }
            Collection::Trigger => {
                let trigger: crate::documents::Trigger =
                    serde_json::from_value(document.update.clone())?;
                if matches!(
                    trigger.source,
                    crate::documents::TriggerSource::Event { .. }
                ) {
                    let consumer = gents_protocol::event_delivery::EventConsumer::Trigger {
                        trigger_id: id.to_owned(),
                    };
                    super::event_source_cursor::load_or_seed(txn, owner, &consumer).await?;
                }
            }
            Collection::CallbackBinding => {
                let consumer = gents_protocol::event_delivery::EventConsumer::CallbackBinding {
                    binding_id: id.to_owned(),
                };
                super::event_source_cursor::load_or_seed(txn, owner, &consumer).await?;
            }
            _ => {}
        }
    }
    let mut owners: BTreeSet<_> = plan
        .documents()
        .iter()
        .map(|document| {
            document_identity(document.collection, &document.add).map(|(owner, _)| owner)
        })
        .collect::<Result<_>>()?;
    owners.extend(plan.removals().iter().map(|(_, owner, _)| owner.as_str()));
    let mut introspected = IntrospectedFields::new();
    for owner in owners {
        let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
        references.validate_affected(&changed_configuration_keys(plan, owner)?)?;
        validate_outcome_source_fields(txn, plan, owner, &references, &mut introspected).await?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
        validate_trigger_document_fields(txn, plan, owner, &references, &mut introspected).await?;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
