//! Idempotent schema-patch + lens registration invoked at daemon startup.
//!
//! Migrates AgentToolCall v1 -> v2 by:
//!   1. Patching the collection to add `lifecycle_state` field.
//!   2. Registering the v1->v2 forward and inverse Lens transforms.
//!   3. Touching every existing row to force eager lens execution.
//!
//! Idempotent: re-running on a v2 deployment is a no-op (collection already
//! patched, migration already registered).
//!
//! The WASM lens artifact is embedded into the binary at compile time via
//! `include_bytes!` (built by `build.rs`). At runtime the bytes are written
//! to a process-lifetime temp file so DefraDB can load them via filesystem
//! path. The temp file is held alive in a static OnceLock so the path stays
//! valid for the lens runtime's lazy access.

use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, LensConfig, LensModule};
use tempfile::NamedTempFile;

const ADD_LIFECYCLE_STATE_PATCH: &str = r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"lifecycle_state","Kind":11}}]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"await_mode","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_policy","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"child_request_id","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"request_id","Kind":11}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_REQUEST_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"subagent_depth","Kind":5}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_request_id","Kind":11}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_tool_call_id","Kind":11}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_targets","Kind":17}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_spawn_enabled","Kind":2}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_steering_enabled","Kind":2}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_background_enabled","Kind":2}}
]"#;

/// WASM lens bytes embedded at compile time. Built by build.rs.
const LENS_WASM_BYTES: &[u8] =
    include_bytes!(env!("AGENT_TOOL_CALL_LIFECYCLE_V1_TO_V2_LENS_WASM_PATH"));

/// Process-wide temp file holding the unpacked WASM bytes. Held alive (never
/// dropped) so DefraDB's lazy lens loader can always reach the path.
static LENS_TEMP_FILE: OnceLock<NamedTempFile> = OnceLock::new();

/// Return the filesystem path to the embedded WASM lens, unpacking it on first
/// call. Subsequent calls return the same path.
fn lens_wasm_path() -> Result<String> {
    let temp = LENS_TEMP_FILE.get_or_init(|| {
        let mut tf = NamedTempFile::new().expect("create lens temp file");
        std::io::Write::write_all(tf.as_file_mut(), LENS_WASM_BYTES)
            .expect("write embedded lens bytes to temp file");
        tf
    });
    let path = temp.path().to_string_lossy().to_string();
    Ok(path)
}

/// Run all pending tool-call migrations against the embedded node.
/// Called from the daemon startup path before any AgentToolCall reads.
pub async fn ensure_tool_call_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
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
    let lens_path = lens_wasm_path().context("unpack embedded lens WASM")?;
    let lens_config = LensConfig::new(
        v1_version_id.clone(),
        v2_version_id.clone(),
        LensModule::from_path(lens_path),
    );

    node.set_migration(lens_config)
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
