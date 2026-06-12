//! Idempotent schema-patch + lens registration invoked at daemon startup.
//!
//! Migrates AgentToolCall v1 -> current by:
//!   1. Patching the collection to add `lifecycle_state` field.
//!   2. Registering the v1->v2 forward and inverse Lens transforms.
//!   3. Touching every existing row to force eager lens execution.
//!   4. Patching later runtime fields that do not need row transforms.
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
use tempfile::{Builder, NamedTempFile};

const ADD_LIFECYCLE_STATE_PATCH: &str = r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"lifecycle_state","Kind":11}}]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"await_mode","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_policy","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"child_request_id","Kind":11}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_REQUEST_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"subagent_depth","Kind":5}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_request_id","Kind":11}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_tool_call_id","Kind":11}}
]"#;

// Kind 21 == ScalarArrayKind::NillableStringArray in defradb.rs. The SDL
// `[String]` (nullable elements) for these fields compiles to that kind, so the
// migration patch must use it. The previous value of 17 was a stale Go-DefraDB
// field-kind number that is unassigned in defradb.rs's enum, so the SDL builder
// treated the patch as a named-type reference and failed with
// "no type found for given name. Kind: 17" — crash-looping every store upgrade.
#[allow(dead_code)]
const ADD_TOOL_SELECTION_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_targets","Kind":21}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_spawn_enabled","Kind":2}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_steering_enabled","Kind":2}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_background_enabled","Kind":2}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_BACKGROUND_TOOLS_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"backgroundable_tool_names","Kind":21}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_R5_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"unclaimed_deadline_at","Kind":10}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_cascade_intent_at","Kind":10}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_pending_remote_ack","Kind":2}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"stuck_since","Kind":10}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_COMMAND_DENIAL_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denial_reason","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argv","Kind":21}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_command","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argument","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_subcommand","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_prefix","Kind":21}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_mode","Kind":11}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_network","Kind":11}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_R5_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"cross_deployment_spawn_timeout_seconds","Kind":5}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_SESSION_HISTORY_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"enable_session_history_tool","Kind":2}}
]"#;

const ADD_TOOL_SELECTION_DEFAULT_AWAIT_MODE_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_default_await_mode","Kind":11}}
]"#;

const ADD_PEER_PAIRING_DESIRED_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"agent_did","Kind":11}}
]"#;

const ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"profiles","Kind":21}}
]"#;

// Kind 11 == NillableString in defradb.rs. SDL `String` (nullable) for these
// fields compiles to that kind. AgentBehavior gained `description` and `summary`
// on branch design/issue-377; existing DBs upgraded from a prior schema version
// must have these fields patched in so that reads/writes referencing them do not
// fail with "unknown field" errors.
#[allow(dead_code)]
const ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH: &str = r#"[
    {"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"description","Kind":11}},
    {"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"summary","Kind":11}}
]"#;

const ADD_TOOL_SERVICE_REGISTRY_SEND_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/ToolServiceRegistry/Fields/-","value":{"Name":"send_agent_did","Kind":2}}
]"#;

const ADD_TOOL_SERVICE_HEALTH_STATE_TOOL_COUNT_PATCH: &str = r#"[
    {"op":"add","path":"/ToolServiceHealthState/Fields/-","value":{"Name":"tool_count","Kind":5}}
]"#;

const ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH: &str = r#"[
    {"op":"add","path":"/AgentRuntime/Fields/-","value":{"Name":"behavior_executor_capacity","Kind":5}},
    {"op":"add","path":"/AgentRuntime/Fields/-","value":{"Name":"behavior_executor_queue_depth","Kind":5}},
    {"op":"add","path":"/AgentRuntime/Fields/-","value":{"Name":"behavior_executor_status_json","Kind":11}}
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
    // DefraDB's lens loader validates that the module path ends with `.wasm`,
    // so the temp file must be created with that suffix.
    let temp = LENS_TEMP_FILE.get_or_init(|| {
        let mut tf = Builder::new()
            .suffix(".wasm")
            .tempfile()
            .expect("create lens temp file");
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
    // 1. Check which AgentToolCall runtime fields already exist.
    let mut collection = node
        .get_collection("AgentToolCall")
        .context("get AgentToolCall collection")?;

    let has_lifecycle_state = match collection {
        Some(ref cv) => collection_has_lifecycle_state(cv),
        None => {
            // Collection doesn't exist yet (fresh install). The schema add at
            // startup creates it directly with lifecycle_state already in the
            // SDL, so no patch is needed. Migration is a no-op.
            tracing::debug!("AgentToolCall collection absent; migration no-op");
            return Ok(());
        }
    };

    if !has_lifecycle_state {
        // 2. Apply the v1 -> v2 schema patch.
        let v1_version_id = collection
            .as_ref()
            .map(|cv| cv.version_id.clone())
            .ok_or_else(|| anyhow::anyhow!("AgentToolCall collection has no version_id"))?;

        let v2 = node
            .patch_collection("AgentToolCall", ADD_LIFECYCLE_STATE_PATCH)
            .await
            .context("patch_collection v1 -> v2 (add lifecycle_state)")?;
        let v2_version_id = v2.version_id.clone();

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

        collection = Some(v2);
    }

    let Some(collection) = collection.as_ref() else {
        return Ok(());
    };

    let mut field_patches = Vec::new();
    if !collection_has_field(collection, "request_id") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"request_id","Kind":11}}"#,
        );
    }
    if !collection_has_field(collection, "deadline_at") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"deadline_at","Kind":10}}"#,
        );
    }
    if !collection_has_field(collection, "cancel_cause") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_cause","Kind":11}}"#,
        );
    }
    for (field, kind) in [
        ("denial_reason", 11),
        ("denied_argv", 21),
        ("denied_command", 11),
        ("denied_argument", 11),
        ("denied_subcommand", 11),
        ("denied_prefix", 21),
        ("policy_mode", 11),
        ("policy_network", 11),
    ] {
        if !collection_has_field(collection, field) {
            field_patches.push(match kind {
                21 => match field {
                    "denied_argv" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argv","Kind":21}}"#
                    }
                    "denied_prefix" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_prefix","Kind":21}}"#
                    }
                    _ => unreachable!("unexpected AgentToolCall array field {field}"),
                },
                11 => match field {
                    "denial_reason" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denial_reason","Kind":11}}"#
                    }
                    "denied_command" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_command","Kind":11}}"#
                    }
                    "denied_argument" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argument","Kind":11}}"#
                    }
                    "denied_subcommand" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_subcommand","Kind":11}}"#
                    }
                    "policy_mode" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_mode","Kind":11}}"#
                    }
                    "policy_network" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_network","Kind":11}}"#
                    }
                    _ => unreachable!("unexpected AgentToolCall string field {field}"),
                },
                _ => unreachable!("unexpected AgentToolCall command-denial field kind {kind}"),
            });
        }
    }

    if field_patches.is_empty() {
        tracing::debug!("AgentToolCall already has all runtime lifecycle fields; migration no-op");
        return Ok(());
    }

    let patch = format!("[{}]", field_patches.join(","));
    let next = node
        .patch_collection("AgentToolCall", &patch)
        .await
        .context("patch_collection add AgentToolCall runtime fields")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version AgentToolCall runtime fields")?;

    tracing::info!(
        version = %next.version_id,
        fields = ?field_patches,
        "AgentToolCall migrated with runtime lifecycle fields"
    );

    Ok(())
}

/// Decide whether a collection version already has the `lifecycle_state`
/// field. Used to detect already-migrated databases.
fn collection_has_lifecycle_state(cv: &defra_node::CollectionVersion) -> bool {
    collection_has_field(cv, "lifecycle_state")
}

fn collection_has_field(cv: &defra_node::CollectionVersion, field_name: &str) -> bool {
    cv.fields.iter().any(|f| f.name == field_name)
}

/// WASM lens bytes for the v2->v3 subagent extension. Embedded at compile time
/// via build.rs.
const SUBAGENT_LENS_WASM_BYTES: &[u8] =
    include_bytes!(env!("AGENT_SUBAGENT_V2_TO_V3_LENS_WASM_PATH"));

/// Process-wide temp file holding the unpacked subagent WASM bytes. Held alive
/// (never dropped) so DefraDB's lazy lens loader can always reach the path.
static SUBAGENT_LENS_TEMP_FILE: OnceLock<NamedTempFile> = OnceLock::new();

/// Return the filesystem path to the embedded subagent WASM lens, unpacking it
/// on first call. Subsequent calls return the same path.
fn subagent_lens_wasm_path() -> Result<String> {
    // DefraDB's lens loader validates that the module path ends with `.wasm`,
    // so the temp file must be created with that suffix.
    let temp = SUBAGENT_LENS_TEMP_FILE.get_or_init(|| {
        let mut tf = Builder::new()
            .suffix(".wasm")
            .tempfile()
            .expect("create subagent lens temp file");
        std::io::Write::write_all(tf.as_file_mut(), SUBAGENT_LENS_WASM_BYTES)
            .expect("write embedded subagent lens bytes to temp file");
        tf
    });
    let path = temp.path().to_string_lossy().to_string();
    Ok(path)
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

    let mut atc_pre_version_id_for_lens = None;
    let mut active_atc_collection = atc_collection.clone();
    let atc_v3_version_id = if let Some(ref cv) = active_atc_collection {
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
            atc_pre_version_id_for_lens = Some(pre_version_id);
            active_atc_collection = Some(v3.clone());
            v3_version_id
        }
    } else {
        tracing::debug!("AgentToolCall collection absent; subagent patch no-op");
        return Ok(());
    };

    if let Some(ref cv) = active_atc_collection {
        if collection_has_field(cv, "unclaimed_deadline_at") {
            tracing::debug!("AgentToolCall already has R5 cross-deployment fields; skipping patch");
        } else {
            let pre_version_id = cv.version_id.clone();
            let v4 = node
                .patch_collection("AgentToolCall", ADD_AGENT_TOOL_CALL_R5_PATCH)
                .await
                .context("patch_collection AgentToolCall R5 cross-deployment fields")?;
            node.set_active_collection_version(&v4.version_id)
                .await
                .context("set_active_collection_version AgentToolCall R5 fields")?;
            tracing::info!(
                pre = %pre_version_id,
                v4 = %v4.version_id,
                "AgentToolCall patched with R5 cross-deployment fields"
            );
        }
    }

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
        let mut active_version = cv.clone();
        if collection_has_field(&active_version, "subagent_targets") {
            tracing::debug!("ToolSelection already has subagent_targets; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
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
            active_version = v3;
        }

        if collection_has_field(&active_version, "backgroundable_tool_names") {
            tracing::debug!("ToolSelection already has backgroundable_tool_names; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v4 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_BACKGROUND_TOOLS_PATCH)
                .await
                .context("patch_collection v3 -> v4 (ToolSelection background tool fields)")?;
            let v4_version_id = v4.version_id.clone();
            node.set_active_collection_version(&v4_version_id)
                .await
                .context("set_active_collection_version v4 (ToolSelection)")?;
            tracing::info!(
                pre = %pre_version_id,
                v4 = %v4_version_id,
                "ToolSelection patched to v4 (background tool fields)"
            );
            active_version = v4;
        }

        if collection_has_field(&active_version, "cross_deployment_spawn_timeout_seconds") {
            tracing::debug!(
                "ToolSelection already has R5 cross-deployment timeout; skipping patch"
            );
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v5 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_R5_PATCH)
                .await
                .context("patch_collection ToolSelection R5 timeout field")?;
            node.set_active_collection_version(&v5.version_id)
                .await
                .context("set_active_collection_version ToolSelection R5 timeout")?;
            tracing::info!(
                pre = %pre_version_id,
                v5 = %v5.version_id,
                "ToolSelection patched with R5 cross-deployment timeout"
            );
            active_version = v5;
        }

        if collection_has_field(&active_version, "enable_session_history_tool") {
            tracing::debug!("ToolSelection already has session history flag; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v6 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_SESSION_HISTORY_PATCH)
                .await
                .context("patch_collection ToolSelection session history tool flag")?;
            node.set_active_collection_version(&v6.version_id)
                .await
                .context("set_active_collection_version ToolSelection session history flag")?;
            tracing::info!(
                pre = %pre_version_id,
                v6 = %v6.version_id,
                "ToolSelection patched with session history tool flag"
            );
            active_version = v6;
        }

        if collection_has_field(&active_version, "subagent_default_await_mode") {
            tracing::debug!(
                "ToolSelection already has subagent default await mode; skipping patch"
            );
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v7 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_DEFAULT_AWAIT_MODE_PATCH)
                .await
                .context("patch_collection ToolSelection subagent default await mode")?;
            node.set_active_collection_version(&v7.version_id)
                .await
                .context("set_active_collection_version ToolSelection default await mode")?;
            tracing::info!(
                pre = %pre_version_id,
                v7 = %v7.version_id,
                "ToolSelection patched with subagent default await mode"
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
    let Some(atc_pre_version_id) = atc_pre_version_id_for_lens else {
        tracing::debug!("AgentToolCall subagent fields already present; lens registration no-op");
        return Ok(());
    };

    let lens_path = subagent_lens_wasm_path().context("unpack embedded subagent lens WASM")?;

    let forward_config = LensConfig::new(
        atc_pre_version_id.clone(),
        atc_v3_version_id.clone(),
        LensModule::from_path(lens_path),
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

pub async fn ensure_peer_pairing_desired_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    ensure_peer_pairing_applied_schema(node.as_ref()).await?;

    let Some(mut collection) = node
        .get_collection("PeerPairingDesired")
        .context("get PeerPairingDesired collection")?
    else {
        return Ok(());
    };

    if !collection_has_field(&collection, "agent_did") {
        let next = node
            .patch_collection(
                "PeerPairingDesired",
                ADD_PEER_PAIRING_DESIRED_AGENT_DID_PATCH,
            )
            .await
            .context("patch_collection PeerPairingDesired agent_did")?;
        node.set_active_collection_version(&next.version_id)
            .await
            .context("set_active_collection_version PeerPairingDesired agent_did")?;
        tracing::info!(
            version = %next.version_id,
            "PeerPairingDesired patched with agent_did field"
        );
        collection = next;
    }

    if !collection_has_field(&collection, "profiles") {
        let next = node
            .patch_collection(
                "PeerPairingDesired",
                ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH,
            )
            .await
            .context("patch_collection PeerPairingDesired profiles")?;
        node.set_active_collection_version(&next.version_id)
            .await
            .context("set_active_collection_version PeerPairingDesired profiles")?;
        tracing::info!(
            version = %next.version_id,
            "PeerPairingDesired patched with profiles field"
        );
    }

    Ok(())
}

async fn ensure_peer_pairing_applied_schema(node: &EmbeddedNode) -> Result<()> {
    if node
        .get_collection("PeerPairingApplied")
        .context("get PeerPairingApplied collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::PEER_PAIRING_APPLIED)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add PeerPairingApplied schema"),
    }
}

pub async fn ensure_tool_service_registry_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    let Some(collection) = node
        .get_collection("ToolServiceRegistry")
        .context("get ToolServiceRegistry collection")?
    else {
        return Ok(());
    };
    if collection_has_field(&collection, "send_agent_did") {
        return Ok(());
    }

    let next = node
        .patch_collection(
            "ToolServiceRegistry",
            ADD_TOOL_SERVICE_REGISTRY_SEND_AGENT_DID_PATCH,
        )
        .await
        .context("patch_collection ToolServiceRegistry send_agent_did")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version ToolServiceRegistry send_agent_did")?;
    tracing::info!(
        version = %next.version_id,
        "ToolServiceRegistry patched with send_agent_did field"
    );
    Ok(())
}

/// Idempotent migration for AgentBehavior: adds `description` and `summary`
/// fields (Kind 11, nullable String) introduced on branch issue-377, plus
/// `skill_refs` and `skill_excludes` (Kind 21, `[String]`) introduced by the
/// Skills feature (#340). Existing DBs upgraded from a prior schema version
/// need these fields patched in so that GraphQL reads/writes referencing them
/// do not fail.
pub async fn ensure_agent_behavior_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    let Some(collection) = node
        .get_collection("AgentBehavior")
        .context("get AgentBehavior collection")?
    else {
        return Ok(());
    };

    // Build the list of fields we need to add (only those absent).
    let mut field_patches: Vec<&str> = Vec::new();
    if !collection_has_field(&collection, "description") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"description","Kind":11}}"#,
        );
    }
    if !collection_has_field(&collection, "summary") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"summary","Kind":11}}"#,
        );
    }
    // `skill_refs` and `skill_excludes` are selected by the AgentBehavior load
    // query; an old-schema DB missing either fails reads with
    // `Cannot query field "..."`. Kind 21 is `[String]`, matching
    // `subagent_targets`.
    if !collection_has_field(&collection, "skill_refs") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"skill_refs","Kind":21}}"#,
        );
    }
    if !collection_has_field(&collection, "skill_excludes") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"skill_excludes","Kind":21}}"#,
        );
    }

    if field_patches.is_empty() {
        tracing::debug!(
            "AgentBehavior already has description, summary, skill_refs, and skill_excludes fields; migration no-op"
        );
        return Ok(());
    }

    let patch = format!("[{}]", field_patches.join(","));
    let next = node
        .patch_collection("AgentBehavior", &patch)
        .await
        .context("patch_collection AgentBehavior description+summary+skill fields")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version AgentBehavior description+summary+skill fields")?;
    tracing::info!(
        version = %next.version_id,
        fields = ?field_patches,
        "AgentBehavior patched with description, summary, skill_refs, and skill_excludes fields"
    );
    Ok(())
}

pub async fn ensure_tool_service_health_state_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    let Some(collection) = node
        .get_collection("ToolServiceHealthState")
        .context("get ToolServiceHealthState collection")?
    else {
        return Ok(());
    };
    if collection_has_field(&collection, "tool_count") {
        return Ok(());
    }

    let next = node
        .patch_collection(
            "ToolServiceHealthState",
            ADD_TOOL_SERVICE_HEALTH_STATE_TOOL_COUNT_PATCH,
        )
        .await
        .context("patch_collection ToolServiceHealthState tool_count")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version ToolServiceHealthState tool_count")?;
    tracing::info!(
        version = %next.version_id,
        "ToolServiceHealthState patched with tool_count field"
    );
    Ok(())
}

pub async fn ensure_agent_runtime_executor_status_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    let Some(collection) = node
        .get_collection("AgentRuntime")
        .context("get AgentRuntime collection")?
    else {
        return Ok(());
    };
    if collection_has_field(&collection, "behavior_executor_status_json") {
        return Ok(());
    }

    let next = node
        .patch_collection("AgentRuntime", ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH)
        .await
        .context("patch_collection AgentRuntime executor status fields")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version AgentRuntime executor status fields")?;
    tracing::info!(
        version = %next.version_id,
        "AgentRuntime patched with behavior executor status fields"
    );
    Ok(())
}

#[cfg(test)]
mod patch_kind_tests {
    use super::*;
    use serde::Deserialize;

    // defradb.rs ScalarArrayKind::NillableStringArray. SDL `[String]` (nullable
    // elements) compiles to this; migration patches adding such fields must match.
    const NILLABLE_STRING_ARRAY_KIND: i64 = 21;
    const OLD_PEER_PAIRING_DESIRED_SCHEMA: &str = r#"
        type PeerPairingDesired {
            peer_id: String @index(unique: true)
            agent_did: String @index
            collections: [String!]!
            replicator_addresses: [String!]!
            created_at: DateTime @index(direction: DESC)
            updated_at: DateTime @index(direction: DESC)
        }
    "#;

    async fn test_node() -> Arc<EmbeddedNode> {
        Arc::new(EmbeddedNode::builder().build().await.unwrap())
    }

    fn field_kinds(patch_json: &str) -> Vec<(String, i64)> {
        let ops: serde_json::Value = serde_json::from_str(patch_json).expect("patch is valid JSON");
        ops.as_array()
            .expect("patch is an array")
            .iter()
            .filter_map(|op| {
                let value = op.get("value")?;
                let name = value.get("Name")?.as_str()?.to_string();
                let kind = value.get("Kind")?.as_i64()?;
                Some((name, kind))
            })
            .collect()
    }

    #[test]
    fn tool_selection_string_array_fields_use_nillable_string_array_kind() {
        for (name, kind) in field_kinds(ADD_TOOL_SELECTION_SUBAGENT_PATCH) {
            if name == "subagent_targets" {
                assert_eq!(
                    kind, NILLABLE_STRING_ARRAY_KIND,
                    "subagent_targets must be NillableStringArray (21), got {kind}"
                );
            }
        }
        for (name, kind) in field_kinds(ADD_TOOL_SELECTION_BACKGROUND_TOOLS_PATCH) {
            assert_eq!(
                name, "backgroundable_tool_names",
                "unexpected field in background-tools patch"
            );
            assert_eq!(
                kind, NILLABLE_STRING_ARRAY_KIND,
                "backgroundable_tool_names must be NillableStringArray (21), got {kind}"
            );
        }
        for (name, kind) in field_kinds(ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH) {
            assert_eq!(name, "profiles", "unexpected field in profiles patch");
            assert_eq!(
                kind, NILLABLE_STRING_ARRAY_KIND,
                "profiles must be NillableStringArray (21), got {kind}"
            );
        }
    }

    #[test]
    fn agent_tool_call_command_denial_string_arrays_use_nillable_string_array_kind() {
        for (name, kind) in field_kinds(ADD_AGENT_TOOL_CALL_COMMAND_DENIAL_PATCH) {
            if name == "denied_argv" || name == "denied_prefix" {
                assert_eq!(
                    kind, NILLABLE_STRING_ARRAY_KIND,
                    "{name} must be NillableStringArray (21), got {kind}"
                );
            }
        }
    }

    #[test]
    fn no_patch_uses_the_unassigned_kind_17() {
        // 17 is unassigned in defradb.rs's FieldKind enum; the SDL builder treats
        // it as a named-type reference and fails with "no type found. Kind: 17".
        for patch in [
            ADD_LIFECYCLE_STATE_PATCH,
            ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH,
            ADD_AGENT_REQUEST_SUBAGENT_PATCH,
            ADD_TOOL_SELECTION_SUBAGENT_PATCH,
            ADD_TOOL_SELECTION_BACKGROUND_TOOLS_PATCH,
            ADD_AGENT_TOOL_CALL_R5_PATCH,
            ADD_AGENT_TOOL_CALL_COMMAND_DENIAL_PATCH,
            ADD_TOOL_SELECTION_R5_PATCH,
            ADD_TOOL_SELECTION_SESSION_HISTORY_PATCH,
            ADD_TOOL_SELECTION_DEFAULT_AWAIT_MODE_PATCH,
            ADD_PEER_PAIRING_DESIRED_AGENT_DID_PATCH,
            ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH,
            ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH,
            ADD_TOOL_SERVICE_HEALTH_STATE_TOOL_COUNT_PATCH,
            ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH,
        ] {
            for (name, kind) in field_kinds(patch) {
                assert_ne!(kind, 17, "field {name} uses unassigned Kind 17");
            }
        }
    }

    #[test]
    fn agent_behavior_description_summary_use_nillable_string_kind() {
        // Kind 11 == NillableString. Both `description` and `summary` are
        // nullable String fields in the AgentBehavior SDL; patches must match.
        const NILLABLE_STRING_KIND: i64 = 11;
        for (name, kind) in field_kinds(ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH) {
            assert_eq!(
                kind, NILLABLE_STRING_KIND,
                "AgentBehavior field '{name}' must be NillableString (11), got {kind}"
            );
        }
    }

    #[test]
    fn agent_runtime_executor_status_field_kinds_match_sdl() {
        let fields = field_kinds(ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH);
        assert_eq!(
            fields,
            vec![
                ("behavior_executor_capacity".to_string(), 5),
                ("behavior_executor_queue_depth".to_string(), 5),
                ("behavior_executor_status_json".to_string(), 11),
            ]
        );
    }

    #[tokio::test]
    async fn peer_pairing_migration_ensures_applied_and_desired_profiles() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        ensure_peer_pairing_desired_migrations(node.clone())
            .await
            .unwrap();
        ensure_peer_pairing_desired_migrations(node.clone())
            .await
            .unwrap();

        let desired = node
            .get_collection("PeerPairingDesired")
            .unwrap()
            .expect("PeerPairingDesired collection");
        assert!(collection_has_field(&desired, "agent_did"));
        assert!(collection_has_field(&desired, "profiles"));

        let applied = node
            .get_collection("PeerPairingApplied")
            .unwrap()
            .expect("PeerPairingApplied collection");
        assert!(collection_has_field(&applied, "collections"));
        assert!(collection_has_field(&applied, "replicator_addresses"));
    }

    #[derive(Debug, Deserialize)]
    struct PairingDesiredRow {
        peer_id: String,
        collections: Option<Vec<String>>,
        replicator_addresses: Option<Vec<String>>,
        profiles: Option<Vec<String>>,
    }

    #[tokio::test]
    async fn peer_pairing_profiles_migration_preserves_existing_rows() {
        let node = test_node().await;
        node.add_schema(OLD_PEER_PAIRING_DESIRED_SCHEMA)
            .await
            .unwrap();
        let response = node
            .execute(
                r#"mutation {
                    create_PeerPairingDesired(input: {
                        peer_id: "peer-b",
                        agent_did: "did:defra-agent:peer-b",
                        collections: ["AgentRequest"],
                        replicator_addresses: ["/ip4/127.0.0.1/tcp/4101/p2p/peer-b"],
                        created_at: "2026-06-12T00:00:00Z",
                        updated_at: "2026-06-12T00:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "create old PeerPairingDesired row failed: {:?}",
            response.errors
        );

        ensure_peer_pairing_desired_migrations(node.clone())
            .await
            .unwrap();
        ensure_peer_pairing_desired_migrations(node.clone())
            .await
            .unwrap();

        let desired = node
            .get_collection("PeerPairingDesired")
            .unwrap()
            .expect("PeerPairingDesired collection");
        assert!(collection_has_field(&desired, "profiles"));
        assert!(
            node.get_collection("PeerPairingApplied").unwrap().is_some(),
            "PeerPairingApplied collection should be added"
        );

        let response = node
            .execute(
                r#"{
                    PeerPairingDesired(filter: { peer_id: { _eq: "peer-b" } }) {
                        peer_id
                        collections
                        replicator_addresses
                        profiles
                    }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "query migrated PeerPairingDesired failed: {:?}",
            response.errors
        );
        let rows: Vec<PairingDesiredRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get("PeerPairingDesired"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].peer_id, "peer-b");
        assert_eq!(
            rows[0].collections.as_deref(),
            Some(&["AgentRequest".to_string()][..])
        );
        assert_eq!(
            rows[0].replicator_addresses.as_deref(),
            Some(&["/ip4/127.0.0.1/tcp/4101/p2p/peer-b".to_string()][..])
        );
        assert!(rows[0].profiles.is_none());
    }
}
