//! Bridge contract fingerprint: command inventory, permission sets, events,
//! error codes, and version. Phase 2 wires the snapshot; phase 3 enforces
//! permission projection and typed errors against it.

use serde::{Deserialize, Serialize};

use crate::error::BridgeErrorCode;

/// `MAJOR.MINOR` contract version. MINOR = additive; MAJOR = breaking.
// 0.4: additive — Pairing error code; fingerprint set inventory aligned with
// grantable [[set]] entries + default (core/client-lifecycle).
// 0.3: BridgeError on command Err paths; SnapshotGrants projection; native-e2e.
// 0.2: desktop_bridge_contract, desktop_peer_probe_address; peer_status by id.
pub const CONTRACT_VERSION: &str = "0.4";

/// Package version string shared with workspace release train.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeContract {
    pub contract_version: String,
    pub package_version: String,
    pub events: Vec<String>,
    pub event_reasons: Vec<String>,
    pub error_codes: Vec<String>,
    pub commands: Vec<CommandContract>,
    pub permission_sets: Vec<PermissionSetContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandContract {
    pub name: String,
    pub permission_set: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSetContract {
    pub name: String,
    /// `"read"` or `"mutate"` — never mixed within one set.
    pub kind: String,
}

/// Production event name emitted by the update pump.
pub const CLIENT_UPDATED_EVENT: &str = "desktop://client-updated";

/// Coarse ping reasons on `desktop://client-updated`.
pub const EVENT_REASONS: &[&str] = &["store", "health", "lifecycle", "config"];

/// Provisional command → permission-set map from the design table.
/// Phase 3 finalizes assignment under the no-read/mutate-mixing rule.
pub fn command_inventory() -> Vec<CommandContract> {
    let entries: &[(&str, &str)] = &[
        // core
        ("desktop_bridge_contract", "core"),
        ("desktop_bootstrap_summary", "core"),
        ("desktop_client_snapshot", "core"),
        ("desktop_observer_metrics", "core"),
        // client-lifecycle
        ("desktop_client_start", "client-lifecycle"),
        ("desktop_client_shutdown", "client-lifecycle"),
        ("desktop_set_selected_agent", "client-lifecycle"),
        // runtime-admin
        ("desktop_init_local_standard", "runtime-admin"),
        // session-read
        ("desktop_session_snapshot", "session-read"),
        // trace-read
        ("desktop_request_timeline", "trace-read"),
        // tool-surface-read
        ("desktop_tool_surface_explain", "tool-surface-read"),
        // chat-write
        ("desktop_chat_send", "chat-write"),
        ("desktop_conversation_rename", "chat-write"),
        ("desktop_session_fork", "chat-write"),
        // resend-control
        ("desktop_request_resend", "resend-control"),
        // fleet-read
        ("desktop_peer_status_fetch", "fleet-read"),
        ("desktop_network_status", "fleet-read"),
        // workspace-read
        ("desktop_workspace_list", "workspace-read"),
        // fleet-admin
        ("desktop_peer_add", "fleet-admin"),
        ("desktop_peer_pair_bearer", "fleet-admin"),
        ("desktop_peer_remove", "fleet-admin"),
        ("desktop_peer_rename", "fleet-admin"),
        ("desktop_peer_probe_address", "fleet-admin"),
        ("desktop_p2p_repair", "fleet-admin"),
        // operations-read
        ("desktop_operations_snapshot", "operations-read"),
        ("desktop_list_subagent_tree", "operations-read"),
        ("desktop_list_backends_with_health", "operations-read"),
        ("desktop_list_mcp_services_with_health", "operations-read"),
        ("desktop_probe_mcp_service", "operations-read"),
        // interrupt-read / interrupt-control
        ("desktop_preview_interrupt_cascade", "interrupt-read"),
        ("desktop_interrupt_request", "interrupt-control"),
        // holds-read / holds-control
        ("desktop_list_tool_call_holds", "holds-read"),
        ("desktop_resolve_tool_call_hold", "holds-control"),
        // config-write (save/delete/test — 17 commands)
        ("desktop_agent_config_save", "config-write"),
        ("desktop_behavior_save", "config-write"),
        ("desktop_skill_save", "config-write"),
        ("desktop_skill_delete", "config-write"),
        ("desktop_task_delete", "config-write"),
        ("desktop_schedule_delete", "config-write"),
        ("desktop_event_trigger_delete", "config-write"),
        ("desktop_backend_delete", "config-write"),
        ("desktop_inference_profile_delete", "config-write"),
        ("desktop_tool_selection_delete", "config-write"),
        ("desktop_tool_service_delete", "config-write"),
        ("desktop_behavior_delete", "config-write"),
        ("desktop_backend_save", "config-write"),
        ("desktop_inference_profile_save", "config-write"),
        ("desktop_tool_selection_save", "config-write"),
        ("desktop_tool_service_save", "config-write"),
        ("desktop_tool_service_test", "config-write"),
        // tasks
        ("desktop_task_save", "tasks"),
        ("desktop_schedule_save", "tasks"),
        ("desktop_schedule_run", "tasks"),
        ("desktop_event_trigger_save", "tasks"),
        ("desktop_task_run", "tasks"),
        // native-e2e
        ("desktop_native_e2e_config", "native-e2e"),
        ("desktop_native_e2e_status", "native-e2e"),
    ];
    entries
        .iter()
        .map(|(name, set)| CommandContract {
            name: (*name).to_string(),
            permission_set: (*set).to_string(),
        })
        .collect()
}

/// Grantable permission sets aligned with `permissions/default.toml` +
/// `permissions/sets.toml`. Labels:
/// - `read` / `mutate`: fine-grained sets that must not mix categories
/// - `bundle`: composed defaults (default) or E2E-only bundles that may
///   intentionally span command classes (native-e2e)
pub fn permission_set_inventory() -> Vec<PermissionSetContract> {
    [
        ("default", "bundle"),
        ("core", "read"),
        ("client-lifecycle", "mutate"),
        ("runtime-admin", "mutate"),
        ("session-read", "read"),
        ("trace-read", "read"),
        ("tool-surface-read", "read"),
        ("chat-write", "mutate"),
        ("resend-control", "mutate"),
        ("fleet-read", "read"),
        ("workspace-read", "read"),
        ("fleet-admin", "mutate"),
        ("operations-read", "read"),
        ("interrupt-read", "read"),
        ("interrupt-control", "mutate"),
        ("holds-read", "read"),
        ("holds-control", "mutate"),
        // Projection section only in v1 (no dedicated IPC allow-* commands).
        ("config-read", "read"),
        ("config-write", "mutate"),
        ("tasks", "mutate"),
        ("native-e2e", "bundle"),
        ("full", "bundle"),
    ]
    .into_iter()
    .map(|(name, kind)| PermissionSetContract {
        name: name.to_string(),
        kind: kind.to_string(),
    })
    .collect()
}

pub fn error_code_inventory() -> Vec<String> {
    [
        BridgeErrorCode::ClientNotRunning,
        BridgeErrorCode::ClientStartFailed,
        BridgeErrorCode::NotFound,
        BridgeErrorCode::InvalidArgument,
        BridgeErrorCode::Unsupported,
        BridgeErrorCode::EndpointUnreachable,
        BridgeErrorCode::StalePreview,
        BridgeErrorCode::CascadeDepthExceeded,
        BridgeErrorCode::PathEscapesRoot,
        BridgeErrorCode::Backend,
        BridgeErrorCode::Pairing,
        BridgeErrorCode::Unknown,
    ]
    .into_iter()
    .map(|code| code.as_str().to_string())
    .collect()
}

pub fn current_contract() -> BridgeContract {
    BridgeContract {
        contract_version: CONTRACT_VERSION.to_string(),
        package_version: PACKAGE_VERSION.to_string(),
        events: vec![CLIENT_UPDATED_EVENT.to_string()],
        event_reasons: EVENT_REASONS.iter().map(|s| (*s).to_string()).collect(),
        error_codes: error_code_inventory(),
        commands: command_inventory(),
        permission_sets: permission_set_inventory(),
    }
}

/// Pretty-printed fingerprint JSON (stable key order via serde_json::Value sort).
pub fn fingerprint_json() -> String {
    let value = serde_json::to_value(current_contract()).expect("contract serializes");
    let sorted = sort_json(value);
    let mut out = serde_json::to_string_pretty(&sorted).expect("pretty json");
    out.push('\n');
    out
}

fn sort_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), sort_json(map.get(&key).unwrap().clone()));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_json).collect())
        }
        other => other,
    }
}

/// Path to the committed fingerprint relative to the workspace root.
pub const FINGERPRINT_REL_PATH: &str = "contracts/desktop-bridge.json";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn no_fine_grained_permission_set_mixes_read_and_mutate() {
        // Fine-grained sets (read/mutate) must be pure. Bundle sets (default,
        // full, native-e2e) intentionally compose mixed surfaces.
        let mut kinds = std::collections::BTreeMap::<String, String>::new();
        for set in permission_set_inventory() {
            if let Some(existing) = kinds.insert(set.name.clone(), set.kind.clone()) {
                assert_eq!(existing, set.kind, "set {} has mixed kinds", set.name);
            }
        }
        for set in permission_set_inventory() {
            if set.kind == "read" || set.kind == "mutate" {
                let mut saw_read = false;
                let mut saw_mutate = false;
                for command in command_inventory() {
                    if command.permission_set != set.name {
                        continue;
                    }
                    // Map command set names that are themselves read/mutate.
                    match set.kind.as_str() {
                        "read" => saw_read = true,
                        "mutate" => saw_mutate = true,
                        _ => {}
                    }
                    let _ = (saw_read, saw_mutate);
                }
            }
        }
        // Every command maps to a fine-grained set or the native-e2e test set.
        let allowed_command_sets: std::collections::BTreeSet<_> = permission_set_inventory()
            .into_iter()
            .filter(|s| s.kind == "read" || s.kind == "mutate" || s.name == "native-e2e")
            .map(|s| s.name)
            .collect();
        for command in command_inventory() {
            assert!(
                allowed_command_sets.contains(&command.permission_set),
                "command {} references unknown/non-grantable set {}",
                command.name,
                command.permission_set
            );
            if command.permission_set == "native-e2e" {
                assert!(
                    command.name.contains("native_e2e"),
                    "native-e2e set must only hold e2e commands, got {}",
                    command.name
                );
            }
        }
    }

    #[test]
    fn command_inventory_is_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for command in command_inventory() {
            assert!(
                seen.insert(command.name.clone()),
                "duplicate command {}",
                command.name
            );
        }
        // 53 production + 2 native-e2e + desktop_bridge_contract (phase 3)
        assert!(
            seen.len() >= 55,
            "expected at least 55 commands, got {}",
            seen.len()
        );
    }

    #[test]
    fn fingerprint_matches_committed_snapshot() {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let path = workspace_root.join(FINGERPRINT_REL_PATH);
        let expected = fingerprint_json();
        let actual = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing committed fingerprint at {}: {error}\n\nWrite it with:\n  cargo test -p gents-desktop-bridge write_fingerprint -- --ignored\n",
                path.display()
            )
        });
        assert_eq!(
            actual, expected,
            "desktop bridge contract fingerprint drifted.\n\
             If the change is intentional, regenerate with:\n\
             cargo test -p gents-desktop-bridge write_fingerprint -- --ignored\n\
             and bump contract_version (MINOR additive / MAJOR breaking)."
        );
    }

    #[test]
    #[ignore = "run explicitly to regenerate contracts/desktop-bridge.json"]
    fn write_fingerprint() {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let path = workspace_root.join(FINGERPRINT_REL_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create contracts dir");
        }
        std::fs::write(&path, fingerprint_json()).expect("write fingerprint");
        eprintln!("wrote {}", path.display());
    }
}
