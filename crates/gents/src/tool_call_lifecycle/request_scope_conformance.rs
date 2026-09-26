//! Physical request-scope checks that require the private accepted-call seam.

use super::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use crate::background_completion::{
    project_background_subagent_completion, BackgroundCompletionOutcome,
};
use crate::identity::AgentIdentity;
use crate::streaming::SpawnAdmissionPlan;
use crate::tool_call_lifecycle::admission_fixture::{
    claimed_request, complete_child, publish_accepted_on_claimed_request,
};
use defra_node::EmbeddedNode;
use std::sync::Arc;

async fn exec(node: &EmbeddedNode, statement: &str) {
    let response = node.execute(statement).await;
    assert!(
        !response.has_errors(),
        "GraphQL errors: {:?}",
        response.errors
    );
}

async fn state(node: &EmbeddedNode, collection: &str, doc_id: &str) -> String {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let response = node.execute(&format!(
        r#"{{ {collection}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ lifecycle_state }} }}"#,
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()[collection][0]["lifecycle_state"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn scope_row(node: &EmbeddedNode, collection: &str, doc_id: &str) -> serde_json::Value {
    let fields = match collection {
        "AgentRequest" => "_docID request_id agent_did lifecycle_state failure_reason deadline execution_generation execution_lease_expires_at",
        "AgentToolCall" => "_docID request_doc_id agent_did tool_call_id child_request_id lifecycle_state cancel_cause tool_failure_class",
        other => panic!("unsupported scope collection {other}"),
    };
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let response = node.execute(&format!(
        r#"{{ {collection}(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{ {fields} }} }}"#,
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.unwrap()[collection][0].clone()
}

async fn expired_child(
    node: &EmbeddedNode,
    agent_did: &str,
    parent_request_id: &str,
    parent_request_doc_id: &str,
    tool_call_id: &str,
    tool_call_doc_id: &str,
    child_request_id: &str,
) -> String {
    let exact_child_request_id = child_request_id.to_owned();
    let fields = [
        agent_did,
        parent_request_id,
        parent_request_doc_id,
        tool_call_id,
        tool_call_doc_id,
        child_request_id,
    ]
    .map(crate::graphql::escape_graphql_string);
    let [agent_did, parent_request_id, parent_request_doc_id, tool_call_id, tool_call_doc_id, child_request_id] =
        fields;
    let created_at = chrono::Utc::now().to_rfc3339();
    let created_at = crate::graphql::escape_graphql_string(&created_at);
    let past = crate::graphql::escape_graphql_string(
        &(chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339(),
    );
    let response = node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{child_request_id}", purpose: "normal", agent_did: "{agent_did}",
        behavior_id: "general", session_id: "session-{child_request_id}",
        retry_parent_request: "", retry_root_request: "{child_request_id}",
        superseded_by_request: "", content: "expired child",
        lifecycle_state: "claimed", backend_id: "", execution_origin: "interactive",
        failure_reason: "", created_at: "{created_at}", claimed_at: "{created_at}",
        deadline: "{past}", execution_generation: "{child_request_id}",
        execution_lease_expires_at: "{past}",
        retry_count: 0, max_retries: 3, subagent_depth: 1,
        caused_by_parent_request_id: "{parent_request_id}",
        caused_by_parent_request_doc_id: "{parent_request_doc_id}",
        caused_by_parent_tool_call_id: "{tool_call_id}",
        caused_by_parent_tool_call_doc_id: "{tool_call_doc_id}",
        caused_by_trigger_id: "{tool_call_id}", caused_by_trigger_kind: "subagent"
    }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    crate::request_binding::require_request_doc_id(node, &exact_child_request_id)
        .await
        .unwrap()
}

async fn queue_snapshot(
    node: &EmbeddedNode,
    session_id: &str,
    queue_key: Option<&str>,
) -> (Vec<String>, usize) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ request_id lifecycle_state input }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone();
    let pending = rows
        .iter()
        .filter(|row| row["lifecycle_state"] == "pending")
        .filter_map(|row| row["request_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let coalesced = rows
        .iter()
        .filter(|row| {
            row["lifecycle_state"] == "pending"
                && row["input"]["queue"]["source"] == "background_completion"
                && row["input"]["queue"]["policy"] == "coalesce"
                && queue_key.is_some_and(|key| row["input"]["queue"]["key"] == key)
        })
        .count();
    (pending, coalesced)
}

#[tokio::test]
async fn generated_background_completion_queue_case_uses_accepted_bridges() {
    let case = crate::lean_vocab_test::lean_queue_deadline_case(
        "background_completion_notification_creates_no_agent_request",
    );
    assert_eq!(case.group, "completion_delivery");
    assert_eq!(case.action, "appendNotification");
    assert!(case.legal);
    assert_eq!(case.pre_active_request_id, case.post_active_request_id);
    let path = std::env::temp_dir().join(format!("queue-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let identity = crate::KeyIdentity::load_or_create(path.join("queue-agent.key"), None).unwrap();
    let agent_did = identity.did();
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(agent_did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(node.as_ref(), agent_did, "general").await;
    let parent_id = format!("queue-deadline-coalesce-parent-{}", uuid::Uuid::new_v4());
    let session_id = case.session_id.to_string();
    let escaped_session = crate::graphql::escape_graphql_string(&session_id);
    exec(node.as_ref(), &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{escaped_session}", agent_did: "{agent_did}", behavior_id: "general", created_at: "2026-03-23T00:00:00Z" }}) {{ _docID }} }}"#)).await;
    let mut parent = claimed_request(&node, &parent_id, &session_id, agent_did).await;
    let parent_doc_id = parent.request().doc_id.clone();
    let queue_key = case.queue_key.as_deref();
    let pre = queue_snapshot(node.as_ref(), &session_id, queue_key).await;
    assert_eq!(
        pre.0.len(),
        case.pre_pending_request_ids.len(),
        "{} pre pending queue drifted",
        case.name
    );
    assert_eq!(pre.1, 0, "{} pre coalesced count drifted", case.name);

    let mut children = Vec::new();
    for turn in 0..2 {
        let tool_id = format!("queue-deadline-coalesce-{}-{turn}", uuid::Uuid::new_v4());
        let child_id = format!("{parent_id}-{tool_id}-child");
        let mut bridge = publish_accepted_on_claimed_request(
            node.clone(), &mut parent, agent_did, turn,
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME, &tool_id,
            serde_json::json!({"name":"child", "prompt": format!("prompt for {tool_id}"), "await_mode":"background"}),
            Some(SpawnAdmissionPlan {
                tool_call_id: tool_id.clone(), child_request_id: child_id.clone(),
                spawn_target_did: agent_did.into(), spawn_behavior_id: "general".into(),
                delegated_workspace: None, await_mode: AwaitMode::Background,
            }), AwaitMode::Background, CancelPolicy::Cascade, true,
        ).await.unwrap();
        assert!(bridge
            .publish_background_receipt("child started")
            .await
            .unwrap());
        assert!(!bridge
            .publish_background_receipt("child started")
            .await
            .unwrap());
        let tool_doc_id = bridge.doc_id().unwrap().to_owned();
        assert_eq!(
            bridge.request_doc_id.as_deref(),
            Some(parent_doc_id.as_str())
        );
        let persisted = scope_row(node.as_ref(), "AgentToolCall", &tool_doc_id).await;
        assert_eq!(persisted["request_doc_id"], parent_doc_id);
        assert_eq!(persisted["agent_did"], agent_did);
        assert_eq!(persisted["child_request_id"], child_id);
        assert_eq!(persisted["lifecycle_state"], "running");
        children.push((child_id, tool_id, tool_doc_id));
    }
    // Both provider turns must be accepted while the parent still owns its
    // execution. The projection premise observes it as terminal afterwards.
    let escaped_parent_doc = crate::graphql::escape_graphql_string(&parent_doc_id);
    exec(node.as_ref(), &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_parent_doc}" }} }}, input: {{ lifecycle_state: "completed" }}) {{ _docID }} }}"#)).await;
    for (index, (child_id, tool_id, tool_doc_id)) in children.iter().enumerate() {
        let child = crate::tool_call_lifecycle::create_subagent_request_with_request_id(
            node.as_ref(),
            child_id.clone(),
            parent_id.clone(),
            parent_doc_id.clone(),
            tool_id.clone(),
            tool_doc_id.clone(),
            0,
            agent_did.into(),
            "general".into(),
            format!("prompt for {tool_id}"),
            Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
        )
        .await
        .unwrap();
        assert!(!child.is_empty());
        complete_child(
            &node,
            child_id,
            agent_did,
            &format!("child {} complete", index + 1),
        )
        .await;
    }
    for (child_id, _, _) in &children {
        assert!(matches!(
            project_background_subagent_completion(node.clone(), child_id, agent_did)
                .await
                .unwrap(),
            BackgroundCompletionOutcome::Projected { .. }
        ));
    }
    let post = queue_snapshot(node.as_ref(), &session_id, queue_key).await;
    assert!(
        case.post_pending_request_ids.is_empty(),
        "{} generated notification must add no modeled request",
        case.name
    );
    assert_eq!(
        post.0.len(),
        1,
        "two child completions must enqueue one wake request"
    );
    assert_eq!(
        post.1, case.post_coalesced_pending_count,
        "{} coalesced count drifted",
        case.name
    );
    node.shutdown().await;
    std::fs::remove_dir_all(path).unwrap();
}

#[tokio::test]
async fn subagent_liveness_sweep_ignores_foreign_did_children() {
    let dir = tempfile::tempdir().unwrap();
    let local_identity =
        crate::KeyIdentity::load_or_create(dir.path().join("local.key"), None).unwrap();
    let foreign_identity =
        crate::KeyIdentity::load_or_create(dir.path().join("foreign.key"), None).unwrap();
    let local_did = local_identity.did();
    let foreign_did = foreign_identity.did();
    assert_ne!(local_did, foreign_did);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path())
            .with_node_identity_did(local_did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let mut children = Vec::new();
    for (index, agent_did) in [local_did, foreign_did].into_iter().enumerate() {
        crate::test_support::install_test_behavior(node.as_ref(), agent_did, "general").await;
        let parent_id = format!("scope-parent-{index}-{}", uuid::Uuid::new_v4());
        let session_id = format!("scope-session-{index}-{}", uuid::Uuid::new_v4());
        let escaped_session = crate::graphql::escape_graphql_string(&session_id);
        let escaped_agent = crate::graphql::escape_graphql_string(agent_did);
        exec(node.as_ref(), &format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{escaped_session}", agent_did: "{escaped_agent}", behavior_id: "general", created_at: "2026-03-23T00:00:00Z" }}) {{ _docID }} }}"#)).await;
        let mut parent = claimed_request(&node, &parent_id, &session_id, agent_did).await;
        let parent_doc_id = parent.request().doc_id.clone();
        let child_id = format!("scope-child-{index}-{}", uuid::Uuid::new_v4());
        let tool_id = format!("scope-bridge-{index}-{}", uuid::Uuid::new_v4());
        let mut bridge = publish_accepted_on_claimed_request(
            node.clone(),
            &mut parent,
            agent_did,
            0,
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            &tool_id,
            serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
            Some(SpawnAdmissionPlan {
                tool_call_id: tool_id.clone(),
                child_request_id: child_id.clone(),
                spawn_target_did: agent_did.to_owned(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Background,
            }),
            AwaitMode::Background,
            CancelPolicy::Cascade,
            true,
        )
        .await
        .unwrap();
        assert!(bridge
            .publish_background_receipt("child started")
            .await
            .unwrap());
        assert!(!bridge
            .publish_background_receipt("child started")
            .await
            .unwrap());
        let bridge_doc_id = bridge.doc_id().unwrap().to_owned();
        assert_eq!(
            bridge.request_doc_id.as_deref(),
            Some(parent_doc_id.as_str())
        );
        let persisted = scope_row(node.as_ref(), "AgentToolCall", &bridge_doc_id).await;
        assert_eq!(persisted["request_doc_id"], parent_doc_id);
        assert_eq!(persisted["agent_did"], agent_did);
        assert_eq!(persisted["child_request_id"], child_id);
        assert_eq!(persisted["lifecycle_state"], "running");
        let child_doc_id = expired_child(
            node.as_ref(),
            agent_did,
            &parent_id,
            &parent_doc_id,
            &tool_id,
            &bridge_doc_id,
            &child_id,
        )
        .await;
        children.push((child_doc_id, bridge_doc_id));
    }
    let foreign_child_before = scope_row(node.as_ref(), "AgentRequest", &children[1].0).await;
    let foreign_bridge_before = scope_row(node.as_ref(), "AgentToolCall", &children[1].1).await;
    let report = ToolCallLifecycle::reconcile_subagent_liveness(&node, local_did)
        .await
        .unwrap();
    assert_eq!(report.expired_children_terminalized, 1);
    assert_eq!(report.bridges_projected, 1);
    assert_eq!(
        state(node.as_ref(), "AgentRequest", &children[0].0).await,
        "dead"
    );
    assert_eq!(
        state(node.as_ref(), "AgentToolCall", &children[0].1).await,
        "failed"
    );
    assert_eq!(
        scope_row(node.as_ref(), "AgentRequest", &children[1].0).await,
        foreign_child_before
    );
    assert_eq!(
        scope_row(node.as_ref(), "AgentToolCall", &children[1].1).await,
        foreign_bridge_before
    );
    node.shutdown().await;
}
