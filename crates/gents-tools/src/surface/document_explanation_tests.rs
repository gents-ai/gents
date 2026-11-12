use crate::surface::{
    measured_mcp_services_for_access, AgentToolSurfaceConfig, RuntimeToolAvailability, ToolCeiling,
};
use gents_protocol::document_query::{fields::tool_service_health_state as health, Create, Input};
use gents_store::config_client::ConfigAccess;
use rapidhash::{HashSetExt, RapidHashSet};
use serde_json::json;
use std::sync::Arc;

#[test]
fn document_explanation_resolves_target_owner_separately_from_destination() {
    let tools = serde_json::from_value(json!({"tools_id":"tools","node_did":"owner",
        "agents":{"enabled":true,"target_ids":["target"]}}))
    .unwrap();
    let target = serde_json::from_value(json!({"target_id":"target","node_did":"owner",
        "target_node_did":"remote","agent_did":"worker","name":"worker"}))
    .unwrap();
    let build = |targets: &[gents_documents::AgentTargetDocument]| {
        AgentToolSurfaceConfig::from_tools_documents(
            "coordinator",
            &tools,
            &[],
            targets,
            &ToolCeiling::meta_only(),
            Vec::new(),
        )
    };
    let config = build(std::slice::from_ref(&target)).unwrap();
    let explanation = config.explain_with_runtime_availability(
        RuntimeToolAvailability::from_online_mcp_services(Vec::<String>::new()),
        "owner",
        &RapidHashSet::new(),
    );
    assert!(explanation.tool_names.contains(&"agent_new".to_owned()));
    assert!(build(&[]).is_err());
    let mut foreign = target.clone();
    foreign.node_did = "foreign".to_owned();
    assert!(build(&[foreign]).is_err());
    assert!(build(&[target.clone(), target]).is_err());
}

#[tokio::test]
async fn measured_explanation_uses_scoped_endpoint_health_not_registry_presence() {
    let node = Arc::new(
        gents_store::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents_store::test_support::acp_policies::install_identity_acp_policies(&node)
        .await
        .unwrap();
    node.add_collection(gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE)
        .await
        .unwrap();
    let access = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(node.node_identity_did().unwrap()).unwrap(),
    );
    let service = serde_json::from_value(json!({"service_id":"service","node_did":"owner",
        "hostname":"server.invalid","mcp_port":8000,"mcp_path":"/mcp"}))
    .unwrap();
    let selected = vec![service];
    assert!(
        measured_mcp_services_for_access(&access, "owner", &selected)
            .await
            .unwrap()
            .is_empty()
    );
    for (owner, endpoint) in [
        ("foreign", "http://server.invalid:8000/mcp"),
        ("owner", "http://127.0.0.1:8000/mcp"),
    ] {
        let mutation = Create::new(
            Input::new()
                .set(health::NODE_DID, owner)
                .set(health::SERVICE_ID, "service")
                .set(health::ENDPOINT, endpoint)
                .set(health::STATUS, "healthy")
                .set(health::UPDATED_AT, "2026-01-01T00:00:00Z"),
        )
        .render();
        access
            .write_rendered("test.mcp.measured_health", &mutation)
            .await
            .unwrap();
    }
    assert!(
        measured_mcp_services_for_access(&access, "owner", &selected)
            .await
            .unwrap()
            .is_empty()
    );
    let mutation = Create::new(
        Input::new()
            .set(health::NODE_DID, "owner")
            .set(health::SERVICE_ID, "service")
            .set(health::ENDPOINT, "http://server.invalid:8000/mcp")
            .set(health::STATUS, "healthy")
            .set(health::UPDATED_AT, "2026-01-02T00:00:00Z"),
    )
    .render();
    access
        .write_rendered("test.mcp.current_health", &mutation)
        .await
        .unwrap();
    assert_eq!(
        measured_mcp_services_for_access(&access, "owner", &selected)
            .await
            .unwrap(),
        vec!["service"]
    );
    let mut disabled = selected;
    disabled[0].enabled = false;
    assert!(
        measured_mcp_services_for_access(&access, "owner", &disabled)
            .await
            .unwrap()
            .is_empty()
    );
}
