use super::*;

pub(super) fn session_request_create_mutation(
    parent: &AgentRequest,
    behavior_id: &str,
    content: &str,
    execution_origin: ExecutionOrigin,
    metadata: &str,
    request_id: &str,
    created_at: &str,
    request_only_control: bool,
) -> Result<String> {
    let parent_linkage_fields = if request_only_control {
        request_only_parent_linkage_graphql_fields(parent)?
    } else {
        parent_linkage_graphql_fields(parent)?
    };
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(&parent.agent_did);
    let requester_did_field = session::requester_did_create_field(parent.requester_did.as_deref());
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_session_id = escape_graphql_string(&parent.session_id);
    let escaped_content = escape_graphql_string(content);
    let escaped_metadata = escape_graphql_string(metadata);
    let escaped_created_at = escape_graphql_string(created_at);
    let execution_origin = execution_origin.as_str();
    let inherited_trigger_context = crate::lifecycle::inherited_trigger_context_graphql_fields(
        parent.caused_by_correlation.as_deref(),
        parent.caused_by_trigger_context.as_deref(),
    )?;
    Ok(format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                {requester_did_field}
                behavior_id: "{escaped_behavior_id}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "{escaped_content}",
                metadata: "{escaped_metadata}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "{execution_origin}",
                failure_reason: "",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries},
                subagent_depth: {subagent_depth}{parent_linkage_fields}{inherited_trigger_context}
            }}) {{ _docID }}
        }}"#,
        max_retries = DEFAULT_REQUEST_MAX_RETRIES,
        subagent_depth = parent.subagent_depth,
    ))
}
