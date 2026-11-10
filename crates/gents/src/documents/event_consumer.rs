use anyhow::{Context, Result};
use gents_protocol::canonical::is_retired_reserved_id;
use gents_protocol::event_delivery::EventConsumer;

use crate::documents::{
    Callback, CallbackBinding, ConcurrencyMode, EventSource, MissingReference, Task, Trigger,
    TriggerSource,
};
use crate::store::config_client::event_source_cursor::{
    self, CursorRecord, ResolvedConsumerBinding,
};
use crate::store::config_client::{
    read_desired_state_document_in_txn, read_desired_state_record_in_txn, ConfigApplyTxn,
    ConfigurationNodeBinding,
};
use crate::store::graphql::escape_graphql_string;
use crate::Collection;

async fn trigger_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
) -> Result<(Trigger, EventSource)> {
    let (_, trigger, _, source) = trigger_binding_with_ids(txn, owner, trigger_id).await?;
    Ok((trigger, source))
}

async fn trigger_binding_with_ids(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
) -> Result<(String, Trigger, String, EventSource)> {
    let (trigger_doc, value) =
        read_desired_state_record_in_txn(txn, Collection::Trigger, owner, trigger_id)
            .await?
            .context("cursor trigger disappeared")?;
    let trigger: Trigger = serde_json::from_value(value)?;
    let TriggerSource::Event { event_source_id } = &trigger.source else {
        anyhow::bail!("arrival cursor requires an event trigger")
    };
    let (source_doc, source) =
        read_desired_state_record_in_txn(txn, Collection::EventSource, owner, event_source_id)
            .await?
            .ok_or_else(|| MissingReference {
                collection: Collection::Trigger,
                id: trigger_id.to_owned(),
                field: "source.event_source_id".into(),
                target: Collection::EventSource,
                target_id: event_source_id.clone(),
                node_did: owner.to_owned(),
            })?;
    Ok((
        trigger_doc,
        trigger,
        source_doc,
        serde_json::from_value(source)?,
    ))
}

struct ResolvedConsumer {
    enabled: bool,
    serial: bool,
    source: EventSource,
    agent_did: String,
}

async fn trigger_agent(txn: &ConfigApplyTxn<'_>, owner: &str, trigger: &Trigger) -> Result<String> {
    Ok(trigger_task(txn, owner, trigger).await?.agent_did)
}

async fn trigger_task(txn: &ConfigApplyTxn<'_>, owner: &str, trigger: &Trigger) -> Result<Task> {
    Ok(trigger_task_with_id(txn, owner, trigger).await?.1)
}

async fn trigger_task_with_id(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger: &Trigger,
) -> Result<(String, Task)> {
    let (doc, task) =
        read_desired_state_record_in_txn(txn, Collection::Task, owner, &trigger.task_id)
            .await?
            .context("trigger Task is unavailable")?;
    Ok((doc, serde_json::from_value(task)?))
}

async fn callback_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    binding_id: &str,
) -> Result<(CallbackBinding, EventSource)> {
    let value =
        read_desired_state_document_in_txn(txn, Collection::CallbackBinding, owner, binding_id)
            .await?
            .context("cursor callback binding disappeared")?;
    let binding: CallbackBinding = serde_json::from_value(value)?;
    let source = read_desired_state_document_in_txn(
        txn,
        Collection::EventSource,
        owner,
        &binding.event_source_id,
    )
    .await?
    .ok_or_else(|| MissingReference {
        collection: Collection::CallbackBinding,
        id: binding_id.to_owned(),
        field: "event_source_id".into(),
        target: Collection::EventSource,
        target_id: binding.event_source_id.clone(),
        node_did: owner.to_owned(),
    })?;
    Ok((binding, serde_json::from_value(source)?))
}

async fn event_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
) -> Result<ResolvedConsumer> {
    match consumer {
        EventConsumer::Trigger { trigger_id } => {
            let (trigger, source) = trigger_binding(txn, owner, trigger_id).await?;
            let agent_did = trigger_agent(txn, owner, &trigger).await?;
            Ok(ResolvedConsumer {
                enabled: trigger.enabled,
                serial: trigger.concurrency == Some(ConcurrencyMode::Serial),
                source,
                agent_did,
            })
        }
        EventConsumer::CallbackBinding { binding_id } => {
            let (binding, source) = callback_binding(txn, owner, binding_id).await?;
            let callback = read_desired_state_document_in_txn(
                txn,
                Collection::Callback,
                owner,
                &binding.callback_id,
            )
            .await?
            .map(serde_json::from_value::<Callback>)
            .transpose()?;
            Ok(ResolvedConsumer {
                enabled: binding.enabled && callback.is_some_and(|callback| callback.enabled),
                serial: false,
                source,
                agent_did: binding.agent_did,
            })
        }
    }
}

async fn consumer_agent(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
) -> Result<String> {
    match consumer {
        EventConsumer::Trigger { trigger_id } => {
            let trigger =
                read_desired_state_document_in_txn(txn, Collection::Trigger, owner, trigger_id)
                    .await?
                    .context("cursor Trigger is unavailable")?;
            let trigger: Trigger = serde_json::from_value(trigger)?;
            trigger_agent(txn, owner, &trigger).await
        }
        EventConsumer::CallbackBinding { binding_id } => {
            let binding = read_desired_state_document_in_txn(
                txn,
                Collection::CallbackBinding,
                owner,
                binding_id,
            )
            .await?
            .context("cursor CallbackBinding is unavailable")?;
            Ok(serde_json::from_value::<CallbackBinding>(binding)?.agent_did)
        }
    }
}

#[cfg(test)]
pub(crate) async fn load_or_seed(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
) -> Result<CursorRecord> {
    load_or_seed_with_node_binding(txn, owner, consumer, None).await
}

pub(crate) async fn load_or_seed_with_node_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    node_binding: Option<&ConfigurationNodeBinding>,
) -> Result<CursorRecord> {
    let binding = event_binding(txn, owner, consumer).await?;
    load_or_seed_for_source_with_node_binding(
        txn,
        owner,
        consumer,
        &binding.source.source_collection,
        node_binding,
    )
    .await
}

pub(crate) async fn load_or_seed_for_source(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
) -> Result<CursorRecord> {
    load_or_seed_for_source_with_node_binding(txn, owner, consumer, collection, None).await
}

/// The selected Agent is resolved only when a cursor must be created, so an
/// existing cursor stays readable after its consumer configuration changes.
pub(crate) async fn load_or_seed_for_source_with_node_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    node_binding: Option<&ConfigurationNodeBinding>,
) -> Result<CursorRecord> {
    if let Some(record) =
        event_source_cursor::load_for_source(txn, owner, consumer, collection).await?
    {
        return Ok(record);
    }
    let writer = consumer_agent(txn, owner, consumer).await?;
    event_source_cursor::seed_for_source(txn, owner, consumer, collection, &writer, node_binding)
        .await
}

/// Cursor creation belongs to the configuration transaction, including a
/// source-only replacement while its consumers are disabled. Waiting until
/// runtime reconciliation would skip documents arriving after that replacement.
pub(crate) async fn seed_referencing_consumers(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    event_source_id: &str,
    node_binding: Option<&ConfigurationNodeBinding>,
) -> Result<()> {
    let response = txn
        .execute(&format!(
            "{{ Trigger(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{trigger_id source}} \
               CallbackBinding(filter: {{node_did: {{_eq: \"{owner}\"}}, event_source_id: {{_eq: \"{}\"}}}}) {{binding_id}} }}",
            escape_graphql_string(event_source_id),
            owner = escape_graphql_string(owner),
        ))
        .await?;
    for row in response["data"]["Trigger"]
        .as_array()
        .context("source consumers omitted rows")?
    {
        if row["source"]["kind"] == "event" && row["source"]["event_source_id"] == event_source_id {
            let trigger_id = row["trigger_id"]
                .as_str()
                .context("source consumer lacks trigger ID")?;
            if is_retired_reserved_id(trigger_id) {
                continue;
            }
            let consumer = EventConsumer::Trigger {
                trigger_id: trigger_id.to_owned(),
            };
            load_or_seed_with_node_binding(txn, owner, &consumer, node_binding).await?;
        }
    }
    for row in response["data"]["CallbackBinding"]
        .as_array()
        .context("source callback consumers omitted rows")?
    {
        let binding_id = row["binding_id"]
            .as_str()
            .context("source consumer lacks binding ID")?;
        if is_retired_reserved_id(binding_id) {
            continue;
        }
        let consumer = EventConsumer::CallbackBinding {
            binding_id: binding_id.to_owned(),
        };
        load_or_seed_with_node_binding(txn, owner, &consumer, node_binding).await?;
    }
    Ok(())
}

/// Seed the cursors of the event consumers a configuration plan writes, after
/// its documents and grants are applied.
pub(crate) async fn seed_planned_consumers(
    txn: &ConfigApplyTxn<'_>,
    plan: &crate::store::config_client::DesiredStateApplyPlan,
    node_binding: Option<&ConfigurationNodeBinding>,
) -> Result<()> {
    for document in plan.documents() {
        let (owner, id) =
            crate::store::config_client::document_identity(document.collection, &document.add)?;
        match document.collection {
            Collection::EventSource => {
                seed_referencing_consumers(txn, owner, id, node_binding).await?;
            }
            Collection::Trigger => {
                let trigger: Trigger = serde_json::from_value(document.update.clone())?;
                if matches!(trigger.source, TriggerSource::Event { .. }) {
                    let consumer = EventConsumer::Trigger {
                        trigger_id: id.to_owned(),
                    };
                    load_or_seed_with_node_binding(txn, owner, &consumer, node_binding).await?;
                }
            }
            Collection::CallbackBinding => {
                let consumer = EventConsumer::CallbackBinding {
                    binding_id: id.to_owned(),
                };
                load_or_seed_with_node_binding(txn, owner, &consumer, node_binding).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Cached runtime snapshots cannot authorize new admissions after a committed
/// disable or source replacement.
pub(crate) async fn validate_event_admission(
    txn: &ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
    expected_generation: &serde_json::Value,
) -> Result<()> {
    let (trigger_doc, trigger, source_doc, source) =
        trigger_binding_with_ids(txn, &fire.identity.owner_did, &fire.identity.trigger_id).await?;
    let (task_doc, task) = trigger_task_with_id(txn, &fire.identity.owner_did, &trigger).await?;
    anyhow::ensure!(
        txn.actor_did() == Some(task.agent_did.as_str()),
        "event trigger selected Agent changed before admission"
    );
    crate::documents::observe_unattended_agent_in_txn(
        txn,
        &fire.identity.owner_did,
        &task.agent_did,
    )
    .await?;
    anyhow::ensure!(
        crate::documents::automation::event_admission_generation(
            &source,
            &trigger,
            &task,
            &crate::documents::automation::EventConfigDocIds {
                trigger: &trigger_doc,
                source: &source_doc,
                task: &task_doc,
            }
        )? == *expected_generation,
        "event source, trigger, or Task changed before admission"
    );
    anyhow::ensure!(trigger.enabled, "event trigger is disabled");
    anyhow::ensure!(task.enabled, "event Task is disabled");
    anyhow::ensure!(
        crate::documents::event_trigger::outcome_source_allowed(
            &source.source_collection,
            fire.emit_outcome,
        ),
        "a Task sourced from FireOutcome cannot emit another FireOutcome"
    );
    anyhow::ensure!(
        trigger.task_id == fire.task_id,
        "event trigger Task binding changed before admission"
    );
    anyhow::ensure!(
        if fire.identity.source_collection == "EventGroupState" {
            source.group.is_some()
        } else {
            source.group.is_none() && source.source_collection == fire.identity.source_collection
        },
        "event source binding changed before admission"
    );
    Ok(())
}

pub(crate) async fn exclude_arrival(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    expected_collection: &str,
    after: &str,
) -> Result<()> {
    anyhow::ensure!(
        checkpoint_prefix(txn, owner, consumer, expected_collection, after, false).await?,
        "arrival prefix remains unadmitted"
    );
    Ok(())
}

/// Completeness comes from the native receiving-node journal, within the same
/// transaction as the persisted binding, filter, receipts and checkpoint.
pub(crate) async fn checkpoint_prefix(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    through: &str,
    legacy_serial_busy: bool,
) -> Result<bool> {
    let ResolvedConsumer {
        enabled,
        serial,
        source,
        agent_did,
    } = event_binding(txn, owner, consumer).await?;
    anyhow::ensure!(
        txn.actor_did() == Some(agent_did.as_str()),
        "event consumer selected Agent changed before checkpoint"
    );
    anyhow::ensure!(
        source.source_collection == collection && source.group.is_none(),
        "event source binding changed before checkpoint"
    );
    event_source_cursor::checkpoint_prefix_for_binding(
        txn,
        owner,
        consumer,
        collection,
        through,
        legacy_serial_busy,
        &ResolvedConsumerBinding {
            agent_did,
            enabled,
            serial,
            filter: source.filter,
        },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn acknowledge_fire(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    source_collection: &str,
    _source_doc_id: &str,
    after: &str,
) -> Result<()> {
    anyhow::ensure!(
        checkpoint_prefix(txn, owner, consumer, source_collection, after, false).await?,
        "arrival prefix remains unadmitted"
    );
    Ok(())
}

#[cfg(test)]
pub(crate) async fn advance(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    after: &str,
) -> Result<()> {
    let writer = consumer_agent(txn, owner, consumer).await?;
    event_source_cursor::persist_checkpoint(txn, owner, consumer, collection, after, &writer).await
}
