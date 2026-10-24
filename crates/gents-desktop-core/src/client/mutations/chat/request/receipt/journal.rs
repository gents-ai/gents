use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use std::sync::Arc;

use super::correlated::{Outcome, Record};
use super::UnresolvedSubmission;

pub(crate) mod caller;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "record", deny_unknown_fields)]
enum Envelope {
    #[serde(rename = "callerReceipt-v1")]
    Caller(Record),
}

enum Entry {
    Uncorrelated(UnresolvedSubmission),
    Caller(Record),
}

fn decode(payload: &str, request_id: &str) -> Result<Entry> {
    let value: serde_json::Value =
        serde_json::from_str(payload).context("decoding retained submission")?;
    if value.get("kind").is_some() {
        let Envelope::Caller(record) =
            serde_json::from_value(value).context("decoding caller submission receipt")?;
        anyhow::ensure!(
            record.intent.request_id == request_id
                && record.result() != super::correlated::Decision::Refused,
            "retained caller receipt binding is invalid"
        );
        Ok(Entry::Caller(record))
    } else {
        let pending: UnresolvedSubmission =
            serde_json::from_value(value).context("decoding retained signed submission")?;
        pending.validate_binding(request_id)?;
        Ok(Entry::Uncorrelated(pending))
    }
}

fn query(request_id: &str) -> String {
    let id = escape_graphql_string(request_id);
    format!("{{ PendingChatSubmission(filter: {{request_id: {{_eq: \"{id}\"}}}}, limit: 2) {{_docID payload}} }}")
}

fn row(data: &serde_json::Value, request_id: &str) -> Result<Option<(String, Entry)>> {
    let rows = data
        .get("PendingChatSubmission")
        .and_then(serde_json::Value::as_array)
        .context("pending submission query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "retained submission identity is ambiguous");
    rows.first()
        .map(|row| {
            let doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .context("pending submission has no document identity")?;
            let payload = row
                .get("payload")
                .and_then(serde_json::Value::as_str)
                .context("pending submission has no payload")?;
            Ok((doc_id.to_owned(), decode(payload, request_id)?))
        })
        .transpose()
}

async fn read_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request_id: &str,
) -> Result<Option<(String, Entry)>> {
    let response = txn.execute(&query(request_id)).await?;
    row(
        response
            .get("data")
            .context("pending submission query omitted data")?,
        request_id,
    )
}

/// The local encrypted store retains signed intent before the owner can see it.
/// This collection stays outside the desktop replication catalog.
pub(crate) async fn save(node: &Arc<EmbeddedNode>, pending: &UnresolvedSubmission) -> Result<()> {
    let request_id = escape_graphql_string(&pending.request_id);
    let payload = escape_graphql_string(&serde_json::to_string(pending)?);
    ConfigAccess::write_local(
        node,
        "desktop.chat.retain_submission",
        &format!("mutation {{ create_PendingChatSubmission(input: {{request_id: \"{request_id}\", payload: \"{payload}\"}}) {{ _docID }} }}"),
    )
    .await?;
    Ok(())
}

pub(crate) async fn load(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<UnresolvedSubmission>> {
    let response =
        graphql_with_transaction_retry(node, &query(request_id), "desktop.chat.load_submission")
            .await?;
    let data = response
        .data
        .as_ref()
        .context("pending submission query omitted data")?;
    Ok(row(data, request_id)?.and_then(|(_, entry)| match entry {
        Entry::Uncorrelated(pending) => Some(pending),
        Entry::Caller(record) if record.outcome == Outcome::Unresolved => record.pending,
        Entry::Caller(_) => None,
    }))
}

pub(crate) async fn forget(node: &Arc<EmbeddedNode>, request_id: &str) {
    let result = ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "desktop.chat.forget_submission",
        move |txn| {
            Box::pin(async move {
                if let Some((doc_id, Entry::Uncorrelated(_))) = read_in_txn(txn, request_id).await?
                {
                    let id = escape_graphql_string(&doc_id);
                    txn.execute(&format!(
                        "mutation {{ delete_PendingChatSubmission(docID: \"{id}\") {{ _docID }} }}"
                    ))
                    .await?;
                }
                Ok(())
            })
        },
    )
    .await;
    if let Err(error) = result {
        tracing::warn!(%request_id, %error, "retained submission cleanup failed after its outcome was resolved");
    }
}
