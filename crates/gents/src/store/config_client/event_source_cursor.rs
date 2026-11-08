use super::ConfigApplyTxn;
use crate::store::graphql::escape_graphql_string;
use anyhow::{Context, Result};
use gents_protocol::event_delivery::EventConsumer;
use gents_protocol::trigger_delivery::EventSourceCursor;
use serde_json::json;

/// Receiving-node offsets must not replicate with shared EventSource configuration.
pub(crate) struct CursorRecord {
    pub doc_id: String,
    pub cursor: EventSourceCursor,
}

/// The consumer configuration a checkpoint is admitted against, resolved by
/// the documents owner inside the same transaction as the journal reads.
pub(crate) struct ResolvedConsumerBinding {
    pub agent_did: String,
    pub enabled: bool,
    pub serial: bool,
    pub filter: Option<String>,
}

fn cursor_key(owner: &str, consumer: &EventConsumer, collection: &str) -> String {
    match consumer {
        EventConsumer::Trigger { trigger_id } => {
            gents_protocol::trigger_delivery::durable_fire_key(
                "arrival-cursor",
                &[owner, trigger_id, collection],
            )
        }
        EventConsumer::CallbackBinding { binding_id } => {
            gents_protocol::trigger_delivery::durable_fire_key(
                "callback-arrival-cursor",
                &[owner, binding_id, collection],
            )
        }
    }
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

/// The caller has checked that no cursor exists and resolved
/// `writer_agent_did`, the consumer's selected Agent, in this transaction.
pub(crate) async fn seed_for_source(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    writer_agent_did: &str,
    node_binding: Option<&super::ConfigurationNodeBinding>,
) -> Result<CursorRecord> {
    let key = cursor_key(owner, consumer, collection);
    let schema = txn
        .execute(&super::schema_introspection::introspection_query(
            collection,
        )?)
        .await?;
    let after =
        if super::schema_introspection::parse_collection_schema(schema.get("data")).is_some() {
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
            "0".to_owned()
        };
    let cursor = EventSourceCursor {
        cursor_key: key,
        owner_did: owner.into(),
        consumer: consumer.clone(),
        source_collection: collection.into(),
        after,
    };
    let response = txn.execute_client_signed_create(
        "mutation($input: EventSourceCursorMutationInputArg!) {create_EventSourceCursor(input: $input) {_docID}}",
        &json!({"input": cursor}), gents_protocol::schemas::EVENT_SOURCE_CURSOR,
    ).await?;
    let doc_id = crate::store::graphql::created_doc_id(&response, "EventSourceCursor")?;
    let node_doc = super::configuration_node_doc(txn, owner, node_binding).await?;
    let agent = identity::Did::new(writer_agent_did)?;
    txn.add_resource_relationship("EventSourceCursor", &doc_id, "parent", "nodes", &node_doc)
        .await?;
    txn.add_actor_relationship("EventSourceCursor", &doc_id, "writer", &agent)
        .await?;
    Ok(CursorRecord { doc_id, cursor })
}

pub(crate) async fn load_or_seed_for_source_as(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    writer_agent_did: &str,
    node_binding: Option<&super::ConfigurationNodeBinding>,
) -> Result<CursorRecord> {
    if let Some(record) = load_for_source(txn, owner, consumer, collection).await? {
        return Ok(record);
    }
    seed_for_source(
        txn,
        owner,
        consumer,
        collection,
        writer_agent_did,
        node_binding,
    )
    .await
}

pub(crate) async fn persist_checkpoint(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    expected_collection: &str,
    after: &str,
    writer_agent_did: &str,
) -> Result<()> {
    let record = load_or_seed_for_source_as(
        txn,
        owner,
        consumer,
        expected_collection,
        writer_agent_did,
        None,
    )
    .await?;
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
/// transaction as the persisted binding, filter, receipts and checkpoint.
/// The caller has resolved `binding` in this transaction and checked that its
/// selected Agent is the actor and its source is this ungrouped collection.
pub(crate) async fn checkpoint_prefix_for_binding(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    collection: &str,
    through: &str,
    legacy_serial_busy: bool,
    binding: &ResolvedConsumerBinding,
) -> Result<bool> {
    let ResolvedConsumerBinding {
        enabled,
        serial,
        filter,
        agent_did,
        ..
    } = binding;
    let (enabled, serial) = (*enabled, *serial);
    crate::store::graphql::validate_collection_identifier(collection)?;
    let record =
        load_or_seed_for_source_as(txn, owner, consumer, collection, agent_did, None).await?;
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
            if admitted_arrival(txn, owner, consumer, collection, doc_id).await? {
                continue;
            }
            if !enabled {
                return Ok(false);
            }
            let filter = crate::store::graphql::selection_filter(
                filter.as_deref(),
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
    persist_checkpoint(txn, owner, consumer, collection, through, agent_did).await?;
    Ok(true)
}

async fn admitted_arrival(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    source_collection: &str,
    source_doc_id: &str,
) -> Result<bool> {
    let trigger_id = match consumer {
        EventConsumer::Trigger { trigger_id } => trigger_id.as_str(),
        EventConsumer::CallbackBinding { binding_id } => {
            return admitted_callback_arrival(
                txn,
                owner,
                binding_id,
                source_collection,
                source_doc_id,
            )
            .await;
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
        "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}, limit: 2) {{ _docID }} }}",
        escape_graphql_string(owner), escape_graphql_string(&request_id))).await?;
    Ok(request["data"]["AgentRequest"]
        .as_array()
        .context("request query omitted rows")?
        .len()
        == 1)
}

async fn admitted_callback_arrival(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    binding_id: &str,
    source_collection: &str,
    source_doc_id: &str,
) -> Result<bool> {
    let key = gents_protocol::trigger_delivery::idempotency_key(
        binding_id,
        source_collection,
        source_doc_id,
    );
    let response = txn
        .execute(&format!(
            "{{ CallbackInvocation(filter: {{owner_node_did: {{_eq: \"{}\"}}, idempotency_key: {{_eq: \"{}\"}}}}, limit: 2) {{ origin claimed_at claimed_callback }} }}",
            escape_graphql_string(owner),
            escape_graphql_string(&key),
        ))
        .await?;
    let rows = response["data"]["CallbackInvocation"]
        .as_array()
        .context("callback receipt query omitted rows")?;
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    let origin: crate::documents::CallbackInvocationOrigin =
        serde_json::from_value(row["origin"].clone()).context("invalid callback receipt origin")?;
    anyhow::ensure!(
        rows.len() == 1
            && matches!(
                origin,
                crate::documents::CallbackInvocationOrigin::Event {
                    binding_id: ref binding,
                    source_collection: ref collection,
                    source_doc_id: ref doc,
                    ..
                } if binding == binding_id && collection == source_collection && doc == source_doc_id
            ),
        "callback receipt disagrees with its arrival"
    );
    Ok(callback_claim_recorded(row))
}

fn callback_claim_recorded(row: &serde_json::Value) -> bool {
    row["claimed_at"].as_str().is_some() && row["claimed_callback"].as_str().is_some()
}

#[cfg(test)]
mod tests;
