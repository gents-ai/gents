use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct PersistedResponseState {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub content: String,
    pub status: String,
    pub token_count: usize,
}

pub(super) fn extract_mutation_doc_id<'a>(
    data: &'a serde_json::Value,
    collection_name: &str,
) -> Option<&'a str> {
    for field_name in [
        format!("upsert_{collection_name}"),
        format!("create_{collection_name}"),
        format!("add_{collection_name}"),
    ] {
        if let Some(value) = data.get(&field_name) {
            if let Some(doc_id) = value.get("_docID").and_then(|value| value.as_str()) {
                return Some(doc_id);
            }

            if let Some(doc_id) = value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(|value| value.as_str())
            {
                return Some(doc_id);
            }
        }
    }

    None
}

pub(super) async fn load_response_state(
    node: &EmbeddedNode,
    doc_id: &str,
) -> Result<Option<PersistedResponseState>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                _docID
                content
                status
                token_count
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading AgentResponse state for doc_id={doc_id}: {:?}",
            resp.errors
        );
    }

    let mut rows: Vec<PersistedResponseState> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    Ok(rows.pop())
}

pub(super) async fn load_response_state_by_key(
    node: &EmbeddedNode,
    response_key: &str,
) -> Result<Option<PersistedResponseState>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ response_key: {{ _eq: "{response_key}" }} }},
                limit: 1
            ) {{
                _docID
                content
                status
                token_count
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading AgentResponse state for response_key={response_key}: {:?}",
            resp.errors
        );
    }

    let mut rows: Vec<PersistedResponseState> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    Ok(rows.pop())
}
