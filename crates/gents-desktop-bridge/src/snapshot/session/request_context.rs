use super::*;

pub(super) async fn load_latest_session_request_context(
    access: &gents::config_client::ConfigAccess,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> anyhow::Result<Option<LoadedRequestContext>> {
    let request_bindings =
        gents::toolset::load_session_request_doc_ids(access, node_did, requester_did, session_id)
            .await?;
    if request_bindings.is_empty() {
        return Ok(None);
    }
    let request_doc_ids = request_bindings
        .keys()
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
        .ok_or_else(|| {
            anyhow::anyhow!("session InferenceCall response omitted its result array")
        })?;
    for row in rows {
        validated_call_request_identity(row, &request_bindings)?;
    }
    decode_latest_request_context(rows)
}

/// `request_doc_ids` come from the scoped transcript page, so each one has a
/// canonical message anchor and needs no second AgentRequest scope check.
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
    let physical_ids = request_doc_ids
        .iter()
        .map(|doc_id| format!(r#""{}""#, gents::graphql::escape_graphql_string(doc_id)))
        .collect::<Vec<_>>()
        .join(", ");
    let requester = requester_did
        .map(gents::graphql::escape_graphql_string)
        .map(|did| format!("\"{did}\""))
        .unwrap_or_else(|| "null".to_string());
    let query = format!(
        r#"query {{
            AgentMessage(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                session_id: {{ _eq: "{session_id}" }},
                requester_did: {{ _eq: {requester} }},
                request_doc_id: {{ _in: [{physical_ids}] }}
            }}) {{ request_doc_id sequence }}
            InferenceCall(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{node_did}" }} }},
                    {{ call_kind: {{ _eq: "inference" }} }},
                    {{ request_doc_id: {{ _in: [{physical_ids}] }} }}
                ] }},
                order: [{{ queued_at: ASC }}, {{ request_doc_id: ASC }}, {{ call_seq: ASC }}, {{ call_id: ASC }}]
            ) {{ request_doc_id request_id call_id call_seq queued_at context_accounting_json }}
        }}"#,
        node_did = gents::graphql::escape_graphql_string(node_did),
        session_id = gents::graphql::escape_graphql_string(session_id),
    );
    let data = access.execute(&query).await?;
    let result = |collection: &str| {
        data.pointer(&format!("/data/{collection}"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("page {collection} response omitted its result array"))
    };
    let mut anchors = std::collections::BTreeMap::<&str, i64>::new();
    for row in result("AgentMessage")? {
        let (Some(doc_id), Some(sequence)) = (
            row.get("request_doc_id")
                .and_then(serde_json::Value::as_str),
            row.get("sequence").and_then(serde_json::Value::as_i64),
        ) else {
            anyhow::bail!("page AgentMessage row has no request anchor");
        };
        anchors
            .entry(doc_id)
            .and_modify(|current| *current = (*current).min(sequence))
            .or_insert(sequence);
    }
    result("InferenceCall")?
        .iter()
        .filter(|row| has_context_accounting(row))
        .map(|row| {
            let mut context = decode_request_context_row(row)?;
            context.anchor_sequence = context
                .request_doc_id
                .as_deref()
                .and_then(|doc_id| anchors.get(doc_id).copied());
            Ok(context)
        })
        .collect()
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

fn has_context_accounting(row: &serde_json::Value) -> bool {
    row.get("context_accounting_json")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

pub(super) fn decode_latest_request_context(
    rows: &[serde_json::Value],
) -> anyhow::Result<Option<LoadedRequestContext>> {
    rows.iter()
        .filter(|row| has_context_accounting(row))
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
        .map(decode_request_context_row)
        .transpose()
}

fn decode_request_context_row(row: &serde_json::Value) -> anyhow::Result<LoadedRequestContext> {
    let required_string = |field: &str| {
        row.get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no {field}"))
    };
    let encoded = required_string("context_accounting_json")?;
    Ok(LoadedRequestContext {
        request_id: required_string("request_id")?,
        request_doc_id: required_string("request_doc_id").ok(),
        anchor_sequence: None,
        call_id: required_string("call_id")?,
        call_sequence: row
            .get("call_seq")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("InferenceCall accounting row has no call_seq"))?,
        queued_at: row
            .get("queued_at")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        accounting: serde_json::from_str(&encoded)
            .map_err(|error| anyhow::anyhow!("decoding context_accounting_json: {error}"))?,
    })
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
