use super::*;
use gents_protocol::request_admission::RequestPurpose;

#[cfg(test)]
#[path = "folding/assignment_tests.rs"]
mod assignment_tests;

pub const FOLDED_REASON: &str = "folded into claimed request";

#[derive(Debug, thiserror::Error)]
#[error("pending input was changed before it was consumed")]
pub(crate) struct PendingInputChanged;

pub(crate) fn folded_input_key(folded_request_doc_id: &str) -> String {
    format!("folded:{folded_request_doc_id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoldedInput {
    pub(crate) request_doc_id: String,
    pub(crate) request_id: String,
    pub(crate) content: String,
    pub(crate) input: gents_protocol::request_input::RequestInput,
}

impl FoldedInput {
    pub(crate) fn message(&self) -> Result<gents_protocol::message::Message> {
        Ok(self.input.authored_message(&self.content)?)
    }

    pub(crate) fn key(&self) -> String {
        folded_input_key(&self.request_doc_id)
    }
}

fn blank(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

pub(super) fn same_goal_assignment(head: &AgentRequest, row: &AgentRequestRow) -> bool {
    match (
        head.input.goal_assignment.as_ref(),
        row.input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref()),
    ) {
        (None, None) => true,
        (Some(head_reference), Some(candidate_reference)) => {
            row.doc_id.as_deref().is_some_and(|doc| {
                head_reference.continuation(&head.doc_id) == candidate_reference.continuation(doc)
            })
        }
        _ => false,
    }
}

pub(crate) fn heads_user_turn(head: &AgentRequest) -> bool {
    head.purpose == RequestPurpose::Normal
        && head.execution_origin.as_deref() == Some(ExecutionOrigin::Interactive.as_str())
        && head.input.goal_continuation.is_none()
        && head.input.queue.as_ref().is_none_or(|queue| {
            queue.source == QueueSource::User && queue.policy == QueuePolicy::Append
        })
        && blank(head.caused_by_trigger_id.as_deref())
        && blank(head.caused_by_source_doc_id.as_deref())
        && blank(head.caused_by_parent_tool_call_doc_id.as_deref())
}

pub(crate) fn folded_request_doc_id(key: &str) -> Option<&str> {
    key.strip_prefix("folded:").filter(|doc| !doc.is_empty())
}

#[derive(Debug, Clone)]
pub(crate) struct FoldedConsumption {
    pub(crate) folded_request_doc_id: String,
    pub(crate) head_request_id: String,
    pub(crate) head_doc_id: String,
    pub(crate) node_did: String,
    head: AgentRequest,
    selected_at_claim: bool,
}

impl FoldedConsumption {
    pub(crate) fn for_key(
        head: &AgentRequest,
        key: &str,
        selected: &[FoldedInput],
    ) -> Option<Self> {
        folded_request_doc_id(key).map(|folded| Self {
            folded_request_doc_id: folded.to_owned(),
            head_request_id: head.request_id.clone(),
            head_doc_id: head.doc_id.clone(),
            node_did: head.node_did.clone(),
            head: head.clone(),
            selected_at_claim: selected.iter().any(|entry| entry.request_doc_id == folded),
        })
    }
}

fn folds_into(
    head: &AgentRequest,
    row: &AgentRequestRow,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(input) = row.input.as_ref() else {
        return false;
    };
    let queued_user = input.queue.as_ref().is_some_and(|queue| {
        queue.source == QueueSource::User
            && queue.policy == QueuePolicy::Append
            && queue.key.is_none()
            && !blank(queue.queued_after_request_id.as_deref())
            && queue.interrupted_request_id.is_none()
            && queue.background_completion_wake_version.is_none()
    });
    let unexpired = matches!(
        crate::lifecycle::parse_valid_until(row.valid_until.as_deref(), now),
        crate::lifecycle::TtlOutcome::NotSet | crate::lifecycle::TtlOutcome::Live(_)
    );
    queued_user
        && unexpired
        && row.lifecycle_state == Some(RequestLifecycleState::Pending)
        && row.purpose == Some(RequestPurpose::Normal)
        && row.execution_origin.as_deref() == Some(ExecutionOrigin::Interactive.as_str())
        && row.node_did.as_deref() == Some(head.node_did.as_str())
        && row.session_id.as_deref() == Some(head.session_id.as_str())
        && row.requester_did == head.requester_did
        && row.agent_id.as_deref() == Some(head.agent_id.as_str())
        && input.cwd == head.input.cwd
        && input.selected_skill_ids == head.input.selected_skill_ids
        && input.initial_title.is_none()
        && input.goal_continuation.is_none()
        && same_goal_assignment(head, row)
        && row.workspace_id == head.workspace_id
        && row.workspace_owner_node_did == head.workspace_owner_node_did
        && row.workspace_authority == head.workspace_authority
        && row.workspace_seal_hash == head.workspace_seal_hash
        && row.execution_generation.is_none()
        && blank(row.interrupt_requested_at.as_deref())
        && blank(row.retry_parent_request.as_deref())
        && blank(row.caused_by_trigger_id.as_deref())
        && blank(row.caused_by_source_doc_id.as_deref())
        && blank(row.caused_by_parent_tool_call_doc_id.as_deref())
        && input.has_authored_input(row.content.as_deref().unwrap_or_default())
}

pub(crate) async fn fold_candidates(
    node: &std::sync::Arc<EmbeddedNode>,
    head: &AgentRequest,
) -> Result<Vec<AgentRequest>> {
    if !heads_user_turn(head) {
        return Ok(Vec::new());
    }
    let rows = crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "queue.fold_candidates",
        |txn| {
            Box::pin(async move {
                super::intake::pending_rows_in_txn(txn, &head.node_did, &head.session_id).await
            })
        },
    )
    .await?;
    let now = chrono::Utc::now();
    rows.into_iter()
        .filter(|row| row.doc_id.as_deref() != Some(head.doc_id.as_str()))
        .filter(|row| folds_into(head, row, now))
        .map(AgentRequest::try_from)
        .collect()
}

pub(crate) async fn select_fold_in_claim_txn(
    txn: &ConfigApplyTxn<'_>,
    head: &AgentRequest,
    admitted: &[String],
    claimed_at: &str,
) -> Result<Vec<FoldedInput>> {
    if admitted.is_empty() || !heads_user_turn(head) {
        return Ok(Vec::new());
    }
    let rows: Vec<AgentRequestRow> =
        super::intake::pending_rows_in_txn(txn, &head.node_did, &head.session_id)
            .await?
            .into_iter()
            .filter(|row| row.doc_id.as_deref() != Some(head.doc_id.as_str()))
            .collect();
    let mut doc_ids = vec![head.doc_id.clone()];
    for row in &rows {
        doc_ids.push(
            row.doc_id
                .clone()
                .context("pending AgentRequest row is missing _docID")?,
        );
    }
    let order = crate::trigger_engine::durable::request_arrival_order(txn, &doc_ids).await?;
    let Some(head_position) = order.iter().position(|doc| doc == &head.doc_id) else {
        return Ok(Vec::new());
    };
    // An unarrived row leaves the run's contiguity unprovable.
    if order.len() != doc_ids.len() {
        return Ok(Vec::new());
    }
    let by_doc: rapidhash::RapidHashMap<&str, &AgentRequestRow> = rows
        .iter()
        .filter_map(|row| row.doc_id.as_deref().map(|doc| (doc, row)))
        .collect();
    let now = chrono::DateTime::parse_from_rfc3339(claimed_at)
        .context("claim time is not RFC 3339")?
        .with_timezone(&chrono::Utc);
    let mut selected = Vec::new();
    for doc in &order[head_position + 1..] {
        let Some(row) = by_doc.get(doc.as_str()) else {
            break;
        };
        if !admitted.contains(doc) || !folds_into(head, row, now) {
            break;
        }
        selected.push(FoldedInput {
            request_doc_id: doc.clone(),
            request_id: row.request_id.clone(),
            content: row.content.clone().unwrap_or_default(),
            input: row.input.clone().unwrap_or_default(),
        });
    }
    Ok(selected)
}

pub(crate) async fn consume_folded_in_txn(
    txn: &ConfigApplyTxn<'_>,
    consumption: &FoldedConsumption,
    published_at: &str,
    expected: &gents_protocol::message::Message,
) -> Result<()> {
    let rows = super::intake::pending_rows_in_txn(
        txn,
        &consumption.head.node_did,
        &consumption.head.session_id,
    )
    .await?;
    let mut ids = rows
        .iter()
        .map(|row| {
            row.doc_id
                .clone()
                .context("pending input lacks physical identity")
        })
        .collect::<Result<Vec<_>>>()?;
    if consumption.selected_at_claim {
        ids.push(consumption.head_doc_id.clone());
    }
    let order = crate::trigger_engine::durable::request_arrival_order(txn, &ids).await?;
    anyhow::ensure!(
        order.len() == ids.len(),
        "pending input arrival order is incomplete"
    );
    let first_id = if consumption.selected_at_claim {
        order
            .iter()
            .position(|id| id == &consumption.head_doc_id)
            .and_then(|index| order.get(index + 1))
    } else {
        None
    };
    let first = if consumption.selected_at_claim {
        first_id.and_then(|id| rows.iter().find(|row| row.doc_id.as_ref() == Some(id)))
    } else {
        let ordered = order
            .iter()
            .filter_map(|id| {
                rows.iter()
                    .find(|row| row.doc_id.as_ref() == Some(id))
                    .cloned()
            })
            .collect::<Vec<_>>();
        super::intake::boundary_candidate(&ordered)
            .and_then(|candidate| rows.iter().find(|row| row.doc_id == candidate.doc_id))
    };
    let Some(row) = first else {
        return Err(PendingInputChanged.into());
    };
    let selected = if consumption.selected_at_claim {
        folds_into(&consumption.head, row, chrono::Utc::now())
    } else {
        super::intake::steering_compatible(&consumption.head, row, chrono::Utc::now())
    };
    if row.doc_id.as_deref() != Some(consumption.folded_request_doc_id.as_str())
        || !selected
        || expected
            != &row
                .input
                .as_ref()
                .unwrap_or(&RequestInput::default())
                .authored_message(row.content.as_deref().unwrap_or_default())?
    {
        return Err(PendingInputChanged.into());
    }
    let doc_id = escape_graphql_string(&consumption.folded_request_doc_id);
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(docID: "{doc_id}", filter: {{
            _docID: {{ _eq: "{doc_id}" }},
            node_did: {{ _eq: "{}" }},
            lifecycle_state: {{ _eq: "pending" }},
            interrupt_requested_at: {{ _eq: null }}
        }}, input: {{
            lifecycle_state: "superseded",
            superseded_by_request: "{}",
            superseded_by_request_doc_id: "{}",
            failure_reason: "{}",
            terminalized_at: "{}",
            terminal_redrive_attempts: 0
        }}) {{ _docID request_id workspace_id workspace_owner_node_did }} }}"#,
        escape_graphql_string(&consumption.node_did),
        escape_graphql_string(&consumption.head_request_id),
        escape_graphql_string(&consumption.head_doc_id),
        escape_graphql_string(FOLDED_REASON),
        escape_graphql_string(published_at),
    );
    let response = txn.execute(&mutation).await?;
    let updated = response["data"]["update_AgentRequest"]
        .as_array()
        .context("folded input publication omitted affected rows")?;
    if updated.is_empty() {
        return Err(PendingInputChanged.into());
    }
    anyhow::ensure!(
        updated.len() == 1,
        "folded input publication changed multiple rows"
    );
    let request_id = updated[0]["request_id"]
        .as_str()
        .context("folded request receipt omitted request_id")?
        .to_owned();
    crate::workspace::release_terminal_writer_binding(txn, &updated[0]).await?;
    crate::trigger_engine::durable::publish_request_outcome(
        txn,
        &consumption.node_did,
        &request_id,
        RequestLifecycleState::Superseded.as_str(),
        FOLDED_REASON,
        published_at,
    )
    .await
}

pub(crate) async fn ensure_folded_consumed_in_txn(
    txn: &ConfigApplyTxn<'_>,
    consumption: &FoldedConsumption,
) -> Result<()> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }} }}) {{
                request_id lifecycle_state superseded_by_request_doc_id failure_reason
            }} }}"#,
            escape_graphql_string(&consumption.folded_request_doc_id),
            escape_graphql_string(&consumption.node_did),
        ))
        .await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    anyhow::ensure!(
        rows.len() == 1
            && rows[0].lifecycle_state == Some(RequestLifecycleState::Superseded)
            && rows[0].superseded_by_request_doc_id.as_deref()
                == Some(consumption.head_doc_id.as_str())
            && rows[0].failure_reason.as_deref() == Some(FOLDED_REASON),
        "replayed folded publication was not consumed by its request"
    );
    Ok(())
}

pub(crate) async fn load_consumed_folded_inputs(
    node: &std::sync::Arc<EmbeddedNode>,
    head: &AgentRequest,
) -> Result<Vec<FoldedInput>> {
    if !heads_user_turn(head) {
        return Ok(Vec::new());
    }
    let query = format!(
        r#"{{ AgentRequest(filter: {{
            node_did: {{ _eq: "{}" }},
            session_id: {{ _eq: "{}" }},
            superseded_by_request_doc_id: {{ _eq: "{}" }},
            lifecycle_state: {{ _eq: "superseded" }},
            failure_reason: {{ _eq: "{}" }}
        }}) {{ _docID request_id requester_did content }} }}"#,
        escape_graphql_string(&head.node_did),
        escape_graphql_string(&head.session_id),
        escape_graphql_string(&head.doc_id),
        escape_graphql_string(FOLDED_REASON),
    );
    let observed = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load the messages a claim folded",
    )
    .await?;
    if crate::graphql::rows::<AgentRequestRow>(&observed, "AgentRequest")?.is_empty() {
        return Ok(Vec::new());
    }
    let query = &query;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "lifecycle.load_folded_inputs",
        move |txn| {
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let rows: Vec<AgentRequestRow> =
                    serde_json::from_value(response["data"]["AgentRequest"].clone())
                        .context("decode folded requests")?;
                let mut by_doc = rapidhash::RapidHashMap::default();
                for row in rows {
                    anyhow::ensure!(
                        row.requester_did == head.requester_did,
                        "folded request {} crossed its claimed request's requester",
                        row.request_id
                    );
                    let doc = row
                        .doc_id
                        .clone()
                        .context("folded AgentRequest row is missing _docID")?;
                    by_doc.insert(doc, row);
                }
                let docs = by_doc.keys().cloned().collect::<Vec<_>>();
                let order =
                    crate::trigger_engine::durable::request_arrival_order(txn, &docs).await?;
                anyhow::ensure!(
                    order.len() == docs.len(),
                    "folded requests of {} lack a queue arrival",
                    head.request_id
                );
                order
                    .into_iter()
                    .map(|doc| {
                        let row = by_doc.remove(&doc).context("folded arrival is ambiguous")?;
                        Ok(FoldedInput {
                            request_doc_id: doc,
                            request_id: row.request_id,
                            content: row.content.unwrap_or_default(),
                            input: row.input.unwrap_or_default(),
                        })
                    })
                    .collect()
            })
        },
    )
    .await
}
