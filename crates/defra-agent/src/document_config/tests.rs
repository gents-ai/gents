use super::*;

#[test]
fn tool_selection_document_accepts_empty_string_arrays() {
    let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
        "selection_id": "did:defra-agent:test:default:tools",
        "agent_did": "did:defra-agent:test",
        "display_name": "Tools",
        "enable_file_tools": true,
        "file_tools_mode": "ReadOnly",
        "file_tool_root": null,
        "enable_bash": false,
        "bash_mode": "disabled",
        "cli_tool_names": "",
        "enable_meta_tools": false,
        "delegate_to": ""
    }))
    .expect("empty string arrays should deserialize");

    assert_eq!(document.cli_tool_names, Some(Vec::new()));
    assert_eq!(document.delegate_to, Some(Vec::new()));
}

#[test]
fn tool_selection_document_accepts_string_array_values() {
    let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
        "selection_id": "did:defra-agent:test:default:tools",
        "agent_did": "did:defra-agent:test",
        "display_name": "Tools",
        "enable_file_tools": true,
        "file_tools_mode": "ReadOnly",
        "file_tool_root": null,
        "enable_bash": false,
        "bash_mode": "disabled",
        "cli_tool_names": ["rg"],
        "enable_meta_tools": false,
        "delegate_to": ["did:defra-agent:other"]
    }))
    .expect("string arrays should deserialize");

    assert_eq!(document.cli_tool_names, Some(vec!["rg".to_string()]));
    assert_eq!(
        document.delegate_to,
        Some(vec!["did:defra-agent:other".to_string()])
    );
}
