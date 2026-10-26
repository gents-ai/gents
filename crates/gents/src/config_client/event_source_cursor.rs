use super::{ConfigApplyTxn, ConfigClientError, TransactionFailure};
use crate::graphql::escape_graphql_string;
use gents_protocol::event_delivery::EventConsumer;
use gents_protocol::trigger_delivery::EventSourceCursor;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum EventSourceCursorError {
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    DesiredState(Box<super::desired_state::DesiredStateError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    EventDelivery(Box<crate::trigger_engine::event_delivery::EventDeliveryError>),
    #[error(transparent)]
    Introspection(#[from] crate::defra_query::DefraQueryFailure),
    #[error(transparent)]
    MissingReference(#[from] crate::document_config::MissingReference),
    #[error(transparent)]
    Protocol(#[from] gents_protocol::graphql::GraphqlError),
    #[error("cursor trigger disappeared")]
    TriggerMissing,
    #[error("arrival cursor requires an event trigger")]
    TriggerNotEvent,
    #[error("cursor callback binding disappeared")]
    CallbackBindingMissing,
    #[error("event trigger is disabled")]
    TriggerDisabled,
    #[error("a Task sourced from FireOutcome cannot emit another FireOutcome")]
    FireOutcomeChained,
    #[error("event trigger Task binding changed before admission")]
    TriggerTaskChanged,
    #[error("event source binding changed before admission")]
    SourceBindingChanged,
    #[error("arrival prefix remains unadmitted")]
    PrefixUnadmitted,
    #[error("arrival cursor must resolve uniquely")]
    CursorAmbiguous,
    #[error("cursor query omitted rows")]
    CursorRowsMissing,
    #[error("arrival cursor lacks document ID")]
    CursorDocIdMissing,
    #[error("cursor must be an object")]
    CursorNotObject,
    #[error("invalid persisted arrival cursor")]
    CursorDecode(#[source] serde_json::Error),
    #[error("arrival cursor scope disagrees with its key")]
    CursorScopeMismatch,
    #[error("arrival journal omitted head")]
    ArrivalJournalHeadMissing,
    #[error("invalid saved arrival position")]
    SavedPosition(#[source] std::num::ParseIntError),
    #[error("invalid new arrival position")]
    NewPosition(#[source] std::num::ParseIntError),
    #[error("arrival source changed while fire was admitted")]
    SourceChangedDuringFire,
    #[error("source consumers omitted rows")]
    SourceConsumersMissing,
    #[error("source consumer lacks trigger ID")]
    ConsumerTriggerIdMissing,
    #[error("source callback consumers omitted rows")]
    SourceCallbackConsumersMissing,
    #[error("source consumer lacks binding ID")]
    ConsumerBindingIdMissing,
    #[error("event source binding changed before checkpoint")]
    CheckpointSourceChanged,
    #[error("invalid checkpoint position")]
    CheckpointPosition(#[source] std::num::ParseIntError),
    #[error("journal omitted head")]
    JournalHeadMissing,
    #[error("journal omitted next")]
    JournalNextMissing,
    #[error("journal omitted entries")]
    JournalEntriesMissing,
    #[error("journal failed to advance a complete prefix")]
    JournalNotAdvancing,
    #[error("arrival omitted cursor")]
    ArrivalCursorMissing,
    #[error("arrival outside native page")]
    ArrivalOutsidePage,
    #[error("arrival omitted document ID")]
    ArrivalDocIdMissing,
    #[error("filter probe omitted rows")]
    FilterProbeRowsMissing,
    #[error("receipt query omitted rows")]
    ReceiptRowsMissing,
    #[error("arrival receipt disagrees with canonical identity")]
    ReceiptMismatch,
    #[error("request query omitted rows")]
    RequestRowsMissing,
    #[error("callback receipt query omitted rows")]
    CallbackReceiptRowsMissing,
    #[error("invalid callback receipt origin")]
    CallbackOriginDecode(#[source] serde_json::Error),
    #[error("callback receipt disagrees with its arrival")]
    CallbackReceiptMismatch,
}

impl From<crate::trigger_engine::event_delivery::EventDeliveryError> for EventSourceCursorError {
    fn from(error: crate::trigger_engine::event_delivery::EventDeliveryError) -> Self {
        Self::EventDelivery(Box::new(error))
    }
}

impl From<super::desired_state::DesiredStateError> for EventSourceCursorError {
    fn from(error: super::desired_state::DesiredStateError) -> Self {
        Self::DesiredState(Box::new(error))
    }
}

impl TransactionFailure for EventSourceCursorError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::DesiredState(error) => error.config_client_error(),
            _ => None,
        }
    }
}

type Result<T, E = EventSourceCursorError> = std::result::Result<T, E>;

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
    .ok_or(EventSourceCursorError::TriggerMissing)?;
    let trigger: crate::document_config::Trigger = serde_json::from_value(value)?;
    let crate::document_config::TriggerSource::Event { event_source_id } = &trigger.source else {
        return Err(EventSourceCursorError::TriggerNotEvent);
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
        node_did: owner.to_owned(),
    })?;
    Ok((trigger, serde_json::from_value(source)?))
}

struct ConsumerBinding {
    enabled: bool,
    serial: bool,
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
    .ok_or(EventSourceCursorError::CallbackBindingMissing)?;
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
        node_did: owner.to_owned(),
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
            "{{ Trigger(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{trigger_id source}} \
               CallbackBinding(filter: {{node_did: {{_eq: \"{owner}\"}}, event_source_id: {{_eq: \"{}\"}}}}) {{binding_id}} }}",
            escape_graphql_string(event_source_id),
            owner = escape_graphql_string(owner),
        ))
        .await?;
    for row in response["data"]["Trigger"]
        .as_array()
        .ok_or(EventSourceCursorError::SourceConsumersMissing)?
    {
        if row["source"]["kind"] == "event" && row["source"]["event_source_id"] == event_source_id {
            let trigger_id = row["trigger_id"]
                .as_str()
                .ok_or(EventSourceCursorError::ConsumerTriggerIdMissing)?;
            let consumer = EventConsumer::Trigger {
                trigger_id: trigger_id.to_owned(),
            };
            load_or_seed(txn, owner, &consumer).await?;
        }
    }
    for row in response["data"]["CallbackBinding"]
        .as_array()
        .ok_or(EventSourceCursorError::SourceCallbackConsumersMissing)?
    {
        let binding_id = row["binding_id"]
            .as_str()
            .ok_or(EventSourceCursorError::ConsumerBindingIdMissing)?;
        let consumer = EventConsumer::CallbackBinding {
            binding_id: binding_id.to_owned(),
        };
        load_or_seed(txn, owner, &consumer).await?;
    }
    Ok(())
}

/// Cached runtime snapshots cannot authorize new admissions after a committed
/// disable or source replacement.
pub(crate) async fn validate_event_admission(
    txn: &ConfigApplyTxn<'_>,
    fire: &gents_protocol::trigger_delivery::TriggerFire,
) -> Result<()> {
    let (trigger, source) =
        trigger_binding(txn, &fire.identity.owner_did, &fire.identity.trigger_id).await?;
    if !trigger.enabled {
        return Err(EventSourceCursorError::TriggerDisabled);
    }
    if !crate::trigger_engine::durable::outcome_source_allowed(
        &source.source_collection,
        fire.emit_outcome,
    ) {
        return Err(EventSourceCursorError::FireOutcomeChained);
    }
    if trigger.task_id != fire.task_id {
        return Err(EventSourceCursorError::TriggerTaskChanged);
    }
    let source_matches = if fire.identity.source_collection == "EventGroupState" {
        source.group.is_some()
    } else {
        source.group.is_none() && source.source_collection == fire.identity.source_collection
    };
    if !source_matches {
        return Err(EventSourceCursorError::SourceBindingChanged);
    }
    Ok(())
}

pub(crate) async fn exclude_arrival(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    consumer: &EventConsumer,
    expected_collection: &str,
    after: &str,
) -> Result<()> {
    if !checkpoint_prefix(txn, owner, consumer, expected_collection, after, false).await? {
        return Err(EventSourceCursorError::PrefixUnadmitted);
    }
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
        .ok_or(EventSourceCursorError::CursorRowsMissing)?;
    if rows.len() > 1 {
        return Err(EventSourceCursorError::CursorAmbiguous);
    }
    if let Some(row) = rows.first() {
        let doc_id = row["_docID"]
            .as_str()
            .ok_or(EventSourceCursorError::CursorDocIdMissing)?
            .to_owned();
        let mut value = row.clone();
        value
            .as_object_mut()
            .ok_or(EventSourceCursorError::CursorNotObject)?
            .remove("_docID");
        let cursor: EventSourceCursor =
            serde_json::from_value(value).map_err(EventSourceCursorError::CursorDecode)?;
        if !(cursor.owner_did == owner
            && &cursor.consumer == consumer
            && cursor.source_collection == collection)
        {
            return Err(EventSourceCursorError::CursorScopeMismatch);
        }
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
                .ok_or(EventSourceCursorError::ArrivalJournalHeadMissing)?
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
    if record.cursor.source_collection != expected_collection {
        return Err(EventSourceCursorError::SourceChangedDuringFire);
    }
    let old: u64 = record
        .cursor
        .after
        .parse()
        .map_err(EventSourceCursorError::SavedPosition)?;
    let next: u64 = after.parse().map_err(EventSourceCursorError::NewPosition)?;
    if next > old {
        txn.execute(&format!("mutation {{update_EventSourceCursor(docID: \"{}\", input: {{after: \"{}\"}}) {{_docID}}}}",
            escape_graphql_string(&record.doc_id), escape_graphql_string(after))).await?;
    }
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
    let ConsumerBinding {
        enabled,
        serial,
        source,
    } = event_binding(txn, owner, consumer).await?;
    if source.source_collection != collection || source.group.is_some() {
        return Err(EventSourceCursorError::CheckpointSourceChanged);
    }
    crate::graphql::validate_collection_identifier(collection)?;
    let record = load_or_seed_for_source(txn, owner, consumer, collection).await?;
    let mut after: u64 = record
        .cursor
        .after
        .parse()
        .map_err(EventSourceCursorError::SavedPosition)?;
    let through_position: u64 = through
        .parse()
        .map_err(EventSourceCursorError::CheckpointPosition)?;
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
            .ok_or(EventSourceCursorError::JournalHeadMissing)?
            .parse()?;
        if through_position > head {
            return Ok(false);
        }
        if through_position == after {
            break;
        }
        let next: u64 = page["next"]
            .as_str()
            .ok_or(EventSourceCursorError::JournalNextMissing)?
            .parse()?;
        if !(next > after && next <= head) {
            return Err(EventSourceCursorError::JournalNotAdvancing);
        }
        let entries = page["entries"]
            .as_array()
            .ok_or(EventSourceCursorError::JournalEntriesMissing)?;
        let mut visible = 0u64;
        for entry in entries {
            let position: u64 = entry["cursor"]
                .as_str()
                .ok_or(EventSourceCursorError::ArrivalCursorMissing)?
                .parse()?;
            if position > through_position {
                break;
            }
            if !(position > after && position <= next) {
                return Err(EventSourceCursorError::ArrivalOutsidePage);
            }
            visible += 1;
            let doc_id = entry["docID"]
                .as_str()
                .ok_or(EventSourceCursorError::ArrivalDocIdMissing)?;
            if admitted_arrival(txn, owner, consumer, collection, doc_id).await? {
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
                .ok_or(EventSourceCursorError::FilterProbeRowsMissing)?
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
        .ok_or(EventSourceCursorError::ReceiptRowsMissing)?;
    if receipts.is_empty() {
        return Ok(false);
    }
    if !(receipts.len() == 1
        && receipts[0]["request_id"] == request_id
        && receipts[0]["owner_did"] == owner
        && receipts[0]["trigger_id"] == trigger_id
        && receipts[0]["source_collection"] == source_collection
        && receipts[0]["source_doc_id"] == source_doc_id)
    {
        return Err(EventSourceCursorError::ReceiptMismatch);
    }
    let request = txn.execute(&format!(
        "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}, limit: 2) {{ _docID }} }}",
        escape_graphql_string(owner), escape_graphql_string(&request_id))).await?;
    Ok(request["data"]["AgentRequest"]
        .as_array()
        .ok_or(EventSourceCursorError::RequestRowsMissing)?
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
    let key = crate::callback::idempotency_key(binding_id, source_collection, source_doc_id);
    let response = txn
        .execute(&format!(
            "{{ CallbackInvocation(filter: {{owner_node_did: {{_eq: \"{}\"}}, idempotency_key: {{_eq: \"{}\"}}}}, limit: 2) {{ origin }} }}",
            escape_graphql_string(owner),
            escape_graphql_string(&key),
        ))
        .await?;
    let rows = response["data"]["CallbackInvocation"]
        .as_array()
        .ok_or(EventSourceCursorError::CallbackReceiptRowsMissing)?;
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    let origin: crate::document_config::CallbackInvocationOrigin =
        serde_json::from_value(row["origin"].clone())
            .map_err(EventSourceCursorError::CallbackOriginDecode)?;
    if !(rows.len() == 1
        && matches!(
            origin,
            crate::document_config::CallbackInvocationOrigin::Event {
                binding_id: ref binding,
                source_collection: ref collection,
                source_doc_id: ref doc,
                ..
            } if binding == binding_id && collection == source_collection && doc == source_doc_id
        ))
    {
        return Err(EventSourceCursorError::CallbackReceiptMismatch);
    }
    Ok(true)
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
    if !checkpoint_prefix(txn, owner, consumer, source_collection, after, false).await? {
        return Err(EventSourceCursorError::PrefixUnadmitted);
    }
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
