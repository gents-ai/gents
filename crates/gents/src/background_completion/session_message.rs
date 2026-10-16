use super::*;

#[derive(Debug, Deserialize)]
struct SessionMessageRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
}

const SESSION_MESSAGE_ROW_FIELDS: &str = "_docID session_id node_did requester_did";

fn running_session_message_filter(local_did: &str) -> String {
    format!(
        r#"node_did: {{ _eq: "{}" }}, lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }}, spawned_by_tool_call_doc_id: {{ _eq: null }}, tool_name: {{ _in: ["{}", "{}"] }}"#,
        escape_graphql_string(local_did),
        crate::toolset::AGENT_NEW_TOOL_NAME,
        crate::toolset::AGENT_MESSAGE_TOOL_NAME,
    )
}

pub(crate) async fn settle_running_session_message_rows(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ {} }}) {{ {SESSION_MESSAGE_ROW_FIELDS} }} }}"#,
        running_session_message_filter(local_did)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &query,
        "load running session-message rows",
    )
    .await?;
    let rows = crate::graphql::rows::<SessionMessageRow>(&response, "AgentToolCall")?;
    let mut settled = 0;
    for row in rows {
        match settle_row(node, &row).await {
            Ok(true) => settled += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                tool_call_doc_id = %row.doc_id,
                error = %format!("{error:#}"),
                "session-message row settlement failed; will retry"
            ),
        }
    }
    Ok(settled)
}

pub(super) async fn settle_rows_after_request_update(
    node: &Arc<EmbeddedNode>,
    local_did: &str,
    request_doc_id: &str,
) -> Result<usize> {
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
            _docID request_id lifecycle_state caused_by_parent_tool_call_doc_id
        }} }}"#,
        escape_graphql_string(request_doc_id)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &query,
        "load updated request for session-message settlement",
    )
    .await?;
    let Some(request) = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
    else {
        return Ok(0);
    };
    if !request
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
    {
        return Ok(0);
    }
    let Some(tool_doc_id) = request.caused_by_parent_tool_call_doc_id else {
        return Ok(0);
    };
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }}, {} }}, limit: 1) {{ {SESSION_MESSAGE_ROW_FIELDS} }} }}"#,
        escape_graphql_string(&tool_doc_id),
        running_session_message_filter(local_did)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node.as_ref(),
        &query,
        "load the session-message row a terminal request names",
    )
    .await?;
    let mut settled = 0;
    for row in crate::graphql::rows::<SessionMessageRow>(&response, "AgentToolCall")? {
        settled += usize::from(settle_row(node, &row).await?);
    }
    Ok(settled)
}

async fn settle_row(node: &Arc<EmbeddedNode>, row: &SessionMessageRow) -> Result<bool> {
    let Some(mut lifecycle) = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &row.doc_id,
        &row.node_did,
        &row.session_id,
        row.requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(false);
    };
    settle_session_message_row(node, &mut lifecycle).await
}

pub(crate) async fn settle_session_message_row(
    node: &Arc<EmbeddedNode>,
    lifecycle: &mut ToolCallLifecycle,
) -> Result<bool> {
    if !lifecycle.is_running() || !lifecycle.is_session_message() {
        return Ok(false);
    }
    let doc_id = lifecycle
        .doc_id()
        .context("session-message row lacks physical identity")?
        .to_owned();
    let calling_request_id =
        crate::session_message::calling_request_id(node.as_ref(), lifecycle).await?;
    let caused = match crate::session_message::observe_caused_request(node, lifecycle).await? {
        crate::session_message::CausedObservation::Bound(caused) => caused,
        crate::session_message::CausedObservation::NotVisible => return Ok(false),
        crate::session_message::CausedObservation::Unbound(reason) => {
            const REASON: &str = "caused_request_unbound";
            if !lifecycle
                .fail_owned_with_completion_reason(reason, FailureClass::External, REASON)
                .await?
            {
                return Ok(false);
            }
            append_background_tool_completion(
                node,
                lifecycle.session_id(),
                &calling_request_id,
                &doc_id,
                lifecycle.tool_name(),
                "failed",
                reason,
                Some(REASON),
                crate::lifecycle::RequestHopCause::Continuation,
            )
            .await?;
            return Ok(true);
        }
    };
    let caused_doc_id = caused
        .doc_id
        .as_deref()
        .context("caused request lacks physical identity")?;
    let Some(terminal) =
        crate::background_tools::load_caused_request_terminal(node, caused_doc_id).await?
    else {
        return Ok(false);
    };
    if !lifecycle.settle_session_message(&terminal).await? {
        return Ok(false);
    }
    append_background_tool_completion(
        node,
        lifecycle.session_id(),
        &calling_request_id,
        &doc_id,
        lifecycle.tool_name(),
        terminal.notification_status(),
        terminal.output(),
        terminal.completion_reason(),
        crate::lifecycle::RequestHopCause::Return,
    )
    .await?;
    Ok(true)
}
