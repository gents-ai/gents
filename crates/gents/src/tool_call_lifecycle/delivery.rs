//! Canonical closure and native invocation-reply publication for a physical
//! tool call.  Tool rows carry lifecycle only; payload bytes live exactly once
//! in their `OutputSource::ToolCall` segment.

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::{
    reconstruction::{reconstruct_stream, ObservedSegment},
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::escape_graphql_string;
use crate::lifecycle::queue::next_append_sequence_in_transaction;
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, output_segment_create_variables,
    transcript_message_create_variables, AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

use super::{CancelCause, FailureClass, ToolCallLifecycle, ToolCallState};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolDispatchRejection {
    #[error("dispatch lost exact request ownership or cancellation")]
    LostRequestOwnership,
}

#[derive(Clone, Copy)]
pub(super) struct TerminalFields<'a> {
    pub state: ToolCallState,
    pub failure: Option<FailureClass>,
    pub cancel: Option<CancelCause>,
    pub remote_cancel_intent_at: Option<DateTime<Utc>>,
    pub completion_reason: Option<&'a str>,
}

/// Physical scope for the one canonical output source owned by an executing
/// tool.  This is deliberately derived from an admitted lifecycle, never a
/// logical tool id supplied by a host callback.
#[derive(Clone)]
pub(crate) struct ToolOutputBinding {
    pub(crate) node: std::sync::Arc<defra_node::EmbeddedNode>,
    pub(crate) tool_call_doc_id: String,
    pub(crate) request_doc_id: String,
    pub(crate) session_id: String,
    pub(crate) agent_did: String,
    pub(crate) requester_did: Option<String>,
}

impl gents_loop::live_output::CanonicalOutputAppender for ToolOutputBinding {
    fn append<'a>(
        &'a self,
        text: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<std::ops::Range<u64>>> + Send + 'a>,
    > {
        Box::pin(async move { append_tool_output(self, text).await })
    }
}

impl std::fmt::Debug for ToolOutputBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolOutputBinding")
            .field("tool_call_doc_id", &self.tool_call_doc_id)
            .field("request_doc_id", &self.request_doc_id)
            .field("session_id", &self.session_id)
            .field("agent_did", &self.agent_did)
            .field("requester_did", &self.requester_did)
            .finish()
    }
}

use crate::graphql::created_doc_id;

pub(crate) fn render_presentation(
    source: &str,
    presentation: &PayloadPresentation,
) -> Result<String> {
    match presentation {
        PayloadPresentation::Full => Ok(source.to_owned()),
        PayloadPresentation::Composed { parts } => {
            let mut rendered = String::new();
            for part in parts {
                match part {
                    gents_protocol::output::PresentationPart::Literal { text } => {
                        rendered.push_str(text)
                    }
                    gents_protocol::output::PresentationPart::OutputRange {
                        start_byte,
                        end_byte,
                    } => {
                        let start = usize::try_from(*start_byte)?;
                        let end = usize::try_from(*end_byte)?;
                        anyhow::ensure!(
                            start <= end
                                && end <= source.len()
                                && source.is_char_boundary(start)
                                && source.is_char_boundary(end),
                            "tool presentation range is not a UTF-8 source boundary"
                        );
                        rendered.push_str(&source[start..end]);
                    }
                }
            }
            Ok(rendered)
        }
    }
}

/// The longest UTF-8-safe byte suffix within the diagnostic budget. This
/// selection is shared with captured process output and terminal presentation.
pub(crate) fn terminal_output_tail(source: &str, budget: usize) -> &str {
    let mut start = source.len().saturating_sub(budget);
    while !source.is_char_boundary(start) {
        start += 1;
    }
    &source[start..]
}

/// Select only committed tool-source bytes. The terminal reason is a literal
/// presentation part and never rewrites the raw source or waits for the tool.
pub(crate) fn terminal_diagnostic_presentation(
    raw: &str,
    cause: &str,
    tail_budget: usize,
) -> Result<PayloadPresentation> {
    use gents_protocol::output::PresentationPart;

    let tail = terminal_output_tail(raw, tail_budget);
    let start = raw.len() - tail.len();
    let parts = if start == raw.len() {
        vec![PresentationPart::Literal {
            text: cause.to_owned(),
        }]
    } else {
        vec![
            PresentationPart::OutputRange {
                start_byte: u64::try_from(start)?,
                end_byte: u64::try_from(raw.len())?,
            },
            PresentationPart::Literal { text: "\n".into() },
            PresentationPart::Literal {
                text: cause.to_owned(),
            },
        ]
    };
    Ok(PayloadPresentation::Composed { parts })
}

struct TerminalOutputPlan<'a> {
    raw: &'a str,
    presentation: PayloadPresentation,
    rendered: String,
    /// An interrupted-call diagnostic: a tail of `raw`, a newline, the cause.
    diagnostic: bool,
}

/// Whether `stored` is exactly the diagnostic `terminal_diagnostic_presentation`
/// produces for `raw` and `cause` under some tail budget (Lean
/// `TerminalDiagnosticReplayContracts.accepted`). The budget follows
/// configuration, which may change between delivery and replay, so it is
/// recovered from the stored presentation itself: the suffix range's length,
/// or zero for a cause-only diagnostic. Any other shape is rejected.
fn diagnostic_replay_matches(raw: &str, cause: &str, stored: &PayloadPresentation) -> Result<bool> {
    use gents_protocol::output::PresentationPart;
    let budget = match stored {
        PayloadPresentation::Composed { parts } => match parts.first() {
            Some(PresentationPart::Literal { .. }) if parts.len() == 1 => 0,
            Some(PresentationPart::OutputRange { start_byte, .. }) => {
                match usize::try_from(*start_byte)
                    .ok()
                    .and_then(|start| raw.len().checked_sub(start))
                {
                    Some(budget) => budget,
                    None => return Ok(false),
                }
            }
            _ => return Ok(false),
        },
        PayloadPresentation::Full => return Ok(false),
    };
    Ok(terminal_diagnostic_presentation(raw, cause, budget)? == *stored)
}

/// `output_budget` bounds an interrupted call's diagnostic tail; it comes from
/// current configuration (`ToolCallLifecycle::output_budget`) and is `None`
/// only for a completed call, whose presentation its tool prepared.
fn terminal_output_plan<'a>(
    prefix: &'a str,
    text: &'a str,
    pending_raw: Option<&'a str>,
    prepared: Option<&PayloadPresentation>,
    state: ToolCallState,
    output_budget: Option<usize>,
) -> Result<TerminalOutputPlan<'a>> {
    let candidate_raw = if prepared.is_some() {
        pending_raw.unwrap_or(prefix)
    } else {
        text
    };
    if state != ToolCallState::Completed && !candidate_raw.starts_with(prefix) {
        let output_budget =
            output_budget.context("interrupted tool call has no resolved output budget")?;
        let presentation = terminal_diagnostic_presentation(prefix, text, output_budget)?;
        let rendered = render_presentation(prefix, &presentation)?;
        return Ok(TerminalOutputPlan {
            raw: prefix,
            presentation,
            rendered,
            diagnostic: true,
        });
    }
    anyhow::ensure!(
        candidate_raw.starts_with(prefix),
        "terminal raw tool result is not an exact extension of persisted raw output"
    );
    let presentation = prepared.cloned().unwrap_or(PayloadPresentation::Full);
    let rendered = render_presentation(candidate_raw, &presentation)?;
    anyhow::ensure!(
        rendered == text,
        "prepared tool presentation does not reconstruct to the terminal native text"
    );
    Ok(TerminalOutputPlan {
        raw: candidate_raw,
        presentation,
        rendered,
        diagnostic: false,
    })
}

fn terminal_metadata_matches(
    row: &serde_json::Value,
    fields: TerminalFields<'_>,
    terminal_status: &str,
) -> bool {
    row["status"].as_str() == Some(terminal_status)
        && row["tool_failure_class"].as_str() == fields.failure.map(FailureClass::as_str)
        && row["cancel_cause"].as_str() == fields.cancel.map(CancelCause::as_str)
}

impl ToolCallLifecycle {
    pub(super) async fn start_running_spawned_with_time(
        &mut self,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let doc_id = self
            .doc_id
            .clone()
            .context("spawned dispatch requires child physical identity")?;
        let parent_doc_id = self
            .spawned_by_tool_call_doc_id
            .clone()
            .context("spawned dispatch requires accepted parent provenance")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("spawned dispatch requires request binding")?;
        let session_id = self.session_id.clone();
        let agent_did = self.agent_did.clone();
        let requester_did = self.requester_did.clone();
        let tool_call_id = self.tool_call_id.clone();
        let tool_name = self.tool_name.clone();
        let message_sequence = self.message_sequence;
        let started = ConfigAccess::transact_local_idempotent(
            &self.node, None, IdempotentTransactionRetry::Standard,
            "tool_call.start_running_spawned_background",
            move |txn| {
                let doc_id = doc_id.clone(); let parent_doc_id = parent_doc_id.clone();
                let request_doc_id = request_doc_id.clone(); let session_id = session_id.clone();
                let agent_did = agent_did.clone(); let requester_did = requester_did.clone();
                let tool_call_id = tool_call_id.clone(); let tool_name = tool_name.clone();
                Box::pin(async move {
                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let doc = escape_graphql_string(&doc_id);
                    let parent = escape_graphql_string(&parent_doc_id);
                    let request = escape_graphql_string(&request_doc_id);
                    let session = escape_graphql_string(&session_id);
                    let agent = escape_graphql_string(&agent_did);
                    let requester_filter = requester_did.as_deref().map(|value| format!(r#", requester_did: {{ _eq: "{}" }}"#, escape_graphql_string(value)))
                        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
                    let mutation = txn.execute(&format!(r#"mutation {{ update_AgentToolCall(filter: {{
                        _docID: {{ _eq: "{doc}" }}, spawned_by_tool_call_doc_id: {{ _eq: "{parent}" }},
                        request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }},
                        agent_did: {{ _eq: "{agent}" }}, tool_call_id: {{ _eq: "{}" }},
                        tool_name: {{ _eq: "{}" }}, message_sequence: {{ _eq: {message_sequence} }},
                        await_mode: {{ _eq: "background" }}, child_request_id: {{ _eq: null }},
                        lifecycle_state: {{ _eq: "pending" }}{requester_filter}
                    }}, input: {{ lifecycle_state: "running", started_at: "{}" }}) {{ _docID }} }}"#,
                        escape_graphql_string(&tool_call_id), escape_graphql_string(&tool_name),
                        escape_graphql_string(&now.to_rfc3339_opts(SecondsFormat::Nanos, true)))).await?;
                    Ok(mutation["data"]["update_AgentToolCall"].as_array()
                        .is_some_and(|rows| !rows.is_empty()).then_some(now))
                })
            }
        ).await?;
        let started = started.context("spawned background child is no longer pending")?;
        self.state = ToolCallState::Running;
        self.started_at = Some(started);
        Ok(())
    }

    /// Admit exactly one childless native background execution from this
    /// running, accepted `spawn_process` call.  The provider publication
    /// already created *this* row; the child is deliberately created here and
    /// has no provider ToolCall block of its own.  Its deterministic key makes
    /// a retry resolve the same parent provenance instead of minting a second
    /// background effect.
    pub(crate) async fn admit_spawned_background(
        &mut self,
        admission: super::SpawnedBackgroundToolAdmission,
        receipt: &str,
    ) -> Result<ToolCallLifecycle> {
        let child = self
            .admit_spawned_background_child_with_time(admission, None)
            .await?;
        // This is a second transaction. A crash between child admission and
        // parent receipt leaves a real replayable intermediate state.
        if self.state == ToolCallState::Running {
            self.complete(receipt).await?;
        }
        Ok(child)
    }

    #[cfg(test)]
    pub(crate) async fn admit_spawned_background_child_at(
        &mut self,
        admission: super::SpawnedBackgroundToolAdmission,
        now: DateTime<Utc>,
    ) -> Result<ToolCallLifecycle> {
        self.admit_spawned_background_child_with_time(admission, Some(now))
            .await
    }

    async fn admit_spawned_background_child_with_time(
        &mut self,
        admission: super::SpawnedBackgroundToolAdmission,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<ToolCallLifecycle> {
        anyhow::ensure!(
            matches!(
                self.state,
                ToolCallState::Running | ToolCallState::Completed
            ) && !self.is_spawned_background()
                && self.tool_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME,
            "spawned background admission requires the accepted spawn_process owner"
        );
        let parent_doc_id = self
            .doc_id
            .clone()
            .context("spawned admission requires parent document")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("spawned admission requires parent request")?;
        let accepted_header_doc_id = self
            .accepted_header_doc_id
            .clone()
            .context("spawned admission requires parent accepted header")?;
        let generation = self
            .execution_generation
            .clone()
            .context("spawned admission requires parent execution generation")?;
        let session_id = self.session_id.clone();
        let agent_did = self.agent_did.clone();
        let requester_did = self.requester_did.clone();
        let message_sequence = self.message_sequence;
        let parent_tool_call_id = self.tool_call_id.clone();
        let deadline_at = admission.deadline_at;
        let tool_name = admission.tool_name;
        let persisted_selected = admission.selected_tool_identity;
        let stable_id = format!("spawned:{parent_doc_id}");
        let stable_key = format!("{parent_doc_id}:spawned-background");
        let persisted_parent_doc_id = parent_doc_id.clone();
        let persisted_request_doc_id = request_doc_id.clone();
        let persisted_accepted_header_doc_id = accepted_header_doc_id.clone();
        let persisted_generation = generation.clone();
        let persisted_session_id = session_id.clone();
        let persisted_agent_did = agent_did.clone();
        let persisted_requester_did = requester_did.clone();
        let persisted_parent_tool_call_id = parent_tool_call_id.clone();
        let persisted_tool_name = tool_name.clone();
        let persisted_stable_id = stable_id.clone();
        let persisted_stable_key = stable_key.clone();

        let created = ConfigAccess::transact_local_idempotent(
            &self.node, None, IdempotentTransactionRetry::Standard,
            "tool_call.admit_spawned_background",
            move |txn| {
                let parent_doc_id = persisted_parent_doc_id.clone();
                let request_doc_id = persisted_request_doc_id.clone();
                let accepted_header_doc_id = persisted_accepted_header_doc_id.clone();
                let generation = persisted_generation.clone();
                let session_id = persisted_session_id.clone();
                let agent_did = persisted_agent_did.clone();
                let requester_did = persisted_requester_did.clone();
                let parent_tool_call_id = persisted_parent_tool_call_id.clone();
                let tool_name = persisted_tool_name.clone();
                let stable_id = persisted_stable_id.clone();
                let stable_key = persisted_stable_key.clone();
                let selected = persisted_selected.clone();
                let fixture_now = fixture_now.clone();
                Box::pin(async move {
                    let parent = escape_graphql_string(&parent_doc_id);
                    let request = escape_graphql_string(&request_doc_id);
                    let session = escape_graphql_string(&session_id);
                    let agent = escape_graphql_string(&agent_did);
                    let requester_filter = requester_did.as_deref().map(|value| format!(
                        r#", requester_did: {{ _eq: "{}" }}"#, escape_graphql_string(value)
                    )).unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
                    let existing = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{
                        spawned_by_tool_call_doc_id: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
                        session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}{requester_filter}
                    }}, limit: 2) {{ _docID tool_call_key tool_call_id tool_name message_sequence
                        lifecycle_state await_mode cancel_policy child_request_id spawned_by_tool_call_doc_id deadline_at
                        selected_service_id selected_tool_name }} }}"#)).await?;
                    let rows = existing["data"]["AgentToolCall"].as_array()
                        .context("spawned admission lookup omitted rows")?;
                    anyhow::ensure!(rows.len() <= 1, "accepted spawn_process already has ambiguous background children");
                    if let Some(child) = rows.first() {
                        let persisted_deadline = child["deadline_at"].as_str()
                            .context("spawned replay omitted child deadline")?;
                        let persisted_deadline = DateTime::parse_from_rfc3339(persisted_deadline)?
                            .with_timezone(&Utc);
                        anyhow::ensure!(
                            child["tool_call_key"].as_str() == Some(stable_key.as_str())
                                && child["tool_call_id"].as_str() == Some(stable_id.as_str())
                                && child["tool_name"].as_str() == Some(tool_name.as_str())
                                && child["message_sequence"].as_u64() == Some(u64::from(message_sequence))
                                && child["await_mode"].as_str() == Some("background")
                                && child["cancel_policy"].as_str() == Some("cascade")
                                && child["child_request_id"].is_null()
                                && child["selected_service_id"].as_str()
                                    == selected.as_ref().map(|(service, _)| service.as_str())
                                && child["selected_tool_name"].as_str()
                                    == selected.as_ref().map(|(_, tool)| tool.as_str())
                                && persisted_deadline == deadline_at,
                            "spawned admission replay conflicts with immutable parent provenance"
                        );
                        return Ok(child["_docID"].as_str().context("spawned replay omitted child physical ID")?.to_owned());
                    }

                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let (accepted, _) = crate::session::load_canonical_message_in_txn(
                        txn,
                        &accepted_header_doc_id,
                        &agent_did,
                        requester_did.as_deref(),
                    )
                    .await?;
                    anyhow::ensure!(
                        accepted.session_id == session_id
                            && accepted.request_doc_id.as_deref() == Some(request_doc_id.as_str())
                            && accepted.role == MessageRole::Assistant
                            && accepted.outcome == OutputOutcome::Complete
                            && accepted.sequence == message_sequence
                            && matches!(&accepted.publication,
                                MessagePublication::RequestExecution { execution_generation }
                                if execution_generation == &generation)
                            && accepted.blocks.iter().any(|block| matches!(block,
                                MessageBlock::ToolCall { tool_call_doc_id, id, name, .. }
                                if tool_call_doc_id == &parent_doc_id
                                    && id == &parent_tool_call_id
                                    && name == crate::toolset::SPAWN_PROCESS_TOOL_NAME)),
                        "spawned admission parent is not exactly bound by its accepted provider header"
                    );
                    let parent_row = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{
                        _docID: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
                        session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }},
                        tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "spawn_process" }},
                        message_sequence: {{ _eq: {message_sequence} }}, lifecycle_state: {{ _eq: "running" }}{requester_filter}
                    }}, limit: 2) {{ _docID spawned_by_tool_call_doc_id }} }}"#, escape_graphql_string(&parent_tool_call_id))).await?;
                    anyhow::ensure!(parent_row["data"]["AgentToolCall"].as_array().is_some_and(|rows|
                        rows.len() == 1 && rows[0]["spawned_by_tool_call_doc_id"].is_null()),
                        "spawned admission lost its exact running accepted parent");
                    let request_row = txn.execute(&format!(r#"{{ AgentRequest(filter: {{
                        _docID: {{ _eq: "{request}" }}, agent_did: {{ _eq: "{agent}" }}, session_id: {{ _eq: "{session}" }}
                    }}, limit: 2) {{ request_id requester_did lifecycle_state execution_generation execution_lease_expires_at interrupt_requested_at }} }}"#)).await?;
                    let requests = request_row["data"]["AgentRequest"].as_array()
                        .context("spawned admission request lookup omitted rows")?;
                    anyhow::ensure!(requests.len() == 1, "spawned admission request is missing or ambiguous");
                    let request_row = &requests[0];
                    anyhow::ensure!(
                        request_row["requester_did"].as_str() == requester_did.as_deref()
                            && request_row["lifecycle_state"].as_str() == Some("processing")
                            && request_row["execution_generation"].as_str() == Some(generation.as_str())
                            && request_row["interrupt_requested_at"].is_null(),
                        "spawned admission lost live request generation ownership"
                    );
                    let expires_at = request_row["execution_lease_expires_at"].as_str()
                        .context("spawned admission request lacks lease")?;
                    anyhow::ensure!(DateTime::parse_from_rfc3339(expires_at)?.with_timezone(&Utc) > now,
                        "spawned admission request lease has expired");
                    let request_id = request_row["request_id"].as_str()
                        .filter(|id| !id.trim().is_empty()).context("spawned admission request lacks logical identity")?;
                    let deadline = escape_graphql_string(&deadline_at.to_rfc3339_opts(SecondsFormat::Nanos, true));
                    let selected_fields = match &selected {
                        Some((service, tool)) => format!(
                            r#"selected_service_id: "{}", selected_tool_name: "{}","#,
                            escape_graphql_string(service), escape_graphql_string(tool)),
                        None => String::new(),
                    };
                    let created = txn.execute(&format!(r#"mutation {{ create_AgentToolCall(input: {{
                        tool_call_key: "{}", request_id: "{}", request_doc_id: "{request}",
                        session_id: "{session}", agent_did: "{agent}", {}
                        message_sequence: {message_sequence}, tool_name: "{}", tool_call_id: "{}",
                        lifecycle_state: "pending", status: "pending", deadline_at: "{deadline}",
                        await_mode: "background", cancel_policy: "cascade", child_request_id: null,
                        {selected_fields} spawned_by_tool_call_doc_id: "{parent}"
                    }}) {{ _docID }} }}"#,
                        escape_graphql_string(&stable_key), escape_graphql_string(request_id),
                        crate::session::requester_did_create_field(requester_did.as_deref()),
                        escape_graphql_string(&tool_name), escape_graphql_string(&stable_id))).await?;
                    created_doc_id(&created, "AgentToolCall")
                })
            }
        ).await?;

        ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            &created,
            &self.agent_did,
            &self.session_id,
            self.requester_did.as_deref(),
        )
        .await?
        .context("spawned admission created child could not be rehydrated by physical provenance")
    }
    pub(crate) fn tool_output_binding(&self) -> Result<ToolOutputBinding> {
        anyhow::ensure!(
            self.state == ToolCallState::Running,
            "raw tool output requires a running admitted lifecycle"
        );
        Ok(ToolOutputBinding {
            node: self.node.clone(),
            tool_call_doc_id: self
                .doc_id
                .clone()
                .context("raw tool output requires a physical tool row")?,
            request_doc_id: self
                .request_doc_id
                .clone()
                .context("raw tool output requires a request binding")?,
            session_id: self.session_id.clone(),
            agent_did: self.agent_did.clone(),
            requester_did: self.requester_did.clone(),
        })
    }

    /// One dispatch-admission fence: immutable accepted header binding,
    /// physical tool row, and the current request lease are observed with the
    /// pending-to-running compare under the same mutation gate.
    pub(super) async fn start_running_canonical_with_time(
        &mut self,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let header = self
            .accepted_header_doc_id
            .clone()
            .context("dispatch requires an accepted tool header binding")?;
        let request = self
            .request_doc_id
            .clone()
            .context("dispatch requires an accepted request binding")?;
        let generation = self
            .execution_generation
            .clone()
            .context("dispatch requires the accepted execution generation")?;
        let arguments = self
            .arguments
            .clone()
            .context("dispatch requires an accepted argument reference")?;
        let agent = self.agent_did.clone();
        let requester = self.requester_did.clone();
        let session = self.session_id.clone();
        let doc_id = self
            .doc_id
            .clone()
            .context("dispatch requires a physical tool row")?;
        let id = self.tool_call_id.clone();
        let call_id = self.call_id.clone();
        let message_sequence = self.message_sequence;
        let name = self.tool_name.clone();
        let selected_tool_fields = self.selected_tool_fields_fragment();
        let await_mode = self.await_mode.as_str();
        let cancel_policy = self.cancel_policy.as_str();
        let expected_child_identity = match (
            self.child_request_id.as_deref(),
            self.spawn_target_did.as_deref(),
            self.spawn_behavior_id.as_deref(),
        ) {
            (Some(child_request_id), Some(spawn_target_did), Some(spawn_behavior_id)) => Some((
                child_request_id.to_owned(),
                spawn_target_did.to_owned(),
                spawn_behavior_id.to_owned(),
            )),
            (None, None, None) => None,
            _ => anyhow::bail!("subagent bridge dispatch has an incomplete child identity"),
        };
        let unclaimed_deadline_field = self
            .unclaimed_deadline_at
            .map(|deadline| {
                format!(
                    "unclaimed_deadline_at: \"{}\"",
                    deadline.to_rfc3339_opts(SecondsFormat::Nanos, true)
                )
            })
            .unwrap_or_else(|| "unclaimed_deadline_at: null".to_string());
        let started_at = ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            "tool_call.start_running_canonical",
            move |txn| {
                let header = header.clone();
                let request = request.clone();
                let generation = generation.clone();
                let arguments = arguments.clone();
                let agent = agent.clone();
                let requester = requester.clone();
                let session = session.clone();
                let doc_id = doc_id.clone();
                let id = id.clone();
                let call_id = call_id.clone();
                let name = name.clone();
                let selected_tool_fields = selected_tool_fields.clone();
                let expected_child_identity = expected_child_identity.clone();
                let unclaimed_deadline_field = unclaimed_deadline_field.clone();
                Box::pin(async move {
                    // This is sampled only after the transaction has acquired
                    // the mutation gate. A queued dispatcher must not use an
                    // earlier timestamp to run past a lease that expired while
                    // it waited.
                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let started_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
                    let (message, _) = crate::session::load_canonical_message_in_txn(
                        txn,
                        &header,
                        &agent,
                        requester.as_deref(),
                    )
                    .await?;
                    anyhow::ensure!(
                        message.session_id == session
                            && message.request_doc_id.as_deref() == Some(request.as_str())
                            && message.role == MessageRole::Assistant
                            && message.outcome == OutputOutcome::Complete
                            && message.sequence == message_sequence
                            && matches!(message.publication, MessagePublication::RequestExecution {
                                execution_generation: ref accepted_generation
                            } if accepted_generation == &generation)
                            && message.blocks.iter().any(|block| matches!(block,
                                MessageBlock::ToolCall { tool_call_doc_id, id: block_id,
                                    call_id: block_call_id, name: block_name,
                                    arguments: block_arguments, .. }
                                if tool_call_doc_id == &doc_id && block_id == &id
                                    && block_call_id == &call_id && block_name == &name
                                    && block_arguments == &arguments)),
                        "accepted header does not bind this exact dispatch"
                    );
                    let request_id = escape_graphql_string(&request);
                    let request_row = txn
                        .execute(&format!(
                            r#"{{ AgentRequest(filter: {{
                        _docID: {{ _eq: "{request_id}" }}, agent_did: {{ _eq: "{}" }},
                        session_id: {{ _eq: "{}" }}
                    }}, limit: 2) {{
                        _docID requester_did lifecycle_state execution_generation
                        execution_lease_expires_at interrupt_requested_at
                    }} }}"#,
                            escape_graphql_string(&agent),
                            escape_graphql_string(&session)
                        ))
                        .await?;
                    let rows = request_row["data"]["AgentRequest"]
                        .as_array()
                        .context("dispatch request lookup omitted rows")?;
                    anyhow::ensure!(rows.len() == 1, "dispatch request is missing or ambiguous");
                    let row = &rows[0];
                    if row["requester_did"].as_str() != requester.as_deref()
                        || row["lifecycle_state"].as_str() != Some("processing")
                        || row["execution_generation"].as_str() != Some(generation.as_str())
                        || !row["interrupt_requested_at"].is_null()
                    {
                        return Err(ToolDispatchRejection::LostRequestOwnership.into());
                    }
                    let expiry = row["execution_lease_expires_at"]
                        .as_str()
                        .context("dispatch request lease deadline missing")?;
                    let expiry = DateTime::parse_from_rfc3339(expiry)?.with_timezone(&Utc);
                    anyhow::ensure!(expiry > now, "dispatch request lease has expired");

                    let requester_filter = requester
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
                        session_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}{requester_filter}
                    }}, limit: 2) {{ child_request_id spawn_target_did spawn_behavior_id }} }}"#,
                            escape_graphql_string(&doc_id),
                            escape_graphql_string(&session),
                            escape_graphql_string(&agent)
                        ))
                        .await?;
                    let physical_rows = physical["data"]["AgentToolCall"]
                        .as_array()
                        .context("dispatch physical tool lookup omitted rows")?;
                    anyhow::ensure!(
                        physical_rows.len() == 1,
                        "dispatch physical tool is missing or ambiguous"
                    );
                    let persisted_child = (
                        physical_rows[0]["child_request_id"].as_str(),
                        physical_rows[0]["spawn_target_did"].as_str(),
                        physical_rows[0]["spawn_behavior_id"].as_str(),
                    );
                    let expected_child = expected_child_identity
                        .as_ref()
                        .map(|(child, target, behavior)| {
                            (
                                Some(child.as_str()),
                                Some(target.as_str()),
                                Some(behavior.as_str()),
                            )
                        })
                        .unwrap_or((None, None, None));
                    anyhow::ensure!(
                        persisted_child == expected_child,
                        "dispatch immutable bridge identity differs from accepted genesis"
                    );
                    let update = txn
                        .execute(&format!(
                            r#"mutation {{ update_AgentToolCall(filter: {{
                        _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{request_id}" }},
                        session_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }},
                        tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }},
                        message_sequence: {{ _eq: {message_sequence} }},
                        lifecycle_state: {{ _eq: "pending" }}{requester_filter}
                    }}, input: {{ lifecycle_state: "running", started_at: "{started_at}",
                        await_mode: "{await_mode}", cancel_policy: "{cancel_policy}",
                        {unclaimed_deadline_field}, {selected_tool_fields}
                    }}) {{ _docID }} }}"#,
                            escape_graphql_string(&doc_id),
                            escape_graphql_string(&session),
                            escape_graphql_string(&agent),
                            escape_graphql_string(&id),
                            escape_graphql_string(&name)
                        ))
                        .await?;
                    Ok(update["data"]["update_AgentToolCall"]
                        .as_array()
                        .is_some_and(|rows| !rows.is_empty())
                        .then_some(now))
                })
            },
        )
        .await?;
        let started_at = started_at
            .context("accepted tool lifecycle binding was altered or is no longer pending")?;
        self.state = ToolCallState::Running;
        self.started_at = Some(started_at);
        Ok(())
    }

    /// Publish the one immediate native receipt for a background subagent
    /// bridge without terminalizing the bridge or closing its ToolCall source.
    /// The receipt has its own immutable authored source, owned by the
    /// physical bridge document; verified child completion is responsible for
    /// the later ToolCall-source closure and ordinary notification.
    pub(crate) async fn publish_background_receipt(&mut self, text: &str) -> Result<bool> {
        anyhow::ensure!(
            (self.state == ToolCallState::Running || self.state.is_terminal())
                && self.await_mode == super::AwaitMode::Background
                && self.is_subagent_bridge()
                && !self.is_spawned_background(),
            "background receipt requires an accepted background bridge"
        );
        let tool_doc_id = self
            .doc_id
            .clone()
            .context("background receipt requires tool document")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("background receipt requires request document")?;
        let accepted_header_doc_id = self
            .accepted_header_doc_id
            .clone()
            .context("background receipt requires accepted header")?;
        let arguments = self
            .arguments
            .clone()
            .context("background receipt requires accepted arguments")?;
        let generation = self
            .execution_generation
            .clone()
            .context("background receipt requires execution generation")?;
        let agent_did = self.agent_did.clone();
        let requester_did = self.requester_did.clone();
        let session_id = self.session_id.clone();
        let tool_call_id = self.tool_call_id.clone();
        let call_id = self.call_id.clone();
        let tool_name = self.tool_name.clone();
        let message_sequence = self.message_sequence;
        let receipt = text.to_owned();
        let key = format!("{session_id}:background-receipt:{tool_doc_id}");
        let published = ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            "tool_call.publish_background_receipt",
            move |txn| {
                let tool_doc_id = tool_doc_id.clone();
                let request_doc_id = request_doc_id.clone();
                let accepted_header_doc_id = accepted_header_doc_id.clone();
                let arguments = arguments.clone();
                let generation = generation.clone();
                let agent_did = agent_did.clone();
                let requester_did = requester_did.clone();
                let session_id = session_id.clone();
                let tool_call_id = tool_call_id.clone();
                let call_id = call_id.clone();
                let tool_name = tool_name.clone();
                let receipt = receipt.clone();
                let key = key.clone();
                Box::pin(async move {
                    let tool = escape_graphql_string(&tool_doc_id);
                    let request = escape_graphql_string(&request_doc_id);
                    let agent = escape_graphql_string(&agent_did);
                    let session = escape_graphql_string(&session_id);
                    let requester_filter = requester_did.as_deref().map(|value|
                        format!(r#", requester_did: {{ _eq: "{}" }}"#, escape_graphql_string(value))
                    ).unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
                    let existing = txn.execute(&format!(r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{}" }}, session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#, escape_graphql_string(&key))).await?;
                    let existing_rows = existing["data"]["AgentMessage"].as_array().context("background receipt lookup omitted messages")?;
                    anyhow::ensure!(existing_rows.len() <= 1, "background receipt is ambiguous");
                    if let Some(row) = existing_rows.first() {
                        let existing = decode_transcript_message_row(row)?;
                        anyhow::ensure!(
                            existing.message.request_doc_id.as_deref() == Some(request_doc_id.as_str())
                                && existing.message.session_id == session_id
                                && existing.message.agent_did == agent_did
                                && existing.message.requester_did.as_deref() == requester_did.as_deref()
                                && existing.message.role == MessageRole::User
                                && existing.message.outcome == OutputOutcome::Complete
                                && matches!(existing.message.publication, MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &tool_doc_id)
                                && matches!(existing.message.blocks.as_slice(), [MessageBlock::ToolResult { tool_call_doc_id, id, call_id: existing_call_id, .. }]
                                    if tool_call_doc_id == &tool_doc_id && id == &tool_call_id && existing_call_id.as_deref() == call_id.as_deref()),
                            "background receipt replay conflicts with physical tool binding"
                        );
                        let close_doc_id = match existing.message.blocks.as_slice() {
                            [MessageBlock::ToolResult { parts, .. }] => match parts.as_slice() {
                                [gents_protocol::output::ToolResultPart::Text { text }]
                                    if text.presentation == PayloadPresentation::Full && text.output.stream == 0 =>
                                { &text.output.close_doc_id },
                                _ => anyhow::bail!("background receipt replay has a non-native payload reference"),
                            },
                            _ => unreachable!("physical binding guard established one ToolResult"),
                        };
                        let close = txn.execute(&format!(r#"{{ AgentOutputSegment(filter: {{ _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#, escape_graphql_string(close_doc_id))).await?;
                        let close_rows = close["data"]["AgentOutputSegment"].as_array()
                            .context("background receipt replay omitted authored close")?;
                        anyhow::ensure!(close_rows.len() == 1, "background receipt replay close is missing or ambiguous");
                        let close = decode_output_segment_row(&close_rows[0])?;
                        anyhow::ensure!(
                            close.segment.source == OutputSource::Authored { key: key.clone() }
                                && close.segment.writer == OutputWriter::ToolExecution { tool_call_doc_id: tool_doc_id.clone() }
                                && close.segment.payload == receipt
                                && matches!(close.segment.close, Some(SourceClose::Closed {
                                    outcome: OutputOutcome::Complete, segments: 1, ref stream_bytes
                                }) if stream_bytes.as_slice() == [receipt.len() as u64]),
                            "background receipt replay conflicts with immutable authored delivery"
                        );
                        return Ok(false);
                    }
                    let (accepted, _) = crate::session::load_canonical_message_in_txn(txn, &accepted_header_doc_id, &agent_did, requester_did.as_deref()).await?;
                    anyhow::ensure!(
                        accepted.session_id == session_id
                            && accepted.request_doc_id.as_deref() == Some(request_doc_id.as_str())
                            && accepted.role == MessageRole::Assistant
                            && accepted.outcome == OutputOutcome::Complete
                            && accepted.sequence == message_sequence
                            && matches!(accepted.publication, MessagePublication::RequestExecution { ref execution_generation } if execution_generation == &generation)
                            && accepted.blocks.iter().any(|block| matches!(block,
                                MessageBlock::ToolCall { tool_call_doc_id, id, call_id: accepted_call_id, name, arguments: accepted_arguments, .. }
                                if tool_call_doc_id == &tool_doc_id && id == &tool_call_id && accepted_call_id.as_deref() == call_id.as_deref()
                                    && name == &tool_name && accepted_arguments == &arguments)),
                        "background receipt lacks exact accepted invocation"
                    );
                    let bridge = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}, tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }}, message_sequence: {{ _eq: {message_sequence} }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}{requester_filter} }}, limit: 2) {{ _docID }} }}"#, escape_graphql_string(&tool_call_id), escape_graphql_string(&tool_name))).await?;
                    anyhow::ensure!(bridge["data"]["AgentToolCall"].as_array().is_some_and(|rows| rows.len() == 1), "background receipt bridge is no longer running");
                    let now = Utc::now();
                    let source = OutputSource::Authored { key: key.clone() };
                    let writer = OutputWriter::ToolExecution { tool_call_doc_id: tool_doc_id.clone() };
                    let segment = OutputSegment {
                        agent_did: agent_did.clone(), requester_did: requester_did.clone(),
                        session_id: session_id.clone(), request_doc_id: request_doc_id.clone(),
                        source, writer, ordinal: Some(0),
                        runs: vec![SegmentRun { stream: 0, bytes: u32::try_from(receipt.len()).context("background receipt exceeds canonical segment size")?, declaration: Some(StreamDeclaration { block_index: 0, part_index: 0, payload: StreamPayload::ToolOutput }) }],
                        payload: receipt.clone(), close: Some(SourceClose::Closed { outcome: OutputOutcome::Complete, segments: 1, stream_bytes: vec![receipt.len() as u64] }),
                        created_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
                    };
                    let segment_response = txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &output_segment_create_variables(&segment)?).await?;
                    let close_doc_id = created_doc_id(&segment_response, "AgentOutputSegment")?;
                    let receipt_fence = txn.execute(&format!(r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}{requester_filter} }}, input: {{ status: "running" }}) {{ _docID }} }}"#)).await?;
                    anyhow::ensure!(receipt_fence["data"]["update_AgentToolCall"].as_array().is_some_and(|rows| rows.len() == 1), "background receipt lost running bridge fence");
                    let sequence = next_append_sequence_in_transaction(txn, &agent_did, &session_id).await?;
                    let message = TranscriptMessage {
                        message_key: key, session_id: session_id.clone(), agent_did: agent_did.clone(), requester_did: requester_did.clone(),
                        request_doc_id: Some(request_doc_id.clone()), publication: MessagePublication::ToolDelivery { tool_call_doc_id: tool_doc_id.clone() },
                        outcome: OutputOutcome::Complete, sequence, role: MessageRole::User, native_id: None,
                        blocks: vec![MessageBlock::ToolResult { tool_call_doc_id: tool_doc_id.clone(), id: tool_call_id, call_id,
                            parts: vec![gents_protocol::output::ToolResultPart::Text { text: PresentedPayload { output: gents_protocol::output::PayloadRef { close_doc_id, stream: 0 }, presentation: PayloadPresentation::Full } }] }],
                        created_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
                    };
                    txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &transcript_message_create_variables(&message)?).await?;
                    Ok(true)
                })
            },
        ).await?;
        Ok(published)
    }

    /// Atomically close a direct tool's sole native-output stream, terminalize
    /// its accepted physical lifecycle row, and publish its native result
    /// header.  Replays discover the immutable delivery key before allocating
    /// another session sequence.
    pub(super) async fn terminalize_with_delivery(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        operation: &'static str,
    ) -> Result<bool> {
        // A never-dispatched failure still owes its sole invocation reply.
        // Only a detached, running bridge has already delivered a receipt.
        let publish_native_result = !(expected != ToolCallState::Pending
            && self.await_mode == super::AwaitMode::Background
            && self.is_subagent_bridge()
            && !self.is_spawned_background());
        self.terminalize_with_options(
            expected,
            fields,
            text,
            None,
            None,
            false,
            publish_native_result,
            operation,
        )
        .await
    }

    /// Bridge terminalization shares the canonical delivery transaction but
    /// may lose its Running compare to a different terminal owner (for
    /// example cancellation or recovery). Native tool completion remains
    /// strict; only bridge projections adopt that already-durable winner.
    pub(super) async fn terminalize_bridge_with_delivery(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        operation: &'static str,
    ) -> Result<bool> {
        let publish_native_result = !(expected != ToolCallState::Pending
            && self.await_mode == super::AwaitMode::Background
            && self.is_subagent_bridge()
            && !self.is_spawned_background());
        self.terminalize_with_options(
            expected,
            fields,
            text,
            None,
            None,
            true,
            publish_native_result,
            operation,
        )
        .await
    }

    pub(super) async fn terminalize_with_presentation(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        presentation: Option<PayloadPresentation>,
        operation: &'static str,
    ) -> Result<bool> {
        self.terminalize_with_options(
            expected,
            fields,
            text,
            None,
            presentation,
            false,
            true,
            operation,
        )
        .await
    }

    /// Atomically append the remaining exact raw bytes, close the source, and
    /// publish `rendered` through `presentation`. Unlike
    /// `terminalize_with_presentation`, this API does not require another
    /// writer to have persisted `raw` first; the lifecycle CAS and raw close
    /// share one transaction, so a losing terminal contender leaves no bytes.
    pub(super) async fn terminalize_raw_with_presentation(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        raw: &str,
        rendered: &str,
        presentation: PayloadPresentation,
        operation: &'static str,
    ) -> Result<bool> {
        self.terminalize_with_options(
            expected,
            fields,
            rendered,
            Some(raw),
            Some(presentation),
            true,
            true,
            operation,
        )
        .await
    }

    pub(super) async fn terminalize_raw_with_presentation_at(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        raw: &str,
        rendered: &str,
        presentation: PayloadPresentation,
        operation: &'static str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        self.terminalize_with_options_at(
            expected,
            fields,
            rendered,
            Some(raw),
            Some(presentation),
            true,
            true,
            operation,
            Some(now),
        )
        .await
    }

    async fn terminalize_with_options(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        pending_raw: Option<&str>,
        presentation: Option<PayloadPresentation>,
        adopt_competing_terminal: bool,
        publish_native_result: bool,
        operation: &'static str,
    ) -> Result<bool> {
        self.terminalize_with_options_at(
            expected,
            fields,
            text,
            pending_raw,
            presentation,
            adopt_competing_terminal,
            publish_native_result,
            operation,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn terminalize_with_options_at(
        &mut self,
        expected: ToolCallState,
        fields: TerminalFields<'_>,
        text: &str,
        pending_raw: Option<&str>,
        presentation: Option<PayloadPresentation>,
        adopt_competing_terminal: bool,
        publish_native_result: bool,
        operation: &'static str,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let doc_id = self
            .doc_id
            .clone()
            .context("canonical tool terminalization requires accepted physical tool identity")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("canonical tool terminalization requires accepted request binding")?;
        let accepted_header = self
            .accepted_header_doc_id
            .clone()
            .context("canonical tool terminalization requires accepted header binding")?;
        let started_at = self.started_at;
        let agent_did = self.agent_did.clone();
        let requester_did = self.requester_did.clone();
        let session_id = self.session_id.clone();
        let tool_call_id = self.tool_call_id.clone();
        let call_id = self.call_id.clone();
        let message_sequence = self.message_sequence;
        let arguments = self.arguments.clone();
        if !self.is_spawned_background() {
            anyhow::ensure!(
                arguments.is_some(),
                "canonical direct tool terminalization requires accepted argument reference"
            );
        }
        let tool_name = self.tool_name.clone();
        let deadline_at = self.deadline_at;
        let output_budget = if fields.state == ToolCallState::Completed {
            None
        } else {
            Some(self.output_budget().await)
        };
        let terminal_status = self.terminal_persistence_status(fields.completion_reason);
        let spawned_by_tool_call_doc_id = self.spawned_by_tool_call_doc_id.clone();

        let published = ConfigAccess::transact_local_idempotent(
            &self.node,
            None,
            IdempotentTransactionRetry::Standard,
            operation,
            move |txn| {
                let doc_id = doc_id.clone();
                let request_doc_id = request_doc_id.clone();
                let accepted_header = accepted_header.clone();
                let agent_did = agent_did.clone();
                let requester_did = requester_did.clone();
                let session_id = session_id.clone();
                let tool_call_id = tool_call_id.clone();
                let call_id = call_id.clone();
                let arguments = arguments.clone();
                let tool_name = tool_name.clone();
                let terminal_status = terminal_status.clone();
                let spawned_by_tool_call_doc_id = spawned_by_tool_call_doc_id.clone();
                let presentation = presentation.clone();
                Box::pin(async move {
                    let now = fixture_now.unwrap_or_else(Utc::now);
                    terminalize_transaction(
                        txn,
                        &doc_id,
                        &request_doc_id,
                        &accepted_header,
                        &agent_did,
                        requester_did.as_deref(),
                        &session_id,
                        &tool_call_id,
                        call_id.as_deref(),
                        message_sequence,
                        arguments.as_ref(),
                        spawned_by_tool_call_doc_id.as_deref(),
                        &tool_name,
                        deadline_at,
                        started_at,
                        now,
                        expected,
                        fields,
                        &terminal_status,
                        text,
                        pending_raw,
                        presentation.as_ref(),
                        output_budget,
                        adopt_competing_terminal,
                        publish_native_result,
                    )
                    .await
                })
            },
        )
        .await?;
        if published {
            self.state = fields.state;
            self.failure_class = fields.failure;
            self.cancel_cause = fields.cancel;
        }
        Ok(published)
    }
}

#[allow(clippy::too_many_arguments)]
async fn terminalize_transaction(
    txn: &ConfigApplyTxn<'_>,
    tool_doc_id: &str,
    request_doc_id: &str,
    accepted_header_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    tool_call_id: &str,
    call_id: Option<&str>,
    message_sequence: u32,
    arguments: Option<&gents_protocol::output::PayloadRef>,
    spawned_by_tool_call_doc_id: Option<&str>,
    tool_name: &str,
    deadline_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    expected: ToolCallState,
    fields: TerminalFields<'_>,
    terminal_status: &str,
    text: &str,
    pending_raw: Option<&str>,
    presentation: Option<&PayloadPresentation>,
    output_budget: Option<usize>,
    adopt_competing_terminal: bool,
    publish_native_result: bool,
) -> Result<bool> {
    let tool = escape_graphql_string(tool_doc_id);
    let request = escape_graphql_string(request_doc_id);
    let agent = escape_graphql_string(agent_did);
    let session = escape_graphql_string(session_id);
    let expected = expected.as_str();
    let tool_id = escape_graphql_string(tool_call_id);
    let name = escape_graphql_string(tool_name);
    let requester_filter = requester_did
        .map(|value| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(value)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    // A terminal bridge contender may arrive after cancellation, timeout, or
    // another differently-shaped terminal projection has already won. Those
    // owners close the ToolCall source without publishing a second native
    // result for a background receipt. Observe that competing lifecycle
    // before validating this candidate's payload; same-state retries still
    // proceed through the exact replay checks below and conflicting payloads
    // remain errors.
    if adopt_competing_terminal {
        let lifecycle = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }},
            tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
            message_sequence: {{ _eq: {message_sequence} }}{requester_filter}
        }}, limit: 2) {{ lifecycle_state status tool_failure_class cancel_cause }} }}"#
            ))
            .await?;
        let lifecycle_rows = lifecycle["data"]["AgentToolCall"]
            .as_array()
            .context("competing tool terminal lifecycle query omitted rows")?;
        anyhow::ensure!(
            lifecycle_rows.len() == 1,
            "competing tool terminal has no unique lifecycle"
        );
        let durable_state = lifecycle_rows[0]["lifecycle_state"]
            .as_str()
            .context("competing tool terminal lifecycle omitted state")?;
        if durable_state != expected && durable_state != fields.state.as_str() {
            let durable_state = ToolCallState::from_persisted(durable_state)
                .context("competing tool terminal has unknown lifecycle vocabulary")?;
            anyhow::ensure!(
                durable_state.is_terminal(),
                "tool terminal contender lost to a non-terminal lifecycle"
            );
            return Ok(false);
        }
        if durable_state == fields.state.as_str()
            && !terminal_metadata_matches(&lifecycle_rows[0], fields, terminal_status)
        {
            // The winning terminal may have the same state and rendered text
            // but a different cause. It still owns the CAS; the caller must
            // adopt its durable row rather than reject its own lost compare.
            return Ok(false);
        }
    }
    let delivery_key = format!("{session_id}:tool-delivery:{tool_doc_id}");
    let delivery_key_escaped = escape_graphql_string(&delivery_key);
    let receipt_key = format!("{session_id}:background-receipt:{tool_doc_id}");
    let receipt_key_escaped = escape_graphql_string(&receipt_key);
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };

    // The accepted header is a physical admission fence, not a latest-message
    // lookup.  It is intentionally read inside the mutation gate before the
    // lifecycle CAS so a forged/reused row cannot publish a result.
    let (accepted, _) = crate::session::load_canonical_message_in_txn(
        txn,
        accepted_header_doc_id,
        agent_did,
        requester_did,
    )
    .await?;
    if let Some(parent_doc_id) = spawned_by_tool_call_doc_id {
        let parent = escape_graphql_string(parent_doc_id);
        let parent_rows = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}{requester_filter}
        }}, limit: 2) {{ _docID tool_call_id tool_name message_sequence lifecycle_state
            spawned_by_tool_call_doc_id }} }}"#
            ))
            .await?;
        let parents = parent_rows["data"]["AgentToolCall"]
            .as_array()
            .context("spawned terminalization parent lookup omitted rows")?;
        anyhow::ensure!(
            parents.len() == 1,
            "spawned terminalization parent is missing or ambiguous"
        );
        let parent = &parents[0];
        let parent_id = parent["tool_call_id"]
            .as_str()
            .context("spawned parent lacks native ID")?;
        let parent_name = parent["tool_name"]
            .as_str()
            .context("spawned parent lacks tool name")?;
        anyhow::ensure!(
            parent["spawned_by_tool_call_doc_id"].is_null()
                && parent_name == crate::toolset::SPAWN_PROCESS_TOOL_NAME
                && parent["message_sequence"].as_u64() == Some(u64::from(message_sequence))
                && accepted.session_id == session_id
                && accepted.request_doc_id.as_deref() == Some(request_doc_id)
                && accepted.role == MessageRole::Assistant
                && accepted.outcome == OutputOutcome::Complete
                && accepted.sequence == message_sequence
                && matches!(accepted.publication, MessagePublication::RequestExecution { .. })
                && accepted.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id, name, .. }
                    if tool_call_doc_id == parent_doc_id && id == parent_id && name == parent_name)),
            "spawned terminalization has no exact accepted spawn_process parent"
        );
    } else {
        let arguments =
            arguments.context("canonical direct terminalization requires accepted arguments")?;
        anyhow::ensure!(
            accepted.session_id == session_id
                && accepted.request_doc_id.as_deref() == Some(request_doc_id)
                && accepted.role == MessageRole::Assistant
                && accepted.outcome == OutputOutcome::Complete
                && accepted.sequence == message_sequence
                && matches!(
                    accepted.publication,
                    MessagePublication::RequestExecution { .. }
                )
                && accepted.blocks.iter().any(|block| matches!(block,
                    MessageBlock::ToolCall { tool_call_doc_id, id, call_id: accepted_call_id,
                        name, arguments: accepted_arguments, .. }
                    if tool_call_doc_id == tool_doc_id && id == tool_call_id
                        && accepted_call_id.as_deref() == call_id && name == tool_name
                        && accepted_arguments == arguments)),
            "accepted tool header does not bind this exact physical invocation"
        );
    }

    // A durable background receipt is already the invocation's unique native
    // reply.  Rehydrated completion paths must discover that fact from the
    // canonical header rather than publishing a second ToolResult merely
    // because an in-memory bridge classification was incomplete.
    let receipt_rows = txn.execute(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{receipt_key_escaped}" }}, session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }}{requester_filter} }}, limit: 2) {{ _docID }} }}"#
    )).await?;
    let receipt_rows = receipt_rows["data"]["AgentMessage"]
        .as_array()
        .context("background receipt lookup omitted messages")?;
    anyhow::ensure!(receipt_rows.len() <= 1, "background receipt is ambiguous");
    let publish_native_result = publish_native_result && receipt_rows.is_empty();

    if !publish_native_result && spawned_by_tool_call_doc_id.is_none() {
        ensure_background_receipt_before_bridge_close(
            txn,
            tool_doc_id,
            request_doc_id,
            &agent_did,
            requester_did,
            session_id,
            tool_call_id,
            call_id,
        )
        .await?;
    }

    let existing = txn.execute(&format!(
        r#"{{ AgentMessage(filter: {{ message_key: {{ _eq: "{delivery_key_escaped}" }}, session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
    )).await?;
    let existing_rows = existing["data"]["AgentMessage"]
        .as_array()
        .context("tool delivery lookup omitted rows")?;
    anyhow::ensure!(
        existing_rows.len() <= 1,
        "ambiguous canonical tool delivery replay"
    );
    if publish_native_result && spawned_by_tool_call_doc_id.is_none() {
        if let Some(row) = existing_rows.first() {
            let existing = decode_transcript_message_row(row)?;
            anyhow::ensure!(
                existing.message.request_doc_id.as_deref() == Some(request_doc_id)
                    && existing.message.session_id == session_id
                    && existing.message.agent_did == agent_did
                    && existing.message.requester_did.as_deref() == requester_did
                    && existing.message.role == MessageRole::User
                    && existing.message.outcome == OutputOutcome::Complete
                    && matches!(&existing.message.publication,
                    MessagePublication::ToolDelivery { tool_call_doc_id: id } if id == tool_doc_id)
                    && matches!(existing.message.blocks.as_slice(),
                    [MessageBlock::ToolResult { tool_call_doc_id: id, id: result_id,
                        call_id: result_call_id, .. }]
                    if id == tool_doc_id && result_id == tool_call_id
                        && result_call_id.as_deref() == call_id),
                "canonical tool delivery replay conflicts with accepted invocation"
            );
            let lifecycle = txn
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }},
            tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
            message_sequence: {{ _eq: {message_sequence} }}{requester_filter}
        }}, limit: 2) {{ lifecycle_state status tool_failure_class cancel_cause }} }}"#
                ))
                .await?;
            let lifecycle_rows = lifecycle["data"]["AgentToolCall"]
                .as_array()
                .context("canonical tool delivery replay lifecycle query omitted rows")?;
            anyhow::ensure!(
                lifecycle_rows.len() == 1,
                "canonical tool delivery replay has no unique lifecycle"
            );
            let durable_state = lifecycle_rows[0]["lifecycle_state"]
                .as_str()
                .context("canonical tool delivery replay lifecycle omitted state")?;
            if adopt_competing_terminal && durable_state != fields.state.as_str() {
                let durable_state = ToolCallState::from_persisted(durable_state)
                    .context("canonical tool delivery replay has unknown lifecycle vocabulary")?;
                anyhow::ensure!(
                    durable_state.is_terminal(),
                    "canonical tool delivery exists without a terminal lifecycle"
                );
                return Ok(false);
            }
            if durable_state == fields.state.as_str() {
                anyhow::ensure!(
                    terminal_metadata_matches(&lifecycle_rows[0], fields, terminal_status),
                    "canonical tool delivery replay conflicts with terminal cause"
                );
            }

            let result_payload = match existing.message.blocks.as_slice() {
                [MessageBlock::ToolResult { parts, .. }]
                    if matches!(parts.as_slice(),
                    [gents_protocol::output::ToolResultPart::Text { text }]
                    if text.output.stream == 0) =>
                {
                    let [gents_protocol::output::ToolResultPart::Text { text }] = parts.as_slice()
                    else {
                        unreachable!("guard established the exact native text part")
                    };
                    text
                }
                _ => anyhow::bail!(
                    "canonical tool delivery replay has a non-native payload reference"
                ),
            };
            let result_output = &result_payload.output;
            let replay_rows = txn
                .execute(&format!(
                    r#"{{ AgentOutputSegment(filter: {{
            {}, request_doc_id: {{ _eq: "{request}" }}
        }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
                    crate::session::session_scope_filter(agent_did, session_id, requester_did)
                ))
                .await?;
            let replay_rows = replay_rows["data"]["AgentOutputSegment"]
                .as_array()
                .context("tool delivery replay source query omitted segments")?
                .iter()
                .map(decode_output_segment_row)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .filter(|row| row.segment.source == source)
                .collect::<Vec<_>>();
            anyhow::ensure!(
                replay_rows
                    .iter()
                    .any(|row| row.doc_id == result_output.close_doc_id
                        && row.segment.writer == writer
                        && matches!(
                            row.segment.close,
                            Some(SourceClose::Closed {
                                outcome: OutputOutcome::Complete,
                                ..
                            })
                        )),
                "canonical tool delivery replay payload does not name this tool closure"
            );
            let observed = replay_rows
                .iter()
                .map(|row| ObservedSegment {
                    doc_id: &row.doc_id,
                    segment: &row.segment,
                })
                .collect::<Vec<_>>();
            let reconstructed = reconstruct_stream(&observed, &[], &[], result_output)
                .map_err(anyhow::Error::from)?;
            let plan = terminal_output_plan(
                &reconstructed.text,
                text,
                pending_raw,
                presentation,
                fields.state,
                output_budget,
            )?;
            anyhow::ensure!(
                reconstructed.text == plan.raw
                    && if plan.diagnostic {
                        diagnostic_replay_matches(
                            &reconstructed.text,
                            text,
                            &result_payload.presentation,
                        )?
                    } else {
                        result_payload.presentation == plan.presentation
                            && render_presentation(
                                &reconstructed.text,
                                &result_payload.presentation,
                            )? == plan.rendered
                    },
                "canonical tool delivery replay payload differs from terminal plan"
            );
            return Ok(false);
        }
    }

    let source_rows = txn
        .execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{
        {}, request_doc_id: {{ _eq: "{request}" }}
    }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#,
            crate::session::session_scope_filter(agent_did, session_id, requester_did)
        ))
        .await?;
    let source_rows = source_rows["data"]["AgentOutputSegment"]
        .as_array()
        .context("tool terminal source query omitted segments")?
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|row| row.segment.source == source)
        .collect::<Vec<_>>();
    if source_rows.iter().any(|row| row.segment.close.is_some()) {
        // Another bridge projector can commit after the lifecycle pre-read
        // above and before this source read. A closed source is replayable
        // only when the exact physical lifecycle and the complete raw output
        // agree with this candidate. A twin closure or changed payload is
        // still a hard error.
        anyhow::ensure!(
            adopt_competing_terminal,
            "tool terminal source already has a closure"
        );
        let lifecycle = txn
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{
            _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
            session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }},
            tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
            message_sequence: {{ _eq: {message_sequence} }}{requester_filter}
        }}, limit: 2) {{ lifecycle_state status tool_failure_class cancel_cause }} }}"#
            ))
            .await?;
        let lifecycle_rows = lifecycle["data"]["AgentToolCall"]
            .as_array()
            .context("closed tool source lifecycle query omitted rows")?;
        anyhow::ensure!(
            lifecycle_rows.len() == 1,
            "closed tool source has no unique lifecycle"
        );
        let durable_state = lifecycle_rows[0]["lifecycle_state"]
            .as_str()
            .context("closed tool source lifecycle omitted state")?;
        anyhow::ensure!(
            durable_state == fields.state.as_str(),
            "closed tool source conflicts with terminal lifecycle"
        );
        anyhow::ensure!(
            terminal_metadata_matches(&lifecycle_rows[0], fields, terminal_status),
            "closed tool source conflicts with terminal cause"
        );
        anyhow::ensure!(
            source_rows.iter().all(|row| row.segment.writer == writer),
            "closed tool source has a foreign writer"
        );
        let closures = source_rows
            .iter()
            .filter(|row| row.segment.close.is_some())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            closures.len() == 1,
            "closed tool source has ambiguous closures"
        );
        let close = closures[0];
        anyhow::ensure!(
            matches!(
                close.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    ..
                })
            ),
            "closed tool source has a non-complete closure"
        );
        let observed = source_rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let reconstructed = reconstruct_stream(
            &observed,
            &[],
            &[],
            &gents_protocol::output::PayloadRef {
                close_doc_id: close.doc_id.clone(),
                stream: 0,
            },
        )
        .map_err(anyhow::Error::from)?;
        let plan = terminal_output_plan(
            &reconstructed.text,
            text,
            pending_raw,
            presentation,
            fields.state,
            output_budget,
        )?;
        anyhow::ensure!(
            reconstructed.text == plan.raw,
            "closed tool source differs from terminal plan"
        );
        return Ok(false);
    }
    let observed = source_rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let extent = gents_protocol::output::extent::inspect_open_source(
        &observed,
        request_doc_id,
        &source,
        &writer,
    )
    .map_err(anyhow::Error::from)?;
    anyhow::ensure!(
        extent.streams.len() <= 1,
        "tool terminal source has unexpected multiple streams"
    );
    let prefix = extent
        .streams
        .first()
        .map(|stream| stream.text.as_str())
        .unwrap_or("");
    if let Some(last_created_at) = extent.last_created_at.as_deref() {
        let last_created_at = DateTime::parse_from_rfc3339(last_created_at)
            .context("tool raw output has an invalid created_at")?
            .with_timezone(&Utc);
        anyhow::ensure!(
            now > last_created_at,
            "clock moved backwards while closing canonical tool output"
        );
    }
    let plan = terminal_output_plan(
        prefix,
        text,
        pending_raw,
        presentation,
        fields.state,
        output_budget,
    )?;
    let suffix = plan.raw[prefix.len()..].to_owned();
    let final_flush = !suffix.is_empty() || extent.segments == 0;
    let stream_bytes = vec![u64::try_from(plan.raw.len())?];
    let delivered_presentation = plan.presentation;
    let segments = extent.segments + u32::from(final_flush);
    let runs = if final_flush {
        vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(suffix.len())
                .context("tool output exceeds canonical segment size")?,
            declaration: (extent.segments == 0).then(|| StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }]
    } else {
        Vec::new()
    };
    let segment = OutputSegment {
        agent_did: agent_did.to_owned(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.to_owned(),
        request_doc_id: request_doc_id.to_owned(),
        source,
        writer,
        ordinal: final_flush.then_some(extent.segments),
        runs,
        payload: suffix,
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments,
            stream_bytes,
        }),
        created_at: now.to_rfc3339_opts(SecondsFormat::Nanos, true),
    };
    let segment_response = txn
        .execute_with_variables(
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            &output_segment_create_variables(&segment)?,
        )
        .await?;
    let close_doc_id = created_doc_id(&segment_response, "AgentOutputSegment")?;

    let completed_at = now.to_rfc3339_opts(SecondsFormat::Nanos, true);
    // A call settled from Pending never started; it records neither a start
    // nor a latency.
    let (started_at, latency_ms) = match started_at {
        Some(started) => (
            format!(
                "\"{}\"",
                started.to_rfc3339_opts(SecondsFormat::Nanos, true)
            ),
            (now - started).num_milliseconds().to_string(),
        ),
        None => ("null".to_owned(), "null".to_owned()),
    };
    let deadline_at = deadline_at.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let failure = fields
        .failure
        .map(|value| format!(r#", tool_failure_class: "{}""#, value.as_str()))
        .unwrap_or_default();
    let cancel = fields
        .cancel
        .map(|value| format!(r#", cancel_cause: "{}""#, value.as_str()))
        .unwrap_or_default();
    let remote_handoff = fields
        .remote_cancel_intent_at
        .map(|at| {
            let at = escape_graphql_string(&at.to_rfc3339_opts(SecondsFormat::Nanos, true));
            format!(r#", cancel_cascade_intent_at: "{at}", cancel_pending_remote_ack: true"#)
        })
        .unwrap_or_default();
    let state = fields.state.as_str();
    let spawned_filter = spawned_by_tool_call_doc_id
        .map(|parent| {
            format!(
                r#", spawned_by_tool_call_doc_id: {{ _eq: "{}" }}"#,
                escape_graphql_string(parent)
            )
        })
        .unwrap_or_else(|| ", spawned_by_tool_call_doc_id: { _eq: null }".to_owned());
    let lifecycle = txn.execute(&format!(r#"mutation {{ update_AgentToolCall(filter: {{
        _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
        session_id: {{ _eq: "{session}" }}, agent_did: {{ _eq: "{agent}" }},
        tool_call_id: {{ _eq: "{tool_id}" }}, tool_name: {{ _eq: "{name}" }},
        message_sequence: {{ _eq: {message_sequence} }},
        lifecycle_state: {{ _eq: "{expected}" }}{requester_filter}{spawned_filter} }}, input: {{
        status: "{}", lifecycle_state: "{state}", started_at: {started_at},
        deadline_at: "{deadline_at}", completed_at: "{completed_at}", latency_ms: {latency_ms}{failure}{cancel}{remote_handoff}
    }}) {{ _docID }} }}"#, escape_graphql_string(terminal_status))).await?;
    if !lifecycle["data"]["update_AgentToolCall"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        // Transaction rollback discards the created closure: a loser cannot
        // leave an unpaired output fact or consume a delivery sequence.
        return Ok(false);
    }

    // A spawned process is not a provider invocation.  Its terminal output
    // is closed above under its own physical source, while its later
    // notification is published by the background-completion owner.  Never
    // invent a second user ToolResult/header for it.
    if spawned_by_tool_call_doc_id.is_some() || !publish_native_result {
        return Ok(true);
    }

    let sequence = next_append_sequence_in_transaction(txn, agent_did, session_id).await?;
    let message = TranscriptMessage {
        message_key: delivery_key,
        session_id: session_id.to_owned(),
        agent_did: agent_did.to_owned(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_doc_id.to_owned()),
        publication: MessagePublication::ToolDelivery {
            tool_call_doc_id: tool_doc_id.to_owned(),
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![MessageBlock::ToolResult {
            tool_call_doc_id: tool_doc_id.to_owned(),
            id: tool_call_id.to_owned(),
            call_id: call_id.map(str::to_owned),
            parts: vec![gents_protocol::output::ToolResultPart::Text {
                text: PresentedPayload {
                    output: gents_protocol::output::PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: delivered_presentation,
                },
            }],
        }],
        created_at: completed_at,
    };
    txn.execute_with_variables(
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&message)?,
    )
    .await?;
    Ok(true)
}

/// A detached bridge receives exactly one native invocation reply while it is
/// running.  Its verified terminal transition closes the ToolCall source and
/// is reported through the background notification owner, so it must never
/// synthesize a second ToolResult for that accepted provider invocation.
async fn ensure_background_receipt_before_bridge_close(
    txn: &ConfigApplyTxn<'_>,
    tool_call_doc_id: &str,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    tool_call_id: &str,
    call_id: Option<&str>,
) -> Result<()> {
    let key = escape_graphql_string(&format!(
        "{session_id}:background-receipt:{tool_call_doc_id}"
    ));
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let response = txn
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ {scope}, message_key: {{ _eq: "{key}" }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        ))
        .await?;
    let rows = response["data"]["AgentMessage"]
        .as_array()
        .context("background receipt lookup omitted AgentMessage rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "background bridge terminalization requires one immutable receipt"
    );
    let receipt = decode_transcript_message_row(&rows[0])?;
    anyhow::ensure!(
        receipt.message.request_doc_id.as_deref() == Some(request_doc_id)
            && receipt.message.session_id == session_id
            && receipt.message.agent_did == agent_did
            && receipt.message.requester_did.as_deref() == requester_did
            && receipt.message.role == MessageRole::User
            && receipt.message.outcome == OutputOutcome::Complete
            && matches!(&receipt.message.publication,
                MessagePublication::ToolDelivery { tool_call_doc_id: id } if id == tool_call_doc_id)
            && matches!(receipt.message.blocks.as_slice(),
                [MessageBlock::ToolResult { tool_call_doc_id: id, id: result_id,
                    call_id: result_call_id, .. }]
                if id == tool_call_doc_id && result_id == tool_call_id
                    && result_call_id.as_deref() == call_id),
        "background receipt conflicts with the accepted bridge invocation"
    );
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ToolOutputAppendRejection {
    #[error("tool output append lost the running tool lifecycle")]
    NotRunning,
    #[error("tool output append exceeded the accepted tool deadline")]
    DeadlineExceeded,
}

#[derive(Debug)]
pub(crate) struct ToolOutputAppendReceipt {
    pub(crate) range: std::ops::Range<u64>,
    pub(crate) segment_doc_id: String,
}

/// Commit one raw tool-output chunk. The per-writer mutex serializes capture
/// callbacks; the transaction derives the next ordinal from immutable facts.
/// Fresh writes observe the clock under the transaction gate and are accepted
/// through the deadline (only `now > deadline` rejects). Exact committed
/// retries bypass only that deadline check, not the Running/open-source fences.
pub(crate) async fn append_tool_output(
    binding: &ToolOutputBinding,
    bytes: &str,
) -> Result<std::ops::Range<u64>> {
    if bytes.is_empty() {
        return Ok(0..0);
    }
    Ok(
        append_tool_output_with_time(binding, bytes, Utc::now(), None)
            .await?
            .range,
    )
}

#[cfg(test)]
pub(crate) async fn append_tool_output_at(
    binding: &ToolOutputBinding,
    bytes: &str,
    operation_created_at: DateTime<Utc>,
    observed_now: DateTime<Utc>,
) -> Result<ToolOutputAppendReceipt> {
    append_tool_output_with_time(binding, bytes, operation_created_at, Some(observed_now)).await
}

async fn append_tool_output_with_time(
    binding: &ToolOutputBinding,
    bytes: &str,
    operation_created_at: DateTime<Utc>,
    fixture_now: Option<DateTime<Utc>>,
) -> Result<ToolOutputAppendReceipt> {
    anyhow::ensure!(
        !bytes.is_empty(),
        "empty tool output has no physical append receipt"
    );
    let created_at = operation_created_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let payload = bytes.to_owned();
    let binding = binding.clone();
    let node = binding.node.clone();
    ConfigAccess::transact_local_idempotent(
        &node,
        None,
        IdempotentTransactionRetry::Standard,
        "tool_call.append_output_segment",
        move |txn| {
            let created_at = created_at.clone();
            let payload = payload.clone();
            let binding = binding.clone();
            Box::pin(async move {
                let now = fixture_now.unwrap_or_else(Utc::now);
                let tool = escape_graphql_string(&binding.tool_call_doc_id);
                let request = escape_graphql_string(&binding.request_doc_id);
                let agent = escape_graphql_string(&binding.agent_did);
                let session = escape_graphql_string(&binding.session_id);
                let requester_filter = binding
                    .requester_did
                    .as_deref()
                    .map(|value| {
                        format!(
                            r#"requester_did: {{ _eq: "{}" }}"#,
                            escape_graphql_string(value)
                        )
                    })
                    .unwrap_or_else(|| "requester_did: { _eq: null }".to_owned());
                let scope = crate::session::session_scope_filter(
                    &binding.agent_did,
                    &binding.session_id,
                    binding.requester_did.as_deref(),
                );
                let row = txn
                    .execute(&format!(
                        r#"{{ AgentToolCall(filter: {{
                _docID: {{ _eq: "{tool}" }}, request_doc_id: {{ _eq: "{request}" }},
                agent_did: {{ _eq: "{agent}" }}, session_id: {{ _eq: "{session}" }},
                {requester_filter}
            }}, limit: 2) {{ _docID lifecycle_state deadline_at }} }}"#
                    ))
                    .await?;
                let tools = row["data"]["AgentToolCall"]
                    .as_array()
                    .context("tool output append query omitted physical rows")?;
                anyhow::ensure!(
                    tools.len() == 1,
                    "tool output append has no unique physical binding"
                );
                let state = tools[0]["lifecycle_state"]
                    .as_str()
                    .and_then(ToolCallState::from_persisted)
                    .context("tool output append has malformed lifecycle state")?;
                if state != ToolCallState::Running {
                    return Err(ToolOutputAppendRejection::NotRunning.into());
                }
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentOutputSegment(filter: {{
                {scope}, request_doc_id: {{ _eq: "{request}" }}
            }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
                    ))
                    .await?;
                let rows = response["data"]["AgentOutputSegment"]
                    .as_array()
                    .context("tool output segment query omitted rows")?
                    .iter()
                    .map(decode_output_segment_row)
                    .collect::<Result<Vec<_>>>()?;
                let source = OutputSource::ToolCall {
                    tool_call_doc_id: binding.tool_call_doc_id.clone(),
                };
                let writer = OutputWriter::ToolExecution {
                    tool_call_doc_id: binding.tool_call_doc_id.clone(),
                };
                let rows = rows
                    .into_iter()
                    .filter(|row| row.segment.source == source)
                    .collect::<Vec<_>>();
                let same_attempt = rows
                    .iter()
                    .filter(|row| row.segment.created_at == created_at)
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    same_attempt.len() <= 1,
                    "canonical tool output append has ambiguous retry identity"
                );
                anyhow::ensure!(
                    rows.iter().all(|row| row.segment.close.is_none()),
                    "cannot append raw output after tool source closure"
                );
                let prior = rows
                    .iter()
                    .filter(|row| row.segment.created_at != created_at)
                    .map(|row| ObservedSegment {
                        doc_id: &row.doc_id,
                        segment: &row.segment,
                    })
                    .collect::<Vec<_>>();
                let extent = gents_protocol::output::extent::inspect_open_source(
                    &prior,
                    &binding.request_doc_id,
                    &source,
                    &writer,
                )
                .map_err(anyhow::Error::from)?;
                anyhow::ensure!(
                    extent.streams.len() <= 1,
                    "tool raw output source has unexpected multiple streams"
                );
                let ordinal = extent.segments;
                let start = extent.stream_bytes.first().copied().unwrap_or(0);
                let segment = OutputSegment {
                    agent_did: binding.agent_did.clone(),
                    requester_did: binding.requester_did.clone(),
                    session_id: binding.session_id.clone(),
                    request_doc_id: binding.request_doc_id.clone(),
                    source,
                    writer,
                    ordinal: Some(ordinal),
                    runs: vec![SegmentRun {
                        stream: 0,
                        bytes: u32::try_from(payload.len())
                            .context("tool output chunk too large")?,
                        declaration: (ordinal == 0).then(|| StreamDeclaration {
                            block_index: 0,
                            part_index: 0,
                            payload: StreamPayload::ToolOutput,
                        }),
                    }],
                    payload,
                    close: None,
                    created_at,
                };
                if let Some(existing) = same_attempt.first() {
                    // Retries use an operation identity selected before the
                    // transaction.  A committed acknowledgement is valid
                    // only for this exact immutable fact, not merely matching
                    // text on an arbitrary segment.
                    anyhow::ensure!(
                        existing.segment == segment,
                        "canonical tool output retry conflicts with the recorded segment"
                    );
                    return Ok(ToolOutputAppendReceipt {
                        range: start..start + segment.payload.len() as u64,
                        segment_doc_id: existing.doc_id.clone(),
                    });
                }
                let deadline = tools[0]["deadline_at"]
                    .as_str()
                    .context("tool output append has no accepted deadline")?;
                let deadline = DateTime::parse_from_rfc3339(deadline)
                    .context("tool output append has malformed accepted deadline")?
                    .with_timezone(&Utc);
                if now > deadline {
                    return Err(ToolOutputAppendRejection::DeadlineExceeded.into());
                }
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(&segment)?,
                    )
                    .await?;
                Ok(ToolOutputAppendReceipt {
                    range: start..start + segment.payload.len() as u64,
                    segment_doc_id: created_doc_id(&response, "AgentOutputSegment")?,
                })
            })
        },
    )
    .await
}

#[cfg(test)]
mod spawned_background_tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_background_bridge, published_spawn_parent,
    };
    use crate::tool_call_lifecycle::SpawnedBackgroundToolAdmission;
    use defra_node::EmbeddedNode;

    #[test]
    fn terminal_plan_preserves_empty_source_extension_before_diagnostic_fallback() {
        let case = crate::lean_vocab_test::lean_terminal_diagnostic_presentation_cases()
            .iter()
            .find(|case| case.name == "whole_prefix_then_cause")
            .expect("generated terminal diagnostic fixture");
        let raw = String::from_utf8(case.raw.clone()).unwrap();
        let cause = String::from_utf8(case.cause.clone()).unwrap();
        let empty = terminal_output_plan(
            "",
            &cause,
            Some(&cause),
            Some(&PayloadPresentation::Full),
            ToolCallState::TimedOut,
            Some(crate::toolset::DEFAULT_MAX_COMMAND_CHARS),
        )
        .unwrap();
        assert_eq!(empty.raw, cause);
        assert_eq!(empty.presentation, PayloadPresentation::Full);
        assert_eq!(empty.rendered, cause);

        let retained = terminal_output_plan(
            &raw,
            &cause,
            Some(&cause),
            Some(&PayloadPresentation::Full),
            ToolCallState::TimedOut,
            Some(crate::toolset::DEFAULT_MAX_COMMAND_CHARS),
        )
        .unwrap();
        let crate::lean_vocab_test::LeanTerminalDiagnosticPresentationExpected::Ok {
            rendered, ..
        } = &case.expected
        else {
            panic!("generated valid terminal diagnostic result");
        };
        assert_eq!(retained.raw, raw);
        assert_eq!(retained.rendered.as_bytes(), rendered);
    }

    #[test]
    fn generated_terminal_diagnostic_cases_bind_native_presentation() {
        use crate::lean_vocab_test::LeanTerminalDiagnosticPresentationExpected;

        let cases = crate::lean_vocab_test::lean_terminal_diagnostic_presentation_cases();
        assert_eq!(
            cases.len(),
            10,
            "all generated terminal diagnostic boundaries must be bound"
        );
        for case in cases {
            let raw = String::from_utf8(case.raw.clone());
            let cause = String::from_utf8(case.cause.clone());
            match &case.expected {
                LeanTerminalDiagnosticPresentationExpected::InvalidUtf8 => {
                    assert!(
                        raw.is_err() || cause.is_err(),
                        "{}: model rejected valid native UTF-8 input",
                        case.name
                    );
                }
                LeanTerminalDiagnosticPresentationExpected::UnexpectedError { error } => {
                    panic!(
                        "{}: model produced unexpected presentation error: {error}",
                        case.name
                    )
                }
                LeanTerminalDiagnosticPresentationExpected::Ok {
                    presentation,
                    rendered,
                } => {
                    let raw = raw.unwrap_or_else(|error| panic!("{}: {error}", case.name));
                    let cause = cause.unwrap_or_else(|error| panic!("{}: {error}", case.name));
                    let budget = usize::try_from(case.tail_budget)
                        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
                    let actual = terminal_diagnostic_presentation(&raw, &cause, budget)
                        .expect("valid native diagnostic input");
                    assert_eq!(
                        actual,
                        crate::lean_vocab_test::native_presentation(presentation)
                            .expect("modeled presentation translates"),
                        "{}: native terminal diagnostic presentation diverged",
                        case.name
                    );
                    assert_eq!(
                        render_presentation(&raw, &actual)
                            .expect("native presentation renders")
                            .as_bytes(),
                        rendered,
                        "{}: native rendered diagnostic diverged",
                        case.name
                    );
                }
            }
        }
    }

    fn admission(deadline_at: DateTime<Utc>) -> SpawnedBackgroundToolAdmission {
        SpawnedBackgroundToolAdmission {
            tool_name: "background_worker".into(),
            deadline_at,
            selected_tool_identity: None,
        }
    }

    async fn tool_delivery_rows(
        node: &EmbeddedNode,
        session_id: &str,
    ) -> Vec<crate::session::canonical_rows::TranscriptMessageRow> {
        let session = crate::graphql::escape_graphql_string(session_id);
        let response = node
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ {} }} }}"#,
                AGENT_MESSAGE_FIELDS,
            ))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        response.data.as_ref().unwrap()["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()
            .unwrap()
    }

    #[tokio::test]
    async fn failed_streaming_tool_preserves_raw_prefix_and_publishes_terminal_failure() {
        let case = crate::lean_vocab_test::lean_terminal_diagnostic_presentation_cases()
            .iter()
            .find(|case| case.name == "whole_prefix_then_cause")
            .expect("generated terminal diagnostic fixture");
        let raw = String::from_utf8(case.raw.clone()).unwrap();
        let cause = String::from_utf8(case.cause.clone()).unwrap();
        let crate::lean_vocab_test::LeanTerminalDiagnosticPresentationExpected::Ok {
            rendered, ..
        } = &case.expected
        else {
            panic!("generated valid terminal diagnostic result");
        };
        let (node, path, mut tool) = published_spawn_parent("streamed-failure").await;
        let tool_doc_id = tool.doc_id().unwrap().to_owned();
        let request_doc_id = tool.request_doc_id.as_deref().unwrap().to_owned();
        let binding = tool
            .tool_output_binding()
            .expect("running tool output binding");
        append_tool_output(&binding, &raw).await.unwrap();

        assert!(tool
            .fail_owned(&cause, FailureClass::External, None)
            .await
            .unwrap());

        let rows = tool_delivery_rows(&node, &tool.session_id).await;
        let delivery = rows
            .iter()
            .find(|row| {
                matches!(
                    row.message.publication,
                    MessagePublication::ToolDelivery { ref tool_call_doc_id }
                        if tool_call_doc_id == &tool_doc_id
                )
            })
            .expect("failed tool delivery header");
        let MessageBlock::ToolResult { parts, .. } = &delivery.message.blocks[0] else {
            panic!("failed tool delivery is not a ToolResult")
        };
        let gents_protocol::output::ToolResultPart::Text { text } = &parts[0] else {
            panic!("failed tool delivery does not select text")
        };
        assert_eq!(
            render_presentation(&raw, &text.presentation)
                .unwrap()
                .as_bytes(),
            rendered
        );
        let observed = crate::tool_call_lifecycle::load_tool_call_presentation(
            &crate::config_client::ConfigAccess::Local(node.clone()),
            &tool_doc_id,
            &tool.agent_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(observed.result.unwrap().as_bytes(), rendered);

        let segments = node
            .execute(&format!(
                r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                escape_graphql_string(&request_doc_id),
                AGENT_OUTPUT_SEGMENT_FIELDS,
            ))
            .await;
        assert!(!segments.has_errors(), "{:#?}", segments.errors);
        let rows = segments.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let tool_rows = rows
            .iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolCall {
                        tool_call_doc_id: tool_doc_id.clone(),
                    }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tool_rows
                .iter()
                .map(|row| row.segment.payload.as_str())
                .collect::<String>(),
            raw
        );
        assert_eq!(
            tool_rows
                .iter()
                .filter(|row| row.segment.close.is_some())
                .count(),
            1
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn cancelled_and_timed_out_streaming_tools_preserve_raw_prefixes() {
        // ToolDeliveryCases.cancelled_delivery_preserves_cancelled_lifecycle
        // and timed_out_delivery_preserves_timed_out_lifecycle compose
        // closeToolOutput with publishToolDelivery. The close retains the
        // physical source; presentation is independently reconstructed by the
        // published header.
        for (name, timeout) in [("streamed-cancel", false), ("streamed-timeout", true)] {
            let (node, path, mut tool) = published_spawn_parent(name).await;
            let tool_doc_id = tool.doc_id().unwrap().to_owned();
            let request_doc_id = tool.request_doc_id.as_deref().unwrap().to_owned();
            let binding = tool
                .tool_output_binding()
                .expect("running tool output binding");
            append_tool_output(&binding, "stdout-before-terminal\n")
                .await
                .unwrap();
            let mut replay = ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                &tool_doc_id,
                &tool.agent_did,
                &tool.session_id,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .expect("running physical tool for same-state replay");
            let mut conflicting_cancel = if timeout {
                None
            } else {
                Some(
                    ToolCallLifecycle::load_by_doc_id(
                        node.clone(),
                        &tool_doc_id,
                        &tool.agent_did,
                        &tool.session_id,
                        tool.requester_did.as_deref(),
                    )
                    .await
                    .unwrap()
                    .expect("running physical tool for conflicting cause"),
                )
            };

            if timeout {
                assert!(tool.timeout().await.unwrap());
            } else {
                assert!(tool
                    .cancel_during_run(CancelCause::Interrupted)
                    .await
                    .unwrap());
            }

            let segments = node
                .execute(&format!(
                    r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    escape_graphql_string(&request_doc_id),
                    AGENT_OUTPUT_SEGMENT_FIELDS,
                ))
                .await;
            assert!(!segments.has_errors(), "{:#?}", segments.errors);
            let rows = segments.data.as_ref().unwrap()["AgentOutputSegment"]
                .as_array()
                .unwrap()
                .iter()
                .map(decode_output_segment_row)
                .collect::<Result<Vec<_>>>()
                .unwrap();
            let tool_rows = rows
                .iter()
                .filter(|row| {
                    row.segment.source
                        == OutputSource::ToolCall {
                            tool_call_doc_id: tool_doc_id.clone(),
                        }
                })
                .collect::<Vec<_>>();
            assert_eq!(
                tool_rows
                    .iter()
                    .map(|row| row.segment.payload.as_str())
                    .collect::<String>(),
                "stdout-before-terminal\n"
            );
            assert_eq!(
                tool_rows
                    .iter()
                    .filter(|row| row.segment.close.is_some())
                    .count(),
                1
            );
            let stored = node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                    escape_graphql_string(&tool_doc_id),
                ))
                .await;
            assert_eq!(
                stored.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
                if timeout { "timedOut" } else { "cancelled" }
            );
            let raw = crate::background_tools::canonical_tool_output(
                node.as_ref(),
                &tool_doc_id,
                &request_doc_id,
                &tool.session_id,
                &tool.agent_did,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap();
            assert_eq!(raw, "stdout-before-terminal\n");
            let presentation = crate::tool_call_lifecycle::load_tool_call_presentation(
                &crate::config_client::ConfigAccess::Local(node.clone()),
                &tool_doc_id,
                &tool.agent_did,
                &tool.session_id,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap();
            let presented = presentation.result.expect("terminal delivery presentation");
            assert!(
                presented.starts_with("stdout-before-terminal\n\n"),
                "terminal diagnostic must expose committed output before the cause: {presented:?}"
            );
            if timeout {
                assert!(presented.contains("tool call deadline exceeded at "));
            } else {
                assert!(presented.ends_with("tool call cancelled"));
            }

            let mut before_messages = tool_delivery_rows(&node, &tool.session_id)
                .await
                .into_iter()
                .map(|row| row.doc_id)
                .collect::<Vec<_>>();
            before_messages.sort();
            let mut before_segments = rows
                .iter()
                .map(|row| row.doc_id.clone())
                .collect::<Vec<_>>();
            before_segments.sort();
            if timeout {
                assert!(!replay.timeout().await.unwrap());
            } else {
                assert!(!replay
                    .cancel_during_run(CancelCause::Interrupted)
                    .await
                    .unwrap());
                let mut conflicting_cancel =
                    conflicting_cancel.take().expect("cancel cause contender");
                assert!(!conflicting_cancel
                    .cancel_during_run(CancelCause::UserCancelled)
                    .await
                    .unwrap());
                assert_eq!(conflicting_cancel.state, ToolCallState::Cancelled);
                assert_eq!(
                    conflicting_cancel.cancel_cause,
                    Some(CancelCause::Interrupted)
                );
            }
            let mut after_messages = tool_delivery_rows(&node, &tool.session_id)
                .await
                .into_iter()
                .map(|row| row.doc_id)
                .collect::<Vec<_>>();
            after_messages.sort();
            assert_eq!(
                after_messages, before_messages,
                "terminal replay added a header"
            );
            let after_segments = node
                .execute(&format!(
                    r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                    escape_graphql_string(&request_doc_id),
                    AGENT_OUTPUT_SEGMENT_FIELDS,
                ))
                .await;
            assert!(!after_segments.has_errors(), "{:#?}", after_segments.errors);
            let mut after_segments = after_segments.data.as_ref().unwrap()["AgentOutputSegment"]
                .as_array()
                .unwrap()
                .iter()
                .map(decode_output_segment_row)
                .collect::<Result<Vec<_>>>()
                .unwrap()
                .into_iter()
                .map(|row| row.doc_id)
                .collect::<Vec<_>>();
            after_segments.sort();
            assert_eq!(
                after_segments, before_segments,
                "terminal replay added raw output"
            );

            node.shutdown().await;
            let _ = std::fs::remove_dir_all(path);
        }
    }

    async fn configure_bash_output_budget(
        node: &std::sync::Arc<EmbeddedNode>,
        owner: &str,
        budget: i64,
    ) {
        use crate::config_client::{
            apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        let tools = serde_json::json!({"agent_did": owner, "tools_id": "general:tools",
            "host": {"bash": {"mode": "ReadOnly", "max_output_chars": budget}}});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: crate::Collection::Tools,
            add: tools.clone(),
            update: tools,
        }])
        .unwrap();
        ConfigAccess::transact_local(node, None, "test.output_budget", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    }

    async fn interrupted_presentation(
        node: &std::sync::Arc<EmbeddedNode>,
        tool: &ToolCallLifecycle,
    ) -> String {
        crate::tool_call_lifecycle::load_tool_call_presentation(
            &crate::config_client::ConfigAccess::Local(node.clone()),
            tool.doc_id().unwrap(),
            &tool.agent_did,
            &tool.session_id,
            tool.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .result
        .expect("terminal delivery presentation")
    }

    /// An interrupted call's diagnostic tail follows the owning behavior's
    /// configured budget for the tool, resolved from configuration so a
    /// reloaded owner (restart recovery) applies it too. Tools without a host
    /// group, and behaviors that no longer resolve, keep the default.
    #[tokio::test]
    async fn interrupted_tool_diagnostic_follows_the_configured_output_budget() {
        use crate::tool_call_lifecycle::admission_fixture::{
            published_admission, PublishedAdmission, PublishedAdmissionOptions,
        };
        for (name, tool_name, configured, reload, bounded) in [
            ("budget-bash", "bash", true, false, true),
            ("budget-bash-restart", "bash", true, true, true),
            ("budget-remote", "mcp__search__query", true, false, false),
            ("budget-unresolved", "bash", false, false, false),
        ] {
            let PublishedAdmission {
                node,
                path,
                mut tool,
                ..
            } = published_admission(PublishedAdmissionOptions {
                name: name.to_owned(),
                tool_name: Some(tool_name.to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
            let binding = tool.tool_output_binding().expect("running output binding");
            append_tool_output(&binding, "0123456789abcdefghij")
                .await
                .unwrap();
            // Configuration is read when the diagnostic is presented.
            if configured {
                crate::test_support::install_test_behavior(&node, &tool.agent_did, "general").await;
                configure_bash_output_budget(&node, &tool.agent_did, 5).await;
            }
            if reload {
                tool = ToolCallLifecycle::load_by_doc_id(
                    node.clone(),
                    tool.doc_id().unwrap(),
                    &tool.agent_did,
                    &tool.session_id,
                    tool.requester_did.as_deref(),
                )
                .await
                .unwrap()
                .expect("reloaded running tool");
            }
            let mut replay = ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                tool.doc_id().unwrap(),
                &tool.agent_did,
                &tool.session_id,
                tool.requester_did.as_deref(),
            )
            .await
            .unwrap()
            .expect("running tool for a later same-state replay");
            assert!(tool.timeout().await.unwrap(), "{name}");
            let presented = interrupted_presentation(&node, &tool).await;
            if bounded {
                assert!(
                    presented.starts_with("fghij\n") && !presented.contains("0123456789abcde"),
                    "{name}: {presented:?}"
                );
            } else {
                assert!(
                    presented.starts_with("0123456789abcdefghij\n"),
                    "{name}: {presented:?}"
                );
            }
            assert!(
                presented.contains("tool call deadline exceeded at "),
                "{presented:?}"
            );

            if bounded && !reload {
                // A configuration change after delivery does not make the
                // delivered diagnostic a conflicting replay.
                configure_bash_output_budget(&node, &tool.agent_did, 12).await;
                assert!(!replay.timeout().await.unwrap(), "{name}: replay");
                assert_eq!(interrupted_presentation(&node, &tool).await, presented);
            }
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(path);
        }
    }

    #[test]
    fn generated_terminal_diagnostic_replay_cases_bind_native_replay() {
        let cases = crate::lean_vocab_test::lean_terminal_diagnostic_replay_cases();
        assert_eq!(cases.len(), 12, "all generated replay shapes must be bound");
        assert!(cases.iter().any(|case| case.accepted) && cases.iter().any(|case| !case.accepted));
        for case in cases {
            let raw = String::from_utf8(case.raw.clone()).expect("valid replay source");
            let cause = String::from_utf8(case.cause.clone()).expect("valid replay cause");
            let stored = crate::lean_vocab_test::native_presentation(&case.stored)
                .expect("modeled presentation translates");
            assert_eq!(
                diagnostic_replay_matches(&raw, &cause, &stored).expect("replay check"),
                case.accepted,
                "{}: native diagnostic replay diverged from the model",
                case.name
            );
            if case.accepted {
                // The configured budget at replay may differ from delivery; the
                // replayed plan's own presentation is irrelevant to acceptance.
                let budget = usize::try_from(case.configured_budget).unwrap();
                let current = terminal_diagnostic_presentation(&raw, &cause, budget).unwrap();
                assert!(
                    diagnostic_replay_matches(&raw, &cause, &current).unwrap(),
                    "{}: freshly planned diagnostic must also replay",
                    case.name
                );
            }
        }
    }

    #[tokio::test]
    async fn completed_streaming_tool_still_rejects_a_non_extension() {
        let (node, path, mut tool) = published_spawn_parent("streamed-complete-conflict").await;
        let binding = tool
            .tool_output_binding()
            .expect("running tool output binding");
        append_tool_output(&binding, "persisted-prefix")
            .await
            .unwrap();

        let error = tool
            .complete_owned("different-success", None)
            .await
            .expect_err("successful completion cannot replace persisted raw bytes");
        assert!(
            error
                .to_string()
                .contains("not an exact extension of persisted raw output"),
            "{error:#}"
        );
        assert_eq!(tool.state, ToolCallState::Running);

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn spawned_background_replays_parent_provenance_and_never_authors_a_child_tool_result() {
        let (node, path, mut parent) = published_spawn_parent("replay").await;
        let parent_doc_id = parent.doc_id().unwrap().to_owned();
        let deadline = parent.deadline_at;
        let receipt = r#"{"ok":true,"status":"started"}"#;
        let mut child = parent
            .admit_spawned_background(admission(deadline), receipt)
            .await
            .unwrap();
        let child_doc_id = child.doc_id().unwrap().to_owned();
        assert_ne!(child_doc_id, parent_doc_id);
        assert_eq!(
            child.spawned_by_tool_call_doc_id.as_deref(),
            Some(parent_doc_id.as_str())
        );

        // A replay after the child has actually run must rehydrate its durable
        // state, rather than returning a synthetic pending lifecycle.
        child.start_running().await.unwrap();
        let binding = child.tool_output_binding().unwrap();
        append_tool_output(&binding, "child raw output")
            .await
            .unwrap();
        assert!(child
            .complete_owned("child raw output", None)
            .await
            .unwrap());
        assert!(
            parent
                .admit_spawned_background(
                    admission(deadline + chrono::Duration::seconds(1)),
                    receipt,
                )
                .await
                .is_err(),
            "a replay cannot change the child deadline"
        );
        let mut replay_parent = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &parent_doc_id,
            "did:test:test",
            &parent.session_id,
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let replay = replay_parent
            .admit_spawned_background(admission(deadline), receipt)
            .await
            .unwrap();
        assert_eq!(replay.doc_id(), Some(child_doc_id.as_str()));
        assert_eq!(replay.state, ToolCallState::Completed);

        let messages = tool_delivery_rows(&node, &parent.session_id).await;
        let deliveries = messages.iter().filter(|row| matches!(
            row.message.publication,
            MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &parent_doc_id
        )).count();
        assert_eq!(
            deliveries, 1,
            "the accepted meta-call gets one immediate receipt"
        );
        assert!(messages.iter().all(|row| !matches!(
            row.message.publication,
            MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &child_doc_id
        )), "spawned work must not invent a direct ToolResult header");

        let segments = node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(child.request_doc_id.as_deref().unwrap()),
            AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        assert!(!segments.has_errors(), "{:#?}", segments.errors);
        let child_segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolCall {
                        tool_call_doc_id: child_doc_id.clone(),
                    }
            })
            .collect::<Vec<_>>();
        assert!(
            child_segments.iter().any(|row| matches!(
                row.segment.close,
                Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    ..
                })
            )),
            "child raw source closes durably under its own physical identity"
        );
        assert_eq!(
            crate::background_tools::canonical_tool_output(
                node.as_ref(),
                &child_doc_id,
                child.request_doc_id.as_deref().unwrap(),
                &child.session_id,
                &child.agent_did,
                child.requester_did.as_deref(),
            )
            .await
            .unwrap(),
            "child raw output",
            "spawned terminal output is read from its physical ToolOutput source, not a fabricated provider reply"
        );

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn spawned_background_admission_rejects_a_stale_parent_generation() {
        let (node, path, mut parent) = published_spawn_parent("stale").await;
        let request_doc_id = parent.request_doc_id.clone().unwrap();
        let deadline = parent.deadline_at;
        let changed = node.execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "stale-generation" }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&request_doc_id),
        )).await;
        assert!(!changed.has_errors(), "{:#?}", changed.errors);
        assert!(parent
            .admit_spawned_background(admission(deadline), "receipt")
            .await
            .is_err());
        let parent_doc_id = crate::graphql::escape_graphql_string(parent.doc_id().unwrap());
        let children = node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ spawned_by_tool_call_doc_id: {{ _eq: "{parent_doc_id}" }} }}) {{ _docID }} }}"#,
        )).await;
        assert!(!children.has_errors(), "{:#?}", children.errors);
        assert!(children.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .is_empty());
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn background_bridge_receipt_is_authored_once_and_keeps_bridge_running() {
        let (node, path, mut bridge) = published_background_bridge("receipt").await;
        let tool_doc_id = bridge.doc_id().unwrap().to_owned();
        assert!(bridge.publish_background_receipt("started").await.unwrap());
        assert!(!bridge.publish_background_receipt("started").await.unwrap());
        assert_eq!(bridge.state, ToolCallState::Running);
        assert!(bridge
            .publish_background_receipt("different")
            .await
            .is_err());
        let rows = tool_delivery_rows(&node, &bridge.session_id).await;
        let receipts = rows.iter().filter(|row| matches!(&row.message.publication,
            MessagePublication::ToolDelivery { tool_call_doc_id } if tool_call_doc_id == &tool_doc_id)).count();
        assert_eq!(receipts, 1);
        let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(&tool_doc_id))).await;
        assert_eq!(
            response.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running"
        );
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn background_bridge_completion_closes_its_source_and_notifies_without_a_second_native_result(
    ) {
        let (node, path, mut bridge) = published_background_bridge("receipt-completion").await;
        let tool_doc_id = bridge.doc_id().unwrap().to_owned();
        let final_bytes = "child terminal bytes";

        assert!(bridge.publish_background_receipt("started").await.unwrap());
        bridge = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &tool_doc_id,
            &bridge.agent_did,
            &bridge.session_id,
            bridge.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .expect("rehydrated background bridge");
        assert!(bridge.bridge_complete(final_bytes.into()).await.unwrap());
        assert_eq!(bridge.state, ToolCallState::Completed);

        // The terminal bridge source is the durable final-output authority.
        // The parent notification must reference it; it cannot manufacture a
        // second native ToolResult for the already-replied provider call.
        crate::background_completion::append_background_tool_completion(
            &node,
            &bridge.session_id,
            "request-receipt-completion",
            &tool_doc_id,
            &bridge.tool_name,
            "completed",
            final_bytes,
            None,
        )
        .await
        .unwrap();

        let response = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#,
                crate::graphql::escape_graphql_string(&tool_doc_id)
            ))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        assert_eq!(
            response.data.as_ref().unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "completed"
        );

        let segments = node
            .execute(&format!(
                r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                crate::graphql::escape_graphql_string(bridge.request_doc_id.as_deref().unwrap()),
                AGENT_OUTPUT_SEGMENT_FIELDS,
            ))
            .await;
        assert!(!segments.has_errors(), "{:#?}", segments.errors);
        let tool_segments = segments.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolCall {
                        tool_call_doc_id: tool_doc_id.clone(),
                    }
            })
            .collect::<Vec<_>>();
        let close = tool_segments
            .iter()
            .find(|row| {
                matches!(
                    row.segment.close,
                    Some(SourceClose::Closed {
                        outcome: OutputOutcome::Complete,
                        ..
                    })
                )
            })
            .expect("terminal bridge source closure");

        let rows = tool_delivery_rows(&node, &bridge.session_id).await;
        let native_results = rows
            .iter()
            .flat_map(|row| row.message.blocks.iter())
            .filter(|block| matches!(
                block,
                MessageBlock::ToolResult { tool_call_doc_id, .. } if tool_call_doc_id == &tool_doc_id
            ))
            .count();
        assert_eq!(
            native_results, 1,
            "the running receipt is the bridge's sole native invocation reply"
        );
        let notification = rows
            .iter()
            .find(|row| {
                row.message.message_key
                    == format!("background-completion-notification:{tool_doc_id}:tool")
            })
            .expect("canonical terminal background notification");
        assert!(matches!(
            notification.message.publication,
            MessagePublication::ToolDelivery { ref tool_call_doc_id } if tool_call_doc_id == &tool_doc_id
        ));
        let [MessageBlock::Text { text }] = notification.message.blocks.as_slice() else {
            panic!("background notification must be a composed text reference");
        };
        assert_eq!(text.output.close_doc_id, close.doc_id);
        let observed = tool_segments
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let reconstructed = reconstruct_stream(&observed, &[], &[], &text.output).unwrap();
        assert_eq!(reconstructed.text, final_bytes);
        assert!(render_presentation(&reconstructed.text, &text.presentation)
            .unwrap()
            .contains(final_bytes));

        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn concurrent_background_bridge_completion_replays_one_exact_closure() {
        let (node, path, mut bridge) = published_background_bridge("concurrent-replay").await;
        let tool_doc_id = bridge.doc_id().unwrap().to_owned();
        assert!(bridge.publish_background_receipt("started").await.unwrap());
        let load = || {
            ToolCallLifecycle::load_by_doc_id(
                node.clone(),
                &tool_doc_id,
                &bridge.agent_did,
                &bridge.session_id,
                bridge.requester_did.as_deref(),
            )
        };
        let mut first = load().await.unwrap().expect("first admitted bridge");
        let mut second = load().await.unwrap().expect("second admitted bridge");
        let (left, right) = tokio::join!(
            first.bridge_complete("one result".into()),
            second.bridge_complete("one result".into())
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_ne!(left, right, "exactly one projector must commit");

        let mut replay = load().await.unwrap().expect("terminal admitted bridge");
        assert!(!replay.bridge_complete("one result".into()).await.unwrap());
        assert!(replay
            .bridge_complete("different result".into())
            .await
            .is_err());

        let request_doc_id = bridge.request_doc_id.as_deref().unwrap();
        let response = node
            .execute(&format!(
                r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                crate::graphql::escape_graphql_string(request_doc_id),
                AGENT_OUTPUT_SEGMENT_FIELDS,
            ))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let closures = response.data.as_ref().unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolCall {
                        tool_call_doc_id: tool_doc_id.clone(),
                    }
                    && row.segment.close.is_some()
            })
            .count();
        assert_eq!(closures, 1);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }
}
