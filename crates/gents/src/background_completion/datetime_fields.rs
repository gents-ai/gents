use super::*;

pub(super) async fn clear_cancel_pending_ack(node: &EmbeddedNode, doc_id: &str) -> Result<()> {
    let escaped = escape_graphql_string(doc_id);
    let datetime_fields =
        agent_tool_call_datetime_update_fragment(node, doc_id, &["stuck_since"]).await?;
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                input: {{
                    cancel_pending_remote_ack: false,
                    stuck_since: null
                    {datetime_fields}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "clear cancel_pending_remote_ack failed: {:?}",
            response.errors
        );
    }
    Ok(())
}

pub(super) async fn set_stuck_since(
    node: &EmbeddedNode,
    doc_id: &str,
    when: DateTime<Utc>,
) -> Result<()> {
    let escaped = escape_graphql_string(doc_id);
    let when = escape_graphql_string(&when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    let datetime_fields =
        agent_tool_call_datetime_update_fragment(node, doc_id, &["stuck_since"]).await?;
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                input: {{ stuck_since: "{when}"{datetime_fields} }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("set stuck_since failed: {:?}", response.errors);
    }
    Ok(())
}

pub(super) async fn agent_tool_call_datetime_update_fragment(
    node: &EmbeddedNode,
    doc_id: &str,
    omit: &[&str],
) -> Result<String> {
    let escaped = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                started_at
                deadline_at
                completed_at
                unclaimed_deadline_at
                cancel_cascade_intent_at
                stuck_since
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentToolCall DateTime fields failed: {:?}",
            response.errors
        );
    }
    let row = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| serde_json::from_value::<Vec<AgentToolCallDateTimeRow>>(v.clone()).ok())
        .and_then(|mut rows| rows.pop())
        .unwrap_or_default();

    let mut fields = Vec::new();
    push_datetime_field(&mut fields, omit, "started_at", row.started_at.as_deref());
    push_datetime_field(&mut fields, omit, "deadline_at", row.deadline_at.as_deref());
    push_datetime_field(
        &mut fields,
        omit,
        "completed_at",
        row.completed_at.as_deref(),
    );
    push_datetime_field(
        &mut fields,
        omit,
        "unclaimed_deadline_at",
        row.unclaimed_deadline_at.as_deref(),
    );
    push_datetime_field(
        &mut fields,
        omit,
        "cancel_cascade_intent_at",
        row.cancel_cascade_intent_at.as_deref(),
    );
    push_datetime_field(&mut fields, omit, "stuck_since", row.stuck_since.as_deref());

    if fields.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(", {}", fields.join(", ")))
    }
}

pub(crate) fn push_datetime_field(
    fields: &mut Vec<String>,
    omit: &[&str],
    field: &'static str,
    value: Option<&str>,
) {
    if omit.contains(&field) {
        return;
    }
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    let value = DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|_| value.to_string());
    fields.push(format!(r#"{field}: "{}""#, escape_graphql_string(&value)));
}
