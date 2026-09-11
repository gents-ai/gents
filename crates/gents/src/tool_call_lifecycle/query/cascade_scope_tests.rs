use super::*;
use crate::config_client::ConfigAccess;
use crate::tool_call_lifecycle::{CancelCause, CascadeDispatch, ToolCallLifecycle};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test]
async fn cascade_selects_reciprocal_physical_child_and_preserves_foreign_scope() {
    let temp = tempfile::tempdir().unwrap();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(temp.path())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    async fn create(node: &EmbeddedNode, collection: &'static str, value: Value) -> String {
        ConfigAccess::transact_local(node, None, "test.cascade.scope", |txn| {
            let value = value.clone();
            Box::pin(async move {
                let result = txn.execute_with_variables(&format!("mutation($input:{collection}MutationInputArg!){{create_{collection}(input:$input){{_docID}}}}"), &json!({"input":value})).await?;
                gents_protocol::graphql::extract_mutation_doc_id(&result, collection)
            })
        }).await.unwrap()
    }
    let owner = "did:test:cascade-owner";
    let foreign = "did:test:cascade-foreign";
    let parent = create(&node, "AgentRequest", json!({"request_id":"parent","agent_did":owner,"requester_did":null,"session_id":"session","behavior_id":"behavior","content":"parent","created_at":"2026-09-01T00:00:00Z","lifecycle_state":"processing"})).await;
    let bridge_input = json!({"tool_call_id":"bridge","tool_call_key":"own-bridge-key","agent_did":owner,"requester_did":null,"session_id":"session","request_id":"parent","request_doc_id":parent,"message_sequence":1,"tool_name":"spawn_agent","args":"{}","lifecycle_state":"running","started_at":"2026-09-01T00:00:00Z","deadline_at":"2030-09-01T00:00:00Z","await_mode":"background","cancel_policy":"cascade","child_request_id":"child","spawn_target_did":owner});
    let bridge = create(&node, "AgentToolCall", bridge_input.clone()).await;
    let mut foreign_bridge = bridge_input;
    foreign_bridge["agent_did"] = json!(foreign);
    foreign_bridge["tool_call_key"] = json!("foreign-bridge-key");
    let foreign_bridge = create(&node, "AgentToolCall", foreign_bridge).await;
    let child_input = json!({"request_id":"child","agent_did":owner,"requester_did":owner,"session_id":"child-session","behavior_id":"behavior","content":"child","created_at":"2026-09-01T00:00:00Z","lifecycle_state":"processing","caused_by_parent_request_id":"parent","caused_by_parent_request_doc_id":parent,"caused_by_parent_tool_call_id":"bridge","caused_by_parent_tool_call_doc_id":bridge});
    let child = create(&node, "AgentRequest", child_input.clone()).await;
    let mut forged_child = child_input;
    forged_child["agent_did"] = json!(foreign);
    forged_child["caused_by_parent_tool_call_doc_id"] = json!(foreign_bridge);
    let forged = create(&node, "AgentRequest", forged_child).await;
    assert!(ToolCallLifecycle::load(node.clone(), "session", "bridge")
        .await
        .is_err());
    assert!(
        ToolCallLifecycle::load_by_doc_id(node.clone(), &bridge, foreign, "session", None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &bridge,
        owner,
        "session",
        Some(owner)
    )
    .await
    .unwrap()
    .is_none());
    let mut lifecycle =
        ToolCallLifecycle::load_by_doc_id(node.clone(), &bridge, owner, "session", None)
            .await
            .unwrap()
            .unwrap();
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(CancelCause::Interrupted, owner)
        .await
        .unwrap()
        .unwrap();
    let CascadeDispatch::Local {
        intent,
        child: selected,
    } = dispatch
    else {
        panic!("actual local reciprocal child must be selected")
    };
    assert_eq!(intent.child_request_id, "child");
    assert_eq!(selected.doc_id.as_deref(), Some(child.as_str()));
    assert_eq!(selected.requester_did.as_deref(), Some(owner));
    crate::interrupt::interrupt_request_by_doc_id(&node, &child, owner, Some(owner))
        .await
        .unwrap();
    let response = node.execute(&format!("{{AgentRequest(filter:{{_docID:{{_eq:\"{}\"}}}}){{request_id interrupt_requested_at}}}}", crate::graphql::escape_graphql_string(&forged))).await;
    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].interrupt_requested_at.is_none(),
        "foreign same-label child must remain unlatched"
    );
    let response = node
        .execute(&format!(
            "{{AgentToolCall(filter:{{_docID:{{_eq:\"{}\"}}}}){{lifecycle_state}}}}",
            crate::graphql::escape_graphql_string(&foreign_bridge)
        ))
        .await;
    let rows: Vec<Value> = crate::graphql::rows(&response, "AgentToolCall").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["lifecycle_state"], "running");
    node.shutdown().await;
}
