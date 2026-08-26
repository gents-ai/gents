use super::*;

pub(super) fn build_session_snapshot_from_store_for_agent_with_transcript(
    store: &ClientStore,
    transcript_store: &ClientStore,
    context_store: &ClientStore,
    transcript_is_bounded: bool,
    context_totals_exact: bool,
    include_live_tail: bool,
    agent_did: Option<&str>,
    session_id: &str,
    preferred_request_id: Option<&str>,
) -> Option<DesktopSessionSnapshot> {
    let conversation = store.conversations.iter().find(|row| {
        row.session_id == session_id
            && agent_did.is_none_or(|agent_did| row.agent_did.as_deref() == Some(agent_did))
    });
    let session_row = store
        .sessions
        .iter()
        .enumerate()
        .find(|(index, row)| {
            row.session_id == session_id
                && agent_did.is_none_or(|agent_did| {
                    source_matches_agent(&store.session_source_agent_dids, *index, agent_did, false)
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

    if conversation.is_none() && session_row.is_none() && requests.is_empty() && goal.is_none() {
        return None;
    }

    let transcript = if transcript_is_bounded {
        transcript_store.transcript(session_id)
    } else {
        agent_did.map_or_else(
            || transcript_store.transcript(session_id),
            |agent_did| transcript_store.transcript_for_agent(session_id, agent_did),
        )
    };
    let latest_request_id = preferred_request_id
        .filter(|request_id| {
            requests.iter().any(|row| {
                row.request_id == *request_id && !request_is_deprecated_background_completion(row)
            })
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
                        .find(|request| !request_is_deprecated_background_completion(request))
                        .copied()
                })
                .flatten()
        });
    let retry_eligibility = project_retry_eligibility(latest_request);
    let turn_state = latest_request_id
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
    let turn_state_label = turn_state.map(turn_state_label).map(str::to_owned);
    let latest_response = latest_request_id
        .as_deref()
        .and_then(|request_id| {
            agent_did.map_or_else(
                || store.latest_response_for_request(request_id),
                |agent_did| store.latest_response_for_request_for_agent(request_id, agent_did),
            )
        })
        .map(|row| {
            let req_evidence = latest_request
                .map(|r| RequestEvidence {
                    interrupt_requested_at: r.interrupt_requested_at.clone(),
                    caused_by_parent_request_id: r.caused_by_parent_request_id.clone(),
                })
                .unwrap_or_default();
            let resp_evidence = ResponseEvidence {
                interrupted_at: normalize_optional(row.interrupted_at.as_deref()),
            };
            let cancel_cause = derive_response_cause(&req_evidence, &resp_evidence);
            let backend_id =
                latest_request.and_then(|r| normalize_optional(r.backend_id.as_deref()));
            ResponseView {
                status: normalize_optional(row.status.as_deref()),
                content: row
                    .content
                    .as_deref()
                    .map(normalize_markdown_text)
                    .filter(|value| !value.is_empty()),
                reasoning: row
                    .reasoning
                    .as_deref()
                    .map(normalize_markdown_text)
                    .filter(|value| !value.is_empty()),
                error_message: normalize_optional(row.error_message.as_deref()),
                token_count: row.token_count,
                materialized_message_sequence: row.materialized_message_sequence,
                materialized_at: normalize_optional(row.materialized_at.as_deref()),
                interrupted_at: normalize_optional(row.interrupted_at.as_deref()),
                completed_at: normalize_optional(row.completed_at.as_deref()),
                cancel_cause,
                backend_id,
            }
        });
    let active_response_overlay = latest_response.clone().filter(|response| {
        let response_status = response
            .status
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        include_live_tail
            && matches!(
                turn_state,
                Some(gents_protocol::client_protocol::ClientTurnState::WaitingForClaim)
                    | Some(gents_protocol::client_protocol::ClientTurnState::Streaming)
            )
            && response.materialized_message_sequence.is_none()
            && response.interrupted_at.is_none()
            && !matches!(
                response_status.as_str(),
                "complete" | "completed" | "error" | "failed"
            )
            && (response
                .content
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                || response
                    .reasoning
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()))
    });
    let pending_turn = (include_live_tail && context_totals_exact)
        .then_some(latest_request_id.as_deref())
        .flatten()
        .as_deref()
        .and_then(|request_id| {
            build_pending_turn(store, context_store, agent_did, session_id, request_id)
        });
    let resolved_agent_did = conversation
        .and_then(|row| normalize_optional(row.agent_did.as_deref()))
        .or_else(|| latest_request.and_then(|row| normalize_optional(row.agent_did.as_deref())));
    let resolved_behavior_id = conversation
        .and_then(|row| normalize_optional(row.behavior_id.as_deref()))
        .or_else(|| session_row.and_then(|row| normalize_optional(row.behavior_id.as_deref())))
        .or_else(|| latest_request.and_then(|row| normalize_optional(row.behavior_id.as_deref())));
    let decoded_messages = transcript
        .messages
        .iter()
        .map(|row| {
            row.role
                .as_deref()
                .zip(row.content.as_deref())
                .map(|(role, content)| {
                    gents_protocol::transcript::decode_persisted_message(role, content)
                })
        })
        .collect::<Vec<_>>();
    let context = build_session_context_from_stores(
        store,
        context_store,
        resolved_agent_did.as_deref(),
        resolved_behavior_id.as_deref(),
        session_id,
        context_totals_exact,
    );

    let requests_by_id: HashMap<&str, &AgentRequestRow> = requests
        .iter()
        .map(|request| (request.request_id.as_str(), *request))
        .collect();
    let keyed_steering_request_ids = keyed_steering_request_ids(&transcript.messages);
    let messages = transcript
        .messages
        .into_iter()
        .zip(decoded_messages)
        .map(|(row, decoded_message)| {
            let role = normalize_optional(row.role.as_deref());
            let content = normalize_optional(row.content.as_deref());
            let presentation = decoded_message.as_ref().map(present_message);

            MessageView {
                message_key: row.message_key.clone(),
                request_id: row.request_id.clone(),
                sequence: row.sequence,
                role,
                content,
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
                runtime_control: message_is_runtime_control(
                    row,
                    &requests_by_id,
                    &keyed_steering_request_ids,
                ),
                timestamp: normalize_optional(row.timestamp.as_deref()),
            }
        })
        .collect::<Vec<_>>();

    let tool_calls = transcript
        .tool_calls
        .into_iter()
        .map(|row| {
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
                    let req_for_tool = row
                        .request_id
                        .as_deref()
                        .and_then(|rid| requests_by_id.get(rid).copied())
                        .or(latest_request);
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
                args: normalize_optional(row.args.as_deref()),
                partial_output_tail: normalize_optional(row.partial_output_tail.as_deref()),
                partial_output_seq: row.partial_output_seq,
                result: normalize_optional(row.result.as_deref()),
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

    let tool_results = transcript
        .tool_results
        .into_iter()
        .map(|row| ToolResultView {
            tool_name: normalize_optional(row.tool_name.as_deref()),
            tool_input: normalize_optional(row.tool_input.as_deref()),
            output_text: normalize_optional(row.output_text.as_deref()),
            truncated: row.truncated,
            created_at: normalize_optional(row.created_at.as_deref()),
        })
        .collect::<Vec<_>>();

    let timeline_items = build_rendered_timeline(
        &messages,
        &tool_calls,
        pending_turn.as_ref(),
        active_response_overlay.as_ref(),
        active_response_overlay
            .as_ref()
            .and(latest_request_id.as_deref()),
    );

    Some(DesktopSessionSnapshot {
        session_id: session_id.to_string(),
        agent_did: resolved_agent_did,
        behavior_id: resolved_behavior_id,
        title: conversation.and_then(|row| normalize_optional(row.title.as_deref())),
        preview_text: conversation.and_then(|row| normalize_optional(row.preview_text.as_deref())),
        status: conversation
            .and_then(|row| normalize_optional(row.status.as_deref()))
            .or_else(|| session_row.and_then(|row| normalize_optional(row.status.as_deref()))),
        goal,
        turn_state: turn_state_label,
        latest_request_id,
        retry_eligibility,
        latest_response,
        active_response_overlay,
        pending_turn,
        context,
        timeline_items,
        timeline_page: None,
        projection_revision: None,
        messages,
        tool_calls,
        tool_results,
    })
}
