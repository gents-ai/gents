use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::Value;

use crate::graphql::escape_graphql_string;

use super::{SubmissionRecord, STATUS_PREPARED, STATUS_SUBMITTED_UNKNOWN};

pub(super) async fn max_in_flight_nonce(
    node: &EmbeddedNode,
    from: &str,
    chain_id: u64,
) -> Result<Option<u64>> {
    let from = escape_graphql_string(&from.to_ascii_lowercase());
    let query = format!(
        r#"{{
            EthSubmission(filter: {{
                from_address: {{ _eq: "{from}" }},
                chain_id: {{ _eq: {chain_id} }}
            }}) {{ nonce status }}
        }}"#
    );
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "max_in_flight_nonce").await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("EthSubmission"))
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if rows.is_null() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct Row {
        nonce: i64,
        status: String,
    }
    let rows: Vec<Row> = serde_json::from_value(rows)?;
    Ok(rows
        .into_iter()
        .filter(|row| row.status == STATUS_PREPARED || row.status == STATUS_SUBMITTED_UNKNOWN)
        .filter_map(|row| u64::try_from(row.nonce).ok())
        .max())
}

pub(super) async fn load_submission(
    node: &EmbeddedNode,
    submission_key: &str,
) -> Result<Option<SubmissionRecord>> {
    let key = escape_graphql_string(submission_key);
    let query = format!(
        r#"{{
            EthSubmission(filter: {{ submission_key: {{ _eq: "{key}" }} }}, limit: 1) {{
                _docID submission_key principal_did chain_id request_hash from_address nonce
                raw_transaction tx_hash status receipt_json
            }}
        }}"#
    );
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "load_submission").await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("EthSubmission"))
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if rows.is_null() {
        return Ok(None);
    }
    let mut rows: Vec<SubmissionRecord> = serde_json::from_value(rows)?;
    Ok(rows.pop())
}

pub(super) async fn create_submission(
    node: &std::sync::Arc<EmbeddedNode>,
    record: &SubmissionRecord,
) -> Result<()> {
    let submission_key = escape_graphql_string(&record.submission_key);
    let principal_did = escape_graphql_string(&record.principal_did);
    let request_hash = escape_graphql_string(&record.request_hash);
    let from_address = escape_graphql_string(&record.from_address);
    let raw_transaction = escape_graphql_string(&record.raw_transaction);
    let tx_hash = escape_graphql_string(&record.tx_hash);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            create_EthSubmission(input: {{
                submission_key: "{submission_key}",
                principal_did: "{principal_did}",
                chain_id: {},
                request_hash: "{request_hash}",
                from_address: "{from_address}",
                nonce: {},
                raw_transaction: "{raw_transaction}",
                tx_hash: "{tx_hash}",
                status: "{STATUS_PREPARED}",
                receipt_json: null,
                created_at: "{now}",
                updated_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        record.chain_id, record.nonce
    );
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "eth.create_submission",
        &mutation,
    )
    .await?;
    Ok(())
}

pub(super) async fn update_submission(
    node: &std::sync::Arc<EmbeddedNode>,
    doc_id: &str,
    status: &str,
    receipt: Option<&Value>,
) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let status = escape_graphql_string(status);
    let receipt = match receipt {
        Some(value) => format!(
            "\"{}\"",
            escape_graphql_string(&serde_json::to_string(value)?)
        ),
        None => "null".to_string(),
    };
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_EthSubmission(
                docID: "{doc_id}",
                input: {{ status: "{status}", receipt_json: {receipt}, updated_at: "{now}" }}
            ) {{ _docID }}
        }}"#
    );
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "eth.update_submission",
        &mutation,
    )
    .await?;
    Ok(())
}
