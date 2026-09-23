use super::*;
use crate::config_client::ConfigAccess;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::streaming::{DefraStreamWriter, SpawnAdmissionPlan};
use crate::tool_call_lifecycle::{
    AwaitMode, CancelCause, CancelPolicy, CascadeDispatch, ToolCallLifecycle,
};
use gents_protocol::message::{AssistantContent, Message, ToolCall, ToolFunction};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Real claimed-request fixture, mirroring `tool_control.rs::claimed_request`:
/// a pending row is claimed through the owned completion loop before the
/// canonical publication runs.
async fn claimed_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
) -> RequestLifecycle {
    let now = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", agent_did: "{agent_did}", behavior_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "cascade fixture", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#)).await;
    assert!(!created.has_errors(), "{:#?}", created.errors);
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow = crate::graphql::first_row(&row, "AgentRequest")
        .unwrap()
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        &agent_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

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

    // Canonical bridge genesis: the parent request is claimed through the real
    // lifecycle, then the spawn publication creates the accepted assistant
    // header and its pending bridge row with the immutable child edge.
    let mut parent_lifecycle = claimed_request(&node, "parent", "session", owner).await;
    let parent = parent_lifecycle.request().doc_id.clone();
    let writer = DefraStreamWriter::new(node.clone(), owner, Duration::from_millis(1));
    parent_lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    writer
        .start_provider_attempt(&parent, 0, 0, "inference.1".parse().unwrap())
        .await;
    let message = Message::Assistant {
        id: Some("cascade-provider-message".into()),
        content: vec![AssistantContent::ToolCall(ToolCall {
            id: "bridge".into(),
            call_id: Some("cascade-provider-call".into()),
            function: ToolFunction::new(
                crate::toolset::SPAWN_SUBAGENT_TOOL_NAME.into(),
                serde_json::json!({"name":"child","prompt":"work","await_mode":"background"}),
            ),
            signature: None,
            additional_params: None,
        })],
    };
    let plan = SpawnAdmissionPlan {
        tool_call_id: "bridge".into(),
        child_request_id: "child".into(),
        spawn_target_did: owner.into(),
        spawn_behavior_id: "behavior".into(),
        delegated_workspace: None,
        await_mode: AwaitMode::Background,
    };
    let mut published = writer
        .publish_native_turn_with_spawn_admissions(
            &parent_lifecycle,
            0,
            0,
            &message,
            std::slice::from_ref(&plan),
        )
        .await
        .unwrap();
    let accepted = published
        .accepted_tools
        .pop()
        .expect("canonical publication accepted spawn_subagent");
    let bridge = accepted.tool_call_doc_id.clone();
    let deadline = parent_lifecycle
        .claimed_deadline_at()
        .expect("claimed request deadline");

    // Foreign negative control: a same-label bridge under another principal,
    // and children whose provenance selects between the physical bridges.
    let foreign_bridge_input = json!({"tool_call_id":"bridge","tool_call_key":"foreign-bridge-key","agent_did":foreign,"requester_did":null,"session_id":"session","request_id":"parent","request_doc_id":parent,"message_sequence":1,"tool_name":"spawn_agent","args":"{}","lifecycle_state":"running","started_at":"2026-09-01T00:00:00Z","deadline_at":"2030-09-01T00:00:00Z","await_mode":"background","cancel_policy":"cascade","child_request_id":"child","spawn_target_did":foreign});
    let foreign_bridge = create(&node, "AgentToolCall", foreign_bridge_input).await;
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
    // The physical accepted binding from the publication is the only dispatch
    // constructor; the bridge becomes running from that exact admission.
    let mut lifecycle = ToolCallLifecycle::from_accepted(
        node.clone(),
        owner.to_owned(),
        None,
        accepted,
        deadline,
        AwaitMode::Background,
        CancelPolicy::Cascade,
    )
    .unwrap();
    lifecycle.start_running().await.unwrap();
    assert!(lifecycle
        .publish_background_receipt("child started")
        .await
        .unwrap());
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
