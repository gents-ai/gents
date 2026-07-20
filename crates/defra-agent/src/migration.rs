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

const ADD_LIFECYCLE_STATE_PATCH: &str = r#"[{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"lifecycle_state","Kind":"String"}}]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"await_mode","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_policy","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"child_request_id","Kind":"String"}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_REQUEST_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"subagent_depth","Kind":"Int"}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_request_id","Kind":"String"}},
    {"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"caused_by_parent_tool_call_id","Kind":"String"}}
]"#;

// #664 durable terminalization: `terminalized_at` schedules by the time the
// terminal edge actually persisted (not request creation time), while the
// persisted attempt counter survives daemon restarts and bounds same-value
// request-field history.
const ADD_AGENT_REQUEST_TERMINALIZED_AT_FIELD: &str = r#"{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"terminalized_at","Kind":"String"}}"#;
const ADD_AGENT_REQUEST_TERMINAL_REDRIVE_ATTEMPTS_FIELD: &str = r#"{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"terminal_redrive_attempts","Kind":"Int"}}"#;

// #683 request-party routing. `requester_did` is written only when a remote
// paired coordinator causes a host-owned child request. It is an immutable
// filter key so a request cannot drift between peer scopes after creation.
const ADD_AGENT_REQUEST_REQUESTER_DID_FIELD: &str = r#"{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"requester_did","Kind":"String","Immutable":true}}"#;

// #713 subagent-return lineage routing. Each returned artifact carries the
// same immutable requester DID as its child AgentRequest, so the host return
// pairing can filter a coordinator's lineage without matching unrelated
// host-owned history.
const SUBAGENT_RETURN_ARTIFACT_COLLECTIONS: [&str; 7] = [
    "AgentResponse",
    "AgentMessage",
    "AgentToolCall",
    "AgentToolResult",
    "AgentSession",
    "AgentConversation",
    "CompactionEntry",
];

// DefraDB #1106 rejects numeric Kind values in schema patches. Canonical SDL
// strings keep scalar and array intent explicit and avoid the #661 class where
// a numeric IntArray code was used for an intended scalar Int field.
#[allow(dead_code)]
const ADD_TOOL_SELECTION_SUBAGENT_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_targets","Kind":"[String]"}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_spawn_enabled","Kind":"Boolean"}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_steering_enabled","Kind":"Boolean"}},
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_background_enabled","Kind":"Boolean"}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_BACKGROUND_TOOLS_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"backgroundable_tool_names","Kind":"[String]"}}
]"#;

const ADD_TOOL_SELECTION_APPROVAL_REQUIRED_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"approval_required_tools","Kind":"[String]"}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_R5_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"unclaimed_deadline_at","Kind":"DateTime"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_cascade_intent_at","Kind":"DateTime"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_pending_remote_ack","Kind":"Boolean"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"stuck_since","Kind":"DateTime"}}
]"#;

const ADD_AGENT_TOOL_CALL_LIVE_OUTPUT_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"partial_output_tail","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"partial_output_seq","Kind":"Int"}}
]"#;

const ADD_AGENT_TOOL_CALL_WORKFLOW_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"workflow_group_id","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"workflow_role","Kind":"String"}}
]"#;

const ADD_AGENT_TOOL_CALL_SPAWN_TARGET_DID_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"spawn_target_did","Kind":"String","Immutable":true}}
]"#;

#[allow(dead_code)]
const ADD_AGENT_TOOL_CALL_COMMAND_DENIAL_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denial_reason","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argv","Kind":"[String]"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_command","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argument","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_subcommand","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_prefix","Kind":"[String]"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_mode","Kind":"String"}},
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_network","Kind":"String"}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_R5_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"cross_deployment_spawn_timeout_seconds","Kind":"Int"}}
]"#;

#[allow(dead_code)]
const ADD_TOOL_SELECTION_SESSION_HISTORY_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"enable_session_history_tool","Kind":"Boolean"}}
]"#;

const ADD_TOOL_SELECTION_CONTEXT_BUDGET_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"enable_context_budget","Kind":"Boolean"}}
]"#;

const ADD_TOOL_SELECTION_DEFAULT_AWAIT_MODE_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"subagent_default_await_mode","Kind":"String"}}
]"#;

const ADD_TOOL_SELECTION_ORCHESTRATION_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"orchestration_enabled","Kind":"Boolean"}}
]"#;

const ADD_TOOL_SELECTION_POLICY_VERSION_PATCH: &str = r#"[
    {"op":"add","path":"/ToolSelection/Fields/-","value":{"Name":"tool_policy_version","Kind":"String"}}
]"#;

const ADD_AGENT_RESPONSE_REASONING_PROGRESS_PATCH: &str = r#"[
    {"op":"add","path":"/AgentResponse/Fields/-","value":{"Name":"reasoning_progress_seq","Kind":"Int"}}
]"#;

// Durable goal accounting excludes cache reads from charged input. Existing
// InferenceCall collections must gain this scalar before any completion write
// or goal usage query references it. Keep Kind as the canonical SDL string:
// numeric DefraDB kind values caused the #661 fleet incident.
const ADD_INFERENCE_CALL_CACHED_INPUT_TOKENS_PATCH: &str = r#"[
    {"op":"add","path":"/InferenceCall/Fields/-","value":{"Name":"cached_input_tokens","Kind":"Int"}}
]"#;

const ADD_PEER_PAIRING_DESIRED_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"agent_did","Kind":"String"}}
]"#;

const ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"profiles","Kind":"[String]"}}
]"#;

const ADD_PEER_PAIRING_DESIRED_SOURCE_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"source","Kind":"String"}}
]"#;

const ADD_PEER_PAIRING_DESIRED_TEMPLATE_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingDesired/Fields/-","value":{"Name":"template","Kind":"String"}}
]"#;

const ADD_DATA_PLANE_PAIRING_DESIRED_SOURCE_PATCH: &str = r#"[
    {"op":"add","path":"/DataPlanePairingDesired/Fields/-","value":{"Name":"source","Kind":"String"}}
]"#;

const ADD_CONSUMED_INVITE_NONCE_CLAIMANT_PATCH: &str = r#"[
    {"op":"add","path":"/ConsumedInviteNonce/Fields/-","value":{"Name":"claimant_did","Kind":"String"}}
]"#;

/// The per-completion retry fields #648 added to `InferenceProfile`, with
/// canonical DefraDB patch kinds.
/// The runtime queries these at startup, so an upgraded home MUST gain them
/// before the first profile read or the server fails to boot.
/// Additive `InferenceProfile` fields patched onto pre-existing collections.
///
/// Adding to this table is the whole migration: every field is nullable, so an
/// upgraded store gains it with no backfill and a fresh store gets it straight
/// from the SDL. The sampling knobs beyond `temperature` are #649 — before
/// them, `top_p`/`top_k` were entirely at the mercy of the served checkpoint's
/// `generation_config.json`, with no way to correct a wrong default from
/// desired state.
const INFERENCE_PROFILE_ADDITIVE_FIELDS: &[(&str, &str)] = &[
    ("retry_max_transport", "Int"),
    ("retry_backoff_ms", "[Int]"),
    ("retry_max_resample", "Int"),
    ("retry_allow_repair", "Boolean"),
    ("retry_interactive_max", "Int"),
    ("top_p", "Float"),
    ("top_k", "Int"),
    ("min_p", "Float"),
    ("frequency_penalty", "Float"),
    ("presence_penalty", "Float"),
    ("repetition_penalty", "Float"),
];

const ADD_PEER_PAIRING_APPLIED_REPLICATOR_FILTER_PATCH: &str = r#"[
    {"op":"add","path":"/PeerPairingApplied/Fields/-","value":{"Name":"replicator_filter","Kind":"String"}}
]"#;

const ADD_PEER_REGISTRY_TEMPLATES_PATCH: &str = r#"[
    {"op":"add","path":"/PeerRegistry/Fields/-","value":{"Name":"templates","Kind":"[String]"}}
]"#;

// AgentBehavior gained nullable string fields over
// time; existing DBs upgraded from a prior schema version must have these fields
// patched in so that reads/writes referencing them do not fail with "unknown
// field" errors.
#[allow(dead_code)]
const ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH: &str = r#"[
    {"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"description","Kind":"String"}},
    {"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"summary","Kind":"String"}},
    {"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"request_context_template","Kind":"String"}}
]"#;

const ADD_TOOL_SERVICE_REGISTRY_SEND_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/ToolServiceRegistry/Fields/-","value":{"Name":"send_agent_did","Kind":"Boolean"}}
]"#;

const ADD_TOOL_SERVICE_HEALTH_STATE_TOOL_COUNT_PATCH: &str = r#"[
    {"op":"add","path":"/ToolServiceHealthState/Fields/-","value":{"Name":"tool_count","Kind":"Int"}}
]"#;

const ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH: &str = r#"[
    {"op":"add","path":"/AgentRuntime/Fields/-","value":{"Name":"behavior_executor_capacity","Kind":"Int"}},
    {"op":"add","path":"/AgentRuntime/Fields/-","value":{"Name":"behavior_executor_queue_depth","Kind":"Int"}},
    {"op":"add","path":"/AgentRuntime/Fields/-","value":{"Name":"behavior_executor_status_json","Kind":"String"}}
]"#;

// The `agent_did` scope key denormalizes the owning
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
    {"op":"add","path":"/AgentMessage/Fields/-","value":{"Name":"agent_did","Kind":"String","Immutable":true}}
]"#;
const ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"agent_did","Kind":"String","Immutable":true}}
]"#;
const ADD_AGENT_SESSION_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/AgentSession/Fields/-","value":{"Name":"agent_did","Kind":"String","Immutable":true}}
]"#;
const ADD_COMPACTION_ENTRY_AGENT_DID_PATCH: &str = r#"[
    {"op":"add","path":"/CompactionEntry/Fields/-","value":{"Name":"agent_did","Kind":"String","Immutable":true}}
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
            r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"request_id","Kind":"String"}}"#,
        );
    }
    if !collection_has_field(collection, "deadline_at") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"deadline_at","Kind":"DateTime"}}"#,
        );
    }
    if !collection_has_field(collection, "cancel_cause") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"cancel_cause","Kind":"String"}}"#,
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
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argv","Kind":"[String]"}}"#
                    }
                    "denied_prefix" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_prefix","Kind":"[String]"}}"#
                    }
                    _ => unreachable!("unexpected AgentToolCall array field {field}"),
                },
                11 => match field {
                    "denial_reason" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denial_reason","Kind":"String"}}"#
                    }
                    "denied_command" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_command","Kind":"String"}}"#
                    }
                    "denied_argument" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_argument","Kind":"String"}}"#
                    }
                    "denied_subcommand" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"denied_subcommand","Kind":"String"}}"#
                    }
                    "policy_mode" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_mode","Kind":"String"}}"#
                    }
                    "policy_network" => {
                        r#"{"op":"add","path":"/AgentToolCall/Fields/-","value":{"Name":"policy_network","Kind":"String"}}"#
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

/// Serialized field kind, for comparing a field's type without depending on
/// defradb.rs's concrete `FieldKind` encoding. `None` if the field is absent.
fn field_kind_value(
    cv: &defra_node::CollectionVersion,
    field_name: &str,
) -> Option<serde_json::Value> {
    cv.fields
        .iter()
        .find(|f| f.name == field_name)
        .and_then(|f| serde_json::to_value(&f.kind).ok())
}

fn ensure_field_kind_matches_reference(
    collection: &defra_node::CollectionVersion,
    collection_name: &str,
    field_name: &str,
    reference_fields: &[&str],
    expected_type: &str,
) -> Result<()> {
    let Some(existing_kind) = field_kind_value(collection, field_name) else {
        return Ok(());
    };
    let Some(expected_kind) = reference_fields
        .iter()
        .find_map(|name| field_kind_value(collection, name))
    else {
        return Ok(());
    };

    anyhow::ensure!(
        existing_kind == expected_kind,
        "{collection_name}.{field_name} has an unexpected field kind ({existing_kind}); \
         it must be scalar {expected_type} like its stable sibling ({expected_kind}). This is a \
         corrupted schema — likely an out-of-band additive patch applied with the wrong type. \
         Repair the field before starting. See sourcenetwork/defra-agent#661 and #663."
    );
    Ok(())
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
#[allow(unused_assignments)]
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
        if collection_has_field(cv, "partial_output_tail") {
            tracing::debug!("AgentToolCall already has live-output fields; skipping patch");
        } else {
            let pre_version_id = cv.version_id.clone();
            let patched = node
                .patch_collection("AgentToolCall", ADD_AGENT_TOOL_CALL_LIVE_OUTPUT_PATCH)
                .await
                .context("patch_collection AgentToolCall live-output fields")?;
            node.set_active_collection_version(&patched.version_id)
                .await
                .context("set_active_collection_version AgentToolCall live-output fields")?;
            tracing::info!(
                pre = %pre_version_id,
                patched = %patched.version_id,
                "AgentToolCall patched with live-output tail fields"
            );
            active_atc_collection = Some(patched);
        }
    }

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

    active_atc_collection = node
        .get_collection("AgentToolCall")
        .context("reload AgentToolCall collection after R5 patch")?;
    if let Some(ref cv) = active_atc_collection {
        if collection_has_field(cv, "spawn_target_did") {
            if !field_is_immutable(cv, "spawn_target_did") {
                anyhow::bail!(
                    "AgentToolCall.spawn_target_did exists but is not immutable; \
                     filtered subagent replication cannot be installed safely"
                );
            }
            tracing::debug!("AgentToolCall already has immutable spawn_target_did; skipping patch");
        } else {
            let pre_version_id = cv.version_id.clone();
            let v5 = node
                .patch_collection("AgentToolCall", ADD_AGENT_TOOL_CALL_SPAWN_TARGET_DID_PATCH)
                .await
                .context("patch_collection AgentToolCall spawn_target_did")?;
            node.set_active_collection_version(&v5.version_id)
                .await
                .context("set_active_collection_version AgentToolCall spawn_target_did")?;
            tracing::info!(
                pre = %pre_version_id,
                v5 = %v5.version_id,
                "AgentToolCall patched with immutable spawn_target_did"
            );
        }
    }

    active_atc_collection = node
        .get_collection("AgentToolCall")
        .context("reload AgentToolCall collection after spawn_target_did patch")?;
    if let Some(ref cv) = active_atc_collection {
        if collection_has_field(cv, "workflow_group_id") {
            tracing::debug!("AgentToolCall already has workflow fields; skipping patch");
        } else {
            let pre_version_id = cv.version_id.clone();
            let v5 = node
                .patch_collection("AgentToolCall", ADD_AGENT_TOOL_CALL_WORKFLOW_PATCH)
                .await
                .context("patch_collection AgentToolCall workflow group fields")?;
            node.set_active_collection_version(&v5.version_id)
                .await
                .context("set_active_collection_version AgentToolCall workflow fields")?;
            tracing::info!(
                pre = %pre_version_id,
                v5 = %v5.version_id,
                "AgentToolCall patched with workflow group fields"
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
            active_version = v7;
        }

        if collection_has_field(&active_version, "orchestration_enabled") {
            tracing::debug!("ToolSelection already has orchestration flag; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v8 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_ORCHESTRATION_PATCH)
                .await
                .context("patch_collection ToolSelection orchestration flag")?;
            node.set_active_collection_version(&v8.version_id)
                .await
                .context("set_active_collection_version ToolSelection orchestration flag")?;
            tracing::info!(
                pre = %pre_version_id,
                v8 = %v8.version_id,
                "ToolSelection patched with orchestration flag"
            );
            active_version = v8;
        }

        if collection_has_field(&active_version, "tool_policy_version") {
            tracing::debug!("ToolSelection already has tool policy version; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v9 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_POLICY_VERSION_PATCH)
                .await
                .context("patch_collection ToolSelection policy version")?;
            node.set_active_collection_version(&v9.version_id)
                .await
                .context("set_active_collection_version ToolSelection policy version")?;
            tracing::info!(
                pre = %pre_version_id,
                v9 = %v9.version_id,
                "ToolSelection patched with policy version"
            );
            active_version = v9;
        }

        if collection_has_field(&active_version, "enable_context_budget") {
            tracing::debug!("ToolSelection already has context budget flag; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v10 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_CONTEXT_BUDGET_PATCH)
                .await
                .context("patch_collection ToolSelection context budget flag")?;
            node.set_active_collection_version(&v10.version_id)
                .await
                .context("set_active_collection_version ToolSelection context budget flag")?;
            tracing::info!(
                pre = %pre_version_id,
                v10 = %v10.version_id,
                "ToolSelection patched with context budget flag"
            );
            active_version = v10;
        }

        if collection_has_field(&active_version, "approval_required_tools") {
            tracing::debug!("ToolSelection already has approval_required_tools; skipping patch");
        } else {
            let pre_version_id = active_version.version_id.clone();
            let v11 = node
                .patch_collection("ToolSelection", ADD_TOOL_SELECTION_APPROVAL_REQUIRED_PATCH)
                .await
                .context("patch_collection ToolSelection approval_required_tools")?;
            node.set_active_collection_version(&v11.version_id)
                .await
                .context("set_active_collection_version ToolSelection approval_required_tools")?;
            tracing::info!(
                pre = %pre_version_id,
                v11 = %v11.version_id,
                "ToolSelection patched with approval_required_tools"
            );
            // Intentionally advance the cursor even though v11 is currently the
            // final hand-written patch. The next patch must inspect v11 rather
            // than the stale v10 schema (see abc30235).
            active_version = v11;
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

/// Idempotent migration ensuring the separate data-plane desired collection
/// exists. Fresh stores get it from `schemas::ALL`; upgraded stores add it at
/// startup before the pairing reconciler reads desired state.
pub async fn ensure_data_plane_pairing_desired_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if let Some(collection) = node
        .get_collection("DataPlanePairingDesired")
        .context("get DataPlanePairingDesired collection")?
    {
        if !collection_has_field(&collection, "source") {
            let next = node
                .patch_collection(
                    "DataPlanePairingDesired",
                    ADD_DATA_PLANE_PAIRING_DESIRED_SOURCE_PATCH,
                )
                .await
                .context("patch_collection DataPlanePairingDesired source")?;
            node.set_active_collection_version(&next.version_id)
                .await
                .context("set_active_collection_version DataPlanePairingDesired source")?;
            tracing::info!(
                version = %next.version_id,
                "DataPlanePairingDesired patched with source field"
            );
        }
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::DATA_PLANE_PAIRING_DESIRED)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add DataPlanePairingDesired schema"),
    }
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
    if let Some(collection) = node
        .get_collection("ConsumedInviteNonce")
        .context("get ConsumedInviteNonce collection")?
    {
        if !collection_has_field(&collection, "claimant_did") {
            let next = node
                .patch_collection(
                    "ConsumedInviteNonce",
                    ADD_CONSUMED_INVITE_NONCE_CLAIMANT_PATCH,
                )
                .await
                .context("patch_collection ConsumedInviteNonce claimant_did")?;
            node.set_active_collection_version(&next.version_id)
                .await
                .context("set_active_collection_version ConsumedInviteNonce claimant_did")?;
            tracing::info!(
                version = %next.version_id,
                "ConsumedInviteNonce patched with claimant_did field"
            );
        }
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

/// Idempotent migration ensuring the `ReciprocalConversationIntent` collection
/// exists. Conversation dapair invites write this server-side intent so the
/// reciprocal reconciler can materialize a self-scoped conversation data-plane
/// edge once the invited member's signed `PeerEndpoint` appears. Fresh databases
/// already get the collection from `schemas::ALL`; upgraded databases add it
/// here.
pub async fn ensure_reciprocal_conversation_intent_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    if node
        .get_collection("ReciprocalConversationIntent")
        .context("get ReciprocalConversationIntent collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::RECIPROCAL_CONVERSATION_INTENT)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add ReciprocalConversationIntent schema"),
    }
}

/// Idempotent migration ensuring `InferenceProfile` carries the #648
/// per-completion retry fields on upgraded stores. Fresh databases get them
/// from the schema; homes initialized before #648 fail at server start with
/// `Cannot query field "retry_max_transport" on type "InferenceProfile"`
/// without this patch (the startup profile load selects every retry column).
pub async fn ensure_inference_profile_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    let Some(collection) = node
        .get_collection("InferenceProfile")
        .context("get InferenceProfile collection")?
    else {
        return match node
            .add_schema(defra_agent_protocol::schemas::INFERENCE_PROFILE)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if error.to_string().contains("already exists") => Ok(()),
            Err(error) => Err(error).context("add InferenceProfile schema"),
        };
    };

    let missing: Vec<&(&str, &str)> = INFERENCE_PROFILE_ADDITIVE_FIELDS
        .iter()
        .filter(|(name, _)| !collection_has_field(&collection, name))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    let operations = missing
        .iter()
        .map(|(name, kind)| {
            format!(
                r#"{{"op":"add","path":"/InferenceProfile/Fields/-","value":{{"Name":"{name}","Kind":"{kind}"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let patch = format!("[{operations}]");
    let next = node
        .patch_collection("InferenceProfile", &patch)
        .await
        .context("patch_collection InferenceProfile additive fields")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version InferenceProfile additive fields")?;
    tracing::info!(
        version = %next.version_id,
        added = missing.len(),
        "InferenceProfile patched with additive fields"
    );
    Ok(())
}

/// Add and type-check the cache-read accounting field on upgraded stores.
///
/// The bundled-schema sweep is a final backstop for missing scalar fields, but
/// this inference write-path field is release-gated explicitly: completion
/// persistence writes it unconditionally and durable-goal usage selects it.
pub async fn ensure_inference_call_cached_input_tokens_migration(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    let Some(collection) = node
        .get_collection("InferenceCall")
        .context("get InferenceCall collection")?
    else {
        return match node
            .add_schema(defra_agent_protocol::schemas::INFERENCE_CALL)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if error.to_string().contains("already exists") => Ok(()),
            Err(error) => Err(error).context("add InferenceCall schema"),
        };
    };

    if collection_has_field(&collection, "cached_input_tokens") {
        ensure_field_kind_matches_reference(
            &collection,
            "InferenceCall",
            "cached_input_tokens",
            &["prompt_tokens", "completion_tokens", "call_seq", "attempt"],
            "Int",
        )?;
        return Ok(());
    }

    let next = node
        .patch_collection(
            "InferenceCall",
            ADD_INFERENCE_CALL_CACHED_INPUT_TOKENS_PATCH,
        )
        .await
        .context("patch_collection InferenceCall cached input tokens")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version InferenceCall cached input tokens")?;
    let migrated = node
        .get_collection("InferenceCall")
        .context("reload InferenceCall after cached input migration")?
        .context("InferenceCall disappeared after cached input migration")?;
    ensure_field_kind_matches_reference(
        &migrated,
        "InferenceCall",
        "cached_input_tokens",
        &["prompt_tokens", "completion_tokens", "call_seq", "attempt"],
        "Int",
    )?;
    tracing::info!(
        version = %next.version_id,
        "InferenceCall patched with cached_input_tokens"
    );
    Ok(())
}

/// Idempotent migration ensuring the `PairingBearerClaim` collection exists.
/// Claimant devices push these rows to the invite issuer; the bearer-claim
/// reconciler validates and consumes them. The rows themselves grant nothing.
/// Fresh databases get the collection from `schemas::ALL`; upgraded databases
/// add it here.
pub async fn ensure_pairing_bearer_claim_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("PairingBearerClaim")
        .context("get PairingBearerClaim collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::PAIRING_BEARER_CLAIM)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add PairingBearerClaim schema"),
    }
}

/// Idempotent migration ensuring the `AgentNetwork` control-plane collection
/// exists (cut-2 network membership, Task 1). A fresh database created from
/// `schemas::ALL` already has this collection, so the migration is a no-op
/// there; it only adds the schema on a database upgraded from before this
/// landed (mirrors `ensure_consumed_invite_nonce_migrations`).
pub async fn ensure_agent_network_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("AgentNetwork")
        .context("get AgentNetwork collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::AGENT_NETWORK)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add AgentNetwork schema"),
    }
}

/// Idempotent migration ensuring the `NetworkMembership` control-plane
/// collection exists (cut-2 network membership, Task 1). No-op on a fresh
/// database (the collection comes from `schemas::ALL`); adds the schema on an
/// upgraded database.
pub async fn ensure_network_membership_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("NetworkMembership")
        .context("get NetworkMembership collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::NETWORK_MEMBERSHIP)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add NetworkMembership schema"),
    }
}

/// Idempotent migration ensuring the `PeerEndpoint` control-plane collection
/// exists (cut-2 network membership, Task 1). No-op on a fresh database (the
/// collection comes from `schemas::ALL`); adds the schema on an upgraded
/// database.
pub async fn ensure_peer_endpoint_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("PeerEndpoint")
        .context("get PeerEndpoint collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::PEER_ENDPOINT)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add PeerEndpoint schema"),
    }
}

/// Idempotent migration ensuring the `AgentToolApproval` collection exists.
/// No-op on a fresh database (the collection comes from `schemas::ALL`); adds
/// the schema on an upgraded database.
pub async fn ensure_agent_tool_approval_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("AgentToolApproval")
        .context("get AgentToolApproval collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::AGENT_TOOL_APPROVAL)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add AgentToolApproval schema"),
    }
}

/// Idempotent migration ensuring the `NetworkJoinRequest` control-plane
/// collection exists (cut-2 network membership, Task 1). No-op on a fresh
/// database (the collection comes from `schemas::ALL`); adds the schema on an
/// upgraded database.
pub async fn ensure_network_join_request_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node
        .get_collection("NetworkJoinRequest")
        .context("get NetworkJoinRequest collection")?
        .is_some()
    {
        return Ok(());
    }

    match node
        .add_schema(defra_agent_protocol::schemas::NETWORK_JOIN_REQUEST)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add NetworkJoinRequest schema"),
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

/// Idempotent migration for AgentBehavior: adds nullable string fields,
/// `skill_refs`, and `skill_excludes` to upgraded databases that predate them.
/// Existing DBs need these fields patched in so GraphQL reads/writes
/// referencing them do not fail.
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
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"description","Kind":"String"}}"#,
        );
    }
    if !collection_has_field(&collection, "summary") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"summary","Kind":"String"}}"#,
        );
    }
    if !collection_has_field(&collection, "request_context_template") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"request_context_template","Kind":"String"}}"#,
        );
    }
    // `skill_refs` and `skill_excludes` are selected by the AgentBehavior load
    // query; an old-schema DB missing either fails reads with
    // `Cannot query field "..."`. Kind 21 is `[String]`, matching
    // `subagent_targets`.
    if !collection_has_field(&collection, "skill_refs") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"skill_refs","Kind":"[String]"}}"#,
        );
    }
    if !collection_has_field(&collection, "skill_excludes") {
        field_patches.push(
            r#"{"op":"add","path":"/AgentBehavior/Fields/-","value":{"Name":"skill_excludes","Kind":"[String]"}}"#,
        );
    }

    if field_patches.is_empty() {
        tracing::debug!(
            "AgentBehavior already has description, summary, request_context_template, skill_refs, and skill_excludes fields; migration no-op"
        );
        return Ok(());
    }

    let patch = format!("[{}]", field_patches.join(","));
    let next = node
        .patch_collection("AgentBehavior", &patch)
        .await
        .context("patch_collection AgentBehavior prompt/context/skill fields")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version AgentBehavior prompt/context/skill fields")?;
    tracing::info!(
        version = %next.version_id,
        fields = ?field_patches,
        "AgentBehavior patched with description, summary, request_context_template, skill_refs, and skill_excludes fields"
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

/// Idempotent migration for AgentResponse: adds a monotonic reasoning-progress
/// counter used by streaming waiters. Existing DBs need this field patched in so
/// reasoning-only output can signal activity even after the bounded live
/// reasoning preview stops changing.
pub async fn ensure_agent_response_reasoning_progress_migration(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    let Some(collection) = node
        .get_collection("AgentResponse")
        .context("get AgentResponse collection")?
    else {
        return Ok(());
    };
    if let Some(existing_kind) = field_kind_value(&collection, "reasoning_progress_seq") {
        // The field already exists. Guard its TYPE, not just its presence: a
        // wrong kind (e.g. an out-of-band manual schema patch that added it as
        // an array instead of scalar Int) makes every create_AgentResponse fail
        // with `Expected array, got: Number(0)` and silently breaks the whole
        // runtime on the first request. Compare against a stable scalar-Int
        // sibling as the reference kind, so we don't hard-code defradb.rs's kind
        // encoding; if none is present we can't validate and fall back to
        // presence-only. The bundled sweep below performs the repair (#661).
        let reference_kind = [
            "progress_seq",
            "token_count",
            "materialized_message_sequence",
        ]
        .iter()
        .find_map(|name| field_kind_value(&collection, name));
        if let Some(expected_kind) = reference_kind {
            if existing_kind != expected_kind {
                // The schema-driven sweep runs after all hand-written
                // migrations and repairs scalar Kind mismatches. Do not abort
                // before it gets that chance: this is the exact v0.6.5 fleet
                // incident where Kind 5 created `[Int]` instead of `Int`.
                // Keep this at error severity: the bundled sweep is the
                // backstop that must repair the mismatch before runtime
                // queries or writes begin. Its parser exhaustiveness tests pin
                // `reasoning_progress_seq: Int` to a supported Kind.
                tracing::error!(
                    collection = "AgentResponse",
                    field = "reasoning_progress_seq",
                    old_kind = %existing_kind,
                    new_kind = %expected_kind,
                    "field kind mismatch deferred to bundled schema sweep"
                );
            }
        }
        return Ok(());
    }

    let next = node
        .patch_collection("AgentResponse", ADD_AGENT_RESPONSE_REASONING_PROGRESS_PATCH)
        .await
        .context("patch_collection AgentResponse reasoning progress field")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version AgentResponse reasoning progress field")?;
    tracing::info!(
        version = %next.version_id,
        "AgentResponse patched with reasoning_progress_seq field"
    );
    Ok(())
}

/// Add and validate the persisted terminal-convergence scheduling fields.
///
/// Row backfill is deliberately separate because this schema migration also
/// runs in generic CLI and desktop processes that do not own every replicated
/// `AgentRequest` in the local store. Only an owner runtime with its concrete
/// DID may call [`backfill_agent_request_terminal_durability`].
pub async fn ensure_agent_request_terminal_durability_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    let Some(collection) = node
        .get_collection("AgentRequest")
        .context("get AgentRequest collection")?
    else {
        return Ok(());
    };

    ensure_field_kind_matches_reference(
        &collection,
        "AgentRequest",
        "terminalized_at",
        &["created_at"],
        "String",
    )?;
    ensure_field_kind_matches_reference(
        &collection,
        "AgentRequest",
        "terminal_redrive_attempts",
        &["retry_count", "max_retries"],
        "Int",
    )?;

    let mut missing_field_patches = Vec::new();
    if !collection_has_field(&collection, "terminalized_at") {
        missing_field_patches.push(ADD_AGENT_REQUEST_TERMINALIZED_AT_FIELD);
    }
    if !collection_has_field(&collection, "terminal_redrive_attempts") {
        missing_field_patches.push(ADD_AGENT_REQUEST_TERMINAL_REDRIVE_ATTEMPTS_FIELD);
    }
    if !missing_field_patches.is_empty() {
        let patch = format!("[{}]", missing_field_patches.join(","));
        let next = node
            .patch_collection("AgentRequest", &patch)
            .await
            .context("patch_collection AgentRequest terminal durability fields")?;
        node.set_active_collection_version(&next.version_id)
            .await
            .context("set_active_collection_version AgentRequest terminal durability fields")?;
        tracing::info!(
            version = %next.version_id,
            "AgentRequest patched with terminal durability fields"
        );
    }

    Ok(())
}

/// Add and validate the immutable request-party routing key (#683).
///
/// Existing rows remain null: the pinned DefraDB correctly rejects null-to-
/// value writes on a patch-added immutable field, so legacy documents cannot
/// be reclassified into a peer scope. New cross-deployment children stamp the
/// key at create time.
pub async fn ensure_agent_request_requester_did_migration(node: Arc<EmbeddedNode>) -> Result<()> {
    let Some(collection) = node
        .get_collection("AgentRequest")
        .context("get AgentRequest collection")?
    else {
        return Ok(());
    };

    ensure_field_kind_matches_reference(
        &collection,
        "AgentRequest",
        "requester_did",
        &["agent_did"],
        "String",
    )?;

    if collection_has_field(&collection, "requester_did") {
        anyhow::ensure!(
            field_is_immutable(&collection, "requester_did"),
            "AgentRequest.requester_did exists but is not immutable; filtered request-party replication cannot be installed safely"
        );
        return Ok(());
    }

    let next = node
        .patch_collection(
            "AgentRequest",
            &format!("[{ADD_AGENT_REQUEST_REQUESTER_DID_FIELD}]"),
        )
        .await
        .context("patch_collection AgentRequest requester_did")?;
    node.set_active_collection_version(&next.version_id)
        .await
        .context("set_active_collection_version AgentRequest requester_did")?;
    tracing::info!(
        version = %next.version_id,
        "AgentRequest patched with immutable requester_did route key"
    );
    Ok(())
}

/// Add and validate the immutable requester route key on every returned child
/// artifact collection (#713). Existing rows remain null and therefore cannot
/// be reclassified into a coordinator's return scope.
pub async fn ensure_subagent_return_requester_did_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<()> {
    for collection_name in SUBAGENT_RETURN_ARTIFACT_COLLECTIONS {
        let Some(collection) = node
            .get_collection(collection_name)
            .with_context(|| format!("get {collection_name} collection"))?
        else {
            continue;
        };

        ensure_field_kind_matches_reference(
            &collection,
            collection_name,
            "requester_did",
            &["agent_did"],
            "String",
        )?;

        if collection_has_field(&collection, "requester_did") {
            anyhow::ensure!(
                field_is_immutable(&collection, "requester_did"),
                "{collection_name}.requester_did exists but is not immutable; filtered subagent-return replication cannot be installed safely"
            );
            continue;
        }

        let patch = format!(
            r#"[{{"op":"add","path":"/{collection_name}/Fields/-","value":{{"Name":"requester_did","Kind":"String","Immutable":true}}}}]"#
        );
        let next = node
            .patch_collection(collection_name, &patch)
            .await
            .with_context(|| format!("patch_collection {collection_name} requester_did"))?;
        node.set_active_collection_version(&next.version_id)
            .await
            .with_context(|| {
                format!("set_active_collection_version {collection_name} requester_did")
            })?;
        tracing::info!(
            collection = collection_name,
            version = %next.version_id,
            "subagent return artifact patched with immutable requester_did route key"
        );
    }
    Ok(())
}

/// Backfill terminal scheduling metadata for requests owned by `agent_did`.
///
/// The owner filter is present on both the page query and each guarded update:
/// replicated foreign requests must remain byte-for-byte untouched by startup
/// migration. The bounded pages are resumable; legacy terminal timestamps use
/// `created_at` as the only durable ordering fact available.
pub async fn backfill_agent_request_terminal_durability(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<()> {
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "terminal durability backfill requires a non-empty owner agent_did"
    );
    let escaped_agent_did = crate::graphql::escape_graphql_string(agent_did);

    loop {
        let query = format!(
            r#"{{
            AgentRequest(
                filter: {{
                    _and: [
                        {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                        {{ lifecycle_state: {{ _in: ["completed", "failed", "superseded", "dead", "interrupted"] }} }},
                        {{ _or: [
                            {{ terminalized_at: {{ _eq: null }} }},
                            {{ terminal_redrive_attempts: {{ _eq: null }} }}
                        ] }}
                    ]
                }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}],
                limit: 64
            ) {{
                _docID
                request_id
                created_at
                terminalized_at
                terminal_redrive_attempts
            }}
        }}"#
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "querying legacy AgentRequest terminal durability rows: {:?}",
                response.errors
            );
        }
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            break;
        }

        for row in rows {
            let doc_id = row
                .get("_docID")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if doc_id.is_empty() {
                continue;
            }
            let request_id = row
                .get("request_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let terminalized_at = row
                .get("created_at")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            let doc_id = crate::graphql::escape_graphql_string(doc_id);
            let terminalized_at = crate::graphql::escape_graphql_string(&terminalized_at);
            let mut assignments = Vec::new();
            if row
                .get("terminalized_at")
                .is_none_or(serde_json::Value::is_null)
            {
                assignments.push(format!("terminalized_at: \"{terminalized_at}\""));
            }
            if row
                .get("terminal_redrive_attempts")
                .is_none_or(serde_json::Value::is_null)
            {
                assignments.push("terminal_redrive_attempts: 0".to_string());
            }
            if assignments.is_empty() {
                continue;
            }
            let mutation = format!(
                r#"mutation {{
                    update_AgentRequest(
                        filter: {{
                            _and: [
                                {{ _docID: {{ _eq: "{doc_id}" }} }},
                                {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                                {{ _or: [
                                    {{ terminalized_at: {{ _eq: null }} }},
                                    {{ terminal_redrive_attempts: {{ _eq: null }} }}
                                ] }}
                            ]
                        }},
                        input: {{
                            {assignments}
                        }}
                    ) {{ _docID }}
                }}"#,
                assignments = assignments.join(", ")
            );
            let response = node.execute(&mutation).await;
            if response.has_errors() {
                anyhow::bail!(
                    "backfilling terminal durability for AgentRequest {request_id}: {:?}",
                    response.errors
                );
            }
        }
    }

    Ok(())
}

/// Per-collection outcome of the legacy `agent_did` scope-key reconciliation
/// (Finding #11): how many rows were backfilled from their owning record, and
/// how many remain unscoped (their `agent_did` is still null after the field was
/// added) and so are excluded from DID-scoped replication.
#[derive(Debug, Default)]
struct ScopeKeyBackfillReport {
    /// `(collection, backfilled_count, unscoped_count)` in processing order.
    entries: Vec<(&'static str, usize, usize)>,
}

impl ScopeKeyBackfillReport {
    fn record(&mut self, collection: &'static str, backfilled: usize, unscoped: usize) {
        self.entries.push((collection, backfilled, unscoped));
    }

    #[cfg(test)]
    fn backfilled_for(&self, collection: &str) -> usize {
        self.entries
            .iter()
            .find(|(name, _, _)| *name == collection)
            .map(|(_, backfilled, _)| *backfilled)
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn unscoped_for(&self, collection: &str) -> usize {
        self.entries
            .iter()
            .find(|(name, _, _)| *name == collection)
            .map(|(_, _, unscoped)| *unscoped)
            .unwrap_or(0)
    }

    /// Emit one warning per collection that still has unscoped legacy rows, so
    /// the operator sees the consequence (the count) rather than a silent drop.
    fn warn_unscoped(&self) {
        for (collection, _, unscoped) in &self.entries {
            if *unscoped > 0 {
                tracing::warn!(
                    collection,
                    unscoped_rows = unscoped,
                    "legacy rows predate the immutable agent_did scope key and \
                     remain null (this defradb pin rejects writing a value to a \
                     newly-@immutable field even on its first write); they are \
                     excluded from DID-scoped replication — re-create or re-scope \
                     them to include"
                );
            }
        }
    }
}

/// Reconcile the freshly-added `agent_did` scope key on legacy conversation rows
/// (Finding #11) and report the outcome per collection.
///
/// INTENT was to BACKFILL each null row from its owning record — children
/// (AgentMessage/AgentToolCall/CompactionEntry) via `session_id` →
/// AgentSession.agent_did, and AgentSession itself via its AgentRequest lineage —
/// with a single first write while the field is brand-new in this migration
/// window. The owner resolution below is real and exercised.
///
/// EMPIRICAL CONSTRAINT (this defradb pin): a write that sets a value on a
/// newly-`@immutable` field of a pre-existing document is REJECTED with
/// "immutable field 'agent_did' cannot be changed" — the immutability check
/// fires on null→value, not just value→value, so the document's history null is
/// treated as a prior write. Backfill is therefore impossible here; every
/// resolvable row falls through to the unscoped count and the per-collection
/// warning. If an upstream version distinguishes the first write, the same code
/// path will start reporting `backfilled` without further change.
///
/// Idempotent and immutability-safe: only rows whose `agent_did` is currently
/// null are even considered, so a row that already carries the key is never
/// re-written.
async fn backfill_conversation_scope_keys(node: &EmbeddedNode) -> ScopeKeyBackfillReport {
    let mut report = ScopeKeyBackfillReport::default();

    // session_id → owning agent_did, recovered from AgentRequest lineage and any
    // AgentSession that already carries the key.
    let owner = build_session_owner_map(node).await;

    // AgentSession is the owning record; the three children key on `session_id`.
    // All four resolve their DID through the same session→owner map.
    for collection in [
        "AgentSession",
        "AgentMessage",
        "AgentToolCall",
        "CompactionEntry",
    ] {
        let (filled, unscoped) = reconcile_scope_key(node, collection, &owner).await;
        report.record(collection, filled, unscoped);
    }

    report
}

/// Map `session_id` → owning `agent_did` from AgentRequest lineage and from any
/// AgentSession that already carries the key. Only non-empty DIDs are recorded,
/// so a session with no resolvable owner is absent (its rows count as unscoped).
async fn build_session_owner_map(node: &EmbeddedNode) -> std::collections::HashMap<String, String> {
    let mut owner: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let trimmed_did = |row: &serde_json::Value| -> Option<String> {
        row.get("agent_did")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let session_of = |row: &serde_json::Value| -> Option<String> {
        row.get("session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    // AgentRequest lineage first (the source of truth for legacy sessions).
    let requests = read_rows(node, "AgentRequest", "session_id agent_did").await;
    for row in &requests {
        if let (Some(session), Some(did)) = (session_of(row), trimmed_did(row)) {
            owner.entry(session).or_insert(did);
        }
    }
    // Any AgentSession that already carries its own DID seeds/overrides the map.
    let sessions = read_rows(node, "AgentSession", "session_id agent_did").await;
    for row in &sessions {
        if let (Some(session), Some(did)) = (session_of(row), trimmed_did(row)) {
            owner.insert(session, did);
        }
    }

    owner
}

/// Read every row of `collection` selecting `_docID` plus the given fields.
async fn read_rows(
    node: &EmbeddedNode,
    collection: &str,
    select_fields: &str,
) -> Vec<serde_json::Value> {
    let query = format!("query {{ {collection} {{ _docID {select_fields} }} }}");
    let response = node.execute(&query).await;
    if response.has_errors() {
        // A collection/field that simply doesn't exist (e.g. a fresh or
        // config-only home that never created this conversation collection) is
        // not a backfill failure: there are zero legacy rows to scope. DefraDB
        // surfaces that as `Cannot query field "..."`. Treat it as "not
        // applicable" and skip silently at debug — the migration runs on every
        // CLI command, so this must not emit a WARN that would corrupt
        // `--output json` stdout. Any OTHER error is a genuine read failure and
        // still warns.
        let absent = response
            .errors
            .iter()
            .any(|error| error.message.contains("Cannot query field"));
        if absent {
            tracing::debug!(
                collection,
                "scope-key backfill: collection/field absent; no rows to scope"
            );
        } else {
            tracing::warn!(
                collection,
                errors = ?response.errors,
                "scope-key backfill read failed"
            );
        }
        return Vec::new();
    }
    response
        .data
        .as_ref()
        .and_then(|d| d.get(collection))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// Reconcile `collection`'s `agent_did` against `owner_map[session_id]`. Returns
/// `(backfilled, unscoped)`. Rows already carrying a non-empty DID are skipped
/// (idempotent; never a second write to the immutable field). Each null row whose
/// owner resolves is offered a single first write; if defradb accepts it the row
/// counts as backfilled, otherwise (and for rows with no resolvable owner) it
/// counts as unscoped. The whole-collection update failure is logged once at
/// debug, not per row, so a pin that universally rejects the first write does not
/// flood the log — the operator-facing signal is the per-collection warn count.
async fn reconcile_scope_key(
    node: &EmbeddedNode,
    collection: &'static str,
    owner_map: &std::collections::HashMap<String, String>,
) -> (usize, usize) {
    let rows = read_rows(node, collection, "session_id agent_did").await;
    let mut backfilled = 0usize;
    let mut unscoped = 0usize;
    let mut logged_reject = false;

    for row in rows {
        let already_scoped = row
            .get("agent_did")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if already_scoped {
            continue;
        }
        let Some(doc_id) = row.get("_docID").and_then(|v| v.as_str()) else {
            continue;
        };
        let session = row
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(did) = session.and_then(|s| owner_map.get(s)) else {
            // No resolvable owner: never guess. Counted + warned by the caller.
            unscoped += 1;
            continue;
        };

        let mutation = format!(
            r#"mutation {{ update_{collection}(
                filter: {{ _docID: {{ _eq: "{}" }} }},
                input: {{ agent_did: "{}" }}
            ) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(doc_id),
            crate::graphql::escape_graphql_string(did),
        );
        let update = node.execute(&mutation).await;
        if update.has_errors() {
            if !logged_reject {
                logged_reject = true;
                tracing::debug!(
                    collection,
                    errors = ?update.errors,
                    "scope-key backfill rejected by defradb (likely the immutable \
                     first-write constraint); affected rows counted as unscoped"
                );
            }
            unscoped += 1;
            continue;
        }
        backfilled += 1;
    }

    (backfilled, unscoped)
}

/// Idempotent migration ensuring every conversation collection carries an
/// `@immutable` `agent_did` scope key, the field filtered replication (#1033)
/// scopes on (and rejects unless immutable).
///
/// Two groups, both required on an UPGRADED database:
///
/// 1. The four collections that key on `session_id` and historically lacked
///    `agent_did` entirely (AgentMessage, AgentToolCall, AgentSession,
///    CompactionEntry) get the field ADDED as an immutable nullable String, then
///    `backfill_conversation_scope_keys` reconciles legacy rows (Finding #11).
///    The intent is to BACKFILL each null row from its owning record — children
///    via `session_id` → AgentSession.agent_did, AgentSession via its
///    AgentRequest lineage — but on the current defradb pin a write that sets a
///    value on a newly-`@immutable` field of a pre-existing document is rejected
///    ("immutable field 'agent_did' cannot be changed"; the constraint fires on
///    null→value, not only value→value). So today every legacy row stays null and
///    is COUNTED and WARNED about per collection — excluded from DID-scoped
///    replication, surfaced rather than silently dropped, and recoverable only by
///    re-creating or re-scoping the row. (The backfill path remains wired so a
///    future pin that permits the first write needs no further change.)
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

    // Group 1 (cont.): backfill the freshly-added scope key on legacy rows from
    // their owning record, and surface (with a per-collection count + warning)
    // any row whose owner cannot be resolved (Finding #11). Safe to run every
    // startup: rows already scoped are skipped, so it self-heals a backfill that
    // crashed midway and never re-writes an immutable value.
    let report = backfill_conversation_scope_keys(&node).await;
    report.warn_unscoped();

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
/// Map a bundled-SDL scalar type to its DefraDB `FieldKind` code for additive
/// field patches. Only the flat scalar/array types the agent schemas use are
/// mapped; anything else (relations, embedded objects) returns `None` and is
/// skipped with a warning — those still need a hand-written migration.
fn sdl_type_to_field_kind(sdl_type: &str) -> Option<u8> {
    // Strip the outer non-null marker: a field added by an additive patch is
    // necessarily nillable for pre-existing rows, so `[String!]!` patches as a
    // `[String!]` (StringArray) and `String!` as a `String`.
    let sdl_type = sdl_type.strip_suffix('!').unwrap_or(sdl_type);
    match sdl_type {
        "Boolean" => Some(2),
        "Int" => Some(4),
        "Float" => Some(6),
        "DateTime" => Some(10),
        "String" => Some(11),
        "Blob" => Some(13),
        "JSON" => Some(14),
        "[Boolean!]" => Some(3),
        "[Int!]" => Some(5),
        "[Float!]" => Some(7),
        "[String!]" => Some(12),
        "[Boolean]" => Some(18),
        "[Int]" => Some(19),
        "[Float]" => Some(20),
        "[String]" => Some(21),
        _ => None,
    }
}

fn field_kind_to_patch_name(kind: u8) -> Option<&'static str> {
    match kind {
        2 => Some("Boolean"),
        3 => Some("[Boolean!]"),
        4 => Some("Int"),
        5 => Some("[Int!]"),
        6 => Some("Float"),
        7 => Some("[Float!]"),
        10 => Some("DateTime"),
        11 => Some("String"),
        12 => Some("[String!]"),
        13 => Some("Blob"),
        14 => Some("JSON"),
        18 => Some("[Boolean]"),
        19 => Some("[Int]"),
        20 => Some("[Float]"),
        21 => Some("[String]"),
        _ => None,
    }
}

/// Parse the collections and scalar fields out of a bundled SDL document.
/// Deliberately line-based and conservative: the bundled agent/inference
/// schemas are flat `type Name { field: Type @directives }` declarations with
/// comments; anything that does not match that shape is ignored.
fn parse_sdl_fields(sdl: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut collections = Vec::new();
    let mut current: Option<(String, Vec<(String, String)>)> = None;
    for raw in sdl.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("type ") {
            let name = rest
                .split(|c: char| c.is_whitespace() || c == '{' || c == '@')
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                current = Some((name, Vec::new()));
            }
            continue;
        }
        if line.starts_with('}') {
            if let Some(done) = current.take() {
                collections.push(done);
            }
            continue;
        }
        let Some((_, fields)) = current.as_mut() else {
            continue;
        };
        let Some((field_name, rest)) = line.split_once(':') else {
            continue;
        };
        let field_name = field_name.trim();
        if field_name.is_empty()
            || !field_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        let field_type = rest
            .trim()
            .split(|c: char| c.is_whitespace() || c == '@')
            .next()
            .unwrap_or("")
            .to_string();
        if !field_type.is_empty() {
            fields.push((field_name.to_string(), field_type));
        }
    }
    if let Some(done) = current.take() {
        collections.push(done);
    }
    collections
}

fn bundled_schema_sdls() -> impl Iterator<Item = &'static str> {
    defra_agent_protocol::schemas::ALL
        .iter()
        .chain(defra_agent_protocol::schemas::RUNTIME_ALL.iter())
        .copied()
}

// DefraDB rejects mutating an existing field in place. A Kind repair therefore
// replaces the field across two schema versions: first rename it to this
// temporary name, then rename it back with the bundled Kind. Each rename is
// treated as a remove+add and receives a proper new field CID. The deterministic
// prefix also lets the next boot finish phase two if startup stopped between
// the patches. Replacing the FieldID does not carry over values stored under the
// old field, so a Kind repair discards that field's existing values. This path
// exists for corrupted schemas such as #661, whose wrong-Kind field was already
// unusable; intentional data-bearing retypes require a hand-written migration.
const KIND_REPAIR_FIELD_PREFIX: &str = "defra_agent_kind_repair_";

async fn finish_bundled_field_kind_repair(
    node: &EmbeddedNode,
    collection_name: &str,
    field_index: usize,
    temporary_name: &str,
    field_name: &str,
    old_kind: &serde_json::Value,
    new_kind: u8,
) -> Result<()> {
    let new_kind_name = field_kind_to_patch_name(new_kind)
        .with_context(|| format!("unsupported bundled field kind {new_kind}"))?;
    let patch = format!(
        r#"[
            {{"op":"remove","path":"/{collection_name}/Fields/{field_index}/FieldID"}},
            {{"op":"replace","path":"/{collection_name}/Fields/{field_index}/Name","value":"{field_name}"}},
            {{"op":"replace","path":"/{collection_name}/Fields/{field_index}/Kind","value":"{new_kind_name}"}}
        ]"#
    );
    let next = node
        .patch_collection(collection_name, &patch)
        .await
        .with_context(|| {
            format!(
                "finish bundled field kind repair {collection_name}.{field_name} from {temporary_name}"
            )
        })?;
    node.set_active_collection_version(&next.version_id)
        .await
        .with_context(|| {
            format!("activate bundled field kind repair {collection_name}.{field_name}")
        })?;
    tracing::warn!(
        collection = %collection_name,
        field = %field_name,
        old_kind = %old_kind,
        new_kind,
        version = %next.version_id,
        stored_values_discarded = true,
        "bundled schema field kind repaired"
    );
    Ok(())
}

async fn repair_bundled_field_kind(
    node: &EmbeddedNode,
    collection_name: &str,
    field_index: usize,
    field_name: &str,
    old_kind: &serde_json::Value,
    new_kind: u8,
) -> Result<()> {
    let temporary_name = format!("{KIND_REPAIR_FIELD_PREFIX}{field_name}");
    let patch = format!(
        r#"[
            {{"op":"remove","path":"/{collection_name}/Fields/{field_index}/FieldID"}},
            {{"op":"replace","path":"/{collection_name}/Fields/{field_index}/Name","value":"{temporary_name}"}}
        ]"#
    );
    let intermediate = node
        .patch_collection(collection_name, &patch)
        .await
        .with_context(|| {
            format!("begin bundled field kind repair {collection_name}.{field_name}")
        })?;
    node.set_active_collection_version(&intermediate.version_id)
        .await
        .with_context(|| {
            format!("activate intermediate field kind repair {collection_name}.{field_name}")
        })?;

    finish_bundled_field_kind_repair(
        node,
        collection_name,
        field_index,
        &temporary_name,
        field_name,
        old_kind,
        new_kind,
    )
    .await
}

/// Schema-driven additive field reconciliation: for every bundled collection
/// that exists in this store, diff the live fields against the bundled SDL and
/// patch in whatever is missing. This is the fix for the recurring upgrade
/// bug class where a schema gains a field, the startup queries select it, and
/// no hand-written migration ships (`tool_policy_version`, `write_tools`,
/// `retry_max_transport`, ...): the bundled schema itself is now the migration
/// source of truth. Missing fields are added; scalar Kind mismatches are
/// replaced across two recoverable schema versions. A Kind repair assigns a new
/// FieldID and therefore discards values stored under the mismatched field; it
/// is intended to recover unusable, incorrectly patched fields such as #661,
/// not to migrate intentional data-bearing retypes. Fields are never silently
/// removed, and unmapped relation/object kinds still require a hand migration.
pub async fn ensure_bundled_schema_fields(node: Arc<EmbeddedNode>) -> Result<()> {
    for sdl in bundled_schema_sdls() {
        for (collection_name, fields) in parse_sdl_fields(sdl) {
            let Some(mut collection) = node
                .get_collection(&collection_name)
                .with_context(|| format!("get {collection_name} collection"))?
            else {
                continue;
            };

            // Resume a repair interrupted between its two schema versions.
            // This must run before missing-field detection, otherwise the
            // original field would be appended and the temporary field left
            // behind.
            loop {
                let interrupted =
                    collection
                        .fields
                        .iter()
                        .enumerate()
                        .find_map(|(index, field)| {
                            let field_name = field.name.strip_prefix(KIND_REPAIR_FIELD_PREFIX)?;
                            let (_, field_type) = fields
                                .iter()
                                .find(|(expected_name, _)| expected_name == field_name)?;
                            let new_kind = sdl_type_to_field_kind(field_type)?;
                            let old_kind = serde_json::to_value(&field.kind).ok()?;
                            Some((
                                index,
                                field.name.clone(),
                                field_name.to_string(),
                                old_kind,
                                new_kind,
                            ))
                        });
                let Some((index, temporary_name, field_name, old_kind, new_kind)) = interrupted
                else {
                    break;
                };
                anyhow::ensure!(
                    !collection_has_field(&collection, &field_name),
                    "cannot resume bundled field kind repair for {collection_name}.{field_name}: \
                     both the original and temporary field exist"
                );
                finish_bundled_field_kind_repair(
                    node.as_ref(),
                    &collection_name,
                    index,
                    &temporary_name,
                    &field_name,
                    &old_kind,
                    new_kind,
                )
                .await?;
                collection = node
                    .get_collection(&collection_name)
                    .with_context(|| format!("reload {collection_name} after resumed Kind repair"))?
                    .with_context(|| format!("{collection_name} disappeared during Kind repair"))?;
            }

            // A temporary field is resumable only while its original name is
            // still present in the bundled SDL with a mapped Kind. Surface any
            // orphan left by an interrupted repair followed by schema drift.
            for field in collection
                .fields
                .iter()
                .filter(|field| field.name.starts_with(KIND_REPAIR_FIELD_PREFIX))
            {
                let original_field = field
                    .name
                    .strip_prefix(KIND_REPAIR_FIELD_PREFIX)
                    .unwrap_or("");
                tracing::warn!(
                    collection = %collection_name,
                    field = %field.name,
                    original_field,
                    "orphaned bundled Kind-repair field cannot be resumed"
                );
            }

            // Correct existing scalar fields whose stored Kind disagrees with
            // the bundled SDL. Repairs run one at a time because each creates
            // and activates two schema versions.
            for (field_name, field_type) in &fields {
                let Some(kind) = sdl_type_to_field_kind(field_type) else {
                    tracing::warn!(
                        collection = %collection_name,
                        field = %field_name,
                        sdl_type = %field_type,
                        "bundled schema field has no scalar kind mapping; needs a hand-written migration"
                    );
                    continue;
                };
                let Some((field_index, field)) = collection
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, field)| field.name == field_name.as_str())
                else {
                    continue;
                };
                let old_kind = serde_json::to_value(&field.kind)
                    .context("serialize live bundled field kind")?;
                if old_kind == serde_json::json!(kind) {
                    continue;
                }
                repair_bundled_field_kind(
                    node.as_ref(),
                    &collection_name,
                    field_index,
                    field_name,
                    &old_kind,
                    kind,
                )
                .await?;
                collection = node
                    .get_collection(&collection_name)
                    .with_context(|| format!("reload {collection_name} after Kind repair"))?
                    .with_context(|| format!("{collection_name} disappeared during Kind repair"))?;
            }

            let mut operations = Vec::new();
            let mut additions = Vec::new();
            for (field_name, field_type) in &fields {
                if collection_has_field(&collection, field_name) {
                    continue;
                }
                let Some(kind) = sdl_type_to_field_kind(field_type) else {
                    continue;
                };
                let patch_kind = field_kind_to_patch_name(kind)
                    .expect("mapped bundled field kind has a canonical patch name");
                operations.push(format!(
                    r#"{{"op":"add","path":"/{collection_name}/Fields/-","value":{{"Name":"{field_name}","Kind":"{patch_kind}"}}}}"#
                ));
                additions.push((field_name, kind));
            }
            if operations.is_empty() {
                continue;
            }

            let patch = format!("[{}]", operations.join(","));
            let next = node
                .patch_collection(&collection_name, &patch)
                .await
                .with_context(|| format!("patch_collection {collection_name} bundled fields"))?;
            node.set_active_collection_version(&next.version_id)
                .await
                .with_context(|| {
                    format!("set_active_collection_version {collection_name} bundled fields")
                })?;
            for (field_name, new_kind) in additions {
                tracing::info!(
                    collection = %collection_name,
                    field = %field_name,
                    old_kind = "missing",
                    new_kind,
                    version = %next.version_id,
                    "bundled schema field added"
                );
            }
        }
    }
    Ok(())
}

pub async fn ensure_all_runtime_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    ensure_agent_runtime_executor_status_migrations(node.clone())
        .await
        .context("ensure AgentRuntime executor status migrations")?;
    ensure_peer_pairing_desired_migrations(node.clone())
        .await
        .context("ensure PeerPairingDesired migrations")?;
    ensure_data_plane_pairing_desired_migrations(node.clone())
        .await
        .context("ensure DataPlanePairingDesired migrations")?;
    ensure_peer_registry_migrations(node.clone())
        .await
        .context("ensure PeerRegistry migrations")?;
    ensure_consumed_invite_nonce_migrations(node.clone())
        .await
        .context("ensure ConsumedInviteNonce migrations")?;
    ensure_reciprocal_conversation_intent_migrations(node.clone())
        .await
        .context("ensure ReciprocalConversationIntent migrations")?;
    ensure_pairing_bearer_claim_migrations(node.clone())
        .await
        .context("ensure PairingBearerClaim migrations")?;
    ensure_inference_profile_migrations(node.clone())
        .await
        .context("ensure InferenceProfile migrations")?;
    ensure_inference_call_cached_input_tokens_migration(node.clone())
        .await
        .context("ensure InferenceCall cached input token migration")?;
    ensure_agent_network_migrations(node.clone())
        .await
        .context("ensure AgentNetwork migrations")?;
    ensure_network_membership_migrations(node.clone())
        .await
        .context("ensure NetworkMembership migrations")?;
    ensure_peer_endpoint_migrations(node.clone())
        .await
        .context("ensure PeerEndpoint migrations")?;
    ensure_network_join_request_migrations(node.clone())
        .await
        .context("ensure NetworkJoinRequest migrations")?;
    ensure_tool_service_registry_migrations(node.clone())
        .await
        .context("ensure ToolServiceRegistry migrations")?;
    ensure_tool_service_health_state_migrations(node.clone())
        .await
        .context("ensure ToolServiceHealthState migrations")?;
    ensure_agent_behavior_migrations(node.clone())
        .await
        .context("ensure AgentBehavior migrations")?;
    ensure_tool_call_migrations(node.clone())
        .await
        .context("ensure tool-call migrations")?;
    ensure_agent_tool_approval_migrations(node.clone())
        .await
        .context("ensure AgentToolApproval migrations")?;
    ensure_subagent_extensions_migrations(node.clone())
        .await
        .context("ensure subagent extension migrations")?;
    ensure_agent_response_reasoning_progress_migration(node.clone())
        .await
        .context("ensure AgentResponse reasoning progress migration")?;
    ensure_agent_request_requester_did_migration(node.clone())
        .await
        .context("ensure AgentRequest requester_did migration")?;
    ensure_conversation_scope_key_migrations(node.clone())
        .await
        .context("ensure conversation agent_did scope-key migrations")?;
    ensure_subagent_return_requester_did_migrations(node.clone())
        .await
        .context("ensure subagent return requester_did migrations")?;
    ensure_agent_request_terminal_durability_migrations(node.clone())
        .await
        .context("ensure AgentRequest terminal durability migrations")?;
    // LAST, after every hand-written migration: schema-driven reconciliation
    // sweeps up any bundled-schema field that has no hand-written patch (the
    // recurring upgrade-crash class: tool_policy_version, write_tools,
    // retry_max_transport, ...). Hand-written migrations run first so fields
    // with special handling keep it.
    ensure_bundled_schema_fields(node)
        .await
        .context("ensure bundled schema fields")?;
    Ok(())
}

#[cfg(test)]
mod patch_kind_tests {
    use super::*;
    use serde::Deserialize;

    const NILLABLE_STRING_ARRAY_KIND: &str = "[String]";
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
    const OLD_INFERENCE_CALL_SCHEMA: &str = r#"
        type InferenceCall {
            call_id: String @index(unique: true)
            request_id: String @index
            call_seq: Int
            prompt_tokens: Int
            completion_tokens: Int
        }
    "#;

    async fn test_node() -> Arc<EmbeddedNode> {
        Arc::new(EmbeddedNode::builder().build().await.unwrap())
    }

    fn field_kinds(patch_json: &str) -> Vec<(String, String)> {
        let ops: serde_json::Value = serde_json::from_str(patch_json).expect("patch is valid JSON");
        ops.as_array()
            .expect("patch is an array")
            .iter()
            .filter_map(|op| {
                let value = op.get("value")?;
                let name = value.get("Name")?.as_str()?.to_string();
                let kind = value
                    .get("Kind")?
                    .as_str()
                    .expect("migration patch Kind must use a canonical string")
                    .to_string();
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
                    "subagent_targets must use [String], got {kind}"
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
                "backgroundable_tool_names must use [String], got {kind}"
            );
        }
        for (name, kind) in field_kinds(ADD_TOOL_SELECTION_APPROVAL_REQUIRED_PATCH) {
            assert_eq!(
                name, "approval_required_tools",
                "unexpected field in approval-required patch"
            );
            assert_eq!(
                kind, NILLABLE_STRING_ARRAY_KIND,
                "approval_required_tools must use [String], got {kind}"
            );
        }
        for (name, kind) in field_kinds(ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH) {
            assert_eq!(name, "profiles", "unexpected field in profiles patch");
            assert_eq!(
                kind, NILLABLE_STRING_ARRAY_KIND,
                "profiles must use [String], got {kind}"
            );
        }
    }

    #[test]
    fn agent_tool_call_command_denial_string_arrays_use_nillable_string_array_kind() {
        for (name, kind) in field_kinds(ADD_AGENT_TOOL_CALL_COMMAND_DENIAL_PATCH) {
            if name == "denied_argv" || name == "denied_prefix" {
                assert_eq!(
                    kind, NILLABLE_STRING_ARRAY_KIND,
                    "{name} must use [String], got {kind}"
                );
            }
        }
    }

    #[test]
    fn all_static_patches_use_canonical_kind_strings() {
        for patch in [
            ADD_LIFECYCLE_STATE_PATCH,
            ADD_AGENT_TOOL_CALL_SUBAGENT_PATCH,
            ADD_AGENT_REQUEST_SUBAGENT_PATCH,
            ADD_TOOL_SELECTION_SUBAGENT_PATCH,
            ADD_TOOL_SELECTION_BACKGROUND_TOOLS_PATCH,
            ADD_AGENT_TOOL_CALL_R5_PATCH,
            ADD_AGENT_TOOL_CALL_WORKFLOW_PATCH,
            ADD_AGENT_TOOL_CALL_SPAWN_TARGET_DID_PATCH,
            ADD_AGENT_TOOL_CALL_COMMAND_DENIAL_PATCH,
            ADD_TOOL_SELECTION_R5_PATCH,
            ADD_TOOL_SELECTION_SESSION_HISTORY_PATCH,
            ADD_TOOL_SELECTION_DEFAULT_AWAIT_MODE_PATCH,
            ADD_TOOL_SELECTION_ORCHESTRATION_PATCH,
            ADD_TOOL_SELECTION_CONTEXT_BUDGET_PATCH,
            ADD_PEER_PAIRING_DESIRED_AGENT_DID_PATCH,
            ADD_PEER_PAIRING_DESIRED_PROFILES_PATCH,
            ADD_PEER_PAIRING_DESIRED_SOURCE_PATCH,
            ADD_PEER_PAIRING_DESIRED_TEMPLATE_PATCH,
            ADD_PEER_PAIRING_APPLIED_REPLICATOR_FILTER_PATCH,
            ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH,
            ADD_TOOL_SERVICE_HEALTH_STATE_TOOL_COUNT_PATCH,
            ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH,
            ADD_INFERENCE_CALL_CACHED_INPUT_TOKENS_PATCH,
            ADD_AGENT_MESSAGE_AGENT_DID_PATCH,
            ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH,
            ADD_AGENT_SESSION_AGENT_DID_PATCH,
            ADD_COMPACTION_ENTRY_AGENT_DID_PATCH,
        ] {
            for (name, kind) in field_kinds(patch) {
                assert_ne!(kind, "17", "field {name} uses unassigned Kind 17");
            }
        }
    }

    #[tokio::test]
    async fn inference_call_cached_input_tokens_migrates_populated_legacy_store() {
        let node = test_node().await;
        node.add_schema(OLD_INFERENCE_CALL_SCHEMA).await.unwrap();
        let seeded = node
            .execute(
                r#"mutation {
                    create_InferenceCall(input: {
                        call_id: "legacy-call",
                        request_id: "legacy-request",
                        call_seq: 1,
                        prompt_tokens: 100,
                        completion_tokens: 5
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !seeded.has_errors(),
            "legacy InferenceCall seed failed: {:?}",
            seeded.errors
        );

        ensure_inference_call_cached_input_tokens_migration(node.clone())
            .await
            .unwrap();
        let first_version = node
            .get_collection("InferenceCall")
            .unwrap()
            .expect("InferenceCall after migration")
            .version_id;
        ensure_inference_call_cached_input_tokens_migration(node.clone())
            .await
            .unwrap();
        let collection = node
            .get_collection("InferenceCall")
            .unwrap()
            .expect("InferenceCall after idempotent migration");
        assert_eq!(collection.version_id, first_version);
        assert!(collection_has_field(&collection, "cached_input_tokens"));
        ensure_field_kind_matches_reference(
            &collection,
            "InferenceCall",
            "cached_input_tokens",
            &["prompt_tokens", "completion_tokens"],
            "Int",
        )
        .unwrap();

        let write = node
            .execute(
                r#"mutation {
                    update_InferenceCall(
                        filter: { call_id: { _eq: "legacy-call" } },
                        input: { cached_input_tokens: 90 }
                    ) { call_id cached_input_tokens }
                }"#,
            )
            .await;
        assert!(
            !write.has_errors(),
            "migrated cached token write failed: {:?}",
            write.errors
        );
        let read = node
            .execute(
                r#"query {
                    InferenceCall(filter: { call_id: { _eq: "legacy-call" } }) {
                        call_id prompt_tokens completion_tokens cached_input_tokens
                    }
                }"#,
            )
            .await;
        assert!(
            !read.has_errors(),
            "migrated cached token read failed: {:?}",
            read.errors
        );
        assert_eq!(
            read.data
                .as_ref()
                .and_then(|data| data.get("InferenceCall"))
                .and_then(|rows| rows.as_array())
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("cached_input_tokens"))
                .and_then(|value| value.as_i64()),
            Some(90)
        );
    }

    #[test]
    fn conversation_scope_key_patches_use_nillable_string_kind() {
        const NILLABLE_STRING_KIND: &str = "String";
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
                    "agent_did scope key must use String, got {kind}"
                );
            }
        }
    }

    #[test]
    fn agent_behavior_description_summary_use_nillable_string_kind() {
        const NILLABLE_STRING_KIND: &str = "String";
        for (name, kind) in field_kinds(ADD_AGENT_BEHAVIOR_DESCRIPTION_SUMMARY_PATCH) {
            assert_eq!(
                kind, NILLABLE_STRING_KIND,
                "AgentBehavior field '{name}' must use String, got {kind}"
            );
        }
    }

    #[test]
    fn agent_runtime_executor_status_field_kinds_match_sdl() {
        let fields = field_kinds(ADD_AGENT_RUNTIME_EXECUTOR_STATUS_PATCH);
        assert_eq!(
            fields,
            vec![
                ("behavior_executor_capacity".to_string(), "Int".to_string()),
                (
                    "behavior_executor_queue_depth".to_string(),
                    "Int".to_string()
                ),
                (
                    "behavior_executor_status_json".to_string(),
                    "String".to_string()
                ),
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

    /// GAP-5: the `source` backfill defaults only BLANK rows to `"operator"`; a
    /// pre-existing `source = "registry"` row is preserved, never reassigned. The
    /// `source` partition is what the discovery/network reconcilers use for
    /// mutual exclusion (a registry-owned peer is excluded from network
    /// materialization and vice versa), so clobbering a legacy registry row to
    /// `operator` post-upgrade would silently change ownership. This fences the
    /// migration's coexistence with a populated network mesh.
    #[tokio::test]
    async fn pairing_source_backfill_preserves_registry_rows_only_defaults_blank() {
        #[derive(serde::Deserialize)]
        struct SourceRow {
            peer_id: String,
            source: Option<String>,
            template: Option<String>,
        }

        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        // A legacy registry-owned row (explicit source) and a pre-migration row
        // with no source/template stamped yet.
        let response = node
            .execute(
                r#"mutation {
                    legacy: create_PeerPairingDesired(input: {
                        peer_id: "peer-reg",
                        agent_did: "did:defra-agent:peer-reg",
                        collections: ["AgentNetwork"],
                        replicator_addresses: ["/ip4/127.0.0.1/tcp/5101/p2p/peer-reg"],
                        source: "registry",
                        template: "network-control"
                    }) { _docID }
                    blank: create_PeerPairingDesired(input: {
                        peer_id: "peer-blank",
                        agent_did: "did:defra-agent:peer-blank",
                        collections: ["AgentRequest"],
                        replicator_addresses: ["/ip4/127.0.0.1/tcp/5102/p2p/peer-blank"]
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seeding PeerPairingDesired rows failed: {:?}",
            response.errors
        );

        backfill_pairing_desired_defaults(node.as_ref()).await;

        let response = node
            .execute(r#"{ PeerPairingDesired { peer_id source template } }"#)
            .await;
        assert!(
            !response.has_errors(),
            "query failed: {:?}",
            response.errors
        );
        let rows: Vec<SourceRow> = response
            .data
            .as_ref()
            .and_then(|d| d.get("PeerPairingDesired"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let reg = rows
            .iter()
            .find(|r| r.peer_id == "peer-reg")
            .expect("registry row present");
        assert_eq!(
            reg.source.as_deref(),
            Some("registry"),
            "legacy registry row must NOT be reassigned to operator by backfill"
        );
        assert_eq!(reg.template.as_deref(), Some("network-control"));

        let blank = rows
            .iter()
            .find(|r| r.peer_id == "peer-blank")
            .expect("blank row present");
        assert_eq!(
            blank.source.as_deref(),
            Some("operator"),
            "a blank-source row defaults to operator"
        );
        assert_eq!(blank.template.as_deref(), Some("conversation"));
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
    const PARTIAL_TERMINAL_DURABILITY_AGENT_REQUEST_SCHEMA: &str = r#"
        type AgentRequest @branchable {
            request_id: String @index
            agent_did: String @index
            session_id: String @index
            content: String
            status: String @index
            lifecycle_state: String @index
            created_at: String @index
            retry_count: Int
            terminalized_at: String
        }
    "#;
    const MUTABLE_REQUESTER_DID_AGENT_REQUEST_SCHEMA: &str = r#"
        type AgentRequest @branchable {
            request_id: String @index
            agent_did: String @index
            requester_did: String @index
            session_id: String @index
            content: String
            status: String @index
            lifecycle_state: String @index
            created_at: String @index
        }
    "#;
    const CORRUPTED_TERMINALIZED_AT_AGENT_REQUEST_SCHEMA: &str = r#"
        type AgentRequest @branchable {
            request_id: String @index
            agent_did: String @index
            lifecycle_state: String @index
            created_at: String @index
            retry_count: Int
            terminalized_at: [String]
            terminal_redrive_attempts: Int
        }
    "#;
    const CORRUPTED_TERMINAL_REDRIVE_AGENT_REQUEST_SCHEMA: &str = r#"
        type AgentRequest @branchable {
            request_id: String @index
            agent_did: String @index
            lifecycle_state: String @index
            created_at: String @index
            retry_count: Int
            terminalized_at: String
            terminal_redrive_attempts: [Int]
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
    // AgentResponse with the scalar-Int `progress_seq` sibling but WITHOUT
    // reasoning_progress_seq, so the migration adds the latter; used to assert
    // the patch adds a scalar Int, not an array (#661).
    const AGENT_RESPONSE_WITH_INT_SIBLING_SCHEMA: &str = r#"
        type AgentResponse @branchable {
            response_key: String @index(unique: true)
            request_id: String @index
            agent_did: String @index
            session_id: String @index
            content: String
            progress_seq: Int
            created_at: String @index
        }
    "#;
    // #661: reasoning_progress_seq mistyped as a list (a wrong manual schema
    // patch), with progress_seq as the correct scalar-Int reference sibling.
    const CORRUPTED_AGENT_RESPONSE_SCHEMA: &str = r#"
        type AgentResponse @branchable {
            response_key: String @index(unique: true)
            request_id: String @index
            agent_did: String @index
            session_id: String @index
            content: String
            progress_seq: Int
            reasoning_progress_seq: [Int]
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
    async fn requester_did_migration_adds_an_immutable_route_key() {
        let node = test_node().await;
        node.add_schema(OLD_AGENT_REQUEST_SCHEMA).await.unwrap();

        ensure_agent_request_requester_did_migration(node.clone())
            .await
            .unwrap();
        ensure_agent_request_requester_did_migration(node.clone())
            .await
            .unwrap();

        let collection = node
            .get_collection("AgentRequest")
            .unwrap()
            .expect("AgentRequest collection");
        assert!(collection_has_field(&collection, "requester_did"));
        assert!(field_is_immutable(&collection, "requester_did"));
        assert_eq!(
            field_kind_value(&collection, "requester_did"),
            field_kind_value(&collection, "agent_did")
        );

        let create = node
            .execute(
                r#"mutation {
                    create_AgentRequest(input: {
                        request_id: "requester-route-key",
                        agent_did: "did:defra-agent:host",
                        requester_did: "did:defra-agent:coordinator",
                        session_id: "requester-route-session",
                        content: "hello",
                        status: "pending",
                        lifecycle_state: "pending",
                        created_at: "2026-07-10T00:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !create.has_errors(),
            "create routed request: {:?}",
            create.errors
        );

        let rewrite = node
            .execute(
                r#"mutation {
                    update_AgentRequest(
                        filter: { request_id: { _eq: "requester-route-key" } },
                        input: { requester_did: "did:defra-agent:other" }
                    ) { _docID }
                }"#,
            )
            .await;
        assert!(
            rewrite.has_errors(),
            "requester_did must remain immutable after creation"
        );
    }

    #[tokio::test]
    async fn subagent_return_migration_adds_immutable_route_keys() {
        let node = test_node().await;
        for schema in [
            OLD_AGENT_RESPONSE_SCHEMA,
            OLD_AGENT_MESSAGE_SCHEMA,
            OLD_AGENT_TOOL_CALL_SCHEMA,
            OLD_AGENT_TOOL_RESULT_SCHEMA,
            OLD_AGENT_SESSION_SCHEMA,
            OLD_AGENT_CONVERSATION_SCHEMA,
            OLD_COMPACTION_ENTRY_SCHEMA,
        ] {
            node.add_schema(schema).await.unwrap();
        }

        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();
        ensure_subagent_return_requester_did_migrations(node.clone())
            .await
            .unwrap();
        ensure_subagent_return_requester_did_migrations(node.clone())
            .await
            .unwrap();

        for collection_name in SUBAGENT_RETURN_ARTIFACT_COLLECTIONS {
            let collection = node
                .get_collection(collection_name)
                .unwrap()
                .unwrap_or_else(|| panic!("{collection_name} collection"));
            assert!(collection_has_field(&collection, "requester_did"));
            assert!(field_is_immutable(&collection, "requester_did"));
            assert_eq!(
                field_kind_value(&collection, "requester_did"),
                field_kind_value(&collection, "agent_did"),
                "{collection_name}.requester_did must remain an all-String route key"
            );
        }
    }

    #[tokio::test]
    async fn requester_did_migration_rejects_a_mutable_existing_field() {
        let node = test_node().await;
        node.add_schema(MUTABLE_REQUESTER_DID_AGENT_REQUEST_SCHEMA)
            .await
            .unwrap();

        let error = ensure_agent_request_requester_did_migration(node)
            .await
            .expect_err("mutable route key must fail closed");
        assert!(
            format!("{error:#}").contains("not immutable"),
            "migration must explain unsafe route-key mutability: {error:#}"
        );
    }

    #[tokio::test]
    async fn terminal_durability_backfill_is_owner_scoped_and_resumable() {
        #[derive(Deserialize)]
        struct MigratedRequestRow {
            request_id: String,
            terminalized_at: Option<String>,
            terminal_redrive_attempts: Option<i64>,
        }

        let node = test_node().await;
        node.add_schema(PARTIAL_TERMINAL_DURABILITY_AGENT_REQUEST_SCHEMA)
            .await
            .unwrap();
        let response = node
            .execute(
                r#"mutation {
                    owner: create_AgentRequest(input: {
                        request_id: "terminal-migration-owner",
                        agent_did: "did:defra-agent:terminal-migration",
                        session_id: "terminal-migration-session",
                        content: "done",
                        status: "completed",
                        lifecycle_state: "completed",
                        created_at: "2026-01-01T00:00:00Z",
                        terminalized_at: "2026-01-02T00:00:00Z"
                    }) { _docID }
                    foreign: create_AgentRequest(input: {
                        request_id: "terminal-migration-foreign",
                        agent_did: "did:defra-agent:foreign",
                        session_id: "terminal-migration-foreign-session",
                        content: "done",
                        status: "completed",
                        lifecycle_state: "completed",
                        created_at: "2026-01-03T00:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "seed row: {:?}", response.errors);

        ensure_agent_request_terminal_durability_migrations(node.clone())
            .await
            .unwrap();
        ensure_agent_request_terminal_durability_migrations(node.clone())
            .await
            .unwrap();
        backfill_agent_request_terminal_durability(
            node.as_ref(),
            "did:defra-agent:terminal-migration",
        )
        .await
        .unwrap();
        backfill_agent_request_terminal_durability(
            node.as_ref(),
            "did:defra-agent:terminal-migration",
        )
        .await
        .unwrap();

        let collection = node
            .get_collection("AgentRequest")
            .unwrap()
            .expect("AgentRequest collection");
        assert!(collection_has_field(&collection, "terminalized_at"));
        assert!(collection_has_field(
            &collection,
            "terminal_redrive_attempts"
        ));
        assert_eq!(
            field_kind_value(&collection, "terminalized_at"),
            field_kind_value(&collection, "created_at")
        );
        assert_eq!(
            field_kind_value(&collection, "terminal_redrive_attempts"),
            field_kind_value(&collection, "retry_count")
        );

        let response = node
            .execute(
                r#"{ AgentRequest {
                    request_id
                    terminalized_at
                    terminal_redrive_attempts
                } }"#,
            )
            .await;
        assert!(!response.has_errors(), "query row: {:?}", response.errors);
        let rows: Vec<MigratedRequestRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        assert_eq!(rows.len(), 2);
        let owner = rows
            .iter()
            .find(|row| row.request_id == "terminal-migration-owner")
            .expect("owner request");
        assert_eq!(
            owner.terminalized_at.as_deref(),
            Some("2026-01-02T00:00:00Z"),
            "partial backfill must preserve an existing terminal timestamp"
        );
        assert_eq!(owner.terminal_redrive_attempts, Some(0));

        let foreign = rows
            .iter()
            .find(|row| row.request_id == "terminal-migration-foreign")
            .expect("foreign request");
        assert_eq!(
            foreign.terminalized_at, None,
            "owner startup must not author a terminal timestamp on a foreign replica"
        );
        assert_eq!(
            foreign.terminal_redrive_attempts, None,
            "owner startup must not refill the redrive cap on a foreign replica"
        );
    }

    #[tokio::test]
    async fn terminal_durability_migration_fails_loud_on_wrong_field_kinds() {
        for (schema, field, expected_type) in [
            (
                CORRUPTED_TERMINALIZED_AT_AGENT_REQUEST_SCHEMA,
                "terminalized_at",
                "scalar String",
            ),
            (
                CORRUPTED_TERMINAL_REDRIVE_AGENT_REQUEST_SCHEMA,
                "terminal_redrive_attempts",
                "scalar Int",
            ),
        ] {
            let node = test_node().await;
            node.add_schema(schema).await.unwrap();

            let error = ensure_agent_request_terminal_durability_migrations(node)
                .await
                .expect_err("migration must reject a wrong-typed durability field");
            let message = format!("{error:#}");
            assert!(
                message.contains(field) && message.contains(expected_type),
                "error must name {field} and require {expected_type}; got: {message}"
            );
            assert!(
                message.contains("663"),
                "error should point at the migration kind guard incident; got: {message}"
            );
        }
    }

    #[tokio::test]
    async fn subagent_extension_migration_adds_immutable_spawn_target_did() {
        let node = test_node().await;
        node.add_schema(OLD_AGENT_TOOL_CALL_SCHEMA).await.unwrap();

        ensure_subagent_extensions_migrations(node.clone())
            .await
            .unwrap();
        ensure_subagent_extensions_migrations(node.clone())
            .await
            .unwrap();

        let cv = node
            .get_collection("AgentToolCall")
            .unwrap()
            .expect("AgentToolCall collection");
        assert!(collection_has_field(&cv, "spawn_target_did"));
        assert!(
            field_is_immutable(&cv, "spawn_target_did"),
            "spawn_target_did must be schema-immutable for filtered replication"
        );

        let create = node
            .execute(
                r#"mutation { create_AgentToolCall(input: {
                    tool_call_key: "spawn-target-mig:tool-1",
                    request_id: "spawn-target-mig",
                    session_id: "spawn-target-session",
                    tool_name: "spawn_agent",
                    tool_call_id: "tool-1",
                    args: "{}",
                    status: "called",
                    lifecycle_state: "running",
                    spawn_target_did: "did:defra-agent:host-a"
                }) { _docID } }"#,
            )
            .await;
        assert!(
            !create.has_errors(),
            "create AgentToolCall with spawn_target_did failed: {:?}",
            create.errors
        );

        let rewrite = node
            .execute(
                r#"mutation {
                    update_AgentToolCall(
                        filter: { tool_call_key: { _eq: "spawn-target-mig:tool-1" } },
                        input: { spawn_target_did: "did:defra-agent:host-b" }
                    ) { _docID }
                }"#,
            )
            .await;
        assert!(
            rewrite.has_errors(),
            "rewriting immutable spawn_target_did must be rejected"
        );
    }

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

    /// Finding #11: legacy session-keyed rows that predate the immutable
    /// `agent_did` scope key must not be silently dropped from DID-scoped
    /// replication. The reconciler resolves each null row's owning DID (children
    /// via `session_id` → AgentSession; sessions via `session_id` → AgentRequest
    /// lineage) and attempts a single first write. On the current defradb pin
    /// that write is rejected (the `@immutable` constraint fires on null→value),
    /// so every legacy row is COUNTED + WARNED rather than silently excluded, and
    /// an owner is never guessed. This test pins that behavior; the owner-
    /// resolution and idempotence paths are fully exercised.
    #[tokio::test]
    async fn conversation_scope_key_migration_counts_legacy_rows_owner_never_guessed() {
        let node = test_node().await;
        // Upgraded DB: session-keyed collections lack agent_did entirely;
        // AgentRequest already carries it (the lineage source for sessions).
        node.add_schema(OLD_AGENT_MESSAGE_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_TOOL_CALL_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_SESSION_SCHEMA).await.unwrap();
        node.add_schema(OLD_COMPACTION_ENTRY_SCHEMA).await.unwrap();
        node.add_schema(OLD_AGENT_REQUEST_SCHEMA).await.unwrap();

        // A scoped session "s-alice": an AgentRequest carries the owning DID, and
        // each child collection references the session. All predate the scope key
        // (no agent_did column exists yet).
        for mutation in [
            r#"mutation { create_AgentRequest(input: {
                request_id: "req-1", session_id: "s-alice",
                agent_did: "did:defra-agent:alice", content: "hi", status: "done",
                lifecycle_state: "terminal", created_at: "2026-06-12T00:00:00Z"
            }) { _docID } }"#,
            r#"mutation { create_AgentSession(input: {
                session_id: "s-alice", agent_name: "alice", behavior_id: "b1", status: "ended"
            }) { _docID } }"#,
            r#"mutation { create_AgentMessage(input: {
                message_key: "m-1", session_id: "s-alice", request_id: "req-1",
                sequence: 0, role: "user", content: "hi"
            }) { _docID } }"#,
            r#"mutation { create_AgentToolCall(input: {
                tool_call_key: "tc-1", session_id: "s-alice", request_id: "req-1",
                tool_name: "bash", tool_call_id: "call-1", status: "done",
                lifecycle_state: "terminal"
            }) { _docID } }"#,
            r#"mutation { create_CompactionEntry(input: {
                compaction_key: "ce-1", session_id: "s-alice", sequence: 0,
                summary: "...", created_at: "2026-06-12T00:00:00Z"
            }) { _docID } }"#,
            // An ORPHAN session with no AgentRequest lineage, plus an orphan
            // message under it: nothing resolves an agent_did for these.
            r#"mutation { create_AgentSession(input: {
                session_id: "s-orphan", agent_name: "ghost", behavior_id: "b1", status: "ended"
            }) { _docID } }"#,
            r#"mutation { create_AgentMessage(input: {
                message_key: "m-orphan", session_id: "s-orphan", request_id: "req-gone",
                sequence: 0, role: "user", content: "?"
            }) { _docID } }"#,
        ] {
            let resp = node.execute(mutation).await;
            assert!(
                !resp.has_errors(),
                "seed mutation failed: {:?}",
                resp.errors
            );
        }

        // Add the immutable scope key (the Group-1 patch the migration runs
        // before backfilling). Legacy rows now read back agent_did: null.
        for (collection, patch) in [
            ("AgentSession", ADD_AGENT_SESSION_AGENT_DID_PATCH),
            ("AgentMessage", ADD_AGENT_MESSAGE_AGENT_DID_PATCH),
            ("AgentToolCall", ADD_AGENT_TOOL_CALL_AGENT_DID_PATCH),
            ("CompactionEntry", ADD_COMPACTION_ENTRY_AGENT_DID_PATCH),
        ] {
            let next = node.patch_collection(collection, patch).await.unwrap();
            node.set_active_collection_version(&next.version_id)
                .await
                .unwrap();
        }

        // First run. The owner map resolves "s-alice" → did:defra-agent:alice
        // from the AgentRequest lineage and offers each null row a first write.
        // On the current defradb pin that write is REJECTED ("immutable field
        // 'agent_did' cannot be changed"), so every legacy row — resolvable or
        // orphan — is counted as unscoped rather than silently dropped, and
        // nothing is ever guessed. (If a future pin permits the first write, the
        // resolvable rows will move into the backfilled column; this assertion
        // pins today's behavior, matching the group-2 limitation test below.)
        let report = backfill_conversation_scope_keys(&node).await;

        // Per collection: s-alice (resolvable but write-rejected) and, for the
        // session-keyed collections that also have an orphan, the orphan too.
        assert_eq!(
            report.unscoped_for("AgentSession"),
            2,
            "s-alice (write rejected) + s-orphan (no lineage) both counted, not dropped"
        );
        assert_eq!(
            report.unscoped_for("AgentMessage"),
            2,
            "m-1 (write rejected) + m-orphan (no scoped session) both counted"
        );
        assert_eq!(report.unscoped_for("AgentToolCall"), 1);
        assert_eq!(report.unscoped_for("CompactionEntry"), 1);
        // Backfill is impossible on this pin: nothing is written.
        for collection in [
            "AgentSession",
            "AgentMessage",
            "AgentToolCall",
            "CompactionEntry",
        ] {
            assert_eq!(
                report.backfilled_for(collection),
                0,
                "{collection}: this defradb pin rejects the first write to a \
                 newly-immutable field, so no row can be backfilled"
            );
        }

        // Every legacy row remains null (no backfill, no guess).
        for (collection, key_field, key) in [
            ("AgentSession", "session_id", "s-alice"),
            ("AgentMessage", "message_key", "m-1"),
            ("AgentToolCall", "tool_call_key", "tc-1"),
            ("CompactionEntry", "compaction_key", "ce-1"),
            ("AgentMessage", "message_key", "m-orphan"),
        ] {
            let q = format!(
                r#"query {{ {collection}(filter: {{ {key_field}: {{ _eq: "{key}" }} }}) {{ agent_did }} }}"#
            );
            let resp = node.execute(&q).await;
            assert!(
                !resp.has_errors(),
                "read {collection} failed: {:?}",
                resp.errors
            );
            let did = resp
                .data
                .as_ref()
                .and_then(|d| d.get(collection))
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|r| r.get("agent_did"))
                .and_then(|v| v.as_str());
            assert_eq!(
                did, None,
                "{collection} {key} must stay null (backfill blocked, owner never guessed)"
            );
        }

        // Idempotent: a second run is stable — same unscoped counts, still zero
        // backfilled, no error.
        let report2 = backfill_conversation_scope_keys(&node).await;
        assert_eq!(report2.unscoped_for("AgentSession"), 2);
        assert_eq!(report2.unscoped_for("AgentMessage"), 2);
        assert_eq!(report2.unscoped_for("AgentToolCall"), 1);
        assert_eq!(report2.unscoped_for("CompactionEntry"), 1);
        for collection in [
            "AgentSession",
            "AgentMessage",
            "AgentToolCall",
            "CompactionEntry",
        ] {
            assert_eq!(report2.backfilled_for(collection), 0);
        }

        // The end-to-end public migration also succeeds and stays idempotent.
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();
    }

    /// A tracing layer that records the level of every event whose target is
    /// this crate's `migration` module, so a test can assert the scope-key
    /// backfill emits NO warn/info on no-op runs (the property that keeps
    /// `--output json` stdout clean now that the CLI runs the full migration set
    /// on every command).
    #[derive(Clone, Default)]
    struct LevelCapture {
        levels: std::sync::Arc<std::sync::Mutex<Vec<tracing::Level>>>,
    }

    impl LevelCapture {
        fn levels(&self) -> Vec<tracing::Level> {
            self.levels.lock().expect("level store should lock").clone()
        }

        fn warns_or_infos(&self) -> Vec<tracing::Level> {
            self.levels()
                .into_iter()
                .filter(|level| *level <= tracing::Level::INFO)
                .collect()
        }
    }

    impl<S> tracing_subscriber::Layer<S> for LevelCapture
    where
        S: tracing::Subscriber,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target().contains("migration") {
                self.levels
                    .lock()
                    .expect("level store should lock")
                    .push(*event.metadata().level());
            }
        }
    }

    /// Absent collections are NOT a backfill failure: a home that never created
    /// the conversation collections (fresh / config-only) has zero legacy rows.
    /// The read must skip silently (DefraDB reports `Cannot query field`), the
    /// report must be empty, and crucially the migration must emit NO warn/info —
    /// otherwise the line corrupts `--output json` stdout on every CLI command.
    #[tokio::test]
    async fn scope_key_backfill_silent_when_collections_absent() {
        use tracing_subscriber::prelude::*;

        let node = test_node().await;
        // Deliberately do NOT register the conversation collections.
        let capture = LevelCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        // `set_default` returns a guard whose subscriber stays active across the
        // awaits below (same thread), unlike the closure-only `with_default`.
        let _guard = tracing::subscriber::set_default(subscriber);

        let report = backfill_conversation_scope_keys(&node).await;

        // No collections -> nothing to scope, nothing to warn about.
        for collection in [
            "AgentSession",
            "AgentMessage",
            "AgentToolCall",
            "CompactionEntry",
        ] {
            assert_eq!(report.backfilled_for(collection), 0);
            assert_eq!(report.unscoped_for(collection), 0);
        }
        report.warn_unscoped();
        assert!(
            capture.warns_or_infos().is_empty(),
            "absent collections must produce no warn/info, got: {:?}",
            capture.warns_or_infos()
        );
    }

    /// Steady state: the conversation collections EXIST and carry the scope key
    /// with no legacy (null) rows. Running the full scope-key migration must be
    /// silent and idempotent — the common case on every CLI invocation.
    #[tokio::test]
    async fn scope_key_migration_silent_on_steady_state_rerun() {
        use tracing_subscriber::prelude::*;

        let node = test_node().await;
        // Fresh SDL: collections are created immutable with agent_did already
        // present, so the migration has nothing to patch and no legacy rows.
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        // Prime once outside capture so any first-run schema work is excluded.
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();

        let capture = LevelCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();
        ensure_conversation_scope_key_migrations(node.clone())
            .await
            .unwrap();
        drop(_guard);

        assert!(
            capture.warns_or_infos().is_empty(),
            "steady-state scope-key migration must be silent, got: {:?}",
            capture.warns_or_infos()
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

    #[tokio::test]
    async fn reciprocal_conversation_intent_migration_creates_collection_with_all_fields() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        // Idempotent: calling twice must not fail.
        ensure_reciprocal_conversation_intent_migrations(node.clone())
            .await
            .unwrap();
        ensure_reciprocal_conversation_intent_migrations(node.clone())
            .await
            .unwrap();

        let collection = node
            .get_collection("ReciprocalConversationIntent")
            .unwrap()
            .expect("ReciprocalConversationIntent collection must exist after migration");

        for field in &["member_did", "template", "created_at", "updated_at"] {
            assert!(
                collection_has_field(&collection, field),
                "ReciprocalConversationIntent must have field '{field}'"
            );
        }
    }

    /// The bug class this fences: a home whose ToolSelection predates weeks of
    /// schema evolution must gain EVERY bundled-schema field at migration time
    /// — the startup toolset load selects all of them, and ten of them
    /// (write_tools, command_network_mode, defra_query_collections, ...) had
    /// no hand-written patch at all. The schema-driven sweep is the fence.
    #[tokio::test]
    async fn bundled_schema_sweep_patches_ancient_tool_selection() {
        let node = test_node().await;
        const ANCIENT_TOOL_SELECTION: &str = r#"
            type ToolSelection {
                selection_id: String @index(unique: true)
                agent_did: String @index
                display_name: String
                enable_file_tools: Boolean
                enable_bash: Boolean
                created_at: DateTime @index(direction: DESC)
                updated_at: DateTime @index(direction: DESC)
            }
        "#;
        node.add_schema(ANCIENT_TOOL_SELECTION).await.unwrap();

        // Idempotent: run twice; the second sweep must be a write-free no-op.
        ensure_bundled_schema_fields(node.clone()).await.unwrap();
        let first_version = node
            .get_collection("ToolSelection")
            .unwrap()
            .expect("ToolSelection collection after first sweep")
            .version_id;
        ensure_bundled_schema_fields(node.clone()).await.unwrap();

        let collection = node
            .get_collection("ToolSelection")
            .unwrap()
            .expect("ToolSelection collection");
        assert_eq!(
            collection.version_id, first_version,
            "the second bundled-schema sweep must not create a schema version"
        );
        for field in &[
            "tool_policy_version",
            "write_tools",
            "command_network_mode",
            "defra_query_collections",
            "enable_defra_query",
            "read_only_command_allowlist",
            "command_execution_policy",
            "command_allowed_argv_prefixes",
            "command_forbidden_argv_prefixes",
            "cli_tool_names",
            "allowed_mcp_service_ids",
            "enable_memory",
            "subagent_allow_cross_deployment",
            "subagent_default_await_mode",
            "enable_session_history_tool",
            "enable_context_budget",
            "enable_self_config",
            "self_config_categories",
            "self_config_no_lockout",
            "self_config_dry_run",
        ] {
            assert!(
                collection_has_field(&collection, field),
                "ToolSelection must have '{field}' after the bundled-schema sweep"
            );
        }
    }

    #[test]
    fn sdl_parser_reads_bundled_schemas() {
        let parsed = parse_sdl_fields(defra_agent_protocol::schemas::TOOL_SELECTION);
        assert_eq!(parsed.len(), 1);
        let (name, fields) = &parsed[0];
        assert_eq!(name, "ToolSelection");
        assert!(fields
            .iter()
            .any(|(f, t)| f == "tool_policy_version" && t == "String"));
        assert!(fields
            .iter()
            .any(|(f, t)| f == "write_tools" && t == "[String]"));

        let parsed_catalog = bundled_schema_sdls()
            .flat_map(parse_sdl_fields)
            .collect::<Vec<_>>();
        let parsed_names = parsed_catalog
            .iter()
            .map(|(collection, _)| collection.clone())
            .collect::<Vec<_>>();
        let expected_names = defra_agent_protocol::schemas::ALL_COLLECTION_NAMES
            .iter()
            .chain(defra_agent_protocol::schemas::RUNTIME_COLLECTION_NAMES.iter())
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            parsed_names, expected_names,
            "the sweep parser must discover every deployment and runtime collection"
        );

        // A collection with zero parsed fields usually means its SDL changed
        // shape (for example to a one-line declaration) and the conservative
        // parser would silently skip the entire migration surface.
        for (collection, fields) in &parsed_catalog {
            assert!(
                !fields.is_empty(),
                "bundled schema parser found no fields for {collection}"
            );
            for (field, sdl_type) in fields {
                assert!(
                    !field.starts_with(KIND_REPAIR_FIELD_PREFIX),
                    "{collection}.{field} collides with the reserved Kind-repair prefix"
                );
                assert!(
                    sdl_type_to_field_kind(sdl_type).is_some(),
                    "{collection}.{field}: unmapped SDL type {sdl_type:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn bundled_schema_sweep_is_noop_on_fresh_schema_catalog() {
        let node = test_node().await;
        for sdl in defra_agent_protocol::schemas::RUNTIME_ALL
            .iter()
            .chain(defra_agent_protocol::schemas::ALL.iter())
        {
            node.add_schema(sdl).await.unwrap();
        }

        let versions = defra_agent_protocol::schemas::RUNTIME_COLLECTION_NAMES
            .iter()
            .chain(defra_agent_protocol::schemas::ALL_COLLECTION_NAMES.iter())
            .map(|collection_name| {
                let collection = node
                    .get_collection(collection_name)
                    .unwrap()
                    .unwrap_or_else(|| panic!("fresh {collection_name} collection must exist"));
                ((*collection_name).to_string(), collection.version_id)
            })
            .collect::<Vec<_>>();

        ensure_bundled_schema_fields(node.clone()).await.unwrap();

        for (collection_name, original_version) in versions {
            let swept = node
                .get_collection(&collection_name)
                .unwrap()
                .unwrap_or_else(|| panic!("swept {collection_name} collection must exist"));
            assert_eq!(
                swept.version_id, original_version,
                "fresh {collection_name} must not gain a schema version during the bundled sweep"
            );
        }
    }

    #[tokio::test]
    async fn bundled_schema_sweep_patches_pre_openai_wire_api_backend() {
        let node = test_node().await;
        let pre_wire_api_sdl = defra_agent_protocol::schemas::INFERENCE_BACKEND
            .lines()
            .filter(|line| !line.contains("openai_wire_api"))
            .collect::<Vec<_>>()
            .join("\n");
        node.add_schema(&pre_wire_api_sdl).await.unwrap();

        ensure_bundled_schema_fields(node.clone()).await.unwrap();
        let patched = node
            .get_collection("InferenceBackend")
            .unwrap()
            .expect("InferenceBackend collection after bundled sweep");
        assert!(
            collection_has_field(&patched, "openai_wire_api"),
            "runtime-only InferenceBackend must gain the #567 field"
        );
        let patched_version = patched.version_id;

        let read = node
            .execute("query { InferenceBackend { _docID openai_wire_api } }")
            .await;
        assert!(
            !read.has_errors(),
            "startup's openai_wire_api selection must compile after migration: {:?}",
            read.errors
        );

        ensure_bundled_schema_fields(node.clone()).await.unwrap();
        assert_eq!(
            node.get_collection("InferenceBackend")
                .unwrap()
                .expect("InferenceBackend collection after second sweep")
                .version_id,
            patched_version,
            "second InferenceBackend sweep must be write-free"
        );
    }

    /// The real upgrade path: a home whose InferenceProfile predates #648's
    /// retry fields must gain all five at migration time — the startup profile
    /// query selects every retry column, so a missing one fails server boot.
    #[tokio::test]
    async fn inference_profile_migration_patches_pre_retry_collections() {
        let node = test_node().await;
        let pre_retry_sdl: String = defra_agent_protocol::schemas::INFERENCE_PROFILE
            .lines()
            .filter(|line| !line.contains("retry_"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            pre_retry_sdl.contains("type InferenceProfile"),
            "filtered SDL must still declare the collection"
        );
        node.add_schema(&pre_retry_sdl).await.unwrap();

        // Idempotent: run twice; second run must be a no-op.
        ensure_inference_profile_migrations(node.clone())
            .await
            .unwrap();
        ensure_inference_profile_migrations(node.clone())
            .await
            .unwrap();

        let collection = node
            .get_collection("InferenceProfile")
            .unwrap()
            .expect("InferenceProfile collection must exist after migration");
        let hand_patched_version = collection.version_id.clone();
        for (field, _) in INFERENCE_PROFILE_ADDITIVE_FIELDS {
            assert!(
                collection_has_field(&collection, field),
                "InferenceProfile must have field '{field}' after migration"
            );
            assert_eq!(
                collection
                    .fields
                    .iter()
                    .filter(|candidate| candidate.name == *field)
                    .count(),
                1,
                "InferenceProfile.{field} must be patched exactly once"
            );
        }

        // #680 coexistence: production still runs the hand-written retry-field
        // migration before the generic sweep. The sweep must recognize those
        // fields and create no second schema version or duplicate field.
        ensure_bundled_schema_fields(node.clone()).await.unwrap();
        let swept = node
            .get_collection("InferenceProfile")
            .unwrap()
            .expect("InferenceProfile collection after bundled sweep");
        assert_eq!(
            swept.version_id, hand_patched_version,
            "bundled sweep must not double-patch #680 retry fields"
        );
        for (field, _) in INFERENCE_PROFILE_ADDITIVE_FIELDS {
            assert_eq!(
                swept
                    .fields
                    .iter()
                    .filter(|candidate| candidate.name == *field)
                    .count(),
                1,
                "InferenceProfile.{field} must remain unique after bundled sweep"
            );
        }
    }

    #[tokio::test]
    async fn pairing_bearer_claim_migration_creates_collection_with_all_fields() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

        // Idempotent: calling twice must not fail.
        ensure_pairing_bearer_claim_migrations(node.clone())
            .await
            .unwrap();
        ensure_pairing_bearer_claim_migrations(node.clone())
            .await
            .unwrap();

        let collection = node
            .get_collection("PairingBearerClaim")
            .unwrap()
            .expect("PairingBearerClaim collection must exist after migration");

        for field in &[
            "token",
            "claimant_did",
            "claimant_node_id",
            "claimant_address",
            "claimed_at",
            "binding_sig",
        ] {
            assert!(
                collection_has_field(&collection, field),
                "PairingBearerClaim must have field '{field}'"
            );
        }
    }

    #[tokio::test]
    async fn consumed_invite_nonce_migration_adds_claimant_did() {
        let node = test_node().await;
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();

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
        assert!(
            collection_has_field(&collection, "claimant_did"),
            "ConsumedInviteNonce must have claimant_did after migration"
        );
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
        assert!(
            node.get_collection("ReciprocalConversationIntent")
                .unwrap()
                .is_some(),
            "ReciprocalConversationIntent must exist after ensure_all_runtime_migrations"
        );
        assert!(
            node.get_collection("PairingBearerClaim").unwrap().is_some(),
            "PairingBearerClaim must exist after ensure_all_runtime_migrations"
        );
    }

    #[tokio::test]
    async fn reasoning_progress_migration_adds_scalar_int_not_array() {
        // #661 root cause: the patch used `Kind:5` (IntArray) where scalar Int
        // is `Kind:4`, so reasoning_progress_seq was created as `[Int]` and every
        // create_AgentResponse (which writes scalar `0`) failed with "Expected
        // array, got: Number(0)". Assert the migration adds a scalar Int and that
        // the incident operation — a scalar create — now succeeds.
        let node = test_node().await;
        node.add_schema(AGENT_RESPONSE_WITH_INT_SIBLING_SCHEMA)
            .await
            .unwrap();

        ensure_agent_response_reasoning_progress_migration(node.clone())
            .await
            .unwrap();

        let collection = node.get_collection("AgentResponse").unwrap().unwrap();
        assert_eq!(
            field_kind_value(&collection, "reasoning_progress_seq"),
            field_kind_value(&collection, "progress_seq"),
            "reasoning_progress_seq must be scalar Int like its sibling progress_seq, not an array"
        );

        let created = node
            .execute(
                r#"mutation {
                    create_AgentResponse(input: {
                        response_key: "scalar-write",
                        request_id: "scalar-write",
                        agent_did: "did:defra-agent:migration-test",
                        session_id: "scalar-write",
                        content: "",
                        reasoning_progress_seq: 0,
                        created_at: "2026-07-08T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !created.has_errors(),
            "a scalar reasoning_progress_seq write must succeed after the migration (#661): {:?}",
            created.errors
        );
    }

    #[tokio::test]
    async fn bundled_schema_sweep_repairs_wrong_reasoning_progress_kind() {
        // #661: a reasoning_progress_seq mistyped as a list (from a bad manual
        // schema patch) makes every create_AgentResponse fail with "Expected
        // array, got: Number(0)". The hand-written migration runs first and
        // defers the mismatch; the schema-driven sweep must then replace the
        // stored `[Int]` field with scalar `Int`, exactly repairing strangenas.
        let node = test_node().await;
        node.add_schema(CORRUPTED_AGENT_RESPONSE_SCHEMA)
            .await
            .unwrap();
        let legacy_row = node
            .execute(
                r#"mutation {
                    create_AgentResponse(input: {
                        response_key: "legacy-before-kind-repair",
                        request_id: "legacy-before-kind-repair",
                        agent_did: "did:defra-agent:migration-test",
                        session_id: "legacy-before-kind-repair",
                        content: "preserve me",
                        progress_seq: 1,
                        created_at: "2026-07-09T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !legacy_row.has_errors(),
            "pre-repair row setup failed: {:?}",
            legacy_row.errors
        );

        ensure_agent_response_reasoning_progress_migration(node.clone())
            .await
            .unwrap();
        ensure_bundled_schema_fields(node.clone()).await.unwrap();

        let repaired = node.get_collection("AgentResponse").unwrap().unwrap();
        assert_eq!(
            field_kind_value(&repaired, "reasoning_progress_seq"),
            field_kind_value(&repaired, "progress_seq"),
            "bundled sweep must replace `[Int]` with scalar `Int`"
        );
        assert!(
            !repaired
                .fields
                .iter()
                .any(|field| field.name.starts_with(KIND_REPAIR_FIELD_PREFIX)),
            "completed repair must not leave its temporary field behind"
        );

        let preserved = node
            .execute(
                r#"query {
                    AgentResponse(filter: {
                        response_key: { _eq: "legacy-before-kind-repair" }
                    }) { response_key content progress_seq reasoning_progress_seq }
                }"#,
            )
            .await;
        assert!(
            !preserved.has_errors(),
            "legacy row must remain queryable after Kind repair: {:?}",
            preserved.errors
        );
        assert_eq!(
            preserved
                .data
                .as_ref()
                .and_then(|data| data.get("AgentResponse"))
                .and_then(|rows| rows.as_array())
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("content"))
                .and_then(|content| content.as_str()),
            Some("preserve me"),
            "Kind repair must preserve pre-existing AgentResponse history"
        );

        let scalar_write = node
            .execute(
                r#"mutation {
                    create_AgentResponse(input: {
                        response_key: "repaired-scalar-write",
                        request_id: "repaired-scalar-write",
                        agent_did: "did:defra-agent:migration-test",
                        session_id: "repaired-scalar-write",
                        content: "",
                        reasoning_progress_seq: 0,
                        created_at: "2026-07-10T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !scalar_write.has_errors(),
            "scalar write must succeed after wrong-Kind repair: {:?}",
            scalar_write.errors
        );

        let repaired_version = repaired.version_id;
        ensure_bundled_schema_fields(node.clone()).await.unwrap();
        assert_eq!(
            node.get_collection("AgentResponse")
                .unwrap()
                .unwrap()
                .version_id,
            repaired_version,
            "second boot after wrong-Kind repair must be a schema no-op"
        );
    }

    #[tokio::test]
    async fn agent_response_reasoning_progress_migration_is_idempotent_and_preserves_rows() {
        let node = test_node().await;
        node.add_schema(OLD_AGENT_RESPONSE_SCHEMA).await.unwrap();

        let pre_migration = node
            .execute(
                r#"mutation {
                    create_AgentResponse(input: {
                        response_key: "pre-migration-response",
                        request_id: "pre-migration-request",
                        agent_did: "did:defra-agent:migration-test",
                        session_id: "pre-migration-session",
                        content: "pre-migration content",
                        created_at: "2026-07-07T12:00:00Z"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !pre_migration.has_errors(),
            "creating pre-migration AgentResponse row failed: {:?}",
            pre_migration.errors
        );

        ensure_agent_response_reasoning_progress_migration(node.clone())
            .await
            .unwrap();
        ensure_agent_response_reasoning_progress_migration(node.clone())
            .await
            .unwrap();

        let collection = node.get_collection("AgentResponse").unwrap().unwrap();
        assert!(
            collection_has_field(&collection, "reasoning_progress_seq"),
            "AgentResponse.reasoning_progress_seq must exist after migration"
        );

        let read = node
            .execute(
                r#"query {
                    AgentResponse {
                        _docID
                        response_key
                        content
                        reasoning_progress_seq
                    }
                }"#,
            )
            .await;
        assert!(
            !read.has_errors(),
            "querying reasoning_progress_seq on AgentResponse must succeed after migration, got: {:?}",
            read.errors
        );

        let rows = read
            .data
            .as_ref()
            .and_then(|data| data.get("AgentResponse"))
            .and_then(|rows| rows.as_array())
            .expect("AgentResponse query should return rows");
        let row = rows
            .iter()
            .find(|row| {
                row.get("response_key").and_then(|value| value.as_str())
                    == Some("pre-migration-response")
            })
            .expect("pre-migration AgentResponse row should survive migration");
        assert_eq!(
            row.get("content").and_then(|value| value.as_str()),
            Some("pre-migration content")
        );
        let progress = row
            .get("reasoning_progress_seq")
            .expect("query should include reasoning_progress_seq");
        assert!(
            progress.is_null() || progress.as_i64() == Some(0),
            "pre-migration row should read with an unset or zero reasoning_progress_seq, got {progress}"
        );
    }
}
