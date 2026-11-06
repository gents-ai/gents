use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use gents::store::graphql::escape_graphql_string;
use std::sync::Arc;

use super::correlated::{Outcome, Record};
use super::UnresolvedSubmission;

pub(crate) mod caller;
pub(crate) mod goal;
pub(crate) mod publication;

pub(crate) fn require_client_signer(node: &EmbeddedNode, client: &str) -> Result<()> {
    anyhow::ensure!(
        !client.is_empty() && node.node_identity_did() == Some(client),
        "client journal actor differs from the local client signer"
    );
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "record", deny_unknown_fields)]
enum Envelope {
    #[serde(rename = "callerReceipt-v1")]
    Caller(Box<Record>),
    #[serde(rename = "goalCallerReceipt-v1")]
    Goal(Box<super::goal_delivery::Record>),
    #[serde(rename = "agentPublication-v1")]
    Publication(Box<publication::Record>),
}

enum Entry {
    Uncorrelated(Box<UnresolvedSubmission>),
    Caller(Box<Record>),
    Goal(Box<super::goal_delivery::Record>),
    Publication(Box<publication::Record>),
}

fn decode(payload: &str, request_id: &str) -> Result<Entry> {
    let value: serde_json::Value =
        serde_json::from_str(payload).context("decoding retained submission")?;
    if value.get("kind").is_some() {
        let envelope: Envelope =
            serde_json::from_value(value).context("decoding caller submission receipt")?;
        match envelope {
            Envelope::Caller(record) => {
                anyhow::ensure!(
                    record.intent.request_id == request_id
                        && record.result() != super::correlated::Decision::Refused,
                    "retained caller receipt binding is invalid"
                );
                Ok(Entry::Caller(record))
            }
            Envelope::Goal(record) => {
                anyhow::ensure!(
                    record.intent.command_id == request_id
                        && record.decision() != super::goal_delivery::Decision::Refused,
                    "retained Goal caller binding is invalid"
                );
                Ok(Entry::Goal(record))
            }
            Envelope::Publication(record) => {
                anyhow::ensure!(
                    record.key() == request_id,
                    "retained publication binding is invalid"
                );
                record.validate()?;
                Ok(Entry::Publication(record))
            }
        }
    } else {
        let pending: UnresolvedSubmission =
            serde_json::from_value(value).context("decoding retained signed submission")?;
        pending.validate_binding(request_id)?;
        Ok(Entry::Uncorrelated(Box::new(pending)))
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

fn require_written(response: &serde_json::Value, field: &str) -> Result<()> {
    let rows = gents::store::graphql::mutation_documents(response, field)?
        .context("client journal mutation omitted its result")?;
    anyhow::ensure!(
        rows.len() == 1,
        "client journal mutation did not write exactly one document"
    );
    Ok(())
}

/// The local encrypted store retains signed intent before the owner can see it.
/// This collection stays outside the desktop replication catalog.
pub(crate) async fn save(
    node: &Arc<EmbeddedNode>,
    client: &str,
    pending: &UnresolvedSubmission,
) -> Result<()> {
    let request_id = escape_graphql_string(&pending.request_id);
    let payload = escape_graphql_string(&serde_json::to_string(pending)?);
    let response = ConfigAccess::local_as(node.clone(), identity::Did::new(client)?).write(
        "desktop.chat.retain_submission",
        &format!("mutation {{ create_PendingChatSubmission(input: {{request_id: \"{request_id}\", payload: \"{payload}\"}}) {{ _docID }} }}"),
    )
    .await?;
    require_written(&response, "create_PendingChatSubmission")
}

pub(crate) async fn load(
    node: &Arc<EmbeddedNode>,
    client: &str,
    request_id: &str,
) -> Result<Option<UnresolvedSubmission>> {
    let response = ConfigAccess::local_as(node.clone(), identity::Did::new(client)?)
        .execute(&query(request_id))
        .await?;
    let data = response
        .get("data")
        .context("pending submission query omitted data")?;
    Ok(row(data, request_id)?.and_then(|(_, entry)| match entry {
        Entry::Uncorrelated(pending) => Some(*pending),
        Entry::Caller(record) if record.outcome == Outcome::Unresolved => record.pending,
        Entry::Caller(_) => None,
        Entry::Goal(_) => None,
        Entry::Publication(_) => None,
    }))
}

pub(crate) async fn forget(node: &Arc<EmbeddedNode>, client: &str, request_id: &str) -> Result<()> {
    let actor = identity::Did::new(client)?;
    ConfigAccess::transact_local_idempotent(
        node,
        Some(actor),
        IdempotentTransactionRetry::Standard,
        "desktop.chat.forget_submission",
        move |txn| {
            Box::pin(async move {
                if let Some((doc_id, Entry::Uncorrelated(_))) = read_in_txn(txn, request_id).await?
                {
                    let id = escape_graphql_string(&doc_id);
                    let response = txn
                        .execute(&format!(
                        "mutation {{ delete_PendingChatSubmission(docID: \"{id}\") {{ _docID }} }}"
                    ))
                        .await?;
                    require_written(&response, "delete_PendingChatSubmission")?;
                }
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
    .context("client journal cleanup failed")
}

#[cfg(test)]
mod mutation_result_tests {
    use super::require_written;

    #[test]
    fn journal_mutation_requires_one_written_document() {
        let empty = serde_json::json!({"data": {"update_PendingChatSubmission": []}});
        assert!(require_written(&empty, "update_PendingChatSubmission").is_err());
        let written = serde_json::json!({"data": {"update_PendingChatSubmission": [{"_docID": "owner-document"}]}});
        require_written(&written, "update_PendingChatSubmission").unwrap();
    }
}
