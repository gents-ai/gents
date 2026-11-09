//! Read-only trace of one Trigger from source document to request. Each fact
//! comes from the owner that decides it; this module only orders the facts and
//! names the first one that stopped delivery.

use anyhow::{Context, Result};
use gents_protocol::event_delivery::EventConsumer;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::trigger_delivery::FireIdentity;
use serde::Serialize;
use serde_json::{json, Value};

use super::{Agent, EventSource, Task, Trigger, TriggerSource, BUDGET_EXHAUSTED_STATUS};
use crate::store::config_client::event_source_cursor;
use crate::store::config_client::{read_desired_state_document_in_txn, ConfigApplyTxn};
use crate::store::graphql::{
    escape_graphql_string, selection_filter, validate_collection_identifier,
};
use crate::Collection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    NotChecked,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExplainStep {
    pub name: &'static str,
    pub status: StepStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriggerExplanation {
    pub answer: String,
    pub how_to_proceed: String,
    pub metadata: Value,
}

#[derive(Default)]
struct Trace {
    steps: Vec<ExplainStep>,
    failure: Option<(String, String)>,
}

impl Trace {
    fn passed(&mut self, name: &'static str, detail: impl Into<String>) {
        self.push(name, StepStatus::Passed, detail.into());
    }

    fn not_checked(&mut self, name: &'static str, detail: impl Into<String>) {
        self.push(name, StepStatus::NotChecked, detail.into());
    }

    fn failed(&mut self, name: &'static str, detail: impl Into<String>, how: impl Into<String>) {
        let detail = detail.into();
        self.failure
            .get_or_insert_with(|| (format!("{name}: {detail}"), how.into()));
        self.push(name, StepStatus::Failed, detail);
    }

    fn push(&mut self, name: &'static str, status: StepStatus, detail: String) {
        self.steps.push(ExplainStep {
            name,
            status,
            detail,
        });
    }

    fn finish(self, delivered: String, next: &str, metadata: Value) -> TriggerExplanation {
        let (answer, how_to_proceed) = self.failure.unwrap_or_else(|| (delivered, next.to_owned()));
        let mut metadata = metadata;
        metadata["steps"] = json!(self.steps);
        TriggerExplanation {
            answer,
            how_to_proceed,
            metadata,
        }
    }
}

fn first_row(response: &Value, collection: &str) -> Result<Option<Value>> {
    Ok(response["data"][collection]
        .as_array()
        .with_context(|| format!("{collection} query omitted rows"))?
        .first()
        .cloned())
}

fn text(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(str::to_owned)
}

async fn decoded<T>(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    read_desired_state_document_in_txn(txn, collection, owner, id)
        .await?
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}

async fn source_matches(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    filter: Option<&str>,
    doc_id: &str,
) -> Result<bool> {
    let filter = selection_filter(filter, Some(("_docID", doc_id)))?;
    let response = txn
        .execute(&format!(
            "{{ {collection}(filter: {filter}, limit: 1) {{ _docID }} }}"
        ))
        .await?;
    Ok(first_row(&response, collection)?.is_some())
}

struct CursorFacts {
    after: u64,
    head: u64,
    arrival: Option<u64>,
}

async fn cursor_facts(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
    collection: &str,
    doc_id: Option<&str>,
) -> Result<Option<CursorFacts>> {
    let consumer = EventConsumer::Trigger {
        trigger_id: trigger_id.to_owned(),
    };
    let Some(record) =
        event_source_cursor::load_for_source(txn, owner, &consumer, collection).await?
    else {
        return Ok(None);
    };
    let doc_filter = doc_id
        .map(|id| format!(", docID: [\"{}\"]", escape_graphql_string(id)))
        .unwrap_or_default();
    let response = txn
        .execute(&format!(
            "{{ _documentArrivals(collection: \"{}\", after: \"0\", limit: 1{doc_filter}) {{ head entries {{ cursor docID }} }} }}",
            escape_graphql_string(collection),
        ))
        .await?;
    let page = &response["data"]["_documentArrivals"];
    let parse = |value: &Value, what: &str| -> Result<u64> {
        value
            .as_str()
            .with_context(|| format!("arrival journal omitted {what}"))?
            .parse()
            .with_context(|| format!("arrival journal {what} is not a position"))
    };
    let arrival = page["entries"]
        .as_array()
        .context("arrival journal omitted entries")?
        .first()
        .map(|entry| parse(&entry["cursor"], "cursor"))
        .transpose()?;
    Ok(Some(CursorFacts {
        after: record
            .cursor
            .after
            .parse()
            .context("arrival cursor is not a position")?,
        head: parse(&page["head"], "head")?,
        arrival,
    }))
}

struct Observation {
    last_status: Option<String>,
    last_error: Option<String>,
    last_doc: Option<String>,
    fire_count: i64,
}

async fn load_observation(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
) -> Result<Observation> {
    let response = txn
        .execute(&format!(
            "{{ Trigger(filter: {{node_did: {{_eq: \"{}\"}}, trigger_id: {{_eq: \"{}\"}}}}, limit: 1) {{ last_status last_error last_fired_source_doc_id fire_count }} }}",
            escape_graphql_string(owner),
            escape_graphql_string(trigger_id),
        ))
        .await?;
    let row = first_row(&response, "Trigger")?.unwrap_or(Value::Null);
    Ok(Observation {
        last_status: text(&row, "last_status"),
        last_error: text(&row, "last_error"),
        last_doc: text(&row, "last_fired_source_doc_id"),
        fire_count: row["fire_count"].as_i64().unwrap_or(0),
    })
}

/// Traces `doc_id` through `trigger`; without a document, the most recent
/// source document the runtime attempted stands in, and with neither, only
/// configuration and the cursor are checked.
pub async fn explain_trigger_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    trigger_id: &str,
    requested_doc: Option<&str>,
) -> Result<TriggerExplanation> {
    let mut trace = Trace::default();
    let Some(trigger) = decoded::<Trigger>(txn, Collection::Trigger, owner, trigger_id).await?
    else {
        trace.failed(
            "trigger",
            format!("no Trigger {trigger_id} on this node"),
            "List triggers with [\"trigger\",\"list\"] and retry with an existing ID.",
        );
        return Ok(trace.finish(String::new(), "", json!({"trigger_id": trigger_id})));
    };
    let observation = load_observation(txn, owner, trigger_id).await?;
    if trigger.enabled {
        trace.passed("trigger", "enabled");
    } else if observation.last_status.as_deref() == Some(BUDGET_EXHAUSTED_STATUS) {
        trace.failed(
            "trigger",
            format!(
                "stopped by its fire budget: {}",
                observation.last_error.as_deref().unwrap_or("no reason recorded")
            ),
            "Fix what writes to the event-source collection (check whether the Task's own agent writes it), then raise max_fires_per_hour or wait for the hour to pass, and update the Trigger with enabled: true.",
        );
    } else {
        trace.failed(
            "trigger",
            "disabled",
            "Update the Trigger with enabled: true when it should run again.",
        );
    }
    match decoded::<Task>(txn, Collection::Task, owner, &trigger.task_id).await? {
        None => trace.failed(
            "task",
            format!("Task {} does not exist", trigger.task_id),
            "Create the Task or point the Trigger at an existing one.",
        ),
        Some(task) if !task.enabled => trace.failed(
            "task",
            format!("Task {} is disabled", trigger.task_id),
            "Update the Task with enabled: true.",
        ),
        Some(task) => {
            match decoded::<Agent>(txn, Collection::Agent, owner, &task.agent_did).await? {
                None => trace.failed(
                    "task",
                    format!("Task agent {} does not exist", task.agent_did),
                    "Create the Task's agent or recreate the Task for an existing agent.",
                ),
                Some(agent) if !agent.enabled => trace.failed(
                    "task",
                    format!("Task agent {} is disabled", task.agent_did),
                    "Enable the agent or recreate the Task for an enabled agent.",
                ),
                Some(_) => trace.passed(
                    "task",
                    format!("Task {} and its agent are enabled", trigger.task_id),
                ),
            }
        }
    }

    let (source, collection) = match &trigger.source {
        TriggerSource::Schedule { schedule_id } => {
            for step in ["source_document", "filter", "cursor", "fire"] {
                trace.not_checked(
                    step,
                    format!("schedule source {schedule_id}; no source document"),
                );
            }
            (None, None)
        }
        TriggerSource::Event { event_source_id } => {
            match decoded::<EventSource>(txn, Collection::EventSource, owner, event_source_id)
                .await?
            {
                Some(source) => {
                    validate_collection_identifier(&source.source_collection)?;
                    let collection = source.source_collection.clone();
                    (Some(source), Some(collection))
                }
                None => {
                    trace.failed(
                        "source_document",
                        format!("EventSource {event_source_id} does not exist"),
                        "Create the EventSource or point the Trigger at an existing one.",
                    );
                    (None, None)
                }
            }
        }
    };

    let doc_id = requested_doc
        .map(str::to_owned)
        .or_else(|| observation.last_doc.clone());
    let cursor = match &collection {
        Some(collection) => {
            cursor_facts(txn, owner, trigger_id, collection, doc_id.as_deref()).await?
        }
        None => None,
    };
    let grouped = source.as_ref().is_some_and(|source| source.group.is_some());
    if let (Some(source), Some(collection)) = (&source, &collection) {
        match &doc_id {
            None => {
                let detail = "no source document given and none attempted yet";
                trace.not_checked("source_document", detail);
                trace.not_checked("filter", detail);
            }
            Some(id) if !source_matches(txn, collection, None, id).await? => {
                trace.failed(
                    "source_document",
                    format!("document {id} is not in {collection} on this node"),
                    "Check the document ID and collection; a document that has not replicated here cannot fire.",
                );
                trace.not_checked("filter", "no source document to match");
            }
            Some(id) => {
                trace.passed(
                    "source_document",
                    format!("{collection} document {id} exists"),
                );
                match source.filter.as_deref() {
                    None => trace.passed("filter", "the event source has no filter"),
                    Some(filter) if source_matches(txn, collection, Some(filter), id).await? => {
                        trace.passed("filter", format!("document matched {filter}"));
                    }
                    Some(filter) => trace.failed(
                        "filter",
                        format!("document does not match {filter}"),
                        "Correct the event source filter or the document fields it tests.",
                    ),
                }
            }
        }
    }
    if grouped {
        for step in ["cursor", "fire"] {
            trace.not_checked(
                step,
                "per-group triggers fire per correlation group, not per arrival",
            );
        }
    } else if source.is_some() {
        match &cursor {
            None => trace.failed(
                "cursor",
                "no arrival cursor for this Trigger",
                "A running runtime seeds the cursor when the Trigger registers; start the runtime or recreate the Trigger.",
            ),
            Some(facts) => match (facts.arrival, doc_id.as_deref()) {
                (Some(arrival), Some(_)) if arrival > facts.after => trace.failed(
                    "cursor",
                    format!(
                        "document arrived at {arrival}, ahead of cursor {} (journal head {})",
                        facts.after, facts.head
                    ),
                    "Delivery has not reached it. Confirm the runtime is running; an earlier arrival may be parked, see last_error.",
                ),
                (Some(arrival), _) => trace.passed(
                    "cursor",
                    format!("cursor {} is at or past arrival {arrival} (journal head {})", facts.after, facts.head),
                ),
                (None, Some(id)) => trace.failed(
                    "cursor",
                    format!("document {id} has no arrival in the journal (cursor {}, head {})", facts.after, facts.head),
                    "Only documents created after the Trigger registered arrive; a replicated document appears once it syncs.",
                ),
                (None, None) => trace.passed(
                    "cursor",
                    format!("cursor {} of journal head {}", facts.after, facts.head),
                ),
            },
        }
    }

    let fire_scope = match (&collection, &doc_id, grouped) {
        (Some(collection), Some(id), false) => Some(FireIdentity {
            owner_did: owner.to_owned(),
            trigger_id: trigger_id.to_owned(),
            source_collection: collection.clone(),
            source_doc_id: id.clone(),
        }),
        _ => None,
    };
    let (mut fire_key, mut request_id, mut session_id) = (None, None, None);
    let attempted_this_doc =
        doc_id.as_ref().is_some() && observation.last_doc.as_ref() == doc_id.as_ref();
    match &fire_scope {
        Some(identity) => {
            let key = identity.fire_key();
            let receipt = first_row(
                &txn.execute(&format!(
                    "{{ TriggerFire(filter: {{fire_key: {{_eq: \"{}\"}}}}, limit: 1) {{ request_id session_id created_at }} }}",
                    escape_graphql_string(&key),
                ))
                .await?,
                "TriggerFire",
            )?;
            fire_key = Some(key);
            match receipt {
                Some(receipt) => {
                    request_id = text(&receipt, "request_id");
                    session_id = text(&receipt, "session_id");
                    trace.passed(
                        "fire",
                        format!(
                            "fire admitted at {} into session {}",
                            receipt["created_at"].as_str().unwrap_or("unknown time"),
                            session_id.as_deref().unwrap_or("unknown")
                        ),
                    );
                    trace.passed("materialization", "the fire committed its request");
                }
                None => {
                    let reason = attempted_this_doc
                        .then(|| observation.last_error.clone())
                        .flatten();
                    let advice = if trigger.session_id_template.is_some()
                        && trigger.create_session != Some(true)
                    {
                        "If the rendered session does not exist yet, set create_session: true so the first fire creates it. "
                    } else {
                        ""
                    };
                    let (detail, how) = match &reason {
                        Some(reason) => (
                            format!("no receipt for this document; the last attempt failed: {reason}"),
                            format!("{advice}Fix the reported cause; the arrival is retried when configuration or the document changes."),
                        ),
                        None => (
                            "no receipt for this document".to_owned(),
                            format!("{advice}A cursor past this arrival with no receipt means it was excluded or predates the Trigger; otherwise wait for delivery."),
                        ),
                    };
                    trace.failed("fire", detail.clone(), how);
                    trace.failed(
                        "materialization",
                        reason
                            .unwrap_or_else(|| "no attempt recorded for this document".to_owned()),
                        "",
                    );
                }
            }
        }
        None if grouped || collection.is_none() => {
            if collection.is_some() {
                trace.not_checked(
                    "materialization",
                    "per-group triggers record their receipt per group",
                );
            } else {
                let detail = match (&observation.last_status, &observation.last_error) {
                    (Some(status), Some(error)) => format!("last status {status}: {error}"),
                    (Some(status), None) => format!("last status {status}"),
                    _ => "no fire attempted yet".to_owned(),
                };
                trace.not_checked("materialization", detail);
            }
        }
        None => {
            trace.not_checked("fire", "no source document to trace");
            trace.not_checked("materialization", "no source document to trace");
        }
    }

    let mut delivered = format!(
        "Trigger {trigger_id} is healthy; last status {}. Pass a source document to trace one delivery.",
        observation.last_status.as_deref().unwrap_or("none")
    );
    match &request_id {
        Some(id) => {
            let request = first_row(
                &txn.execute(&format!(
                    "{{ AgentRequest(filter: {{node_did: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}}}, limit: 1) {{ session_id lifecycle_state failure_reason }} }}",
                    escape_graphql_string(owner),
                    escape_graphql_string(id),
                ))
                .await?,
                "AgentRequest",
            )?;
            match request {
                None => trace.failed(
                    "request",
                    format!("request {id} is missing"),
                    "Its receipt and request commit together, so a missing row is unreadable to this caller.",
                ),
                Some(request) => {
                    let state = request["lifecycle_state"].as_str().unwrap_or("unknown");
                    delivered = format!(
                        "Delivered: request {id} is {state} in session {}.",
                        session_id.as_deref().unwrap_or("unknown")
                    );
                    if RequestLifecycleState::is_terminal_str(Some(state))
                        && state != RequestLifecycleState::Completed.as_str()
                    {
                        trace.failed(
                            "request",
                            format!(
                                "request {id} ended {state}: {}",
                                request["failure_reason"].as_str().unwrap_or("no failure reason recorded")
                            ),
                            "The fire reached the agent; inspect the session for what stopped the request.",
                        );
                    } else {
                        trace.passed("request", format!("request {id} is {state}"));
                    }
                }
            }
        }
        None => trace.not_checked("request", "no admitted fire to follow"),
    }

    let metadata = json!({
        "trigger_id": trigger_id,
        "task_id": trigger.task_id,
        "source_collection": collection,
        "source_doc_id": doc_id,
        "fire_key": fire_key,
        "request_id": request_id,
        "session_id": session_id,
        "session_id_template": trigger.session_id_template,
        "create_session": trigger.create_session.unwrap_or(false),
        "max_fires_per_hour": trigger.effective_max_fires_per_hour(),
        "cursor": cursor.as_ref().map(|facts| json!({"after": facts.after.to_string(), "journal_head": facts.head.to_string(), "arrival": facts.arrival.map(|a| a.to_string())})),
        "last_status": observation.last_status,
        "last_error": observation.last_error,
        "fire_count": observation.fire_count,
    });
    Ok(trace.finish(
        delivered,
        "Nothing is blocking delivery; use the request and session IDs to inspect the run.",
        metadata,
    ))
}
