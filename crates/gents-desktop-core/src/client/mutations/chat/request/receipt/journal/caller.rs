use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use std::sync::Arc;

use super::super::correlated::{self, Decision, Intent, Outcome, Record, Reference};
use super::{Entry, Envelope};

pub(crate) async fn lookup(
    node: &EmbeddedNode,
    client_did: &str,
    node_did: &str,
    request_id: &str,
) -> Result<Option<Record>> {
    let response = graphql_with_transaction_retry(
        node,
        &super::query(request_id),
        "desktop.chat.recover_caller_receipt",
    )
    .await?;
    let data = response
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("caller receipt query omitted data"))?;
    match super::row(data, request_id)? {
        None => Ok(None),
        Some((_, Entry::Caller(record))) => {
            anyhow::ensure!(
                record.intent.client_did == client_did && record.intent.node_did == node_did,
                "caller submission belongs to another actor or runtime"
            );
            Ok(Some(*record))
        }
        Some((_, Entry::Uncorrelated(_))) => {
            anyhow::bail!("request identity has no caller binding")
        }
        Some((_, Entry::Goal(_))) => anyhow::bail!("request identity belongs to a Goal command"),
    }
}

pub(crate) async fn recover(node: &EmbeddedNode, intent: &Intent) -> Result<Option<Record>> {
    let record = lookup(
        node,
        &intent.client_did,
        &intent.node_did,
        &intent.request_id,
    )
    .await?;
    if let Some(record) = &record {
        anyhow::ensure!(
            correlated::recover(std::slice::from_ref(record), intent)
                .is_some_and(|decision| decision != Decision::Refused),
            "caller submission identity is already bound to different input"
        );
    }
    Ok(record)
}

/// The transaction owner reconciles a unique durable winner on every retry.
/// The proposal is signed before entering the transaction; no owner submission
/// is authorized until this function returns its committed record.
pub(crate) async fn reserve(node: &Arc<EmbeddedNode>, proposal: &Record) -> Result<Record> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "desktop.chat.reserve_caller_receipt",
        move |txn| Box::pin(async move {
            let existing = super::read_in_txn(txn, &proposal.intent.request_id).await?;
            let records = match existing {
                Some((_, Entry::Caller(record))) => vec![*record],
                Some((_, Entry::Uncorrelated(_))) => anyhow::bail!("request identity has no caller binding"),
                Some((_, Entry::Goal(_))) => anyhow::bail!("request identity belongs to a Goal command"),
                None => Vec::new(),
            };
            let (staged, decision) = correlated::prepare(&records, proposal, true);
            anyhow::ensure!(decision != Decision::Refused, "caller submission binding was refused");
            if let Some(record) = staged {
                let id = escape_graphql_string(&record.intent.request_id);
                let payload = encode(&record)?;
                txn.execute(&format!("mutation {{ create_PendingChatSubmission(input: {{request_id: \"{id}\", payload: \"{payload}\"}}) {{ _docID }} }}")).await?;
                Ok(record)
            } else {
                Ok(records.into_iter().next().expect("recovery requires an existing record"))
            }
        }),
    ).await
}

fn encode(record: &Record) -> Result<String> {
    Ok(escape_graphql_string(&serde_json::to_string(
        &Envelope::Caller(Box::new(record.clone())),
    )?))
}

async fn write(txn: &ConfigApplyTxn<'_>, doc_id: &str, record: &Record) -> Result<()> {
    let doc_id = escape_graphql_string(doc_id);
    let payload = encode(record)?;
    txn.execute(&format!("mutation {{ update_PendingChatSubmission(docID: \"{doc_id}\", input: {{payload: \"{payload}\"}}) {{ _docID }} }}")).await?;
    Ok(())
}

/// Updates reload the winner inside the same transaction as the write. A stale
/// acknowledgement or observation cannot overwrite a newer confirmed outcome.
pub(crate) async fn update(
    node: &Arc<EmbeddedNode>,
    intent: &Intent,
    reference: &Reference,
    observation: Option<Outcome>,
) -> Result<Record> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "desktop.chat.update_caller_receipt",
        move |txn| {
            Box::pin(async move {
                let Some((doc_id, Entry::Caller(record))) =
                    super::read_in_txn(txn, &intent.request_id).await?
                else {
                    anyhow::bail!("caller submission receipt is unavailable");
                };
                anyhow::ensure!(
                    correlated::recover(std::slice::from_ref(&record), intent)
                        .is_some_and(|decision| decision != Decision::Refused)
                        && record.reference == *reference,
                    "caller submission receipt binding was refused"
                );
                let before = encode(&record)?;
                let mut records = [*record];
                if let Some(outcome) = observation {
                    correlated::observe(
                        &mut records,
                        &intent.client_did,
                        &intent.request_id,
                        reference,
                        outcome,
                    );
                } else {
                    correlated::acknowledge(
                        &mut records,
                        &intent.client_did,
                        &intent.request_id,
                        reference,
                    );
                }
                let [record] = records;
                if before != encode(&record)? {
                    write(txn, &doc_id, &record).await?;
                }
                Ok(record)
            })
        },
    )
    .await
}
