//! Bounded native external-premise experiment: real multi-document
//! transaction atomicity against the pinned embedded DefraDB node, through
//! `ConfigAccess::transact_local_write`. Commit/rollback evidence only,
//! including that a duplicate key on a single-field or a composite unique
//! index errors with "violates unique index" and, when the caller propagates
//! that error, none of the transaction's writes persists: no claim about host
//! crash durability or remote P2P merge. A blocked outside read is permitted;
//! a partial pair is not.

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

const ATOMICITY_SCHEMA: &str = r#"
type TxnAtomicityLeft { label: String amount: Int }
type TxnAtomicityRight { label: String amount: Int }
type TxnAtomicityUnique { key: String @index(unique: true) label: String }
type TxnAtomicityNote @index(fields: ["chain_root_doc_id", "note_id"], unique: true) { chain_root_doc_id: String note_id: String label: String }
"#;

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
                Ok::<_, anyhow::Error>((left, right))
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
                Ok::<_, anyhow::Error>(())
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

/// `label` is outside the indexed field so documents sharing a key still get
/// distinct content-addressed docIDs; only the unique index can reject them.
fn write_unique(key: &str, label: &str) -> String {
    format!(
        r#"mutation {{ create_TxnAtomicityUnique(input: {{ key: "{}", label: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(key),
        escape_graphql_string(label)
    )
}

/// `label` is outside the index so rows sharing the key pair still get
/// distinct docIDs; only the composite unique index can reject them.
fn write_note(chain_root_doc_id: &str, note_id: &str, label: &str) -> String {
    format!(
        r#"mutation {{ create_TxnAtomicityNote(input: {{ chain_root_doc_id: "{}", note_id: "{}", label: "{}" }}) {{ _docID }} }}"#,
        escape_graphql_string(chain_root_doc_id),
        escape_graphql_string(note_id),
        escape_graphql_string(label)
    )
}

/// Read the key inside the transaction and insert it only when absent.
async fn read_then_insert_unique(txn: &ConfigApplyTxn<'_>, key: &str) -> Result<bool> {
    let read = ensure_no_errors(
        txn.execute(&format!(
            r#"query {{ TxnAtomicityUnique(filter: {{ key: {{ _eq: "{}" }} }}) {{ key }} }}"#,
            escape_graphql_string(key)
        ))
        .await?,
    )?;
    let found = read["data"]["TxnAtomicityUnique"]
        .as_array()
        .context("unique read omitted collection")?
        .len();
    if found > 0 {
        return Ok(false);
    }
    ensure_no_errors(txn.execute(&write_unique(key, "inserted")).await?)?;
    Ok(true)
}

/// Premise for read-then-insert with a unique index as the backstop, on the
/// DefraDB rev pinned in the workspace `Cargo.toml`
/// (`013c3441e5d6`): a duplicate insert of a
/// unique-indexed key errors with "violates unique index", and the error,
/// propagated out of `transact_local_write`, rolls back the whole transaction, so
/// neither the duplicate nor an earlier write in it persists. DefraDB rejects
/// the insert; the rollback comes from propagating the error, not from
/// DefraDB aborting the transaction itself.
#[tokio::test]
async fn duplicate_unique_key_errors_and_propagated_error_rolls_back() -> Result<()> {
    let node = disposable_node().await?;

    let outcome: Result<()> = ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_unique_violation",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                ensure_no_errors(txn.execute(&write_left("unique-abort", 1)).await?)?;
                ensure_no_errors(txn.execute(&write_unique("same", "first")).await?)?;
                ensure_no_errors(txn.execute(&write_unique("same", "second")).await?)?;
                Ok(())
            })
        },
    )
    .await;

    let error = format!(
        "{:#}",
        outcome.expect_err("a duplicate unique-key insert must fail")
    );
    assert!(
        error.contains("violates unique index"),
        "the unique index must reject the duplicate key: {error}"
    );
    assert_eq!(collection_count(&node, "TxnAtomicityLeft").await?, 0);
    assert_eq!(collection_count(&node, "TxnAtomicityUnique").await?, 0);
    Ok(())
}

/// The composite form of the premise above, on the same pinned DefraDB rev:
/// a duplicate insert on the composite `(chain_root_doc_id, note_id)` unique
/// index errors with "violates unique index", and the error, propagated out
/// of `transact_local_write`, rolls back the transaction, so neither its earlier
/// writes nor the duplicate persists. Rows that share only one of the two
/// fields are accepted.
#[tokio::test]
async fn duplicate_composite_note_key_errors_and_propagated_error_rolls_back() -> Result<()> {
    let node = disposable_node().await?;

    ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_seed_composite_notes",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                ensure_no_errors(txn.execute(&write_note("root-a", "note-1", "seed")).await?)?;
                ensure_no_errors(txn.execute(&write_note("root-a", "note-2", "seed")).await?)?;
                ensure_no_errors(txn.execute(&write_note("root-b", "note-1", "seed")).await?)?;
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await?;
    assert_eq!(collection_count(&node, "TxnAtomicityNote").await?, 3);

    let outcome: Result<()> = ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_composite_violation",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                ensure_no_errors(txn.execute(&write_left("composite-abort", 1)).await?)?;
                ensure_no_errors(
                    txn.execute(&write_note("root-c", "note-1", "fresh"))
                        .await?,
                )?;
                ensure_no_errors(
                    txn.execute(&write_note("root-a", "note-1", "duplicate"))
                        .await?,
                )?;
                Ok(())
            })
        },
    )
    .await;

    let error = format!(
        "{:#}",
        outcome.expect_err("a duplicate composite-key insert must fail")
    );
    assert!(
        error.contains("violates unique index"),
        "the composite unique index must reject the duplicate pair: {error}"
    );
    assert_eq!(collection_count(&node, "TxnAtomicityLeft").await?, 0);
    assert_eq!(collection_count(&node, "TxnAtomicityNote").await?, 3);
    Ok(())
}

/// Two concurrent read-then-insert transactions on one unique key leave one
/// row. `transact_local_write` serializes writers through the per-node mutation
/// gate, so the second read always sees the first commit: the read is the
/// guard on this path and the unique index is not reached. Same pinned
/// DefraDB rev as `duplicate_unique_key_errors_and_propagated_error_rolls_back`.
#[tokio::test]
async fn concurrent_read_then_insert_leaves_one_row() -> Result<()> {
    let node = disposable_node().await?;
    let attempt = || {
        ConfigAccess::transact_local_write(
            &node,
            None,
            "txn_atomicity_concurrent_read_then_insert",
            |txn: &ConfigApplyTxn<'_>| {
                Box::pin(async move { read_then_insert_unique(txn, "contended").await })
            },
        )
    };

    let (first, second) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(attempt(), attempt())
    })
    .await
    .context("concurrent read-then-insert timed out")?;

    let inserted = [first?, second?];
    assert_eq!(
        inserted.iter().filter(|inserted| **inserted).count(),
        1,
        "exactly one transaction inserts: {inserted:?}"
    );
    assert_eq!(collection_count(&node, "TxnAtomicityUnique").await?, 1);
    Ok(())
}

/// A read-then-insert of an existing key inserts nothing, so the other write
/// in the same transaction commits. Same pinned DefraDB rev as above.
#[tokio::test]
async fn read_then_insert_of_existing_key_commits_the_other_write() -> Result<()> {
    let node = disposable_node().await?;
    ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_seed_existing_key",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                ensure_no_errors(txn.execute(&write_unique("existing", "seed")).await?)?;
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await?;

    let inserted = ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_existing_key",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                let inserted = read_then_insert_unique(txn, "existing").await?;
                ensure_no_errors(txn.execute(&write_left("other-write", 1)).await?)?;
                Ok::<_, anyhow::Error>(inserted)
            })
        },
    )
    .await?;

    assert!(!inserted, "the existing key must be found, not inserted");
    assert_eq!(collection_count(&node, "TxnAtomicityLeft").await?, 1);
    assert_eq!(collection_count(&node, "TxnAtomicityUnique").await?, 1);
    Ok(())
}
