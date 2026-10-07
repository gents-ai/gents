use super::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use anyhow::{Context, Result};
use gents_protocol::event_delivery::EventConsumer;
use gents_protocol::trigger_delivery::EventSourceCursor;
use serde_json::json;

/// Receiving-node offsets must not replicate with shared EventSource configuration.
pub(crate) struct CursorRecord {
    pub doc_id: String,
    pub cursor: EventSourceCursor,
}

async fn trigger_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
) -> Result<(
    crate::document_config::Trigger,
    crate::document_config::EventSource,
)> {
    let value = super::read_desired_state_document_in_txn(
        txn,
        crate::Collection::Trigger,
        owner,
        trigger_id,
    )
    .await?
    .context("cursor trigger disappeared")?;
    let trigger: crate::document_config::Trigger = serde_json::from_value(value)?;
    let crate::document_config::TriggerSource::Event { event_source_id } = &trigger.source else {
        anyhow::bail!("arrival cursor requires an event trigger")
    };
    let source = super::read_desired_state_document_in_txn(
        txn,
        crate::Collection::EventSource,
        owner,
        event_source_id,
    )
    .await?
    .ok_or_else(|| crate::document_config::MissingReference {
        collection: crate::Collection::Trigger,
        id: trigger_id.to_owned(),
        field: "source.event_source_id".into(),
        target: crate::Collection::EventSource,
        target_id: event_source_id.clone(),
        agent_did: owner.to_owned(),
    })?;
    Ok((trigger, serde_json::from_value(source)?))
}

/// The per-document delivery settings one consumer's cursor checkpoints under.
struct ConsumerBinding {
    enabled: bool,
    serial: bool,
    /// The callback whose invocations are a callback binding's receipts.
    callback_id: Option<String>,
    source: crate::document_config::EventSource,
}

async fn callback_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    binding_id: &str,
) -> Result<(
    crate::document_config::CallbackBinding,
    crate::document_config::EventSource,
)> {
    let value = super::read_desired_state_document_in_txn(
        txn,
        crate::Collection::CallbackBinding,
        owner,
        binding_id,
    )
    .await?
    .context("cursor callback binding disappeared")?;
    let binding: crate::document_config::CallbackBinding = serde_json::from_value(value)?;
    let source = super::read_desired_state_document_in_txn(
        txn,
        crate::Collection::EventSource,
        owner,
        &binding.event_source_id,
    )
    .await?
    .ok_or_else(|| crate::document_config::MissingReference {
        collection: crate::Collection::CallbackBinding,
        id: binding_id.to_owned(),
        field: "event_source_id".into(),
        target: crate::Collection::EventSource,
        target_id: binding.event_source_id.clone(),
        agent_did: owner.to_owned(),
    })?;
    Ok((binding, serde_json::from_value(source)?))
}

async fn event_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
) -> Result<ConsumerBinding> {
    match consumer {
        EventConsumer::Trigger { trigger_id } => {
            let (trigger, source) = trigger_binding(txn, owner, trigger_id).await?;
            Ok(ConsumerBinding {
                enabled: trigger.enabled,
                serial: trigger.concurrency
                    == Some(crate::document_config::ConcurrencyMode::Serial),
                callback_id: None,
                source,
            })
        }
        EventConsumer::CallbackBinding { binding_id } => {
            let (binding, source) = callback_binding(txn, owner, binding_id).await?;
            let callback = super::read_desired_state_document_in_txn(
                txn,
                crate::Collection::Callback,
                owner,
                &binding.callback_id,
            )
            .await?
            .map(serde_json::from_value::<crate::document_config::Callback>)
            .transpose()?;
            Ok(ConsumerBinding {
                enabled: binding.enabled && callback.is_some_and(|callback| callback.enabled),
                serial: false,
                callback_id: Some(binding.callback_id),
                source,
            })
        }
    }
}

fn cursor_key(owner: &str, consumer: &EventConsumer, collection: &str) -> String {
    match consumer {
        EventConsumer::Trigger { trigger_id } => crate::trigger_engine::durable_fire_key(
            "arrival-cursor",
            &[owner, trigger_id, collection],
        ),
        EventConsumer::CallbackBinding { binding_id } => crate::trigger_engine::durable_fire_key(
            "callback-arrival-cursor",
            &[owner, binding_id, collection],
        ),
    }
}

pub(crate) async fn load_or_seed(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
) -> Result<CursorRecord> {
    let binding = event_binding(txn, owner, consumer).await?;
    load_or_seed_for_source(txn, owner, consumer, &binding.source.source_collection).await
}

/// Cursor creation belongs to the configuration transaction, including a
/// source-only replacement while its consumers are disabled. Waiting until
/// runtime reconciliation would skip documents arriving after that replacement.
pub(crate) async fn seed_referencing_consumers(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    event_source_id: &str,
) -> Result<()> {
    let response = txn
        .execute(&format!(
            "{{ Trigger(filter: {{agent_did: {{_eq: \"{owner}\"}}}}) {{trigger_id source}} \
               CallbackBinding(filter: {{agent_did: {{_eq: \"{owner}\"}}, event_source_id: {{_eq: \"{}\"}}}}) {{binding_id}} }}",
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
            let consumer = EventConsumer::Trigger {
                trigger_id: trigger_id.to_owned(),
            };
            load_or_seed(txn, owner, &consumer).await?;
        }
    }
    for row in response["data"]["CallbackBinding"]
        .as_array()
        .context("source callback consumers omitted rows")?
    {
        let binding_id = row["binding_id"]
            .as_str()
            .context("source consumer lacks binding ID")?;
        let consumer = EventConsumer::CallbackBinding {
            binding_id: binding_id.to_owned(),
        };
        load_or_seed(txn, owner, &consumer).await?;
    }
    Ok(())
}

/// Cached runtime snapshots cannot authorize new admissions after a committed
/// disable or source replacement. Duplicate receipts bypass this check because
/// their original request already committed before the control-plane change.
pub(crate) async fn validate_event_admission(
    txn: &ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
) -> Result<()> {
    let (trigger, source) =
        trigger_binding(txn, &fire.identity.owner_did, &fire.identity.trigger_id).await?;
    anyhow::ensure!(trigger.enabled, "event trigger is disabled");
    anyhow::ensure!(
        crate::trigger_engine::durable::outcome_source_allowed(
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

pub(crate) async fn load_for_source(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
) -> Result<Option<CursorRecord>> {
    let key = cursor_key(owner, consumer, collection);
    let response = txn.execute(&format!("{{ EventSourceCursor(filter: {{cursor_key: {{_eq: \"{}\"}}}}, limit: 2) {{ _docID cursor_key owner_did consumer source_collection after }} }}", escape_graphql_string(&key))).await?;
    let rows = response["data"]["EventSourceCursor"]
        .as_array()
        .context("cursor query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "arrival cursor must resolve uniquely");
    if let Some(row) = rows.first() {
        let doc_id = row["_docID"]
            .as_str()
            .context("arrival cursor lacks document ID")?
            .to_owned();
        let mut value = row.clone();
        value
            .as_object_mut()
            .context("cursor must be an object")?
            .remove("_docID");
        let cursor: EventSourceCursor =
            serde_json::from_value(value).context("invalid persisted arrival cursor")?;
        anyhow::ensure!(
            cursor.owner_did == owner
                && &cursor.consumer == consumer
                && cursor.source_collection == collection,
            "arrival cursor scope disagrees with its key"
        );
        return Ok(Some(CursorRecord { doc_id, cursor }));
    }
    Ok(None)
}

pub(crate) async fn load_or_seed_for_source(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
) -> Result<CursorRecord> {
    if let Some(record) = load_for_source(txn, owner, consumer, collection).await? {
        return Ok(record);
    }
    let key = cursor_key(owner, consumer, collection);
    let schema = txn
        .execute(&crate::defra_query::schema::introspection_query(
            collection,
        )?)
        .await?;
    let after =
        if crate::defra_query::schema::parse_collection_schema(schema.get("data")).is_some() {
            let response = txn
                .execute(&format!(
            "{{ _documentArrivals(collection: \"{}\", after: \"0\", limit: 1) {{ head }} }}",
            escape_graphql_string(collection)))
                .await?;
            response["data"]["_documentArrivals"]["head"]
                .as_str()
                .context("arrival journal omitted head")?
                .to_owned()
        } else {
            // Configuration may precede collection installation; an absent collection
            // has no arrival history, so registration must retain its first arrival.
            "0".to_owned()
        };
    let cursor = EventSourceCursor {
        cursor_key: key,
        owner_did: owner.into(),
        consumer: consumer.clone(),
        source_collection: collection.into(),
        after,
    };
    let response = txn.execute_with_variables("mutation($input: EventSourceCursorMutationInputArg!) {create_EventSourceCursor(input: $input) {_docID}}", &json!({"input": cursor})).await?;
    let doc_id = crate::graphql::created_doc_id(&response, "EventSourceCursor")?;
    Ok(CursorRecord { doc_id, cursor })
}

async fn persist_checkpoint(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    expected_collection: &str,
    after: &str,
) -> Result<()> {
    let record = load_or_seed_for_source(txn, owner, consumer, expected_collection).await?;
    anyhow::ensure!(
        record.cursor.source_collection == expected_collection,
        "arrival source changed while fire was admitted"
    );
    let old: u64 = record
        .cursor
        .after
        .parse()
        .context("invalid saved arrival position")?;
    let next: u64 = after.parse().context("invalid new arrival position")?;
    if next > old {
        txn.execute(&format!("mutation {{update_EventSourceCursor(docID: \"{}\", input: {{after: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(&record.doc_id), escape_graphql_string(after))).await?;
    }
    Ok(())
}

/// Completeness comes from the native receiving-node journal, within the same
/// transaction as the persisted binding, filter, receipts and checkpoint. Hidden
/// arrivals are exclusions only while enabled; disabled sources retain them.
/// Created-source selection is evaluated at delivery: reliable handoff producers
/// keep payload/filter fields immutable until admitted. A later mutation does
/// not replay a row already excluded by the committed filter snapshot.
/// A legacy-serial busy result may exclude only the next visible arrival, never
/// authorize an arbitrary prefix of unadmitted work.
pub(crate) async fn checkpoint_prefix(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    through: &str,
    legacy_serial_busy: bool,
) -> Result<bool> {
    let ConsumerBinding {
        enabled,
        serial,
        callback_id,
        source,
    } = event_binding(txn, owner, consumer).await?;
    anyhow::ensure!(
        source.source_collection == collection && source.group.is_none(),
        "event source binding changed before checkpoint"
    );
    crate::graphql::validate_collection_identifier(collection)?;
    let record = load_or_seed_for_source(txn, owner, consumer, collection).await?;
    let mut after: u64 = record
        .cursor
        .after
        .parse()
        .context("invalid saved arrival position")?;
    let through_position: u64 = through.parse().context("invalid checkpoint position")?;
    if through_position < after {
        return Ok(false);
    }
    let serial_exclusion = legacy_serial_busy && enabled && serial;
    let mut excluded_busy = false;
    loop {
        let response = txn.execute(&format!(
            "{{ _documentArrivals(collection: \"{}\", after: \"{}\", limit: 128) {{ head next entries {{ cursor docID }} }} }}",
            escape_graphql_string(collection), after)).await?;
        let page = &response["data"]["_documentArrivals"];
        let head: u64 = page["head"]
            .as_str()
            .context("journal omitted head")?
            .parse()?;
        if through_position > head {
            return Ok(false);
        }
        if through_position == after {
            break;
        }
        let next: u64 = page["next"]
            .as_str()
            .context("journal omitted next")?
            .parse()?;
        anyhow::ensure!(
            next > after && next <= head,
            "journal failed to advance a complete prefix"
        );
        let entries = page["entries"]
            .as_array()
            .context("journal omitted entries")?;
        let mut visible = 0u64;
        for entry in entries {
            let position: u64 = entry["cursor"]
                .as_str()
                .context("arrival omitted cursor")?
                .parse()?;
            if position > through_position {
                break;
            }
            anyhow::ensure!(
                position > after && position <= next,
                "arrival outside native page"
            );
            visible += 1;
            let doc_id = entry["docID"]
                .as_str()
                .context("arrival omitted document ID")?;
            if admitted_arrival(
                txn,
                owner,
                consumer,
                callback_id.as_deref(),
                collection,
                doc_id,
            )
            .await?
            {
                continue;
            }
            if !enabled {
                return Ok(false);
            }
            let filter = crate::trigger_engine::event_delivery::selection_filter(
                source.filter.as_deref(),
                Some(("_docID", doc_id)),
            )?;
            let matched = txn
                .execute(&format!(
                    "{{{collection}(filter: {filter}, limit: 1) {{_docID}}}}"
                ))
                .await?;
            let matched = !matched["data"][collection]
                .as_array()
                .context("filter probe omitted rows")?
                .is_empty();
            if !matched {
                continue;
            }
            if serial_exclusion && !excluded_busy && position == through_position {
                excluded_busy = true;
                continue;
            }
            return Ok(false);
        }
        let end = next.min(through_position);
        if !enabled && visible != end - after {
            return Ok(false);
        }
        after = end;
    }
    persist_checkpoint(txn, owner, consumer, collection, through).await?;
    Ok(true)
}

async fn admitted_arrival(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    callback_id: Option<&str>,
    source_collection: &str,
    source_doc_id: &str,
) -> Result<bool> {
    let trigger_id = match consumer {
        EventConsumer::Trigger { trigger_id } => trigger_id.as_str(),
        EventConsumer::CallbackBinding { binding_id } => {
            return admitted_callback_arrival(
                txn,
                owner,
                callback_id.context("callback consumer lacks its callback")?,
                binding_id,
                source_collection,
                source_doc_id,
            )
            .await
        }
    };
    let identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: owner.into(),
        trigger_id: trigger_id.into(),
        source_collection: source_collection.into(),
        source_doc_id: source_doc_id.into(),
    };
    let fire_key = identity.fire_key();
    let request_id = identity.request_id();
    let receipt = txn.execute(&format!(
        "{{ TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}, limit: 2) {{ owner_did trigger_id source_collection source_doc_id request_id }} }}",
        escape_graphql_string(&fire_key))).await?;
    let receipts = receipt["data"]["TriggerFire"]
        .as_array()
        .context("receipt query omitted rows")?;
    if receipts.is_empty() {
        return Ok(false);
    }
    anyhow::ensure!(
        receipts.len() == 1
            && receipts[0]["request_id"] == request_id
            && receipts[0]["owner_did"] == owner
            && receipts[0]["trigger_id"] == trigger_id
            && receipts[0]["source_collection"] == source_collection
            && receipts[0]["source_doc_id"] == source_doc_id,
        "arrival receipt disagrees with canonical identity"
    );
    let request = txn.execute(&format!(
        "{{ AgentRequest(filter: {{agent_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}, limit: 2) {{ _docID }} }}",
        escape_graphql_string(owner), escape_graphql_string(&request_id))).await?;
    Ok(request["data"]["AgentRequest"]
        .as_array()
        .context("request query omitted rows")?
        .len()
        == 1)
}

/// A callback binding's receipt is its event invocation for the arrival. The
/// idempotency key also names the source version, so the receipt is matched on
/// the invocation's origin: a later edit of an admitted document is not a new
/// arrival.
async fn admitted_callback_arrival(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    callback_id: &str,
    binding_id: &str,
    source_collection: &str,
    source_doc_id: &str,
) -> Result<bool> {
    // DefraDB's `_like` wildcard is `%` alone; an identifier containing one
    // only widens the candidates the exact origin match then narrows.
    let prefix = crate::callback::idempotency_key(binding_id, source_doc_id, "");
    let response = txn
        .execute(&format!(
            "{{ CallbackInvocation(filter: {{owner_agent_did: {{_eq: \"{}\"}}, callback_id: {{_eq: \"{}\"}}, idempotency_key: {{_like: \"{}%\"}}}}) {{ origin }} }}",
            escape_graphql_string(owner),
            escape_graphql_string(callback_id),
            escape_graphql_string(&prefix),
        ))
        .await?;
    let rows = response["data"]["CallbackInvocation"]
        .as_array()
        .context("callback receipt query omitted rows")?;
    Ok(rows.iter().any(|row| {
        matches!(
            serde_json::from_value::<crate::document_config::CallbackInvocationOrigin>(row["origin"].clone()),
            Ok(crate::document_config::CallbackInvocationOrigin::Event {
                binding_id: ref binding,
                source_collection: ref collection,
                source_doc_id: ref doc,
                ..
            }) if binding == binding_id && collection == source_collection && doc == source_doc_id
        )
    }))
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
    persist_checkpoint(txn, owner, consumer, collection, after).await
}

#[cfg(test)]
mod tests;
