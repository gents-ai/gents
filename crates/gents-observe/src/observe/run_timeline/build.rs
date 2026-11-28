use std::collections::BTreeSet;

use super::events::{
    RunTimeline, RunTimelineEvent, TimelineCompactionEvent, TimelineInferenceCallEvent,
    TimelineMessageEvent, TimelineProviderContextReductionEvent, TimelineToolCallEvent,
};
use super::goals::goal_transition_events;
use super::inference::{apply_retry_summaries, sorted_inference_calls};
use super::rendered::{rendered_request_event, rendered_request_tiebreak};
use super::requests::{
    child_bridge_is_corroborated, infer_request_id_for_tool_call, push_request_event,
    resolve_logical_request_id_for_doc,
};
use super::rows::RunTimelineRows;
use super::values::{first_nonempty, timestamp_millis};

pub fn build_run_timeline(mut rows: RunTimelineRows) -> RunTimeline {
    let root_request_id = rows.request.request_id.clone();
    let session_id = rows.request.session_id.clone();
    let mut included_request_ids = BTreeSet::from([root_request_id.clone()]);
    for request in &rows.requests {
        if request.request_id != root_request_id
            && child_bridge_is_corroborated(&rows.request, request, &rows.tool_calls)
        {
            included_request_ids.insert(request.request_id.clone());
        }
    }
    let mut inference_calls = sorted_inference_calls(rows.inference_calls, &included_request_ids);
    for call in &mut inference_calls {
        call.backend_config_fingerprint =
            gents_inference::admission::exportable_backend_fingerprint(
                call.backend_config_fingerprint.as_deref(),
            );
    }
    apply_retry_summaries(&mut rows.request, &mut rows.requests, &inference_calls);

    let mut events = Vec::new();
    push_request_event(&mut events, &rows.request);
    for request in &rows.requests {
        if request.request_id != root_request_id
            && included_request_ids.contains(&request.request_id)
        {
            push_request_event(&mut events, request);
        }
    }

    for call in &inference_calls {
        events.push(RunTimelineEvent::InferenceCall(
            TimelineInferenceCallEvent {
                call_id: call.call_id.clone(),
                runtime_instance_id: call.runtime_instance_id.clone(),
                request_id: call.request_id.clone(),
                call_seq: call.call_seq,
                attempt: call.attempt,
                call_state: call.call_state.clone(),
                failure_reason: call.failure_reason.clone(),
                backend_id: call.backend_id.clone(),
                agent_did: call.agent_did.clone(),
                node_did: call.node_did.clone(),
                call_kind: call.call_kind.clone(),
                priority: call.priority,
                queue_depth_at_enqueue: call.queue_depth_at_enqueue,
                controller_generation: call.controller_generation,
                backend_config_fingerprint: call.backend_config_fingerprint.clone(),
                prompt_tokens: call.prompt_tokens,
                completion_tokens: call.completion_tokens,
                cached_input_tokens: call.cached_input_tokens,
                cache_creation_input_tokens: call.cache_creation_input_tokens,
                reasoning_output_tokens: call.reasoning_output_tokens,
                provider_total_tokens: call.provider_total_tokens,
                usage_completeness: call.usage_completeness.clone(),
                context_accounting: call.context_accounting_json.as_deref().and_then(|json| {
                    gents_inference::rendered_request::ContextAccounting::parse(json)
                        .ok()?
                        .current()
                }),
                queued_at: call.queued_at.clone(),
                started_at: call.started_at.clone(),
                ended_at: call.ended_at.clone(),
                account: None,
            },
        ));
    }

    for rendered in &rows.rendered_requests {
        let belongs = rendered
            .request_id
            .as_deref()
            .is_some_and(|request_id| included_request_ids.contains(request_id));
        if belongs {
            events.push(RunTimelineEvent::RenderedRequest(rendered_request_event(
                rendered,
            )));
        }
    }

    for compaction in &rows.compactions {
        if let Some(request_id) = compaction
            .request_id
            .as_ref()
            .filter(|id| included_request_ids.contains(*id))
        {
            events.push(RunTimelineEvent::Compaction(TimelineCompactionEvent {
                compaction_key: compaction.compaction_key.clone(),
                request_id: request_id.clone(),
                session_id: compaction.session_id.clone(),
                sequence: compaction.sequence,
                summary: compaction.summary.clone(),
                messages_compacted: compaction.messages_compacted,
                compacted_through_sequence: compaction.compacted_through_sequence,
                source_message_versions: compaction
                    .source_message_versions
                    .clone()
                    .unwrap_or_default(),
                original_tokens: compaction.original_tokens,
                compacted_tokens: compaction.compacted_tokens,
                timestamp: compaction.created_at.clone(),
            }));
        }
    }

    for reduction in &rows.provider_context_reductions {
        if included_request_ids.contains(&reduction.request_id) {
            events.push(RunTimelineEvent::ProviderContextReduction(
                TimelineProviderContextReductionEvent {
                    reduction_key: reduction.reduction_key.clone(),
                    reduction_doc_id: reduction.doc_id.clone(),
                    request_id: reduction.request_id.clone(),
                    request_doc_id: reduction.request_doc_id.clone(),
                    session_id: reduction.session_id.clone(),
                    reduction_index: reduction.reduction_index,
                    turn_index: reduction.turn_index,
                    parent_reduction_key: reduction.parent_reduction_key.clone(),
                    producer_call_id: reduction.producer_call_id.clone(),
                    producer_call_seq: reduction.producer_call_seq,
                    messages_compacted: reduction.messages_compacted,
                    original_tokens: reduction.original_tokens,
                    compacted_tokens: reduction.compacted_tokens,
                    timestamp: reduction.created_at.clone(),
                },
            ));
        }
    }

    for message in &rows.messages {
        let request_id = resolve_logical_request_id_for_doc(
            message.header.request_doc_id.as_deref(),
            std::iter::once(&rows.request).chain(rows.requests.iter()),
        )
        .map(ToOwned::to_owned);
        if should_include_event(
            request_id.as_deref(),
            Some(message.session_id.as_str()),
            &included_request_ids,
            session_id.as_deref(),
        ) {
            events.push(RunTimelineEvent::Message(TimelineMessageEvent {
                request_id,
                session_id: message.session_id.clone(),
                sequence: message.sequence,
                doc_id: message.doc_id.clone(),
                header: message.header.clone(),
                message: message.message.clone(),
            }));
        }
    }

    for tool_call in &rows.tool_calls {
        let request_id = infer_request_id_for_tool_call(tool_call, &rows.requests);
        if should_include_event(
            request_id.as_deref(),
            Some(tool_call.session_id.as_str()),
            &included_request_ids,
            session_id.as_deref(),
        ) {
            events.push(RunTimelineEvent::ToolCall(TimelineToolCallEvent {
                request_id,
                session_id: tool_call.session_id.clone(),
                message_sequence: tool_call.message_sequence,
                tool_name: tool_call.tool_name.clone(),
                tool_call_id: tool_call.tool_call_id.clone(),
                args: tool_call.args.clone(),
                result: tool_call.result.clone(),
                status: tool_call.status.clone(),
                lifecycle_state: tool_call.lifecycle_state.clone(),
                selected_service_id: tool_call.selected_service_id.clone(),
                selected_tool_name: tool_call.selected_tool_name.clone(),
                tool_failure_class: tool_call.tool_failure_class.clone(),
                denial_reason: tool_call.denial_reason.clone(),
                denied_argv: tool_call.denied_argv.clone(),
                denied_command: tool_call.denied_command.clone(),
                denied_argument: tool_call.denied_argument.clone(),
                denied_subcommand: tool_call.denied_subcommand.clone(),
                denied_prefix: tool_call.denied_prefix.clone(),
                policy_mode: tool_call.policy_mode.clone(),
                policy_network: tool_call.policy_network.clone(),
                latency_ms: tool_call.latency_ms,
                await_mode: tool_call.await_mode.clone(),
                cancel_cause: tool_call.cancel_cause.clone(),
                child_request_id: tool_call
                    .child_request_id
                    .clone()
                    .filter(|request_id| included_request_ids.contains(request_id)),
                started_at: tool_call.started_at.clone(),
                completed_at: tool_call.completed_at.clone(),
            }));
        }
    }

    events.extend(goal_transition_events(
        &rows.goal_versions,
        session_id.as_deref(),
    ));

    events.sort_by_key(event_sort_key);
    let child_request_ids = child_request_ids(&included_request_ids, &root_request_id);

    RunTimeline {
        request_id: root_request_id,
        request_doc_id: rows.request.doc_id.clone(),
        session_id,
        node_did: rows.request.node_did.clone(),
        agent_did: rows.request.agent_did.clone(),
        request: rows.request,
        session: rows.session,
        child_request_ids,
        inference_calls,
        rendered_request_refs: rows.rendered_request_refs,
        background_completions: Vec::new(),
        background_completion_diagnostics_error: None,
        events,
    }
}

fn should_include_event(
    request_id: Option<&str>,
    event_session_id: Option<&str>,
    included_request_ids: &BTreeSet<String>,
    root_session_id: Option<&str>,
) -> bool {
    request_id
        .map(|request_id| included_request_ids.contains(request_id))
        .unwrap_or_else(|| event_session_id.is_some() && event_session_id == root_session_id)
}

fn event_sort_key(event: &RunTimelineEvent) -> (i64, i64, i64, String) {
    match event {
        RunTimelineEvent::Request(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            0,
            -1,
            event.request_id.clone(),
        ),
        RunTimelineEvent::RenderedRequest(event) => (
            event
                .created_at
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            2,
            event.call_seq.unwrap_or(i64::MAX),
            rendered_request_tiebreak(event),
        ),
        RunTimelineEvent::InferenceCall(event) => (
            first_nonempty([
                event.queued_at.as_deref(),
                event.started_at.as_deref(),
                event.ended_at.as_deref(),
            ])
            .and_then(timestamp_millis)
            .unwrap_or(i64::MIN),
            3,
            event.call_seq,
            event.call_id.clone(),
        ),
        RunTimelineEvent::Compaction(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            4,
            event.sequence,
            event.compaction_key.clone(),
        ),
        RunTimelineEvent::Message(event) => (
            timestamp_millis(&event.header.created_at).unwrap_or(i64::MIN),
            4,
            event.sequence,
            format!("{}:{}", event.session_id, event.sequence),
        ),
        RunTimelineEvent::ToolCall(event) => (
            event
                .started_at
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            5,
            event.message_sequence.unwrap_or(i64::MAX),
            event.tool_call_id.clone(),
        ),
        RunTimelineEvent::ProviderContextReduction(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            1,
            event.reduction_index,
            event.reduction_key.clone(),
        ),
        RunTimelineEvent::GoalTransition(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            8,
            event.height,
            event.commit_cid.clone(),
        ),
    }
}

fn child_request_ids(
    included_request_ids: &BTreeSet<String>,
    root_request_id: &str,
) -> Vec<String> {
    included_request_ids
        .iter()
        .filter(|request_id| request_id.as_str() != root_request_id)
        .cloned()
        .collect()
}
