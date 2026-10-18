//! Bounded native external-premise experiment: real multi-document
//! transaction atomicity against the pinned embedded DefraDB node, through
//! `ConfigAccess::transact_local_write`. Commit/rollback evidence only:
//! no claim about host crash durability or remote P2P merge. A blocked
//! outside read is permitted; a partial pair is not.

use anyhow::{bail, Context, Result};
use gents::config_client::{ConfigAccess, ConfigApplyTxn};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::watch;

const ATOMICITY_SCHEMA: &str = "
type TxnAtomicityLeft { label: String amount: Int }
type TxnAtomicityRight { label: String amount: Int }
";

async fn disposable_node() -> Result<Arc<EmbeddedNode>> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    node.add_schema(ATOMICITY_SCHEMA).await?;
    Ok(node)
}

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

async fn pair_counts(node: &EmbeddedNode) -> Result<(usize, usize)> {
    let response = gents::graphql::graphql_with_transaction_retry(
        node,
        "query { TxnAtomicityLeft { label } TxnAtomicityRight { label } }",
        "txn_atomicity_independent_reader",
    )
    .await?;
    let data = response.data.context("pair query omitted data")?;
    let left = data["TxnAtomicityLeft"]
        .as_array()
        .context("pair query omitted left collection")?
        .len();
    let right = data["TxnAtomicityRight"]
        .as_array()
        .context("pair query omitted right collection")?
        .len();
    Ok((left, right))
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

#[tokio::test]
async fn committed_transaction_persists_both_writes() -> Result<()> {
    let node = disposable_node().await?;

    let (left, right) = ConfigAccess::transact_local_write(
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

#[tokio::test]
async fn injected_error_after_first_write_persists_neither() -> Result<()> {
    let node = disposable_node().await?;

    let outcome: Result<()> = ConfigAccess::transact_local_write(
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

#[tokio::test]
async fn failed_second_mutation_persists_neither_when_propagated() -> Result<()> {
    let node = disposable_node().await?;

    let outcome: Result<()> = ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_second_failure",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                let first = ensure_no_errors(txn.execute(&write_left("second-failure", 5)).await?)?;
                assert_acknowledged(&first, "TxnAtomicityLeft")?;
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

#[tokio::test]
async fn independent_reader_never_observes_half_of_a_two_document_commit() -> Result<()> {
    let node = disposable_node().await?;
    let (first_written_tx, mut first_written_rx) = watch::channel(false);
    let (release_tx, release_rx) = watch::channel(false);
    let release_on_timeout = release_tx.clone();

    let writer = ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_reader_visibility",
        move |txn: &ConfigApplyTxn<'_>| {
            let first_written_tx = first_written_tx.clone();
            let mut release_rx = release_rx.clone();
            Box::pin(async move {
                let first = ensure_no_errors(txn.execute(&write_left("visible", 1)).await?)?;
                assert_acknowledged(&first, "TxnAtomicityLeft")?;
                first_written_tx.send_replace(true);
                while !*release_rx.borrow() {
                    release_rx.changed().await?;
                }
                let second = ensure_no_errors(txn.execute(&write_right("visible", 2)).await?)?;
                assert_acknowledged(&second, "TxnAtomicityRight")?;
                Ok(())
            })
        },
    );
    let observer = async {
        while !*first_written_rx.borrow() {
            first_written_rx.changed().await?;
        }
        let read = pair_counts(&node);
        tokio::pin!(read);
        let first_poll =
            std::future::poll_fn(|cx| Poll::Ready(Pin::as_mut(&mut read).poll(cx))).await;
        let before_release = match first_poll {
            Poll::Ready(result) => Some(result),
            Poll::Pending => tokio::time::timeout(Duration::from_millis(250), read.as_mut())
                .await
                .ok(),
        };
        release_tx.send_replace(true);
        let pair = match before_release {
            Some(result) => {
                let pair = result?;
                anyhow::ensure!(
                    pair == (0, 0),
                    "reader saw uncommitted half-pair before writer release: {pair:?}"
                );
                pair
            }
            None => read.await?,
        };
        anyhow::ensure!(
            pair == (0, 0) || pair == (1, 1),
            "independent reader saw a partial pair: {pair:?}"
        );
        Ok::<_, anyhow::Error>(())
    };

    let result = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(writer, observer)
    })
    .await;
    release_on_timeout.send_replace(true);
    let (write, read) = result.context("two-document reader experiment timed out")?;
    write?;
    read?;
    assert_eq!(pair_counts(&node).await?, (1, 1));
    Ok(())
}
