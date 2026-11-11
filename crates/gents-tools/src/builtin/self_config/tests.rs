use super::command::{agents::agent_params, contracts::help_patch_contracts};
use super::*;
use gents_protocol::tool_names::{CONFIG_TOOL_NAME, SELF_CONFIG_TOOL_NAMES};


#[test]
fn every_tool_name_is_reserved_builtin() {
    for name in SELF_CONFIG_TOOL_NAMES {
        assert!(
            gents_documents::is_reserved_builtin_tool_name(name),
            "{name} must be reserved so write_tools declarations cannot shadow it"
        );
    }
}

#[test]
fn help_contracts_conform_to_canonical_types_and_enum_vocabulary() {
    let all_contracts = [
        "agent",
        "tools",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "datastore",
        "skill",
        "page",
        "plugin-source",
        "agent-target",
        "execution",
    ]
    .into_iter()
    .flat_map(|resource| {
        help_patch_contracts(Some(resource))
            .as_array()
            .unwrap()
            .clone()
    })
    .collect::<Vec<_>>();
    for target in gents_documents::patch::ALL_SELF_CONFIG_TARGETS {
        let contract = all_contracts
            .iter()
            .find(|contract| contract["collection"] == target.collection_name())
            .unwrap_or_else(|| panic!("missing help contract for {}", target.collection_name()));
        let described = contract["field_shapes"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let writable = target
            .writable_fields()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            described,
            writable,
            "{} help drift",
            target.collection_name()
        );
    }

    let tools = all_contracts
        .iter()
        .find(|contract| contract["collection"] == "Tools")
        .unwrap();
    let shapes = &tools["field_shapes"];
    let assert_fields = |shape: &Value, canonical: &[&str], name: &str| {
        let described = shape
            .as_object()
            .unwrap_or_else(|| panic!("{name} help shape must be an object"))
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let canonical = canonical.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(described, canonical, "{name} nested help drift");
    };
    use gents_documents::canonical_struct_fields;
    use gents_documents::*;
    assert_fields(
        &shapes["host"],
        canonical_struct_fields::<HostTools>().unwrap(),
        "host",
    );
    assert_fields(
        &shapes["host"]["files"],
        canonical_struct_fields::<FileTools>().unwrap(),
        "files",
    );
    assert_fields(
        &shapes["host"]["bash"],
        canonical_struct_fields::<BashTools>().unwrap(),
        "bash",
    );
    assert_fields(
        &shapes["host"]["cli"][0],
        canonical_struct_fields::<CliTool>().unwrap(),
        "cli[]",
    );
    assert_fields(
        &shapes["remote"],
        canonical_struct_fields::<RemoteTools>().unwrap(),
        "remote",
    );
    assert_fields(
        &shapes["agents"],
        canonical_struct_fields::<AgentTools>().unwrap(),
        "agents",
    );
    let built_in_fields = canonical_struct_fields::<BuiltInTools>()
        .unwrap()
        .iter()
        .copied()
        .filter(|field| !field.ends_with("_graph_tools"))
        .collect::<Vec<_>>();
    assert_fields(&shapes["built_ins"], &built_in_fields, "built_ins");
    assert_fields(
        &shapes["datastore"],
        canonical_struct_fields::<DatastoreTools>().unwrap(),
        "datastore",
    );
    // `eth_tool_ids` is accepted on read from v1.0.0-rc0 documents and
    // dropped; help must not offer it, so it is not part of the canonical set.
    let integration_fields = canonical_struct_fields::<IntegrationTools>()
        .unwrap()
        .iter()
        .copied()
        .filter(|field| *field != "eth_tool_ids")
        .collect::<Vec<_>>();
    assert_fields(&shapes["integrations"], &integration_fields, "integrations");
    assert_fields(
        &shapes["integrations"]["lsp"],
        canonical_struct_fields::<LspTools>().unwrap(),
        "lsp",
    );
    assert_fields(
        &shapes["self_config"],
        canonical_struct_fields::<SelfConfigTools>().unwrap(),
        "self_config",
    );

    let rendered = serde_json::to_string(&all_contracts).unwrap();
    let serialized_values = gents_inference::backends::provider::BackendProviderKind::ALL
        .into_iter()
        .map(|value| value.as_str())
        .chain(
            gents_documents::inference_defaults::ReasoningEffort::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            gents_loop::openai_wire::OpenAiWireApi::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            crate::native::CommandNetworkMode::ALL
                .into_iter()
                .map(|value| value.as_str()),
        );
    for value in serialized_values {
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
    for value in gents_documents::FileToolMode::ALL
        .into_iter()
        .map(|value| serde_json::to_value(value).unwrap())
        .chain(
            gents_documents::BashMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::native::CommandExecutionMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            gents_documents::RemoteToolStyle::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            gents_documents::ConcurrencyMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            gents_inference::compaction::CompactionStrategy::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
    {
        let value = value.as_str().unwrap();
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
}
