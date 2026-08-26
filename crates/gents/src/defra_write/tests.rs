use std::sync::Arc;

use crate::llm::tool::Tool;
use defra_node::EmbeddedNode;
use serde_json::json;

use super::BoundedWriteTool;
use crate::document_config::{WriteToolDecl, WriteToolField, WriteToolFieldFill};

async fn node_with_actionrequest() -> Arc<EmbeddedNode> {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    node.add_schema(
        r#"
        type ActionRequest {
            drift_sig: String
            summary: String
            status: String
            run_id: String
            expected_total: String
        }
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
                fill: None,
            },
            WriteToolField {
                name: "summary".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "status".into(),
                required: false,
                fill: None,
            },
        ],
        output_obligation: None,
    }
}

#[tokio::test]
async fn rejects_raw_mailbox_writer_even_when_declaration_is_canonical() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let tool = BoundedWriteTool::new(node, crate::mailbox::canonical_mailbox_write_decl());
    assert!(!tool.is_well_formed());
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

#[tokio::test]
async fn runtime_backstop_rejects_invalid_graphql_identifiers() {
    let node = node_with_actionrequest().await;
    let mut empty_collection = decl();
    empty_collection.collection.clear();
    let mut bad_collection = decl();
    bad_collection.collection = "ActionRequest) { _docID } mutation {".into();
    let mut bad_field = decl();
    bad_field.fields[0].name = "drift_sig: \"injected\"".into();

    for declaration in [empty_collection, bad_collection, bad_field] {
        let result = Tool::call(
            &BoundedWriteTool::new(Arc::clone(&node), declaration),
            serde_json::from_value(json!({ "drift_sig": "a", "summary": "b" })).unwrap(),
        )
        .await;
        assert!(result.is_err(), "runtime must reject invalid identifiers");
    }
}

/// Regression for audit finding `write-tool-graphql-identifier-injection`:
/// collection and field names are interpolated as bare GraphQL identifiers,
/// so a non-identifier field name must be rejected before the mutation is
/// built (mirroring the query-tool path's `validate_identifier`).
#[tokio::test]
async fn rejects_decl_with_non_identifier_field_name() {
    let node = node_with_actionrequest().await;
    let bad = WriteToolDecl {
        tool_name: "broken".into(),
        collection: "ActionRequest".into(),
        description: "field name breaks out of identifier position".into(),
        fields: vec![WriteToolField {
            name: "summary\" }) { _docID } } mutation evil { drop(input: { x".into(),
            required: false,
            fill: None,
        }],
        output_obligation: None,
    };
    let tool = BoundedWriteTool::new(node, bad);
    assert!(
        Tool::call(&tool, serde_json::from_value(json!({})).unwrap())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn runtime_fills_are_hidden_rejected_from_model_input_and_stamped_at_call_time() {
    let node = node_with_actionrequest().await;
    let tool = BoundedWriteTool::new(
        Arc::clone(&node),
        WriteToolDecl {
            tool_name: "write_result".into(),
            collection: "ActionRequest".into(),
            description: "write a correlated result".into(),
            fields: vec![
                WriteToolField {
                    name: "summary".into(),
                    required: true,
                    fill: None,
                },
                WriteToolField {
                    name: "run_id".into(),
                    required: false,
                    fill: Some(WriteToolFieldFill::Correlation),
                },
                WriteToolField {
                    name: "expected_total".into(),
                    required: false,
                    fill: Some(WriteToolFieldFill::SourceField("expected_total".into())),
                },
            ],
            output_obligation: None,
        },
    );
    let definition = Tool::definition(&tool, String::new()).await;
    let properties = definition.parameters["properties"].as_object().unwrap();
    assert!(properties.contains_key("summary"));
    assert!(!properties.contains_key("run_id"));
    assert!(!properties.contains_key("expected_total"));

    let supplied = Tool::call(
        &tool,
        serde_json::from_value(json!({"summary": "done", "run_id": "model-value"})).unwrap(),
    )
    .await;
    assert!(supplied.unwrap_err().to_string().contains("runtime-filled"));

    let mut source_fields = std::collections::BTreeMap::new();
    source_fields.insert("expected_total".to_string(), "3".to_string());
    crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None,
        tokio_util::sync::CancellationToken::new(),
        None,
        None,
        None,
        Some("run-42".to_string()),
        source_fields,
        false,
        async {
            Tool::call(
                &tool,
                serde_json::from_value(json!({"summary": "done"})).unwrap(),
            )
            .await
            .expect("runtime-filled write");
        },
    )
    .await;

    let response = node
        .execute("{ ActionRequest { summary run_id expected_total } }")
        .await;
    let row = response.data.unwrap()["ActionRequest"][0].clone();
    assert_eq!(row["run_id"], "run-42");
    assert_eq!(row["expected_total"], "3");
}
