use super::*;
use serde_json::json;

#[test]
fn minimal_tools_round_trip_without_disabled_group_boilerplate() {
    let authored = json!({
        "tools_id": "coding", "node_did": "did:key:example",
        "host": {"root": ".", "files": {"mode": "ReadWrite"}}
    });
    let tools: Tools = serde_json::from_value(authored.clone()).unwrap();
    assert!(tools.remote.is_none());
    assert!(tools.host.as_ref().unwrap().bash.is_none());
    assert_eq!(serde_json::to_value(&tools).unwrap(), authored);
    assert_eq!(
        serde_json::from_value::<Tools>(serde_json::to_value(&tools).unwrap()).unwrap(),
        tools
    );
}

#[test]
fn missing_and_null_remote_defaults_grant_no_tools() {
    let tools: Tools = serde_json::from_value(json!({
        "tools_id": "research", "node_did": "did:key:example",
        "remote": {"services": [{"mcp_service_id": "web", "tool_names": null, "style": null}]}
    }))
    .unwrap();
    let service = &tools.remote.as_ref().unwrap().services[0];
    assert!(service.tool_names.is_empty());
    assert!(service.background_tool_names.is_empty());
    assert!(!service.required);
    assert_eq!(service.style, RemoteToolStyle::Discovery);
    assert_eq!(
        serde_json::to_value(tools).unwrap(),
        json!({
            "tools_id": "research", "node_did": "did:key:example",
            "remote": {"services": [{"mcp_service_id": "web"}]}
        })
    );
}

fn document(groups: serde_json::Value) -> Tools {
    let mut value = groups;
    value["tools_id"] = "tools".into();
    value["node_did"] = "owner".into();
    serde_json::from_value(value).unwrap()
}

#[test]
fn integrations_plugins_job_block_errors_name_the_field() {
    let tools = |job: serde_json::Value| {
        document(json!({"integrations":{"plugins":[{"plugin":"ns/runner","job":job}]}}))
    };
    assert!(tools(json!({"agent_targets":["a"]})).validate().is_ok());
    for (job, field) in [
        (json!({"agent_targets":[]}), "job.agent_targets"),
        (json!({"agent_targets":["a"],"max_live":0}), "job.max_live"),
        (
            json!({"agent_targets":["a"],"policy":{"trusted_packs":["not a pack"]}}),
            "job.policy.trusted_packs",
        ),
        (
            json!({"agent_targets":["a"],"policy":{"trusted_packs":["Bad-Pack"]}}),
            "job.policy.trusted_packs",
        ),
        (
            json!({"agent_targets":["a"],"policy":{"trusted_packs":["ns/pack@1.0.0"]}}),
            "job.policy.trusted_packs",
        ),
    ] {
        let message = tools(job).validate().unwrap_err().to_string();
        assert!(
            message.contains(&format!("integrations.plugins[ns/runner].{field}")),
            "{message}"
        );
    }
}

#[test]
fn a_0_21_job_policy_with_project_roots_still_loads_and_drops_them() {
    let tools = document(
        json!({"integrations":{"plugins":[{"plugin":"ns/runner","job":{
            "agent_targets":["a"],
            "policy":{"trusted_packs":["ns/pack"],"project_roots":["/work"]},
        }}]}}),
    );
    tools.validate().unwrap();
    let policy =
        &serde_json::to_value(&tools).unwrap()["integrations"]["plugins"][0]["job"]["policy"];
    assert_eq!(policy["trusted_packs"], json!(["ns/pack"]), "{policy}");
    assert!(policy.get("project_roots").is_none(), "{policy}");
}

#[test]
fn rc0_eth_tool_ids_are_read_and_dropped_but_other_unknown_keys_still_fail() {
    for legacy in [json!([]), json!(["eth-mainnet"]), json!(null)] {
        let stored = json!({
            "tools_id": "coding", "node_did": "did:key:example",
            "integrations": {"eth_tool_ids": legacy, "plugins": []}
        });
        let tools: Tools = serde_json::from_value(stored).unwrap();
        let integrations = tools.integrations.as_ref().unwrap();
        assert_eq!(integrations.plugins.as_deref(), Some(&[][..]));
        let written = serde_json::to_value(&tools).unwrap();
        assert!(
            written["integrations"].get("eth_tool_ids").is_none(),
            "{written}"
        );
    }
    let unknown = json!({
        "tools_id": "coding", "node_did": "did:key:example",
        "integrations": {"not_a_group": true}
    });
    assert!(serde_json::from_value::<Tools>(unknown).is_err());
}

#[test]
fn hidden_tool_names_defaults_roundtrip_and_exact_selection_validation() {
    use gents_store::collection::Collection;
    let base = json!({"tools_id":"tools", "node_did":"owner"});
    for value in [
        base.clone(),
        json!({"tools_id":"tools", "node_did":"owner", "hidden_tool_names":null}),
        json!({"tools_id":"tools", "node_did":"owner", "hidden_tool_names":[]}),
    ] {
        let doc: Tools = serde_json::from_value(value).unwrap();
        assert!(doc.hidden_tool_names.is_empty());
        assert_eq!(serde_json::to_value(doc).unwrap(), base);
    }
    let valid = json!({"tools_id":"tools", "node_did":"owner", "hidden_tool_names":["schema"],
        "built_ins":{"enable_schema_tool":true},
        "integrations":{"plugins":[{"plugin":"script/script", "tool_calls":true}]}});
    let validate = |value: serde_json::Value| {
        crate::ConfigReferences::from_documents("owner", [(Collection::Tools, value)])
            .unwrap()
            .validate()
    };
    validate(valid.clone()).unwrap();
    let doc: Tools = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(serde_json::to_value(&doc).unwrap(), valid);
    for names in [
        json!(["ungranted"]),
        json!(["schema*"]),
        json!(["schema", "schema"]),
        json!(["schema "]),
        json!(["script"]),
        json!(["config"]),
        json!(["spawn_process"]),
    ] {
        let mut invalid = valid.clone();
        invalid["hidden_tool_names"] = names;
        assert!(validate(invalid).is_err());
    }
    let mut no_script = valid.clone();
    no_script["integrations"] = serde_json::Value::Null;
    assert!(validate(no_script).is_err());
    let mut no_calls = valid.clone();
    no_calls["integrations"]["plugins"][0]["tool_calls"] = false.into();
    assert!(validate(no_calls).is_err());
    let mut no_grant = valid;
    no_grant["built_ins"] = serde_json::Value::Null;
    assert!(validate(no_grant).is_err());
}

#[test]
fn hidden_remote_names_use_selected_invocations_not_the_service_catalog() {
    let remote = RemoteTools {
        services: vec![RemoteServiceTools {
            mcp_service_id: "research".into(),
            tool_names: vec!["lookup".into()],
            style: RemoteToolStyle::Flat,
            ..Default::default()
        }],
    };
    let name = RemoteTools::flat_tool_name("research", "lookup");
    let mut tools = document(
        json!({"remote":remote, "integrations":{"plugins":[{"plugin":"script/script","tool_calls":true}]}, "hidden_tool_names":[name]}),
    );
    tools
        .validate_hidden_tools(&crate::MergedSurfaceTools::default())
        .unwrap();
    tools.hidden_tool_names = vec![RemoteTools::flat_tool_name("research", "unselected")];
    assert!(tools
        .validate_hidden_tools(&crate::MergedSurfaceTools::default())
        .is_err());
    tools.remote.as_mut().unwrap().services[0].style = RemoteToolStyle::Discovery;
    tools.hidden_tool_names = vec!["call_tool".into()];
    tools
        .validate_hidden_tools(&crate::MergedSurfaceTools::default())
        .unwrap();
    tools.hidden_tool_names = vec!["lookup".into()];
    assert!(tools
        .validate_hidden_tools(&crate::MergedSurfaceTools::default())
        .is_err());
}

#[test]
fn hidden_datastore_and_plugin_names_must_be_selected_not_just_available() {
    use gents_store::collection::Collection;
    let mut tools = document(json!({
        "datastore":{"datastore_tool_surface_ids":["selected"]},
        "integrations":{"plugins":[{"plugin":"script/script","tool_calls":true},{"plugin":"team/schema"}]},
        "hidden_tool_names":["read_selected", "schema"]
    }));
    let surface = |id: &str, name: &str| -> crate::DatastoreToolSurfaceDocument {
        serde_json::from_value(json!({"node_did":"owner","surface_id":id,
            "entries":[{"kind":"query","tool_name":name,"collection":"Records","fields":["name"],"description":"Reads rows."}]})).unwrap()
    };
    let selected = surface("selected", "read_selected");
    let available = surface("available", "read_available");
    let validate = |tools: &Tools| {
        crate::ConfigReferences::from_documents(
            "owner",
            [
                (Collection::Tools, serde_json::to_value(tools).unwrap()),
                (
                    Collection::DatastoreToolSurface,
                    serde_json::to_value(&selected).unwrap(),
                ),
                (
                    Collection::DatastoreToolSurface,
                    serde_json::to_value(&available).unwrap(),
                ),
            ],
        )
        .unwrap()
        .validate()
    };
    validate(&tools).unwrap();
    tools.hidden_tool_names = vec!["read_available".into()];
    assert!(validate(&tools).is_err());
}
