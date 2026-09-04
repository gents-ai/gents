use super::*;

#[test]
fn wide_open_preset_is_permissive_and_versioned() {
    let did = "did:test:amy";
    let id = wide_open_tool_selection_id_for_agent(did);
    assert_eq!(id, "did:test:amy:wide-open");

    let preset = wide_open_tool_selection_document(did);
    // Canonical, per-principal id (passes the agent_did hydration filter).
    assert_eq!(preset.selection_id, id);
    assert_eq!(preset.agent_did, did);
    // Stamped at the current version so its explicit permissive values are NOT
    // re-decoded under secure-minimal defaults.
    assert_eq!(
        preset.tool_policy_version.as_deref(),
        Some(crate::tool_surface::TOOL_POLICY_V1)
    );
    // Pin the explicit permissive surface instead of recomputing expectations
    // from the preset builder. Meta and DefraDB query are on; every other
    // privilege-bearing capability is explicitly false.
    assert_eq!(preset.enable_meta_tools, Some(true));
    assert_eq!(preset.enable_goal_tools, None);
    assert_eq!(preset.enable_goal_creation, None);
    assert_eq!(preset.enable_defra_query, Some(true));
    // Context budget is the third capability explicitly enabled by this preset.
    assert_eq!(preset.enable_context_budget, Some(true));
    assert_eq!(preset.enable_file_tools, Some(false));
    assert_eq!(preset.enable_bash, Some(false));
    assert_eq!(preset.subagent_spawn_enabled, Some(false));
    assert_eq!(preset.subagent_steering_enabled, Some(false));
    assert_eq!(preset.subagent_background_enabled, Some(false));
    assert_eq!(preset.subagent_allow_cross_deployment, Some(false));
    assert_eq!(preset.enable_memory, Some(false));
    assert_eq!(preset.enable_session_history_tool, Some(false));
}

#[test]
fn tool_selection_document_accepts_empty_string_arrays() {
    let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
        "selection_id": "default-tools",
        "agent_did": "did:test:test",
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
    assert_eq!(document.enable_goal_tools, None);
    assert_eq!(document.enable_goal_creation, None);
}

#[test]
fn tool_selection_document_accepts_string_array_values() {
    let document: ToolSelectionDocument = serde_json::from_value(serde_json::json!({
        "selection_id": "default-tools",
        "agent_did": "did:test:test",
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
fn required_mcp_services_are_explicit_callable_dependencies() {
    let mut selection = ToolSelectionDocument {
        selection_id: "required-mcp".to_string(),
        agent_did: "did:key:test".to_string(),
        enable_meta_tools: Some(false),
        allowed_mcp_service_ids: Some(vec!["research".to_string()]),
        required_mcp_service_ids: Some(vec!["research".to_string()]),
        ..Default::default()
    };
    assert!(selection
        .validate()
        .unwrap_err()
        .to_string()
        .contains("enable_meta_tools=true"));
    selection.enable_meta_tools = Some(true);
    selection.allowed_mcp_service_ids = Some(vec!["other".to_string()]);
    assert!(selection
        .validate()
        .unwrap_err()
        .to_string()
        .contains("not permitted"));
    selection.allowed_mcp_service_ids = Some(vec!["research".to_string()]);
    selection.validate().unwrap();
}

#[test]
fn validate_rejects_empty_string_in_subagent_targets() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
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
        agent_did: "did:test:test".to_string(),
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
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "   ".to_string(),
            collection: "ActionRequest".to_string(),
            description: String::new(),
            fields: Vec::new(),
            output_obligation: None,
        }]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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
fn validate_rejects_invalid_write_tool_collection_identifiers() {
    for collection in ["  ", "ActionRequest) { _docID } mutation {"] {
        let doc = ToolSelectionDocument {
            selection_id: "test-tools".to_string(),
            agent_did: "did:test:test".to_string(),
            write_tools: Some(vec![WriteToolDecl {
                tool_name: "request_action".to_string(),
                collection: collection.to_string(),
                description: String::new(),
                fields: Vec::new(),
                output_obligation: None,
            }]),
            datastore_tool_surface_ids: None,
            eth_tool_ids: None,
            ..Default::default()
        };
        let err = doc
            .validate()
            .expect_err("invalid collection identifier must be rejected")
            .to_string();
        assert!(
            err.contains("write_tools")
                && err.contains("request_action")
                && err.contains("invalid collection"),
            "error must identify the declaration and invalid collection: {err}"
        );
    }
}

#[test]
fn validate_rejects_invalid_write_tool_field_identifiers() {
    for field_name in ["  ", "title: \"escaped\""] {
        let doc = ToolSelectionDocument {
            selection_id: "test-tools".to_string(),
            agent_did: "did:test:test".to_string(),
            write_tools: Some(vec![WriteToolDecl {
                tool_name: "request_action".to_string(),
                collection: "ActionRequest".to_string(),
                description: String::new(),
                fields: vec![WriteToolField {
                    name: field_name.to_string(),
                    required: true,
                    fill: None,
                }],
                output_obligation: None,
            }]),
            datastore_tool_surface_ids: None,
            eth_tool_ids: None,
            ..Default::default()
        };
        let err = doc
            .validate()
            .expect_err("invalid field identifier must be rejected")
            .to_string();
        assert!(
            err.contains("write_tools")
                && err.contains("request_action")
                && err.contains("invalid field[0] name"),
            "error must identify the declaration and invalid field: {err}"
        );
    }
}

#[test]
fn validate_rejects_model_provided_requester_identity() {
    let mut decl = WriteToolDecl {
        tool_name: "write_gate".to_string(),
        collection: "GraphGate".to_string(),
        description: String::new(),
        fields: vec![WriteToolField {
            name: "requester_did".to_string(),
            required: true,
            fill: None,
        }],
        output_obligation: None,
    };
    assert!(decl
        .validate()
        .expect_err("models must not choose requester identity")
        .to_string()
        .contains("must be runtime-filled"));

    decl.fields[0].required = false;
    decl.fields[0].fill = Some(WriteToolFieldFill::SourceField("requester_did".to_string()));
    assert!(decl.validate().is_ok());
}

#[test]
fn validate_rejects_duplicate_write_tool_names() {
    let decl = |collection: &str| WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: collection.to_string(),
        description: String::new(),
        fields: Vec::new(),
        output_obligation: None,
    };
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![decl("ActionRequest"), decl("OtherCollection")]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![
            WriteToolDecl {
                tool_name: "request_action".to_string(),
                collection: "ActionRequest".to_string(),
                description: "Request an action".to_string(),
                fields: vec![WriteToolField {
                    name: "title".to_string(),
                    required: true,
                    fill: None,
                }],
                output_obligation: None,
            },
            WriteToolDecl {
                tool_name: "log_note".to_string(),
                collection: "Note".to_string(),
                description: String::new(),
                fields: Vec::new(),
                output_obligation: None,
            },
        ]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
        ..Default::default()
    };
    assert!(
        doc.validate().is_ok(),
        "well-formed, uniquely-named write_tools must validate"
    );
}

#[test]
fn write_tool_output_obligation_round_trips_and_rejects_zero_minimum() {
    let value = serde_json::json!({
        "tool_name": "write_result",
        "collection": "Result",
        "description": "Persist the request output.",
        "fields": [{"name": "expected_total", "required": true}],
        "output_obligation": {
            "scope": "trigger",
            "minimum_writes": 1,
            "expected_count_field": "expected_total"
        }
    });
    let decl: WriteToolDecl = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decl).unwrap(), value);

    let doc = ToolSelectionDocument {
        selection_id: "obligated-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            output_obligation: Some(WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Trigger,
                minimum_writes: 0,
                expected_count_field: None,
            }),
            ..decl
        }]),
        ..Default::default()
    };
    assert!(doc
        .validate()
        .unwrap_err()
        .to_string()
        .contains("minimum_writes"));
}

#[test]
fn dynamic_output_obligation_requires_a_model_provided_required_field() {
    for fields in [
        vec![WriteToolField {
            name: "expected_total".to_string(),
            required: false,
            fill: None,
        }],
        vec![WriteToolField {
            name: "expected_total".to_string(),
            required: false,
            fill: Some(WriteToolFieldFill::SourceField(
                "expected_total".to_string(),
            )),
        }],
    ] {
        let doc = ToolSelectionDocument {
            selection_id: "dynamic-obligation-tools".to_string(),
            agent_did: "did:test:test".to_string(),
            write_tools: Some(vec![WriteToolDecl {
                tool_name: "write_result".to_string(),
                collection: "Result".to_string(),
                description: String::new(),
                fields,
                output_obligation: Some(WriteToolOutputObligation {
                    scope: WriteToolOutputObligationScope::Trigger,
                    minimum_writes: 1,
                    expected_count_field: Some("expected_total".to_string()),
                }),
            }]),
            ..Default::default()
        };
        assert!(doc
            .validate()
            .unwrap_err()
            .to_string()
            .contains("expected_count_field"));
    }
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_builtin() {
    // `read_file` is a native tool; reusing it as a write-tool name would
    // silently shadow the native impl at registration.
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "read_file".to_string(),
            collection: "AuditLog".to_string(),
            description: String::new(),
            fields: vec![WriteToolField {
                name: "path".to_string(),
                required: true,
                fill: None,
            }],
            output_obligation: None,
        }]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "defra_query".to_string(),
            collection: "AuditLog".to_string(),
            description: String::new(),
            fields: Vec::new(),
            output_obligation: None,
        }]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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
        agent_did: "did:test:test".to_string(),
        cli_tool_names: Some(vec!["rg".to_string()]),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "rg".to_string(),
            collection: "AuditLog".to_string(),
            description: String::new(),
            fields: Vec::new(),
            output_obligation: None,
        }]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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
        agent_did: "did:test:test".to_string(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: "ActionRequest".to_string(),
            description: String::new(),
            fields: vec![
                WriteToolField {
                    name: "summary".to_string(),
                    required: true,
                    fill: None,
                },
                WriteToolField {
                    name: "summary".to_string(),
                    required: false,
                    fill: None,
                },
            ],
            output_obligation: None,
        }]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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

    // Every constructible native tool's `tool_name()` must appear in the
    // `NativeTool::ALL_NAMES` registry (guards it against drifting from
    // `tool_name` as variants are added), and every registry entry must be
    // reserved.
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
            NativeTool::ALL_NAMES.contains(&tool.tool_name().as_str()),
            "native tool {:?} missing from NativeTool::ALL_NAMES",
            tool.tool_name()
        );
        assert!(
            is_reserved_builtin_tool_name(&tool.tool_name()),
            "native tool {:?} must be reserved",
            tool.tool_name()
        );
    }
    for name in NativeTool::ALL_NAMES {
        assert!(
            is_reserved_builtin_tool_name(name),
            "registry name {name:?} must be reserved"
        );
    }
    assert!(is_reserved_builtin_tool_name("bash"));
    assert!(is_reserved_builtin_tool_name("bash_unrestricted"));
    assert!(is_reserved_builtin_tool_name("lsp"));

    for meta in crate::meta_tools::META_TOOL_NAMES {
        assert!(
            is_reserved_builtin_tool_name(meta),
            "meta tool {meta:?} must be reserved"
        );
    }
    for goal in [
        crate::goal::CREATE_GOAL_TOOL_NAME,
        crate::goal::GET_GOAL_TOOL_NAME,
        crate::goal::UPDATE_GOAL_TOOL_NAME,
    ] {
        assert!(
            is_reserved_builtin_tool_name(goal),
            "goal tool {goal:?} must be reserved"
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
        "agent_did": "did:test:test",
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
        tool_policy_version: Some(crate::tool_surface::TOOL_POLICY_V1.to_string()),
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
    assert_eq!(
        loaded.tool_policy_version,
        Some(crate::tool_surface::TOOL_POLICY_V1.to_string())
    );
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

#[tokio::test]
async fn tool_selection_update_can_clear_lsp_config() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let mut doc = ToolSelectionDocument {
        selection_id: "lsp-config-clear".to_string(),
        agent_did: "did:key:z-lsp-config-clear".to_string(),
        enable_lsp: Some(true),
        lsp_config: Some(r#"{"idle_timeout_ms":1000}"#.to_string()),
        ..Default::default()
    };
    upsert_tool_selection(&node, &doc).await.unwrap();
    let loaded = load_tool_selection(&node, &doc.selection_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.lsp_config, doc.lsp_config);

    doc.lsp_config = None;
    upsert_tool_selection(&node, &doc).await.unwrap();
    let cleared = load_tool_selection(&node, &doc.selection_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        cleared.lsp_config.as_deref().is_none_or(str::is_empty),
        "removing lsp_config from desired state must clear the stored override: {:?}",
        cleared.lsp_config
    );
}

#[tokio::test]
async fn tool_selection_document_round_trips_read_only_command_allowlist() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = ToolSelectionDocument {
        selection_id: "steward-readonly-allowlist".to_string(),
        agent_did: "did:key:z-test-allowlist".to_string(),
        read_only_command_allowlist: Some(vec!["jq".to_string(), "echo".to_string()]),
        ..Default::default()
    };
    upsert_tool_selection(&node, &doc)
        .await
        .expect("upsert should persist the read_only_command_allowlist field");

    let loaded = load_tool_selection(&node, "steward-readonly-allowlist")
        .await
        .expect("load should succeed")
        .expect("selection should exist");
    assert_eq!(
        loaded.read_only_command_allowlist,
        Some(vec!["jq".to_string(), "echo".to_string()]),
        "read_only_command_allowlist must round-trip through the GraphQL document representation"
    );
}

#[test]
fn read_only_command_allowlist_absent_decodes_to_none() {
    // A stored/manifest doc that omits the key must decode to None so the
    // runtime falls back to the hardcoded default_read_only_commands() list.
    let json = serde_json::json!({
        "selection_id": "sel-1",
        "agent_did": "did:test:test",
    });
    let loaded: ToolSelectionDocument = serde_json::from_value(json).unwrap();
    assert_eq!(loaded.read_only_command_allowlist, None);
}

#[test]
fn write_tools_round_trip() {
    let json = serde_json::json!({
        "selection_id": "sel-1",
        "agent_did": "did:test:test",
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
                    fill: None,
                },
                WriteToolField {
                    name: "summary".to_string(),
                    required: true,
                    fill: None,
                },
                WriteToolField {
                    name: "target_paths".to_string(),
                    required: false,
                    fill: None,
                },
            ],
            output_obligation: None,
        }]),
        datastore_tool_surface_ids: None,
        eth_tool_ids: None,
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
async fn tool_selection_document_round_trips_subagent_default_await_mode() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = ToolSelectionDocument {
        selection_id: "amy-background-tools".to_string(),
        agent_did: "did:key:z-test-background".to_string(),
        subagent_background_enabled: Some(true),
        subagent_default_await_mode: Some("background".to_string()),
        ..Default::default()
    };
    upsert_tool_selection(&node, &doc)
        .await
        .expect("upsert should persist the subagent default await mode");

    let loaded = load_tool_selection(&node, "amy-background-tools")
        .await
        .expect("load should succeed")
        .expect("selection should exist");
    assert_eq!(
        loaded.subagent_default_await_mode.as_deref(),
        Some("background")
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
        request_context_template: None,
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

#[tokio::test]
async fn inference_profile_completion_retry_fields_round_trip() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let profile = InferenceProfile {
        profile_id: "retry-profile".to_string(),
        display_name: Some("Retry Profile".to_string()),
        context_window: None,
        max_output_tokens: None,
        max_turns: None,
        temperature: None,
        reasoning_effort: Some("max".to_string()),
        stream_batch_ms: None,
        stream_liveness_timeout_secs: None,
        deadline_duration_secs: None,
        retry_max_transport: Some(4),
        retry_backoff_ms: Some(vec![1_000, 5_000, 30_000]),
        retry_max_resample: Some(2),
        retry_allow_repair: Some(false),
        retry_interactive_max: Some(2),
        ..Default::default()
    };

    upsert_inference_profile(&node, &profile)
        .await
        .expect("upsert should persist retry fields");

    let loaded = load_inference_profile(&node, "retry-profile")
        .await
        .expect("load should succeed")
        .expect("profile should exist");
    assert_eq!(loaded.retry_max_transport, Some(4));
    assert_eq!(loaded.reasoning_effort.as_deref(), Some("max"));
    assert_eq!(loaded.retry_backoff_ms, Some(vec![1_000, 5_000, 30_000]));
    assert_eq!(loaded.retry_max_resample, Some(2));
    assert_eq!(loaded.retry_allow_repair, Some(false));
    assert_eq!(loaded.retry_interactive_max, Some(2));
}

#[tokio::test]
async fn inference_profile_upsert_rejects_negative_seed() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let profile = InferenceProfile {
        profile_id: "negative-seed-profile".to_string(),
        seed: Some(-1),
        ..Default::default()
    };

    assert_eq!(
        upsert_inference_profile(&node, &profile)
            .await
            .unwrap_err()
            .to_string(),
        "InferenceProfile negative-seed-profile seed must be non-negative"
    );
}

#[test]
fn inference_profile_empty_retry_backoff_serializes_null_and_resolves_defaults() {
    let fields = crate::agent::completion_retry::CompletionRetryProfileFields {
        retry_backoff_ms: Some(Vec::new()),
        ..Default::default()
    };
    let resolved = crate::agent::completion_retry::CompletionRetryPolicy::resolve(
        &fields,
        crate::lifecycle::ExecutionOrigin::Scheduled,
    );
    assert_eq!(
        resolved,
        crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default()
    );

    let profile = InferenceProfile {
        profile_id: "empty-backoff".to_string(),
        display_name: None,
        context_window: None,
        max_output_tokens: None,
        max_turns: None,
        temperature: None,
        stream_batch_ms: None,
        stream_liveness_timeout_secs: None,
        deadline_duration_secs: None,
        retry_max_transport: None,
        retry_backoff_ms: Some(Vec::new()),
        retry_max_resample: None,
        retry_allow_repair: None,
        retry_interactive_max: None,
        ..Default::default()
    };
    let mutation = super::inference_profile::upsert_inference_profile_mutation(&profile);
    assert!(
        mutation.contains("retry_backoff_ms: null"),
        "empty retry_backoff_ms must render as null, not []; mutation was {mutation}"
    );
    assert!(
        !mutation.contains("retry_backoff_ms: []"),
        "DefraDB mutations must never emit [] for retry_backoff_ms"
    );
}

#[test]
fn completion_retry_policy_resolution_uses_origin_and_profile_fields() {
    let scheduled = crate::agent::completion_retry::CompletionRetryPolicy::resolve(
        &crate::agent::completion_retry::CompletionRetryProfileFields::default(),
        crate::lifecycle::ExecutionOrigin::Scheduled,
    );
    assert_eq!(
        scheduled,
        crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default()
    );

    let interactive_fields = crate::agent::completion_retry::CompletionRetryProfileFields {
        retry_interactive_max: Some(2),
        ..Default::default()
    };
    let interactive = crate::agent::completion_retry::CompletionRetryPolicy::resolve(
        &interactive_fields,
        crate::lifecycle::ExecutionOrigin::Interactive,
    );
    assert_eq!(
        interactive.transport_backoff,
        vec![
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(2)
        ]
    );

    let scheduled_fields = crate::agent::completion_retry::CompletionRetryProfileFields {
        retry_backoff_ms: Some(vec![1_000, 5_000]),
        ..Default::default()
    };
    let scheduled = crate::agent::completion_retry::CompletionRetryPolicy::resolve(
        &scheduled_fields,
        crate::lifecycle::ExecutionOrigin::Scheduled,
    );
    assert_eq!(
        scheduled.transport_backoff,
        vec![
            std::time::Duration::from_millis(1_000),
            std::time::Duration::from_millis(5_000)
        ]
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
        agent_did: "did:test:test".to_string(),
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
fn validate_rejects_background_default_when_background_disabled() {
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        subagent_background_enabled: Some(false),
        subagent_default_await_mode: Some("background".to_string()),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(
        format!("{}", result.unwrap_err()).contains("subagent_default_await_mode"),
        "error message must mention subagent_default_await_mode"
    );
}

#[test]
fn tool_selection_validation_reports_every_violation() {
    let doc = ToolSelectionDocument {
        selection_id: "invalid-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        subagent_targets: Some(vec![String::new()]),
        backgroundable_tool_names: Some(vec![String::new()]),
        subagent_background_enabled: Some(false),
        subagent_default_await_mode: Some("background".to_string()),
        ..Default::default()
    };

    let violations = doc.validation_violations();
    assert_eq!(violations.len(), 3, "{violations:?}");
    assert!(violations
        .iter()
        .any(|error| error.contains("subagent_targets[0]")));
    assert!(violations
        .iter()
        .any(|error| error.contains("backgroundable_tool_names[0]")));
    assert!(violations
        .iter()
        .any(|error| error.contains("subagent_default_await_mode")));
}

#[test]
fn validate_rejects_bare_string_subagent_target() {
    // A bare behavior-id string is NOT a valid SubagentTarget JSON entry.
    // The runtime silently drops non-JSON entries, so validate() must catch
    // this misconfiguration early with a clear error.
    let doc = ToolSelectionDocument {
        selection_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
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

#[test]
fn write_tool_fill_grammar_is_exact_and_runtime_fields_cannot_be_required() {
    let correlation: WriteToolField = serde_json::from_value(serde_json::json!({
        "name": "run_id",
        "fill": "correlation"
    }))
    .expect("correlation fill");
    assert_eq!(correlation.fill, Some(WriteToolFieldFill::Correlation));

    let source: WriteToolField = serde_json::from_value(serde_json::json!({
        "name": "expected_total",
        "fill": {"source_field": "expected_total"}
    }))
    .expect("source-field fill");
    assert_eq!(
        source.fill,
        Some(WriteToolFieldFill::SourceField("expected_total".into()))
    );

    for invalid in [
        serde_json::json!({"name": "run_id", "fill": "Correlation"}),
        serde_json::json!({"name": "run_id", "fill": {"source_field": "run-id"}}),
        serde_json::json!({"name": "run_id", "fill": {"source_field": "run_id", "extra": true}}),
    ] {
        assert!(serde_json::from_value::<WriteToolField>(invalid).is_err());
    }

    let doc = ToolSelectionDocument {
        selection_id: "filled-tools".into(),
        agent_did: "did:test:test".into(),
        write_tools: Some(vec![WriteToolDecl {
            tool_name: "write_result".into(),
            collection: "Result".into(),
            description: String::new(),
            fields: vec![WriteToolField {
                name: "run_id".into(),
                required: true,
                fill: Some(WriteToolFieldFill::Correlation),
            }],
            output_obligation: None,
        }]),
        ..Default::default()
    };
    assert!(doc.validate().is_err());
}

// ---------------------------------------------------------------------------
// InferenceProfile::validate (#1331) — table-driven from the historical
// gents-cli desired-state rules (crates/gents-cli/src/desired_state/validate/agent.rs).
// ---------------------------------------------------------------------------

fn base_profile(profile_id: &str) -> InferenceProfile {
    InferenceProfile {
        profile_id: profile_id.to_string(),
        ..Default::default()
    }
}

#[test]
fn inference_profile_validate_accepts_defaults() {
    assert!(base_profile("defaults").validate().is_ok());
}

#[test]
fn inference_profile_validate_rejects_non_positive_stream_liveness_timeout() {
    for value in [0, -1] {
        let mut profile = base_profile("liveness");
        profile.stream_liveness_timeout_secs = Some(value);
        let error = profile.validate().unwrap_err().to_string();
        assert!(
            error.contains("stream_liveness_timeout_secs must be positive"),
            "value {value}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_rejects_non_positive_deadline() {
    for value in [0, -1] {
        let mut profile = base_profile("deadline");
        profile.stream_liveness_timeout_secs = Some(300);
        profile.deadline_duration_secs = Some(value);
        let error = profile.validate().unwrap_err().to_string();
        assert!(
            error.contains("deadline_duration_secs must be positive"),
            "value {value}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_rejects_liveness_at_or_past_deadline() {
    for (liveness, deadline) in [(300, 300), (600, 300)] {
        let mut profile = base_profile("relationship");
        profile.stream_liveness_timeout_secs = Some(liveness);
        profile.deadline_duration_secs = Some(deadline);
        let error = profile.validate().unwrap_err().to_string();
        assert!(
            error.contains(&format!(
                "stream_liveness_timeout_secs ({liveness}) must be less than deadline_duration_secs ({deadline})"
            )),
            "liveness {liveness} deadline {deadline}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_accepts_liveness_shorter_than_deadline() {
    let mut profile = base_profile("ok-relationship");
    profile.stream_liveness_timeout_secs = Some(300);
    profile.deadline_duration_secs = Some(600);
    assert!(profile.validate().is_ok());
}

#[test]
fn inference_profile_validate_rejects_negative_seed() {
    let mut profile = base_profile("seeded");
    profile.seed = Some(-1);
    let error = profile.validate().unwrap_err().to_string();
    assert_eq!(error, "InferenceProfile seeded seed must be non-negative");
}

#[test]
fn inference_profile_validate_accepts_unset_reasoning_effort_in_every_empty_form() {
    for unset in [None, Some(""), Some("   ")] {
        let mut profile = base_profile("unset-effort");
        profile.reasoning_effort = unset.map(str::to_string);
        assert!(profile.validate().is_ok(), "unset form {unset:?} failed");
    }
}

#[test]
fn inference_profile_validate_accepts_every_reasoning_effort_in_the_vocabulary() {
    for value in [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ] {
        let mut profile = base_profile("vocab");
        profile.reasoning_effort = Some(value.to_string());
        assert!(profile.validate().is_ok(), "value {value} failed");
    }
}

#[test]
fn inference_profile_validate_rejects_reasoning_effort_outside_the_vocabulary() {
    let mut profile = base_profile("bad-effort");
    profile.reasoning_effort = Some("extreme".to_string());
    let error = profile.validate().unwrap_err().to_string();
    assert!(error.contains("reasoning_effort must be one of"), "{error}");
}

// ---------------------------------------------------------------------------
// Sampling bounds (#1331 fix round 1 — moved from the imperative
// `gents config profile set` writer, the only place that enforced them).
// ---------------------------------------------------------------------------

#[test]
fn inference_profile_validate_accepts_sampling_bounds_at_their_edges() {
    let mut profile = base_profile("edges");
    profile.top_p = Some(0.0);
    profile.min_p = Some(1.0);
    profile.top_k = Some(1);
    profile.repetition_penalty = Some(f64::MIN_POSITIVE);
    profile.frequency_penalty = Some(-2.0);
    profile.presence_penalty = Some(2.0);
    assert!(profile.validate().is_ok());
}

#[test]
fn inference_profile_validate_rejects_top_p_outside_unit_interval() {
    for value in [-0.01, 1.01] {
        let mut profile = base_profile("top-p");
        profile.top_p = Some(value);
        let error = profile.validate().unwrap_err().to_string();
        assert!(
            error.contains("top_p must be within [0, 1]"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_rejects_min_p_outside_unit_interval() {
    for value in [-0.01, 1.01] {
        let mut profile = base_profile("min-p");
        profile.min_p = Some(value);
        let error = profile.validate().unwrap_err().to_string();
        assert!(
            error.contains("min_p must be within [0, 1]"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_rejects_non_positive_top_k() {
    for value in [0, -1] {
        let mut profile = base_profile("top-k");
        profile.top_k = Some(value);
        let error = profile.validate().unwrap_err().to_string();
        assert!(error.contains("top_k must be positive"), "{value}: {error}");
    }
}

#[test]
fn inference_profile_validate_rejects_non_positive_repetition_penalty() {
    for value in [0.0, -1.0] {
        let mut profile = base_profile("rep-penalty");
        profile.repetition_penalty = Some(value);
        let error = profile.validate().unwrap_err().to_string();
        assert!(
            error.contains("repetition_penalty must be positive"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_rejects_frequency_and_presence_penalty_outside_range() {
    for value in [-2.01, 2.01] {
        let mut frequency = base_profile("freq-penalty");
        frequency.frequency_penalty = Some(value);
        let error = frequency.validate().unwrap_err().to_string();
        assert!(
            error.contains("frequency_penalty must be within [-2, 2]"),
            "{value}: {error}"
        );

        let mut presence = base_profile("presence-penalty");
        presence.presence_penalty = Some(value);
        let error = presence.validate().unwrap_err().to_string();
        assert!(
            error.contains("presence_penalty must be within [-2, 2]"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_profile_validate_reports_every_violation_at_once() {
    let mut profile = base_profile("multi-bad");
    profile.seed = Some(-1);
    profile.top_p = Some(2.0);
    profile.top_k = Some(0);
    let error = profile.validate().unwrap_err().to_string();
    assert!(error.contains("seed must be non-negative"), "{error}");
    assert!(error.contains("top_p must be within [0, 1]"), "{error}");
    assert!(error.contains("top_k must be positive"), "{error}");
}

// ---------------------------------------------------------------------------
// InferenceBackend::validate lives in `backend_registry` (crate root); see
// `crates/gents/src/backend_registry/tests.rs` for its table-driven cases.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// AgentBehavior::validate_references (#1331)
// ---------------------------------------------------------------------------

fn base_behavior(behavior_id: &str) -> AgentBehavior {
    AgentBehavior {
        behavior_id: behavior_id.to_string(),
        agent_did: "did:test:agent".to_string(),
        display_name: None,
        description: None,
        summary: None,
        system_prompt: None,
        request_context_template: None,
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
    }
}

fn refs_with(
    backends: &[(&str, &[&str])],
    tool_selections: &[&str],
    profiles: &[&str],
    skills: &[&str],
) -> ConfigReferences {
    ConfigReferences {
        backends: backends
            .iter()
            .map(|(id, models)| {
                (
                    id.to_string(),
                    models.iter().map(|m| m.to_string()).collect(),
                )
            })
            .collect(),
        tool_selections: tool_selections.iter().map(|s| s.to_string()).collect(),
        profiles: profiles.iter().map(|s| s.to_string()).collect(),
        skills: skills.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn agent_behavior_validate_references_accepts_no_references() {
    let behavior = base_behavior("empty");
    let refs = ConfigReferences::default();
    assert!(behavior.validate_references(&refs).is_ok());
}

#[test]
fn agent_behavior_validate_references_rejects_missing_backend() {
    let mut behavior = base_behavior("b");
    behavior.backend_id = Some("ghost".to_string());
    let refs = ConfigReferences::default();
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("references missing backend_id ghost"),
        "{error}"
    );
}

#[test]
fn agent_behavior_validate_references_rejects_model_not_advertised() {
    let mut behavior = base_behavior("b");
    behavior.backend_id = Some("reviewers".to_string());
    behavior.model_name = Some("GLM-5.2".to_string());
    let refs = refs_with(&[("reviewers", &["d4f"])], &[], &[], &[]);
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("selects model GLM-5.2 which backend reviewers does not advertise"),
        "{error}"
    );
}

#[test]
fn agent_behavior_validate_references_accepts_model_when_backend_advertises_none() {
    // An empty advertised-models list means "any model is accepted" — the
    // backend hasn't been probed, or advertises nothing specific.
    let mut behavior = base_behavior("b");
    behavior.backend_id = Some("reviewers".to_string());
    behavior.model_name = Some("anything".to_string());
    let refs = refs_with(&[("reviewers", &[])], &[], &[], &[]);
    assert!(behavior.validate_references(&refs).is_ok());
}

#[test]
fn agent_behavior_validate_references_accepts_advertised_model() {
    let mut behavior = base_behavior("b");
    behavior.backend_id = Some("reviewers".to_string());
    behavior.model_name = Some("d4f".to_string());
    let refs = refs_with(&[("reviewers", &["d4f"])], &[], &[], &[]);
    assert!(behavior.validate_references(&refs).is_ok());
}

#[test]
fn agent_behavior_validate_references_rejects_missing_tool_selection() {
    let mut behavior = base_behavior("b");
    behavior.tool_selection_id = Some("ghost-tools".to_string());
    let refs = ConfigReferences::default();
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("references missing tool_selection_id ghost-tools"),
        "{error}"
    );
}

#[test]
fn agent_behavior_validate_references_accepts_known_tool_selection() {
    let mut behavior = base_behavior("b");
    behavior.tool_selection_id = Some("known-tools".to_string());
    let refs = refs_with(&[], &["known-tools"], &[], &[]);
    assert!(behavior.validate_references(&refs).is_ok());
}

#[test]
fn agent_behavior_validate_references_rejects_missing_profile() {
    let mut behavior = base_behavior("b");
    behavior.inference_profile_id = Some("ghost-profile".to_string());
    let refs = ConfigReferences::default();
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("references missing inference_profile_id ghost-profile"),
        "{error}"
    );
}

#[test]
fn agent_behavior_validate_references_accepts_known_profile() {
    let mut behavior = base_behavior("b");
    behavior.inference_profile_id = Some("known-profile".to_string());
    let refs = refs_with(&[], &[], &["known-profile"], &[]);
    assert!(behavior.validate_references(&refs).is_ok());
}

#[test]
fn agent_behavior_validate_references_rejects_missing_skill_ref() {
    let mut behavior = base_behavior("b");
    behavior.skill_refs = vec!["ghost-skill".to_string()];
    let refs = ConfigReferences::default();
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("references missing skill_ref ghost-skill"),
        "{error}"
    );
}

#[test]
fn agent_behavior_validate_references_rejects_missing_skill_exclude() {
    let mut behavior = base_behavior("b");
    behavior.skill_excludes = vec!["ghost-skill".to_string()];
    let refs = ConfigReferences::default();
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("references missing skill_exclude ghost-skill"),
        "{error}"
    );
}

#[test]
fn agent_behavior_validate_references_accepts_known_skills() {
    let mut behavior = base_behavior("b");
    behavior.skill_refs = vec!["known-skill".to_string()];
    behavior.skill_excludes = vec!["known-skill".to_string()];
    let refs = refs_with(&[], &[], &[], &["known-skill"]);
    assert!(behavior.validate_references(&refs).is_ok());
}

#[test]
fn agent_behavior_reference_violations_reports_every_missing_reference_at_once() {
    // Regression (#1331 fix round 2): a behavior dangling on backend, tool
    // selection, AND profile simultaneously must surface all three, not
    // just the first-checked one — this is what desired state's
    // `config validate` renders as separate error-list entries.
    let mut behavior = base_behavior("b");
    behavior.backend_id = Some("missing-backend".to_string());
    behavior.tool_selection_id = Some("missing-tools".to_string());
    behavior.inference_profile_id = Some("missing-profile".to_string());
    let refs = ConfigReferences::default();

    let violations = behavior.reference_violations(&refs);
    assert_eq!(
        violations.len(),
        3,
        "expected 3 violations, got {violations:?}"
    );
    assert!(violations
        .iter()
        .any(|msg| msg.contains("missing backend_id missing-backend")));
    assert!(violations
        .iter()
        .any(|msg| msg.contains("missing tool_selection_id missing-tools")));
    assert!(violations
        .iter()
        .any(|msg| msg.contains("missing inference_profile_id missing-profile")));

    // validate_references (the Result wrapper) joins them into one error —
    // still all three, just not as separate Vec entries.
    let error = behavior.validate_references(&refs).unwrap_err().to_string();
    assert!(
        error.contains("missing backend_id missing-backend"),
        "{error}"
    );
    assert!(
        error.contains("missing tool_selection_id missing-tools"),
        "{error}"
    );
    assert!(
        error.contains("missing inference_profile_id missing-profile"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// ConfigReferences::load — lenient backend parsing (#1331, fix round 1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_references_load_tolerates_a_malformed_backend_row_beside_a_good_one() {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();

    // Missing max_concurrent: `InferenceBackend::from_value` (the strict
    // registry parser) requires it and would fail on this row. It still has
    // a usable backend_id, so `ConfigReferences::load` — which reads
    // backend_id/models directly, not through that parser — must not sink
    // the whole load over it.
    let malformed = node
        .execute(
            r#"mutation {
                create_InferenceBackend(input: {
                    backend_id: "malformed-backend",
                    name: "Malformed",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "http://127.0.0.1:11434/v1",
                    enabled: true,
                    models: ["should-not-matter"],
                    probe_status: "unknown"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!malformed.has_errors(), "{:?}", malformed.errors);

    let good = node
        .execute(
            r#"mutation {
                create_InferenceBackend(input: {
                    backend_id: "good-backend",
                    name: "Good",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "http://127.0.0.1:11434/v1",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["model-a", "model-b"],
                    probe_status: "healthy"
                }) { _docID }
            }"#,
        )
        .await;
    assert!(!good.has_errors(), "{:?}", good.errors);

    let txn = crate::config_client::ConfigApplyTxn::begin_local(&node, None)
        .await
        .expect("begin reference-load transaction");
    let refs = ConfigReferences::load_in_txn(&txn, "did:test:whatever")
        .await
        .expect("load must tolerate a malformed unrelated backend row");
    txn.discard().await.expect("discard read-only transaction");

    assert_eq!(
        refs.backends.get("good-backend").map(Vec::as_slice),
        Some(&["model-a".to_string(), "model-b".to_string()][..]),
        "the good backend's models must still come through"
    );
    // The malformed row's own backend_id is fine — only max_concurrent is
    // missing — so it's present too; ConfigReferences skips a row only when
    // even backend_id is unusable. The point of this test is that its
    // absence of max_concurrent doesn't fail the whole load, not that the
    // row itself is excluded.
    assert!(refs.backends.contains_key("malformed-backend"));
}
