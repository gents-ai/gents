//! Row fetch for [`crate::run_timeline`]: loads the persisted documents a
//! request's timeline is reconstructed from, over either transport
//! ([`ConfigAccess::Graphql`] or [`ConfigAccess::Local`]). Lifted from the
//! CLI `trace` command so the desktop client shares one fetcher.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::run_timeline::{
    build_run_timeline, RunTimeline, RunTimelineRows, TimelineCompactionRow,
    TimelineConversationRow, TimelineInferenceCallRow, TimelineMessageRow,
    TimelineRenderedRequestRef, TimelineRenderedRequestRow, TimelineRequestRow,
    TimelineResponseRow, TimelineSessionRow, TimelineToolCallRow,
};
use gents_protocol::graphql::graphql_rows_from_response;

pub async fn load_run_timeline(access: &ConfigAccess, request_id: &str) -> Result<RunTimeline> {
    Ok(build_run_timeline(
        load_run_timeline_rows(access, request_id).await?,
    ))
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
    if session_ids.is_empty() || root_session_id.is_none() {
        responses.extend(load_timeline_responses_for_request(access, request_doc_id).await?);
    }
    let mut inference_calls = Vec::new();
    for request_doc_id in timeline_request_doc_ids(&requests)? {
        inference_calls
            .extend(load_timeline_inference_calls_for_request(access, &request_doc_id).await?);
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
    // rows, while retaining any row that claims an in-scope logical or physical
    // request so forged/partial bindings still fail closed below.
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
    validate_request_scoped_rows(
        &request_bindings,
        &messages,
        &tool_calls,
        &responses,
        &inference_calls,
        &compactions,
    )?;
    validate_child_tool_bridges(&request, &requests, &tool_calls)?;

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
        inference_calls,
        compactions,
        responses,
        rendered_requests,
        rendered_request_refs,
    })
}

async fn load_timeline_request_by_id(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<TimelineRequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: DESC }},
                limit: 2
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_trigger_id
                caused_by_trigger_kind
                caused_by_source_doc_id
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                execution_origin
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    let mut rows = load_rows::<TimelineRequestRow>(access, "AgentRequest", &query).await?;
    match rows.len() {
        0 => Err(anyhow::anyhow!("request {request_id} not found")),
        1 => Ok(rows.remove(0)),
        count => anyhow::bail!(
            "request_id {request_id} is ambiguous across {count} AgentRequest documents"
        ),
    }
}

async fn load_timeline_requests_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_trigger_id
                caused_by_trigger_kind
                caused_by_source_doc_id
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                execution_origin
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_timeline_child_requests(
    access: &ConfigAccess,
    parent_request_doc_id: &str,
) -> Result<Vec<TimelineRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_doc_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_trigger_id
                caused_by_trigger_kind
                caused_by_source_doc_id
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
                execution_origin
            }}
        }}"#,
        escape_graphql_string(parent_request_doc_id)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_timeline_messages_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineMessageRow>> {
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                _docID
                session_id
                request_id
                request_doc_id
                sequence
                role
                content
                reasoning
                timestamp
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentMessage", &query).await
}

async fn load_timeline_tool_calls_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineToolCallRow>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                request_id
                request_doc_id
                session_id
                message_sequence
                tool_name
                tool_call_id
                args
                result
                status
                lifecycle_state
                started_at
                deadline_at
                completed_at
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                denied_argv
                denied_command
                denied_argument
                denied_subcommand
                denied_prefix
                policy_mode
                policy_network
                latency_ms
                await_mode
                cancel_policy
                cancel_cause
                child_request_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentToolCall", &query).await
}

async fn load_timeline_responses_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineResponseRow>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                request_doc_id
                agent_did
                behavior_id
                session_id
                content
                reasoning
                status
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                created_at
                completed_at
                interrupted_at
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_timeline_responses_for_request(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineResponseRow>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_id
                request_doc_id
                agent_did
                behavior_id
                session_id
                content
                reasoning
                status
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                created_at
                completed_at
                interrupted_at
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_timeline_inference_calls_for_request(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineInferenceCallRow>> {
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ call_seq: ASC }}
            ) {{
                _docID
                call_id
                request_id
                request_doc_id
                call_seq
                attempt
                call_state
                failure_reason
                queued_at
                started_at
                ended_at
                backend_id
                call_kind
                prompt_tokens
                completion_tokens
                cached_input_tokens
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    load_rows(access, "InferenceCall", &query).await
}

/// The rendered-request capture rows for one session, metadata columns only.
/// `request_json` is deliberately never selected here — see
/// `TimelineRenderedRequestRow`. Pre-#1059 databases have no `RenderedRequest`
/// collection; `load_rows` reports that as an empty section, not a failed
/// timeline.
async fn load_timeline_rendered_requests_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineRenderedRequestRow>> {
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                capture_key
                request_doc_id
                request_id
                session_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                prompt_hash
                tools_hash
                provenance_json
                created_at
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}

async fn load_timeline_rendered_requests_for_request(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<TimelineRenderedRequestRow>> {
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                capture_key
                request_doc_id
                request_id
                session_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                prompt_hash
                tools_hash
                provenance_json
                created_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}

async fn load_timeline_compactions_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineCompactionRow>> {
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                _docID
                compaction_key
                request_id
                request_doc_id
                session_id
                sequence
                summary
                messages_compacted
                original_tokens
                compacted_tokens
                created_at
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "CompactionEntry", &query).await
}

async fn load_timeline_rendered_request_refs(
    access: &ConfigAccess,
    request_doc_id: &str,
) -> Result<Vec<TimelineRenderedRequestRef>> {
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                request_doc_id
                request_commit_cid
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}

async fn load_timeline_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Option<TimelineSessionRow>> {
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                _docID
                session_id
                agent_name
                behavior_id
                started
                ended
                status
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    Ok(
        load_rows::<TimelineSessionRow>(access, "AgentSession", &query)
            .await?
            .into_iter()
            .next(),
    )
}

async fn load_timeline_conversation(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Option<TimelineConversationRow>> {
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                _docID
                session_id
                agent_name
                agent_did
                behavior_id
                title
                title_source
                preview_text
                status
                created_at
                updated_at
                latest_request_id
                forked_from_session_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    Ok(
        load_rows::<TimelineConversationRow>(access, "AgentConversation", &query)
            .await?
            .into_iter()
            .next(),
    )
}

fn merge_timeline_request(
    rows: &mut Vec<TimelineRequestRow>,
    request: TimelineRequestRow,
) -> Result<()> {
    if let Some(existing) = rows.iter().find(|row| row.request_id == request.request_id) {
        if existing.doc_id != request.doc_id {
            anyhow::bail!(
                "request_id {} is ambiguous across AgentRequest documents {:?} and {:?}",
                request.request_id,
                existing.doc_id,
                request.doc_id
            );
        }
    } else {
        rows.push(request);
    }
    Ok(())
}

fn ensure_unique_timeline_request_ids(rows: &[TimelineRequestRow]) -> Result<()> {
    let mut seen = std::collections::BTreeMap::<&str, &Option<String>>::new();
    for request in rows {
        if let Some(existing_doc_id) = seen.insert(&request.request_id, &request.doc_id) {
            anyhow::bail!(
                "request_id {} is ambiguous across AgentRequest documents {:?} and {:?}",
                request.request_id,
                existing_doc_id,
                request.doc_id
            );
        }
    }
    Ok(())
}

fn timeline_session_ids(requests: &[TimelineRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            request
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn timeline_request_doc_ids(requests: &[TimelineRequestRow]) -> Result<Vec<String>> {
    let doc_ids = requests
        .iter()
        .map(|request| {
            required_lineage_value(
                "AgentRequest",
                &request.request_id,
                "_docID",
                request.doc_id.as_deref(),
            )
            .map(ToOwned::to_owned)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    Ok(doc_ids.into_iter().collect())
}

fn timeline_request_bindings(
    requests: &[TimelineRequestRow],
    root: &TimelineRequestRow,
) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for request in requests.iter().chain(std::iter::once(root)) {
        let doc_id = required_lineage_value(
            "AgentRequest",
            &request.request_id,
            "_docID",
            request.doc_id.as_deref(),
        )?;
        match bindings.insert(doc_id.to_string(), request.request_id.clone()) {
            Some(existing) if existing != request.request_id => anyhow::bail!(
                "AgentRequest _docID {doc_id} is bound to both {existing} and {}",
                request.request_id
            ),
            _ => {}
        }
    }
    Ok(bindings)
}

fn validate_request_scoped_rows(
    bindings: &BTreeMap<String, String>,
    messages: &[TimelineMessageRow],
    tool_calls: &[TimelineToolCallRow],
    responses: &[TimelineResponseRow],
    inference_calls: &[TimelineInferenceCallRow],
    compactions: &[TimelineCompactionRow],
) -> Result<()> {
    for row in messages {
        validate_optional_request_binding(
            bindings,
            "AgentMessage",
            row.doc_id.as_deref().unwrap_or("<unknown>"),
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in tool_calls {
        validate_optional_request_binding(
            bindings,
            "AgentToolCall",
            row.doc_id.as_deref().unwrap_or(&row.tool_call_id),
            row.request_id.as_deref(),
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in responses {
        validate_required_request_binding(
            bindings,
            "AgentResponse",
            row.doc_id.as_deref().unwrap_or(&row.request_id),
            &row.request_id,
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in inference_calls {
        validate_required_request_binding(
            bindings,
            "InferenceCall",
            row.doc_id.as_deref().unwrap_or(&row.call_id),
            &row.request_id,
            row.request_doc_id.as_deref(),
        )?;
    }
    for row in compactions {
        // A fork copies compaction state so the child session can preserve its
        // prompt-reduction boundary, but it does not copy the parent requests.
        // Such imported session context is intentionally unbound; any bound
        // compaction must still resolve as an exact logical/physical pair.
        validate_optional_request_binding(
            bindings,
            "CompactionEntry",
            row.doc_id.as_deref().unwrap_or(&row.compaction_key),
            Some(&row.request_id),
            row.request_doc_id.as_deref(),
        )?;
    }
    Ok(())
}

fn request_scoped_row_is_in_timeline(
    bindings: &BTreeMap<String, String>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
) -> bool {
    let request_id = nonempty(request_id);
    let request_doc_id = nonempty(request_doc_id);
    if request_id.is_none() && request_doc_id.is_none() {
        return true;
    }
    request_doc_id.is_some_and(|doc_id| bindings.contains_key(doc_id))
        || request_id.is_some_and(|request_id| bindings.values().any(|id| id == request_id))
}

fn validate_optional_request_binding(
    bindings: &BTreeMap<String, String>,
    collection: &str,
    label: &str,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
) -> Result<()> {
    match (nonempty(request_id), nonempty(request_doc_id)) {
        (None, None) => Ok(()),
        (Some(request_id), Some(request_doc_id)) => {
            validate_binding_pair(bindings, collection, label, request_id, request_doc_id)
        }
        _ => anyhow::bail!(
            "{collection} {label} has incomplete request lineage: request_id={request_id:?} request_doc_id={request_doc_id:?}"
        ),
    }
}

fn validate_required_request_binding(
    bindings: &BTreeMap<String, String>,
    collection: &str,
    label: &str,
    request_id: &str,
    request_doc_id: Option<&str>,
) -> Result<()> {
    let request_id = required_lineage_value(collection, label, "request_id", Some(request_id))?;
    let request_doc_id =
        required_lineage_value(collection, label, "request_doc_id", request_doc_id)?;
    validate_binding_pair(bindings, collection, label, request_id, request_doc_id)
}

fn validate_binding_pair(
    bindings: &BTreeMap<String, String>,
    collection: &str,
    label: &str,
    request_id: &str,
    request_doc_id: &str,
) -> Result<()> {
    match bindings.get(request_doc_id) {
        Some(expected) if expected == request_id => Ok(()),
        Some(expected) => anyhow::bail!(
            "{collection} {label} request_doc_id {request_doc_id} belongs to {expected}, not {request_id}"
        ),
        None => anyhow::bail!(
            "{collection} {label} points to AgentRequest {request_doc_id}, which is outside this timeline"
        ),
    }
}

fn validate_child_tool_bridges(
    root: &TimelineRequestRow,
    requests: &[TimelineRequestRow],
    tool_calls: &[TimelineToolCallRow],
) -> Result<()> {
    let root_doc_id = required_lineage_value(
        "AgentRequest",
        &root.request_id,
        "_docID",
        root.doc_id.as_deref(),
    )?;
    for child in requests.iter().filter(|request| {
        nonempty(request.caused_by_parent_request_doc_id.as_deref()) == Some(root_doc_id)
    }) {
        let tool_doc_id = nonempty(child.caused_by_parent_tool_call_doc_id.as_deref());
        let logical_tool_id = nonempty(child.caused_by_parent_tool_call_id.as_deref());
        let (tool_doc_id, logical_tool_id) = match (tool_doc_id, logical_tool_id) {
            (None, None) => continue,
            (Some(tool_doc_id), Some(logical_tool_id)) => (tool_doc_id, logical_tool_id),
            _ => anyhow::bail!(
                "child AgentRequest {} has incomplete parent tool lineage",
                child.request_id
            ),
        };
        let tool = tool_calls
            .iter()
            .find(|tool| nonempty(tool.doc_id.as_deref()) == Some(tool_doc_id))
            .with_context(|| {
                format!(
                    "child AgentRequest {} points to missing AgentToolCall {tool_doc_id}",
                    child.request_id
                )
            })?;
        if nonempty(tool.request_doc_id.as_deref()) != Some(root_doc_id)
            || nonempty(tool.request_id.as_deref()) != Some(root.request_id.as_str())
            || tool.tool_call_id != logical_tool_id
            || nonempty(tool.child_request_id.as_deref()) != Some(child.request_id.as_str())
        {
            anyhow::bail!(
                "child AgentRequest {} has a mismatched physical AgentToolCall bridge {}",
                child.request_id,
                tool_doc_id
            );
        }
    }
    Ok(())
}

fn required_lineage_value<'a>(
    collection: &str,
    label: &str,
    field: &str,
    value: Option<&'a str>,
) -> Result<&'a str> {
    nonempty(value).with_context(|| format!("{collection} {label} has no {field}"))
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

async fn load_rows<T>(access: &ConfigAccess, collection: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    rows_or_empty_if_collection_missing(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

async fn rows_or_empty_if_collection_missing(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let rows = match access.execute(query).await {
        Ok(response) => Ok(graphql_rows_from_response(&response, collection_name)),
        Err(error) => Err(error),
    };
    match rows {
        Ok(rows) => Ok(rows),
        Err(error)
            if {
                let message = error.to_string();
                message.contains(collection_name)
                    && (message.contains("collection not found")
                        || message.contains("Cannot query field"))
            } =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("doc-root".to_string(), "req-root".to_string()),
            ("doc-child".to_string(), "req-child".to_string()),
        ])
    }

    #[test]
    fn physical_request_edge_rejects_forged_logical_join() {
        let error = validate_required_request_binding(
            &bindings(),
            "AgentResponse",
            "response-1",
            "req-root",
            Some("doc-child"),
        )
        .expect_err("mismatched physical edge must fail closed");
        assert!(error.to_string().contains("belongs to req-child"));
    }

    #[test]
    fn genuinely_unbound_context_row_is_permitted_but_half_binding_is_not() {
        validate_optional_request_binding(
            &bindings(),
            "AgentMessage",
            "message-context",
            None,
            None,
        )
        .expect("unbound context message");
        let error = validate_optional_request_binding(
            &bindings(),
            "AgentMessage",
            "message-forged",
            Some("req-root"),
            None,
        )
        .expect_err("partial binding must fail closed");
        assert!(error.to_string().contains("incomplete request lineage"));
    }

    #[test]
    fn session_rows_for_nested_requests_are_out_of_scope_without_hiding_forged_root_edges() {
        let bindings = bindings();
        assert!(!request_scoped_row_is_in_timeline(
            &bindings,
            Some("req-grandchild"),
            Some("doc-grandchild")
        ));
        assert!(request_scoped_row_is_in_timeline(
            &bindings,
            Some("req-root"),
            Some("doc-grandchild")
        ));
        assert!(request_scoped_row_is_in_timeline(
            &bindings,
            Some("req-root"),
            None
        ));
        assert!(request_scoped_row_is_in_timeline(
            &bindings,
            None,
            Some("doc-root")
        ));
        assert!(!request_scoped_row_is_in_timeline(
            &bindings,
            Some("req-grandchild"),
            None
        ));
        assert!(request_scoped_row_is_in_timeline(&bindings, None, None));
    }

    #[test]
    fn child_bridge_requires_the_exact_parent_tool_document() {
        let root = TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            ..Default::default()
        };
        let child = TimelineRequestRow {
            doc_id: Some("doc-child".to_string()),
            request_id: "req-child".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            caused_by_parent_tool_call_id: Some("call-parent".to_string()),
            caused_by_parent_tool_call_doc_id: Some("doc-forged-tool".to_string()),
            ..Default::default()
        };
        let tool = TimelineToolCallRow {
            doc_id: Some("doc-real-tool".to_string()),
            request_id: Some("req-root".to_string()),
            request_doc_id: Some("doc-root".to_string()),
            tool_call_id: "call-parent".to_string(),
            child_request_id: Some("req-child".to_string()),
            ..Default::default()
        };

        let error = validate_child_tool_bridges(&root, &[root.clone(), child], &[tool])
            .expect_err("forged tool document edge must fail closed");
        assert!(error.to_string().contains("missing AgentToolCall"));
    }

    #[test]
    fn direct_child_without_tool_lineage_is_valid_but_half_bridge_is_rejected() {
        let root = TimelineRequestRow {
            doc_id: Some("doc-root".to_string()),
            request_id: "req-root".to_string(),
            ..Default::default()
        };
        let direct_child = TimelineRequestRow {
            doc_id: Some("doc-direct".to_string()),
            request_id: "req-direct".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            ..Default::default()
        };
        validate_child_tool_bridges(&root, &[root.clone(), direct_child], &[])
            .expect("direct parent lineage does not fabricate a tool delegation");

        let half_bridge = TimelineRequestRow {
            doc_id: Some("doc-half".to_string()),
            request_id: "req-half".to_string(),
            caused_by_parent_request_id: Some("req-root".to_string()),
            caused_by_parent_request_doc_id: Some("doc-root".to_string()),
            caused_by_parent_tool_call_id: Some("call-only".to_string()),
            ..Default::default()
        };
        let error = validate_child_tool_bridges(&root, &[root.clone(), half_bridge], &[])
            .expect_err("half tool bridge must fail closed");
        assert!(error.to_string().contains("incomplete parent tool lineage"));
    }
}
