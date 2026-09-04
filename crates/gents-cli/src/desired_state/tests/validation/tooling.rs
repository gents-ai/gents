use super::super::*;

#[test]
fn validate_rejects_empty_string_in_subagent_targets() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_targets = vec!["".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    // `ToolSelectionDocument::validate` (#1331, the single owner) reports
    // this now; its message doesn't repeat the selection_id (a document
    // validator sees one document, not the manifest it came from) — the
    // manifest-side `validate_subagent_targets` no longer re-checks entry
    // parseability, only duplicate names and cross-deployment permission.
    assert!(
        errors
            .iter()
            .any(|msg| msg.contains("subagent_targets[0] is empty")),
        "expected empty subagent_targets entry rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_subagent_spawn_enabled_without_targets() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_targets = Vec::new();
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| {
            msg.contains("agent-tools")
                && msg.contains("subagent_spawn_enabled")
                && msg.contains("subagent_targets")
        }),
        "expected subagent_spawn_enabled-without-targets rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_subagent_spawn_enabled_with_empty_targets_vec() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_targets = Vec::new();
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| {
            msg.contains("agent-tools")
                && msg.contains("subagent_spawn_enabled")
                && msg.contains("subagent_targets")
        }),
        "expected subagent_spawn_enabled-with-empty-targets-vec rejection, got {errors:?}"
    );
}

#[test]
fn validate_accepts_subagent_spawn_enabled_with_targets() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_targets = vec![gents::subagent_target_entry(
        "researcher",
        "did:test:test",
        "amy-research",
        None,
    )];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|msg| msg.contains("subagent_targets") || msg.contains("subagent_spawn_enabled")),
        "expected no subagent rejections for valid config, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_subagent_target_name() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_targets = vec![
        gents::subagent_target_entry("dup", "did:test:test", "amy-research", None),
        gents::subagent_target_entry("dup", "did:test:test", "amy-code", None),
    ];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|msg| msg.contains("duplicate subagent target name") && msg.contains("dup")),
        "expected duplicate-name rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_remote_did_target_when_cross_deployment_off() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_allow_cross_deployment = false;
    sel.subagent_targets = vec![gents::subagent_target_entry(
        "remote-researcher",
        "did:test:OTHER-deployment",
        "amy-research",
        None,
    )];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| {
            msg.contains("cross-deployment subagent delegation is deferred")
                && msg.contains("remote-researcher")
                && msg.contains("subagent_allow_cross_deployment=true")
        }),
        "expected remote-DID rejection when flag is off, got {errors:?}"
    );
}

#[test]
fn validate_accepts_remote_did_target_when_cross_deployment_on() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_allow_cross_deployment = true;
    sel.subagent_targets = vec![gents::subagent_target_entry(
        "remote-researcher",
        "did:test:OTHER-deployment",
        "amy-research",
        None,
    )];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|msg| msg.contains("cross-deployment subagent delegation is deferred")),
        "expected no cross-deployment rejection when flag is on, got {errors:?}"
    );
}

#[test]
fn validate_accepts_local_did_target_when_cross_deployment_off() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.subagent_spawn_enabled = true;
    sel.subagent_allow_cross_deployment = false;
    sel.subagent_targets = vec![gents::subagent_target_entry(
        "local-researcher",
        "did:test:test",
        "amy-research",
        None,
    )];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        !errors
            .iter()
            .any(|msg| msg.contains("cross-deployment subagent delegation is deferred")),
        "expected no cross-deployment rejection for local target, got {errors:?}"
    );
}

#[test]
fn write_tools_deserializer_accepts_only_canonical_objects() {
    fn selection(write_tools: serde_json::Value) -> serde_json::Value {
        json!({
            "selection_id": "conv-sel",
            "agent_did": "did:test:test",
            "tool_policy_version": gents::tool_surface::TOOL_POLICY_V1,
            "display_name": null,
            "enable_file_tools": false,
            "file_tools_mode": "ReadOnly",
            "file_tool_root": null,
            "enable_bash": false,
            "bash_mode": "ReadOnly",
            "enable_meta_tools": false,
            "write_tools": write_tools,
        })
    }

    fn deser_write_tools(write_tools: serde_json::Value) -> Vec<String> {
        let parsed: DesiredToolSelection =
            serde_json::from_value(selection(write_tools)).expect("canonical selection");
        parsed.write_tools
    }

    let object_list = deser_write_tools(json!([
        {
            "tool_name": "request_action",
            "collection": "ActionRequest",
            "fields": [{ "name": "drift_sig", "required": true }]
        }
    ]));

    assert_eq!(object_list.len(), 1);
    assert!(
        serde_json::from_value::<DesiredToolSelection>(selection(json!([
            "{\"collection\":\"ActionRequest\",\"fields\":[{\"required\":true,\"name\":\"drift_sig\"}],\"tool_name\":\"request_action\"}"
        ])))
        .is_err(),
        "DefraDB's encoded storage shape is not a valid manifest input"
    );
}

fn write_tool_storage_entry(decl: &gents::WriteToolDecl) -> String {
    serde_json::to_string(decl).expect("WriteToolDecl serializes to JSON")
}

#[test]
fn validate_rejects_write_tool_with_invalid_collection_identifier() {
    use gents::WriteToolDecl;
    for collection in ["", "ActionRequest) { _docID } mutation {"] {
        let mut manifest = manifest_with_default_behavior();
        let mut sel = sample_tool_selection("agent-tools");
        sel.agent_did = "did:test:test".to_string();
        sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
            tool_name: "request_action".to_string(),
            collection: collection.to_string(),
            description: String::new(),
            fields: Vec::new(),
            output_obligation: None,
        })];
        manifest.tool_selections.push(sel);

        let errors = validation_errors(&manifest);
        assert!(
            errors.iter().any(|msg| msg.contains("write_tools")
                && msg.contains("agent-tools")
                && msg.contains("collection")),
            "expected invalid-collection write_tools rejection, got {errors:?}"
        );
    }
}

#[test]
fn validate_rejects_write_tool_with_empty_field_name() {
    use gents::{WriteToolDecl, WriteToolField};
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: "ActionRequest".to_string(),
        description: String::new(),
        output_obligation: None,
        fields: vec![WriteToolField {
            name: "   ".to_string(),
            required: true,
            fill: None,
        }],
    })];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| msg.contains("write_tools")
            && msg.contains("agent-tools")
            && msg.contains("empty name")),
        "expected empty-field-name write_tools rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_write_tool_name() {
    use gents::WriteToolDecl;
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    let decl = WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: "ActionRequest".to_string(),
        description: String::new(),
        output_obligation: None,
        fields: Vec::new(),
    };
    sel.write_tools = vec![
        write_tool_storage_entry(&decl),
        write_tool_storage_entry(&WriteToolDecl {
            collection: "OtherCollection".to_string(),
            ..decl
        }),
    ];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| {
            msg.contains("duplicate write_tools tool_name") && msg.contains("request_action")
        }),
        "expected duplicate write_tools tool_name rejection, got {errors:?}"
    );
}

#[test]
fn validate_accepts_well_formed_write_tools() {
    use gents::{WriteToolDecl, WriteToolField};
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: "ActionRequest".to_string(),
        description: "Request a bounded action".to_string(),
        output_obligation: None,
        fields: vec![
            WriteToolField {
                name: "title".to_string(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "detail".to_string(),
                required: false,
                fill: None,
            },
        ],
    })];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        !errors.iter().any(|msg| msg.contains("write_tools")),
        "expected no write_tools rejections for a well-formed decl, got {errors:?}"
    );
}

#[test]
fn validate_rejects_zero_write_tool_output_obligation() {
    use gents::document_config::{WriteToolOutputObligation, WriteToolOutputObligationScope};
    use gents::WriteToolDecl;
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: "ActionRequest".to_string(),
        description: String::new(),
        fields: Vec::new(),
        output_obligation: Some(WriteToolOutputObligation {
            scope: WriteToolOutputObligationScope::Trigger,
            minimum_writes: 0,
            expected_count_field: None,
        }),
    })];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("output_obligation.minimum_writes")),
        "{errors:?}"
    );
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_builtin() {
    use gents::{WriteToolDecl, WriteToolField};
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
        tool_name: "read_file".to_string(),
        collection: "AuditLog".to_string(),
        description: String::new(),
        output_obligation: None,
        fields: vec![WriteToolField {
            name: "path".to_string(),
            required: true,
            fill: None,
        }],
    })];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| msg.contains("write_tools")
            && msg.contains("read_file")
            && msg.contains("built-in")),
        "expected built-in collision rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_write_tool_name_colliding_with_cli_tool() {
    use gents::WriteToolDecl;
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.cli_tool_names = vec!["rg".to_string()];
    sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
        tool_name: "rg".to_string(),
        collection: "AuditLog".to_string(),
        description: String::new(),
        output_obligation: None,
        fields: Vec::new(),
    })];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| msg.contains("write_tools")
            && msg.contains("rg")
            && msg.contains("cli_tool_names")),
        "expected cli_tool_names collision rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_duplicate_write_tool_field_name() {
    use gents::{WriteToolDecl, WriteToolField};
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("agent-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.write_tools = vec![write_tool_storage_entry(&WriteToolDecl {
        tool_name: "request_action".to_string(),
        collection: "ActionRequest".to_string(),
        description: String::new(),
        output_obligation: None,
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
    })];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| msg.contains("write_tools")
            && msg.contains("request_action")
            && msg.contains("duplicate field name")),
        "expected duplicate field-name rejection, got {errors:?}"
    );
}

fn sample_surface(surface_id: &str) -> DesiredDatastoreToolSurface {
    use gents::WriteToolDecl;
    DesiredDatastoreToolSurface {
        surface_id: surface_id.to_string(),
        agent_did: "did:test:test".to_string(),
        display_name: Some("experiment writes".to_string()),
        enabled: true,
        entries: vec![write_tool_storage_entry(&WriteToolDecl {
            tool_name: "write_experiment_finding".to_string(),
            collection: "ExperimentFinding".to_string(),
            description: "Record a finding".to_string(),
            output_obligation: None,
            fields: Vec::new(),
        })],
    }
}

#[test]
fn validate_accepts_surface_query_entry() {
    use gents::{QueryToolDecl, SurfaceToolDecl, WriteToolField, WriteToolFieldFill};
    let mut manifest = manifest_with_default_behavior();
    let mut surface = sample_surface("scan-reads");
    surface.entries.push(
        serde_json::to_string(&SurfaceToolDecl::Query(QueryToolDecl {
            tool_name: "query_candidate_finding".to_string(),
            collection: "CandidateFinding".to_string(),
            description: "Load candidates".to_string(),
            fields: vec!["finding_id".to_string(), "title".to_string()],
            filter_fields: vec![WriteToolField {
                name: "run_id".to_string(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            }],
        }))
        .unwrap(),
    );
    manifest.datastore_tool_surfaces.push(surface);
    let mut sel = sample_tool_selection("stage-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.datastore_tool_surface_ids = vec!["scan-reads".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.is_empty(),
        "query surface entries should validate, got {errors:?}"
    );
}

#[test]
fn validate_rejects_query_tool_name_colliding_with_cli_tool() {
    use gents::{QueryToolDecl, SurfaceToolDecl};
    let mut manifest = manifest_with_default_behavior();
    let mut surface = sample_surface("scan-reads");
    surface.entries.push(
        serde_json::to_string(&SurfaceToolDecl::Query(QueryToolDecl {
            tool_name: "gh".to_string(),
            collection: "CandidateFinding".to_string(),
            description: "Load candidates".to_string(),
            fields: vec!["finding_id".to_string()],
            filter_fields: Vec::new(),
        }))
        .unwrap(),
    );
    manifest.datastore_tool_surfaces.push(surface);
    let mut sel = sample_tool_selection("stage-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.datastore_tool_surface_ids = vec!["scan-reads".to_string()];
    sel.cli_tool_names = vec!["gh".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|error| error.contains("cli_tool_names")),
        "query/cli name collision should fail apply validation, got {errors:?}"
    );
}

#[test]
fn validate_accepts_surface_linked_tool_selection() {
    let mut manifest = manifest_with_default_behavior();
    manifest
        .datastore_tool_surfaces
        .push(sample_surface("experiment-writes"));
    let mut sel = sample_tool_selection("stage-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.datastore_tool_surface_ids = vec!["experiment-writes".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.is_empty(),
        "surface-linked selection should validate cleanly, got {errors:?}"
    );
}

#[test]
fn validate_rejects_missing_surface_link() {
    let mut manifest = manifest_with_default_behavior();
    let mut sel = sample_tool_selection("stage-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.datastore_tool_surface_ids = vec!["does-not-exist".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|msg| msg.contains("missing DatastoreToolSurface")
                && msg.contains("does-not-exist")),
        "expected missing surface rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_disabled_surface_link() {
    let mut manifest = manifest_with_default_behavior();
    let mut surface = sample_surface("experiment-writes");
    surface.enabled = false;
    manifest.datastore_tool_surfaces.push(surface);
    let mut sel = sample_tool_selection("stage-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.datastore_tool_surface_ids = vec!["experiment-writes".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors.iter().any(|msg| msg.contains("disabled")),
        "expected disabled surface rejection, got {errors:?}"
    );
}

#[test]
fn validate_rejects_foreign_agent_surface_link() {
    let mut manifest = manifest_with_default_behavior();
    let mut surface = sample_surface("experiment-writes");
    surface.agent_did = "did:key:zOther".to_string();
    manifest.datastore_tool_surfaces.push(surface);
    let mut sel = sample_tool_selection("stage-tools");
    sel.agent_did = "did:test:test".to_string();
    sel.datastore_tool_surface_ids = vec!["experiment-writes".to_string()];
    manifest.tool_selections.push(sel);

    let errors = validation_errors(&manifest);
    assert!(
        errors
            .iter()
            .any(|msg| msg.contains("different agent") || msg.contains("does not match principal")),
        "expected foreign surface rejection, got {errors:?}"
    );
}
