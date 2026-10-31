pub(super) mod plan;
pub(super) mod projection;
pub(super) mod read;
pub(super) mod schema_fields;
pub(super) mod validate;

use super::{mint_recreate_identity, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::Collection;
use anyhow::Result;
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
    validate_advertised_profiles, validate_outcome_source_fields, validate_trigger_document_fields,
};

/// All writes remain in the caller's transaction. Reference checks run after
/// staged writes so valid cyclic configurations do not depend on write order.
pub async fn apply_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<DesiredStateApplyCounts> {
    ensure_expectations_hold(txn, plan).await?;
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
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        let existing =
            existing_documents.remove(&(document.collection, owner.to_owned(), id.to_owned()));
        let changed = existing
            .as_ref()
            .is_none_or(|(_, current)| current != &document.update);
        let name = document.collection.graphql_type();
        let (mutation, input) = if let Some((doc_id, current)) = existing {
            if document.collection == Collection::Node {
                validate_node_replacement(&current, &document.update)?;
            }
            let update = document.update.clone();
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
                .execute_with_variables(&mutation, &serde_json::json!({"input": input}))
                .await?;
            extract_mutation_doc_id(&response, name)?;
        }
        counts.increment(document.collection);
        if changed && document.collection == Collection::InferenceBackend {
            if let Ok(backend) =
                serde_json::from_value::<crate::InferenceBackend>(document.update.clone())
            {
                crate::OpenAiWireApi::warn_if_ignored(
                    backend.provider_kind,
                    backend.openai_wire_api,
                    &backend.backend_id,
                );
            }
        }
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
            let name = collection.graphql_type();
            txn.execute(&format!(
                r#"mutation {{ delete_{name}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&doc_id)
            ))
            .await?;
        }
    }
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        match document.collection {
            Collection::EventSource => {
                super::event_source_cursor::seed_referencing_consumers(txn, owner, id).await?;
            }
            Collection::Trigger => {
                let trigger: crate::document_config::Trigger =
                    serde_json::from_value(document.update.clone())?;
                if matches!(
                    trigger.source,
                    crate::document_config::TriggerSource::Event { .. }
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
        references.validate()?;
        validate_outcome_source_fields(txn, plan, owner, &references, &mut introspected).await?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
        validate_trigger_document_fields(txn, plan, owner, &references, &mut introspected).await?;
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
