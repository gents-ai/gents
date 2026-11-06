use std::sync::Arc;

use super::super::*;
use super::support::*;
use crate::tools::surface::ToolCeiling;

#[tokio::test]
async fn builder_requires_backend_admission_before_checking_node_identity() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let identity = Arc::new(test_identity("builder-unsigned-node"));

    let error = match Gents::builder()
        .node(node)
        .identity(identity)
        .agent("policy-ops")
        .done()
        .build()
        .await
    {
        Ok(_) => panic!("builder should reject a node without backend protection admission"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "fresh backend protection provenance is required"
    );
}

#[tokio::test]
async fn builder_includes_custom_tools_in_resolved_tool_surface() {
    let (db, _owner_identity, owner) = test_node("builder-custom-tools").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    insert_backend(
        identity.did(),
        &owner,
        "builder-backend",
        "http://127.0.0.1:8777/v1",
    )
    .await;

    let binding = crate::store::identity::provision_agent_identity_authorized(
        &node,
        identity.as_ref(),
        &identity::Did::new(owner.actor_did().expect("signed fixture owner")).unwrap(),
    )
    .await
    .unwrap();
    let expected_agent_did = binding.agent_did.clone();
    let agent = Gents::builder()
        .node(node.clone())
        .backend_admission(db.home.backend_admission())
        .identity(identity.clone())
        .tool_ceiling(ToolCeiling::meta_only())
        .agent("policy-ops")
        .identity_binding(binding)
        .backend_id("builder-backend")
        .system_prompt("You manage policies.")
        .custom_tool(EchoTool)
        .done()
        .build()
        .await
        .unwrap();

    assert_eq!(agent.node_did(), identity.did());
    assert_eq!(agent.default_agent_did(), expected_agent_did);
    assert!(agent.document_runtime_context().is_none());
    assert_eq!(
        agent.agents()[0].tools.custom_tool_names(),
        vec!["echo_value".to_string()]
    );

    let tool_surface = agent.agents()[0]
        .tools
        .resolve(&node, identity.did(), &Default::default())
        .await
        .unwrap();
    assert!(tool_surface
        .tool_names()
        .contains(&"echo_value".to_string()));
}

#[tokio::test]
async fn builder_requires_resolvable_backend_documents() {
    let (db, _owner_identity, _owner) = test_node("builder-missing-backend").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();

    let error = match Gents::builder()
        .node(node)
        .backend_admission(db.home.backend_admission())
        .identity(identity)
        .agent("policy-ops")
        .backend_id("missing-backend")
        .done()
        .build()
        .await
    {
        Ok(_) => panic!("builder should reject missing backend docs"),
        Err(error) => error,
    };

    assert!(error
        .to_string()
        .contains("agent 'policy-ops' references missing backend missing-backend"));
}

#[tokio::test]
async fn builder_rejects_backend_vetoed_by_measured_health() {
    let (db, _owner_identity, owner) = test_node("builder-measured-unhealthy").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    insert_backend(
        identity.did(),
        &owner,
        "builder-vetoed-backend",
        "http://127.0.0.1:8778/v1",
    )
    .await;

    let backend_health = crate::inference::backends::health::BackendHealthMap::new();
    backend_health
        .set_for_test(
            "builder-vetoed-backend",
            crate::inference::backends::health::BackendHealthState::Unhealthy,
            3,
        )
        .await;

    let error = match Gents::builder()
        .node(node)
        .backend_admission(db.home.backend_admission())
        .identity(identity)
        .backend_health(backend_health)
        .agent("policy-ops")
        .backend_id("builder-vetoed-backend")
        .done()
        .build()
        .await
    {
        Ok(_) => panic!("builder should reject a backend this runtime has measured unhealthy"),
        Err(error) => error,
    };

    assert!(
        error.to_string().contains("measured_unhealthy=true"),
        "error should name the measured-health veto: {error}"
    );
}
