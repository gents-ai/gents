use super::*;

#[test]
fn tool_selection_document_accepts_empty_string_arrays() {
    let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
        "selection_id": "default-tools",
        "agent_did": "did:defra-agent:test",
        "display_name": "Tools",
        "enable_file_tools": true,
        "file_tools_mode": "ReadOnly",
        "file_tool_root": null,
        "enable_bash": false,
        "bash_mode": "disabled",
        "cli_tool_names": "",
        "enable_meta_tools": false,
        "allowed_mcp_service_ids": "",
        "delegate_to": ""
    }))
    .expect("empty string arrays should deserialize");

    assert_eq!(document.cli_tool_names, Some(Vec::new()));
    assert_eq!(document.allowed_mcp_service_ids, Some(Vec::new()));
    assert_eq!(document.delegate_to, Some(Vec::new()));
}

#[test]
fn tool_selection_document_accepts_string_array_values() {
    let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
        "selection_id": "default-tools",
        "agent_did": "did:defra-agent:test",
        "display_name": "Tools",
        "enable_file_tools": true,
        "file_tools_mode": "ReadOnly",
        "file_tool_root": null,
        "enable_bash": false,
        "bash_mode": "disabled",
        "cli_tool_names": ["rg"],
        "enable_meta_tools": false,
        "allowed_mcp_service_ids": ["x-data"],
        "delegate_to": ["did:defra-agent:other"]
    }))
    .expect("string arrays should deserialize");

    assert_eq!(document.cli_tool_names, Some(vec!["rg".to_string()]));
    assert_eq!(
        document.allowed_mcp_service_ids,
        Some(vec!["x-data".to_string()])
    );
    assert_eq!(
        document.delegate_to,
        Some(vec!["did:defra-agent:other".to_string()])
    );
}

#[test]
fn validate_rejects_empty_string_in_subagent_targets() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        subagent_targets: Some(vec!["".to_string()]),
        subagent_spawn_enabled: Some(true),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(
        format!("{}", result.unwrap_err()).contains("subagent_targets"),
        "error message must mention subagent_targets"
    );
}

#[test]
fn validate_rejects_empty_string_in_backgroundable_tool_names() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        backgroundable_tool_names: Some(vec!["".to_string()]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(
        format!("{}", result.unwrap_err()).contains("backgroundable_tool_names"),
        "error message must mention backgroundable_tool_names"
    );
}

#[test]
fn validate_accepts_well_formed_subagent_targets() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        subagent_targets: Some(vec!["amy-code".to_string(), "amy-research".to_string()]),
        subagent_spawn_enabled: Some(true),
        subagent_steering_enabled: Some(false),
        subagent_background_enabled: Some(true),
        ..Default::default()
    };
    assert!(doc.validate().is_ok());
}
