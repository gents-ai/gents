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
        document(
            json!({"agents":{"target_ids":["a"]},"integrations":{"plugins":[{"plugin":"ns/runner","job":job}]}}),
        )
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
        json!({"agents":{"target_ids":["a"]},"integrations":{"plugins":[{"plugin":"ns/runner","job":{
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

fn hiding(hidden: &[&str], script: bool) -> Tools {
    let mut tools = document(if script {
        json!({"integrations": {"plugins": [{"plugin": "gents/script", "tool_calls": true}]}})
    } else {
        json!({})
    });
    tools.hidden_tool_names = hidden.iter().map(|name| name.to_string()).collect();
    tools
}

#[test]
fn hiding_configuration_conforms_to_lean() {
    gents_lean_vocab::families::tool_presentation_configuration_valid::replay(|case| {
        let mut hidden = Vec::new();
        if case.hides_others {
            hidden.push("schema");
        }
        if case.hides_gateway {
            hidden.push("config");
        }
        hiding(&hidden, case.script_selected)
            .validation_violations()
            .is_empty()
    });
}

fn wired(tool: &str, selected: bool, script: bool) -> Tools {
    let mut plugins = Vec::new();
    if selected {
        plugins.push(json!({"plugin": format!("gents/{tool}")}));
    }
    if script {
        plugins.push(json!({"plugin": "gents/script", "tool_calls": true}));
    }
    document(json!({"integrations": {"plugins": plugins}}))
}

#[test]
fn wiring_lists_hidden_tools_as_lean_says() {
    gents_lean_vocab::families::tool_presentation_listed_after_wiring::replay(|case| {
        let tool = if case.default_hidden {
            "charts"
        } else {
            "query"
        };
        let before = wired(tool, case.selected_before, case.script_before);
        let mut after = wired(tool, case.selected_after, case.script_after);
        if case.listed {
            after.hidden_tool_names = vec![tool.to_owned()];
        }
        after.list_hidden_after_wiring(&before);
        after.hidden_tool_names.iter().any(|name| name == tool)
    });
}

#[test]
fn hidden_names_are_exact_and_round_trip_but_bind_only_writes() {
    let tools = hiding(&["schema"], true);
    let value = serde_json::to_value(&tools).unwrap();
    assert_eq!(value["hidden_tool_names"], json!(["schema"]));
    assert_eq!(serde_json::from_value::<Tools>(value).unwrap(), tools);
    let mut absent = serde_json::to_value(hiding(&[], true)).unwrap();
    assert!(absent.get("hidden_tool_names").is_none());
    absent["hidden_tool_names"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<Tools>(absent)
        .unwrap()
        .hidden_tool_names
        .is_empty());
    for bad in [" schema", "sch*", ""] {
        let violations = hiding(&[bad], true).stored_violations();
        assert!(
            violations
                .iter()
                .any(|v| v.starts_with("hidden_tool_names")),
            "{bad:?}: {violations:?}"
        );
    }
    let unreachable = hiding(&["schema"], false);
    assert!(unreachable.validate_stored().is_ok());
    let refusal = unreachable.validate().unwrap_err().to_string();
    assert!(
        refusal.contains("select script with tool_calls: true"),
        "{refusal}"
    );
}

#[test]
fn a_job_target_the_caller_cannot_start_is_refused_on_write_only() {
    let tools = |agents: serde_json::Value| {
        let mut value = json!({"integrations": {"plugins": [
            {"plugin": "ns/runner", "job": {"agent_targets": ["a", "b"]}}
        ]}});
        if !agents.is_null() {
            value["agents"] = agents;
        }
        document(value)
    };
    let refused = tools(json!({"target_ids": ["a"]}));
    let message = refused.validate().unwrap_err().to_string();
    assert!(
        message.contains("integrations.plugins[ns/runner].job.agent_targets names [\"b\"]"),
        "{message}"
    );
    assert!(refused.validate_stored().is_ok());
    assert!(tools(serde_json::Value::Null).validate().is_err());
    assert!(tools(json!({"target_ids": ["a", "b"]})).validate().is_ok());
    assert!(tools(json!({"target_ids": ["a", "b"], "enabled": false}))
        .validate()
        .is_ok());
}
