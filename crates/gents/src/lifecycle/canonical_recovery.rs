//! Atomic recovery of output owned by an expired request generation.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::recovery::plan_recovery_prefix;
use gents_protocol::output::{
    MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
    SourceClose, TerminalOutput, TranscriptMessage,
};
use gents_protocol::rendered_request::CaptureOrderKey;
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session;
use crate::session::canonical_rows::{
    decode_output_segment_row, output_segment_create_variables,
    transcript_message_create_variables, OutputSegmentRow, TranscriptMessageRow,
    AGENT_OUTPUT_SEGMENT_FIELDS, CREATE_AGENT_MESSAGE_MUTATION,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryResult {
    Won { published: usize },
    Lost,
}

#[derive(Debug, Clone)]
pub(crate) enum RecoverySelectionChoice {
    NoMessage,
    MessageKey(String),
}

#[derive(Debug, thiserror::Error)]
#[error("recovery terminal selection is not an exact reconstructable owned assistant header")]
pub(crate) struct RecoverySelectionRejected;

use crate::graphql::created_doc_id;

pub(in crate::lifecycle) async fn request_segments(
    txn: &ConfigApplyTxn<'_>,
    request_doc_id: &str,
    agent: &str,
    session_id: &str,
    requester: Option<&str>,
) -> Result<Vec<OutputSegmentRow>> {
    let request = escape_graphql_string(request_doc_id);
    let scope = crate::session::session_scope_filter(agent, session_id, requester);
    let value = txn
        .execute_local_response(&format!(
            "{{ AgentOutputSegment(filter: {{ {scope}, request_doc_id: {{ _eq: \"{request}\" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"
        ))
        .await?;
    let rows = value
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("AgentOutputSegment query omitted rows")?;
    rows.iter().map(decode_output_segment_row).collect()
}

async fn validate_selection(
    txn: &ConfigApplyTxn<'_>,
    headers: &[TranscriptMessageRow],
    row: &AgentRequestRow,
    selection: &TerminalOutput,
) -> Result<()> {
    if row.purpose.context("recovery request purpose missing")? == RequestPurpose::TitleAudit {
        anyhow::ensure!(
            matches!(selection, TerminalOutput::NoMessage) && headers.is_empty(),
            "title recovery requires NoMessage and no transcript headers"
        );
        return Ok(());
    }
    let eligible = |header: &&TranscriptMessageRow| {
        header.message.role == MessageRole::Assistant
            && matches!(
                header.message.publication,
                MessagePublication::RequestExecution { .. }
                    | MessagePublication::RequestRecovery { .. }
            )
    };
    let agent = row.agent_did.as_deref().context("missing request agent")?;
    match selection {
        TerminalOutput::NoMessage => {
            for header in headers.iter().filter(eligible) {
                match session::load_canonical_message_in_txn(
                    txn,
                    &header.doc_id,
                    agent,
                    row.requester_did.as_deref(),
                )
                .await
                {
                    Ok(_) => return Err(RecoverySelectionRejected.into()),
                    Err(error)
                        if error
                            .downcast_ref::<gents_protocol::output::ReconstructionError>()
                            .is_some() => {}
                    Err(error) => return Err(error),
                }
            }
        }
        TerminalOutput::Message { message_doc_id } => {
            let matching = headers
                .iter()
                .filter(eligible)
                .filter(|header| &header.doc_id == message_doc_id)
                .collect::<Vec<_>>();
            let [header] = matching.as_slice() else {
                return Err(RecoverySelectionRejected.into());
            };
            session::load_canonical_message_in_txn(
                txn,
                &header.doc_id,
                agent,
                row.requester_did.as_deref(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn ensure_title_has_no_tools(txn: &ConfigApplyTxn<'_>, request_doc_id: &str) -> Result<()> {
    let request = escape_graphql_string(request_doc_id);
    let response = txn
        .execute_local_response(&format!(
            "{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: \"{request}\" }} }}, limit: 1) {{ _docID }} }}"
        ))
        .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .context("title recovery tool query omitted rows")?;
    anyhow::ensure!(rows.is_empty(), "title recovery found a tool lifecycle row");
    Ok(())
}

fn selection_from_choice(
    headers: &[TranscriptMessageRow],
    choice: &RecoverySelectionChoice,
) -> Result<TerminalOutput> {
    match choice {
        RecoverySelectionChoice::NoMessage => Ok(TerminalOutput::NoMessage),
        RecoverySelectionChoice::MessageKey(key) => {
            let matching = headers
                .iter()
                .filter(|header| {
                    header.message.message_key == *key
                        && header.message.role == MessageRole::Assistant
                        && matches!(
                            header.message.publication,
                            MessagePublication::RequestExecution { .. }
                                | MessagePublication::RequestRecovery { .. }
                        )
                })
                .collect::<Vec<_>>();
            let [header] = matching.as_slice() else {
                return Err(RecoverySelectionRejected.into());
            };
            Ok(TerminalOutput::Message {
                message_doc_id: header.doc_id.clone(),
            })
        }
    }
}

pub(super) async fn recover_expired_generation(
    node: &defra_node::EmbeddedNode,
    observed: &AgentRequestRow,
    expected_generation: &str,
    expected_expiry: &str,
) -> Result<RecoveryResult> {
    recover_expired_generation_with_facts(
        node,
        observed,
        expected_generation,
        expected_expiry,
        uuid::Uuid::new_v4().to_string(),
        Utc::now(),
        None,
        None,
    )
    .await
}

pub(crate) async fn recover_expired_generation_with_facts(
    node: &defra_node::EmbeddedNode,
    observed: &AgentRequestRow,
    expected_generation: &str,
    expected_expiry: &str,
    fresh_generation: String,
    observed_now: DateTime<Utc>,
    selection_choice: Option<RecoverySelectionChoice>,
    expected_outcome: Option<RequestLifecycleState>,
) -> Result<RecoveryResult> {
    let request_doc_id = observed
        .doc_id
        .as_deref()
        .context("missing request document")?
        .to_owned();
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "lifecycle.recover_expired_generation",
        move |txn| {
            let request_doc_id = request_doc_id.clone();
            let fresh_generation = fresh_generation.clone();
            let selection_choice = selection_choice.clone();
            Box::pin(async move {
            let request_id = escape_graphql_string(&request_doc_id);
            let value = txn.execute_local_response(&format!(r#"{{ AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_id}" }} }}, limit: 1) {{
                _docID request_id purpose agent_did requester_did session_id lifecycle_state interrupt_requested_at
                execution_generation execution_lease_expires_at terminal_output
            }} }}"#)).await?;
            let row = crate::graphql::first_row::<AgentRequestRow>(&value, "AgentRequest")?
                .context("execution request disappeared")?;
            let is_title = row.purpose.context("recovery request purpose missing")?
                == RequestPurpose::TitleAudit;
            let state = row.lifecycle_state.context("missing request lifecycle")?;
            if state.is_terminal()
                && row.execution_generation.as_deref() == Some(fresh_generation.as_str())
                && row.terminal_output.is_some()
            {
                if expected_outcome.is_some_and(|expected| state != expected) {
                    return Err(RecoverySelectionRejected.into());
                }
                let headers = session::load_request_headers_in_txn(
                    txn,
                    row.session_id.as_deref().context("missing request session")?,
                    row.agent_did.as_deref().context("missing request agent")?,
                    row.requester_did.as_deref(),
                    &request_doc_id,
                )
                .await?;
                if is_title {
                    ensure_title_has_no_tools(txn, &request_doc_id).await?;
                    let writer = OutputWriter::RequestExecution {
                        execution_generation: expected_generation.to_owned(),
                    };
                    let segments = request_segments(
                        txn,
                        &request_doc_id,
                        row.agent_did.as_deref().context("missing request agent")?,
                        row.session_id.as_deref().context("missing request session")?,
                        row.requester_did.as_deref(),
                    )
                    .await?;
                    for record in segments.iter().filter(|record| record.segment.writer == writer) {
                        crate::streaming::canonical::validate_source_purpose(
                            RequestPurpose::TitleAudit,
                            &record.segment.source,
                        )?;
                    }
                    super::execution_lease::validate_title_sources_decided(
                        &request_doc_id,
                        &segments,
                    )?;
                }
                let published = headers
                    .iter()
                    .filter(|header| matches!(
                        &header.message.publication,
                        MessagePublication::RequestRecovery { execution_generation }
                            if execution_generation == &fresh_generation
                    ))
                    .count();
                validate_selection(txn, &headers, &row, row.terminal_output.as_ref().expect("checked"))
                    .await?;
                if let Some(choice) = &selection_choice {
                    let expected = selection_from_choice(&headers, choice)?;
                    if row.terminal_output.as_ref() != Some(&expected) {
                        return Err(RecoverySelectionRejected.into());
                    }
                }
                return Ok(RecoveryResult::Won { published });
            }
            if !matches!(state, RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
                || row.execution_generation.as_deref() != Some(expected_generation)
                || row.execution_lease_expires_at.as_deref() != Some(expected_expiry)
                || row.terminal_output.is_some()
            {
                return Ok(RecoveryResult::Lost);
            }
            let deadline = DateTime::parse_from_rfc3339(expected_expiry)?;
            if deadline > observed_now {
                return Ok(RecoveryResult::Lost);
            }
            let agent = row.agent_did.as_deref().context("missing request agent")?;
            let session_id = row.session_id.as_deref().context("missing request session")?;
            let writer = OutputWriter::RequestExecution { execution_generation: expected_generation.to_owned() };
            let mut segments = request_segments(
                txn, &request_doc_id, agent, session_id, row.requester_did.as_deref(),
            ).await?;
            let existing_headers = session::load_request_headers_in_txn(
                txn, session_id, agent, row.requester_did.as_deref(), &request_doc_id,
            ).await?;
            if is_title {
                anyhow::ensure!(existing_headers.is_empty(), "title recovery found a transcript header");
                ensure_title_has_no_tools(txn, &request_doc_id).await?;
            }
            let referenced = existing_headers.iter().flat_map(|header| header.message.payload_references())
                .map(|reference| reference.close_doc_id.as_str()).collect::<BTreeSet<_>>();
            let mut sources = BTreeMap::<CaptureOrderKey, OutputSource>::new();
            for record in &segments {
                if record.segment.writer == writer {
                    if let OutputSource::ProviderTurn { scope, turn_index, attempt } = &record.segment.source {
                        sources.insert(CaptureOrderKey {
                            scope: *scope,
                            turn_index: i64::from(*turn_index),
                            attempt: i64::from(*attempt),
                        }, record.segment.source.clone());
                    }
                }
            }
            let mut sequence = if is_title {
                None
            } else {
                Some(super::queue::next_append_sequence_in_transaction(
                    txn, agent, session_id,
                ).await?)
            };
            let timestamp = observed_now.to_rfc3339();
            let mut published = Vec::<TranscriptMessageRow>::new();
            for source in sources.into_values() {
                if is_title {
                    crate::streaming::canonical::validate_source_purpose(
                        RequestPurpose::TitleAudit, &source,
                    )?;
                } else if crate::streaming::canonical::validate_source_purpose(
                    RequestPurpose::Normal, &source,
                ).is_err() {
                    continue;
                }
                let closures = segments.iter().filter(|record| record.segment.source == source && record.segment.close.is_some()).collect::<Vec<_>>();
                let observations = segments.iter().map(|record| ObservedSegment { doc_id: &record.doc_id, segment: &record.segment }).collect::<Vec<_>>();
                let blocks = match closures.as_slice() {
                    [] => {
                        let plan = plan_recovery_prefix(&observations, &request_doc_id, &source, &writer)?;
                        let exemplar = segments.iter().find(|record| record.segment.source == source).context("source disappeared")?;
                        let closing = OutputSegment {
                            agent_did: exemplar.segment.agent_did.clone(), requester_did: exemplar.segment.requester_did.clone(),
                            session_id: exemplar.segment.session_id.clone(), request_doc_id: request_doc_id.clone(),
                            source: source.clone(), writer: writer.clone(), ordinal: None, runs: Vec::new(), payload: String::new(),
                            close: Some(plan.close.clone()), created_at: timestamp.clone(),
                        };
                        let response = txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &output_segment_create_variables(&closing)?).await?;
                        let id = created_doc_id(&response, "AgentOutputSegment")?;
                        let blocks = plan.retained_blocks(&id)?;
                        segments.push(OutputSegmentRow { doc_id: id.clone(), segment: closing });
                        blocks
                    }
                    [closing] if is_title => {
                        anyhow::ensure!(closing.segment.writer == writer,
                            "title recovery closure belongs to another generation");
                        Vec::new()
                    }
                    [closing] if closing.segment.writer == writer
                        && matches!(closing.segment.close, Some(SourceClose::Closed { outcome: OutputOutcome::Partial, .. }))
                        && !referenced.contains(closing.doc_id.as_str()) => {
                        crate::streaming::canonical::retained_partial_blocks(
                            &observations, &request_doc_id, &source, &writer, closing,
                        )?
                    }
                    [_] => continue,
                    _ => continue,
                };
                if is_title {
                    anyhow::ensure!(blocks.is_empty(), "title recovery planned a public header");
                    continue;
                }
                if blocks.is_empty() { continue; }
                let next_sequence = sequence.as_mut().context("normal recovery has no sequence")?;
                let message = TranscriptMessage {
                    message_key: crate::streaming::canonical::partial_message_key(&request_doc_id, &source)?, session_id: session_id.to_owned(), agent_did: agent.to_owned(),
                    requester_did: row.requester_did.clone(), request_doc_id: Some(request_doc_id.clone()),
                    publication: MessagePublication::RequestRecovery { execution_generation: fresh_generation.clone() },
                    outcome: OutputOutcome::Partial, sequence: *next_sequence, role: MessageRole::Assistant, native_id: None,
                    blocks, created_at: timestamp.clone(),
                };
                *next_sequence = next_sequence.checked_add(1).context("recovery transcript sequence overflow")?;
                let response = txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &transcript_message_create_variables(&message)?).await?;
                let doc_id = created_doc_id(&response, "AgentMessage")?;
                published.push(TranscriptMessageRow { doc_id, message });
            }
            if is_title {
                for record in segments.iter().filter(|record| record.segment.writer == writer) {
                    crate::streaming::canonical::validate_source_purpose(
                        RequestPurpose::TitleAudit,
                        &record.segment.source,
                    )?;
                }
                super::execution_lease::validate_title_sources_decided(
                    &request_doc_id,
                    &segments,
                )?;
            }
            let mut headers = existing_headers;
            headers.extend(published.iter().cloned());
            for header in &published {
                session::load_canonical_message_in_txn(
                    txn, &header.doc_id, agent, row.requester_did.as_deref(),
                ).await?;
            }
            if !is_title {
                super::terminal_tools::account_tools_in_txn(
                    txn,
                    &row,
                    &headers,
                    expected_generation,
                    false,
                    &timestamp,
                )
                .await?;
            }
            let mut eligible = headers.iter().filter(|header| {
                header.message.role == MessageRole::Assistant
                    && matches!(header.message.publication,
                        MessagePublication::RequestExecution { .. }
                            | MessagePublication::RequestRecovery { .. })
            }).collect::<Vec<_>>();
            eligible.sort_by_key(|header| header.message.sequence);
            let mut selected = published.last().map(|header| header.doc_id.clone());
            if selected.is_none() {
                for header in eligible.into_iter().rev() {
                    if headers.iter().filter(|other|
                        other.message.role == MessageRole::Assistant &&
                        matches!(other.message.publication,
                            MessagePublication::RequestExecution { .. }
                                | MessagePublication::RequestRecovery { .. }) &&
                        other.message.sequence == header.message.sequence
                    ).count() != 1 {
                        return Err(RecoverySelectionRejected.into());
                    }
                    match session::load_canonical_message_in_txn(
                        txn, &header.doc_id, agent, row.requester_did.as_deref(),
                    ).await {
                        Ok(_) => { selected = Some(header.doc_id.clone()); break; }
                        Err(error) if error.downcast_ref::<gents_protocol::output::ReconstructionError>().is_some() => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            let selection = if let Some(message_doc_id) = selected {
                TerminalOutput::Message { message_doc_id }
            } else {
                TerminalOutput::NoMessage
            };
            let selection = if is_title {
                anyhow::ensure!(selection_choice.as_ref().is_none_or(|choice|
                    matches!(choice, RecoverySelectionChoice::NoMessage)),
                    "title recovery cannot select a transcript header");
                anyhow::ensure!(matches!(selection, TerminalOutput::NoMessage),
                    "title recovery selected a transcript header");
                TerminalOutput::NoMessage
            } else {
                match &selection_choice {
                    None => selection,
                    Some(choice) => selection_from_choice(&headers, choice)?,
                }
            };
            validate_selection(txn, &headers, &row, &selection).await?;
            let target = if row.interrupt_requested_at.as_deref().is_some_and(|v| !v.trim().is_empty()) {
                RequestLifecycleState::Interrupted
            } else { RequestLifecycleState::Failed };
            if expected_outcome.is_some_and(|expected| target != expected) {
                return Err(RecoverySelectionRejected.into());
            }
            let generation = escape_graphql_string(expected_generation);
            let expiry = escape_graphql_string(expected_expiry);
            let fresh = escape_graphql_string(&fresh_generation);
            let target = escape_graphql_string(target.as_str());
            let old_state = escape_graphql_string(state.as_str());
            let now = escape_graphql_string(&timestamp);
            let response = txn.execute_with_variables(&format!(r#"mutation($terminal_output: JSON) {{ update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_id}" }}, lifecycle_state: {{ _eq: "{old_state}" }},
                    execution_generation: {{ _eq: "{generation}" }}, execution_lease_expires_at: {{ _eq: "{expiry}" }} }},
                input: {{ lifecycle_state: "{target}", execution_generation: "{fresh}", execution_lease_expires_at: "{now}",
                    failure_reason: "execution lease expired", terminalized_at: "{now}", terminal_redrive_attempts: 0,
                    terminal_output: $terminal_output }}) {{ _docID }} }}"#), &serde_json::json!({"terminal_output": selection})).await?;
            if !response.get("data").and_then(|data| data.get("update_AgentRequest")).is_some_and(response_has_documents) {
                anyhow::bail!("expired-generation recovery lost request CAS");
            }
            if !is_title {
                session::refresh_session_request_observation_in_txn(txn, agent, row.requester_did.as_deref(), session_id, &request_doc_id, &row.request_id, &timestamp).await?;
            }
            Ok(RecoveryResult::Won { published: published.len() })
        })},
    ).await
}
