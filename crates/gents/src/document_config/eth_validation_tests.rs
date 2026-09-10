use crate::document_config::ConfigReferences;
use crate::Collection;
use serde_json::{json, Value};

fn validate(documents: Vec<(Collection, Value)>) -> anyhow::Result<()> {
    ConfigReferences::from_documents("owner", documents)?.validate()
}

#[test]
fn retained_eth_selection_validates_declarations_without_disabling_the_enable_gate() {
    let tools = json!({"tools_id":"tools","agent_did":"owner",
        "integrations":{"eth_tool_ids":["eth"]}});
    let eth = json!({"tool_id":"eth","agent_did":"owner","enabled":false,
        "chain_id":1,"rpc_url":"http://localhost:8545","query_methods":["eth_chainId"]});
    validate(vec![
        (Collection::Tools, tools.clone()),
        (Collection::EthTool, eth.clone()),
    ])
    .unwrap();
    for (field, value) in [
        ("query_methods", json!(["eth_sendRawTransaction"])),
        ("rpc_timeout_secs", json!(0)),
        ("rpc_url", Value::Null),
        ("chain_id", json!(-1)),
    ] {
        let mut invalid = eth.clone();
        invalid[field] = value;
        assert!(validate(vec![
            (Collection::Tools, tools.clone()),
            (Collection::EthTool, invalid)
        ])
        .is_err());
    }
    validate(vec![(
        Collection::EthTool,
        json!({"tool_id":"empty","agent_did":"owner"}),
    )])
    .unwrap();
}

#[test]
fn retained_eth_expansion_intersects_existing_datastore_name_checks() {
    let tools = json!({"tools_id":"tools","agent_did":"owner",
        "datastore":{"datastore_tool_surface_ids":["surface"]},
        "integrations":{"eth_tool_ids":["eth"]}});
    let eth = json!({"tool_id":"eth","agent_did":"owner",
        "chain_id":1,"rpc_url":"http://localhost:8545","query_methods":["eth_chainId"]});
    let surface = |name| {
        json!({"surface_id":"surface","agent_did":"owner",
        "entries":[{"kind":"query","tool_name":name,"collection":"Records","fields":["name"]}]})
    };
    let candidate = |eth, name| {
        vec![
            (Collection::Tools, tools.clone()),
            (Collection::EthTool, eth),
            (Collection::DatastoreToolSurface, surface(name)),
        ]
    };
    validate(candidate(eth.clone(), "inspect")).unwrap();
    assert!(validate(candidate(eth.clone(), "eth_query")).is_err());
    let mut disabled = eth;
    disabled["enabled"] = json!(false);
    validate(candidate(disabled, "eth_query")).unwrap();
}

#[test]
fn chain_binding_intrinsics_use_signer_requirements_without_reading_host_keys() {
    let binding = json!({"binding_id":"binding","agent_did":"owner",
        "address":"0x1111111111111111111111111111111111111111",
        "key_backend":crate::KEY_BACKEND_KEYRING,
        "attestation":"checked-by-signing-owner", "created_at":"2026-01-01T00:00:00Z"});
    validate(vec![(Collection::ChainKeyBinding, binding.clone())]).unwrap();
    for field in ["key_backend", "attestation", "created_at"] {
        let mut invalid = binding.clone();
        invalid[field] = Value::Null;
        assert!(validate(vec![(Collection::ChainKeyBinding, invalid)]).is_err());
    }
    let mut invalid = binding;
    invalid["address"] = json!("invalid");
    assert!(validate(vec![(Collection::ChainKeyBinding, invalid)]).is_err());
}
