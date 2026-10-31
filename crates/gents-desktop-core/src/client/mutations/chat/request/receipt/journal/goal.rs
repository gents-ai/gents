use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use gents::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents_protocol::goal_control::SessionGoalControlReceipt;
use std::sync::Arc;

use super::super::goal_delivery::{self, Decision, Intent, Record, Reference};
use super::{Entry, Envelope};

pub(crate) async fn lookup(
    node: &EmbeddedNode,
    client_did: &str,
    node_did: &str,
    command_id: &str,
) -> Result<Option<Record>> {
    let response = graphql_with_transaction_retry(
        node,
        &super::query(command_id),
        "desktop.goal.recover_caller_receipt",
    )
    .await?;
    match super::row(
        response
            .data
            .as_ref()
            .context("Goal caller query omitted data")?,
        command_id,
    )? {
        None => Ok(None),
        Some((_, Entry::Goal(record))) => {
            anyhow::ensure!(
                record.intent.client_did == client_did && record.intent.node_did == node_did,
                "Goal command belongs to another client or runtime"
            );
            Ok(Some(*record))
        }
        Some(_) => anyhow::bail!("command identity belongs to another submission"),
    }
}

pub(crate) async fn recover(node: &EmbeddedNode, intent: &Intent) -> Result<Option<Record>> {
    let record = lookup(
        node,
        &intent.client_did,
        &intent.node_did,
        &intent.command_id,
    )
    .await?;
    if let Some(record) = &record {
        anyhow::ensure!(
            goal_delivery::query(std::slice::from_ref(record), intent) != Decision::Refused,
            "Goal caller identity is bound to different authored input"
        );
    }
    Ok(record)
}

pub(crate) async fn reserve(node: &Arc<EmbeddedNode>, proposal: &Record) -> Result<Record> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "desktop.goal.reserve_caller_receipt",
        move |txn| Box::pin(async move {
            let records = match super::read_in_txn(txn, &proposal.intent.command_id).await? {
                Some((_, Entry::Goal(record))) => vec![*record],
                Some(_) => anyhow::bail!("command identity belongs to another submission"),
                None => Vec::new(),
            };
            let (staged, decision) = goal_delivery::reserve(&records, proposal, true);
            anyhow::ensure!(decision != Decision::Refused, "Goal caller binding was refused");
            if let Some(record) = staged {
                let id = escape_graphql_string(&record.intent.command_id);
                let payload = encode(&record)?;
                txn.execute(&format!("mutation {{ create_PendingChatSubmission(input: {{request_id: \"{id}\", payload: \"{payload}\"}}) {{ _docID }} }}")).await?;
                Ok(record)
            } else {
                records.into_iter().next().context("Goal reservation omitted its retained record")
            }
        }),
    ).await
}

fn encode(record: &Record) -> Result<String> {
    Ok(escape_graphql_string(&serde_json::to_string(
        &Envelope::Goal(Box::new(record.clone())),
    )?))
}

async fn write(txn: &ConfigApplyTxn<'_>, doc_id: &str, record: &Record) -> Result<()> {
    let id = escape_graphql_string(doc_id);
    let payload = encode(record)?;
    txn.execute(&format!("mutation {{ update_PendingChatSubmission(docID: \"{id}\", input: {{payload: \"{payload}\"}}) {{ _docID }} }}")).await?;
    Ok(())
}

/// The caller supplies only a receipt verified by the native Goal owner. The
/// journal transaction reloads its winner so stale settlement or acknowledgement
/// cannot replace another command, signed preparation or confirmed outcome.
pub(crate) async fn update(
    node: &Arc<EmbeddedNode>,
    intent: &Intent,
    reference: &Reference,
    receipt: Option<&SessionGoalControlReceipt>,
) -> Result<Record> {
    ConfigAccess::transact_local_idempotent(
        node,
        None,
        IdempotentTransactionRetry::Standard,
        "desktop.goal.update_caller_receipt",
        move |txn| {
            Box::pin(async move {
                let Some((doc_id, Entry::Goal(record))) =
                    super::read_in_txn(txn, &intent.command_id).await?
                else {
                    anyhow::bail!("Goal caller preparation is unavailable");
                };
                anyhow::ensure!(
                    goal_delivery::query(std::slice::from_ref(&record), intent)
                        != Decision::Refused
                        && record.reference == *reference,
                    "Goal caller binding was refused"
                );
                let before = encode(&record)?;
                let mut records = [*record];
                if let Some(receipt) = receipt {
                    goal_delivery::observe(&mut records, intent, receipt, true);
                } else {
                    goal_delivery::acknowledge(&mut records, intent, reference);
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
