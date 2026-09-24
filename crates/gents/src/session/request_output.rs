//! Typed canonical output observation for one exact physical request.
//!
//! This composes the modeled live-target selector and live classifier with the
//! exact terminal selection reader. It stores no response-shaped state and
//! never chooses among retained partial attempts for the product answer.

use anyhow::{Context, Result};
use gents_protocol::output::live::{
    observed_request_execution_owner, project_live, select_live_target, LiveObservation,
    LiveTarget, LiveTargetSelection, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    LiveStream, MessagePublication, MessageRole, OutputSource, OutputWriter, ReconstructionError,
    TerminalOutput,
};
use gents_protocol::row::AgentRequestRow;
use gents_protocol::transcript::present_message;

use crate::config_client::ConfigAccess;

use super::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, OutputSegmentRow,
    TranscriptMessageRow, AGENT_MESSAGE_FIELDS, AGENT_OUTPUT_SEGMENT_FIELDS,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalPresentation {
    pub body_markdown: String,
    pub reasoning_markdown: Option<String>,
    /// Exact live-target coordinate selected by the canonical owner. Native
    /// terminal presentations have no active source and leave this absent.
    pub selected_source: Option<CanonicalSelectedSource>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalSelectedSource {
    pub request_doc_id: String,
    pub source: OutputSource,
    pub writer: OutputWriter,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalRequestOutput {
    Absent,
    Live(CanonicalPresentation),
    Settling(CanonicalPresentation),
    Loading,
    Denied,
    Conflicted,
    Invalid,
    Retracted,
    /// Diagnostic history only. Callers must not present this as the current
    /// or terminal answer.
    RetainedPartial(Vec<LiveStream>),
    Published {
        header: gents_protocol::output::TranscriptMessage,
        message: gents_protocol::message::Message,
        presentation: CanonicalPresentation,
    },
    TerminalNoMessage,
    TerminalMessage {
        header: gents_protocol::output::TranscriptMessage,
        message: gents_protocol::message::Message,
        presentation: CanonicalPresentation,
    },
}

fn stream_presentation(
    streams: &[LiveStream],
    selected_source: &CanonicalSelectedSource,
) -> CanonicalPresentation {
    use gents_protocol::output::StreamPayload;
    let mut body = String::new();
    let mut reasoning = String::new();
    for stream in streams {
        match stream.declaration.payload {
            StreamPayload::Text => body.push_str(&stream.text),
            StreamPayload::Reasoning | StreamPayload::ReasoningSummary => {
                reasoning.push_str(&stream.text)
            }
            _ => {}
        }
    }
    CanonicalPresentation {
        body_markdown: body,
        reasoning_markdown: (!reasoning.is_empty()).then_some(reasoning),
        selected_source: Some(selected_source.clone()),
    }
}

fn native_presentation(message: &gents_protocol::message::Message) -> CanonicalPresentation {
    let presentation = present_message(message);
    CanonicalPresentation {
        body_markdown: presentation.body_markdown,
        reasoning_markdown: presentation.reasoning_markdown,
        selected_source: None,
    }
}

fn classify_terminal_read_error(error: &anyhow::Error) -> Option<CanonicalRequestOutput> {
    if matches!(
        error.downcast_ref::<super::CanonicalOutputReadError>(),
        Some(super::CanonicalOutputReadError::MissingCanonicalHeader { .. })
    ) {
        return Some(CanonicalRequestOutput::Loading);
    }
    let error = error.downcast_ref::<ReconstructionError>()?;
    if error.is_incomplete() {
        Some(CanonicalRequestOutput::Loading)
    } else {
        match error {
            ReconstructionError::AccessDenied { .. } => Some(CanonicalRequestOutput::Denied),
            ReconstructionError::ConflictingSegments { .. }
            | ReconstructionError::ConflictingClosures { .. }
            | ReconstructionError::ConflictingMessages { .. } => {
                Some(CanonicalRequestOutput::Conflicted)
            }
            _ => Some(CanonicalRequestOutput::Invalid),
        }
    }
}

async fn load_facts(
    access: &ConfigAccess,
    request: &AgentRequestRow,
) -> Result<(Vec<OutputSegmentRow>, Vec<TranscriptMessageRow>)> {
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("request output observation omitted physical request identity")?;
    let agent_did = request
        .agent_did
        .as_deref()
        .context("request output observation omitted principal")?;
    let session_id = request
        .session_id
        .as_deref()
        .context("request output observation omitted session")?;
    let scope =
        super::session_scope_filter(agent_did, session_id, request.requester_did.as_deref());
    let physical = crate::graphql::escape_graphql_string(request_doc_id);
    let response = access
        .execute(&format!(
            r#"{{
                AgentOutputSegment(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{{AGENT_OUTPUT_SEGMENT_FIELDS}}}
                AgentMessage(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{{AGENT_MESSAGE_FIELDS}}}
            }}"#
        ))
        .await?;
    anyhow::ensure!(
        response
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty),
        "canonical request output query failed: {}",
        response.get("errors").unwrap_or(&serde_json::Value::Null)
    );
    let segments = response
        .pointer("/data/AgentOutputSegment")
        .and_then(serde_json::Value::as_array)
        .context("canonical request output query omitted segments")?
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    let messages = response
        .pointer("/data/AgentMessage")
        .and_then(serde_json::Value::as_array)
        .context("canonical request output query omitted messages")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;
    Ok((segments, messages))
}

/// Observe canonical output for one already-selected physical request row.
/// Request admission input remains on `AgentRequestRow`; it is never folded
/// into this output projection.
pub async fn observe_request_output(
    access: &ConfigAccess,
    request: &AgentRequestRow,
) -> Result<CanonicalRequestOutput> {
    anyhow::ensure!(
        request.purpose == Some(gents_protocol::request_admission::RequestPurpose::Normal),
        "public request output requires normal purpose"
    );
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("request output observation omitted physical request identity")?;
    let state = request
        .lifecycle_state
        .context("request output observation omitted lifecycle state")?;
    if state.is_terminal() {
        return match request.terminal_output.as_ref() {
            None => Ok(CanonicalRequestOutput::Loading),
            Some(TerminalOutput::NoMessage) => Ok(CanonicalRequestOutput::TerminalNoMessage),
            Some(TerminalOutput::Message { message_doc_id }) => {
                let owner = request
                    .agent_did
                    .as_deref()
                    .context("terminal request output omitted principal")?;
                match super::load_canonical_message(
                    access,
                    message_doc_id,
                    owner,
                    request.requester_did.as_deref(),
                )
                .await
                {
                    Ok((header, message)) => {
                        if header.request_doc_id.as_deref() != Some(request_doc_id)
                            || header.session_id
                                != request.session_id.as_deref().unwrap_or_default()
                            || header.role != MessageRole::Assistant
                            || !matches!(
                                header.publication,
                                MessagePublication::RequestExecution { .. }
                                    | MessagePublication::RequestRecovery { .. }
                            )
                        {
                            Ok(CanonicalRequestOutput::Invalid)
                        } else {
                            let presentation = native_presentation(&message);
                            Ok(CanonicalRequestOutput::TerminalMessage {
                                header,
                                message,
                                presentation,
                            })
                        }
                    }
                    Err(error) => {
                        classify_terminal_read_error(&error)
                            .map(Ok)
                            .unwrap_or_else(|| {
                                Err(error.context("resolving canonical terminal output"))
                            })
                    }
                }
            }
        };
    }

    let Some(generation) = request
        .execution_generation
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(CanonicalRequestOutput::Absent);
    };
    let (segments, headers) = load_facts(access, request).await?;
    let observed = segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let messages = headers
        .iter()
        .map(|row| (row.doc_id.as_str(), &row.message))
        .collect::<Vec<_>>();
    let selection = select_live_target(request_doc_id, generation, &observed, &messages);
    let LiveTargetSelection::Selected {
        source,
        writer,
        message_id,
    } = selection
    else {
        return Ok(match selection {
            LiveTargetSelection::Absent => CanonicalRequestOutput::Absent,
            LiveTargetSelection::Conflicted => CanonicalRequestOutput::Conflicted,
            LiveTargetSelection::Selected { .. } => unreachable!(),
        });
    };
    let selected_source = CanonicalSelectedSource {
        request_doc_id: request_doc_id.to_owned(),
        source: source.clone(),
        writer: writer.clone(),
    };
    let view = project_live(&LiveObservation {
        request_doc_id,
        session_id: request
            .session_id
            .as_deref()
            .expect("validated by load_facts"),
        target: LiveTarget {
            request_doc_id,
            source: &source,
            writer: &writer,
            message_id: message_id.as_deref(),
        },
        messages: &messages,
        agent_did: request
            .agent_did
            .as_deref()
            .expect("validated by load_facts"),
        requester_did: request.requester_did.as_deref(),
        records: &observed,
        denied_headers: &[],
        denied_segments: &[],
        dependency_denials: &[],
        owner: OwnerLiveness {
            // This is the last durable active-owner observation, not fresh
            // write authorization. Lease expiry/recovery is decided only by
            // the lifecycle owner; this read projection never compares its
            // own wall clock. A stray generation on Pending therefore cannot
            // establish liveness.
            current_request: observed_request_execution_owner(request),
            live_tools: Vec::new(),
        },
        request_terminal: false,
        terminal_selection: None,
    });
    Ok(match view {
        LiveView::Absent => CanonicalRequestOutput::Absent,
        LiveView::Live { streams } => {
            CanonicalRequestOutput::Live(stream_presentation(&streams, &selected_source))
        }
        LiveView::Settling { streams } => {
            CanonicalRequestOutput::Settling(stream_presentation(&streams, &selected_source))
        }
        LiveView::Loading => CanonicalRequestOutput::Loading,
        LiveView::Denied => CanonicalRequestOutput::Denied,
        LiveView::Conflicted => CanonicalRequestOutput::Conflicted,
        LiveView::Invalid => CanonicalRequestOutput::Invalid,
        LiveView::Retracted => CanonicalRequestOutput::Retracted,
        LiveView::RetainedPartial { streams } => CanonicalRequestOutput::RetainedPartial(streams),
        LiveView::Published { message, native } => CanonicalRequestOutput::Published {
            header: message,
            presentation: CanonicalPresentation {
                selected_source: Some(selected_source),
                ..native_presentation(&native)
            },
            message: native,
        },
    })
}
