//! Row fetch for [`crate::run_timeline`]: loads the persisted documents a
//! request's timeline is reconstructed from, over either transport
//! ([`ConfigAccess::Graphql`] or [`ConfigAccess::Local`]). Lifted from the
//! CLI `trace` command so the desktop client shares one fetcher.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::descendant_graph::{
    resolve_descendant_graph, resolve_descendant_root_request_id, DescendantGraphAccess,
    DescendantQuery, MAX_DESCENDANT_PAGE_LIMIT,
};
use crate::graphql::escape_graphql_string;
use crate::run_timeline::{
    build_run_timeline, RunActivityRows, RunTimeline, RunTimelineRows, TimelineCompactionRow,
    TimelineConversationRow, TimelineGoalVersionRow, TimelineInferenceCallRow, TimelineMessageRow,
    TimelineProviderContextReductionRow, TimelineRenderedRequestRef, TimelineRenderedRequestRow,
    TimelineRequestRow, TimelineResponseRow, TimelineSessionRow, TimelineToolApprovalRow,
    TimelineToolCallRow,
};
use gents_protocol::graphql::graphql_rows_from_response;

const MAX_RUN_ACTIVITY_ROWS: usize = 10_000;

pub async fn load_run_timeline(access: &ConfigAccess, request_id: &str) -> Result<RunTimeline> {
    let mut timeline = build_run_timeline(load_run_timeline_rows(access, request_id).await?);
    match load_timeline_descendant_edges(access, request_id).await {
        Ok(edges) => timeline.descendant_edges = edges,
        Err(error) => {
            timeline.descendant_graph_diagnostics_error = Some(error.to_string());
        }
    }
    if let Some(agent_did) = timeline.agent_did.as_deref() {
        match crate::load_background_completion_diagnostics(access, agent_did).await {
            Ok(diagnostics) => {
                timeline.background_completions = diagnostics
                    .epochs
                    .into_iter()
                    .filter(|epoch| {
                        timeline
                            .session_id
                            .as_deref()
                            .is_some_and(|session_id| epoch.session_id == session_id)
                            || epoch.root_request_id == timeline.request_id
                            || epoch.active_request_id == timeline.request_id
                    })
                    .collect();
            }
            Err(error) => {
                timeline.background_completion_diagnostics_error = Some(error.to_string());
            }
        }
    }
    Ok(timeline)
}

/// Load the prompt-free subset of ordinary timeline rows needed by live run
/// observers. One bounded query serves CLI and future bridge projections.
pub async fn load_run_activity_rows(
    access: &ConfigAccess,
    request_ids: &[String],
    session_ids: &[String],
) -> Result<RunActivityRows> {
    let request_ids = request_ids
        .iter()
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .collect::<BTreeSet<_>>();
    if request_ids.is_empty() {
        return Ok(RunActivityRows::default());
    }
    let request_list = request_ids
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let session_ids = session_ids
        .iter()
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .collect::<BTreeSet<_>>();
    let session_query = if session_ids.is_empty() {
        String::new()
    } else {
        let session_list = session_ids
            .iter()
            .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "AgentSession(filter: {{ session_id: {{ _in: [{session_list}] }} }}) {{ session_id behavior_id started ended status }}"
        )
    };
    let limit = MAX_RUN_ACTIVITY_ROWS + 1;
    let response = access
        .execute(&format!(
            r#"{{
                {session_query}
                InferenceCall(
                    filter: {{
                        request_id: {{ _in: [{request_list}] }},
                        call_kind: {{ _eq: "inference" }}
                    }},
                    order: {{ started_at: ASC }}, limit: {limit}
                ) {{
                    call_id request_id call_seq attempt call_state failure_reason
                    queued_at started_at ended_at backend_id behavior_id agent_did call_kind
                    prompt_tokens completion_tokens cached_input_tokens context_accounting_json
                }}
                AgentToolCall(
                    filter: {{ request_id: {{ _in: [{request_list}] }} }},
                    order: {{ started_at: ASC }}, limit: {limit}
                ) {{
                    request_id session_id message_sequence tool_name tool_call_id status
                    lifecycle_state started_at completed_at selected_service_id
                    selected_tool_name tool_failure_class latency_ms
                }}
            }}"#
        ))
        .await?;
    let mut sessions = graphql_rows_from_response(&response, "AgentSession")
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<TimelineSessionRow>, _>>()
        .context("decoding live AgentSession activity")?;
    let mut inference_calls = graphql_rows_from_response(&response, "InferenceCall")
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<TimelineInferenceCallRow>, _>>()
        .context("decoding live InferenceCall activity")?;
    let mut tool_calls = graphql_rows_from_response(&response, "AgentToolCall")
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<TimelineToolCallRow>, _>>()
        .context("decoding live AgentToolCall activity")?;
    let truncated = sessions.len() > MAX_RUN_ACTIVITY_ROWS
        || inference_calls.len() > MAX_RUN_ACTIVITY_ROWS
        || tool_calls.len() > MAX_RUN_ACTIVITY_ROWS;
    sessions.truncate(MAX_RUN_ACTIVITY_ROWS);
    inference_calls.truncate(MAX_RUN_ACTIVITY_ROWS);
    tool_calls.truncate(MAX_RUN_ACTIVITY_ROWS);
    Ok(RunActivityRows {
        sessions,
        inference_calls,
        tool_calls,
        truncated,
    })
}

async fn load_timeline_descendant_edges(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<crate::DescendantEdge>> {
    let descendant_root =
        resolve_descendant_root_request_id(DescendantGraphAccess::Config(access), request_id)
            .await?;
    let mut after = None;
    let mut edges = Vec::new();
    loop {
        let page = resolve_descendant_graph(
            DescendantGraphAccess::Config(access),
            &DescendantQuery {
                after: after.clone(),
                limit: MAX_DESCENDANT_PAGE_LIMIT,
                ..DescendantQuery::all(&descendant_root)
            },
        )
        .await?;
        edges.extend(page.edges);
        if !page.has_more {
            break;
        }
        after = page.next_cursor;
    }
    Ok(edges)
}

pub async fn load_run_timeline_rows(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<RunTimelineRows> {
    let request = load_timeline_request_by_id(access, request_id).await?;
    let root_session_id = request.session_id.clone();

    let mut requests = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_requests_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    ensure_unique_timeline_request_ids(&requests)?;
    merge_timeline_request(&mut requests, request.clone())?;
    let request_doc_id = request
        .doc_id
        .as_deref()
        .context("timeline root AgentRequest has no _docID")?;
    for child in load_timeline_child_requests(access, request_doc_id).await? {
        if child.caused_by_parent_request_doc_id.as_deref() != Some(request_doc_id) {
            continue;
        }
        if child.caused_by_parent_request_id.as_deref() != Some(request.request_id.as_str()) {
            anyhow::bail!(
                "child AgentRequest {} physical parent is {}, but logical parent is {:?}",
                child.request_id,
                request_doc_id,
                child.caused_by_parent_request_id
            );
        }
        merge_timeline_request(&mut requests, child)?;
    }

    let rendered_request_refs = match request.doc_id.as_deref() {
        Some(request_doc_id) => load_timeline_rendered_request_refs(access, request_doc_id).await?,
        None => Vec::new(),
    };

    let session_ids = timeline_session_ids(&requests);
    let mut messages = Vec::new();
    let mut tool_calls = Vec::new();
    let mut responses = Vec::new();
    let mut compactions = Vec::new();
    for session_id in &session_ids {
        messages.extend(load_timeline_messages_for_session(access, session_id).await?);
        tool_calls.extend(load_timeline_tool_calls_for_session(access, session_id).await?);
        responses.extend(load_timeline_responses_for_session(access, session_id).await?);
        compactions.extend(load_timeline_compactions_for_session(access, session_id).await?);
    }
    // Goal transitions describe the root run's session-scoped objective. Child
    // sessions have independent goals and are not projected into this timeline.
    let goal_versions = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_goal_versions_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    if session_ids.is_empty() || root_session_id.is_none() {
        responses.extend(load_timeline_responses_for_request(access, request_doc_id).await?);
    }
    let mut inference_calls = Vec::new();
    let mut provider_context_reductions = Vec::new();
    for request_doc_id in timeline_request_doc_ids(&requests)? {
        inference_calls
            .extend(load_timeline_inference_calls_for_request(access, &request_doc_id).await?);
        provider_context_reductions.extend(
            load_timeline_provider_context_reductions_for_request(access, &request_doc_id).await?,
        );
    }
    let mut rendered_requests = Vec::new();
    for session_id in &session_ids {
        rendered_requests
            .extend(load_timeline_rendered_requests_for_session(access, session_id).await?);
    }
    if session_ids.is_empty() || root_session_id.is_none() {
        rendered_requests.extend(
            load_timeline_rendered_requests_for_request(access, &request.request_id).await?,
        );
    }

    let request_bindings = timeline_request_bindings(&requests, &request)?;
    // Direct children may have their own sessions.  Session-wide queries also
    // return rows for grandchildren (and later continuation requests) that are
    // not part of this root + direct-child timeline.  Ignore those unrelated
    // rows. Logical-only rows are legacy/unverified and are quarantined from
    // provenance-bearing projections; physical-only or mismatched in-scope
    // claims still fail closed below.
    let in_scope_ids = request_bindings
        .values()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let legacy_logical_only_rows = messages
        .iter()
        .filter(|row| {
            nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                && nonempty(row.request_doc_id.as_deref()).is_none()
        })
        .count()
        + tool_calls
            .iter()
            .filter(|row| {
                nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + responses
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + inference_calls
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + compactions
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count()
        + provider_context_reductions
            .iter()
            .filter(|row| {
                in_scope_ids.contains(row.request_id.as_str())
                    && nonempty(Some(row.request_doc_id.as_str())).is_none()
            })
            .count()
        + rendered_requests
            .iter()
            .filter(|row| {
                nonempty(row.request_id.as_deref()).is_some_and(|id| in_scope_ids.contains(&id))
                    && nonempty(row.request_doc_id.as_deref()).is_none()
            })
            .count();
    if legacy_logical_only_rows > 0 {
        tracing::warn!(
            request_id = %request.request_id,
            legacy_logical_only_rows,
            "timeline omitted rows without physical AgentRequest provenance",
        );
    }
    messages.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    tool_calls.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    responses.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    inference_calls.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    compactions.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            row.request_doc_id.as_deref(),
        )
    });
    provider_context_reductions.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            Some(row.request_id.as_str()),
            Some(row.request_doc_id.as_str()),
        )
    });
    rendered_requests.retain(|row| {
        request_scoped_row_is_in_timeline(
            &request_bindings,
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )
    });
    validate_request_scoped_rows(
        &request_bindings,
        &messages,
        &tool_calls,
        &responses,
        &inference_calls,
        &compactions,
        &provider_context_reductions,
        &rendered_requests,
    )?;
    validate_child_tool_bridges(&request, &requests, &tool_calls)?;

    let mut tool_approvals = Vec::new();
    for tool_call_doc_id in tool_calls
        .iter()
        .filter_map(|tool_call| nonempty(tool_call.doc_id.as_deref()))
    {
        tool_approvals
            .extend(load_timeline_tool_approvals_for_call(access, tool_call_doc_id).await?);
    }
    validate_tool_approval_bindings(&tool_calls, &tool_approvals)?;

    let session = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_session(access, session_id).await?,
        None => None,
    };
    let conversation = match root_session_id.as_deref() {
        Some(session_id) => load_timeline_conversation(access, session_id).await?,
        None => None,
    };

    Ok(RunTimelineRows {
        request,
        session,
        conversation,
        requests,
        messages,
        tool_calls,
        tool_approvals,
        goal_versions,
        inference_calls,
        compactions,
        provider_context_reductions,
        responses,
        rendered_requests,
        rendered_request_refs,
    })
}

mod context_loaders;
mod event_loaders;
mod goal_history;
mod query_helpers;
mod request_loaders;
mod validation;

use context_loaders::{load_timeline_conversation, load_timeline_session};
use event_loaders::{
    load_timeline_compactions_for_session, load_timeline_inference_calls_for_request,
    load_timeline_messages_for_session, load_timeline_provider_context_reductions_for_request,
    load_timeline_rendered_request_refs, load_timeline_rendered_requests_for_request,
    load_timeline_rendered_requests_for_session, load_timeline_responses_for_request,
    load_timeline_responses_for_session, load_timeline_tool_approvals_for_call,
    load_timeline_tool_calls_for_session,
};
use goal_history::load_timeline_goal_versions_for_session;
use query_helpers::load_rows;
use request_loaders::{
    load_timeline_child_requests, load_timeline_request_by_id, load_timeline_requests_for_session,
};
use validation::{
    ensure_unique_timeline_request_ids, merge_timeline_request, nonempty,
    request_scoped_row_is_in_timeline, timeline_request_bindings, timeline_request_doc_ids,
    timeline_session_ids, validate_child_tool_bridges, validate_request_scoped_rows,
    validate_tool_approval_bindings,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn created_doc_id(response: &defra_node::QueryResponse, field: &str) -> String {
        let alternate = field
            .strip_prefix("create_")
            .map(|collection| format!("add_{collection}"));
        let created = response
            .data
            .as_ref()
            .and_then(|data| {
                data.get(field)
                    .or_else(|| alternate.as_deref().and_then(|field| data.get(field)))
            })
            .expect("create response field");
        created
            .as_array()
            .and_then(|rows| rows.first())
            .unwrap_or(created)
            .get("_docID")
            .and_then(Value::as_str)
            .expect("created _docID")
            .to_string()
    }

    #[tokio::test]
    async fn activity_rows_reuse_prompt_free_timeline_vocabulary() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        let response = node
            .execute(
                r#"mutation {
                    create_AgentSession(input: {
                        session_id: "activity-session"
                        agent_name: "agent"
                        agent_did: "did:test:agent"
                        behavior_id: "review"
                        started: "2026-08-26T00:00:00Z"
                        status: "active"
                    }) { _docID }
                    create_InferenceCall(input: {
                        call_id: "activity-call"
                        request_id: "activity-request"
                        call_seq: 1
                        attempt: 1
                        call_kind: "inference"
                        call_state: "completed"
                        prompt_tokens: 120
                        completion_tokens: 30
                        cached_input_tokens: 64
                        context_accounting_json: "{\"estimated_input_tokens\":144}"
                    }) { _docID }
                    create_AgentToolCall(input: {
                        tool_call_key: "activity-tool-key"
                        request_id: "activity-request"
                        session_id: "activity-session"
                        agent_did: "did:test:agent"
                        tool_name: "read_file"
                        tool_call_id: "activity-tool"
                        status: "completed"
                        lifecycle_state: "completed"
                        selected_service_id: "activity-service"
                        selected_tool_name: "activity-selected-tool"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed activity rows: {:?}",
            response.errors
        );

        let rows = load_run_activity_rows(
            &ConfigAccess::Local(node.clone()),
            &["activity-request".to_owned()],
            &["activity-session".to_owned()],
        )
        .await
        .unwrap();
        assert_eq!(rows.sessions[0].status.as_deref(), Some("active"));
        assert_eq!(rows.inference_calls[0].prompt_tokens, Some(120));
        assert_eq!(
            rows.inference_calls[0].context_accounting_json.as_deref(),
            Some(r#"{"estimated_input_tokens":144}"#)
        );
        assert_eq!(rows.tool_calls[0].tool_name, "read_file");
        assert_eq!(
            rows.tool_calls[0].selected_service_id.as_deref(),
            Some("activity-service")
        );
        assert_eq!(
            rows.tool_calls[0].selected_tool_name.as_deref(),
            Some("activity-selected-tool")
        );
        assert!(rows.tool_calls[0].args.is_empty());
        assert!(rows.tool_calls[0].result.is_empty());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn fetches_approvals_and_complete_inference_provenance() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let response = node
            .execute(
                r#"mutation {
                    create_AgentSession(input: {
                        session_id: "session-timeline"
                        agent_name: "agent"
                        agent_did: "did:test:agent"
                        behavior_id: "general"
                        started: "2026-08-14T12:00:00Z"
                        status: "active"
                    }) { _docID }
                    create_AgentRequest(input: {
                        request_id: "request-timeline"
                        agent_did: "did:test:agent"
                        behavior_id: "general"
                        session_id: "session-timeline"
                        content: "run"
                        lifecycle_state: "completed"
                        created_at: "2026-08-14T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed request: {:?}",
            response.errors
        );
        let request_doc_id = created_doc_id(&response, "create_AgentRequest");

        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentToolCall(input: {{
                        tool_call_key: "session-timeline:tool-1"
                        request_id: "request-timeline"
                        request_doc_id: "{request_doc_id}"
                        session_id: "session-timeline"
                        agent_did: "did:test:agent"
                        message_sequence: 1
                        tool_name: "call_tool"
                        tool_call_id: "tool-1"
                        args: "{{}}"
                        result: "ok"
                        status: "completed"
                        lifecycle_state: "completed"
                        started_at: "2026-08-14T12:00:02Z"
                        deadline_at: "2026-08-14T12:05:00Z"
                        completed_at: "2026-08-14T12:00:04Z"
                        selected_service_id: "metrics-prod"
                        selected_tool_name: "query_metrics"
                    }}) {{ _docID }}
                    create_InferenceCall(input: {{
                        call_id: "inference-1"
                        runtime_instance_id: "runtime-a"
                        request_id: "request-timeline"
                        request_doc_id: "{request_doc_id}"
                        call_seq: 1
                        backend_id: "backend-a"
                        behavior_id: "general"
                        agent_did: "did:test:agent"
                        call_kind: "inference"
                        attempt: 1
                        call_state: "completed"
                        queued_at: "2026-08-14T12:00:01Z"
                        priority: 7
                        queue_depth_at_enqueue: 3
                        controller_generation: 11
                        backend_config_fingerprint: "InferenceBackend synthetic-inline-secret"
                        prompt_tokens: 10
                        completion_tokens: 5
                        cached_input_tokens: 2
                    }}) {{ _docID }}
                }}"#,
            ))
            .await;
        assert!(!response.has_errors(), "seed calls: {:?}", response.errors);
        let tool_call_doc_id = created_doc_id(&response, "create_AgentToolCall");

        let response = node
            .execute(&format!(
                r#"mutation {{
                    create_AgentToolApproval(input: {{
                        approval_id: "approval-1"
                        tool_call_doc_id: "{tool_call_doc_id}"
                        tool_call_id: "tool-1"
                        request_id: "request-timeline"
                        agent_did: "did:test:agent"
                        decision: "approved"
                        approver_did: "did:test:operator"
                        reason: "reviewed"
                        created_at: "2026-08-14T12:00:03Z"
                    }}) {{ _docID }}
                }}"#,
            ))
            .await;
        assert!(
            !response.has_errors(),
            "seed approval: {:?}",
            response.errors
        );

        let access = ConfigAccess::Local(node.clone());
        let timeline = load_run_timeline(&access, "request-timeline")
            .await
            .expect("load timeline");
        assert!(timeline.events.iter().any(|event| matches!(
            event,
            crate::run_timeline::RunTimelineEvent::ToolApproval(approval)
                if approval.approval_id == "approval-1"
                    && approval.tool_call_id == "tool-1"
        )));
        let inference = timeline.events.iter().find_map(|event| match event {
            crate::run_timeline::RunTimelineEvent::InferenceCall(inference) => Some(inference),
            _ => None,
        });
        let inference = inference.expect("inference event");
        assert_eq!(inference.runtime_instance_id.as_deref(), Some("runtime-a"));
        assert_eq!(inference.priority, Some(7));
        assert_eq!(inference.queue_depth_at_enqueue, Some(3));
        assert_eq!(inference.controller_generation, Some(11));
        assert_eq!(inference.backend_config_fingerprint, None);
        assert!(!serde_json::to_string(&timeline)
            .unwrap()
            .contains("synthetic-inline-secret"));

        node.shutdown().await;
    }

    #[tokio::test]
    async fn fetches_native_goal_history_without_projecting_usage_only_commits() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let response = node
            .execute(
                r#"mutation {
                    create_AgentSession(input: {
                        session_id: "session-goal-history"
                        agent_name: "agent"
                        agent_did: "did:test:agent"
                        behavior_id: "general"
                        started: "2026-08-14T12:00:00Z"
                        status: "active"
                    }) { _docID }
                    create_AgentRequest(input: {
                        request_id: "request-goal-history"
                        agent_did: "did:test:agent"
                        behavior_id: "general"
                        session_id: "session-goal-history"
                        content: "ship the timeline"
                        lifecycle_state: "completed"
                        created_at: "2026-08-14T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed goal run: {:?}",
            response.errors
        );

        let active = crate::goal::set_goal(
            node.as_ref(),
            "did:test:agent",
            "session-goal-history",
            Some("ship the timeline"),
            Some(crate::goal::GoalStatus::Active),
            None,
        )
        .await
        .expect("create goal");
        crate::goal::refresh_goal_usage(node.as_ref(), &active)
            .await
            .expect("refresh usage");
        crate::goal::set_goal(
            node.as_ref(),
            "did:test:agent",
            "session-goal-history",
            None,
            Some(crate::goal::GoalStatus::Paused),
            None,
        )
        .await
        .expect("pause goal");

        let access = ConfigAccess::Local(node.clone());
        let timeline = load_run_timeline(&access, "request-goal-history")
            .await
            .expect("load timeline");
        let transitions = timeline
            .events
            .iter()
            .filter_map(|event| match event {
                crate::run_timeline::RunTimelineEvent::GoalTransition(event) => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(transitions.len(), 2, "usage-only commit must be omitted");
        assert_eq!(transitions[0].state.status, "active");
        assert_eq!(transitions[1].state.status, "paused");
        assert!(!transitions[0].commit_cid.is_empty());
        assert!(!transitions[1].commit_cid.is_empty());
        assert_eq!(transitions[1].parents.len(), 1);
        assert_eq!(transitions[1].parents[0].state.status, "active");
        assert_ne!(
            transitions[1].parents[0].commit_cid, transitions[0].commit_cid,
            "the semantic event must preserve its immediate native usage-only parent",
        );

        node.shutdown().await;
    }
}
