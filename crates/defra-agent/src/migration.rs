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

/// Resolve the path to the bundled WASM lens artifact for the v2->v3 subagent
/// extension migration.
fn subagent_lens_wasm_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-unknown-unknown/release/agent_subagent_v2_to_v3_lens.wasm")
}

fn collection_has_field(cv: &defra_node::CollectionVersion, field_name: &str) -> bool {
    cv.fields.iter().any(|f| f.name == field_name)
}

/// Per-collection idempotent migration orchestrator for v2->v3.
/// Applies the three subagent-extension patches and registers the unified
/// lens. Re-running after a partial failure picks up at the un-migrated
/// collection without manual intervention.
pub async fn ensure_subagent_extensions_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    // 1. AgentToolCall — patch only if v3 fields not already present.
    let atc_collection = node
        .get_collection("AgentToolCall")
        .context("get AgentToolCall collection")?;

    let atc_v3_version_id = if let Some(ref cv) = atc_collection {
        if collection_has_field(cv, "await_mode") {
            tracing::debug!("AgentToolCall already has await_mode; skipping patch");
            cv.version_id.clone()
        } else {
            let pre_version_id = cv.version_id.clone();
            let v3 = node
                .patch_collection("AgentToolCall", ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH)
                .await
                .context("patch_collection v2 -> v3 (AgentToolCall subagent fields)")?;
            let v3_version_id = v3.version_id.clone();
            node.set_active_collection_version(&v3_version_id)
                .await
                .context("set_active_collection_version v3 (AgentToolCall)")?;
            tracing::info!(
                pre = %pre_version_id,
                v3 = %v3_version_id,
                "AgentToolCall patched to v3 (subagent fields)"
            );
            v3_version_id
        }
    } else {
        tracing::debug!("AgentToolCall collection absent; subagent patch no-op");
        return Ok(());
    };

    // 2. AgentRequest — independent idempotency check.
    let ar_collection = node
        .get_collection("AgentRequest")
        .context("get AgentRequest collection")?;

    if let Some(ref cv) = ar_collection {
        if collection_has_field(cv, "caused_by_parent_request_id") {
            tracing::debug!("AgentRequest already has caused_by_parent_request_id; skipping patch");
        } else {
            let pre_version_id = cv.version_id.clone();
            let v3 = node
                .patch_collection("AgentRequest", ADD_AGENT_REQUEST_SUBAGENT_PATCH)
                .await
                .context("patch_collection v2 -> v3 (AgentRequest subagent fields)")?;
            let v3_version_id = v3.version_id.clone();
            node.set_active_collection_version(&v3_version_id)
                .await
                .context("set_active_collection_version v3 (AgentRequest)")?;
            tracing::info!(
                pre = %pre_version_id,
                v3 = %v3_version_id,
                "AgentRequest patched to v3 (subagent fields)"
            );
        }
    } else {
        tracing::debug!("AgentRequest collection absent; subagent patch no-op");
    }

    // 3. ToolSelection — independent idempotency check.
    let ts_collection = node
        .get_collection("ToolSelection")
        .context("get ToolSelection collection")?;

    if let Some(ref cv) = ts_collection {
        if collection_has_field(cv, "subagent_targets") {
            tracing::debug!("ToolSelection already has subagent_targets; skipping patch");
        } else {
            let pre_version_id = cv.version_id.clone();
            let v3 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_SUBAGENT_PATCH)
                .await
                .context("patch_collection v2 -> v3 (ToolSelection subagent fields)")?;
            let v3_version_id = v3.version_id.clone();
            node.set_active_collection_version(&v3_version_id)
                .await
                .context("set_active_collection_version v3 (ToolSelection)")?;
            tracing::info!(
                pre = %pre_version_id,
                v3 = %v3_version_id,
                "ToolSelection patched to v3 (subagent fields)"
            );
        }
    } else {
        tracing::debug!("ToolSelection collection absent; subagent patch no-op");
    }

    // 4. Register the v2→v3 forward lens for AgentToolCall only.
    //
    // AgentRequest and ToolSelection are NOT registered: their schema patches
    // are pure field additions with nullable types, so no transform is needed
    // for round-trip (a v2 client reading a v3 row sees null for unknown
    // fields; a v3 client reading a v2 row sees null for the new fields).
    // DefraDB's nullable-field semantics handle the back-compat without a
    // lens.
    //
    // AgentToolCall, by contrast, needs the lens to populate `await_mode` and
    // `cancel_policy` with their non-null defaults ("foreground", "cascade")
    // for v2 rows being read by v3 clients — the runtime expects these fields
    // to have meaningful values matching today's foreground+cascade semantics.
    // Null is not a valid runtime value for either field; the lens fills them
    // in on the forward read path.
    //
    // The WASM module (agent_subagent_v2_to_v3) retains transform logic for
    // all three collections in case future patches are non-additive, but only
    // the AgentToolCall transform is registered here via set_migration.
    let lens_path = subagent_lens_wasm_path();
    let lens_path_str = lens_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-utf8 subagent lens path"))?;

    // The "from" version is the AgentToolCall version before the patch
    // (captured above) and "to" is the v3 version we just activated.
    // Re-read the pre-patch version from the collection we already fetched.
    let atc_pre_version_id = atc_collection
        .as_ref()
        .map(|cv| cv.version_id.clone())
        .ok_or_else(|| anyhow::anyhow!("AgentToolCall collection absent after earlier check"))?;

    let forward_config = LensConfig::new(
        atc_pre_version_id.clone(),
        atc_v3_version_id.clone(),
        LensModule::from_path(lens_path_str),
    );

    node.set_migration(forward_config)
        .await
        .context("set_migration forward AgentToolCall v2 -> v3")?;

    tracing::info!(
        v2 = %atc_pre_version_id,
        v3 = %atc_v3_version_id,
        "agent_subagent_v2_to_v3 lens registered"
    );

    Ok(())
}
