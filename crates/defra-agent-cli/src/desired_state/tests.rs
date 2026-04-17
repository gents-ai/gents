use std::fs;

use serde_json::json;

use super::convert::tool_service_registry_from_live_value;
use super::diff::diff_collection;
use super::*;

#[test]
fn desired_tool_service_registry_normalizes_address_storage_fields() {
    let service: DesiredToolServiceRegistry = serde_json::from_value(json!({
        "service_id": "observability-mcp",
        "display_name": "Observability",
        "description": null,
        "hostname": null,
        "tailscale_ip": " 100.64.0.10 ",
        "lan_ip": null,
        "mcp_port": 9201,
        "mcp_path": "mcp"
    }))
    .expect("desired tool service should deserialize");

    assert_eq!(service.hostname.as_deref(), Some(""));
    assert_eq!(service.tailscale_ip.as_deref(), Some("100.64.0.10"));
    assert_eq!(service.lan_ip.as_deref(), Some(""));
    assert_eq!(service.mcp_path.as_deref(), Some("/mcp"));
}

#[test]
fn live_tool_service_registry_preserves_null_storage_for_diff() {
    let service = tool_service_registry_from_live_value(&json!({
        "service_id": "observability-mcp",
        "hostname": null,
        "tailscale_ip": null,
        "lan_ip": null,
        "mcp_port": 9201,
        "mcp_path": null
    }))
    .expect("live tool service should parse");

    assert_eq!(service.hostname, None);
    assert_eq!(service.tailscale_ip, None);
    assert_eq!(service.lan_ip, None);
    assert_eq!(service.mcp_path, None);
}

#[test]
fn diff_marks_live_null_tool_service_storage_for_update() {
    let desired: DesiredToolServiceRegistry = serde_json::from_value(json!({
        "service_id": "observability-mcp",
        "hostname": "studio-1",
        "mcp_port": 9201
    }))
    .expect("desired tool service should deserialize");
    let live = tool_service_registry_from_live_value(&json!({
        "service_id": "observability-mcp",
        "hostname": "studio-1",
        "tailscale_ip": null,
        "lan_ip": null,
        "mcp_port": 9201,
        "mcp_path": null
    }))
    .expect("live tool service should parse");

    let diff = diff_collection(
        vec![(desired.service_id.clone(), &desired)],
        vec![(live.service_id.clone(), &live)],
    );

    assert_eq!(diff.update, vec!["observability-mcp"]);
    assert!(diff.unchanged.is_empty());
}

#[test]
fn deprecated_backend_capability_fields_are_ignored_for_diff_equality() {
    let with_deprecated: DesiredInferenceBackend = serde_json::from_value(json!({
        "backend_id": "local",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:11434/v1",
        "api_key": null,
        "api_key_env_var": null,
        "max_concurrent": 1,
        "max_queue_depth": 100,
        "enabled": true,
        "supports_tool_calls": false,
        "supports_streaming": false,
        "supports_structured_outputs": true,
        "supports_json_schema": true,
        "context_window": 32768,
        "max_output_tokens": 4096,
        "models": ["test-model"]
    }))
    .expect("deprecated fields should deserialize");

    let current: DesiredInferenceBackend = serde_json::from_value(json!({
        "backend_id": "local",
        "name": "Local",
        "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:11434/v1",
        "api_key": null,
        "api_key_env_var": null,
        "max_concurrent": 1,
        "max_queue_depth": 100,
        "enabled": true,
        "models": ["test-model"]
    }))
    .expect("current fields should deserialize");

    assert_eq!(with_deprecated, current);
    assert_eq!(
        serde_json::to_value(with_deprecated).unwrap(),
        serde_json::to_value(current).unwrap()
    );
}

#[test]
fn validate_manifest_accepts_deprecated_backend_capability_fields() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path();

    fs::write(
        root.join("agent-principal.json"),
        r#"{
            "agent_did": "did:defra-agent:test",
            "display_name": "Test",
            "default_behavior_id": "did:defra-agent:test:default",
            "enabled": true
        }"#,
    )
    .unwrap();
    fs::write(
        root.join("agent-behaviors.json"),
        r#"[{
            "behavior_id": "did:defra-agent:test:default",
            "agent_did": "did:defra-agent:test",
            "display_name": "Default",
            "system_prompt": null,
            "backend_id": "local",
            "model_name": "test-model",
            "tool_selection_id": "tools",
            "inference_profile_id": null,
            "compaction_strategy": null,
            "compaction_threshold": null,
            "enabled": true
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("tool-selections.json"),
        r#"[{
            "selection_id": "tools",
            "agent_did": "did:defra-agent:test",
            "display_name": "Tools",
            "enable_file_tools": false,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": false,
            "bash_mode": "Off",
            "cli_tool_names": [],
            "enable_meta_tools": true,
            "delegate_to": []
        }]"#,
    )
    .unwrap();
    fs::write(
        root.join("inference-backends.json"),
        r#"[{
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "supports_tool_calls": true,
            "supports_streaming": true,
            "supports_structured_outputs": false,
            "supports_json_schema": false,
            "models": ["test-model"]
        }]"#,
    )
    .unwrap();

    let report = validate_manifest_root(root);
    assert!(
        report.ok,
        "expected valid manifest, got {:?}",
        report.errors
    );
}
