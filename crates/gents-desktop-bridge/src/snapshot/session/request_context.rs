use super::*;

pub(super) async fn load_latest_session_request_context(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> anyhow::Result<Option<LoadedRequestContext>> {
    let requester = requester_did
        .map(gents::graphql::escape_graphql_string)
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".to_string());
    let requests_query = format!(
        r#"query {{ AgentRequest(filter: {{ purpose: {{ _eq: "normal" }}, node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}, order: {{ created_at: DESC }}, limit: 500) {{ _docID request_id }} }}"#,
        gents::graphql::escape_graphql_string(node_did),
        gents::graphql::escape_graphql_string(session_id),
    );
    let requests = access.execute(&requests_query).await?;
    let request_rows = requests
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("session AgentRequest response omitted its result array"))?;
    let mut request_bindings = std::collections::BTreeMap::new();
    for row in request_rows {
        let doc_id = row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("session AgentRequest row has no physical document ID"))?;
        let request_id = row
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("session AgentRequest row has no logical request ID"))?;
        request_bindings.insert(doc_id.to_owned(), request_id.to_owned());
    }
    let request_doc_ids = request_bindings.keys().cloned().collect::<Vec<_>>();
    if request_doc_ids.is_empty() {
        return Ok(None);
    }
    let request_doc_ids = request_doc_ids
        .iter()
        .map(|request_doc_id| {
            format!(
                "\"{}\"",
                gents::graphql::escape_graphql_string(request_doc_id)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"query {{
            InferenceCall(
                filter: {{
                    request_doc_id: {{ _in: [{request_doc_ids}] }},
                    node_did: {{ _eq: "{node_did}" }},
                    call_kind: {{ _eq: "inference" }}
                }},
                order: {{ queued_at: DESC }},
                limit: 10
            ) {{
                request_doc_id
                request_id
                call_id
                call_seq
                queued_at
                context_accounting_json
            }}
        }}"#,
        node_did = gents::graphql::escape_graphql_string(node_did),
    );
    let data = access.execute(&query).await?;
    let rows = data
        .pointer("/data/InferenceCall")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("session InferenceCall response omitted its result array"))?;
    let mut scoped_rows = Vec::with_capacity(rows.len());
    for row in rows {
        validated_call_request_identity(row, &request_bindings)?;
        let mut row = row.clone();
        row["_validated_request_doc_id"] = row["request_doc_id"].clone();
        scoped_rows.push(row);
    }
    decode_latest_request_context(&scoped_rows)
}

pub(super) async fn load_session_request_context_history(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_ids: &[String],
) -> anyhow::Result<Vec<LoadedRequestContext>> {
    if request_doc_ids.is_empty() {
        return Ok(Vec::new());
    }
    let request_doc_ids = request_doc_ids
        .iter()
        .map(|request_doc_id| format!(r#""{}""#, gents::graphql::escape_graphql_string(request_doc_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let requester = requester_did
        .map(gents::graphql::escape_graphql_string)
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".to_string());
    let request_query = format!(
        r#"query {{ AgentRequest(docIDs: [{request_doc_ids}], filter: {{ purpose: {{ _eq: "normal" }}, node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}) {{ _docID request_id }} }}"#,
        gents::graphql::escape_graphql_string(node_did),
        gents::graphql::escape_graphql_string(session_id),
    );
    let request_data = access.execute(&request_query).await?;
    let request_rows = request_data
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("page AgentRequest response omitted its result array"))?;
    let mut request_bindings = std::collections::BTreeMap::new();
    for row in request_rows {
        let doc_id = row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("page AgentRequest row has no physical document ID"))?;
        let request_id = row
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("page AgentRequest row has no logical request ID"))?;
        request_bindings.insert(doc_id.to_owned(), request_id.to_owned());
    }
    if request_bindings.is_empty() {
        return Ok(Vec::new());
    }
    let physical_ids = request_bindings
        .keys()
        .map(|doc_id| format!(r#""{}""#, gents::graphql::escape_graphql_string(doc_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let message_query = format!(
        r#"query {{ AgentMessage(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}, request_doc_id: {{ _in: [{physical_ids}] }} }}) {{ request_doc_id sequence }} }}"#,
        gents::graphql::escape_graphql_string(node_did),
        gents::graphql::escape_graphql_string(session_id),
    );
    let message_data = access.execute(&message_query).await?;
    let message_rows = message_data
        .pointer("/data/AgentMessage")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("page AgentMessage response omitted its result array"))?;
    let mut anchors = std::collections::BTreeMap::<String, i64>::new();
    for row in message_rows {
        let doc_id = row
            .get("request_doc_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("page AgentMessage row has no physical request ID"))?;
        let sequence = row
            .get("sequence")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("page AgentMessage row has no sequence"))?;
        anchors.entry(doc_id.to_owned()).and_modify(|current| *current = (*current).min(sequence)).or_insert(sequence);
    }
    let mut history = Vec::new();
    let mut offset = 0_usize;
    const PAGE: usize = 250;
    const MAX_ROWS: usize = 2_000;
    loop {
        anyhow::ensure!(offset <= MAX_ROWS, "page context history exceeds bounded limit of {MAX_ROWS} calls");
        let page_limit = if offset == MAX_ROWS { 1 } else { PAGE };
        let query = format!(
        r#"query {{
            InferenceCall(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ call_kind: {{ _eq: "inference" }} }},
                    {{ request_doc_id: {{ _in: [{physical_ids}] }} }}
                ] }}, order: [{{ queued_at: ASC }}, {{ request_doc_id: ASC }}, {{ call_seq: ASC }}, {{ call_id: ASC }}], limit: {page_limit}, offset: {offset}
            ) {{ request_doc_id request_id call_id call_seq queued_at context_accounting_json }}
        }}"#,
        node_did = gents::graphql::escape_graphql_string(node_did),
    );
        let data = access.execute(&query).await?;
        let rows = data
            .pointer("/data/InferenceCall")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("page InferenceCall response omitted its result array"))?;
        if offset == MAX_ROWS {
            anyhow::ensure!(rows.is_empty(), "page context history exceeds bounded limit of {MAX_ROWS} calls");
            break;
        }
        for row in rows {
        let Some(encoded) = row
            .get("context_accounting_json")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let accounting = serde_json::from_str(encoded)
            .map_err(|error| anyhow::anyhow!("decoding context_accounting_json: {error}"))?;
        let required = |field: &str| {
            row.get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no {field}"))
        };
        let (request_doc_id, request_id) =
            validated_call_request_identity(row, &request_bindings)?;
        anyhow::ensure!(
            anchors.contains_key(&request_doc_id),
            "page AgentRequest has context accounting but no canonical message anchor"
        );
        history.push(LoadedRequestContext {
            request_id,
            request_doc_id: Some(request_doc_id.clone()),
            anchor_sequence: anchors.get(&request_doc_id).copied(),
            call_id: required("call_id")?,
            call_sequence: row
                .get("call_seq")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no call_seq"))?,
            queued_at: row.get("queued_at").and_then(serde_json::Value::as_str).map(str::to_owned),
            accounting,
        });
        }
        if rows.len() < PAGE {
            break;
        }
        offset = offset.saturating_add(PAGE);
    }
    Ok(history)
}

fn validated_call_request_identity(
    row: &serde_json::Value,
    request_bindings: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<(String, String)> {
    let required = |field: &str| {
        row.get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no {field}"))
    };
    let request_doc_id = required("request_doc_id")?;
    let request_id = required("request_id")?;
    anyhow::ensure!(
        request_bindings.get(&request_doc_id) == Some(&request_id),
        "InferenceCall request identity does not match its scoped AgentRequest"
    );
    Ok((request_doc_id, request_id))
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
        request_doc_id: row.get("_validated_request_doc_id").and_then(serde_json::Value::as_str).map(str::to_owned),
        anchor_sequence: None,
        call_id: required_string("call_id")?,
        call_sequence: row
            .get("call_seq")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no call_seq"))?,
        queued_at: row.get("queued_at").and_then(serde_json::Value::as_str).map(str::to_owned),
        accounting: serde_json::from_str(&encoded)
            .map_err(|error| anyhow::anyhow!("decoding context_accounting_json: {error}"))?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_call_logical_ids_must_match_the_scoped_physical_request() {
        let bindings = std::collections::BTreeMap::from([(
            "AgentRequest/physical-1".to_owned(),
            "request-1".to_owned(),
        )]);
        let valid = serde_json::json!({
            "request_doc_id": "AgentRequest/physical-1",
            "request_id": "request-1"
        });
        assert_eq!(
            validated_call_request_identity(&valid, &bindings).unwrap(),
            ("AgentRequest/physical-1".to_owned(), "request-1".to_owned())
        );
        let collision = serde_json::json!({
            "request_doc_id": "AgentRequest/physical-1",
            "request_id": "request-from-another-session"
        });
        assert!(validated_call_request_identity(&collision, &bindings).is_err());
    }
}
