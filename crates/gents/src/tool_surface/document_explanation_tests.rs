use crate::config_client::ConfigAccess;
use crate::tool_surface::{
    measured_mcp_services_for_access, BehaviorToolConfig, RuntimeToolAvailability, ToolCeiling,
};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};

#[test]
fn document_explanation_resolves_target_owner_separately_from_destination() {
    let tools = serde_json::from_value(json!({"tools_id":"tools","agent_did":"owner",
        "subagents":{"spawn_enabled":true,"allow_cross_principal":true,"target_ids":["target"]}}))
    .unwrap();
    let target = serde_json::from_value(json!({"target_id":"target","agent_did":"owner",
        "target_agent_did":"remote","behavior_id":"worker","name":"worker"}))
    .unwrap();
    let build = |targets: &[crate::document_config::SubagentTargetDocument]| {
        BehaviorToolConfig::from_tools_documents(
            "coordinator",
            &tools,
            &[],
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
        &HashSet::new(),
    );
    assert!(explanation
        .tool_names
        .contains(&"spawn_subagent".to_owned()));
    assert!(build(&[]).is_err());
    let mut foreign = target.clone();
    foreign.agent_did = "foreign".to_owned();
    assert!(build(&[foreign]).is_err());
    assert!(build(&[target.clone(), target]).is_err());
}

#[tokio::test]
async fn measured_explanation_uses_scoped_endpoint_health_not_registry_presence() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(gents_protocol::schemas::TOOL_SERVICE_HEALTH_STATE)
        .await
        .unwrap();
    let access = ConfigAccess::Local(node.clone());
    let service = serde_json::from_value(json!({"service_id":"service","agent_did":"owner",
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
        let response = node.execute(&format!(r#"mutation {{ create_ToolServiceHealthState(input: {{agent_did:"{owner}",service_id:"service",endpoint:"{endpoint}",status:"healthy"}}) {{_docID}} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }
    assert!(
        measured_mcp_services_for_access(&access, "owner", &selected)
            .await
            .unwrap()
            .is_empty()
    );
    let response = node.execute(r#"mutation { create_ToolServiceHealthState(input: {agent_did:"owner",service_id:"service",endpoint:"http://server.invalid:8000/mcp",status:"healthy"}) {_docID} }"#).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
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
