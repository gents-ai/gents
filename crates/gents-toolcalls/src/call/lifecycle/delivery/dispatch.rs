use super::ToolDispatchRejection;
use crate::call::lifecycle::ToolCallLifecycle;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::session::tool_call_state::ToolCallState;
use gents_store::config_client::{ConfigApplyTxn, IdempotentTransactionRetry};
use gents_store::graphql::escape_graphql_string;

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
            effect: self.plugin_effect_terminal()?,
            deadline: self.deadline_at,
        })
    }

    pub(crate) fn mark_started(&mut self, started_at: DateTime<Utc>) {
        self.state = ToolCallState::Running;
        self.started_at = Some(started_at);
    }

    /// The dispatch reads the request document to fence on its lease, so a
    /// renewal committing meanwhile aborts an attempt; renewals are at least
    /// half a lease apart, so the Standard attempts absorb the one that can
    /// land (`tla/BackgroundAdmissionContention`).
    pub(in crate::call::lifecycle) async fn start_running_canonical_with_time(
        &mut self,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let start = self.dispatch_start()?;
        let started_at = self
            .access()?
            .transact_idempotent(
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
    effect: Option<super::super::plugin_effect::PluginEffectTerminal>,
    deadline: DateTime<Utc>,
}

pub(crate) async fn start_running_in_txn(
    txn: &ConfigApplyTxn<'_>,
    start: &DispatchStart,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<Option<DateTime<Utc>>> {
    let now = fixture_now.unwrap_or_else(Utc::now);
    let started_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
    if let Some(effect) = &start.effect {
        super::super::plugin_effect::validate_parent(
            txn,
            &effect.parent,
            &start.header,
            &start.generation,
            start.message_sequence,
            start.deadline,
            true,
        )
        .await?;
        let (arguments, _) = super::super::plugin_effect::arguments_in_txn(
            txn,
            &effect.parent,
            &start.doc_id,
            &start.generation,
            &start.id,
            &start.name,
        )
        .await?;
        anyhow::ensure!(
            arguments == start.arguments,
            "plugin effect dispatch arguments changed"
        );
    } else {
        let (message, _) = gents_session::session::load_canonical_message_in_txn(
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
        let request_row = {
            use gents_protocol::document_query::{
                fields::agent_request as fields, Filter, Query, Selection,
            };
            let rendered = Query::select(
                Selection::doc_id()
                    .field(fields::REQUESTER_DID)
                    .field(fields::LIFECYCLE_STATE)
                    .field(fields::EXECUTION_GENERATION)
                    .field(fields::EXECUTION_LEASE_EXPIRES_AT)
                    .field(fields::INTERRUPT_REQUESTED_AT),
            )
            .filter(Filter::eq(fields::DOC_ID, &start.request))
            .filter(Filter::eq(fields::NODE_DID, &start.agent))
            .filter(Filter::eq(fields::SESSION_ID, &start.session))
            .limit(2)
            .render();
            txn.run(&rendered).await?
        };
        let rows = request_row["data"]["AgentRequest"]
            .as_array()
            .context("dispatch request lookup omitted rows")?;
        anyhow::ensure!(rows.len() == 1, "dispatch request is missing or ambiguous");
        let row = &rows[0];
        if row["requester_did"].as_str() != start.requester.as_deref()
            || RequestLifecycleState::parse_opt(row["lifecycle_state"].as_str())
                != Some(RequestLifecycleState::Processing)
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
    }

    let request_id = escape_graphql_string(&start.request);

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
    let physical = {
        use gents_protocol::document_query::{
            fields::agent_tool_call as fields, Filter, Query, Selection,
        };
        let requester_scope = match start.requester.as_ref() {
            Some(requester) => Filter::eq(fields::REQUESTER_DID, requester),
            None => Filter::is_null(fields::REQUESTER_DID),
        };
        let rendered = Query::select(Selection::doc_id())
            .filter(Filter::eq(fields::DOC_ID, &start.doc_id))
            .filter(Filter::eq(fields::REQUEST_DOC_ID, &start.request))
            .filter(Filter::eq(fields::SESSION_ID, &start.session))
            .filter(Filter::eq(fields::NODE_DID, &start.agent))
            .filter(requester_scope)
            .limit(2)
            .render();
        txn.run(&rendered).await?
    };
    let physical_rows = physical["data"]["AgentToolCall"]
        .as_array()
        .context("dispatch physical tool lookup omitted rows")?;
    anyhow::ensure!(
        physical_rows.len() == 1,
        "dispatch physical tool is missing or ambiguous"
    );
    if start.await_mode == "background" {
        crate::call::hook::persistence::reserve_background_slot_in_txn(
            txn,
            &start.request,
            crate::call::hook::persistence::BackgroundSlot::Requested,
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
