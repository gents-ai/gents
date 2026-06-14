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

const ADD_PEER_PAIRING_DESIRED_SOURCE_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"source","Kind":11}}
]"#;

const ADD_PEER_PAIRING_DESIRED_TEMPLATE_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"template","Kind":11}}
]"#;

const ADD_PEER_PAIRING_APPLIED_REPLICATOR_FILTER_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingApplied/Fields/-","value":{"Name":"replicator_filter","Kind":11}}
]"#;

const ADD_PEER_REGISTRY_TEMPLATES_PATCH: &str = r#"[
    {"op":"add","path":"/PeerRegistry/Fields/-","value":{"Name":"templates","Kind":21}}
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

// Kind 11 == NillableString. The `agent_did` scope key denormalizes the owning
// agent onto the four conversation collections that key on `session_id` and
// historically lacked it (AgentMessage, AgentToolCall, AgentSession,
// CompactionEntry). The field is `@immutable` in the SDL: it is logically
// write-once (stamped from the session owner at create), which lets filtered
// replication (#1033) scope each collection to one agent's DID — and #1033
// REJECTS any replication-filter field that is not immutable. The patch must
// therefore carry `"Immutable":true`: `FieldDescription.immutable` deserializes
// from that key, and adding a brand-new field has no prior values to violate
// immutability, so this stays an ordinary additive patch.
const ADD_AGENT_MESSAGE_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/AgentMessage/Fields/-","value":{"Name":"agent_did","Kind":11,"Immutable":true}}
]"#;
const ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"agent_did","Kind":11,"Immutable":true}}
]"#;
const ADD_AGENT_SESSION_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"agent_did","Kind":11,"Immutable":true}}
]"#;
const ADD_COMPACTION_ENTRY_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/CompactionEntry/Fields/-","value":{"Name":"agent_did","Kind":11,"Immutable":true}}
]"#;

// The other four conversation collections (AgentRequest, AgentResponse,
// AgentToolResult, AgentConversation) already carried `agent_did` as a plain
// `@index` field; this version's SDL adds `@immutable`. A FRESH database is
// created immutable from the SDL, but an UPGRADED database keeps the existing
// MUTABLE field — `add_schema` short-circuits on an existing collection, and
// defradb's schema patcher REJECTS changing any property of an existing field
// (`validate_field_not_mutated`: `new_field != old_field` ⇒ "mutating an existing
// field is not supported"; this mirrors Go DefraDB's `validateFieldNotMutated`,
// which `reflect.DeepEqual`s the whole field).
//
// Flipping a field to immutable is NOT an additive change: `@immutable` (a
// defradb.rs-only concept — Go DefraDB has no write-once field) is enforced at
// DAG-merge time, so an existing document that already has multiple writes to
// the field in its history would retroactively violate the invariant. A safe
// flip therefore needs defradb to scan each document's history and prove a
// single write first — an upstream feature, not an in-place patch. Until that
// lands, the migration only DETECTS the stale shape and warns, so the operator
// can see that filtered replication of the conversation template is unavailable
// on this upgraded node (defradb rejects the non-immutable scope filter at
// `add_replicator`). Fresh databases are immutable from the SDL and unaffected.
const PRE_EXISTING_AGENT_DID_COLLECTIONS: [&str; 4] = [
    "AgentRequest",
    "AgentResponse",
    "AgentToolResult",
    "AgentConversation",
];

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

fn field_is_immutable(cv: &defra_node::CollectionVersion, field_name: &str) -> bool {
    cv.fields
        .iter()
        .any(|f| f.name == field_name && f.immutable)
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
        collection = next;
    }

    if !collection_has_field(&collection, "source") {
        let next = node
            .patch_collection("PeerPairingDesired", ADD_PEER_PAIRING_DESIRED_SOURCE_PATCH)
            .await
            .context("patch_collection PeerPairingDesired source")?;
        node.set_active_collection_version(&next.version_id)
            .await
            .context("set_active_collection_version PeerPairingDesired source")?;
        tracing::info!(
            version = %next.version_id,
            "PeerPairingDesired patched with source field"
        );
    }

    if !collection_has_field(&collection, "template") {
        let next = node
            .patch_collection(
                "PeerPairingDesired",
                ADD_PEER_PAIRING_DESIRED_TEMPLATE_PATCH,
            )
            .await
            .context("patch_collection PeerPairingDesired template")?;
        node.set_active_collection_version(&next.version_id)
            .await
            .context("set_active_collection_version PeerPairingDesired template")?;
        tracing::info!(
            version = %next.version_id,
            "PeerPairingDesired patched with template field"
        );
    }

    // Self-healing backfill (gated on ROW STATE, runs every startup): default any
    // row still missing `source`/`template` to the operator partition and the
    // `conversation` template. Previously the backfill lived inside the
    // field-add branch, so a crash between adding the field and running the
    // backfill left rows at `source: null` forever — invisible to BOTH the
    // operator (`source _eq "operator"`) and registry (`source _eq "registry"`)
    // partition queries, and reconciled by neither. Reading rows and updating
    // by `_docID` (rather than an `_eq: null` filter) also avoids clobbering
    // legitimate registry-owned rows.
    backfill_pairing_desired_defaults(&node).await;

    Ok(())
}

/// Fill any `PeerPairingDesired` row still missing `source` or `template` with
/// the operator-partition / `conversation` defaults. Idempotent and convergent:
/// once every row carries both fields it updates nothing, so it is safe to run on
/// every startup and self-heals a migration that crashed mid-backfill.
async fn backfill_pairing_desired_defaults(node: &EmbeddedNode) {
    let response = node
        .execute(r#"query { PeerPairingDesired { _docID source template } }"#)
        .await;
    if response.has_errors() {
        tracing::warn!(errors = ?response.errors, "PeerPairingDesired backfill read failed");
        return;
    }
    let rows = response
        .data
        .as_ref()
        .and_then(|d| d.get("PeerPairingDesired"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for row in rows {
        let Some(doc_id) = row.get("_docID").and_then(|v| v.as_str()) else {
            continue;
        };
        let is_blank = |field: &str| {
            row.get(field)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .map(str::is_empty)
                .unwrap_or(true)
        };
        let mut assignments = Vec::new();
        if is_blank("source") {
            assignments.push("source: \"operator\"".to_string());
        }
        if is_blank("template") {
            assignments.push("template: \"conversation\"".to_string());
        }
        if assignments.is_empty() {
            continue;
        }
        let mutation = format!(
            r#"mutation {{ update_PeerPairingDesired(
                filter: {{ _docID: {{ _eq: "{}" }} }},
                input: {{ {} }}
            ) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(doc_id),
            assignments.join(", ")
        );
        let update = node.execute(&mutation).await;
        if update.has_errors() {
            tracing::warn!(
                doc_id,
                errors = ?update.errors,
                "PeerPairingDesired default backfill update failed"
            );
        }
    }
}

/// Idempotent migration for PeerRegistry: registers the collection schema if
/// it does not yet exist. PeerRegistry is a new collection introduced for
/// service-discovery.
///
/// Additive field migration: the registry offer field was renamed `profiles` →
/// `templates` (a node now advertises the scope templates it offers, not raw
/// collection profiles). Existing DBs that registered PeerRegistry under the old
/// schema gain the `templates` field via patch. No backfill — a node freshly
/// re-advertises its offered templates on its next heartbeat, so the stale
/// `profiles` value is simply left unread.
pub async fn ensure_peer_registry_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    let existing = node
        .get_collection("PeerRegistry")
        .context("get PeerRegistry collection")?;

    if let Some(collection) = existing {
        if !collection_has_field(&collection, "templates") {
            let next = node
                .patch_collection("PeerRegistry", ADD_PEER_REGISTRY_TEMPLATES_PATCH)
                .await
                .context("patch_collection PeerRegistry templates")?;
            node.set_active_collection_version(&next.version_id)
                .await
                .context("set_active_collection_version PeerRegistry templates")?;
            tracing::info!(
                version = %next.version_id,
                "PeerRegistry patched with templates field"
            );
        }
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::PEER_REGISTRY)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add PeerRegistry schema"),
    }
}

/// Idempotent migration ensuring the `ConsumedInviteNonce` ledger collection
/// exists. This is the runtime backing for single-use pairing invites (Task C2,
/// Finding #16): the join path records each redeemed token's `nonce` here and
/// rejects any token whose nonce is already present, mirroring the Lean
/// `consumedNonces` ledger and the `replay_rejected` theorem. The `nonce` field
/// carries a unique index (declared in the SDL) so a concurrent double-redeem
/// loses the race at insert time rather than slipping through.
///
/// A fresh database created from `schemas::ALL` already has this collection, so
/// the migration is a no-op there; it only adds the schema on a database
/// upgraded from before C2 landed (mirrors `ensure_peer_registry_migrations`).
pub async fn ensure_consumed_invite_nonce_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("ConsumedInviteNonce")
        .context("get ConsumedInviteNonce collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::CONSUMED_INVITE_NONCE)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add ConsumedInviteNonce schema"),
    }
}

async fn ensure_peer_pairing_applied_schema(node: &EmbeddedNode) -> Result<()> {
    let existing = node
        .get_collection("PeerPairingApplied")
        .context("get PeerPairingApplied collection")?;

    if let Some(collection) = existing {
        // Additive: existing DBs gain the `replicator_filter` field that records
        // the scope filter last installed for this pairing's replicators, so a
        // changed desired filter is detected as divergence (Lean
        // `filter_change_forces_reinstall`). No backfill: null == unfiltered.
        if !collection_has_field(&collection, "replicator_filter") {
            let next = node
                .patch_collection(
                    "PeerPairingApplied",
                    ADD_PEER_PAIRING_APPLIED_REPLICATOR_FILTER_PATCH,
                )
                .await
                .context("patch_collection PeerPairingApplied replicator_filter")?;
            node.set_active_collection_version(&next.version_id)
                .await
                .context("set_active_collection_version PeerPairingApplied replicator_filter")?;
            tracing::info!(
                version = %next.version_id,
                "PeerPairingApplied patched with replicator_filter field"
            );
        }
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

/// Idempotent migration ensuring every conversation collection carries an
/// `@immutable` `agent_did` scope key, the field filtered replication (#1033)
/// scopes on (and rejects unless immutable).
///
/// Two groups, both required on an UPGRADED database:
///
/// 1. The four collections that key on `session_id` and historically lacked
///    `agent_did` entirely (AgentMessage, AgentToolCall, AgentSession,
///    CompactionEntry) get the field ADDED as an immutable nullable String. No
///    lens is required: a row written before the migration reads back
///    `agent_did: null` until it is rewritten or recreated.
/// 2. The four that already carried `agent_did` as a plain `@index` field
///    (AgentRequest, AgentResponse, AgentToolResult, AgentConversation) cannot
///    be flipped to immutable on the current pin — defradb rejects any property
///    change to an existing field. These are only DETECTED: a mutable field on an
///    upgraded node is warned about (filtered replication of the conversation
///    template is unavailable there until an upstream immutable-flip lands).
///
/// Each collection is checked independently so a partial failure resumes at the
/// un-migrated collection on the next run, and each step is a no-op once its
/// target shape is already present (so fresh databases, created immutable from
/// the SDL, skip every patch and warning).
pub async fn ensure_conversation_scope_key_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    // Group 1: add the immutable scope key where it is missing entirely.
    for (collection_name, patch) in [
        ("AgentMessage", ADD_AGENT_MESSAGE_AGENT_DID_PATCH),
        ("AgentToolCall", ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH),
        ("AgentSession", ADD_AGENT_SESSION_AGENT_DID_PATCH),
        ("CompactionEntry", ADD_COMPACTION_ENTRY_AGENT_DID_PATCH),
    ] {
        let Some(collection) = node
            .get_collection(collection_name)
            .with_context(|| format!("get {collection_name} collection"))?
        else {
            tracing::debug!(
                collection = collection_name,
                "collection absent; scope-key patch no-op"
            );
            continue;
        };

        if collection_has_field(&collection, "agent_did") {
            tracing::debug!(
                collection = collection_name,
                "already has agent_did scope key; skipping patch"
            );
            continue;
        }

        let next = node
            .patch_collection(collection_name, patch)
            .await
            .with_context(|| format!("patch_collection {collection_name} agent_did scope key"))?;
        node.set_active_collection_version(&next.version_id)
            .await
            .with_context(|| {
                format!("set_active_collection_version {collection_name} agent_did scope key")
            })?;
        tracing::info!(
            collection = collection_name,
            version = %next.version_id,
            "patched with agent_did immutable scope key"
        );
    }

    // Group 2: detect (cannot fix in place) a pre-existing mutable agent_did.
    for collection_name in PRE_EXISTING_AGENT_DID_COLLECTIONS {
        let Some(collection) = node
            .get_collection(collection_name)
            .with_context(|| format!("get {collection_name} collection"))?
        else {
            continue;
        };

        if collection_has_field(&collection, "agent_did")
            && !field_is_immutable(&collection, "agent_did")
        {
            tracing::warn!(
                collection = collection_name,
                "agent_did exists but is not immutable on this upgraded database; \
                 defradb cannot flip an existing field to immutable, so filtered \
                 replication of the conversation template is unavailable here until \
                 an upstream immutable-flip migration lands (a fresh database is \
                 created immutable from the SDL and is unaffected)"
            );
        }
    }

    Ok(())
}

/// Run every idempotent runtime schema migration in dependency order. The daemon
/// (`run_agent`) and the desktop bootstrap both call this so the two hosts can
/// never drift on which migrations have run — a drift that previously left
/// upgraded desktop databases without the `@immutable` conversation scope keys
/// (and without the PeerRegistry / AgentRuntime-status collections), silently
/// disabling filtered replication there.
//
// THIS IS THE ONLY SANCTIONED MIGRATION ENTRY POINT. Every host (daemon,
// desktop, and ALL CLI-local paths: `init`, `server`, `subagent`, oneshot, and
// offline config diff/apply/export via `resolve_config_access`) must call this
// and nothing else. Do NOT hand-enumerate a SUBSET of the individual `ensure_*`
// migrations at a new call site: that is exactly the host-drift bug (Finding #3
// / Pattern 3) where some paths skipped `ensure_conversation_scope_key_migrations`
// and an upgraded DB failed reads with `Cannot query field "agent_did"`. The
// individual `ensure_*` fns exist only as the building blocks of this function
// (and the migration unit tests); new code adds a step HERE.
pub async fn ensure_all_runtime_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    ensure_agent_runtime_executor_status_migrations(node.clone())
        .await
        .context("ensure AgentRuntime executor status migrations")?;
    ensure_peer_pairing_desired_migrations(node.clone())
        .await
        .context("ensure PeerPairingDesired migrations")?;
    ensure_peer_registry_migrations(node.clone())
        .await
        .context("ensure PeerRegistry migrations")?;
    ensure_consumed_invite_nonce_migrations(node.clone())
        .await
        .context("ensure ConsumedInviteNonce migrations")?;
    ensure_tool_service_registry_migrations(node.clone())
        .await
        .context("ensure ToolServiceRegistry migrations")?;
    ensure_tool_service_health_state_migrations(node.clone())
        .await
        .context("ensure ToolServiceHealthState migrations")?;
    ensure_agent_behavior_migrations(node.clone())
        .await
        .context("ensure AgentBehavior migrations")?;
    ensure_conversation_scope_key_migrations(node)
        .await
        .context("ensure conversation agent_did scope-key migrations")?;
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
            ADD_PEER_PAIRING_DESIRED_SOURCE_PATCH,
            ADD_PEER_PAIRING_DESIRED_TEMPLATE_PATCH,
            ADD_PEER_PAIRING_APPLIED_REPLICATOR_FILTER_PATCH,
            ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH,
            ADD_TOOL_SERVICE_HEALTH_STATE_TOOL_COUNT_PATCH,
            ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH,
            ADD_AGENT_MESSAGE_AGENT_DID_PATCH,
            ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH,
            ADD_AGENT_SESSION_AGENT_DID_PATCH,
            ADD_COMPACTION_ENTRY_AGENT_DID_PATCH,
        ] {
            for (name, kind) in field_kinds(patch) {
                assert_ne!(kind, 17, "field {name} uses unassigned Kind 17");
            }
        }
    }

    #[test]
    fn conversation_scope_key_patches_use_nillable_string_kind() {
        const NILLABLE_STRING_KIND: i64 = 11;
        for patch in [
            ADD_AGENT_MESSAGE_AGENT_DID_PATCH,
            ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH,
            ADD_AGENT_SESSION_AGENT_DID_PATCH,
            ADD_COMPACTION_ENTRY_AGENT_DID_PATCH,
        ] {
            for (name, kind) in field_kinds(patch) {
                assert_eq!(name, "agent_did", "unexpected field in scope-key patch");
                assert_eq!(
                    kind, NILLABLE_STRING_KIND,
                    "agent_did scope key must be NillableString (11), got {kind}"
                );
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
        assert!(collection_has_field(&desired, "source"));
        assert!(collection_has_field(&desired, "template"));

        let applied = node
            .get_collection("PeerPairingApplied")
            .unwrap()
            .expect("PeerPairingApplied collection");
        assert!(collection_has_field(&applied, "collections"));
        assert!(collection_has_field(&applied, "replicator_addresses"));
        assert!(collection_has_field(&applied, "replicator_filter"));
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

    // Pre-scope-key SDL for the four session-keyed conversation collections:
    // identical to the shipped schema minus the `agent_did` field. Registering
    // these on a fresh node simulates a database upgraded from a schema version
    // that predates the immutable scope key, which the migration must patch.
    const OLD_AGENT_MESSAGE_SCHEMA: &str = r#"
        type AgentMessage @branchable {
            message_key: String @index(unique: true)
            session_id: String @index
            request_id: String @index
            sequence: Int @index
            role: String
            content: String
            timestamp: DateTime
        }
    "#;
    const OLD_AGENT_TOOL_CALL_SCHEMA: &str = r#"
        type AgentToolCall @branchable {
            tool_call_key: String @index(unique: true)
            request_id: String @index
            session_id: String @index
            tool_name: String @index
            tool_call_id: String @index
            args: String
            status: String
            lifecycle_state: String @index
        }
    "#;
    const OLD_AGENT_SESSION_SCHEMA: &str = r#"
        type AgentSession @branchable {
            session_id: String @index(unique: true)
            agent_name: String @index
            behavior_id: String @index
            started: DateTime
            ended: DateTime
            status: String
        }
    "#;
    const OLD_COMPACTION_ENTRY_SCHEMA: &str = r#"
        type CompactionEntry @branchable {
            compaction_key: String @index(unique: true)
            session_id: String @index
            sequence: Int @index
            summary: String
            created_at: DateTime
        }
    "#;

    // Pre-`@immutable` SDL for the four conversation collections that already
    // carried `agent_did` but only as a plain `@index` field. Registering these
    // simulates a database upgraded from a version before the scope key was made
    // immutable; the migration must flip the existing field in place. These are
    // trimmed to the fields the migration test exercises.
    const OLD_AGENT_REQUEST_SCHEMA: &str = r#"
        type AgentRequest @branchable {
            request_id: String @index
            agent_did: String @index
            session_id: String @index
            content: String
            status: String @index
            lifecycle_state: String @index
            created_at: String @index
        }
    "#;
    const OLD_AGENT_RESPONSE_SCHEMA: &str = r#"
        type AgentResponse @branchable {
            response_key: String @index(unique: true)
            request_id: String @index
            agent_did: String @index
            session_id: String @index
            content: String
            created_at: String @index
        }
    "#;
    const OLD_AGENT_TOOL_RESULT_SCHEMA: &str = r#"
        type AgentToolResult @branchable {
            agent_did: String @index
            session_id: String @index
            tool_name: String @index
            created_at: String @index
        }
    "#;
    const OLD_AGENT_CONVERSATION_SCHEMA: &str = r#"
        type AgentConversation @branchable {
            session_id: String @index(unique: true)
            agent_name: String @index
            agent_did: String @index
            created_at: DateTime @index(direction: DESC)
        }
    "#;

    #[tokio::test]
    async fn conversation_scope_key_migration_makes_agent_did_immutable_on_upgrade() {
        let node = test_node().await;
        // Group 1: agent_did absent entirely.
        node.add_schema(OLD_AGENT_MESSAGE_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_TOOL_CALL_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_SESSION_SCHEMA).await.unwrap();
        node.add_schema(OLD_COMPACTION_ENTRY_SCHEMA).await.unwrap();
        // Group 2: agent_did present but mutable (@index only).
        node.add_schema(OLD_AGENT_REQUEST_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_RESPONSE_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_TOOL_RESULT_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_CONVERSATION_SCHEMA)
            .await
            .unwrap();

        const GROUP1: [&str; 4] = [
            "AgentMessage",
            "AgentToolCall",
            "AgentSession",
            "CompactionEntry",
        ];
        const GROUP2: [&str; 4] = [
            "AgentRequest",
            "AgentResponse",
            "AgentToolResult",
            "AgentConversation",
        ];

        // Pre-state: group 1 lacks agent_did; group 2 has it but mutable.
        for collection in GROUP1 {
            let cv = node.get_collection(collection).unwrap().unwrap();
            assert!(
                !collection_has_field(&cv, "agent_did"),
                "{collection} should not yet have agent_did before migration"
            );
        }
        for collection in GROUP2 {
            let cv = node.get_collection(collection).unwrap().unwrap();
            assert!(
                collection_has_field(&cv, "agent_did") && !field_is_immutable(&cv, "agent_did"),
                "{collection} should start with a mutable agent_did before migration"
            );
        }

        // Idempotent: running twice must not fail.
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();

        // Post-state, group 1 (ADDED field): agent_did is now IMMUTABLE — the
        // property #1033 requires before it will install the scope filter.
        for collection in GROUP1 {
            let cv = node.get_collection(collection).unwrap().unwrap();
            assert!(
                field_is_immutable(&cv, "agent_did"),
                "{collection}.agent_did must be immutable after migration"
            );
        }

        // Post-state, group 2 (PRE-EXISTING field): defradb refuses to flip an
        // existing field to immutable, so the migration can only warn — the field
        // stays mutable on an upgraded DB. This assertion pins that limitation; it
        // should flip to `field_is_immutable` once an upstream immutable-flip
        // migration exists (at which point this test, and the group-2 detection
        // branch in `ensure_conversation_scope_key_migrations`, must be updated).
        for collection in GROUP2 {
            let cv = node.get_collection(collection).unwrap().unwrap();
            assert!(
                !field_is_immutable(&cv, "agent_did"),
                "{collection}.agent_did cannot yet be made immutable on upgrade \
                 (see ensure_conversation_scope_key_migrations group 2)"
            );
        }

        // Enforcement actually fires on the migrated ADDED field: create a row
        // with the original owner, then attempt to reassign the scope key — the
        // update must error and the stored value must survive.
        let create = node
            .execute(
                r#"mutation { create_AgentMessage(input: {
                    message_key: "mig-m1", session_id: "mig-s1",
                    agent_did: "did:defra-agent:alice", role: "user", content: "hi"
                }) { _docID } }"#,
            )
            .await;
        assert!(
            !create.has_errors(),
            "create AgentMessage with agent_did failed: {:?}",
            create.errors
        );
        let rewrite = node
            .execute(
                r#"mutation {
                    update_AgentMessage(
                        filter: { agent_did: { _eq: "did:defra-agent:alice" } },
                        input: { agent_did: "did:defra-agent:mallory" }
                    ) { _docID }
                }"#,
            )
            .await;
        assert!(
            rewrite.has_errors(),
            "rewriting the migrated immutable agent_did on AgentMessage must be rejected"
        );
    }

    /// Guard: every conversation collection must declare `agent_did` as an
    /// `@immutable` scope key in its canonical SDL — the property filtered
    /// replication (#1033) relies on. This checks the SDL source; the companion
    /// runtime tests below verify that the pinned #1033 rev actually ENFORCES it
    /// (rejecting a rewrite) on both the fresh-SDL and migrated-upgrade paths.
    #[test]
    fn all_conversation_collections_declare_agent_did_immutable() {
        use defra_agent_protocol::schemas;
        let conversation_sdls = [
            ("AgentRequest", schemas::AGENT_REQUEST),
            ("AgentResponse", schemas::AGENT_RESPONSE),
            ("AgentToolResult", schemas::AGENT_TOOL_RESULT),
            ("AgentConversation", schemas::AGENT_CONVERSATION),
            ("AgentMessage", schemas::AGENT_MESSAGE),
            ("AgentToolCall", schemas::AGENT_TOOL_CALL),
            ("AgentSession", schemas::AGENT_SESSION),
            ("CompactionEntry", schemas::COMPACTION_ENTRY),
        ];
        for (name, sdl) in conversation_sdls {
            let line = sdl
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("agent_did:"))
                .unwrap_or_else(|| panic!("{name} SDL must declare an agent_did field"));
            assert!(
                line.contains("@immutable"),
                "{name}.agent_did must be declared @immutable (the filtered-replication scope key); got: {line}"
            );
        }
    }

    /// Companion to the SDL guard, FRESH-SDL path: with the defradb.rs #1033 rev
    /// pinned, `@immutable` enforcement is LIVE — a row is created with
    /// `agent_did`, but any later rewrite of the immutable scope key is REJECTED.
    /// This is the runtime half of the filtered-replication DAG-safety guarantee.
    #[tokio::test]
    async fn agent_did_rewrite_is_rejected_by_immutable_enforcement() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        let create = node
            .execute(
                r#"mutation {
                    create_AgentRequest(input: {
                        request_id: "scope-guard-req",
                        agent_did: "did:defra-agent:alice",
                        session_id: "scope-guard-session",
                        content: "hi",
                        status: "pending",
                        lifecycle_state: "pending",
                        created_at: "2026-06-13T00:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !create.has_errors(),
            "create AgentRequest with agent_did failed: {:?}",
            create.errors
        );

        // Attempt to rewrite the immutable scope key to a different DID; the
        // #1033 enforcement must reject it (or leave the stored value unchanged).
        let rewrite = node
            .execute(
                r#"mutation {
                    update_AgentRequest(
                        filter: { request_id: { _eq: "scope-guard-req" } },
                        input: { agent_did: "did:defra-agent:mallory" }
                    ) { _docID }
                }"#,
            )
            .await;

        // Enforcement must REJECT the rewrite outright, not silently no-op it.
        assert!(
            rewrite.has_errors(),
            "rewrite of the immutable agent_did scope key must be rejected with an error"
        );

        // ...and the stored value must still be the original owner.
        let read = node
            .execute(
                r#"query {
                    AgentRequest(filter: { request_id: { _eq: "scope-guard-req" } }) {
                        agent_did
                    }
                }"#,
            )
            .await;
        let stored = read
            .data
            .as_ref()
            .and_then(|d| d.get("AgentRequest"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("agent_did"))
            .and_then(|v| v.as_str());
        assert_eq!(
            stored,
            Some("did:defra-agent:alice"),
            "immutable agent_did scope key must survive a rewrite attempt \
             (rewrite errors={:?})",
            rewrite.errors
        );
    }

    #[tokio::test]
    async fn peer_registry_migration_creates_collection_with_all_fields() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        // Idempotent: calling twice must not fail.
        ensure_peer_registry_migrations(node.clone()).await.unwrap();
        ensure_peer_registry_migrations(node.clone()).await.unwrap();

        let collection = node
            .get_collection("PeerRegistry")
            .unwrap()
            .expect("PeerRegistry collection must exist after migration");

        for field in &[
            "peer_id",
            "agent_did",
            "addresses",
            "templates",
            "display_name",
            "status",
            "network_id",
            "invited_by",
            "registered_at",
            "updated_at",
        ] {
            assert!(
                collection_has_field(&collection, field),
                "PeerRegistry must have field '{field}'"
            );
        }
    }

    #[tokio::test]
    async fn consumed_invite_nonce_migration_creates_collection_with_all_fields() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        // Idempotent: calling twice must not fail.
        ensure_consumed_invite_nonce_migrations(node.clone())
            .await
            .unwrap();
        ensure_consumed_invite_nonce_migrations(node.clone())
            .await
            .unwrap();

        let collection = node
            .get_collection("ConsumedInviteNonce")
            .unwrap()
            .expect("ConsumedInviteNonce collection must exist after migration");

        for field in &["nonce", "issuer_did", "consumed_at"] {
            assert!(
                collection_has_field(&collection, field),
                "ConsumedInviteNonce must have field '{field}'"
            );
        }
    }

    /// Regression for the host-drift bug (Finding #3 / Pattern 3): CLI-local
    /// paths used to hand-enumerate a SUBSET of migrations
    /// (`ensure_tool_call_migrations` + `ensure_subagent_extensions_migrations`)
    /// that does NOT add the `agent_did` scope key. On a database upgraded from
    /// a pre-scope-key schema, that left AgentToolCall without `agent_did` —
    /// while `ToolCallLifecycle::load` unconditionally SELECTs it, so
    /// `subagent cancel` (and other config-read paths) failed with
    /// `Cannot query field "agent_did"`.
    ///
    /// This test pins the consolidation: starting from a pre-scope-key DB,
    /// `ensure_all_runtime_migrations` (the single sanctioned entry point every
    /// host now calls) makes `agent_did` queryable on all four session-keyed
    /// conversation collections AND creates the ConsumedInviteNonce ledger
    /// (Task C2). It also pins the drift it fixes: the old subset leaves
    /// `agent_did` absent.
    #[tokio::test]
    async fn ensure_all_runtime_migrations_adds_scope_key_the_subset_misses() {
        let node = test_node().await;
        // Simulate a database upgraded from a schema version that predates the
        // immutable scope key: AgentToolCall (and its session-keyed siblings)
        // exist but lack `agent_did`.
        node.add_schema(OLD_AGENT_MESSAGE_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_TOOL_CALL_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_SESSION_SCHEMA).await.unwrap();
        node.add_schema(OLD_COMPACTION_ENTRY_SCHEMA).await.unwrap();

        const SCOPE_KEYED: [&str; 4] = [
            "AgentMessage",
            "AgentToolCall",
            "AgentSession",
            "CompactionEntry",
        ];

        // Drift pin: the hand-enumerated subset the CLI-local paths used to run
        // never adds the scope key, so AgentToolCall stays without `agent_did`.
        ensure_tool_call_migrations(node.clone()).await.unwrap();
        ensure_subagent_extensions_migrations(node.clone())
            .await
            .unwrap();
        let atc = node.get_collection("AgentToolCall").unwrap().unwrap();
        assert!(
            !collection_has_field(&atc, "agent_did"),
            "the old subset must NOT add agent_did (this is the drift the fix closes)"
        );

        // The single sanctioned entry point: idempotent, run twice.
        ensure_all_runtime_migrations(node.clone()).await.unwrap();
        ensure_all_runtime_migrations(node.clone()).await.unwrap();

        for collection in SCOPE_KEYED {
            let cv = node.get_collection(collection).unwrap().unwrap();
            assert!(
                collection_has_field(&cv, "agent_did"),
                "{collection}.agent_did must exist after ensure_all_runtime_migrations"
            );
        }

        // ...and the field is actually QUERYABLE — exactly the SELECT that
        // `ToolCallLifecycle::load` issues and that used to fail.
        let read = node
            .execute(r#"query { AgentToolCall { _docID agent_did } }"#)
            .await;
        assert!(
            !read.has_errors(),
            "querying agent_did on AgentToolCall must succeed after the full migration set, \
             got: {:?}",
            read.errors
        );

        // Task C2 ledger is part of the sanctioned set too.
        assert!(
            node.get_collection("ConsumedInviteNonce")
                .unwrap()
                .is_some(),
            "ConsumedInviteNonce ledger must exist after ensure_all_runtime_migrations"
        );
    }
}
