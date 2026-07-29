//! Populated v0.6.12 store upgrade coverage for the pinned DefraDB release.
//!
//! Store-format open still succeeds. The gents lens-first engine rejects the
//! pre-baseline multi-version lineage with [`gents_migration::Error::UnknownLineage`]
//! (or ForeignVersion) — no legacy field-presence migrations remain.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use gents::defra_node::{EmbeddedNode, StorageBackend};

fn materialize_v0612_store(root: &Path) -> Result<PathBuf> {
    let encoded: String =
        include_str!("../fixtures/defradb_store_upgrade/v0612_populated_rocksdb.tar.zst.b64")
            .split_whitespace()
            .collect();
    let compressed = BASE64_STANDARD
        .decode(encoded)
        .context("decode v0.6.12 RocksDB fixture")?;
    let decoder = zstd::stream::Decoder::new(compressed.as_slice())
        .context("decompress v0.6.12 RocksDB fixture")?;
    let data_path = root.join("data");
    fs::create_dir_all(&data_path).context("create fixture data directory")?;
    tar::Archive::new(decoder)
        .unpack(&data_path)
        .context("unpack v0.6.12 RocksDB fixture")?;
    Ok(data_path)
}

#[tokio::test]
async fn populated_v0612_store_opens_but_rejects_pre_baseline_lineage() -> Result<()> {
    let directory = tempfile::tempdir().context("create upgrade fixture directory")?;
    let data_path = materialize_v0612_store(directory.path())?;

    // EmbeddedNode::build runs DefraDB's physical migration before any gents
    // schema work. The fixture must open under the current pin.
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_path)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await
            .context("open populated v0.6.12 store with the current DefraDB pin")?,
    );

    let err = gents::migration::ensure_all_runtime_migrations(node.clone())
        .await
        .expect_err("pre-baseline multi-version DAGs must hard-fail (no legacy support)");
    let msg = format!("{err:#}");
    ensure!(
        msg.contains("unknown lineage")
            || msg.contains("UnknownLineage")
            || msg.contains("foreign version")
            || msg.contains("ForeignVersion")
            || msg.contains("export/import"),
        "expected UnknownLineage/ForeignVersion diagnostic, got: {msg}"
    );

    node.shutdown().await;
    Ok(())
}
