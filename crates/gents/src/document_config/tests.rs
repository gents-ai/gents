use super::*;
use crate::config_client::write_tools_document;

/// The implicit wide-open preset builder was retired with ToolSelection; the
/// preset is now an authored nested `Tools` document. Pin its explicit
/// permissive surface: meta dispatch and DefraDB query capabilities on, every
/// privilege-bearing host capability absent (absence grants nothing).
fn wide_open_tools_document(agent_did: &str) -> Tools {
    serde_json::from_value(serde_json::json!({
        "tools_id": "wide-open",
        "agent_did": agent_did,
        "built_ins": {"enable_context_budget": true},
        "datastore": {"enable_defra_query": true},
    }))
    .unwrap()
}

#[test]
fn wide_open_preset_is_permissive_and_explicit() {
    let did = "did:test:amy";
    let preset = wide_open_tools_document(did);
    // Owner-scoped logical id; `agent_did` passes the per-principal filter.
    assert_eq!(preset.tools_id, "wide-open");
    assert_eq!(preset.agent_did, did);
    // Two capabilities are explicitly enabled by this preset.
    assert_eq!(
        preset.built_ins.as_ref().unwrap().enable_context_budget,
        Some(true)
    );
    assert_eq!(
        preset.datastore.as_ref().unwrap().enable_defra_query,
        Some(true)
    );
    // No host tools, remote services (hence no meta dispatch), subagents,
    // integrations, or self-config: the permissive surface is explicit, never
    // implied by a policy version — the canonical document has no historical
    // re-interpretation contract.
    assert!(preset.host.is_none());
    assert!(preset.remote.is_none());
    assert!(preset.subagents.is_none());
    assert!(preset.integrations.is_none());
    assert!(preset.self_config.is_none());
    assert!(preset.validate().is_ok());
}

#[test]
fn tools_document_accepts_empty_string_arrays_and_null_groups() {
    let document: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "default-tools",
        "agent_did": "did:test:test",
        "display_name": "Tools",
        "host": {
            "files": {"mode": "ReadOnly"},
            "bash": {"mode": null},
            "cli": null
        },
        "built_ins": null,
        "remote": {"services": null}
    }))
    .expect("null groups and empty-string arrays should deserialize");

    // Null/empty list forms (DefraDB empty-list transport) decode to the
    // canonical empty defaults, never an implicit grant.
    assert!(document.host.as_ref().unwrap().cli.is_empty());
    assert!(document.remote.as_ref().unwrap().services.is_empty());
    // Goal capabilities are opt-in only; unset means disabled.
    assert!(document.built_ins.is_none());
    assert_eq!(
        document.host.as_ref().unwrap().files.as_ref().unwrap().mode,
        crate::tool_surface::FileToolMode::ReadOnly
    );
}

#[test]
fn tools_document_accepts_explicit_selections() {
    let document: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "default-tools",
        "agent_did": "did:test:test",
        "remote": {"services": [{"mcp_service_id": "x-data", "tool_names": ["search"]}]},
        "host": {"cli": [{"name": "rg"}]}
    }))
    .expect("explicit selections should deserialize");

    assert_eq!(
        document.remote.as_ref().unwrap().services[0].tool_names,
        vec!["search".to_string()]
    );
    assert_eq!(document.host.as_ref().unwrap().cli[0].name, "rg");
}

#[test]
fn required_mcp_service_selection_is_explicit_and_unique() {
    // Required is authored on the same explicit service selection. This test
    // checks its local shape; runtime readiness remains the admission owner.
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "required-mcp",
        "agent_did": "did:key:test",
        "remote": {"services": [{"mcp_service_id": "research", "tool_names": ["search"], "required": true}]},
    }))
    .unwrap();
    tools.validate().unwrap();
    // A duplicate service id stays invalid regardless of required status.
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "required-mcp",
        "agent_did": "did:key:test",
        "remote": {"services": [
            {"mcp_service_id": "research", "required": true},
            {"mcp_service_id": "research"}
        ]},
    }))
    .unwrap();
    let error = tools.validate().unwrap_err().to_string();
    assert!(
        error.contains("remote.services") && error.contains("duplicate name"),
        "duplicate service ids must be rejected: {error}"
    );
}

#[test]
fn validate_rejects_blank_subagent_target_ids() {
    let doc = Tools {
        tools_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        subagents: Some(SubagentTools {
            target_ids: vec!["".to_string()],
            spawn_enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(
        format!("{}", result.unwrap_err()).contains("subagents.target_ids"),
        "error message must mention subagents.target_ids"
    );
}

/// Validate write-tool declarations through the shared registration owner used
/// by every write path (`document_config::write_tool`); `cli_tool_names` and
/// additional runtime tools are empty unless a collision test provides them.
fn validate_write_tools(decls: &[WriteToolDecl]) -> anyhow::Result<()> {
    super::validate_write_tool_declarations(decls, &[], &[])
}

#[test]
fn validate_rejects_write_tool_with_empty_tool_name() {
    let decls = vec![WriteToolDecl {
        tool_name: "   ".to_string(),
        collection: "ActionRequest".to_string(),
        description: String::new(),
        fields: Vec::new(),
        output_obligation: None,
    }];
    let result = validate_write_tools(&decls);
    assert!(result.is_err(), "empty tool_name must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("write_tools") && err.contains("malformed"),
        "error message must mention write_tools and the malformed declaration: {err}"
    );
}

#[test]
fn validate_rejects_invalid_write_tool_collection_identifiers() {
    for collection in ["  ", "ActionRequest) { _docID } mutation {"] {
        let decls = vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: collection.to_string(),
            description: String::new(),
            fields: Vec::new(),
            output_obligation: None,
        }];
        let err = validate_write_tools(&decls)
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
        let decls = vec![WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: "ActionRequest".to_string(),
            description: String::new(),
            fields: vec![WriteToolField {
                name: field_name.to_string(),
                required: true,
                fill: None,
            }],
            output_obligation: None,
        }];
        let err = validate_write_tools(&decls)
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
    assert!(
        decl.validate()
            .expect_err("models must not choose requester identity")
            .to_string()
            .contains("must be runtime-filled")
    );

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
    let decls = vec![decl("ActionRequest"), decl("OtherCollection")];
    let result = validate_write_tools(&decls);
    assert!(result.is_err(), "duplicate tool_name must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("write_tools") && err.contains("request_action"),
        "error must name write_tools and the duplicated tool: {err}"
    );
}

#[test]
fn validate_accepts_well_formed_write_tools() {
    let decls = vec![
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
    ];
    assert!(
        validate_write_tools(&decls).is_ok(),
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

    let decls = vec![WriteToolDecl {
        output_obligation: Some(WriteToolOutputObligation {
            scope: WriteToolOutputObligationScope::Trigger,
            minimum_writes: 0,
            expected_count_field: None,
        }),
        ..decl
    }];
    let error = validate_write_tools(&decls).unwrap_err().to_string();
    assert!(error.contains("minimum_writes"));
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
        let decls = vec![WriteToolDecl {
            tool_name: "write_result".to_string(),
            collection: "Result".to_string(),
            description: String::new(),
            fields,
            output_obligation: Some(WriteToolOutputObligation {
                scope: WriteToolOutputObligationScope::Trigger,
                minimum_writes: 1,
                expected_count_field: Some("expected_total".to_string()),
            }),
        }];
        assert!(
            validate_write_tools(&decls)
                .unwrap_err()
                .to_string()
                .contains("expected_count_field")
        );
    }
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_builtin() {
    // `read_file` is a native tool; reusing it as a write-tool name would
    // silently shadow the native impl at registration.
    let decls = vec![WriteToolDecl {
        tool_name: "read_file".to_string(),
        collection: "AuditLog".to_string(),
        description: String::new(),
        fields: vec![WriteToolField {
            name: "path".to_string(),
            required: true,
            fill: None,
        }],
        output_obligation: None,
    }];
    let result = validate_write_tools(&decls);
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
    let decls = vec![WriteToolDecl {
        tool_name: "defra_query".to_string(),
        collection: "AuditLog".to_string(),
        description: String::new(),
        fields: Vec::new(),
        output_obligation: None,
    }];
    assert!(
        validate_write_tools(&decls).is_err(),
        "collision with the built-in defra_query tool must be rejected"
    );
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_cli_tool() {
    // A host.cli entry is advertised as its own tool in the same Tools
    // document, so a write tool reusing that name is a dispatch collision.
    let decls = vec![WriteToolDecl {
        tool_name: "rg".to_string(),
        collection: "AuditLog".to_string(),
        description: String::new(),
        fields: Vec::new(),
        output_obligation: None,
    }];
    let cli_tool_names = vec!["rg".to_string()];
    let result = super::validate_write_tool_declarations(&decls, &cli_tool_names, &[]);
    assert!(
        result.is_err(),
        "collision with a host.cli entry must be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("rg") && err.contains("CLI entry"),
        "error must name the colliding tool and the category: {err}"
    );
}

#[test]
fn validate_rejects_duplicate_field_names_within_decl() {
    let decls = vec![WriteToolDecl {
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
    }];
    let result = validate_write_tools(&decls);
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
    let json = serde_json::json!([{
        "tool_name": "  request_action  ",
        "collection": " ActionRequest ",
        "fields": [{ "name": "  summary  ", "required": true }]
    }]);
    let loaded = super::deserialize_dual_shape::<WriteToolDecl>(Some(json), "write_tools").unwrap();
    let decl = &loaded[0];
    assert_eq!(decl.tool_name, "request_action");
    assert_eq!(decl.collection, "ActionRequest");
    assert_eq!(decl.fields[0].name, "summary");
}

#[tokio::test]
async fn tools_document_round_trips_defra_query_fields() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = Tools {
        tools_id: "amy-general-tools".to_string(),
        agent_did: "did:key:z-test".to_string(),
        built_ins: Some(BuiltInTools {
            enable_session_history_tool: Some(true),
            ..Default::default()
        }),
        datastore: Some(DatastoreTools {
            enable_defra_query: Some(false),
            defra_query_collections: Some(vec![
                "AgentRequest".to_string(),
                "AgentResponse".to_string(),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let access = crate::config_client::ConfigAccess::Local(std::sync::Arc::new(node));
    write_tools_document(&access, &doc)
        .await
        .expect("write should persist the defra_query fields");

    let loaded: Tools = access
        .transact("test.tools.read", |txn| {
            let doc = &doc;
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Tools,
                    &doc.agent_did,
                    &doc.tools_id,
                )
                .await?
                .map(|(_, value)| {
                    serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                })
                .transpose()
            })
        })
        .await
        .expect("read should succeed")
        .expect("tools should exist");
    assert_eq!(
        loaded
            .built_ins
            .as_ref()
            .unwrap()
            .enable_session_history_tool,
        Some(true)
    );
    assert_eq!(
        loaded.datastore.as_ref().unwrap().enable_defra_query,
        Some(false)
    );
    assert_eq!(
        loaded.datastore.as_ref().unwrap().defra_query_collections,
        Some(vec![
            "AgentRequest".to_string(),
            "AgentResponse".to_string()
        ])
    );
}

#[tokio::test]
async fn tools_update_can_clear_lsp_config() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let access = crate::config_client::ConfigAccess::Local(std::sync::Arc::new(node));
    let lsp = |config: Option<String>| Tools {
        tools_id: "lsp-config-clear".to_string(),
        agent_did: "did:key:z-lsp-config-clear".to_string(),
        integrations: Some(IntegrationTools {
            lsp: config.map(|config| LspTools {
                config: Some(config),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    let doc = lsp(Some(r#"{"idle_timeout_ms":1000}"#.to_string()));
    write_tools_document(&access, &doc).await.unwrap();
    async fn read(access: &crate::config_client::ConfigAccess) -> anyhow::Result<Option<Tools>> {
        access
            .transact("test.tools.read", |txn| {
                Box::pin(async move {
                    crate::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Tools,
                        "did:key:z-lsp-config-clear",
                        "lsp-config-clear",
                    )
                    .await?
                    .map(|(_, value)| {
                        serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                    })
                    .transpose()
                })
            })
            .await
    }
    let loaded: Tools = read(&access).await.unwrap().unwrap();
    assert_eq!(
        loaded
            .integrations
            .as_ref()
            .unwrap()
            .lsp
            .as_ref()
            .unwrap()
            .config,
        Some(r#"{"idle_timeout_ms":1000}"#.to_string())
    );

    write_tools_document(&access, &lsp(None)).await.unwrap();
    let cleared: Tools = read(&access).await.unwrap().unwrap();
    let cleared_config = cleared
        .integrations
        .as_ref()
        .unwrap()
        .lsp
        .as_ref()
        .and_then(|lsp| lsp.config.as_deref());
    assert!(
        cleared_config.is_none_or(str::is_empty),
        "removing lsp config from desired state must clear the stored override: {:?}",
        cleared_config
    );
}

#[tokio::test]
async fn tools_document_round_trips_read_only_commands() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = Tools {
        tools_id: "steward-readonly-allowlist".to_string(),
        agent_did: "did:key:z-test-allowlist".to_string(),
        host: Some(HostTools {
            bash: Some(BashTools {
                read_only_commands: Some(vec!["jq".to_string(), "echo".to_string()]),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let access = crate::config_client::ConfigAccess::Local(std::sync::Arc::new(node));
    write_tools_document(&access, &doc)
        .await
        .expect("write should persist the read_only_commands field");

    let loaded: Tools = access
        .transact("test.tools.read", |txn| {
            let doc = &doc;
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Tools,
                    &doc.agent_did,
                    &doc.tools_id,
                )
                .await?
                .map(|(_, value)| {
                    serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                })
                .transpose()
            })
        })
        .await
        .expect("read should succeed")
        .expect("tools should exist");
    assert_eq!(
        loaded
            .host
            .as_ref()
            .unwrap()
            .bash
            .as_ref()
            .unwrap()
            .read_only_commands,
        Some(vec!["jq".to_string(), "echo".to_string()]),
        "read_only_commands must round-trip through the GraphQL document representation"
    );
}

#[test]
fn read_only_commands_absent_decodes_to_none() {
    // A stored/manifest doc that omits the key must decode to None so the
    // runtime falls back to the hardcoded default_read_only_commands() list.
    let json = serde_json::json!({
        "tools_id": "sel-1",
        "agent_did": "did:test:test",
        "host": {"bash": {}},
    });
    let loaded: Tools = serde_json::from_value(json).unwrap();
    assert_eq!(
        loaded
            .host
            .as_ref()
            .unwrap()
            .bash
            .as_ref()
            .unwrap()
            .read_only_commands,
        None
    );
}

#[test]
fn write_tools_round_trip() {
    let decls = vec![WriteToolDecl {
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
    }];
    let serialized = serde_json::to_value(&decls).unwrap();
    let parsed: Vec<WriteToolDecl> = serde_json::from_value(serialized).unwrap();
    assert_eq!(parsed, decls);
    assert_eq!(parsed[0].fields.len(), 3);
    assert!(parsed[0].fields[0].required);
    assert!(!parsed[0].fields[2].required);
    // The DefraDB `[String]` storage form also round-trips through the
    // dual-shape deserializer shared with surface entries.
    let stored = serde_json::json!([serde_json::to_string(&decls[0]).unwrap()]);
    let loaded =
        super::deserialize_dual_shape::<WriteToolDecl>(Some(stored), "write_tools").unwrap();
    assert_eq!(loaded, decls);
}

#[tokio::test]
async fn tools_document_round_trips_write_tools() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let decls = vec![WriteToolDecl {
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
    }];
    // Canonical Tools carries datastore surface references; create/query tool
    // declarations are expanded from the referenced DatastoreToolSurface.
    let tools = Tools {
        tools_id: "steward-write-tools".to_string(),
        agent_did: "did:key:z-test-write".to_string(),
        datastore: Some(DatastoreTools {
            datastore_tool_surface_ids: Some(vec!["surface".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let surface: DatastoreToolSurfaceDocument = serde_json::from_value(serde_json::json!({
        "surface_id": "surface",
        "agent_did": "did:key:z-test-write",
        "entries": decls,
    }))
    .unwrap();
    let access = crate::config_client::ConfigAccess::Local(std::sync::Arc::new(node));
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::DatastoreToolSurface,
            add: serde_json::to_value(&surface).unwrap(),
            update: serde_json::to_value(&surface).unwrap(),
        },
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Tools,
            add: serde_json::to_value(&tools).unwrap(),
            update: serde_json::to_value(&tools).unwrap(),
        },
    ])
    .unwrap();
    access
        .transact("test.tools.write", |txn| {
            let plan = &plan;
            let tools = &tools;
            let surface = &surface;
            let decls = &decls;
            Box::pin(async move {
                crate::config_client::apply_desired_state_plan(txn, plan).await?;
                let merged = super::merge_datastore_tool_surfaces(tools, [surface])?;
                assert_eq!(
                    merged.write_tools, *decls,
                    "write_tools must expand from the referenced surface through the desired-state owner"
                );
                Ok(())
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn tools_document_round_trips_subagent_default_await_mode() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    let doc = Tools {
        tools_id: "amy-background-tools".to_string(),
        agent_did: "did:key:z-test-background".to_string(),
        subagents: Some(SubagentTools {
            background_enabled: Some(true),
            default_await_mode: Some("background".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let access = crate::config_client::ConfigAccess::Local(std::sync::Arc::new(node));
    write_tools_document(&access, &doc)
        .await
        .expect("write should persist the subagent default await mode");

    let loaded: Tools = access
        .transact("test.tools.read", |txn| {
            let doc = &doc;
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Tools,
                    &doc.agent_did,
                    &doc.tools_id,
                )
                .await?
                .map(|(_, value)| {
                    serde_json::from_value::<Tools>(value).map_err(anyhow::Error::from)
                })
                .transpose()
            })
        })
        .await
        .expect("read should succeed")
        .expect("tools should exist");
    assert_eq!(
        loaded
            .subagents
            .as_ref()
            .unwrap()
            .default_await_mode
            .as_deref(),
        Some("background")
    );
}

#[tokio::test]
async fn agent_behavior_description_round_trip_with_explicit_owner_fixture() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    // Canonical AgentBehavior(context_id, inference_profile_id, tags) carries
    // UI text and references only. Literal instructions, skill selection, tools,
    // and compaction live on AgentContext; model selection lives on
    // InferenceProfile — the retired parallel behavior fields (summary,
    // system_prompt, backend/model, compaction, skill lists) must not return.
    // install_test_behavior provides the explicit context/tools/profile/backend
    // chain; the principal bootstrap itself creates no executable configuration.
    let agent_did = "did:key:z-test-desc";
    crate::test_support::install_test_behavior(&node, agent_did, "amy-general").await;

    // Compact authored document. `tags: null` is the DefraDB empty-list form and
    // must decode to the empty default; an omitted context_id decodes to None
    // (no instructions, skills, or tools; runtime-default compaction).
    let authored = serde_json::json!({
        "behavior_id": "amy-general",
        "agent_did": agent_did,
        "display_name": "Amy General",
        "description": "A general-purpose assistant for research and writing.",
        "context_id": null,
        "inference_profile_id": "amy-general:inference",
        "tags": null,
    });
    let sparse: AgentBehavior = serde_json::from_value(authored.clone()).unwrap();
    assert_eq!(
        sparse.context_id, None,
        "null context_id must decode to None"
    );
    assert!(
        sparse.tags.is_empty(),
        "null tags must decode to the empty default"
    );
    assert!(sparse.enabled, "omitted enabled must default to true");
    // Compact roundtrip: no disabled/empty boilerplate is re-emitted.
    assert_eq!(
        serde_json::to_value(&sparse).unwrap(),
        serde_json::json!({
            "behavior_id": "amy-general",
            "agent_did": agent_did,
            "display_name": "Amy General",
            "description": "A general-purpose assistant for research and writing.",
            "inference_profile_id": "amy-general:inference",
        })
    );

    let doc = AgentBehavior {
        behavior_id: "amy-general".to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some("Amy General".to_string()),
        description: Some("A general-purpose assistant for research and writing.".to_string()),
        context_id: Some("amy-general:context".to_string()),
        inference_profile_id: "amy-general:inference".to_string(),
        enabled: true,
        tags: Vec::new(),
        created_at: None,
    };
    upsert_agent_behavior(&node, &doc)
        .await
        .expect("upsert should persist description fields");

    let loaded = load_agent_behavior(&node, "amy-general")
        .await
        .expect("load should succeed")
        .expect("behavior should exist after upsert");
    assert_eq!(
        loaded.description,
        Some("A general-purpose assistant for research and writing.".to_string()),
        "description must round-trip through upsert/load"
    );
    assert_eq!(loaded.context_id, doc.context_id);
    assert_eq!(loaded.inference_profile_id, doc.inference_profile_id);
    assert!(loaded.enabled);
}

/// Borrow the embedded node back out of a Local `ConfigAccess` for the
/// document-level readers that still take `&EmbeddedNode`.
fn access_node(access: &crate::config_client::ConfigAccess) -> &defra_node::EmbeddedNode {
    match access {
        crate::config_client::ConfigAccess::Local(node) => node,
        crate::config_client::ConfigAccess::Graphql(_) => {
            unreachable!("test helpers use Local access")
        }
    }
}

#[tokio::test]
async fn inference_retry_policy_fields_round_trip() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();

    // Retry knobs are now owned by the referenced InferenceRetryPolicy, not the
    // profile; typed effort lives on the profile. The backend must exist for
    // the references closure to accept the profile.
    let retry = InferenceRetryPolicy {
        agent_did: "did:key:z-test-retry".to_string(),
        retry_policy_id: "retry-policy".to_string(),
        display_name: Some("Retry Policy".to_string()),
        max_transport_retries: Some(4),
        backoff_ms: Some(vec![1_000, 5_000, 30_000]),
        max_resample_retries: Some(2),
        allow_repair: Some(false),
        interactive_max_retries: Some(2),
        ..Default::default()
    };
    let backend: crate::document_config::InferenceBackend =
        serde_json::from_value(serde_json::json!({
            "agent_did": "did:key:z-test-retry",
            "backend_id": "backend",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:1/v1",
            "auth": {"kind": "unauthenticated"}
        }))
        .unwrap();
    let profile = InferenceProfile {
        agent_did: "did:key:z-test-retry".to_string(),
        profile_id: "retry-profile".to_string(),
        display_name: Some("Retry Profile".to_string()),
        backend_id: "backend".to_string(),
        model_name: "test-model".to_string(),
        reasoning_effort: Some(crate::config::ReasoningEffort::Max),
        execution_id: Some("retry-execution".to_string()),
        ..Default::default()
    };
    let execution = InferenceExecution {
        agent_did: "did:key:z-test-retry".to_string(),
        execution_id: "retry-execution".to_string(),
        retry_policy_id: Some("retry-policy".to_string()),
        ..Default::default()
    };

    let access = crate::config_client::ConfigAccess::Local(std::sync::Arc::new(node));
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::InferenceBackend,
            add: serde_json::to_value(&backend).unwrap(),
            update: serde_json::to_value(&backend).unwrap(),
        },
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::InferenceRetryPolicy,
            add: serde_json::to_value(&retry).unwrap(),
            update: serde_json::to_value(&retry).unwrap(),
        },
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::InferenceExecution,
            add: serde_json::to_value(&execution).unwrap(),
            update: serde_json::to_value(&execution).unwrap(),
        },
        crate::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::InferenceProfile,
            add: serde_json::to_value(&profile).unwrap(),
            update: serde_json::to_value(&profile).unwrap(),
        },
    ])
    .expect("retry chain plan must be valid");
    access
        .transact("test.retry_chain.write", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        })
        .await
        .expect("apply should persist the retry chain");

    let loaded = load_inference_profile(
        &access_node(&access),
        "did:key:z-test-retry",
        "retry-profile",
    )
    .await
    .expect("load should succeed")
    .expect("profile should exist");
    assert_eq!(
        loaded.reasoning_effort,
        Some(crate::config::ReasoningEffort::Max)
    );
    assert_eq!(loaded.execution_id.as_deref(), Some("retry-execution"));
}

#[test]
fn inference_sampling_rejects_negative_seed() {
    // Seed is owned by InferenceSampling (#1430 split); the canonical validator
    // rejects it before any write is staged, so the DB-level negative-seed
    // rejection now lives with this owner.
    let sampling = InferenceSampling {
        agent_did: "did:key:z-test-seed".to_string(),
        sampling_id: "negative-seed-sampling".to_string(),
        seed: Some(-1),
        ..Default::default()
    };

    assert_eq!(
        sampling.validate().unwrap_err().to_string(),
        "InferenceSampling negative-seed-sampling seed must be non-negative"
    );
}

#[test]
fn inference_retry_policy_null_backoff_decodes_to_defaults() {
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

    // `backoff_ms: null` (DefraDB's unset form — never `[]`) decodes to the
    // unset default and resolves the scheduled ladder.
    let retry: InferenceRetryPolicy = serde_json::from_value(serde_json::json!({
        "agent_did": "did:key:z-test-backoff",
        "retry_policy_id": "empty-backoff",
        "backoff_ms": null,
    }))
    .unwrap();
    assert_eq!(retry.backoff_ms, None);
    let fields = crate::agent::completion_retry::CompletionRetryProfileFields {
        retry_max_transport: retry.max_transport_retries,
        retry_backoff_ms: retry.backoff_ms,
        retry_max_resample: retry.max_resample_retries,
        retry_allow_repair: retry.allow_repair,
        retry_interactive_max: retry.interactive_max_retries,
    };
    let resolved = crate::agent::completion_retry::CompletionRetryPolicy::resolve(
        &fields,
        crate::lifecycle::ExecutionOrigin::Scheduled,
    );
    assert_eq!(
        resolved,
        crate::agent::completion_retry::CompletionRetryPolicy::scheduled_default()
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
fn validate_accepts_well_formed_subagent_target_documents() {
    // Delegation targets are SubagentTarget documents: same-owner behavior
    // references close through the references closure, foreign destinations are
    // explicit. The calling owner owns each target document; target_agent_did
    // owns the destination behavior.
    let code_entry = SubagentTargetDocument {
        target_id: "amy-code".to_string(),
        agent_did: "did:key:zParent".to_string(),
        target_agent_did: "did:key:zParent".to_string(),
        behavior_id: "amy-code".to_string(),
        name: "Code assistant".to_string(),
        description: None,
        tags: Vec::new(),
    };
    let research_entry = SubagentTargetDocument {
        target_id: "amy-research".to_string(),
        agent_did: "did:key:zParent".to_string(),
        target_agent_did: "did:key:zDestination".to_string(),
        behavior_id: "amy-research".to_string(),
        name: "amy-research".to_string(),
        description: None,
        tags: Vec::new(),
    };
    // Canonical documents deserialize strictly and preserve their fields.
    for entry in [&code_entry, &research_entry] {
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(
            serde_json::from_value::<SubagentTargetDocument>(value).unwrap(),
            *entry
        );
    }
    // The Tools subagent group references these by target_id only.
    let doc = Tools {
        tools_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        subagents: Some(SubagentTools {
            target_ids: vec!["amy-code".to_string(), "amy-research".to_string()],
            spawn_enabled: Some(true),
            steering_enabled: Some(false),
            background_enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        doc.validate().is_ok(),
        "well-formed subagent target references must be accepted"
    );
}

#[test]
fn validate_rejects_background_default_when_background_disabled() {
    let doc = Tools {
        tools_id: "test-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        subagents: Some(SubagentTools {
            background_enabled: Some(false),
            default_await_mode: Some("background".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(
        format!("{}", result.unwrap_err()).contains("default_await_mode"),
        "error message must mention subagents.default_await_mode"
    );
}

#[test]
fn tools_validation_reports_every_violation() {
    let doc = Tools {
        tools_id: "invalid-tools".to_string(),
        agent_did: "did:test:test".to_string(),
        subagents: Some(SubagentTools {
            target_ids: vec![String::new()],
            background_enabled: Some(false),
            default_await_mode: Some("background".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let violations = doc.validation_violations();
    assert_eq!(violations.len(), 2, "{violations:?}");
    assert!(
        violations
            .iter()
            .any(|error| error.contains("subagents.target_ids"))
    );
    assert!(
        violations
            .iter()
            .any(|error| error.contains("default_await_mode"))
    );
}

#[test]
fn validate_rejects_undeclared_subagent_target_reference() {
    // SubagentTools.target_ids are references to SubagentTarget documents; the
    // references closure rejects undeclared ids early with a clear error.
    let refs = ConfigReferences::from_documents(
        "did:test:test",
        vec![(
            crate::Collection::Tools,
            serde_json::json!({
                "tools_id": "test-tools",
                "agent_did": "did:test:test",
                "subagents": {"target_ids": ["amy-code"], "spawn_enabled": true}
            }),
        )],
    )
    .unwrap();
    let result = refs.validate();
    assert!(
        result.is_err(),
        "undeclared subagent target reference must be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("subagents.target_ids"),
        "error must mention subagents.target_ids; got: {err_msg}"
    );
    assert!(
        err_msg.contains("SubagentTarget"),
        "error must mention the SubagentTarget collection; got: {err_msg}"
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

    let decls = vec![WriteToolDecl {
        tool_name: "write_result".into(),
        collection: "Result".into(),
        description: String::new(),
        fields: vec![WriteToolField {
            name: "run_id".into(),
            required: true,
            fill: Some(WriteToolFieldFill::Correlation),
        }],
        output_obligation: None,
    }];
    assert!(validate_write_tools(&decls).is_err());
}

// ---------------------------------------------------------------------------
// InferenceSampling::validate (#1331 rules, #1430 owner split) — table-driven
// from the historical gents-cli desired-state rules
// (crates/gents-cli/src/desired_state/validate/agent.rs).
// ---------------------------------------------------------------------------

fn base_sampling(sampling_id: &str) -> InferenceSampling {
    InferenceSampling {
        agent_did: "owner".to_string(),
        sampling_id: sampling_id.to_string(),
        ..Default::default()
    }
}

fn base_execution(execution_id: &str) -> InferenceExecution {
    InferenceExecution {
        agent_did: "owner".to_string(),
        execution_id: execution_id.to_string(),
        ..Default::default()
    }
}

#[test]
fn inference_execution_validate_accepts_defaults() {
    assert!(base_execution("defaults").validate().is_ok());
}

#[test]
fn inference_execution_validate_rejects_non_positive_stream_liveness_timeout() {
    for value in [0, -1] {
        let mut execution = base_execution("liveness");
        execution.stream_liveness_timeout_secs = Some(value);
        let error = execution.validate().unwrap_err().to_string();
        assert!(
            error.contains("stream_liveness_timeout_secs must be positive"),
            "value {value}: {error}"
        );
    }
}

#[test]
fn inference_execution_validate_rejects_non_positive_deadline() {
    for value in [0, -1] {
        let mut execution = base_execution("deadline");
        execution.stream_liveness_timeout_secs = Some(300);
        execution.deadline_duration_secs = Some(value);
        let error = execution.validate().unwrap_err().to_string();
        assert!(
            error.contains("deadline_duration_secs must be positive"),
            "value {value}: {error}"
        );
    }
}

#[test]
fn inference_execution_validate_rejects_liveness_at_or_past_deadline() {
    for (liveness, deadline) in [(300, 300), (600, 300)] {
        let mut execution = base_execution("relationship");
        execution.stream_liveness_timeout_secs = Some(liveness);
        execution.deadline_duration_secs = Some(deadline);
        let error = execution.validate().unwrap_err().to_string();
        assert!(
            error.contains(&format!(
                "stream_liveness_timeout_secs ({liveness}) must be less than deadline_duration_secs ({deadline})"
            )),
            "liveness {liveness} deadline {deadline}: {error}"
        );
    }
}

#[test]
fn inference_execution_validate_accepts_liveness_shorter_than_deadline() {
    let mut execution = base_execution("ok-relationship");
    execution.stream_liveness_timeout_secs = Some(300);
    execution.deadline_duration_secs = Some(600);
    assert!(execution.validate().is_ok());
}

#[test]
fn inference_sampling_validate_rejects_negative_seed() {
    let mut sampling = base_sampling("seeded");
    sampling.seed = Some(-1);
    let error = sampling.validate().unwrap_err().to_string();
    assert!(error.contains("seed must be non-negative"), "{error}");
}

#[test]
fn inference_profile_accepts_unset_and_every_vocabulary_reasoning_effort() {
    // Effort is a typed enum on the canonical profile: unset (None variant)
    // and every vocabulary entry are valid; unknown strings are rejected at
    // deserialization by the enum itself.
    use crate::config::ReasoningEffort;
    let unset_forms: [Option<ReasoningEffort>; 1] = [None];
    for unset in unset_forms {
        let profile = InferenceProfile {
            agent_did: "owner".into(),
            profile_id: "unset-effort".into(),
            backend_id: "backend".into(),
            model_name: "model".into(),
            reasoning_effort: unset,
            ..Default::default()
        };
        assert!(profile.validate().is_ok(), "unset form {unset:?} failed");
    }
    for value in [
        ReasoningEffort::None,
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
        ReasoningEffort::Ultra,
    ] {
        let profile = InferenceProfile {
            agent_did: "owner".into(),
            profile_id: "vocab".into(),
            backend_id: "backend".into(),
            model_name: "model".into(),
            reasoning_effort: Some(value),
            ..Default::default()
        };
        assert!(profile.validate().is_ok(), "value {value:?} failed");
        // Round-trip through the typed serialization.
        let value_json = serde_json::to_value(value).unwrap();
        assert_eq!(
            serde_json::from_value::<ReasoningEffort>(value_json.clone()).unwrap(),
            value
        );
    }
    // Outside the vocabulary the parser is the rejection owner.
    assert!(ReasoningEffort::parse("extreme").is_err());
    let error = ReasoningEffort::parse("extreme").unwrap_err().to_string();
    assert!(error.contains("reasoning_effort must be one of"), "{error}");
    // And an unknown serialized string must not decode onto the profile.
    let decoded: Result<InferenceProfile, _> = serde_json::from_value(serde_json::json!({
        "agent_did": "owner", "profile_id": "bad-effort",
        "backend_id": "backend", "model_name": "model",
        "reasoning_effort": "extreme"
    }));
    assert!(decoded.is_err(), "unknown effort strings must not decode");
}

// ---------------------------------------------------------------------------
// Sampling bounds (#1331 fix round 1 — moved from the imperative
// `gents config profile set` writer, the only place that enforced them;
// #1430 moved the rules into InferenceSampling::validate).
// ---------------------------------------------------------------------------

#[test]
fn inference_sampling_validate_accepts_sampling_bounds_at_their_edges() {
    let mut sampling = base_sampling("edges");
    sampling.top_p = Some(0.0);
    sampling.min_p = Some(1.0);
    sampling.top_k = Some(1);
    sampling.repetition_penalty = Some(f64::MIN_POSITIVE);
    sampling.frequency_penalty = Some(-2.0);
    sampling.presence_penalty = Some(2.0);
    assert!(sampling.validate().is_ok());
}

#[test]
fn inference_sampling_validate_rejects_top_p_outside_unit_interval() {
    for value in [-0.01, 1.01] {
        let mut sampling = base_sampling("top-p");
        sampling.top_p = Some(value);
        let error = sampling.validate().unwrap_err().to_string();
        assert!(
            error.contains("top_p must be within [0, 1]"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_sampling_validate_rejects_min_p_outside_unit_interval() {
    for value in [-0.01, 1.01] {
        let mut sampling = base_sampling("min-p");
        sampling.min_p = Some(value);
        let error = sampling.validate().unwrap_err().to_string();
        assert!(
            error.contains("min_p must be within [0, 1]"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_sampling_validate_rejects_non_positive_top_k() {
    for value in [0, -1] {
        let mut sampling = base_sampling("top-k");
        sampling.top_k = Some(value);
        let error = sampling.validate().unwrap_err().to_string();
        assert!(error.contains("top_k must be positive"), "{value}: {error}");
    }
}

#[test]
fn inference_sampling_validate_rejects_non_positive_repetition_penalty() {
    for value in [0.0, -1.0] {
        let mut sampling = base_sampling("rep-penalty");
        sampling.repetition_penalty = Some(value);
        let error = sampling.validate().unwrap_err().to_string();
        assert!(
            error.contains("repetition_penalty must be positive"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_sampling_validate_rejects_frequency_and_presence_penalty_outside_range() {
    for value in [-2.01, 2.01] {
        let mut frequency = base_sampling("freq-penalty");
        frequency.frequency_penalty = Some(value);
        let error = frequency.validate().unwrap_err().to_string();
        assert!(
            error.contains("frequency_penalty must be within [-2, 2]"),
            "{value}: {error}"
        );

        let mut presence = base_sampling("presence-penalty");
        presence.presence_penalty = Some(value);
        let error = presence.validate().unwrap_err().to_string();
        assert!(
            error.contains("presence_penalty must be within [-2, 2]"),
            "{value}: {error}"
        );
    }
}

#[test]
fn inference_sampling_validate_reports_every_violation_at_once() {
    let mut sampling = base_sampling("multi-bad");
    sampling.seed = Some(-1);
    sampling.top_p = Some(2.0);
    sampling.top_k = Some(0);
    let error = sampling.validate().unwrap_err().to_string();
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

fn reference_documents() -> Vec<(crate::Collection, serde_json::Value)> {
    use crate::Collection;
    vec![
        (
            Collection::InferenceBackend,
            serde_json::json!({"agent_did":"owner","backend_id":"backend","name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}),
        ),
        (
            Collection::InferenceProfile,
            serde_json::json!({"agent_did":"owner","profile_id":"profile","backend_id":"backend","model_name":"model"}),
        ),
        (
            Collection::AgentContext,
            serde_json::json!({"agent_did":"owner","context_id":"context","tools_id":"tools","skill_ids":["skill"]}),
        ),
        (
            Collection::Tools,
            serde_json::json!({"agent_did":"owner","tools_id":"tools"}),
        ),
        (
            Collection::Skill,
            serde_json::json!({"agent_did":"owner","skill_id":"skill","name":"Skill","description":"Skill","instructions":"literal"}),
        ),
    ]
}

fn reference_behavior() -> AgentBehavior {
    serde_json::from_value(serde_json::json!({"agent_did":"owner","behavior_id":"behavior","context_id":"context","inference_profile_id":"profile"})).unwrap()
}

#[test]
fn behavior_and_context_share_canonical_reference_closure() {
    let refs = ConfigReferences::from_documents("owner", reference_documents()).unwrap();
    reference_behavior().validate_references(&refs).unwrap();
    // Absence invokes context defaults, not an implicit tool or skill set.
    let mut behavior = reference_behavior();
    behavior.context_id = None;
    behavior.validate_references(&refs).unwrap();
    behavior.inference_profile_id = "missing".into();
    assert!(
        behavior
            .validate_references(&refs)
            .unwrap_err()
            .to_string()
            .contains("InferenceProfile")
    );
}

#[test]
fn unchanged_nested_links_are_validated_without_a_behavior_write() {
    for missing in [
        crate::Collection::Tools,
        crate::Collection::Skill,
        crate::Collection::InferenceBackend,
    ] {
        let documents = reference_documents()
            .into_iter()
            .filter(|(collection, _)| *collection != missing);
        let refs = ConfigReferences::from_documents("owner", documents).unwrap();
        assert!(
            refs.validate()
                .unwrap_err()
                .to_string()
                .contains(missing.graphql_type())
        );
    }
}

#[test]
fn reference_snapshot_rejects_foreign_roots_duplicates_and_malformed_rows() {
    let mut documents = reference_documents();
    documents[0].1["agent_did"] = "foreign".into();
    assert!(ConfigReferences::from_documents("owner", documents).is_err());
    let mut documents = reference_documents();
    documents.push(documents[0].clone());
    assert!(
        ConfigReferences::from_documents("owner", documents)
            .unwrap_err()
            .to_string()
            .contains("multiple live")
    );
    // Genuinely malformed canonical backend row: the auth object with a
    // competing credential selection fails the backend validator.
    let mut documents = reference_documents();
    documents[0].1["auth"] = serde_json::json!({"kind":"api_key", "key":"key", "variable":"KEY"});
    assert!(ConfigReferences::from_documents("owner", documents).is_err());
}

#[test]
fn references_preserve_exact_ids_and_reject_present_blank_selections() {
    let refs = ConfigReferences::from_documents("owner", reference_documents()).unwrap();
    for selected in ["", " ", " context ", "missing"] {
        let mut behavior = reference_behavior();
        behavior.context_id = Some(selected.into());
        assert!(behavior.validate_references(&refs).is_err(), "{selected:?}");
    }
}

#[test]
fn same_owner_delegation_requires_a_behavior_but_foreign_admission_is_separate() {
    let target = |destination| {
        (
            crate::Collection::SubagentTarget,
            serde_json::json!({"agent_did":"owner","target_id":"worker","name":"worker","target_agent_did":destination,"behavior_id":"remote"}),
        )
    };
    // Same-owner targets must resolve the referenced behavior inside the same
    // closure; foreign destinations are checked by delegation admission under
    // ACP, never satisfied by a config lookup, so the foreign case validates.
    let refs = ConfigReferences::from_documents("owner", vec![target("owner")]).unwrap();
    assert!(refs.validate().is_err());
    let refs = ConfigReferences::from_documents("owner", vec![target("foreign")]).unwrap();
    refs.validate().unwrap();
}

#[tokio::test]
async fn reference_loader_isolates_foreign_malformed_backends_and_rejects_owned_corruption() {
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();
    // Use raw storage only to seed deliberately malformed rows. The common
    // desired-state writer correctly rejects these before publication.
    for (owner, id, auth) in [
        ("owner", "good", "unauthenticated"),
        ("foreign", "bad", "invalid-auth"),
    ] {
        let owner = crate::graphql::escape_graphql_string(owner);
        let id = crate::graphql::escape_graphql_string(id);
        let auth = crate::graphql::escape_graphql_string(auth);
        let result = node.execute(&format!(r#"mutation {{ create_InferenceBackend(input: {{agent_did: "{owner}", backend_id: "{id}", name: "fixture", provider_kind: "OpenAiCompatible", endpoint: "http://localhost:8000/v1", auth: {{kind: "{auth}"}}, enabled: true}}) {{_docID}} }}"#)).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
    }
    crate::config_client::ConfigAccess::transact_local(
        &node,
        None,
        "test.reference_owner_isolation",
        |txn| {
            Box::pin(async move {
                let refs = ConfigReferences::load_in_txn(txn, "owner").await?;
                refs.validate()?;
                assert!(refs.documents().any(|((collection, id), _)| *collection
                    == crate::Collection::InferenceBackend
                    && id == "good"));
                assert!(!refs.documents().any(|((_, id), _)| id == "bad"));
                Ok(())
            })
        },
    )
    .await
    .unwrap();
    let result = node.execute(r#"mutation { update_InferenceBackend(filter: {agent_did: {_eq: "owner"}, backend_id: {_eq: "good"}}, input: {auth: {kind: "invalid-auth"}}) {_docID} }"#).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    let result = crate::config_client::ConfigAccess::transact_local(
        &node,
        None,
        "test.reference_owned_corruption",
        |txn| {
            Box::pin(async move {
                ConfigReferences::load_in_txn(txn, "owner")
                    .await?
                    .validate()
            })
        },
    )
    .await;
    assert!(
        result.is_err(),
        "owned malformed configuration must fail closed"
    );
}
