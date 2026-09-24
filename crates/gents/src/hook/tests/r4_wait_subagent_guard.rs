//! Exact accepted-control replacements for the old direct-hook wait fixtures.

use super::r4_subagent_control::{accepted_subagent_hook, skip_json, wait_for_child_materialized};
use super::r4c_private_support::{accepted_call, assert_accepted_control_rows};
use super::*;
use crate::support::fixtures::spawn_subagent_source;

const CHILD: &str = "r4-accepted-control-child";

async fn exact_tool_row(hook: &DefraSessionHook, tool_call_id: &str) -> serde_json::Value {
    let session = crate::graphql::escape_graphql_string(
        &hook.session_id().await.expect("accepted control session"),
    );
    let id = crate::graphql::escape_graphql_string(tool_call_id);
    let response = hook.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_call_id: {{ _eq: "{id}" }} }}, limit: 2) {{ _docID request_doc_id agent_did requester_did lifecycle_state await_mode tool_failure_class child_request_id }} }}"#)).await;
    assert!(
        !response.has_errors(),
        "load exact accepted tool row: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "one exact physical tool row for {tool_call_id}"
    );
    rows[0].clone()
}

async fn persist_child_terminal(
    hook: &DefraSessionHook,
    child_request_id: &str,
    state: &str,
    reason: Option<&str>,
) {
    let child_id = crate::graphql::escape_graphql_string(child_request_id);
    let state = crate::graphql::escape_graphql_string(state);
    let reason = reason
        .map(|value| {
            format!(
                r#", failure_reason: "{}""#,
                crate::graphql::escape_graphql_string(value)
            )
        })
        .unwrap_or_default();
    let response = hook.node.execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{child_id}" }} }}, input: {{ lifecycle_state: "{state}"{reason} }}) {{ _docID }} }}"#)).await;
    assert!(
        !response.has_errors(),
        "terminalize exact child: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.as_ref().unwrap()["update_AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn accepted_background_child(
    label: &str,
    prompt: &str,
    spawn_internal_id: &str,
) -> (
    crate::support::TestDb,
    DefraSessionHook,
    String,
    String,
    crate::support::fixtures::SubagentSourceGuard,
) {
    let (db, hook, session_id) = accepted_subagent_hook(label).await;
    let source = spawn_subagent_source(db.node.clone(), db.node_identity.did(), "general", CHILD);
    let args = json!({ "name": CHILD, "prompt": prompt, "await_mode": "background" }).to_string();
    let receipt =
        skip_json(accepted_call(&hook, "spawn_subagent", None, spawn_internal_id, &args).await);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["await_mode"], "background");
    let child_id = receipt["child_request_id"]
        .as_str()
        .expect("child request id")
        .to_owned();
    wait_for_child_materialized(db.node.as_ref(), &child_id).await;
    let bridge = exact_tool_row(&hook, spawn_internal_id).await;
    assert_eq!(bridge["await_mode"], "background");
    assert_eq!(bridge["lifecycle_state"], "running");
    (db, hook, session_id, child_id, source)
}

#[tokio::test]
async fn wait_subagent_maps_child_terminal_failures_without_lifecycle_row() {
    let cases = [
        (
            "failed",
            "failed",
            "failed",
            Some("child failed reason"),
            "child failed reason",
        ),
        (
            "dead",
            "dead",
            "failed",
            None,
            "child request reached the dead terminal state",
        ),
        (
            "interrupted",
            "interrupted",
            "cancelled",
            None,
            "child request was interrupted",
        ),
        (
            "superseded",
            "superseded",
            "failed",
            None,
            "child request was superseded",
        ),
    ];
    for (child_state, expected_status, expected_bridge_state, reason, expected_error_reason) in
        cases
    {
        let spawn_id = format!("accepted-wait-terminal-spawn-{child_state}");
        let (_db, hook, _session_id, child_id, _source) = accepted_background_child(
            &format!("accepted-wait-terminal-{child_state}"),
            &format!("background child terminal {child_state}"),
            &spawn_id,
        )
        .await;
        let wait_args = json!({ "child_request_id": child_id }).to_string();
        let wait_id = format!("accepted-wait-terminal-control-{child_state}");
        accept_hook_tool_call(&hook, &wait_id, "wait_subagent", &wait_args, None).await;
        let wait_hook = hook.clone();
        let wait_id_for_task = wait_id.clone();
        let wait_task = tokio::spawn(async move {
            wait_hook
                .on_tool_call("wait_subagent", None, &wait_id_for_task, &wait_args)
                .await
        });
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let bridge = exact_tool_row(&hook, &spawn_id).await;
            if bridge["await_mode"] == "foreground" {
                assert_eq!(bridge["lifecycle_state"], "running");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "wait did not foreground bridge"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        persist_child_terminal(&hook, &child_id, child_state, reason).await;
        let action = tokio::time::timeout(std::time::Duration::from_secs(5), wait_task)
            .await
            .expect("wait completes on child terminal")
            .expect("wait task joins");
        let result = skip_json(action);
        assert_eq!(result["ok"], false);
        assert_eq!(result["await_mode"], "foreground");
        assert_eq!(result["status"], expected_status);
        assert_eq!(result["error"]["reason"], expected_error_reason);
        assert_eq!(result["error"]["failure_class"], "external");
        let bridge = exact_tool_row(&hook, &spawn_id).await;
        assert_eq!(bridge["lifecycle_state"], expected_bridge_state);
        if let Some(reason) = reason {
            let session_id = hook.session_id().await.expect("accepted bridge session");
            let output = crate::background_tools::canonical_tool_output(
                hook.node.as_ref(),
                bridge["_docID"].as_str().unwrap(),
                bridge["request_doc_id"].as_str().unwrap(),
                &session_id,
                bridge["agent_did"].as_str().unwrap(),
                bridge["requester_did"].as_str(),
            )
            .await
            .expect("read exact canonical bridge result");
            assert_eq!(output, reason);
            assert_eq!(bridge["tool_failure_class"], "external");
        }
        assert_accepted_control_rows(&hook, "wait_subagent", 1).await;
    }
}

#[tokio::test]
async fn uncorroborated_materialized_child_is_nonretryable_and_remains_listed() {
    let (db, hook, _session_id) = accepted_subagent_hook("accepted-wait-corrupt-child").await;
    let spawn_id = "accepted-wait-corrupt-spawn";
    // Accept the bridge through the real provider publication owner, but do
    // not run SubagentSource. This negative import materializes a child with
    // the bridge's logical IDs but without its immutable physical doc joins.
    let spawn_args =
        json!({ "name": CHILD, "prompt": "corrupt child", "await_mode": "background" }).to_string();
    let receipt =
        skip_json(accepted_call(&hook, "spawn_subagent", None, spawn_id, &spawn_args).await);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["await_mode"], "background");
    let child_id = receipt["child_request_id"]
        .as_str()
        .expect("reserved child id")
        .to_owned();
    let bridge = exact_tool_row(&hook, spawn_id).await;
    assert_eq!(bridge["lifecycle_state"], "running");
    let parent_id = hook
        .state
        .lock()
        .await
        .current_request_id
        .clone()
        .expect("accepted parent id");
    let child = crate::graphql::escape_graphql_string(&child_id);
    let parent_id = crate::graphql::escape_graphql_string(&parent_id);
    let agent = crate::graphql::escape_graphql_string(db.node_identity.did());
    let child_session = format!("accepted-wait-corrupt-child-session-{child}");
    let child_session = crate::graphql::escape_graphql_string(&child_session);
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let deadline = (chrono::Utc::now() + chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let response = hook
        .node
        .execute(&format!(
            r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{child}", purpose: "normal", agent_did: "{agent}", requester_did: "{agent}",
        behavior_id: "{CHILD}", session_id: "{child_session}",
        retry_root_request: "{child}", content: "corrupt child",
        lifecycle_state: "processing", execution_origin: "interactive",
        failure_reason: "", created_at: "{created_at}", deadline: "{deadline}",
        retry_count: 0, max_retries: 3, subagent_depth: 1,
        caused_by_parent_request_id: "{parent_id}",
        caused_by_parent_tool_call_id: "{spawn_id}"
    }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "import malformed physical child row: {:?}",
        response.errors
    );
    let child_doc = crate::graphql::single_mutation_document(&response, "create_AgentRequest")
        .expect("normalize child creation response")
        .expect("one uncorroborated physical child");
    assert!(
        child_doc["_docID"].is_string(),
        "uncorroborated child must have a physical document ID: {child_doc:?}"
    );

    let wait_args = json!({ "child_request_id": child_id }).to_string();
    let wait_error = skip_json(
        accepted_call(
            &hook,
            "wait_subagent",
            None,
            "accepted-wait-corrupt",
            &wait_args,
        )
        .await,
    );
    assert_eq!(wait_error["ok"], false);
    assert_eq!(wait_error["failure_class"], "service_unavailable");
    assert_eq!(wait_error["retryable"], false);
    let message = wait_error["message"].as_str().expect("wait diagnostic");
    assert!(
        message.contains("does not corroborate") && message.contains("accepted-wait-corrupt-spawn"),
        "rejects corrupt physical provenance: {message}"
    );
    assert!(
        !message.contains("has no materialized row yet"),
        "must not mask corruption as lag"
    );

    let cancel_error = skip_json(
        accepted_call(
            &hook,
            "cancel_subagent",
            None,
            "accepted-cancel-corrupt",
            &wait_args,
        )
        .await,
    );
    assert_eq!(cancel_error["ok"], false);
    assert_eq!(cancel_error["failure_class"], "service_unavailable");
    assert_eq!(cancel_error["retryable"], false);
    assert!(cancel_error["message"]
        .as_str()
        .is_some_and(|m| m.contains("does not corroborate")));

    let steer_args =
        json!({ "child_request_id": child_id, "message": "do not retry rejected lineage" })
            .to_string();
    let steer_error = skip_json(
        accepted_call(
            &hook,
            "steer_subagent",
            None,
            "accepted-steer-corrupt",
            &steer_args,
        )
        .await,
    );
    assert_eq!(steer_error["ok"], false);
    assert_eq!(steer_error["failure_class"], "service_unavailable");
    assert_eq!(steer_error["retryable"], false);
    assert!(steer_error["message"]
        .as_str()
        .is_some_and(|m| m.contains("does not corroborate")));

    let listed = skip_json(
        accepted_call(&hook, "list_subagents", None, "accepted-list-corrupt", "{}").await,
    );
    let entries = listed["entries"].as_array().expect("list entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["child_request_id"], child_id);
    assert_eq!(entries[0]["status"], "pending_child_authorization");
    for name in [
        "wait_subagent",
        "cancel_subagent",
        "steer_subagent",
        "list_subagents",
    ] {
        assert_accepted_control_rows(&hook, name, 1).await;
    }
}
