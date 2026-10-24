use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use serde_json::{json, Value};
use tokio::sync::{Barrier, Semaphore};
use tokio::task::JoinSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type Result<T = (), E = BoxError> = std::result::Result<T, E>;

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
                        let value = row["data"]["WriterCounter"][0]["value"].as_i64().ok_or("counter absent")?;
                        tokio::task::yield_now().await;
                        txn.execute(&format!(r#"mutation {{ update_WriterCounter(filter: {{key: {{_eq: "hot"}}}}, input: {{value: {}}}) {{_docID}} create_WriterReceipt(input: {{writer: {writer}, iteration: {iteration}}}) {{_docID}} }}"#, value + 1)).await?;
                        Ok::<_, BoxError>(())
                    })).await;
                    let accepted = match result {
                        Ok(()) => true,
                        Err(error) if error.to_string() == "DefraDB transaction conflict" => false,
                        Err(error) => return Err(error),
                    };
                    observations.push(((writer, iteration), accepted, started.elapsed().as_secs_f64() * 1000.0));
                }
                Ok::<_, BoxError>(observations)
            });
        }
        next_writer += width as i64;
        let observations = tokio::time::timeout(Duration::from_secs(60), async {
            let mut observations = Vec::new();
            while let Some(worker) = workers.join_next().await {
                observations.extend(worker??);
            }
            Ok::<_, BoxError>(observations)
        })
        .await
        .map_err(|_| "shared owner writers stalled")??;
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
            if response.has_errors() {
                return Err(format!("native write: {:?}", response.errors).into());
            }
            let committed = match transaction.commit().await {
                Ok(()) => true,
                Err(query::TransactionError::Conflict(_)) => false,
                Err(error) => return Err(error.into()),
            };
            active.fetch_sub(1, Ordering::SeqCst);
            Ok::<_, BoxError>(committed.then_some(writer as i64))
        });
    }
    let committed = tokio::time::timeout(Duration::from_secs(60), async {
        let mut committed = BTreeSet::new();
        while let Some(worker) = workers.join_next().await {
            if let Some(writer) = worker?? {
                committed.insert(writer);
            }
        }
        Ok::<_, BoxError>(committed)
    })
    .await
    .map_err(|_| "native concurrent writers stalled")??;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "persistent encrypted admission experiment; emits optional JSON measurements"]
async fn encrypted_branchable_writers_at_varying_admission_widths() -> Result<()> {
    measure_encrypted_admission(64, &[1, 2, 4, 8, 16, 32, 64], "native-admission-sweep").await
}

async fn measure_encrypted_admission(
    writers: usize,
    capacities: &[usize],
    report_name: &str,
) -> Result<()> {
    const ITERATIONS: usize = 4;
    let mut measurements = Vec::new();
    for &capacity in capacities {
        let home = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(home.path().join("db"))
                .with_at_rest_encryption_key([0x29; 32])
                .build()
                .await?,
        );
        ConfigAccess::local(node.clone())
            .add_schema("type AdmissionDocument @branchable { writer: Int @immutable iteration: Int @immutable body: String @immutable }")
            .await?;
        let admission = Arc::new(Semaphore::new(capacity));
        let barrier = Arc::new(Barrier::new(writers));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut workers = JoinSet::new();
        let started = Instant::now();
        for writer in 0..writers {
            let node = node.clone();
            let admission = admission.clone();
            let barrier = barrier.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            workers.spawn(async move {
                barrier.wait().await;
                let mut observations = Vec::new();
                for iteration in 0..ITERATIONS {
                    let requested = Instant::now();
                    let permit = admission.acquire().await?;
                    let admitted = Instant::now();
                    let transaction = node.begin_transaction_guard(false).await?;
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(count, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    let body = gents::graphql::escape_graphql_string(&"x".repeat(1024));
                    let response = transaction
                        .execute(&format!(
                            "mutation {{ create_AdmissionDocument(input: {{writer: {writer}, iteration: {iteration}, body: \"{body}\"}}) {{_docID}} }}"
                        ))
                        .await;
                    let committed = if response.is_transaction_conflict() {
                        drop(transaction);
                        false
                    } else {
                        if response.has_errors() {
                            return Err(format!("native execute: {:?}", response.errors).into());
                        }
                        match transaction.commit().await {
                            Ok(()) => true,
                            Err(query::TransactionError::Conflict(_)) => false,
                            Err(error) => return Err(error.into()),
                        }
                    };
                    active.fetch_sub(1, Ordering::SeqCst);
                    let service_ms = admitted.elapsed().as_secs_f64() * 1000.0;
                    drop(permit);
                    observations.push((
                        (writer as i64, iteration as i64),
                        committed,
                        requested.elapsed().as_secs_f64() * 1000.0,
                        service_ms,
                    ));
                }
                Ok::<_, BoxError>(observations)
            });
        }
        let observations = tokio::time::timeout(Duration::from_secs(120), async {
            let mut all = Vec::new();
            while let Some(worker) = workers.join_next().await {
                all.extend(worker??);
            }
            Ok::<_, BoxError>(all)
        })
        .await
        .map_err(|_| "admitted native writers stalled")??;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let committed: BTreeSet<_> = observations
            .iter()
            .filter(|row| row.1)
            .map(|row| row.0)
            .collect();
        let rows = ConfigAccess::local(node.clone())
            .execute("{ AdmissionDocument {writer iteration} }")
            .await?;
        let rows = rows["data"]["AdmissionDocument"]
            .as_array()
            .ok_or("documents missing")?;
        let durable: BTreeSet<_> = rows
            .iter()
            .map(|row| -> Result<(i64, i64)> {
                Ok((
                    row["writer"].as_i64().ok_or("writer missing")?,
                    row["iteration"].as_i64().ok_or("iteration missing")?,
                ))
            })
            .collect::<Result<_>>()?;
        assert_eq!(observations.len(), writers * ITERATIONS);
        assert_eq!(
            durable, committed,
            "durable identities at admission {capacity}"
        );
        assert_eq!(
            rows.len(),
            committed.len(),
            "duplicate or partial commits at admission {capacity}"
        );
        assert!(!committed.is_empty(), "no commits at admission {capacity}");
        let overlap = maximum.load(Ordering::SeqCst);
        assert!(overlap <= capacity);
        if capacity == 1 {
            assert_eq!(overlap, 1);
        } else {
            assert!(
                overlap > 1,
                "admission {capacity} did not overlap transactions"
            );
        }
        let mut latency: Vec<_> = observations.iter().map(|row| row.2).collect();
        let mut service: Vec<_> = observations.iter().map(|row| row.3).collect();
        latency.sort_by(f64::total_cmp);
        service.sort_by(f64::total_cmp);
        measurements.push(json!({
            "admission": capacity,
            "workers": writers,
            "attempts": observations.len(),
            "committed": committed.len(),
            "typed_conflicts": observations.len() - committed.len(),
            "elapsed_ms": elapsed_ms,
            "commits_per_second": committed.len() as f64 * 1000.0 / elapsed_ms,
            "p50_total_ms": latency[latency.len()/2],
            "p95_total_ms": latency[(latency.len()-1)*95/100],
            "p50_service_ms": service[service.len()/2],
            "p95_service_ms": service[(service.len()-1)*95/100],
            "max_open_transactions": overlap,
            "durable_identity_set_matches_commits": true
        }));
        report(
            report_name,
            json!({"measurements": measurements, "retries": false}),
        )?;
        node.shutdown().await;
    }
    Ok(())
}
