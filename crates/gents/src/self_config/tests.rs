//! Unit tests for the self-config tool family. End-to-end lifecycle coverage
//! (identity-scoped writes, reconcile pickup) lives in
//! `tests/e2e_runtime/self_config_tools.rs`.

use super::*;

fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        behavior_id: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        dry_run: false,
    }
}

#[test]
fn tool_names_follow_enabled_categories() {
    let names = self_config_tool_names(&config(&["behavior", "tools", "profile"]));
    assert_eq!(
        names,
        vec![
            GET_MY_CONFIG_TOOL_NAME.to_string(),
            CONFIGURE_BEHAVIOR_TOOL_NAME.to_string(),
            CONFIGURE_PROFILE_TOOL_NAME.to_string(),
            CONFIGURE_TOOLS_TOOL_NAME.to_string(),
        ],
        "get_my_config always leads; configure tools follow the sorted category set"
    );

    let disabled = SelfConfigToolConfig::default();
    assert!(self_config_tool_names(&disabled).is_empty());
}

#[test]
fn every_tool_name_is_reserved_builtin() {
    for name in SELF_CONFIG_TOOL_NAMES {
        assert!(
            crate::document_config::is_reserved_builtin_tool_name(name),
            "{name} must be reserved so write_tools declarations cannot shadow it"
        );
    }
}

#[tokio::test]
async fn build_fails_closed_without_agent_did() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        String::new(),
        &config(&["behavior"]),
    );
    assert!(
        tools.is_empty(),
        "an empty agent DID must register no self-config tools"
    );
}

#[tokio::test]
async fn build_registers_gated_family() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::EmbeddedNode::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::new(node),
        "did:key:zSelfConfigTest".to_string(),
        &config(&["behavior", "backend"]),
    );
    let names: Vec<String> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(
        names,
        vec![
            GET_MY_CONFIG_TOOL_NAME.to_string(),
            CONFIGURE_BACKEND_TOOL_NAME.to_string(),
            CONFIGURE_BEHAVIOR_TOOL_NAME.to_string(),
        ]
    );
}
