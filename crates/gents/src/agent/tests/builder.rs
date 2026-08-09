use std::sync::Arc;

use super::super::*;
use super::support::*;
use crate::ensure_runtime_schemas;
use crate::tool_surface::ToolCeiling;

#[tokio::test]
async fn builder_includes_custom_tools_in_resolved_tool_surface() {
    let identity = Arc::new(test_identity("builder-custom-tools"));
    let node = test_node_for_identity(identity.as_ref()).await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    insert_backend(node.as_ref(), "builder-backend", "http://127.0.0.1:8777/v1").await;

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
    let identity = Arc::new(test_identity("builder-missing-backend"));
    let node = test_node_for_identity(identity.as_ref()).await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

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

#[tokio::test]
async fn builder_rejects_unsigned_node_before_behavior_resolution() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let identity = Arc::new(test_identity("builder-unsigned-node"));

    let error = Gents::builder()
        .node(node)
        .identity(identity.clone())
        .behavior("policy-ops")
        .backend_id("never-resolved")
        .done()
        .build()
        .await
        .err()
        .expect("unsigned node must be rejected");

    let message = error.to_string();
    assert!(message.contains("node is unsigned"), "{message}");
    assert!(message.contains(identity.did()), "{message}");
}

#[tokio::test]
async fn builder_rejects_node_signer_that_differs_from_principal() {
    let node_identity = test_identity("builder-node-signer");
    let node = test_node_for_identity(&node_identity).await;
    let principal_identity = Arc::new(test_identity("builder-principal-signer"));

    let error = Gents::builder()
        .node(node)
        .identity(principal_identity.clone())
        .behavior("policy-ops")
        .backend_id("never-resolved")
        .done()
        .build()
        .await
        .err()
        .expect("mismatched signer must be rejected");

    let message = error.to_string();
    assert!(message.contains("identity mismatch"), "{message}");
    assert!(message.contains(node_identity.did()), "{message}");
    assert!(message.contains(principal_identity.did()), "{message}");
}
