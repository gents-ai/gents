use std::sync::Arc;

use crate::llm::tool::Tool;
use defra_node::EmbeddedNode;
use serde_json::json;

use super::BoundedWriteTool;
use crate::document_config::{WriteToolDecl, WriteToolField};

async fn node_with_actionrequest() -> Arc<EmbeddedNode> {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(
        r#"
        type ActionRequest { drift_sig: String summary: String status: String }
    "#,
    )
    .await
    .unwrap();
    node
}

fn decl() -> WriteToolDecl {
    WriteToolDecl {
        tool_name: "request_action".into(),
        collection: "ActionRequest".into(),
        description: "Emit one ActionRequest.".into(),
        fields: vec![
            WriteToolField {
                name: "drift_sig".into(),
                required: true,
            },
            WriteToolField {
                name: "summary".into(),
                required: true,
            },
            WriteToolField {
                name: "status".into(),
                required: false,
            },
        ],
    }
}

#[tokio::test]
async fn writes_one_bounded_doc() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(Arc::clone(&node), decl());
    let out = Tool::call(
        &tool,
        serde_json::from_value(json!({
            "drift_sig": "abc", "summary": "stale host doc", "status": "open"
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    assert!(out.contains("ActionRequest"));
    let resp = node
        .execute("{ ActionRequest { drift_sig summary status } }")
        .await;
    let rows = resp
        .data
        .unwrap()
        .get("ActionRequest")
        .unwrap()
        .as_array()
        .unwrap()
        .len();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn rejects_missing_required_and_undeclared_fields() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(node, decl());
    assert!(Tool::call(
        &tool,
        serde_json::from_value(json!({ "summary": "x" })).unwrap()
    )
    .await
    .is_err());
    assert!(Tool::call(
        &tool,
        serde_json::from_value(json!({
            "drift_sig": "a", "summary": "b", "evil": "1"
        }))
        .unwrap()
    )
    .await
    .is_err());
}

/// The advertised tool name comes from the declaration, not a shared const —
/// this is what makes one declaration map to one uniquely-named runtime tool.
#[tokio::test]
async fn advertised_name_and_required_array_come_from_decl() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(node, decl());

    assert_eq!(Tool::name(&tool), "request_action");

    let def = Tool::definition(&tool, String::new()).await;
    assert_eq!(def.name, "request_action");
    assert_eq!(def.description, "Emit one ActionRequest.");

    let required = def.parameters["required"].as_array().unwrap();
    assert!(required.iter().any(|v| v == "drift_sig"));
    assert!(required.iter().any(|v| v == "summary"));
    assert!(!required.iter().any(|v| v == "status"));

    let props = def.parameters["properties"].as_object().unwrap();
    assert!(props.contains_key("drift_sig"));
    assert!(props.contains_key("summary"));
    assert!(props.contains_key("status"));
}

/// A declaration with an empty collection is a config/programming error and must
/// not silently produce a tool that writes to `""`.
#[tokio::test]
async fn rejects_write_with_empty_collection_decl() {
    let node = node_with_actionrequest().await;
    let bad = WriteToolDecl {
        tool_name: "broken".into(),
        collection: "".into(),
        description: "no collection".into(),
        fields: vec![],
    };
    let tool = BoundedWriteTool::new(node, bad);
    assert!(
        Tool::call(&tool, serde_json::from_value(json!({})).unwrap())
            .await
            .is_err()
    );
}
