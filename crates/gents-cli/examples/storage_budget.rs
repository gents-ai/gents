use std::alloc::{GlobalAlloc, Layout};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, ensure, Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::store_key::{StoreEncryption, StoreKeyCustodyChoice};

struct MeasuredAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn allocated(size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

/// Every pointer and layout stays with MiMalloc; failed reallocations leave the
/// original allocation live. Accounting must not allocate, which would recurse
/// into this allocator. The counters exclude native-library allocations and
/// allocator overhead.
unsafe impl GlobalAlloc for MeasuredAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { mimalloc::MiMalloc.alloc(layout) };
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { mimalloc::MiMalloc.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { mimalloc::MiMalloc.realloc(ptr, layout, size) };
        if !result.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            allocated(size);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: MeasuredAllocator = MeasuredAllocator;

/// Encrypted persistent-node workload with sequential durable writes,
/// transcript-sized values, bounded indexed reads and retained-home recovery.
/// Run each profile in a fresh process/home; allocator retention crosses cases.
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("warn,storage_budget=info")
        .with_ansi(false)
        .with_target(false)
        .init();
    let args: Vec<String> = std::env::args().collect();
    ensure!(
        args.len() == 5,
        "storage_budget PROFILE FRESH_HOME WRITES PAYLOAD_BYTES; profiles: local, server, embedded, 8mib, 16mib, 32mib"
    );
    let profile = &args[1];
    let home = PathBuf::from(&args[2]);
    let writes: usize = args[3].parse()?;
    let bytes: usize = args[4].parse()?;
    ensure!(writes > 0 && bytes > 0, "positive workload sizes required");
    ensure!(!home.exists(), "fresh benchmark home required");
    std::fs::create_dir_all(&home)?;
    let data = home.join("data");
    let key_file = home.join("store.key");
    let metadata = home.join("encryption.json");
    let record = StoreEncryption::prepare(StoreKeyCustodyChoice::File, &key_file, &data)?;
    std::fs::write(&metadata, serde_json::to_vec(&record)?)?;
    let key = record.initialize(&key_file, &data)?;
    let mut options = match profile.as_str() {
        "server" => storage::RegolithStoreOptions::default(),
        "embedded" => storage::RegolithStoreOptions::embedded(),
        "local" => gents::storage_backend::regolith_options(),
        "8mib" | "16mib" | "32mib" => {
            let mut options = gents::storage_backend::regolith_options();
            options.engine.write_buffer_size = profile
                .strip_suffix("mib")
                .context("memtable profile")?
                .parse::<usize>()?
                * 1024
                * 1024;
            options
        }
        _ => bail!("unknown profile"),
    };
    tracing::info!(
        profile,
        write_buffer_bytes = options.engine.write_buffer_size,
        max_write_buffers = options.engine.max_write_buffer_number,
        block_cache_bytes = options.engine.block_cache_size,
        max_key_bytes = options.engine.max_key_size,
        max_value_bytes = options.engine.max_value_size,
        background_compactions = options.engine.max_background_compactions,
        "storage budget profile"
    );
    let statistics = options
        .engine
        .statistics
        .get_or_insert_with(|| Arc::new(Default::default()))
        .clone();
    let build = || {
        gents::store_key::persistent_builder(&data, &key)
            .map(|builder| builder.with_regolith_options(options.clone()))
    };
    let node = Arc::new(build()?.build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let access = ConfigAccess::Local(node.clone());
    access
        .add_schema("type StorageBudgetRecord { ordinal: Int @index payload: String }")
        .await?;
    let start = Instant::now();
    let mut latencies = Vec::with_capacity(writes);
    let mut read_us = 0;
    let mut state = 0x1234_5678_9abc_def0_u64;
    let mut last_payload = String::new();
    let mut last_doc_id = String::new();
    for ordinal in 0..writes {
        let payload: String = (0..bytes)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                char::from(b'a' + (state % 26) as u8)
            })
            .collect();
        let mutation = format!(
            "mutation {{ create_StorageBudgetRecord(input: {{ ordinal: {ordinal}, payload: \"{}\" }}) {{ _docID }} }}",
            escape_graphql_string(&payload),
        );
        let write_start = Instant::now();
        let inserted = access
            .write("benchmark.storage_budget", &mutation)
            .await
            .with_context(|| format!("profile {profile}, payload {bytes}, write {ordinal}"))?;
        latencies.push(write_start.elapsed().as_micros() as u64);
        if ordinal + 1 == writes {
            last_doc_id =
                gents_protocol::graphql::extract_mutation_doc_id(&inserted, "StorageBudgetRecord")?;
            last_payload = payload;
        }
        if ordinal % 16 == 0 {
            let read_start = Instant::now();
            let result = access.execute(&format!(
                "{{ StorageBudgetRecord(filter: {{ ordinal: {{ _ge: {} }} }}, limit: 16) {{ ordinal payload }} }}",
                ordinal.saturating_sub(15),
            )).await?;
            let rows = result["data"]["StorageBudgetRecord"]
                .as_array()
                .context("read rows")?;
            ensure!(
                rows.len() == (ordinal + 1).min(16),
                "bounded read row count"
            );
            for row in rows {
                let read_ordinal = row["ordinal"].as_u64().context("read ordinal")? as usize;
                ensure!(
                    read_ordinal >= ordinal.saturating_sub(15) && read_ordinal <= ordinal,
                    "bounded read ordinal"
                );
                ensure!(
                    row["payload"].as_str().context("read payload")?.len() == bytes,
                    "bounded read payload length"
                );
            }
            read_us += read_start.elapsed().as_micros();
        }
        if ordinal % 1000 == 0 {
            tracing::info!(
                profile,
                ordinal,
                elapsed_ms = start.elapsed().as_millis(),
                live_bytes = LIVE.load(Ordering::Relaxed),
                peak_bytes = PEAK.load(Ordering::Relaxed),
                "storage budget progress"
            );
        }
    }
    let elapsed = start.elapsed();
    latencies.sort_unstable();
    tracing::info!(
        profile,
        writes,
        bytes,
        elapsed_ms = elapsed.as_millis(),
        writes_per_second = writes as f64 / elapsed.as_secs_f64(),
        p50_us = latencies[writes / 2],
        p95_us = latencies[(writes * 95 / 100).min(writes - 1)],
        p99_us = latencies[(writes * 99 / 100).min(writes - 1)],
        read_us,
        live_bytes = LIVE.load(Ordering::Relaxed),
        peak_bytes = PEAK.load(Ordering::Relaxed),
        "storage budget result"
    );
    drop(access);
    node.shutdown().await;
    drop(node);
    let reopened = Arc::new(build()?.build().await?);
    let result = ConfigAccess::Local(reopened.clone())
        .execute(&format!(
            "{{ StorageBudgetRecord(filter: {{ ordinal: {{ _eq: {} }} }}) {{ _docID ordinal payload }} }}",
            writes - 1,
        ))
        .await?;
    let rows = result["data"]["StorageBudgetRecord"]
        .as_array()
        .context("reopened rows")?;
    ensure!(rows.len() == 1, "unique durable last write");
    ensure!(
        rows[0]["_docID"].as_str() == Some(last_doc_id.as_str()),
        "durable document ID"
    );
    ensure!(
        rows[0]["payload"].as_str() == Some(last_payload.as_str()),
        "durable payload"
    );
    tracing::info!(profile, statistics = %statistics.dump(), "storage budget recovery verified");
    reopened.shutdown().await;
    Ok(())
}
