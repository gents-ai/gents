use super::*;

pub(super) async fn load_timeline_messages_for_session(
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

pub(super) async fn load_timeline_tool_calls_for_session(
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

pub(super) async fn load_timeline_responses_for_session(
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

pub(super) async fn load_timeline_responses_for_request(
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

pub(super) async fn load_timeline_inference_calls_for_request(
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
pub(super) async fn load_timeline_rendered_requests_for_session(
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
                provenance_json
                created_at
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}

pub(super) async fn load_timeline_rendered_requests_for_request(
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
                provenance_json
                created_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows(access, "RenderedRequest", &query).await
}

pub(super) async fn load_timeline_compactions_for_session(
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

pub(super) async fn load_timeline_rendered_request_refs(
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
