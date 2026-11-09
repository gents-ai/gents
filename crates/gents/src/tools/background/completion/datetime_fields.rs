use super::*;

#[derive(Debug, Deserialize, Default)]
pub(crate) struct AgentToolCallDateTimeRow {
    pub(crate) started_at: Option<String>,
    pub(crate) deadline_at: Option<String>,
    pub(crate) completed_at: Option<String>,
    pub(crate) stuck_since: Option<String>,
}

pub(super) async fn agent_tool_call_datetime_update_fragment(
    access: &crate::ConfigAccess,
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
                stuck_since
            }}
        }}"#
    );
    let response = access.execute(&query).await?;
    let mut rows: Vec<AgentToolCallDateTimeRow> =
        serde_json::from_value(response["data"]["AgentToolCall"].clone())?;
    let row = rows
        .pop()
        .context("background completion tool row not found")?;

    let mut fields = Vec::new();
    push_datetime_field(&mut fields, omit, "started_at", row.started_at.as_deref());
    push_datetime_field(&mut fields, omit, "deadline_at", row.deadline_at.as_deref());
    push_datetime_field(
        &mut fields,
        omit,
        "completed_at",
        row.completed_at.as_deref(),
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
    // Re-supplied DateTime fields must round-trip exactly: `started_at` is
    // part of the background listing order, so truncating it reorders rows.
    let value = DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
        .unwrap_or_else(|_| value.to_string());
    fields.push(format!(r#"{field}: "{}""#, escape_graphql_string(&value)));
}
