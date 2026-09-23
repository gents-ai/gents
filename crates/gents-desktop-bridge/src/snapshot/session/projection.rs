use super::*;
use gents_desktop_core::client::canonical_output::{
    project_canonical_live, CanonicalMessageProjection,
};

fn project_message_with_dependencies(
    row: &gents::session::canonical_rows::TranscriptMessageRow,
    observed_messages: &[gents_protocol::output::origin::ObservedMessage<'_>],
    output_segments: &[gents::session::canonical_rows::OutputSegmentRow],
    denied_headers: &[String],
    denied_segments: &[String],
    dependency_denials: &[gents_protocol::output::reconstruction::DependencyDenial],
) -> gents_desktop_core::client::canonical_output::CanonicalMessageProjection {
    use gents_desktop_core::client::canonical_output::{
        project_canonical_message, CanonicalMessageProjection,
    };
    let origin = gents_protocol::output::origin::resolve_origin(
        observed_messages,
        denied_headers,
        &row.doc_id,
        &row.message.agent_did,
        row.message.requester_did.as_deref(),
    );
    match origin {
        Ok(origin) => match project_canonical_message(
            &gents::session::canonical_rows::TranscriptMessageRow {
                doc_id: origin.doc_id.to_string(),
                message: origin.message.clone(),
            },
            output_segments,
            denied_segments,
            dependency_denials,
        ) {
            CanonicalMessageProjection::Ready(_) => {
                project_canonical_message(row, output_segments, denied_segments, dependency_denials)
            }
            other => other,
        },
        Err(gents_protocol::output::origin::OriginError::Denied { doc_id }) => {
            CanonicalMessageProjection::Denied { doc_id }
        }
        Err(error) if error.is_incomplete() => CanonicalMessageProjection::Loading(
            gents_protocol::output::ReconstructionError::UnresolvedClose {
                close_doc_id: row.doc_id.clone(),
            },
        ),
        Err(error) => CanonicalMessageProjection::Invalid(
            gents_protocol::output::ReconstructionError::InvalidStructure {
                detail: error.to_string(),
            },
        ),
    }
}

/// Tool payloads are native message blocks reconstructed from canonical output
/// segments.  The join is deliberately by AgentToolCall's physical `_docID`,
/// never a logical tool key or provider-native id.
fn canonical_tool_payload(
    tool: &AgentToolCallRow,
    messages: &[&gents::session::canonical_rows::TranscriptMessageRow],
    observed_messages: &[gents_protocol::output::origin::ObservedMessage<'_>],
    output_segments: &[gents::session::canonical_rows::OutputSegmentRow],
    denied_headers: &[String],
    denied_segments: &[String],
    dependency_denials: &[gents_protocol::output::reconstruction::DependencyDenial],
) -> (Option<String>, Option<String>, MessageReconstructionView) {
    use gents_protocol::output::reconstruction::{
        reconstruct_presented_payload, reconstruct_stream, ObservedSegment,
    };
    use gents_protocol::output::{MessageBlock, ReconstructionError, ToolResultPart};

    let ready = || MessageReconstructionView {
        state: ReconstructionState::Ready,
        error: None,
        denied_dependency_doc_id: None,
    };
    let failed = |error: ReconstructionError| match error {
        ReconstructionError::AccessDenied { doc_id } => MessageReconstructionView {
            state: ReconstructionState::Denied,
            error: None,
            denied_dependency_doc_id: Some(doc_id),
        },
        error if error.is_incomplete() => MessageReconstructionView {
            state: ReconstructionState::Loading,
            error: Some(error.to_string()),
            denied_dependency_doc_id: None,
        },
        error => MessageReconstructionView {
            state: ReconstructionState::Invalid,
            error: Some(error.to_string()),
            denied_dependency_doc_id: None,
        },
    };
    let (Some(tool_doc_id), Some(agent_did), Some(request_doc_id)) = (
        tool.doc_id.as_deref(),
        tool.agent_did.as_deref(),
        tool.request_doc_id.as_deref(),
    ) else {
        return (
            None,
            None,
            failed(ReconstructionError::InvalidStructure {
                detail: "tool payload has no exact physical owner".to_string(),
            }),
        );
    };
    let observed = output_segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let mut arguments = None;
    let mut result = None;
    let mut found_call = false;
    for header in messages {
        if header.message.agent_did != agent_did
            || header.message.requester_did != tool.requester_did
            || tool.session_id.as_deref() != Some(header.message.session_id.as_str())
            || header.message.request_doc_id.as_deref() != Some(request_doc_id)
        {
            continue;
        }
        let matches_tool = header.message.blocks.iter().any(|block| match block {
            MessageBlock::ToolCall {
                tool_call_doc_id, ..
            }
            | MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } => tool_call_doc_id == tool_doc_id,
            _ => false,
        });
        if !matches_tool {
            continue;
        }
        match project_message_with_dependencies(
            header,
            observed_messages,
            output_segments,
            denied_headers,
            denied_segments,
            dependency_denials,
        ) {
            CanonicalMessageProjection::Ready(_) => {}
            CanonicalMessageProjection::Loading(error)
            | CanonicalMessageProjection::Invalid(error) => return (None, None, failed(error)),
            CanonicalMessageProjection::Denied { doc_id } => {
                return (
                    None,
                    None,
                    failed(ReconstructionError::AccessDenied { doc_id }),
                );
            }
        }
        for block in &header.message.blocks {
            match block {
                MessageBlock::ToolCall {
                    tool_call_doc_id,
                    arguments: reference,
                    ..
                } if tool_call_doc_id == tool_doc_id => {
                    found_call = true;
                    let stream = match reconstruct_stream(
                        &observed,
                        denied_segments,
                        dependency_denials,
                        reference,
                    ) {
                        Ok(stream) => stream,
                        Err(error) => return (None, None, failed(error)),
                    };
                    arguments = Some(stream.text);
                }
                MessageBlock::ToolResult {
                    tool_call_doc_id,
                    parts,
                    ..
                } if tool_call_doc_id == tool_doc_id => {
                    let text = parts
                        .iter()
                        .filter_map(|part| match part {
                            ToolResultPart::Text { text } => Some(reconstruct_presented_payload(
                                &observed,
                                denied_segments,
                                dependency_denials,
                                text,
                            )),
                            ToolResultPart::Media(_) => None,
                        })
                        .collect::<Result<Vec<_>, _>>();
                    let text = match text {
                        Ok(text) => text,
                        Err(error) => return (None, None, failed(error)),
                    };
                    // A matching result with zero text parts is a complete,
                    // legitimately empty textual projection (possibly media-only).
                    result = Some(text.join("\n"));
                }
                _ => {}
            }
        }
    }
    if !found_call {
        if let Some(source) = tool
            .delegated_input
            .as_ref()
            .map(|input| &input.source.close_doc_id)
        {
            if let Some(denied_doc_id) = dependency_denials
                .iter()
                .filter(|denial| &denial.root_close_id == source)
                .map(|denial| &denial.denied_doc_id)
                .min()
                .or_else(|| denied_segments.iter().find(|id| *id == source))
            {
                return (
                    None,
                    None,
                    failed(ReconstructionError::AccessDenied {
                        doc_id: denied_doc_id.clone(),
                    }),
                );
            }
        }
    }
    if !found_call && tool.spawned_by_tool_call_doc_id.is_none() && tool.delegated_input.is_none() {
        return (
            None,
            None,
            MessageReconstructionView {
                state: ReconstructionState::Loading,
                error: Some("waiting for canonical tool-call header".to_string()),
                denied_dependency_doc_id: None,
            },
        );
    }
    (arguments, result, ready())
}

fn canonical_live_tool_output(
    tool: &AgentToolCallRow,
    request: Option<&AgentRequestRow>,
    messages: &[&gents::session::canonical_rows::TranscriptMessageRow],
    output_segments: &[gents::session::canonical_rows::OutputSegmentRow],
    denied_headers: &[String],
    denied_segments: &[String],
    dependency_denials: &[gents_protocol::output::reconstruction::DependencyDenial],
) -> Option<String> {
    use gents_protocol::output::live::{LiveView, OwnerLiveness};
    use gents_protocol::output::{OutputSource, OutputWriter, StreamPayload};

    let (Some(tool_doc_id), Some(request_doc_id), Some(agent_did), Some(session_id)) = (
        tool.doc_id.as_deref(),
        tool.request_doc_id.as_deref(),
        tool.agent_did.as_deref(),
        tool.session_id.as_deref(),
    ) else {
        return None;
    };
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_string(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool_doc_id.to_string(),
    };
    let headers = messages
        .iter()
        .map(|row| (*row).clone())
        .collect::<Vec<_>>();
    let request_terminal = request
        .and_then(|row| row.lifecycle_state)
        .is_some_and(RequestLifecycleState::is_terminal);
    let view = project_canonical_live(
        request_doc_id,
        session_id,
        request_doc_id,
        &source,
        &writer,
        None,
        agent_did,
        tool.requester_did.as_deref(),
        &headers,
        output_segments,
        denied_headers,
        denied_segments,
        dependency_denials,
        OwnerLiveness {
            current_request: None,
            live_tools: if tool.lifecycle_state.as_deref() == Some("running") {
                vec![tool_doc_id]
            } else {
                Vec::new()
            },
        },
        request_terminal,
        request.and_then(|row| row.terminal_output.clone()),
    );
    let streams = match view {
        LiveView::Live { streams } | LiveView::Settling { streams } => streams,
        _ => return None,
    };
    let text = streams
        .into_iter()
        .filter(|stream| stream.declaration.payload == StreamPayload::ToolOutput)
        .map(|stream| stream.text)
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

pub(super) fn build_session_snapshot_from_store_for_agent_with_transcript(
    store: &ClientStore,
    transcript_store: &ClientStore,
    context_store: &ClientStore,
    canonical_dependencies: Option<&gents_desktop_core::client::CanonicalTranscriptDependencies>,
    transcript_is_bounded: bool,
    context_totals_exact: bool,
    include_live_tail: bool,
    agent_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let session_row = store
        .sessions
        .iter()
        .enumerate()
        .find(|(index, row)| {
            row.session_id == session_id
                && agent_did.is_none_or(|agent_did| {
                    row.agent_did == agent_did
                        && source_matches_agent(
                            &store.session_source_agent_dids,
                            *index,
                            agent_did,
                            false,
                        )
                })
        })
        .map(|(_index, row)| row);
    let requests = agent_did.map_or_else(
        || store.requests_for_session(session_id),
        |agent_did| store.requests_for_session_for_agent(session_id, agent_did),
    );
    let goal = store
        .goals
        .iter()
        .filter(|row| {
            row.session_id == session_id
                && agent_did.is_none_or(|agent_did| row.agent_did == agent_did)
        })
        .min_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.goal_id.cmp(&right.goal_id))
        })
        .map(|row| GoalView {
            goal_id: row.goal_id.clone(),
            objective: normalize_optional(row.objective.as_deref()),
            status: normalize_optional(row.status.as_deref()),
            token_budget: row.token_budget,
            tokens_used: row.tokens_used.unwrap_or_default().max(0),
            active_time_seconds: row.active_time_seconds.unwrap_or_default().max(0),
            consecutive_blocked_audits: row.consecutive_blocked_audits.unwrap_or_default().max(0),
            continuation_sequence: row.continuation_sequence.unwrap_or_default().max(0),
            wrapup_requested: row.wrapup_requested.unwrap_or(false),
            wrapup_completed: row.wrapup_completed.unwrap_or(false),
            last_blocked_reason: normalize_optional(row.last_blocked_reason.as_deref()),
            last_failure: normalize_optional(row.last_failure.as_deref()),
            completion_evidence: normalize_optional(row.completion_evidence.as_deref()),
        });

    if session_row.is_none() && requests.is_empty() && goal.is_none() {
        return None;
    }

    let mut transcript = if transcript_is_bounded {
        transcript_store.transcript(session_id)
    } else {
        agent_did.map_or_else(
            || transcript_store.transcript(session_id),
            |agent_did| transcript_store.transcript_for_agent(session_id, agent_did),
        )
    };
    if let Some(session) = session_row {
        transcript
            .messages
            .retain(|row| row.message.requester_did == session.requester_did);
        transcript
            .output_segments
            .retain(|row| row.segment.requester_did == session.requester_did);
        transcript
            .tool_calls
            .retain(|row| row.requester_did == session.requester_did);
    }
    let latest_request_id = preferred_request_id
        .filter(|request_id| {
            requests
                .iter()
                .any(|row| row.request_id == *request_id && !request_is_background_completion(row))
        })
        .map(str::to_owned)
        .or_else(|| {
            agent_did.map_or_else(
                || store.latest_request_id_for_session(session_id),
                |agent_did| store.latest_request_id_for_session_for_agent(session_id, agent_did),
            )
        });
    let latest_request = latest_request_id
        .as_deref()
        .and_then(|request_id| {
            requests
                .iter()
                .find(|row| row.request_id == request_id)
                .copied()
        })
        .or_else(|| {
            latest_request_id
                .is_none()
                .then(|| {
                    requests
                        .iter()
                        .rev()
                        .find(|request| !request_is_background_completion(request))
                        .copied()
                })
                .flatten()
        });
    let retry_eligibility = project_retry_eligibility(latest_request);
    let latest_request_outcome = latest_request.and_then(|request| {
        let failure_reason = normalize_optional(request.failure_reason.as_deref());
        let evidence = RequestEvidence {
            interrupt_requested_at: normalize_optional(request.interrupt_requested_at.as_deref()),
            caused_by_parent_request_id: normalize_optional(
                request.caused_by_parent_request_id.as_deref(),
            ),
        };
        let cancel_cause = crate::cause_derivation::derive_request_cause(
            request.lifecycle_state.map(|state| state.as_str()),
            &evidence,
            normalize_optional(request.terminalized_at.as_deref()),
        );
        (failure_reason.is_some() || cancel_cause.is_some()).then_some(RequestOutcomeView {
            failure_reason,
            cancel_cause,
        })
    });
    let request_turn_state = latest_request_id
        .as_deref()
        .and_then(|request_id| {
            agent_did.map_or_else(
                || store.derive_turn_for_request(request_id),
                |agent_did| store.derive_turn_for_request_for_agent(request_id, agent_did),
            )
        })
        .or_else(|| {
            if agent_did.is_none() {
                store.derive_turn(session_id)
            } else {
                None
            }
        });
    // AgentSession's observation is the canonical index projection of the
    // exact latest AgentRequest identity. A desktop replica can still hold an
    // older request row while operator GraphQL has already advanced both the
    // request and session. Let the exact observation advance (never regress)
    // the detailed turn projection until the request row catches up.
    let observed_turn_state = session_row
        .and_then(|session| session.observation.as_ref())
        .and_then(|observation| observation.latest_request.as_ref())
        .filter(|observation| latest_request_id.as_deref() == Some(observation.request_id.as_str()))
        .and_then(|observation| {
            gents_protocol::client_protocol::derive_persisted_attempt(
                observation.lifecycle_state.as_str(),
                false,
            )
        });
    let turn_state = match (request_turn_state, observed_turn_state) {
        (Some(request), Some(observed)) if observed.rank() > request.rank() => Some(observed),
        (Some(request), _) => Some(request),
        (None, observed) => observed,
    };
    let turn_state_label = turn_state.map(turn_state_label).map(str::to_owned);
    let pending_turn = (include_live_tail && context_totals_exact)
        .then_some(latest_request_id.as_deref())
        .flatten()
        .as_deref()
        .and_then(|request_id| {
            build_pending_turn(store, context_store, agent_did, session_id, request_id)
        });
    let resolved_agent_did = session_row
        .map(|row| row.agent_did.clone())
        .or_else(|| latest_request.and_then(|row| normalize_optional(row.agent_did.as_deref())));
    let resolved_behavior_id = session_row
        .and_then(|row| normalize_optional(Some(row.behavior_id.as_str())))
        .or_else(|| latest_request.and_then(|row| normalize_optional(row.behavior_id.as_deref())));
    let context = build_session_context_from_stores(
        store,
        context_store,
        resolved_agent_did.as_deref(),
        resolved_behavior_id.as_deref(),
        session_id,
        context_totals_exact,
    );

    let requests_by_doc_id: HashMap<&str, &AgentRequestRow> = requests
        .iter()
        .filter_map(|request| request.doc_id.as_deref().map(|doc_id| (doc_id, *request)))
        .collect();
    let mut observed_messages = transcript
        .messages
        .iter()
        .map(|row| gents_protocol::output::origin::ObservedMessage {
            doc_id: &row.doc_id,
            message: &row.message,
        })
        .collect::<Vec<_>>();
    if let Some(dependencies) = canonical_dependencies {
        observed_messages.extend(dependencies.origin_headers.iter().map(|row| {
            gents_protocol::output::origin::ObservedMessage {
                doc_id: &row.doc_id,
                message: &row.message,
            }
        }));
    }
    let mut output_segment_rows = transcript
        .output_segments
        .iter()
        .map(|row| (*row).clone())
        .collect::<Vec<_>>();
    if let Some(dependencies) = canonical_dependencies {
        output_segment_rows.extend(dependencies.output_segments.iter().cloned());
    }
    let denied_headers = canonical_dependencies
        .map(|dependencies| dependencies.denied_header_doc_ids.as_slice())
        .unwrap_or_default();
    let denied_segments = canonical_dependencies
        .map(|dependencies| dependencies.denied_segment_doc_ids.as_slice())
        .unwrap_or_default();
    let dependency_denials = canonical_dependencies
        .map(|dependencies| dependencies.dependency_denials.as_slice())
        .unwrap_or_default();
    let messages = transcript
        .messages
        .iter()
        .copied()
        .map(|row| {
            // Forks resolve only against the exact rows supplied by the
            // dependency reader.  Missing facts remain Loading; there is no
            // parent-session scan or serialized-text fallback.
            let projection = project_message_with_dependencies(
                row,
                &observed_messages,
                &output_segment_rows,
                denied_headers,
                denied_segments,
                dependency_denials,
            );
            let (
                reconstructed,
                reconstruction_state,
                reconstruction_error,
                denied_dependency_doc_id,
            ) = match projection {
                CanonicalMessageProjection::Ready(message) => {
                    (Some(message), ReconstructionState::Ready, None, None)
                }
                CanonicalMessageProjection::Loading(error) => (
                    None,
                    ReconstructionState::Loading,
                    Some(error.to_string()),
                    None,
                ),
                CanonicalMessageProjection::Denied { doc_id } => {
                    (None, ReconstructionState::Denied, None, Some(doc_id))
                }
                CanonicalMessageProjection::Invalid(error) => (
                    None,
                    ReconstructionState::Invalid,
                    Some(error.to_string()),
                    None,
                ),
            };
            let presentation = reconstructed.as_ref().map(present_message);

            MessageView {
                message_key: row.message.message_key.clone(),
                request_id: row.message.request_doc_id.clone(),
                sequence: Some(i64::from(row.message.sequence)),
                role: Some(
                    match row.message.role {
                        gents_protocol::output::MessageRole::System => "system",
                        gents_protocol::output::MessageRole::User => "user",
                        gents_protocol::output::MessageRole::Assistant => "assistant",
                    }
                    .to_string(),
                ),
                display_role: presentation
                    .as_ref()
                    .map(|presentation| presentation.role.label().to_ascii_lowercase()),
                display_content: presentation.as_ref().and_then(|presentation| {
                    normalize_optional(Some(presentation.body_markdown.as_str()))
                }),
                reasoning: presentation.as_ref().and_then(|presentation| {
                    presentation
                        .reasoning_markdown
                        .as_deref()
                        .and_then(|reasoning| normalize_optional(Some(reasoning)))
                }),
                has_tool_calls: presentation
                    .as_ref()
                    .is_some_and(|presentation| presentation.has_tool_calls),
                has_tool_results: presentation
                    .as_ref()
                    .is_some_and(|presentation| presentation.has_tool_results),
                reconstruction_state,
                reconstruction_error,
                denied_dependency_doc_id,
                runtime_control: message_is_runtime_control(row, &requests_by_doc_id),
                timestamp: Some(row.message.created_at.clone()),
            }
        })
        .collect::<Vec<_>>();

    let tool_calls = transcript
        .tool_calls
        .into_iter()
        .map(|row| {
            let (args, result, reconstruction) = canonical_tool_payload(
                row,
                &transcript.messages,
                &observed_messages,
                &output_segment_rows,
                denied_headers,
                denied_segments,
                dependency_denials,
            );
            let partial_output_tail = canonical_live_tool_output(
                row,
                row.request_doc_id
                    .as_deref()
                    .and_then(|id| requests_by_doc_id.get(id).copied()),
                &transcript.messages,
                &output_segment_rows,
                denied_headers,
                denied_segments,
                dependency_denials,
            );
            let cancel_cause =
                if let Some(persisted) = row.cancel_cause.as_deref().filter(|s| !s.is_empty()) {
                    Some(DerivedCancelCauseView {
                        cause: persisted.to_string(),
                        source: "toolLifecycle".into(),
                        confidence: "direct".into(),
                        at: normalize_optional(row.completed_at.as_deref()),
                        evidence: vec![format!(
                            "AgentToolCall.cancel_cause = {persisted:?} (persisted)"
                        )],
                    })
                } else {
                    // Canonical tool ownership is the physical request binding.
                    // Missing evidence cannot be borrowed from the latest turn
                    // or a coincidentally matching logical request identifier.
                    let req_for_tool = row
                        .request_doc_id
                        .as_deref()
                        .and_then(|id| requests_by_doc_id.get(id).copied());
                    let req_evidence = req_for_tool
                        .map(|r| RequestEvidence {
                            interrupt_requested_at: r.interrupt_requested_at.clone(),
                            caused_by_parent_request_id: r.caused_by_parent_request_id.clone(),
                        })
                        .unwrap_or_default();
                    let tool_evidence = ToolCallEvidence {
                        lifecycle_state: row.lifecycle_state.clone(),
                        deadline_at: row.deadline_at.clone(),
                        cancel_policy: row.cancel_policy.clone(),
                        completed_at: row.completed_at.clone(),
                        timed_out: row.lifecycle_state.as_deref() == Some("timedOut"),
                    };
                    derive_tool_call_cause(&req_evidence, &tool_evidence)
                };
            ToolCallView {
                tool_call_key: row.tool_call_key.clone(),
                request_id: normalize_optional(row.request_id.as_deref()),
                message_sequence: row.message_sequence,
                tool_name: normalize_optional(row.tool_name.as_deref()),
                tool_call_id: normalize_optional(row.tool_call_id.as_deref()),
                args,
                partial_output_tail,
                partial_output_seq: None,
                result,
                reconstruction,
                status: normalize_optional(row.status.as_deref()),
                lifecycle_state: normalize_optional(row.lifecycle_state.as_deref()),
                child_request_id: normalize_optional(row.child_request_id.as_deref()),
                await_mode: normalize_optional(row.await_mode.as_deref()),
                cancel_policy: normalize_optional(row.cancel_policy.as_deref()),
                started_at: normalize_optional(row.started_at.as_deref()),
                deadline_at: normalize_optional(row.deadline_at.as_deref()),
                completed_at: normalize_optional(row.completed_at.as_deref()),
                denial: command_denial_from_row(&row),
                cancel_cause,
            }
        })
        .collect::<Vec<_>>();

    let mut timeline_items = build_rendered_timeline(&messages, &tool_calls, pending_turn.as_ref());
    if include_live_tail {
        if let Some(request_id) = latest_request_id.as_deref() {
            if let Some((content, reasoning)) = super::live_delta::canonical_live_text(
                store,
                context_store,
                session_id,
                agent_did,
                request_id,
            ) {
                let content = normalize_optional(Some(&content));
                let reasoning = normalize_optional(Some(&reasoning));
                if content.is_some() || reasoning.is_some() {
                    timeline_items.push(crate::types::RenderedTimelineItem::LiveAssistant {
                        item_key: format!("live-assistant-{request_id}"),
                        content,
                        reasoning,
                    });
                }
            }
        }
    }

    Some(DesktopSessionSnapshot {
        session_id: session_id.to_string(),
        agent_did: resolved_agent_did,
        behavior_id: resolved_behavior_id,
        title: session_row.and_then(|row| {
            row.title
                .as_ref()
                .and_then(|title| normalize_optional(Some(&title.text)))
        }),
        preview_text: session_row.and_then(|row| {
            row.observation
                .as_ref()
                .and_then(|observation| normalize_optional(observation.preview.as_deref()))
        }),
        status: session_row.map(|row| {
            if row.closed_at.is_some() {
                "closed".to_string()
            } else {
                "active".to_string()
            }
        }),
        goal,
        turn_state: turn_state_label,
        latest_request_id,
        retry_eligibility,
        latest_request_outcome,
        pending_turn,
        context,
        timeline_items,
        hydration: None,
        timeline_page: None,
        projection_revision: None,
        messages,
        tool_calls,
    })
}

#[cfg(test)]
mod canonical_projection_tests {
    use super::*;
    use gents::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
    use gents_protocol::output::{
        MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
        SegmentRun, StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    use serde_json::json;

    #[test]
    fn running_tool_keeps_canonical_live_output_after_parent_terminal() {
        let tool: AgentToolCallRow = serde_json::from_value(json!({
            "_docID": "tool-physical", "tool_call_key": "tool-key",
            "agent_did": "did:test:agent", "request_doc_id": "request",
            "session_id": "session", "lifecycle_state": "running"
        }))
        .expect("tool row");
        let request: AgentRequestRow = serde_json::from_value(json!({
            "_docID": "request", "request_id": "request-id",
            "agent_did": "did:test:agent", "session_id": "session",
            "lifecycle_state": "completed",
            "terminal_output": TerminalOutput::NoMessage,
        }))
        .expect("parent row");
        let row = OutputSegmentRow {
            doc_id: "flush-0".to_string(),
            segment: OutputSegment {
                agent_did: "did:test:agent".to_string(),
                requester_did: None,
                session_id: "session".to_string(),
                request_doc_id: "request".to_string(),
                source: OutputSource::ToolCall {
                    tool_call_doc_id: "tool-physical".to_string(),
                },
                writer: OutputWriter::ToolExecution {
                    tool_call_doc_id: "tool-physical".to_string(),
                },
                ordinal: Some(0),
                runs: vec![SegmentRun {
                    stream: 0,
                    bytes: 9,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::ToolOutput,
                    }),
                }],
                payload: "live text".to_string(),
                close: None,
                created_at: "2026-09-01T00:00:00Z".to_string(),
            },
        };
        assert_eq!(
            canonical_live_tool_output(&tool, Some(&request), &[], &[row], &[], &[], &[]),
            Some("live text".to_string())
        );
    }

    #[test]
    fn spawned_background_row_has_no_direct_call_header() {
        let tool: AgentToolCallRow = serde_json::from_value(json!({
            "_docID": "child-physical", "tool_call_key": "child-key",
            "agent_did": "did:test:agent", "request_doc_id": "request",
            "session_id": "session", "lifecycle_state": "running",
            "spawned_by_tool_call_doc_id": "spawn-physical"
        }))
        .expect("spawned tool row");
        let (args, result, reconstruction) =
            canonical_tool_payload(&tool, &[], &[], &[], &[], &[], &[]);
        assert!(args.is_none());
        assert!(result.is_none());
        assert_eq!(reconstruction.state, ReconstructionState::Ready);
    }

    #[test]
    fn tool_result_dependency_is_never_presented_partially() {
        use gents_protocol::output::{
            MessageBlock, PayloadPresentation, PayloadRef, PresentedPayload, SourceClose,
            ToolResultPart,
        };
        use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

        let tool: AgentToolCallRow = serde_json::from_value(json!({
            "_docID": "tool-physical", "tool_call_key": "tool-key",
            "agent_did": "did:test:agent", "request_doc_id": "request",
            "session_id": "session", "lifecycle_state": "completed"
        }))
        .expect("direct tool row");
        let mut call_header = header("call", "session");
        call_header.message.message_key = "session:call".to_string();
        call_header.message.requester_did = None;
        call_header.message.blocks = vec![MessageBlock::ToolCall {
            tool_call_doc_id: "tool-physical".to_string(),
            id: "tool-id".to_string(),
            call_id: None,
            name: "example".to_string(),
            arguments: PayloadRef {
                close_doc_id: "args-close".to_string(),
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }];
        let args = OutputSegmentRow {
            doc_id: "args-close".to_string(),
            segment: OutputSegment {
                agent_did: "did:test:agent".to_string(),
                requester_did: None,
                session_id: "session".to_string(),
                request_doc_id: "request".to_string(),
                source: OutputSource::ProviderTurn {
                    scope: CaptureScope {
                        kind: CaptureScopeKind::Inference,
                        seq: 1,
                    },
                    turn_index: 0,
                    attempt: 0,
                },
                writer: OutputWriter::RequestExecution {
                    execution_generation: "generation".to_string(),
                },
                ordinal: Some(0),
                runs: vec![SegmentRun {
                    stream: 0,
                    bytes: 2,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::ToolArguments {
                            id: "tool-id".to_string(),
                            call_id: None,
                            name: "example".to_string(),
                        },
                    }),
                }],
                payload: "{}".to_string(),
                close: Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    segments: 1,
                    stream_bytes: vec![2],
                }),
                created_at: "2026-09-01T00:00:00Z".to_string(),
            },
        };
        let mut result_header = header("result", "session");
        result_header.message.message_key = "session:result".to_string();
        result_header.message.sequence = 8;
        result_header.message.requester_did = None;
        result_header.message.role = MessageRole::User;
        result_header.message.publication = MessagePublication::ToolDelivery {
            tool_call_doc_id: "tool-physical".to_string(),
        };
        result_header.message.blocks = vec![MessageBlock::ToolResult {
            tool_call_doc_id: "tool-physical".to_string(),
            id: "tool-id".to_string(),
            call_id: None,
            parts: vec![ToolResultPart::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: "missing-close".to_string(),
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            }],
        }];
        let rows = vec![call_header, result_header];
        let messages = rows.iter().collect::<Vec<_>>();
        let observed_rows = observed(&rows);
        let (_, result, loading) = canonical_tool_payload(
            &tool,
            &messages,
            &observed_rows,
            &[args.clone()],
            &[],
            &[],
            &[],
        );
        assert!(result.is_none());
        assert_eq!(loading.state, ReconstructionState::Loading);
        let (_, result, denied) = canonical_tool_payload(
            &tool,
            &messages,
            &observed_rows,
            &[args.clone()],
            &[],
            &["missing-close".to_string()],
            &[],
        );
        assert!(result.is_none());
        assert_eq!(denied.state, ReconstructionState::Denied);

        let result_segment = OutputSegmentRow {
            doc_id: "result-close".to_string(),
            segment: OutputSegment {
                agent_did: "did:test:agent".to_string(),
                requester_did: None,
                session_id: "session".to_string(),
                request_doc_id: "request".to_string(),
                source: OutputSource::ToolCall {
                    tool_call_doc_id: "tool-physical".to_string(),
                },
                writer: OutputWriter::ToolExecution {
                    tool_call_doc_id: "tool-physical".to_string(),
                },
                ordinal: Some(0),
                runs: vec![SegmentRun {
                    stream: 0,
                    bytes: 5,
                    declaration: Some(StreamDeclaration {
                        block_index: 0,
                        part_index: 0,
                        payload: StreamPayload::ToolOutput,
                    }),
                }],
                payload: "first".to_string(),
                close: Some(SourceClose::Closed {
                    outcome: OutputOutcome::Complete,
                    segments: 1,
                    stream_bytes: vec![5],
                }),
                created_at: "2026-09-01T00:01:00Z".to_string(),
            },
        };
        let mut missing_media = rows[1].clone();
        if let MessageBlock::ToolResult { parts, .. } = &mut missing_media.message.blocks[0] {
            if let ToolResultPart::Text { text } = &mut parts[0] {
                text.output.close_doc_id = "result-close".to_string();
            }
            parts.push(ToolResultPart::Media(gents_protocol::output::MediaBlock {
                kind: gents_protocol::output::MediaKind::Image,
                data: gents_protocol::output::MediaData::Base64 {
                    data: PayloadRef {
                        close_doc_id: "missing-media".to_string(),
                        stream: 1,
                    },
                },
                media_type: None,
                detail: None,
                additional_params: None,
            }));
        }
        let media_rows = vec![rows[0].clone(), missing_media];
        let media_messages = media_rows.iter().collect::<Vec<_>>();
        let (_, result, missing_media_state) = canonical_tool_payload(
            &tool,
            &media_messages,
            &observed(&media_rows),
            &[args.clone(), result_segment],
            &[],
            &[],
            &[],
        );
        assert!(
            result.is_none(),
            "a valid text part cannot mask missing media"
        );
        assert_eq!(missing_media_state.state, ReconstructionState::Loading);

        let mut valid_empty = rows[1].clone();
        if let MessageBlock::ToolResult { parts, .. } = &mut valid_empty.message.blocks[0] {
            parts.clear();
        }
        let empty_rows = vec![rows[0].clone(), valid_empty];
        let empty_messages = empty_rows.iter().collect::<Vec<_>>();
        let (_, result, ready) = canonical_tool_payload(
            &tool,
            &empty_messages,
            &observed(&empty_rows),
            &[args],
            &[],
            &[],
            &[],
        );
        assert_eq!(result.as_deref(), Some(""));
        assert_eq!(ready.state, ReconstructionState::Ready);
    }

    fn header(doc_id: &str, session_id: &str) -> TranscriptMessageRow {
        TranscriptMessageRow {
            doc_id: doc_id.to_string(),
            message: TranscriptMessage {
                message_key: format!("{session_id}:message"),
                session_id: session_id.to_string(),
                agent_did: "did:test:agent".to_string(),
                requester_did: Some("did:test:requester".to_string()),
                request_doc_id: Some("request".to_string()),
                publication: MessagePublication::RequestExecution {
                    execution_generation: "generation".to_string(),
                },
                outcome: OutputOutcome::Complete,
                sequence: 7,
                role: MessageRole::Assistant,
                native_id: None,
                blocks: Vec::new(),
                created_at: "2026-09-01T00:00:00Z".to_string(),
            },
        }
    }

    fn observed<'a>(
        rows: &'a [TranscriptMessageRow],
    ) -> Vec<gents_protocol::output::origin::ObservedMessage<'a>> {
        rows.iter()
            .map(|row| gents_protocol::output::origin::ObservedMessage {
                doc_id: &row.doc_id,
                message: &row.message,
            })
            .collect()
    }

    #[test]
    fn missing_denied_and_invalid_fork_dependencies_remain_distinct() {
        use gents_desktop_core::client::canonical_output::CanonicalMessageProjection;

        let origin = header("origin", "parent");
        let mut child = header("child", "child-session");
        child.message.request_doc_id = None;
        child.message.publication = MessagePublication::Fork {
            origin_message_doc_id: "origin".to_string(),
        };

        let child_only = vec![child.clone()];
        assert!(matches!(
            project_message_with_dependencies(&child, &observed(&child_only), &[], &[], &[], &[],),
            CanonicalMessageProjection::Loading(_)
        ));
        assert!(matches!(
            project_message_with_dependencies(
                &child,
                &observed(&child_only),
                &[],
                &["origin".to_string()],
                &[],
                &[],
            ),
            CanonicalMessageProjection::Denied { ref doc_id } if doc_id == "origin"
        ));

        let mut invalid_origin = origin.clone();
        invalid_origin.message.sequence = 8;
        let invalid_rows = vec![child.clone(), invalid_origin];
        assert!(matches!(
            project_message_with_dependencies(&child, &observed(&invalid_rows), &[], &[], &[], &[],),
            CanonicalMessageProjection::Invalid(_)
        ));
    }

    #[test]
    fn exact_origin_header_enables_fork_without_rendering_parent_membership() {
        use gents_desktop_core::client::canonical_output::CanonicalMessageProjection;

        let origin = header("origin", "parent");
        let mut child = header("child", "child-session");
        child.message.request_doc_id = None;
        child.message.publication = MessagePublication::Fork {
            origin_message_doc_id: "origin".to_string(),
        };
        let rows = vec![child.clone(), origin];
        assert!(matches!(
            project_message_with_dependencies(&child, &observed(&rows), &[], &[], &[], &[],),
            CanonicalMessageProjection::Ready(_)
        ));
    }
}
