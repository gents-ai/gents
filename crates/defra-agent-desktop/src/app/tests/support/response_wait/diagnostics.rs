pub(crate) fn describe_response_wait_state(
    request: Option<&defra_agent_protocol::row::AgentRequestRow>,
    response: Option<&defra_agent_protocol::row::AgentResponseRow>,
    prior_response_count: usize,
    current_response_count: usize,
) -> String {
    let request_summary = request.map_or_else(
        || "request=<missing>".to_string(),
        |row| {
            format!(
                "request={{status={}, lifecycle_state={}, agent_did={}, behavior_id={}, backend_id={}, execution_origin={}, failure_reason={}, claimed_at={}, deadline={}}}",
                optional_str(row.status.as_deref()),
                optional_str(row.lifecycle_state.as_deref()),
                optional_str(row.agent_did.as_deref()),
                optional_str(row.behavior_id.as_deref()),
                optional_str(row.backend_id.as_deref()),
                optional_str(row.execution_origin.as_deref()),
                optional_str(row.failure_reason.as_deref()),
                optional_str(row.claimed_at.as_deref()),
                optional_str(row.deadline.as_deref()),
            )
        },
    );
    let response_summary = response.map_or_else(
        || "response=<missing>".to_string(),
        |row| {
            format!(
                "response={{key={}, status={}, agent_did={}, behavior_id={}, error_message={}, content_len={}, progress_seq={}, completed_at={}}}",
                row.response_key,
                optional_str(row.status.as_deref()),
                optional_str(row.agent_did.as_deref()),
                optional_str(row.behavior_id.as_deref()),
                optional_str(row.error_message.as_deref()),
                row.content.as_deref().map(str::len).unwrap_or_default(),
                row.progress_seq.unwrap_or_default(),
                optional_str(row.completed_at.as_deref()),
            )
        },
    );
    format!(
        "{request_summary}; {response_summary}; responses_before_submit={prior_response_count}; responses_now={current_response_count}"
    )
}

pub(crate) fn optional_str(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("<empty>")
}
