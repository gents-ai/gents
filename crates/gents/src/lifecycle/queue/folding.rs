//! Folding queued user messages into the turn that claims them (Lean
//! `SessionQueue.claimFolding`, `consumeFolded`). Each folded message keeps its
//! own signed request. The claim only selects it; the turn's publication of it
//! as an authored user message supersedes its row in the same transaction. A
//! turn that ends before that publication leaves it queued.

use super::*;
use gents_protocol::request_admission::RequestPurpose;

/// The supersession reason that marks a request as folded into
/// `superseded_by_request_doc_id`. Other supersessions never carry it.
pub const FOLDED_REASON: &str = "folded into claimed request";

/// Authored key of a folded message within its claimed request.
pub(crate) fn folded_input_key(folded_request_doc_id: &str) -> String {
    format!("folded:{folded_request_doc_id}")
}

/// A folded message, in queue order, as its claimed turn sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FoldedInput {
    pub(crate) request_doc_id: String,
    pub(crate) request_id: String,
    pub(crate) content: String,
}

impl FoldedInput {
    pub(crate) fn key(&self) -> String {
        folded_input_key(&self.request_doc_id)
    }
}

fn blank(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.trim().is_empty())
}

/// Lean `QueueEntry.foldsInto`, head side: an interactive user turn that
/// executes its own content. Trigger, session-message, mailbox-reply and
/// controller requests keep their own turns.
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

/// The authored key's folded request, when the key names one.
pub(crate) fn folded_request_doc_id(key: &str) -> Option<&str> {
    key.strip_prefix("folded:").filter(|doc| !doc.is_empty())
}

/// The queued message an authored publication answers.
#[derive(Debug, Clone)]
pub(crate) struct FoldedConsumption {
    pub(crate) folded_request_doc_id: String,
    pub(crate) head_request_id: String,
    pub(crate) head_doc_id: String,
    pub(crate) agent_did: String,
}

impl FoldedConsumption {
    pub(crate) fn for_key(head: &AgentRequest, key: &str) -> Option<Self> {
        folded_request_doc_id(key).map(|folded| Self {
            folded_request_doc_id: folded.to_owned(),
            head_request_id: head.request_id.clone(),
            head_doc_id: head.doc_id.clone(),
            agent_did: head.agent_did.clone(),
        })
    }
}

/// Lean `QueueEntry.foldsInto`, candidate side: a queued user message under
/// the head's requester authority and execution settings that has never been
/// claimed, interrupted or retried, and is not expired.
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
    let unexpired = row.valid_until.as_deref().is_none_or(|valid_until| {
        valid_until.trim().is_empty()
            || chrono::DateTime::parse_from_rfc3339(valid_until)
                .is_ok_and(|deadline| deadline.with_timezone(&chrono::Utc) >= now)
    });
    queued_user
        && unexpired
        && row.lifecycle_state == Some(RequestLifecycleState::Pending)
        && row.purpose == Some(RequestPurpose::Normal)
        && row.execution_origin.as_deref() == Some(ExecutionOrigin::Interactive.as_str())
        && row.agent_did.as_deref() == Some(head.agent_did.as_str())
        && row.session_id.as_deref() == Some(head.session_id.as_str())
        && row.requester_did == head.requester_did
        && row.behavior_id.as_deref() == Some(head.behavior_id.as_str())
        && input.cwd == head.input.cwd
        && input.selected_skill_ids == head.input.selected_skill_ids
        && input.initial_title.is_none()
        && input.goal_continuation.is_none()
        && row.workspace_id == head.workspace_id
        && row.workspace_owner_agent_did == head.workspace_owner_agent_did
        && row.workspace_authority == head.workspace_authority
        && row.workspace_seal_hash == head.workspace_seal_hash
        && row.execution_generation.is_none()
        && blank(row.interrupt_requested_at.as_deref())
        && blank(row.retry_parent_request.as_deref())
        && blank(row.caused_by_trigger_id.as_deref())
        && blank(row.caused_by_source_doc_id.as_deref())
        && blank(row.caused_by_parent_tool_call_doc_id.as_deref())
        && !blank(row.content.as_deref())
}

const CANDIDATE_FIELDS: &str =
    "lifecycle_state interrupt_requested_at valid_until retry_parent_request";

fn pending_session_query(head: &AgentRequest) -> String {
    format!(
        r#"{{ AgentRequest(filter: {{
            agent_did: {{ _eq: "{}" }},
            session_id: {{ _eq: "{}" }},
            purpose: {{ _eq: "normal" }},
            lifecycle_state: {{ _eq: "pending" }}
        }}, order: [{{ created_at: ASC }}, {{ request_id: ASC }}]) {{
            {} {CANDIDATE_FIELDS}
        }} }}"#,
        escape_graphql_string(&head.agent_did),
        escape_graphql_string(&head.session_id),
        crate::watcher::AGENT_REQUEST_FIELDS,
    )
}

/// Pending messages that may fold into `head`'s claim, for the caller
/// to verify each one's signed admission before the claim. Queue order and
/// contiguity are decided by the claim transaction.
pub(crate) async fn fold_candidates(
    node: &EmbeddedNode,
    head: &AgentRequest,
) -> Result<Vec<AgentRequest>> {
    if !heads_user_turn(head) {
        return Ok(Vec::new());
    }
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &pending_session_query(head),
        "load pending messages that may fold into a claim",
    )
    .await?;
    let now = chrono::Utc::now();
    crate::graphql::rows::<AgentRequestRow>(&response, "AgentRequest")?
        .into_iter()
        .filter(|row| row.doc_id.as_deref() != Some(head.doc_id.as_str()))
        .filter(|row| folds_into(head, row, now))
        .map(AgentRequest::try_from)
        .collect()
}

/// Select the run of pending messages directly behind `head` that fold into
/// it, inside `head`'s claim transaction, which is the cutoff. `admitted`
/// names the requests whose signed admission the caller verified. The run
/// follows the native arrival order and stops at the first row that does not
/// fold, so no request passes one that stays queued. Selection writes nothing.
pub(crate) async fn select_fold_in_claim_txn(
    txn: &ConfigApplyTxn<'_>,
    head: &AgentRequest,
    admitted: &[String],
    claimed_at: &str,
) -> Result<Vec<FoldedInput>> {
    if admitted.is_empty() || !heads_user_turn(head) {
        return Ok(Vec::new());
    }
    let response = txn.execute(&pending_session_query(head)).await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())
            .context("decode pending messages behind a claim")?;
    let rows: Vec<AgentRequestRow> = rows
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
    // A row without an arrival has no provable place in the queue, so the
    // run cannot be shown contiguous past it.
    if order.len() != doc_ids.len() {
        return Ok(Vec::new());
    }
    let by_doc: std::collections::HashMap<&str, &AgentRequestRow> = rows
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
        });
    }
    Ok(selected)
}

/// Supersede the folded request its turn is publishing, in the publication
/// transaction. The request must still be pending: a message interrupted or
/// otherwise ended after the claim cannot be answered.
pub(crate) async fn consume_folded_in_txn(
    txn: &ConfigApplyTxn<'_>,
    consumption: &FoldedConsumption,
    published_at: &str,
) -> Result<()> {
    let doc_id = escape_graphql_string(&consumption.folded_request_doc_id);
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(docID: "{doc_id}", filter: {{
            _docID: {{ _eq: "{doc_id}" }},
            agent_did: {{ _eq: "{}" }},
            lifecycle_state: {{ _eq: "pending" }}
        }}, input: {{
            lifecycle_state: "superseded",
            superseded_by_request: "{}",
            superseded_by_request_doc_id: "{}",
            failure_reason: "{}",
            terminalized_at: "{}",
            terminal_redrive_attempts: 0
        }}) {{ _docID request_id workspace_id workspace_owner_agent_did }} }}"#,
        escape_graphql_string(&consumption.agent_did),
        escape_graphql_string(&consumption.head_request_id),
        escape_graphql_string(&consumption.head_doc_id),
        escape_graphql_string(FOLDED_REASON),
        escape_graphql_string(published_at),
    );
    let response = txn.execute(&mutation).await?;
    let updated = response["data"]["update_AgentRequest"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .with_context(|| {
            format!(
                "folded request {} is no longer pending",
                consumption.folded_request_doc_id
            )
        })?;
    let request_id = updated[0]["request_id"]
        .as_str()
        .context("folded request receipt omitted request_id")?
        .to_owned();
    crate::workspace::release_terminal_writer_binding(txn, &updated[0]).await?;
    crate::trigger_engine::durable::publish_request_outcome(
        txn,
        &consumption.agent_did,
        &request_id,
        RequestLifecycleState::Superseded.as_str(),
        FOLDED_REASON,
        published_at,
    )
    .await
}

/// A replayed folded publication was consumed by the same request.
pub(crate) async fn ensure_folded_consumed_in_txn(
    txn: &ConfigApplyTxn<'_>,
    consumption: &FoldedConsumption,
) -> Result<()> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}) {{
                request_id lifecycle_state superseded_by_request_doc_id failure_reason
            }} }}"#,
            escape_graphql_string(&consumption.folded_request_doc_id),
            escape_graphql_string(&consumption.agent_did),
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

/// The messages `head`'s turn already published and consumed, in queue order.
/// A redrive reuses them ahead of its own selection.
pub(crate) async fn load_consumed_folded_inputs(
    node: &EmbeddedNode,
    head: &AgentRequest,
) -> Result<Vec<FoldedInput>> {
    if !heads_user_turn(head) {
        return Ok(Vec::new());
    }
    let query = format!(
        r#"{{ AgentRequest(filter: {{
            agent_did: {{ _eq: "{}" }},
            session_id: {{ _eq: "{}" }},
            superseded_by_request_doc_id: {{ _eq: "{}" }},
            lifecycle_state: {{ _eq: "superseded" }},
            failure_reason: {{ _eq: "{}" }}
        }}) {{ _docID request_id requester_did content }} }}"#,
        escape_graphql_string(&head.agent_did),
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
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "lifecycle.load_folded_inputs",
        move |txn| {
            Box::pin(async move {
                let response = txn.execute(query).await?;
                let rows: Vec<AgentRequestRow> =
                    serde_json::from_value(response["data"]["AgentRequest"].clone())
                        .context("decode folded requests")?;
                let mut by_doc = std::collections::HashMap::new();
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
                        })
                    })
                    .collect()
            })
        },
    )
    .await
}
