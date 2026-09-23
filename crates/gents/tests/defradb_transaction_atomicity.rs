//! Bounded native external-premise experiment: real multi-document
//! transaction atomicity against the pinned embedded DefraDB node.
//!
//! These tests drive `ConfigAccess::transact_local` — the existing Gents
//! gated transaction adapter (`crates/gents/src/config_client/txn.rs`) —
//! over a disposable embedded DefraDB node with a minimal two-collection
//! schema. They provide commit/rollback evidence for DefraDB transactions
//! only: a committed transaction persists every write in the callback, an
//! injected error after the first write persists neither, and a failed
//! second mutation persists neither when the caller propagates the error.
//! No claim is made about host crash durability or remote P2P merge, and
//! the retired AgentResponse surface is not involved.

use anyhow::{bail, Context, Result};
use gents::config_client::{ConfigAccess, ConfigApplyTxn};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use serde_json::Value;

const ATOMICITY_SCHEMA: &str = "
type TxnAtomicityLeft { label: String amount: Int }
type TxnAtomicityRight { label: String amount: Int }
";

/// Disposable node with only the minimal two-collection schema installed.
async fn disposable_node() -> Result<EmbeddedNode> {
    let node = EmbeddedNode::builder().build().await?;
    node.add_schema(ATOMICITY_SCHEMA).await?;
    Ok(node)
}

/// Surface DefraDB-level errors even when the adapter returns `Ok`.
fn ensure_no_errors(value: Value) -> Result<Value> {
    if value
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        bail!("defradb reported errors: {value}");
    }
    Ok(value)
}

fn assert_acknowledged(value: &Value, collection: &str) -> Result<()> {
    let response = defra_node::QueryResponse::success(
        value.get("data").context("mutation omitted data")?.clone(),
    );
    let field = format!("create_{collection}");
    let row = gents::graphql::single_mutation_document(&response, &field)?
        .context("mutation returned no document")?;
    anyhow::ensure!(
        row.get("_docID")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "mutation omitted physical identity: {value}"
    );
    Ok(())
}

async fn collection_count(node: &EmbeddedNode, collection: &str) -> Result<usize> {
    let value = node
        .execute(&format!("query {{ {collection} {{ label }} }}"))
        .await;
    anyhow::ensure!(
        !value.has_errors(),
        "count query failed: {:?}",
        value.errors
    );
    Ok(value.data.context("count query omitted data")?[collection]
        .as_array()
        .context("count query omitted collection")?
        .len())
}

async fn counts(node: &EmbeddedNode) -> Result<(usize, usize)> {
    Ok((
        collection_count(node, "TxnAtomicityLeft").await?,
        collection_count(node, "TxnAtomicityRight").await?,
    ))
}

fn write_left(label: &str, amount: i64) -> String {
    format!(
        r#"mutation {{ create_TxnAtomicityLeft(input: {{ label: "{}", amount: {} }}) {{ _docID }} }}"#,
        escape_graphql_string(label),
        amount
    )
}

fn write_right(label: &str, amount: i64) -> String {
    format!(
        r#"mutation {{ create_TxnAtomicityRight(input: {{ label: "{}", amount: {} }}) {{ _docID }} }}"#,
        escape_graphql_string(label),
        amount
    )
}

/// Case 1: a successful transaction persists both writes.
#[tokio::test]
async fn committed_transaction_persists_both_writes() -> Result<()> {
    let node = disposable_node().await?;

    let (left, right) = ConfigAccess::transact_local(
        &node,
        None,
        "txn_atomicity_commit",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                let left = ensure_no_errors(txn.execute(&write_left("commit", 1)).await?)?;
                let right = ensure_no_errors(txn.execute(&write_right("commit", 2)).await?)?;
                Ok((left, right))
            })
        },
    )
    .await?;

    assert_acknowledged(&left, "TxnAtomicityLeft")?;
    assert_acknowledged(&right, "TxnAtomicityRight")?;
    assert_eq!(
        counts(&node).await?,
        (1, 1),
        "commit must persist both documents"
    );
    Ok(())
}

/// Case 2: an injected error after the first write leaves neither write,
/// because the caller returns the error out of the transaction callback.
#[tokio::test]
async fn injected_error_after_first_write_persists_neither() -> Result<()> {
    let node = disposable_node().await?;

    let outcome: Result<()> = ConfigAccess::transact_local(
        &node,
        None,
        "txn_atomicity_injected_rollback",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                let first = ensure_no_errors(txn.execute(&write_left("rollback", 3)).await?)?;
                assert_acknowledged(&first, "TxnAtomicityLeft")?;
                Err(anyhow::anyhow!("injected failure after first write"))
            })
        },
    )
    .await;

    assert!(
        outcome.is_err(),
        "the adapter must surface the injected error"
    );
    assert!(outcome
        .unwrap_err()
        .to_string()
        .contains("injected failure after first write"));
    assert_eq!(
        counts(&node).await?,
        (0, 0),
        "rollback after the first write must persist neither document"
    );
    Ok(())
}

/// Case 3: the second mutation fails inside the transaction and the caller
/// propagates its error, so neither write is persisted.
#[tokio::test]
async fn failed_second_mutation_persists_neither_when_propagated() -> Result<()> {
    let node = disposable_node().await?;

    let outcome: Result<()> = ConfigAccess::transact_local(
        &node,
        None,
        "txn_atomicity_second_failure",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                let first = ensure_no_errors(txn.execute(&write_left("second-failure", 5)).await?)?;
                assert_acknowledged(&first, "TxnAtomicityLeft")?;
                // The second mutation targets an absent collection. Unknown
                // input fields are not a reliable failure injection in the
                // pinned node: it accepts them rather than rejecting a write.
                // Propagate the actual database error with `?`.
                ensure_no_errors(
                    txn.execute(
                        r#"mutation { create_TxnAtomicityMissing(input: { label: "x", amount: 1 }) { _docID } }"#,
                    )
                    .await?,
                )?;
                Ok(())
            })
        },
    )
    .await;

    assert!(
        outcome.is_err(),
        "propagating the failed second mutation must fail the transaction: {outcome:?}"
    );
    let error = format!("{:#}", outcome.unwrap_err());
    assert!(
        error.contains("TxnAtomicityMissing"),
        "unexpected failure: {error}"
    );
    assert_eq!(
        counts(&node).await?,
        (0, 0),
        "a failed second mutation must persist neither document"
    );
    Ok(())
}
