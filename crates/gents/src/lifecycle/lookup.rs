use super::*;

#[derive(Debug, Clone, serde::Deserialize)]
struct LogicalStatusRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    status: String,
}

fn resolve_status_row(
    collection: &'static str,
    request_id: &str,
    rows: Vec<LogicalStatusRow>,
) -> Result<Option<LogicalStatusRow>> {
    Ok(crate::session::resolve_exact_logical_match(
        collection,
        "request_id",
        request_id,
        rows,
        |row| row.doc_id.as_str(),
    )?)
}

pub(super) async fn lookup_response_status_by_request_id(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
) -> Result<Option<String>> {
    if request_id.is_empty() {
        return Ok(None);
    }

    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                }}
            ) {{
                _docID
                status
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "querying response status for request_id={request_id}: {:?}",
            resp.errors
        );
    }

    let rows: Vec<LogicalStatusRow> = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentResponse"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Ok(resolve_status_row("AgentResponse", request_id, rows)?.map(|row| row.status))
}

pub(super) async fn lookup_request_status_by_request_id(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
) -> Result<Option<String>> {
    if request_id.is_empty() {
        return Ok(None);
    }

    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                }}
            ) {{
                _docID
                status
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "querying request status for request_id={request_id}: {:?}",
            resp.errors
        );
    }

    let rows: Vec<LogicalStatusRow> = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Ok(resolve_status_row("AgentRequest", request_id, rows)?.map(|row| row.status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_resolution_rejects_logical_twins_deterministically() {
        let error = resolve_status_row(
            "AgentRequest",
            "request-same",
            vec![
                LogicalStatusRow {
                    doc_id: "doc-z".to_string(),
                    status: "processing".to_string(),
                },
                LogicalStatusRow {
                    doc_id: "doc-a".to_string(),
                    status: "completed".to_string(),
                },
            ],
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "AgentRequest logical identity conflict for request_id=request-same: _docIDs=[\"doc-a\", \"doc-z\"]"
        );
    }

    #[test]
    fn status_resolution_accepts_one_physical_document() {
        let row = resolve_status_row(
            "AgentResponse",
            "request-one",
            vec![LogicalStatusRow {
                doc_id: "doc-one".to_string(),
                status: "completed".to_string(),
            }],
        )
        .unwrap()
        .unwrap();

        assert_eq!(row.doc_id, "doc-one");
        assert_eq!(row.status, "completed");
    }
}
