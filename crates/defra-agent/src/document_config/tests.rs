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
        "allowed_mcp_service_ids": ""
    }))
    .expect("empty string arrays should deserialize");

    assert_eq!(document.cli_tool_names, Some(Vec::new()));
    assert_eq!(document.allowed_mcp_service_ids, Some(Vec::new()));
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
        "allowed_mcp_service_ids": ["x-data"]
    }))
    .expect("string arrays should deserialize");

    assert_eq!(document.cli_tool_names, Some(vec!["rg".to_string()]));
    assert_eq!(
        document.allowed_mcp_service_ids,
        Some(vec!["x-data".to_string()])
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
fn validate_rejects_write_tool_with_empty_tool_name() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "   ".to_string(),
            collection: "ActionRequest".to_string(),
            description: String::new(),
            fields: Vec::new(),
        }]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err(), "empty tool_name must be rejected");
    assert!(
        format!("{}", result.unwrap_err()).contains("write_tools"),
        "error message must mention write_tools"
    );
}

#[test]
fn validate_rejects_write_tool_with_empty_collection() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: "  ".to_string(),
            description: String::new(),
            fields: Vec::new(),
        }]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err(), "empty collection must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("write_tools") && err.contains("request_action"),
        "error must name write_tools and the offending tool: {err}"
    );
}

#[test]
fn validate_rejects_write_tool_field_with_empty_name() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: "ActionRequest".to_string(),
            description: String::new(),
            fields: vec![WriteToolField {
                name: "  ".to_string(),
                required: true,
            }],
        }]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err(), "empty field name must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("write_tools") && err.contains("request_action"),
        "error must name write_tools and the offending tool: {err}"
    );
}

#[test]
fn validate_rejects_duplicate_write_tool_names() {
    let decl = |collection: &str| WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: collection.to_string(),
        description: String::new(),
        fields: Vec::new(),
    };
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![decl("ActionRequest"), decl("OtherCollection")]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err(), "duplicate tool_name must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("write_tools") && err.contains("request_action"),
        "error must name write_tools and the duplicated tool: {err}"
    );
}

#[test]
fn validate_accepts_well_formed_write_tools() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![
            WriteToolDecl {
                tool_name: "request_action".to_string(),
                collection: "ActionRequest".to_string(),
                description: "Request an action".to_string(),
                fields: vec![WriteToolField {
                    name: "title".to_string(),
                    required: true,
                }],
            },
            WriteToolDecl {
                tool_name: "log_note".to_string(),
                collection: "Note".to_string(),
                description: String::new(),
                fields: Vec::new(),
            },
        ]),
        ..Default::default()
    };
    assert!(
        doc.validate().is_ok(),
        "well-formed, uniquely-named write_tools must validate"
    );
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_builtin() {
    // `read_file` is a native tool; reusing it as a write-tool name would
    // silently shadow the native impl at registration.
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "read_file".to_string(),
            collection: "AuditLog".to_string(),
            description: String::new(),
            fields: vec![WriteToolField {
                name: "path".to_string(),
                required: true,
            }],
        }]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(
        result.is_err(),
        "collision with a native tool must be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("read_file") && err.contains("built-in"),
        "error must name the colliding tool and the reason: {err}"
    );
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_defra_query() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "defra_query".to_string(),
            collection: "AuditLog".to_string(),
            description: String::new(),
            fields: Vec::new(),
        }]),
        ..Default::default()
    };
    assert!(
        doc.validate().is_err(),
        "collision with the built-in defra_query tool must be rejected"
    );
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_cli_tool() {
    // A cli_tool_names entry is advertised as its own tool in the same
    // selection, so a write tool reusing that name is a dispatch collision.
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        cli_tool_names: Some(vec!["rg".to_string()]),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "rg".to_string(),
            collection: "AuditLog".to_string(),
            description: String::new(),
            fields: Vec::new(),
        }]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(
        result.is_err(),
        "collision with a cli_tool_names entry must be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("rg") && err.contains("cli_tool_names"),
        "error must name the colliding tool and the category: {err}"
    );
}

#[test]
fn validate_rejects_duplicate_field_names_within_decl() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: "ActionRequest".to_string(),
            description: String::new(),
            fields: vec![
                WriteToolField {
                    name: "summary".to_string(),
                    required: true,
                },
                WriteToolField {
                    name: "summary".to_string(),
                    required: false,
                },
            ],
        }]),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err(), "duplicate field names must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("request_action") && err.contains("summary"),
        "error must name the tool and the duplicated field: {err}"
    );
}

#[test]
fn reserved_names_cover_native_and_meta_tools() {
    use crate::toolset::NativeTool;

    // Every native tool name must be reserved (guards the hardcoded native
    // literal list in `is_reserved_builtin_tool_name` against drift).
    let native = [
        NativeTool::ListFiles { max_entries: 1 },
        NativeTool::ReadFile { max_chars: 1 },
        NativeTool::Glob { max_matches: 1 },
        NativeTool::Grep { max_matches: 1 },
        NativeTool::WriteFile {
            root: std::path::PathBuf::from("/tmp"),
        },
        NativeTool::EditFile {
            root: std::path::PathBuf::from("/tmp"),
        },
    ];
    for tool in &native {
        assert!(
            is_reserved_builtin_tool_name(&tool.tool_name()),
            "native tool {:?} must be reserved",
            tool.tool_name()
        );
    }
    assert!(is_reserved_builtin_tool_name("bash"));
    assert!(is_reserved_builtin_tool_name("bash_unrestricted"));

    for meta in crate::meta_tools::META_TOOL_NAMES {
        assert!(
            is_reserved_builtin_tool_name(meta),
            "meta tool {meta:?} must be reserved"
        );
    }

    // A name that is not a built-in stays available for write tools.
    assert!(!is_reserved_builtin_tool_name("request_action"));
}

#[cfg(feature = "agent-memory")]
#[test]
fn memory_tool_name_is_reserved() {
    assert!(is_reserved_builtin_tool_name(
        crate::toolset::MEMORY_TOOL_NAME
    ));
}

#[test]
fn write_tools_deserialize_trims_whitespace() {
    // Padded tool_name / collection / field name would otherwise survive to
    // verbatim GraphQL interpolation and corrupt the mutation.
    let json = serde_json::json!({
        "selection_id": "sel-1",
        "agent_did": "did:defra-agent:test",
        "write_tools": [{
            "tool_name": "  request_action  ",
            "collection": " ActionRequest ",
            "fields": [{ "name": "  summary  ", "required": true }]
        }]
    });
    let loaded: ToolSelectionDocument = serde_json::from_value(json).unwrap();
    let decl = &loaded.write_tools.as_ref().unwrap()[0];
    assert_eq!(decl.tool_name, "request_action");
    assert_eq!(decl.collection, "ActionRequest");
    assert_eq!(decl.fields[0].name, "summary");
}

#[tokio::test]
async fn tool_selection_document_round_trips_defra_query_fields() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = ToolSelectionDocument {
        selection_id: "amy-general-tools".to_string(),
        agent_did: "did:key:z-test".to_string(),
        enable_session_history_tool: Some(true),
        enable_defra_query: Some(false),
        defra_query_collections: Some(vec![
            "AgentRequest".to_string(),
            "AgentResponse".to_string(),
        ]),
        ..Default::default()
    };
    upsert_tool_selection(&node, &doc)
        .await
        .expect("upsert should persist the defra_query fields");

    let loaded = load_tool_selection(&node, "amy-general-tools")
        .await
        .expect("load should succeed")
        .expect("selection should exist");
    assert_eq!(loaded.enable_session_history_tool, Some(true));
    assert_eq!(loaded.enable_defra_query, Some(false));
    assert_eq!(
        loaded.defra_query_collections,
        Some(vec![
            "AgentRequest".to_string(),
            "AgentResponse".to_string()
        ])
    );
}

#[test]
fn write_tools_round_trip() {
    let json = serde_json::json!({
        "selection_id": "sel-1",
        "agent_did": "did:defra-agent:test",
        "write_tools": [{
            "tool_name": "request_action",
            "collection": "ActionRequest",
            "description": "Emit one ActionRequest describing a remediable drift.",
            "fields": [
                { "name": "drift_sig", "required": true },
                { "name": "summary", "required": true },
                { "name": "target_paths", "required": false }
            ]
        }]
    });
    let loaded: ToolSelectionDocument = serde_json::from_value(json).unwrap();
    let decls = loaded.write_tools.clone().unwrap();
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0].tool_name, "request_action");
    assert_eq!(decls[0].collection, "ActionRequest");
    assert_eq!(decls[0].fields.len(), 3);
    assert!(decls[0].fields[0].required);
    assert!(!decls[0].fields[2].required);
}

#[tokio::test]
async fn tool_selection_document_round_trips_write_tools() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = ToolSelectionDocument {
        selection_id: "steward-write-tools".to_string(),
        agent_did: "did:key:z-test-write".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: "ActionRequest".to_string(),
            description: "Emit one ActionRequest describing a remediable drift.".to_string(),
            fields: vec![
                WriteToolField {
                    name: "drift_sig".to_string(),
                    required: true,
                },
                WriteToolField {
                    name: "summary".to_string(),
                    required: true,
                },
                WriteToolField {
                    name: "target_paths".to_string(),
                    required: false,
                },
            ],
        }]),
        ..Default::default()
    };
    upsert_tool_selection(&node, &doc)
        .await
        .expect("upsert should persist the write_tools field");

    let loaded = load_tool_selection(&node, "steward-write-tools")
        .await
        .expect("load should succeed")
        .expect("selection should exist");
    assert_eq!(
        loaded.write_tools, doc.write_tools,
        "write_tools must round-trip through the GraphQL document representation"
    );
}

#[tokio::test]
async fn agent_behavior_description_and_summary_round_trip() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = AgentBehavior {
        behavior_id: "amy-general".to_string(),
        agent_did: "did:key:z-test-desc".to_string(),
        display_name: Some("Amy General".to_string()),
        description: Some("A general-purpose assistant for research and writing.".to_string()),
        summary: Some("General assistant".to_string()),
        system_prompt: Some("You are a helpful assistant.".to_string()),
        backend_id: None,
        model_name: None,
        tool_selection_id: None,
        inference_profile_id: None,
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: None,
    };
    upsert_agent_behavior(&node, &doc)
        .await
        .expect("upsert should persist description and summary fields");

    let loaded = load_agent_behavior(&node, "amy-general")
        .await
        .expect("load should succeed")
        .expect("behavior should exist after upsert");
    assert_eq!(
        loaded.description,
        Some("A general-purpose assistant for research and writing.".to_string()),
        "description must round-trip through upsert/load"
    );
    assert_eq!(
        loaded.summary,
        Some("General assistant".to_string()),
        "summary must round-trip through upsert/load"
    );
}

#[test]
fn validate_accepts_well_formed_subagent_targets() {
    // Bare behavior-id strings like "amy-code" are NOT valid SubagentTarget
    // entries — the runtime silently drops them. Proper entries are JSON
    // objects built with subagent_target_entry().
    let code_entry = subagent_target_entry(
        "amy-code",
        "did:key:zParent",
        "did:key:zParent:amy-code",
        Some("Code assistant".to_string()),
    );
    let research_entry = subagent_target_entry(
        "amy-research",
        "did:key:zParent",
        "did:key:zParent:amy-research",
        None,
    );
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        subagent_targets: Some(vec![code_entry, research_entry]),
        subagent_spawn_enabled: Some(true),
        subagent_steering_enabled: Some(false),
        subagent_background_enabled: Some(true),
        ..Default::default()
    };
    assert!(
        doc.validate().is_ok(),
        "well-formed JSON SubagentTarget entries must be accepted"
    );
}

#[test]
fn validate_rejects_bare_string_subagent_target() {
    // A bare behavior-id string is NOT a valid SubagentTarget JSON entry.
    // The runtime silently drops non-JSON entries, so validate() must catch
    // this misconfiguration early with a clear error.
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        subagent_targets: Some(vec!["amy-code".to_string()]),
        subagent_spawn_enabled: Some(true),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(
        result.is_err(),
        "bare behavior-id string must be rejected by validate()"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("subagent_targets"),
        "error must mention subagent_targets; got: {err_msg}"
    );
    assert!(
        err_msg.contains("SubagentTarget JSON"),
        "error must mention SubagentTarget JSON; got: {err_msg}"
    );
}
