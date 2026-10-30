use super::plan::{document_identity, validate_node_replacement, DesiredStateApplyPlan};
use super::schema_fields::{
    declared_fields, prefetch_declared_fields, validate_event_source_live_fields,
    validate_output_obligation_count_fields, IntrospectedFields,
};
use super::ConfigApplyTxn;
use crate::defra_query::SchemaField;
use crate::Collection;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) async fn validate_desired_state_plan(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let mut owners = BTreeSet::new();
    for document in plan.documents() {
        owners.insert(document_identity(document.collection, &document.add)?.0);
    }
    owners.extend(plan.removals().iter().map(|(_, owner, _)| owner.as_str()));
    let mut introspected = IntrospectedFields::new();
    prefetch_declared_fields(txn, plan, &mut introspected).await?;
    for owner in owners {
        let retained = crate::ConfigReferences::load_in_txn(txn, owner).await?;
        let mut candidate: BTreeMap<_, _> = retained
            .documents()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for document in plan.documents() {
            let (document_owner, id) = document_identity(document.collection, &document.add)?;
            if document_owner != owner {
                continue;
            }
            let key = (document.collection, id.to_owned());
            let replacement = if let Some(current) = candidate.get(&key) {
                if document.collection == Collection::Node {
                    validate_node_replacement(current, &document.update)?;
                }
                &document.update
            } else {
                &document.add
            };
            match document.collection {
                Collection::DatastoreToolSurface => {
                    validate_output_obligation_count_fields(txn, replacement, &mut introspected)
                        .await?;
                }
                Collection::EventSource => {
                    validate_event_source_live_fields(txn, replacement, &mut introspected).await?;
                }
                _ => {}
            }
            candidate.insert(key, replacement.clone());
        }
        for (collection, document_owner, id) in plan.removals() {
            if document_owner == owner {
                candidate.remove(&(*collection, id.clone()));
            }
        }
        let references = crate::ConfigReferences::from_documents(
            owner,
            candidate
                .into_iter()
                .map(|((collection, _), value)| (collection, value)),
        )?;
        references.validate()?;
        validate_outcome_source_fields(txn, plan, owner, &references, &mut introspected).await?;
        validate_advertised_profiles(txn, plan, owner, &references).await?;
        validate_trigger_document_fields(txn, plan, owner, &references, &mut introspected).await?;
    }
    Ok(())
}

pub(super) async fn validate_trigger_document_fields(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    owner: &str,
    references: &crate::ConfigReferences,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let mut written = BTreeSet::new();
    for document in plan.documents() {
        let (document_owner, id) = document_identity(document.collection, &document.add)?;
        if document_owner == owner {
            written.insert((document.collection, id.to_owned()));
        }
    }
    if written.is_empty() {
        return Ok(());
    }
    let lookup = |collection: Collection, id: &str| {
        references
            .documents()
            .find(|((kind, key), _)| *kind == collection && key == id)
            .map(|(_, value)| value.clone())
    };
    let triggers = references
        .documents()
        .filter(|((collection, _), _)| *collection == Collection::Trigger)
        .map(|(_, value)| serde_json::from_value::<crate::document_config::Trigger>(value.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for trigger in triggers {
        let crate::document_config::TriggerSource::Event { event_source_id } = &trigger.source
        else {
            continue;
        };
        if !written.contains(&(Collection::Trigger, trigger.trigger_id.clone()))
            && !written.contains(&(Collection::Task, trigger.task_id.clone()))
            && !written.contains(&(Collection::EventSource, event_source_id.clone()))
        {
            continue;
        }
        let (Some(task), Some(source)) = (
            lookup(Collection::Task, &trigger.task_id),
            lookup(Collection::EventSource, event_source_id),
        ) else {
            continue;
        };
        let task: crate::document_config::Task = serde_json::from_value(task)?;
        let source: crate::document_config::EventSource = serde_json::from_value(source)?;
        let Some(fields) = declared_fields(txn, &source.source_collection, introspected).await?
        else {
            continue;
        };
        validate_event_trigger_document_fields(&trigger, &task, &source, fields)?;
    }
    Ok(())
}

fn event_trigger_templates<'a>(
    trigger: &'a crate::document_config::Trigger,
    task: &'a crate::document_config::Task,
) -> [(&'static str, Option<&'a str>); 3] {
    [
        ("prompt_template", Some(task.prompt_template.as_str())),
        (
            "goal_objective_template",
            task.goal_objective_template.as_deref(),
        ),
        (
            "session_id_template",
            trigger.session_id_template.as_deref(),
        ),
    ]
}

pub fn validate_event_trigger_document_fields(
    trigger: &crate::document_config::Trigger,
    task: &crate::document_config::Task,
    source: &crate::document_config::EventSource,
    declared: &BTreeMap<String, SchemaField>,
) -> Result<()> {
    for (field, template) in event_trigger_templates(trigger, task) {
        let Some(template) =
            template.filter(|template| !crate::template::template_guards_undefined(template))
        else {
            continue;
        };
        for reference in crate::template::parse_template_for_validation(template)? {
            let Some(name) = reference
                .path
                .get(1)
                .filter(|_| reference.root() == Some("doc"))
            else {
                continue;
            };
            let own_field = |name: &String| {
                !name.starts_with('_')
                    && !crate::defra_query::schema::is_aggregate_pseudo_field(name)
            };
            let resolved_native_route_field = task.emit_outcome
                && source.group.is_none()
                && matches!(
                    source.source_collection.as_str(),
                    "CallbackResult" | "WorkspaceReceipt"
                )
                && matches!(name.as_str(), "handoff_id" | "reply_session_id" | "attempt");
            if name.starts_with('_')
                || declared.contains_key(name) && own_field(name)
                || resolved_native_route_field
            {
                continue;
            }
            anyhow::bail!(
                "trigger {} {field} references doc.{name}, but {} has no field {name:?}; its fields are {}",
                trigger.trigger_id,
                source.source_collection,
                declared
                    .keys()
                    .filter(|name| own_field(name))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

pub fn event_trigger_document_field_names(
    trigger: &crate::document_config::Trigger,
    task: &crate::document_config::Task,
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for (_, template) in event_trigger_templates(trigger, task) {
        let Some(template) =
            template.filter(|template| !crate::template::template_guards_undefined(template))
        else {
            continue;
        };
        for reference in crate::template::parse_template_for_validation(template)? {
            if let Some(name) = reference
                .path
                .get(1)
                .filter(|_| reference.root() == Some("doc"))
            {
                names.insert(name.clone());
            }
        }
    }
    Ok(names)
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct OutcomeSourceSchemaError {
    pub collection: String,
    pub missing_handoff: bool,
    message: String,
}

pub(super) async fn validate_outcome_source_fields(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    owner: &str,
    references: &crate::ConfigReferences,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let mut written = BTreeSet::new();
    for document in plan.documents() {
        let (document_owner, id) = document_identity(document.collection, &document.add)?;
        if document_owner == owner {
            written.insert((document.collection, id.to_owned()));
        }
    }
    for (trigger_id, task_id, event_source_id, collection, per_document) in
        references.outcome_event_deliveries()
    {
        if !written.contains(&(Collection::Trigger, trigger_id.clone()))
            && !written.contains(&(Collection::Task, task_id.clone()))
            && !written.contains(&(Collection::EventSource, event_source_id.clone()))
        {
            continue;
        }
        let Some(fields) = declared_fields(txn, &collection, introspected).await? else {
            continue;
        };
        let declared = fields.get("handoff_id");
        if declared.is_none()
            && per_document
            && matches!(collection.as_str(), "CallbackResult" | "WorkspaceReceipt")
        {
            continue;
        }
        let message = match declared {
            Some(declared) if declared.named_type() == "String" => continue,
            Some(declared) => format!(
                "Trigger {trigger_id} delivers {collection} to Task {task_id}, which sets emit_outcome, but {collection}.handoff_id is {}, not String; a FireOutcome copies the delivered document's handoff_id. Inspect the collection with schema before changing its field type; preserve existing data",
                declared.type_name
            ),
            None => format!(
                "Trigger {trigger_id} delivers {collection} to Task {task_id}, which sets emit_outcome, but {collection} has no handoff_id field; a FireOutcome copies the delivered document's handoff_id, so every fire would be refused. Use the schema tool to add handoff_id: String (collection update), then update the write tool to populate it. Existing rows need meaningful values before delivery. Preserve Tasks, Triggers and sources; deleting them cannot repair the schema"
            ),
        };
        return Err(OutcomeSourceSchemaError {
            collection,
            missing_handoff: declared.is_none(),
            message,
        }
        .into());
    }
    Ok(())
}

pub(super) async fn validate_advertised_profiles(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    owner: &str,
    references: &crate::ConfigReferences,
) -> Result<()> {
    let mut profiles = BTreeSet::new();
    for document in plan.documents() {
        let (document_owner, id) = document_identity(document.collection, &document.add)?;
        if document_owner != owner {
            continue;
        }
        match document.collection {
            Collection::InferenceProfile => {
                profiles.insert(id.to_owned());
            }
            Collection::InferenceBackend => {
                profiles.extend(references.profiles_on_backend(id));
            }
            _ => {}
        }
    }
    for profile_id in profiles {
        let Some((profile, backend)) = references.profile_with_backend(&profile_id)? else {
            continue;
        };
        let observation = crate::inference::backends::registry::lookup_backend_observation_in_txn(
            txn,
            owner,
            &backend.backend_id,
        )
        .await?;
        crate::inference::resolved::advertised_model_for_profile(
            &backend,
            &profile,
            observation.as_ref(),
        )?;
    }
    Ok(())
}
