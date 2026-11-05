use super::{plan::document_identity, read, ConfigApplyTxn};
use crate::Collection;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "event_grants/tests.rs"]
mod tests;

#[derive(Clone, PartialEq, Eq)]
struct Selection {
    agent: String,
    source: Option<String>,
    collection: Option<String>,
    callback: Option<String>,
    module: Option<String>,
}

pub(super) struct ObservedTrigger {
    doc_id: String,
    selected: Option<Selection>,
}

pub(super) struct Capture {
    triggers: BTreeMap<(String, String), Option<ObservedTrigger>>,
    callbacks: BTreeMap<(String, String), Option<Selection>>,
}

async fn source_collection(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    id: &str,
) -> Result<Option<String>> {
    Ok(read::read_record(txn, Collection::EventSource, owner, id)
        .await?
        .and_then(|(_, value)| value["source_collection"].as_str().map(str::to_owned)))
}

async fn observe_callback(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    id: &str,
) -> Result<Option<Selection>> {
    let Some((_, value)) = read::read_record(txn, Collection::CallbackBinding, owner, id).await?
    else {
        return Ok(None);
    };
    let binding: crate::documents::CallbackBinding = serde_json::from_value(value)?;
    let collection = source_collection(txn, owner, &binding.event_source_id).await?;
    let module =
        match read::read_record(txn, Collection::Callback, owner, &binding.callback_id).await? {
            Some((_, value)) => {
                let callback: crate::documents::Callback = serde_json::from_value(value)?;
                match callback.handler {
                    crate::documents::CallbackHandler::Module { module_id } => Some(module_id),
                    _ => None,
                }
            }
            None => None,
        };
    Ok(Some(Selection {
        agent: binding.agent_did,
        source: Some(binding.event_source_id),
        collection,
        callback: Some(binding.callback_id),
        module,
    }))
}

async fn observe(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    id: &str,
) -> Result<Option<ObservedTrigger>> {
    let Some((doc_id, value)) = read::read_record(txn, Collection::Trigger, owner, id).await?
    else {
        return Ok(None);
    };
    let trigger: crate::documents::Trigger = serde_json::from_value(value)?;
    let selected = match read::read_record(txn, Collection::Task, owner, &trigger.task_id).await? {
        Some((_, task)) => {
            let task: crate::documents::Task = serde_json::from_value(task)?;
            let source = match trigger.source {
                crate::documents::TriggerSource::Event { event_source_id } => Some(event_source_id),
                crate::documents::TriggerSource::Schedule { .. } => None,
            };
            let collection = match source.as_deref() {
                Some(id) => source_collection(txn, owner, id).await?,
                None => None,
            };
            Some(Selection {
                agent: task.agent_did,
                source,
                collection,
                callback: None,
                module: None,
            })
        }
        None => None,
    };
    Ok(Some(ObservedTrigger { doc_id, selected }))
}

pub(super) async fn capture(
    txn: &ConfigApplyTxn<'_>,
    plan: &super::plan::DesiredStateApplyPlan,
) -> Result<Capture> {
    let mut triggers = BTreeSet::new();
    let mut callbacks = BTreeMap::new();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        match document.collection {
            Collection::Trigger => {
                triggers.insert((owner.to_owned(), id.to_owned()));
            }
            Collection::CallbackBinding => {
                callbacks.insert(
                    (owner.to_owned(), id.to_owned()),
                    observe_callback(txn, owner, id).await?,
                );
            }
            Collection::Callback => {
                let response = txn.execute(&format!(
                    "{{ CallbackBinding(filter: {{node_did: {{_eq: \"{}\"}}, callback_id: {{_eq: \"{}\"}}}}) {{binding_id}} }}",
                    crate::store::graphql::escape_graphql_string(owner),
                    crate::store::graphql::escape_graphql_string(id),
                )).await?;
                for row in response["data"]["CallbackBinding"]
                    .as_array()
                    .context("Callback consumers omitted bindings")?
                {
                    let binding = row["binding_id"]
                        .as_str()
                        .context("Callback consumer lacks binding ID")?;
                    callbacks.insert(
                        (owner.to_owned(), binding.to_owned()),
                        observe_callback(txn, owner, binding).await?,
                    );
                }
            }
            Collection::Task => {
                let before = read::read_record(txn, Collection::Task, owner, id).await?;
                let changed = before
                    .as_ref()
                    .is_none_or(|(_, value)| value["agent_did"] != document.update["agent_did"]);
                if changed {
                    let response = txn.execute(&format!(
                        "{{ Trigger(filter: {{node_did: {{_eq: \"{}\"}}, task_id: {{_eq: \"{}\"}}}}) {{trigger_id}} }}",
                        crate::store::graphql::escape_graphql_string(owner),
                        crate::store::graphql::escape_graphql_string(id),
                    )).await?;
                    for row in response["data"]["Trigger"]
                        .as_array()
                        .context("Task consumers omitted Trigger rows")?
                    {
                        let trigger_id = row["trigger_id"]
                            .as_str()
                            .context("Task consumer lacks Trigger ID")?;
                        triggers.insert((owner.to_owned(), trigger_id.to_owned()));
                    }
                }
            }
            Collection::EventSource => {
                let before = read::read_record(txn, Collection::EventSource, owner, id).await?;
                let changed = before.as_ref().is_none_or(|(_, value)| {
                    value["source_collection"] != document.update["source_collection"]
                });
                if changed {
                    let response = txn.execute(&format!(
                        "{{ Trigger(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{trigger_id source}} CallbackBinding(filter: {{node_did: {{_eq: \"{owner}\"}}, event_source_id: {{_eq: \"{id}\"}}}}) {{binding_id}} }}",
                        owner = crate::store::graphql::escape_graphql_string(owner),
                        id = crate::store::graphql::escape_graphql_string(id),
                    )).await?;
                    for row in response["data"]["Trigger"]
                        .as_array()
                        .context("EventSource Trigger consumers omitted rows")?
                    {
                        if row["source"]["kind"] == "event"
                            && row["source"]["event_source_id"] == id
                        {
                            triggers.insert((
                                owner.to_owned(),
                                row["trigger_id"]
                                    .as_str()
                                    .context("source consumer lacks Trigger ID")?
                                    .to_owned(),
                            ));
                        }
                    }
                    for row in response["data"]["CallbackBinding"]
                        .as_array()
                        .context("EventSource callback consumers omitted rows")?
                    {
                        let binding = row["binding_id"]
                            .as_str()
                            .context("source consumer lacks CallbackBinding ID")?;
                        callbacks.insert(
                            (owner.to_owned(), binding.to_owned()),
                            observe_callback(txn, owner, binding).await?,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    for (collection, owner, id) in plan.removals() {
        match collection {
            Collection::Trigger => {
                triggers.insert((owner.clone(), id.clone()));
            }
            Collection::CallbackBinding => {
                callbacks.insert(
                    (owner.clone(), id.clone()),
                    observe_callback(txn, owner, id).await?,
                );
            }
            _ => {}
        }
    }
    let mut captured = BTreeMap::new();
    for (owner, id) in triggers {
        let prior = observe(txn, &owner, &id).await?;
        captured.insert((owner, id), prior);
    }
    Ok(Capture {
        triggers: captured,
        callbacks,
    })
}

async fn rebind_cursor(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &gents_protocol::event_delivery::EventConsumer,
    before: Option<&Selection>,
    after: Option<&Selection>,
) -> Result<()> {
    let old = before.and_then(|selection| {
        selection
            .collection
            .as_deref()
            .map(|collection| (selection.agent.as_str(), collection))
    });
    let new = after.and_then(|selection| {
        selection
            .collection
            .as_deref()
            .map(|collection| (selection.agent.as_str(), collection))
    });
    if old == new {
        return Ok(());
    }
    if let Some((agent, collection)) = old {
        if let Some(cursor) = crate::store::config_client::event_source_cursor::load_for_source(
            txn, owner, consumer, collection,
        )
        .await?
        {
            txn.delete_actor_relationship(
                "EventSourceCursor",
                &cursor.doc_id,
                "writer",
                &identity::Did::new(agent)?,
            )
            .await?;
        }
    }
    if let Some((agent, collection)) = new {
        if let Some(cursor) = crate::store::config_client::event_source_cursor::load_for_source(
            txn, owner, consumer, collection,
        )
        .await?
        {
            txn.add_actor_relationship(
                "EventSourceCursor",
                &cursor.doc_id,
                "writer",
                &identity::Did::new(agent)?,
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn grant_changes(txn: &ConfigApplyTxn<'_>, captured: Capture) -> Result<()> {
    for ((owner, id), prior) in captured.triggers {
        let current = observe(txn, &owner, &id).await?;
        let Some(current) = current else {
            rebind_cursor(
                txn,
                &owner,
                &gents_protocol::event_delivery::EventConsumer::Trigger { trigger_id: id },
                prior.as_ref().and_then(|trigger| trigger.selected.as_ref()),
                None,
            )
            .await?;
            continue;
        };
        if prior.is_none() {
            txn.add_actor_relationship(
                "Trigger",
                &current.doc_id,
                "writer",
                &identity::Did::new(&owner)?,
            )
            .await?;
        }
        let prior_selection = prior.as_ref().and_then(|trigger| trigger.selected.as_ref());
        rebind_cursor(
            txn,
            &owner,
            &gents_protocol::event_delivery::EventConsumer::Trigger {
                trigger_id: id.clone(),
            },
            prior_selection,
            current.selected.as_ref(),
        )
        .await?;
        let Some(selected) = current.selected.as_ref() else {
            continue;
        };
        let agent_changed = prior_selection.is_none_or(|before| before.agent != selected.agent);
        if agent_changed {
            if let Some(before) = prior_selection {
                txn.delete_actor_relationship(
                    "Trigger",
                    &current.doc_id,
                    "reader",
                    &identity::Did::new(&before.agent)?,
                )
                .await?;
            }
            txn.add_actor_relationship(
                "Trigger",
                &current.doc_id,
                "reader",
                &identity::Did::new(&selected.agent)?,
            )
            .await?;
        }
        if let Some(source_id) = &selected.source {
            let source_changed = prior_selection.is_none_or(|before| {
                before.source.as_ref() != Some(source_id)
                    || (before.collection.is_none() && selected.collection.is_some())
            });
            if agent_changed || source_changed {
                let (source_doc, _) =
                    read::read_record(txn, Collection::EventSource, &owner, source_id)
                        .await?
                        .context("selected EventSource is unavailable")?;
                txn.add_actor_relationship(
                    "EventSource",
                    &source_doc,
                    "reader",
                    &identity::Did::new(&selected.agent)?,
                )
                .await?;
            }
        }
    }
    for ((owner, id), prior) in captured.callbacks {
        let Some(selected) = observe_callback(txn, &owner, &id).await? else {
            rebind_cursor(
                txn,
                &owner,
                &gents_protocol::event_delivery::EventConsumer::CallbackBinding { binding_id: id },
                prior.as_ref(),
                None,
            )
            .await?;
            continue;
        };
        rebind_cursor(
            txn,
            &owner,
            &gents_protocol::event_delivery::EventConsumer::CallbackBinding {
                binding_id: id.clone(),
            },
            prior.as_ref(),
            Some(&selected),
        )
        .await?;
        let selection_changed = prior.as_ref().is_none_or(|before| {
            before.agent != selected.agent
                || before.source != selected.source
                || (before.collection.is_none() && selected.collection.is_some())
        });
        if selection_changed {
            let source = selected
                .source
                .as_deref()
                .context("callback EventSource is missing")?;
            let (source_doc, _) = read::read_record(txn, Collection::EventSource, &owner, source)
                .await?
                .context("callback EventSource is unavailable")?;
            txn.add_actor_relationship(
                "EventSource",
                &source_doc,
                "reader",
                &identity::Did::new(&selected.agent)?,
            )
            .await?;
        }
        let actor_changed = prior
            .as_ref()
            .is_none_or(|before| before.agent != selected.agent);
        let callback_changed = prior
            .as_ref()
            .is_none_or(|before| before.callback != selected.callback);
        let bound = |collection: Collection| {
            gents_protocol::schemas::IDENTITY_POLICY_BINDINGS
                .iter()
                .any(|(name, resource)| {
                    *name == collection.graphql_type() && *resource == "configuration"
                })
        };
        if prior.is_some() && actor_changed && bound(Collection::CallbackBinding) {
            let (binding_doc, _) = read::read_record(txn, Collection::CallbackBinding, &owner, &id)
                .await?
                .context("selected CallbackBinding is unavailable")?;
            if let Some(before) = prior.as_ref() {
                txn.delete_actor_relationship(
                    "CallbackBinding",
                    &binding_doc,
                    "reader",
                    &identity::Did::new(&before.agent)?,
                )
                .await?;
            }
            txn.add_actor_relationship(
                "CallbackBinding",
                &binding_doc,
                "reader",
                &identity::Did::new(&selected.agent)?,
            )
            .await?;
        }
        if prior.is_some() && (actor_changed || callback_changed) && bound(Collection::Callback) {
            let callback_id = selected
                .callback
                .as_deref()
                .context("selected Callback is missing")?;
            let (callback_doc, _) =
                read::read_record(txn, Collection::Callback, &owner, callback_id)
                    .await?
                    .context("selected Callback is unavailable")?;
            txn.add_actor_relationship(
                "Callback",
                &callback_doc,
                "reader",
                &identity::Did::new(&selected.agent)?,
            )
            .await?;
        }
        let module_changed = prior
            .as_ref()
            .is_none_or(|before| before.module != selected.module);
        if (actor_changed || callback_changed || module_changed)
            && bound(Collection::CallbackModule)
        {
            if let Some(module_id) = selected.module.as_deref() {
                let (module_doc, _) =
                    read::read_record(txn, Collection::CallbackModule, &owner, module_id)
                        .await?
                        .context("selected CallbackModule is unavailable")?;
                txn.add_actor_relationship(
                    "CallbackModule",
                    &module_doc,
                    "reader",
                    &identity::Did::new(&selected.agent)?,
                )
                .await?;
            }
        }
    }
    Ok(())
}
