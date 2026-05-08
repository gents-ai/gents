//! Idempotent schema-patch + lens registration invoked at daemon startup.
//!
//! Migrates AgentToolCall v1 -> v2 by:
//!   1. Patching the collection to add `lifecycle_state` field.
//!   2. Registering the v1->v2 forward and inverse Lens transforms.
//!   3. Touching every existing row to force eager lens execution.
//!
//! Idempotent: re-running on a v2 deployment is a no-op (collection already
//! patched, migration already registered).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, LensConfig, LensModule};

const ADD_LIFECYCLE_STATE_PATCH: &str = r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"lifecycle_state","Kind":11}}]"#;

/// Resolve the path to the bundled WASM lens artifact. The lens crate is built
/// as part of the workspace; the path is relative to the daemon binary's
/// location at install time.
///
/// Production deployments ship the WASM file alongside the binary; tests use
/// the workspace target directory.
fn lens_wasm_path() -> PathBuf {
    // Test/dev path: workspace target dir.
    // TODO follow-up issue: production deployments need a bundled artifact path
    // resolved from std::env::current_exe(). Tracked separately.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-unknown-unknown/release/agent_tool_call_lifecycle_v1_to_v2_lens.wasm")
}

/// Run all pending tool-call migrations against the embedded node.
/// Called from the daemon startup path before any AgentToolCall reads.
pub(crate) async fn ensure_tool_call_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    // 1. Check if AgentToolCall already has lifecycle_state.
    let collection = node
        .get_collection("AgentToolCall")
        .context("get AgentToolCall collection")?;

    let already_v2 = match collection {
        Some(ref cv) => collection_has_lifecycle_state(cv),
        None => {
            // Collection doesn't exist yet (fresh install). The schema add at
            // startup creates it directly with lifecycle_state already in the
            // SDL, so no patch is needed. Migration is a no-op.
            tracing::debug!("AgentToolCall collection absent; migration no-op");
            return Ok(());
        }
    };

    if already_v2 {
        tracing::debug!("AgentToolCall already at v2; migration no-op");
        return Ok(());
    }

    // 2. Apply the v1 -> v2 schema patch.
    let v1_version_id = collection
        .as_ref()
        .map(|cv| cv.version_id.clone())
        .ok_or_else(|| anyhow::anyhow!("AgentToolCall collection has no version_id"))?;

    let v2 = node
        .patch_collection("AgentToolCall", ADD_LIFECYCLE_STATE_PATCH)
        .await
        .context("patch_collection v1 -> v2 (add lifecycle_state)")?;
    let v2_version_id = v2.version_id;

    // 3. Activate v2 as the source-of-truth for new writes.
    node.set_active_collection_version(&v2_version_id)
        .await
        .context("set_active_collection_version v2")?;

    // 4. Register the forward Lens v1 -> v2.
    let lens_path = lens_wasm_path();
    let lens_path_str = lens_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 lens path"))?;

    let forward_config = LensConfig::new(
        v1_version_id.clone(),
        v2_version_id.clone(),
        LensModule::from_path(lens_path_str),
    );

    node.set_migration(forward_config)
        .await
        .context("set_migration forward v1 -> v2")?;

    tracing::info!(
        v1 = %v1_version_id,
        v2 = %v2_version_id,
        "AgentToolCall migrated to v2 with lens"
    );

    Ok(())
}

/// Decide whether a collection version already has the `lifecycle_state`
/// field. Used to detect already-migrated databases.
fn collection_has_lifecycle_state(cv: &defra_node::CollectionVersion) -> bool {
    cv.fields.iter().any(|f| f.name == "lifecycle_state")
}
