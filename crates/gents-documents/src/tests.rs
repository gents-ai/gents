use super::*;

#[test]
fn tools_document_accepts_empty_string_arrays_and_null_groups() {
    let document: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "default-tools",
        "node_did": "did:test:test",
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

    assert!(document.host.as_ref().unwrap().cli.is_empty());
    assert!(document.remote.as_ref().unwrap().services.is_empty());
    assert!(document.built_ins.is_none());
    assert_eq!(
        document.host.as_ref().unwrap().files.as_ref().unwrap().mode,
        crate::FileToolMode::ReadOnly
    );
}

#[test]
fn required_mcp_service_selection_is_explicit_and_unique() {
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "required-mcp",
        "node_did": "did:key:test",
        "remote": {"services": [{"mcp_service_id": "research", "tool_names": ["search"], "required": true}]},
    }))
    .unwrap();
    tools.validate().unwrap();
    let tools: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "required-mcp",
        "node_did": "did:key:test",
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
fn validate_rejects_blank_agent_target_ids() {
    let doc = Tools {
        tools_id: "test-tools".to_string(),
        node_did: "did:test:test".to_string(),
        agents: Some(AgentTools {
            target_ids: vec!["".to_string()],
            enabled: Some(true),
        }),
        ..Default::default()
    };
    let result = doc.validate();
    assert!(result.is_err());
    assert!(
        format!("{}", result.unwrap_err()).contains("agents.target_ids"),
        "error message must mention agents.target_ids"
    );
}

fn validate_write_tools(decls: &[WriteToolDecl]) -> anyhow::Result<()> {
    super::write_tool::validate_write_tool_declarations(decls, &[], &[])
}

#[test]
fn validate_rejects_write_tool_with_empty_tool_name() {
    let decls = vec![WriteToolDecl {
        notification: None,
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
            notification: None,
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
            notification: None,
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
        notification: None,
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
        notification: None,
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
            notification: None,
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
            notification: None,
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
        notification: None,
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
            notification: None,
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
        assert!(validate_write_tools(&decls)
            .unwrap_err()
            .to_string()
            .contains("expected_count_field"));
    }
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_builtin() {
    let decls = vec![WriteToolDecl {
        notification: None,
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
        notification: None,
        tool_name: "query".to_string(),
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
    let decls = vec![WriteToolDecl {
        notification: None,
        tool_name: "rg".to_string(),
        collection: "AuditLog".to_string(),
        description: String::new(),
        fields: Vec::new(),
        output_obligation: None,
    }];
    let cli_tool_names = vec!["rg".to_string()];
    let result = super::write_tool::validate_write_tool_declarations(&decls, &cli_tool_names, &[]);
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
        notification: None,
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
fn write_tools_deserialize_trims_whitespace() {
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

#[test]
fn stored_built_ins_with_retired_key_decode() {
    let stored: BuiltInTools = serde_json::from_value(
        serde_json::json!({"enable_graph_tools": true, "enable_goal_tools": true}),
    )
    .expect("a stored built_ins with the retired key decodes");
    let written = serde_json::to_value(&stored).unwrap();
    assert_eq!(written, serde_json::json!({"enable_goal_tools": true}));
    let without: BuiltInTools =
        serde_json::from_value(serde_json::json!({"enable_goal_tools": true})).unwrap();
    assert_eq!(stored, without);
}

#[test]
fn built_ins_reject_the_removed_memory_setting() {
    assert!(serde_json::from_value::<BuiltInTools>(serde_json::json!({
        "enable_memory": true
    }))
    .is_err());
}

#[test]
fn validate_accepts_well_formed_agent_target_documents() {
    let code_entry = AgentTargetDocument {
        target_id: "amy-code".to_string(),
        node_did: "did:key:zParent".to_string(),
        target_node_did: "did:key:zParent".to_string(),
        agent_did: "amy-code".to_string(),
        name: "Code assistant".to_string(),
        description: None,
        tags: Vec::new(),
    };
    let research_entry = AgentTargetDocument {
        target_id: "amy-research".to_string(),
        node_did: "did:key:zParent".to_string(),
        target_node_did: "did:key:zDestination".to_string(),
        agent_did: "amy-research".to_string(),
        name: "amy-research".to_string(),
        description: None,
        tags: Vec::new(),
    };
    for entry in [&code_entry, &research_entry] {
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(
            serde_json::from_value::<AgentTargetDocument>(value).unwrap(),
            *entry
        );
    }
    let doc = Tools {
        tools_id: "test-tools".to_string(),
        node_did: "did:test:test".to_string(),
        agents: Some(AgentTools {
            target_ids: vec!["amy-code".to_string(), "amy-research".to_string()],
            enabled: Some(true),
        }),
        ..Default::default()
    };
    assert!(
        doc.validate().is_ok(),
        "well-formed agent target references must be accepted"
    );
}

#[test]
fn tools_validation_reports_every_violation() {
    let doc: Tools = serde_json::from_value(serde_json::json!({
        "tools_id": "invalid-tools",
        "node_did": "did:test:test",
        "host": {"bash": {"mode": "ReadOnly", "max_output_chars": 0}},
        "agents": {"target_ids": [""]}
    }))
    .unwrap();

    let violations = doc.validation_violations();
    assert_eq!(violations.len(), 2, "{violations:?}");
    assert!(violations
        .iter()
        .any(|error| error.contains("agents.target_ids")));
    assert!(violations
        .iter()
        .any(|error| error.contains("host.bash.max_output_chars")));
}

#[test]
fn validate_rejects_undeclared_agent_target_reference() {
    let refs = ConfigReferences::from_documents(
        "did:test:test",
        vec![(
            gents_store::collection::Collection::Tools,
            serde_json::json!({
                "tools_id": "test-tools",
                "node_did": "did:test:test",
                "agents": {"target_ids": ["amy-code"], "enabled": true}
            }),
        )],
    )
    .unwrap();
    let result = refs.validate();
    assert!(
        result.is_err(),
        "undeclared agent target reference must be rejected"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("agents.target_ids"),
        "error must mention agents.target_ids; got: {err_msg}"
    );
    assert!(
        err_msg.contains("AgentTarget"),
        "error must mention the AgentTarget collection; got: {err_msg}"
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
        notification: None,
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

#[test]
fn source_filled_write_field_is_optional_in_an_individual_surface_entry() {
    let mut decl: WriteToolDecl = serde_json::from_value(serde_json::json!({
        "tool_name": "write_result",
        "collection": "Result",
        "fields": [{"name": "handoff_id", "fill": {"source_field": "handoff_id"}}]
    }))
    .unwrap();
    let entry = super::SurfaceToolDecl::Create(decl.clone());
    assert!(entry.validate().is_ok());

    decl.fields[0].required = true;
    let error = super::SurfaceToolDecl::Create(decl).validate().unwrap_err();
    assert!(error
        .to_string()
        .contains("runtime-filled and cannot be required"));
}

fn reference_documents() -> Vec<(gents_store::collection::Collection, serde_json::Value)> {
    use gents_store::collection::Collection;
    vec![
        (
            Collection::InferenceBackend,
            serde_json::json!({"node_did":"owner","backend_id":"backend","name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}),
        ),
        (
            Collection::InferenceProfile,
            serde_json::json!({"node_did":"owner","profile_id":"profile","backend_id":"backend","model_name":"model"}),
        ),
        (
            Collection::AgentContext,
            serde_json::json!({"node_did":"owner","context_id":"context","tools_id":"tools","skill_ids":["skill"]}),
        ),
        (
            Collection::Tools,
            serde_json::json!({"node_did":"owner","tools_id":"tools"}),
        ),
        (
            Collection::Skill,
            serde_json::json!({"node_did":"owner","skill_id":"skill","name":"Skill","description":"Skill","instructions":"literal"}),
        ),
    ]
}

fn reference_agent() -> Agent {
    use gents_store::identity::NodeIdentity;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("node.key"), None)
            .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&identity).unwrap();
    serde_json::from_value(serde_json::json!({"node_did":identity.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"context_id":"context","inference_profile_id":"profile"})).unwrap()
}

#[test]
fn agent_and_context_share_canonical_reference_closure() {
    let mut agent = reference_agent();
    let mut documents = reference_documents();
    for (_, document) in &mut documents {
        document["node_did"] = agent.node_did.clone().into();
    }
    let refs = ConfigReferences::from_documents(&agent.node_did, documents).unwrap();
    agent.validate_references(&refs).unwrap();
    // Absence invokes context defaults, not an implicit tool or skill set.
    agent.context_id = None;
    agent.validate_references(&refs).unwrap();
    agent.inference_profile_id = "missing".into();
    assert!(agent
        .validate_references(&refs)
        .unwrap_err()
        .to_string()
        .contains("InferenceProfile"));
}

#[test]
fn default_agent_publication_matches_lean() {
    use gents_lean_vocab::families::configuration_scope_default_agent_publication as family;
    use gents_store::collection::Collection;
    use gents_store::identity::NodeIdentity;
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let alice =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("alice"), None)
            .unwrap();
    let bob =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("bob"), None)
            .unwrap();
    let missing = gents_store::identity::provision_agent_identity(&alice).unwrap();
    let principal = |label: &str| match label {
        "alice" => &alice,
        "bob" => &bob,
        _ => panic!("unknown modeled principal {label}"),
    };
    family::replay(|i| {
        let scope = principal(i.scope.as_str()).did();
        let agent_owner = principal(i.agent_owner.as_str());
        let binding = gents_store::identity::provision_agent_identity(agent_owner).unwrap();
        let default = i.default_agent_did.as_deref().map(|id| {
            if id == i.agent_did {
                binding.agent_did.as_str()
            } else {
                missing.agent_did.as_str()
            }
        });
        let documents = vec![
            (
                Collection::InferenceBackend,
                json!({"node_did":scope,"backend_id":"backend","name":"Local","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}),
            ),
            (
                Collection::InferenceProfile,
                json!({"node_did":scope,"profile_id":"profile","backend_id":"backend","model_name":"model"}),
            ),
            (
                Collection::AgentContext,
                json!({"node_did":scope,"context_id":"context"}),
            ),
            (
                Collection::Agent,
                json!({"node_did":agent_owner.did(),"agent_did":binding.agent_did,
                    "creation_nonce":binding.creation_nonce,
                    "context_id":i.context_id,"inference_profile_id":"profile",
                    "enabled":i.enabled}),
            ),
            (Collection::Node, json!({"node_did":scope})),
            (
                Collection::DefaultAgentSelection,
                json!({"node_did":scope,"default_agent_did":default}),
            ),
        ];
        ConfigReferences::from_documents(scope, documents)
            .and_then(|refs| refs.validate())
            .is_ok()
    });
}

#[test]
fn unchanged_nested_links_are_validated_without_an_agent_write() {
    for missing in [
        gents_store::collection::Collection::Tools,
        gents_store::collection::Collection::Skill,
        gents_store::collection::Collection::InferenceBackend,
    ] {
        let documents = reference_documents()
            .into_iter()
            .filter(|(collection, _)| *collection != missing);
        let refs = ConfigReferences::from_documents("owner", documents).unwrap();
        assert!(refs
            .validate()
            .unwrap_err()
            .to_string()
            .contains(missing.graphql_type()));
    }
}

#[test]
fn reference_snapshot_rejects_foreign_roots_duplicates_and_malformed_rows() {
    let mut documents = reference_documents();
    documents[0].1["node_did"] = "foreign".into();
    assert!(ConfigReferences::from_documents("owner", documents).is_err());
    let mut documents = reference_documents();
    documents.push(documents[0].clone());
    assert!(ConfigReferences::from_documents("owner", documents)
        .unwrap_err()
        .to_string()
        .contains("multiple live"));
    // Genuinely malformed canonical backend row fails the backend validator.
    let mut documents = reference_documents();
    documents[0].1["auth"] = serde_json::json!({"kind":"api_key", "key":"key", "variable":"KEY"});
    assert!(ConfigReferences::from_documents("owner", documents).is_err());
}

#[test]
fn references_preserve_exact_ids_and_reject_present_blank_selections() {
    let mut agent = reference_agent();
    let mut documents = reference_documents();
    for (_, document) in &mut documents {
        document["node_did"] = agent.node_did.clone().into();
    }
    let refs = ConfigReferences::from_documents(&agent.node_did, documents).unwrap();
    for selected in ["", " ", " context ", "missing"] {
        agent.context_id = Some(selected.into());
        assert!(agent.validate_references(&refs).is_err(), "{selected:?}");
    }
}

#[test]
fn same_owner_delegation_requires_an_agent_but_foreign_admission_is_separate() {
    let target = |destination| {
        (
            gents_store::collection::Collection::AgentTarget,
            serde_json::json!({"node_did":"owner","target_id":"worker","name":"worker","target_node_did":destination,"agent_did":"remote"}),
        )
    };
    let refs = ConfigReferences::from_documents("owner", vec![target("owner")]).unwrap();
    assert!(refs.validate().is_err());
    let refs = ConfigReferences::from_documents("owner", vec![target("foreign")]).unwrap();
    refs.validate().unwrap();
}

#[test]
fn cached_datastore_tool_matches_modeled_selection_and_visibility() {
    let model = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    assert_eq!(model.configuration_datastore_tool_use_cases.len(), 5);
    let expected = SurfaceToolDecl::Query(QueryToolDecl {
        tool_name: "find_entries".into(),
        collection: "Entry".into(),
        description: "Find entries".into(),
        fields: vec!["name".into()],
        filter_fields: vec![],
    });
    for case in &model.configuration_datastore_tool_use_cases {
        let selected = case["selected"].as_bool().unwrap();
        let tools = Tools {
            tools_id: "tools".into(),
            node_did: "node".into(),
            datastore: Some(DatastoreTools {
                datastore_tool_surface_ids: Some(if selected {
                    vec!["surface".into()]
                } else {
                    vec![]
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut entry = expected.clone();
        if !case["declaration_matches"].as_bool().unwrap() {
            if let SurfaceToolDecl::Query(query) = &mut entry {
                query.fields = vec!["other".into()];
            }
        }
        let surface: DatastoreToolSurfaceDocument = serde_json::from_value(serde_json::json!({
            "surface_id": "surface", "node_did": "node", "enabled": case["enabled"],
            "entries": [entry]
        }))
        .unwrap();
        let visible = if case["visible"].as_bool().unwrap() {
            vec![surface]
        } else {
            vec![]
        };
        let actual = super::surface_tool::confirm_cached_datastore_tool(
            &tools, &visible, "surface", &expected,
        )
        .is_ok();
        assert_eq!(actual, case["expected"].as_bool().unwrap(), "{case}");
    }
}
