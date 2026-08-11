use std::sync::Arc;

use super::super::*;
use super::support::*;
use crate::ensure_runtime_schemas;
use crate::tool_surface::ToolCeiling;

#[tokio::test]
async fn builder_rejects_node_without_signing_did() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let identity = Arc::new(test_identity("builder-unsigned-node"));

    let error = match Gents::builder()
        .node(node)
        .identity(identity)
        .behavior("policy-ops")
        .done()
        .build()
        .await
    {
        Ok(_) => panic!("builder should reject an unsigned node"),
        Err(error) => error,
    };

    assert!(error
        .to_string()
        .contains("EmbeddedNode configured with a node signing DID"));
}

#[tokio::test]
async fn builder_includes_custom_tools_in_resolved_tool_surface() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    insert_backend(node.as_ref(), "builder-backend", "http://127.0.0.1:8777/v1").await;
    let identity = Arc::new(test_identity("builder-custom-tools"));

    let agent = Gents::builder()
        .node(node.clone())
        .identity(identity.clone())
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior("policy-ops")
        .backend_id("builder-backend")
        .system_prompt("You manage policies.")
        .custom_tool(EchoTool)
        .done()
        .build()
        .await
        .unwrap();

    assert_eq!(agent.agent_did(), identity.did());
    assert_eq!(agent.default_behavior_id(), "policy-ops");
    assert!(agent.document_runtime_context().is_none());
    assert_eq!(
        agent.behaviors()[0].tools.custom_tool_names(),
        vec!["echo_value".to_string()]
    );

    let tool_surface = agent.behaviors()[0]
        .tools
        .resolve(node.as_ref())
        .await
        .unwrap();
    assert!(tool_surface
        .tool_names()
        .contains(&"echo_value".to_string()));
}

#[tokio::test]
async fn builder_requires_resolvable_backend_documents() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("builder-missing-backend"));

    let error = match Gents::builder()
        .node(node)
        .identity(identity)
        .behavior("policy-ops")
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
        .contains("behavior 'policy-ops' references missing backend missing-backend"));
}
