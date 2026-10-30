use super::ToolDispatchRejection;
use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::escape_graphql_string;
use crate::tool_call_lifecycle::{ToolCallLifecycle, ToolCallState};
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};

impl ToolCallLifecycle {
    pub(crate) fn dispatch_start(&self) -> Result<DispatchStart> {
        Ok(DispatchStart {
            header: self
                .accepted_header_doc_id
                .clone()
                .context("dispatch requires an accepted tool header binding")?,
            request: self
                .request_doc_id
                .clone()
                .context("dispatch requires an accepted request binding")?,
            generation: self
                .execution_generation
                .clone()
                .context("dispatch requires the accepted execution generation")?,
            arguments: self
                .arguments
                .clone()
                .context("dispatch requires an accepted argument reference")?,
            agent: self.node_did.clone(),
            requester: self.requester_did.clone(),
            session: self.session_id.clone(),
            doc_id: self
                .doc_id
                .clone()
                .context("dispatch requires a physical tool row")?,
            id: self.tool_call_id.clone(),
            call_id: self.call_id.clone(),
            message_sequence: self.message_sequence,
            name: self.tool_name.clone(),
            selected_tool_fields: self.selected_tool_fields_fragment(),
            await_mode: self.await_mode.as_str(),
        })
    }

    pub(crate) fn mark_started(&mut self, started_at: DateTime<Utc>) {
        self.state = ToolCallState::Running;
        self.started_at = Some(started_at);
    }

    pub(in crate::tool_call_lifecycle) async fn start_running_canonical_with_time(
        &mut self,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let start = self.dispatch_start()?;
        let started_at = ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            "tool_call.start_running_canonical",
            move |txn| {
                let start = start.clone();
                Box::pin(async move { start_running_in_txn(txn, &start, fixture_now).await })
            },
        )
        .await?;
        let started_at = started_at
            .context("accepted tool lifecycle binding was altered or is no longer pending")?;
        self.mark_started(started_at);
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct DispatchStart {
    header: String,
    request: String,
    generation: String,
    arguments: gents_protocol::output::PayloadRef,
    agent: String,
    requester: Option<String>,
    session: String,
    doc_id: String,
    id: String,
    call_id: Option<String>,
    message_sequence: u32,
    name: String,
    selected_tool_fields: String,
    await_mode: &'static str,
}

pub(crate) async fn start_running_in_txn(
    txn: &ConfigApplyTxn<'_>,
    start: &DispatchStart,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<Option<DateTime<Utc>>> {
    let now = fixture_now.unwrap_or_else(Utc::now);
    let started_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let (message, _) = crate::session::load_canonical_message_in_txn(
        txn,
        &start.header,
        &start.agent,
        start.requester.as_deref(),
    )
    .await?;
    anyhow::ensure!(
        message.session_id == start.session
            && message.request_doc_id.as_deref() == Some(start.request.as_str())
            && message.role == MessageRole::Assistant
            && message.outcome == OutputOutcome::Complete
            && message.sequence == start.message_sequence
            && matches!(message.publication, MessagePublication::RequestExecution {
                    execution_generation: ref accepted_generation
                } if accepted_generation == &start.generation)
            && message.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id: block_id,
                        call_id: block_call_id, name: block_name,
                        arguments: block_arguments, .. }
                    if tool_call_doc_id == &start.doc_id && block_id == &start.id
                        && block_call_id == &start.call_id && block_name == &start.name
                        && block_arguments == &start.arguments)),
        "accepted header does not bind this exact dispatch"
    );
    let request_id = escape_graphql_string(&start.request);
    let request_row = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{
            _docID: {{ _eq: "{request_id}" }}, node_did: {{ _eq: "{}" }},
            session_id: {{ _eq: "{}" }}
        }}, limit: 2) {{
            _docID requester_did lifecycle_state execution_generation
            execution_lease_expires_at interrupt_requested_at
        }} }}"#,
            escape_graphql_string(&start.agent),
            escape_graphql_string(&start.session)
        ))
        .await?;
    let rows = request_row["data"]["AgentRequest"]
        .as_array()
        .context("dispatch request lookup omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "dispatch request is missing or ambiguous");
    let row = &rows[0];
    if row["requester_did"].as_str() != start.requester.as_deref()
        || row["lifecycle_state"].as_str() != Some("processing")
        || row["execution_generation"].as_str() != Some(start.generation.as_str())
        || !row["interrupt_requested_at"].is_null()
    {
        return Err(ToolDispatchRejection::LostRequestOwnership.into());
    }
    let expiry = row["execution_lease_expires_at"]
        .as_str()
        .context("dispatch request lease deadline missing")?;
    let expiry = DateTime::parse_from_rfc3339(expiry)?.with_timezone(&Utc);
    anyhow::ensure!(expiry > now, "dispatch request lease has expired");

    let requester_filter = start
        .requester
        .as_deref()
        .map(|value| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    let physical = txn
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{request_id}" }},
            session_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}{requester_filter}
        }}, limit: 2) {{ _docID }} }}"#,
            escape_graphql_string(&start.doc_id),
            escape_graphql_string(&start.session),
            escape_graphql_string(&start.agent)
        ))
        .await?;
    let physical_rows = physical["data"]["AgentToolCall"]
        .as_array()
        .context("dispatch physical tool lookup omitted rows")?;
    anyhow::ensure!(
        physical_rows.len() == 1,
        "dispatch physical tool is missing or ambiguous"
    );
    if start.await_mode == "background" {
        crate::hook::persistence::reserve_background_slot_in_txn(
            txn,
            &start.request,
            crate::hook::persistence::BackgroundSlot::Requested,
            "",
        )
        .await?;
    }
    let update = txn
        .execute(&format!(
            r#"mutation {{ update_AgentToolCall(docID: "{physical_doc_id}", filter: {{
            _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{request_id}" }},
            session_id: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }},
            tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }},
            message_sequence: {{ _eq: {} }},
            lifecycle_state: {{ _eq: "pending" }}{requester_filter}
        }}, input: {{ lifecycle_state: "running", started_at: "{started_at}",
            await_mode: "{}", {}
        }}) {{ _docID }} }}"#,
            escape_graphql_string(&start.doc_id),
            escape_graphql_string(&start.session),
            escape_graphql_string(&start.agent),
            escape_graphql_string(&start.id),
            escape_graphql_string(&start.name),
            start.message_sequence,
            start.await_mode,
            start.selected_tool_fields,
            physical_doc_id = escape_graphql_string(&start.doc_id),
        ))
        .await?;
    Ok(update["data"]["update_AgentToolCall"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
        .then_some(now))
}
