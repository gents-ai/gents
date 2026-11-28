//! Bounded native external-premise experiment: real multi-document
//! transaction atomicity against the pinned embedded DefraDB node, through
//! `ConfigAccess::transact_local_write`. Commit/rollback evidence only,
//! including that a duplicate key on a single-field or a composite unique
//! index errors with "violates unique index" and, when the caller propagates
//! that error, none of the transaction's writes persists: no claim about host
//! crash durability or remote P2P merge. A blocked outside read is permitted;
//! a partial pair is not.

use anyhow::{bail, Context, Result};
use defra_node::Node as EmbeddedNode;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};
use tokio::sync::{watch, Barrier};
use tokio::task::JoinSet;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;

const ATOMICITY_SCHEMA: &str = r#"
type TxnAtomicityLeft { label: String amount: Int }
type TxnAtomicityRight { label: String amount: Int }
type TxnAtomicityUnique { key: String @index(unique: true) label: String }
type TxnAtomicityNote @index(fields: ["chain_root_doc_id", "note_id"], unique: true) { chain_root_doc_id: String note_id: String label: String }
"#;

async fn disposable_node() -> Result<Arc<EmbeddedNode>> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::node_store().render(ATOMICITY_SCHEMA),
    )
    .await?;
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
    let row = crate::graphql::single_mutation_document(&response, &field)?
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
    let response = crate::graphql::graphql_with_transaction_retry(
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

/// Commit, rollback, and unique-index backstop premises on the DefraDB rev
/// pinned in workspace `Cargo.toml` (`013c3441e5d6`), verified on one shared
/// disposable node:
/// - Injected errors, failed second mutations, and single-field or composite
///   unique-index violations roll back earlier writes in the transaction.
/// - Two concurrent read-then-insert transactions on one unique key leave one
///   row (`tla/SharedOwnerHotCounter.tla`).
/// - Reading an existing unique key skips insert while committing another write.
#[tokio::test]
async fn transaction_atomicity_rollback_and_unique_index_premises() -> Result<()> {
    let node = disposable_node().await?;

    let injected: Result<()> = ConfigAccess::transact_local_write(
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
    assert!(injected
        .expect_err("the adapter must surface the injected error")
        .to_string()
        .contains("injected failure after first write"));
    assert_eq!(counts(&node).await?, (0, 0));

    let failed_second: Result<()> = ConfigAccess::transact_local_write(
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
    let error = format!(
        "{:#}",
        failed_second.expect_err("propagating the failed second mutation must fail")
    );
    assert!(
        error.contains("TxnAtomicityMissing"),
        "unexpected failure: {error}"
    );
    assert_eq!(counts(&node).await?, (0, 0));

    let unique_abort: Result<()> = ConfigAccess::transact_local_write(
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
        unique_abort.expect_err("a duplicate unique-key insert must fail")
    );
    assert!(
        error.contains("violates unique index"),
        "the unique index must reject the duplicate key: {error}"
    );
    assert_eq!(collection_count(&node, "TxnAtomicityLeft").await?, 0);
    assert_eq!(collection_count(&node, "TxnAtomicityUnique").await?, 0);

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

    let composite_abort: Result<()> = ConfigAccess::transact_local_write(
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
        composite_abort.expect_err("a duplicate composite-key insert must fail")
    );
    assert!(
        error.contains("violates unique index"),
        "the composite unique index must reject the duplicate pair: {error}"
    );
    assert_eq!(collection_count(&node, "TxnAtomicityLeft").await?, 0);
    assert_eq!(collection_count(&node, "TxnAtomicityNote").await?, 3);

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
    assert_eq!(counts(&node).await?, (1, 1));

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

    let inserted_existing = ConfigAccess::transact_local_write(
        &node,
        None,
        "txn_atomicity_existing_key",
        |txn: &ConfigApplyTxn<'_>| {
            Box::pin(async move {
                let inserted = read_then_insert_unique(txn, "contended").await?;
                ensure_no_errors(txn.execute(&write_left("other-write", 1)).await?)?;
                Ok::<_, anyhow::Error>(inserted)
            })
        },
    )
    .await?;
    assert!(
        !inserted_existing,
        "the existing key must be found, not inserted"
    );
    assert_eq!(collection_count(&node, "TxnAtomicityLeft").await?, 2);
    assert_eq!(collection_count(&node, "TxnAtomicityUnique").await?, 1);
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

fn report(name: &str, value: Value) -> Result<()> {
    if let Some(dir) = std::env::var_os("GENTS_WRITER_REPORT_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value)?,
        )?;
    }
    Ok(())
}

const SCHEMA: &str = "type WriterCounter { key: String @index(unique: true) value: Int }
type WriterReceipt { writer: Int iteration: Int }";

async fn node() -> Result<(tempfile::TempDir, Arc<EmbeddedNode>)> {
    let home = tempfile::tempdir()?;
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(home.path().join("db"))
            .build()
            .await?,
    );
    ConfigAccess::local(node.clone())
        .add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(SCHEMA))
        .await?;
    Ok((home, node))
}

/// Every acknowledged increment is durable with its receipt, a returned
/// conflict applied nothing, and every call returns (`tla/SharedOwnerHotCounter
/// .tla`: `TestAccounting`, `EveryCallSettles`). A wave has no wall-clock
/// bound: the owner bounds attempts and each native phase, not the time the
/// host takes to schedule them (`WaveDeadline` fails with every safety
/// property holding), so a hang surfaces as the owner's phase-timeout error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_owner_hot_counter_has_no_lost_updates_at_c64() -> Result<()> {
    let (_home, node) = node().await?;
    let access = ConfigAccess::local(node.clone());
    access
        .write(
            "writer_probe.seed",
            r#"mutation { create_WriterCounter(input: {key: "hot", value: 0}) {_docID} }"#,
        )
        .await?;
    let mut measurements = Vec::new();
    let mut committed = BTreeSet::new();
    let mut next_writer = 0i64;
    for width in [1, 2, 4, 8, 16, 32, 64] {
        let barrier = Arc::new(Barrier::new(width));
        let mut workers = JoinSet::new();
        let started = Instant::now();
        for writer in 0..width {
            let writer = next_writer + writer as i64;
            let access = ConfigAccess::local(node.clone());
            let barrier = barrier.clone();
            workers.spawn(async move {
                barrier.wait().await;
                let mut observations = Vec::new();
                for iteration in 0..8 {
                    let started = Instant::now();
                    let result = access.transact_write("writer_probe.increment", |txn| Box::pin(async move {
                        let row = txn.execute("{ WriterCounter {value} }").await?;
                        let value = row["data"]["WriterCounter"][0]["value"].as_i64().context("counter absent")?;
                        tokio::task::yield_now().await;
                        txn.execute(&format!(r#"mutation {{ update_WriterCounter(filter: {{key: {{_eq: "hot"}}}}, input: {{value: {}}}) {{_docID}} create_WriterReceipt(input: {{writer: {writer}, iteration: {iteration}}}) {{_docID}} }}"#, value + 1)).await?;
                        Ok::<_, anyhow::Error>(())
                    })).await;
                    let accepted = match result {
                        Ok(()) => true,
                        Err(error) if error.to_string() == "DefraDB transaction conflict" => false,
                        Err(error) => return Err(error),
                    };
                    observations.push(((writer, iteration), accepted, started.elapsed().as_secs_f64() * 1000.0));
                }
                Ok::<_, anyhow::Error>(observations)
            });
        }
        next_writer += width as i64;
        let mut observations = Vec::new();
        while let Some(worker) = workers.join_next().await {
            observations.extend(worker??);
        }
        assert_eq!(observations.len(), width * 8);
        let accepted = observations.iter().filter(|row| row.1).count();
        assert!(accepted > 0, "at least one hot-counter update commits");
        committed.extend(observations.iter().filter(|row| row.1).map(|row| row.0));
        let after = access
            .execute("{ WriterCounter {value} WriterReceipt {writer iteration} }")
            .await?;
        assert_eq!(
            after["data"]["WriterCounter"][0]["value"],
            committed.len() as i64
        );
        let rows = after["data"]["WriterReceipt"].as_array().unwrap();
        let durable: BTreeSet<_> = rows
            .iter()
            .map(|row| {
                (
                    row["writer"].as_i64().unwrap(),
                    row["iteration"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            durable, committed,
            "every accepted update has its atomic receipt"
        );
        assert_eq!(rows.len(), committed.len(), "no duplicate receipts");
        let mut latency: Vec<_> = observations.iter().map(|row| row.2).collect();
        latency.sort_by(f64::total_cmp);
        measurements.push(json!({"concurrency": width, "attempts": width * 8, "committed": accepted, "typed_conflicts": width * 8 - accepted, "elapsed_ms": started.elapsed().as_secs_f64()*1000.0, "p50_ms": latency[latency.len()/2], "p95_ms": latency[(latency.len()-1)*95/100], "max_ms":latency.last()}));
    }
    report(
        "shared-owner",
        json!({"measurements": measurements, "counter":committed.len()}),
    )?;
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

/// Native API experiment: bypass the Gents gate only here to measure the
/// database's own admission and commit behavior for overlapping transactions.
/// The round asserts progress, not time: first-committer-wins commits at
/// least one writer of every round of single-attempt transactions
/// (`tla/SharedOwnerHotCounter.tla`, `Native`: `RoundProgress`), and a round
/// that ends with none fails with each writer's phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_c64_distinct_writers_expose_conflicts_without_partial_commits() -> Result<()> {
    let (_home, node) = node().await?;
    let barrier = Arc::new(Barrier::new(64));
    let mut workers = JoinSet::new();
    let started = Instant::now();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let phases = Arc::new(std::sync::Mutex::new(vec!["waiting"; 64]));
    for writer in 0..64 {
        let node = node.clone();
        let barrier = barrier.clone();
        let active = active.clone();
        let maximum = maximum.clone();
        let phases = phases.clone();
        workers.spawn(async move {
            let phase = |name| phases.lock().unwrap_or_else(|e| e.into_inner())[writer] = name;
            barrier.wait().await;
            let transaction = node.begin_transaction_guard(false).await?;
            phase("begun");
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            tokio::task::yield_now().await;
            let response = transaction.execute(&format!("mutation {{ create_WriterReceipt(input: {{writer: {writer}, iteration: 0}}) {{_docID}} }}")).await;
            anyhow::ensure!(!response.has_errors(), "native write: {:?}", response.errors);
            phase("committing");
            let committed = match transaction.commit().await {
                Ok(()) => true,
                Err(query::TransactionError::Conflict(_)) => false,
                Err(error) => return Err(error.into()),
            };
            phase(if committed { "committed" } else { "conflict" });
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>(committed.then_some(writer as i64))
        });
    }
    let mut committed = BTreeSet::new();
    while let Some(worker) = workers.join_next().await {
        if let Some(writer) = worker?? {
            committed.insert(writer);
        }
    }
    anyhow::ensure!(
        !committed.is_empty(),
        "a native round ended with no commit; writer phases: {:?}",
        phases.lock().unwrap_or_else(|e| e.into_inner())
    );
    let rows = ConfigAccess::local(node.clone())
        .execute("{ WriterReceipt {writer} }")
        .await?;
    let receipts = rows["data"]["WriterReceipt"].as_array().unwrap();
    let observed: BTreeSet<_> = receipts
        .iter()
        .map(|row| row["writer"].as_i64().unwrap())
        .collect();
    assert_eq!(observed, committed);
    assert_eq!(receipts.len(), committed.len());
    assert!(
        maximum.load(Ordering::SeqCst) > 1,
        "native transactions did not overlap"
    );
    report(
        "native-distinct",
        json!({"concurrency":64,"committed":committed.len(),"typed_conflicts":64-committed.len(),"max_open_transactions":maximum.load(Ordering::SeqCst),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}),
    )?;
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
