//! Populated v0.6.12 store upgrade coverage for the pinned DefraDB release.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use defra_agent::defra_node::{EmbeddedNode, StorageBackend};
use serde_json::Value;

const AGENT_DID: &str = "did:key:z6Mkihm5VkTy99iTnDPQcJZC6MPZXuUQxeRN36C4UMFQEXYY";
const BACKEND_ID: &str = "did:key:z6Mkihm5VkTy99iTnDPQcJZC6MPZXuUQxeRN36C4UMFQEXYY:backend";
const BEHAVIOR_ID: &str = "did:key:z6Mkihm5VkTy99iTnDPQcJZC6MPZXuUQxeRN36C4UMFQEXYY:default";
const DEFAULT_TOOLS_ID: &str =
    "did:key:z6Mkihm5VkTy99iTnDPQcJZC6MPZXuUQxeRN36C4UMFQEXYY:default-tools";
const LEGACY_BACKEND_DOC_ID: &str = "bae-4e1c372b-d0ea-573f-a22b-0793c469b921";

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

async fn execute_ok(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = node.execute(query).await;
    ensure!(
        !response.has_errors(),
        "query failed: {:?}",
        response.errors
    );
    response.data.context("successful query returned no data")
}

#[tokio::test]
async fn populated_v0612_store_upgrades_and_reopens_without_reset() -> Result<()> {
    let directory = tempfile::tempdir().context("create upgrade fixture directory")?;
    let data_path = materialize_v0612_store(directory.path())?;

    // EmbeddedNode::build runs DefraDB's physical migration before any agent
    // schema migration. Without it, v0.15 documents are invisible through the
    // v0.16 key layout and indexed reads can fail with
    // `trailing bytes after doc short ID`.
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_path)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await
            .context("open populated v0.6.12 store with the current DefraDB pin")?,
    );
    defra_agent::migration::ensure_all_runtime_migrations(node.clone())
        .await
        .context("run the existing agent runtime migration pipeline")?;

    let state = execute_ok(
        node.as_ref(),
        r#"{
            AgentPrincipal { agent_did display_name }
            InferenceBackend { backend_id name endpoint provider_kind }
            AgentBehavior { behavior_id agent_did backend_id model_name enabled }
            ToolSelection { selection_id agent_did }
        }"#,
    )
    .await?;
    assert_eq!(state["AgentPrincipal"].as_array().map(Vec::len), Some(1));
    assert_eq!(state["AgentPrincipal"][0]["agent_did"], AGENT_DID);
    assert_eq!(state["InferenceBackend"].as_array().map(Vec::len), Some(1));
    assert_eq!(state["InferenceBackend"][0]["backend_id"], BACKEND_ID);
    assert_eq!(state["AgentBehavior"].as_array().map(Vec::len), Some(1));
    assert_eq!(state["AgentBehavior"][0]["behavior_id"], BEHAVIOR_ID);
    ensure!(
        state["ToolSelection"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["selection_id"] == DEFAULT_TOOLS_ID)),
        "default ToolSelection did not survive the upgrade: {state}"
    );

    let commits = execute_ok(
        node.as_ref(),
        &format!(
            r#"query {{ _commits(docID: "{LEGACY_BACKEND_DOC_ID}") {{ cid docID height }} }}"#
        ),
    )
    .await?;
    ensure!(
        commits["_commits"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "legacy backend DocID did not resolve to its migrated history: {commits}"
    );

    let updated = execute_ok(
        node.as_ref(),
        &format!(
            r#"mutation {{
                update_InferenceBackend(
                    filter: {{ backend_id: {{ _eq: "{BACKEND_ID}" }} }},
                    input: {{ name: "migration fixture verified" }}
                ) {{ backend_id name }}
            }}"#
        ),
    )
    .await?;
    assert_eq!(
        updated["update_InferenceBackend"][0]["name"],
        "migration fixture verified"
    );

    node.shutdown().await;
    drop(node);

    let reopened = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_path)
            .with_storage_backend(StorageBackend::RocksDb)
            .build()
            .await
            .context("reopen migrated v0.6.12 store")?,
    );
    defra_agent::migration::ensure_all_runtime_migrations(reopened.clone())
        .await
        .context("rerun runtime migrations idempotently")?;
    let after_reopen = execute_ok(
        reopened.as_ref(),
        &format!(
            r#"{{
                InferenceBackend(filter: {{ backend_id: {{ _eq: "{BACKEND_ID}" }} }}) {{
                    backend_id
                    name
                }}
            }}"#
        ),
    )
    .await?;
    assert_eq!(
        after_reopen["InferenceBackend"][0]["name"],
        "migration fixture verified"
    );
    reopened.shutdown().await;

    Ok(())
}
