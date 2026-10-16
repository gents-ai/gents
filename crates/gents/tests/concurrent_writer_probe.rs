use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use serde_json::{json, Value};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

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
    ConfigAccess::local(node.clone()).add_schema(SCHEMA).await?;
    Ok((home, node))
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
    for width in [1, 2, 4, 8, 16, 32, 64] {
        let before = access.execute("{ WriterCounter {value} }").await?["data"]["WriterCounter"][0]
            ["value"]
            .as_i64()
            .unwrap();
        let barrier = Arc::new(Barrier::new(width));
        let mut workers = JoinSet::new();
        let started = Instant::now();
        for writer in 0..width {
            let writer = before + writer as i64;
            let access = ConfigAccess::local(node.clone());
            let barrier = barrier.clone();
            workers.spawn(async move {
                barrier.wait().await;
                let mut latencies = Vec::new();
                for iteration in 0..8 {
                    let started = Instant::now();
                    access.transact_write("writer_probe.increment", |txn| Box::pin(async move {
                        let row = txn.execute("{ WriterCounter {value} }").await?;
                        let value = row["data"]["WriterCounter"][0]["value"].as_i64().context("counter absent")?;
                        tokio::task::yield_now().await;
                        txn.execute(&format!(r#"mutation {{ update_WriterCounter(filter: {{key: {{_eq: "hot"}}}}, input: {{value: {}}}) {{_docID}} create_WriterReceipt(input: {{writer: {writer}, iteration: {iteration}}}) {{_docID}} }}"#, value + 1)).await?;
                        Ok(())
                    })).await?;
                    latencies.push(started.elapsed().as_secs_f64() * 1000.0);
                }
                Ok::<_, anyhow::Error>(latencies)
            });
        }
        let mut latency = tokio::time::timeout(Duration::from_secs(60), async {
            let mut latency = Vec::new();
            while let Some(worker) = workers.join_next().await {
                latency.extend(worker??);
            }
            Ok::<_, anyhow::Error>(latency)
        })
        .await
        .context("shared owner writers stalled")??;
        let after = access
            .execute("{ WriterCounter {value} WriterReceipt {writer iteration} }")
            .await?;
        assert_eq!(
            after["data"]["WriterCounter"][0]["value"],
            before + (width * 8) as i64
        );
        assert_eq!(
            after["data"]["WriterReceipt"].as_array().unwrap().len(),
            (before as usize) + width * 8
        );
        latency.sort_by(f64::total_cmp);
        measurements.push(json!({"concurrency": width, "transactions": width * 8, "elapsed_ms": started.elapsed().as_secs_f64()*1000.0, "p50_ms": latency[latency.len()/2], "p95_ms": latency[(latency.len()-1)*95/100], "max_ms":latency.last()}));
    }
    report(
        "shared-owner",
        json!({"measurements": measurements, "counter":1016}),
    )?;
    node.shutdown().await;
    Ok(())
}

/// Native API experiment: bypass the Gents gate only here to measure the
/// database's own admission and commit behavior for overlapping transactions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_c64_distinct_writers_expose_conflicts_without_partial_commits() -> Result<()> {
    let (_home, node) = node().await?;
    let barrier = Arc::new(Barrier::new(64));
    let mut workers = JoinSet::new();
    let started = Instant::now();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    for writer in 0..64 {
        let node = node.clone();
        let barrier = barrier.clone();
        let active = active.clone();
        let maximum = maximum.clone();
        workers.spawn(async move {
            barrier.wait().await;
            let transaction = node.begin_transaction_guard(false).await?;
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(current, Ordering::SeqCst);
            tokio::task::yield_now().await;
            let response = transaction.execute(&format!("mutation {{ create_WriterReceipt(input: {{writer: {writer}, iteration: 0}}) {{_docID}} }}")).await;
            anyhow::ensure!(!response.has_errors(), "native write: {:?}", response.errors);
            let committed = match transaction.commit().await {
                Ok(()) => true,
                Err(query::TransactionError::Conflict(_)) => false,
                Err(error) => return Err(error.into()),
            };
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, anyhow::Error>(committed.then_some(writer as i64))
        });
    }
    let committed = tokio::time::timeout(Duration::from_secs(60), async {
        let mut committed = BTreeSet::new();
        while let Some(worker) = workers.join_next().await {
            if let Some(writer) = worker?? {
                committed.insert(writer);
            }
        }
        Ok::<_, anyhow::Error>(committed)
    })
    .await
    .context("native concurrent writers stalled")??;
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
    assert!(!committed.is_empty());
    assert!(
        maximum.load(Ordering::SeqCst) > 1,
        "native transactions did not overlap"
    );
    report(
        "native-distinct",
        json!({"concurrency":64,"committed":committed.len(),"typed_conflicts":64-committed.len(),"max_open_transactions":maximum.load(Ordering::SeqCst),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0}),
    )?;
    node.shutdown().await;
    Ok(())
}
