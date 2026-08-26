use super::*;

pub(super) async fn load_latest_session_request_context(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    request_ids: &[String],
) -> anyhow::Result<Option<LoadedRequestContext>> {
    if request_ids.is_empty() {
        return Ok(None);
    }
    let request_ids = request_ids
        .iter()
        .map(|request_id| format!("\"{}\"", gents::graphql::escape_graphql_string(request_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"query {{
            InferenceCall(
                filter: {{
                    request_id: {{ _in: [{request_ids}] }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    call_kind: {{ _eq: "inference" }}
                }},
                order: {{ queued_at: DESC }},
                limit: 10
            ) {{
                request_id
                call_id
                call_seq
                queued_at
                context_accounting_json
            }}
        }}"#,
        agent_did = gents::graphql::escape_graphql_string(agent_did),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "querying session InferenceCall context accounting: {:?}",
            response.errors
        );
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    decode_latest_request_context(rows)
}

pub(super) fn decode_latest_request_context(
    rows: &[serde_json::Value],
) -> anyhow::Result<Option<LoadedRequestContext>> {
    let Some(row) = rows
        .iter()
        .filter(|row| {
            row.get("context_accounting_json")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
        .max_by(|left, right| {
            left.get("queued_at")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("queued_at")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )
                .then_with(|| {
                    left.get("call_seq")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or_default()
                        .cmp(
                            &right
                                .get("call_seq")
                                .and_then(serde_json::Value::as_i64)
                                .unwrap_or_default(),
                        )
                })
                .then_with(|| {
                    left.get("call_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .cmp(
                            right
                                .get("call_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                        )
                })
        })
    else {
        return Ok(None);
    };
    let required_string = |field: &str| {
        row.get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no {field}"))
    };
    let encoded = required_string("context_accounting_json")?;
    Ok(Some(LoadedRequestContext {
        request_id: required_string("request_id")?,
        call_id: required_string("call_id")?,
        call_sequence: row
            .get("call_seq")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no call_seq"))?,
        accounting: serde_json::from_str(&encoded)
            .map_err(|error| anyhow::anyhow!("decoding context_accounting_json: {error}"))?,
    }))
}
