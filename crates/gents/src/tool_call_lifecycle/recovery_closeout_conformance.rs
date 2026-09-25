//! Recovery conformance after real native tool and subagent admissions. Faults
//! alter only recovery observations; assistant headers and tool rows come from
//! the production stream owner.

use crate::identity::AgentIdentity;
use crate::streaming::SpawnAdmissionPlan;
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use std::sync::Arc;

const REMOTE_DID: &str = "did:test:remote-host";

async fn accepted_remote_bridge(
    name: &str,
    await_mode: AwaitMode,
) -> (
    Arc<crate::defra_node::EmbeddedNode>,
    std::path::PathBuf,
    ToolCallLifecycle,
    String,
) {
    let path =
        std::env::temp_dir().join(format!("recovery-remote-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create exact remote recovery fixture directory");
    let identity = crate::KeyIdentity::load_or_create(path.join("coordinator.key"), None)
        .expect("create coordinator identity");
    let agent_did = identity.did().to_owned();
    let node = Arc::new(
        crate::defra_node::EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(&agent_did)
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    // The delegated-input gate permits the coordinator's actual node DID to
    // reconstruct its own accepted parent output; a foreign host cannot.
    assert_eq!(node.node_identity_did(), Some(agent_did.as_str()));
    let mut parent = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request(
        &node,
        &format!("request-{name}"),
        &format!("session-{name}"),
        &identity,
        None,
    )
    .await;
    let background = matches!(await_mode, AwaitMode::Background);
    let await_name = if background {
        "background"
    } else {
        "foreground"
    };
    let mut tool =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(),
            &mut parent,
            &agent_did,
            0,
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            "remote-native-tool",
            serde_json::json!({"name":"child", "prompt":"work", "await_mode":await_name}),
            Some(SpawnAdmissionPlan {
                tool_call_id: "remote-native-tool".into(),
                child_request_id: format!("child-{name}"),
                spawn_target_did: REMOTE_DID.into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: await_mode.clone(),
            }),
            await_mode,
            CancelPolicy::Cascade,
            true,
        )
        .await
        .expect("publish accepted remote spawn");
    if background {
        tool.publish_background_receipt("child started")
            .await
            .expect("publish immutable remote bridge receipt");
    }
    (node, path, tool, agent_did)
}

/// Fixture writes to a `@branchable` collection also advance its collection
/// head, so two back-to-back writes can conflict; retry those, bounded.
async fn execute_fixture_write(node: &crate::defra_node::EmbeddedNode, mutation: &str) {
    let mut backoff = std::time::Duration::from_millis(5);
    for _ in 0..8 {
        let result = node.execute(mutation).await;
        let conflicted = result.errors.iter().any(|error| {
            error
                .extensions
                .as_ref()
                .is_some_and(|extensions| extensions.code == "TXN_CONFLICT")
        });
        if !conflicted {
            assert!(!result.has_errors(), "{:?}", result.errors);
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_millis(200));
    }
    panic!("fixture write kept conflicting: {mutation}");
}

async fn update(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

async fn update_request(node: &crate::defra_node::EmbeddedNode, doc_id: &str, fields: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    execute_fixture_write(node, &format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#
    ))
    .await;
}

async fn remove_parent(node: &crate::defra_node::EmbeddedNode, doc_id: &str) {
    let doc_id = crate::graphql::escape_graphql_string(doc_id);
    let result = node
        .execute(&format!(
            r#"mutation {{ delete_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!result.has_errors(), "{:?}", result.errors);
}

async fn completion_obligations(
    node: &crate::defra_node::EmbeddedNode,
    session_id: &str,
    agent_did: &str,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ _docID }} AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ input }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let mut notifications = Vec::new();
    for row in data["AgentMessage"].as_array().unwrap() {
        let (_, message) = crate::session::load_canonical_message_from_node(
            node,
            row["_docID"].as_str().unwrap(),
            agent_did,
            Some(agent_did),
        )
        .await
        .expect("reconstruct canonical recovery notification");
        if let crate::llm::message::Message::User { content } = message {
            for item in content {
                if let crate::llm::message::UserContent::Text(text) = item {
                    if text.text.contains("<tool-completion") {
                        notifications.push(text.text);
                    }
                }
            }
        }
    }
    let wakes = data["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["input"].clone())
        .collect();
    (notifications, wakes)
}

async fn claim_child(
    node: &Arc<crate::defra_node::EmbeddedNode>,
    child_request_id: &str,
    agent_did: &str,
    processing: bool,
) -> String {
    let child = crate::graphql::escape_graphql_string(child_request_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}, limit: 2) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .unwrap();
    let mut owner = crate::lifecycle::RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        agent_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(
        owner.claim().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    if processing {
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            agent_did,
            std::time::Duration::ZERO,
        );
        owner.begin_owned_execution(&writer).await.unwrap();
    }
    owner.request().doc_id.clone()
}

#[tokio::test]
async fn generated_missing_parent_and_unclaimed_restart_cases_use_accepted_spawn() {
    let cases = crate::lean_vocab_test::lean_restart_disposition_cases();
    for name in [
        "restart_subagent_missing_parent_left_running",
        "restart_unclaimed_spawn_expired_fails",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert!(case.child_linked, "{name}");
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            cancel_policy: CancelPolicy::Cascade,
            spawn_plan: Some(SpawnAdmissionPlan {
                tool_call_id: "bridge-native-tool".into(),
                child_request_id: format!("child-{name}"),
                spawn_target_did: "overridden-by-fixture".into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Background,
            }),
            ..Default::default()
        })
        .await
        .expect("publish accepted restart bridge");
        admission
            .tool
            .publish_background_receipt("child started")
            .await
            .expect("publish immutable restart bridge receipt");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        if case.parent_observation == "missing" {
            // Simulate a missing parent replica after accepted publication.
            // Only the fixture's exact request document is removed.
            remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        }
        if case.unclaimed_expired {
            update(
                &admission.node,
                &tool_doc_id,
                r#"unclaimed_deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let result = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .expect("recover accepted restart bridge");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
        assert!(admission.tool.accepted_header_doc_id().is_some(), "{name}");
        match case.disposition.as_str() {
            "leave_running" => {
                assert_eq!(result.tool_calls_recovered, 0, "{name}");
                assert_eq!(row["lifecycle_state"], "running", "{name}");
                assert!(row["cancel_cause"].is_null(), "{name}");
                assert!(row["tool_failure_class"].is_null(), "{name}");
            }
            "terminalize" => {
                assert_eq!(result.tool_calls_recovered, 1, "{name}");
                assert_eq!(
                    row["lifecycle_state"],
                    case.terminal_state.as_deref().unwrap(),
                    "{name}"
                );
                assert_eq!(row["tool_failure_class"], "serviceUnavailable", "{name}");
            }
            other => panic!("unexpected restart disposition {other}"),
        }
        let second = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .expect("repeat restart recovery");
        assert_eq!(second.tool_calls_recovered, 0, "{name}");
        let session_id = format!("session-restart-closeout-{name}");
        let (notifications, wakes) =
            completion_obligations(&admission.node, &session_id, &admission.agent_did).await;
        assert!(notifications.is_empty(), "{name}: no restart notification");
        assert!(wakes.is_empty(), "{name}: no scheduled wake is due");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn generated_native_missing_parent_restart_cases_defer() {
    let cases = crate::lean_vocab_test::lean_restart_disposition_cases();
    for name in [
        "restart_native_background_expired_missing_parent_deferred",
        "restart_unclaimed_missing_parent_deferred",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.disposition, "leave_running", "{name}");
        assert_eq!(case.parent_observation, "missing", "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("restart-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted native restart call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        let field = if case.deadline_expired {
            r#"deadline_at: "2020-01-01T00:00:00Z""#
        } else {
            assert!(case.unclaimed_expired, "{name}");
            r#"unclaimed_deadline_at: "2020-01-01T00:00:00Z""#
        };
        update(&admission.node, &tool_doc_id, field).await;
        let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
            .await
            .unwrap();
        assert_eq!(report.tool_calls_recovered, 0, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#)).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running",
            "{name}"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn generated_linked_recovery_cases_use_accepted_spawn() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "tool_running_child_completed_to_completed",
        "tool_running_child_failed_to_failed",
        "tool_running_child_dead_to_failed",
        "tool_running_child_interrupted_to_cancelled",
        "detached_bridge_child_completed_to_completed",
        "detached_bridge_child_failed_to_failed",
        "detached_bridge_child_interrupted_to_cancelled",
        "detached_bridge_terminal_parent_to_failed",
        "detached_bridge_deadline_exceeded_to_timed_out",
        "live_detached_bridge_parent_failed_to_failed",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        let fixture_name = format!("recovery-closeout-{name}");
        let child_request_id = format!("child-{fixture_name}");
        let detached = name.starts_with("detached_") || name.starts_with("live_detached_");
        let await_mode = if detached {
            AwaitMode::Background
        } else {
            AwaitMode::Foreground
        };
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: fixture_name.clone(),
            real_identity: true,
            await_mode: await_mode.clone(),
            cancel_policy: if detached {
                CancelPolicy::Detach
            } else {
                CancelPolicy::Cascade
            },
            spawn_plan: Some(SpawnAdmissionPlan {
                tool_call_id: "bridge-native-tool".into(),
                child_request_id: child_request_id.clone(),
                spawn_target_did: "overridden-by-fixture".into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode,
            }),
            ..Default::default()
        })
        .await
        .expect("publish accepted recovery bridge");
        crate::test_support::install_test_behavior(
            &admission.node,
            &admission.agent_did,
            "general",
        )
        .await;
        if detached {
            admission
                .tool
                .publish_background_receipt("child started")
                .await
                .expect("publish immutable detached bridge receipt");
        }
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let parent_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        let parent_request_id = format!("request-{fixture_name}");
        crate::tool_call_lifecycle::create_subagent_request_with_request_id(
            &admission.node,
            child_request_id.clone(),
            parent_request_id,
            parent_doc_id.clone(),
            admission.tool.tool_call_id().to_owned(),
            tool_doc_id.clone(),
            0,
            admission.agent_did.clone(),
            "general".into(),
            "child work".into(),
            Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
        )
        .await
        .expect("materialize exact accepted child");
        let child_escaped = crate::graphql::escape_graphql_string(&child_request_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_escaped}" }} }}, limit: 2) {{ _docID }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let children = response.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(children.len(), 1, "{name}: exact physical child");
        let child_doc_id = children[0]["_docID"].as_str().unwrap();
        if name.contains("child_completed") {
            crate::tool_call_lifecycle::admission_fixture::complete_child(
                &admission.node,
                &child_request_id,
                &admission.agent_did,
                "done",
            )
            .await;
        } else if name.contains("child_failed") {
            update_request(
                &admission.node,
                child_doc_id,
                r#"lifecycle_state: "failed""#,
            )
            .await;
        } else if name.contains("child_dead") {
            update_request(&admission.node, child_doc_id, r#"lifecycle_state: "dead""#).await;
        } else if name.contains("child_interrupted") {
            update_request(
                &admission.node,
                child_doc_id,
                r#"lifecycle_state: "interrupted""#,
            )
            .await;
        }
        if name.contains("terminal_parent") || name.contains("parent_failed") {
            update_request(
                &admission.node,
                &parent_doc_id,
                r#"lifecycle_state: "failed""#,
            )
            .await;
        }
        if name.contains("deadline_exceeded") {
            update(
                &admission.node,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if case.sweep_id == "tool_call_lifecycle_reconcile_terminal_parent_owned_tools" {
            let report = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
                &admission.node,
                &admission.agent_did,
            )
            .await
            .unwrap();
            assert_eq!(report.tool_calls_terminalized, 1, "{name}");
        } else {
            let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
                .await
                .unwrap();
            assert_eq!(report.tool_calls_recovered, 1, "{name}");
        }
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        assert_eq!(row["status"], "completed", "{name}");
        if case.terminal_state == "timedOut" {
            assert_eq!(row["cancel_cause"], "deadline", "{name}");
            assert_eq!(row["tool_failure_class"], "external", "{name}");
        } else if case.terminal_state == "cancelled" {
            assert_eq!(row["cancel_cause"], "interrupted", "{name}");
        }
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn generated_expired_child_liveness_cases_use_accepted_bridge() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "expired_processing_child_to_dead",
        "expired_claimed_child_to_dead",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        let fixture_name = format!("recovery-closeout-{name}");
        let child_request_id = format!("child-{fixture_name}");
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: fixture_name.clone(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            spawn_plan: Some(SpawnAdmissionPlan {
                tool_call_id: "bridge-native-tool".into(),
                child_request_id: child_request_id.clone(),
                spawn_target_did: "overridden-by-fixture".into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Background,
            }),
            ..Default::default()
        })
        .await
        .expect("publish accepted liveness bridge");
        crate::test_support::install_test_behavior(
            &admission.node,
            &admission.agent_did,
            "general",
        )
        .await;
        admission
            .tool
            .publish_background_receipt("child started")
            .await
            .expect("publish immutable liveness bridge receipt");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        crate::tool_call_lifecycle::create_subagent_request_with_request_id(
            &admission.node,
            child_request_id.clone(),
            format!("request-{fixture_name}"),
            admission.tool.request_doc_id().unwrap().to_owned(),
            admission.tool.tool_call_id().to_owned(),
            tool_doc_id.clone(),
            0,
            admission.agent_did.clone(),
            "general".into(),
            "work".into(),
            Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
        )
        .await
        .unwrap();
        let child_doc_id = claim_child(
            &admission.node,
            &child_request_id,
            &admission.agent_did,
            case.pre_state == "processing",
        )
        .await;
        update_request(
            &admission.node,
            &child_doc_id,
            r#"deadline: "2020-01-01T00:00:00Z""#,
        )
        .await;
        let report =
            ToolCallLifecycle::reconcile_subagent_liveness(&admission.node, &admission.agent_did)
                .await
                .unwrap();
        assert_eq!(report.expired_children_terminalized, 1, "{name}");
        assert_eq!(report.bridges_projected, 1, "{name}");
        let child = crate::graphql::escape_graphql_string(&child_request_id);
        let tool_doc = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}) {{ lifecycle_state }} AgentToolCall(filter: {{ _docID: {{ _eq: "{tool_doc}" }} }}) {{ lifecycle_state }} }}"#)).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let data = response.data.unwrap();
        assert_eq!(
            data["AgentRequest"][0]["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        assert_eq!(
            data["AgentToolCall"][0]["lifecycle_state"], "failed",
            "{name}"
        );
        let second =
            ToolCallLifecycle::reconcile_subagent_liveness(&admission.node, &admission.agent_did)
                .await
                .unwrap();
        assert!(second.is_noop(), "{name}");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn liveness_convergence_projects_two_accepted_bridges_and_releases_queued_child() {
    let fixture_name = "recovery-closeout-liveness-convergence";
    let child_ids = [
        format!("child-{fixture_name}-1"),
        format!("child-{fixture_name}-2"),
    ];
    let (mut admission, mut parent) =
        crate::tool_call_lifecycle::admission_fixture::published_admission_with_owner(
            PublishedAdmissionOptions {
                name: fixture_name.into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                spawn_plan: Some(SpawnAdmissionPlan {
                    tool_call_id: "bridge-native-tool".into(),
                    child_request_id: child_ids[0].clone(),
                    spawn_target_did: "overridden-by-fixture".into(),
                    spawn_behavior_id: "general".into(),
                    delegated_workspace: None,
                    await_mode: AwaitMode::Background,
                }),
                ..Default::default()
            },
        )
        .await
        .expect("publish first accepted liveness bridge");
    crate::test_support::install_test_behavior(&admission.node, &admission.agent_did, "general")
        .await;
    admission
        .tool
        .publish_background_receipt("child one started")
        .await
        .unwrap();
    let mut second =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            admission.node.clone(),
            &mut parent,
            &admission.agent_did,
            1,
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            "bridge-second",
            serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
            Some(SpawnAdmissionPlan {
                tool_call_id: "bridge-second".into(),
                child_request_id: child_ids[1].clone(),
                spawn_target_did: admission.agent_did.clone(),
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
    second
        .publish_background_receipt("child two started")
        .await
        .unwrap();
    let bridges = [&admission.tool, &second];
    for (index, bridge) in bridges.into_iter().enumerate() {
        let child_request_id = &child_ids[index];
        crate::tool_call_lifecycle::create_subagent_request_with_request_id(
            &admission.node,
            child_request_id.clone(),
            format!("request-{fixture_name}"),
            bridge.request_doc_id().unwrap().to_owned(),
            bridge.tool_call_id().to_owned(),
            bridge.doc_id().unwrap().to_owned(),
            0,
            admission.agent_did.clone(),
            "general".into(),
            "work".into(),
            Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
        )
        .await
        .unwrap();
        let child_doc = claim_child(
            &admission.node,
            child_request_id,
            &admission.agent_did,
            true,
        )
        .await;
        update_request(
            &admission.node,
            &child_doc,
            r#"deadline: "2020-01-01T00:00:00Z""#,
        )
        .await;
    }
    let queued_parent_id = format!("request-{fixture_name}-queued");
    let mut queued_parent = crate::tool_call_lifecycle::admission_fixture::claimed_request(
        &admission.node,
        &queued_parent_id,
        &format!("session-{fixture_name}-queued"),
        &admission.agent_did,
    )
    .await;
    let queued_child_id = format!("child-{fixture_name}-queued");
    let mut queued_bridge =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            admission.node.clone(),
            &mut queued_parent,
            &admission.agent_did,
            0,
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            "bridge-queued",
            serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
            Some(SpawnAdmissionPlan {
                tool_call_id: "bridge-queued".into(),
                child_request_id: queued_child_id.clone(),
                spawn_target_did: admission.agent_did.clone(),
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
    queued_bridge
        .publish_background_receipt("queued child accepted")
        .await
        .unwrap();
    crate::tool_call_lifecycle::create_subagent_request_with_request_id(
        &admission.node,
        queued_child_id.clone(),
        queued_parent_id,
        queued_bridge.request_doc_id().unwrap().to_owned(),
        queued_bridge.tool_call_id().to_owned(),
        queued_bridge.doc_id().unwrap().to_owned(),
        0,
        admission.agent_did.clone(),
        "general".into(),
        "work".into(),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .unwrap();
    update_request(
        &admission.node,
        queued_bridge.request_doc_id().unwrap(),
        r#"lifecycle_state: "completed""#,
    )
    .await;
    let report =
        ToolCallLifecycle::reconcile_subagent_liveness(&admission.node, &admission.agent_did)
            .await
            .unwrap();
    assert_eq!(report.expired_children_terminalized, 2);
    assert_eq!(report.bridges_projected, 2);
    assert_eq!(report.queued_descendants_interrupted, 1);
    for bridge in [&admission.tool, &second] {
        let tool = crate::graphql::escape_graphql_string(bridge.doc_id().unwrap());
        let response = admission.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool}" }} }}) {{ lifecycle_state }} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "failed"
        );
    }
    let child = crate::graphql::escape_graphql_string(&queued_child_id);
    let response = admission.node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}) {{ lifecycle_state }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        response.data.unwrap()["AgentRequest"][0]["lifecycle_state"],
        "interrupted"
    );
    let second_report =
        ToolCallLifecycle::reconcile_subagent_liveness(&admission.node, &admission.agent_did)
            .await
            .unwrap();
    assert!(second_report.is_noop());
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).expect("remove exact liveness fixture");
}

#[tokio::test]
async fn generated_queued_descendant_case_releases_local_and_foreign_parent_children() {
    let case = crate::lean_vocab_test::lean_recovery_sweep_cases()
        .into_iter()
        .find(|case| case.name == "queued_descendant_terminal_parent_to_interrupted")
        .unwrap();
    let fixture_name = "recovery-closeout-queued-descendants";
    let local_child_id = format!("child-{fixture_name}-local");
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: fixture_name.into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        spawn_plan: Some(SpawnAdmissionPlan {
            tool_call_id: "bridge-native-tool".into(),
            child_request_id: local_child_id.clone(),
            spawn_target_did: "overridden-by-fixture".into(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        ..Default::default()
    })
    .await
    .expect("publish accepted local parent bridge");
    crate::test_support::install_test_behavior(&admission.node, &admission.agent_did, "general")
        .await;
    admission
        .tool
        .publish_background_receipt("local child accepted")
        .await
        .unwrap();
    let local_parent_doc = admission.tool.request_doc_id().unwrap().to_owned();
    crate::tool_call_lifecycle::create_subagent_request_with_request_id(
        &admission.node,
        local_child_id.clone(),
        format!("request-{fixture_name}"),
        local_parent_doc.clone(),
        admission.tool.tool_call_id().to_owned(),
        admission.tool.doc_id().unwrap().to_owned(),
        0,
        admission.agent_did.clone(),
        "general".into(),
        "work".into(),
        None,
    )
    .await
    .unwrap();
    update_request(
        &admission.node,
        &local_parent_doc,
        r#"lifecycle_state: "completed""#,
    )
    .await;
    let bystander_id = format!("wake-{fixture_name}");
    let bystander = crate::graphql::escape_graphql_string(&bystander_id);
    let local_did = crate::graphql::escape_graphql_string(&admission.agent_did);
    let local_session = crate::graphql::escape_graphql_string(&format!("session-{fixture_name}"));
    let local_parent = crate::graphql::escape_graphql_string(&format!("request-{fixture_name}"));
    let local_tool = crate::graphql::escape_graphql_string(admission.tool.tool_call_id());
    let response = admission.node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{bystander}", agent_did: "{local_did}", behavior_id: "general", session_id: "{local_session}", retry_parent_request: "", retry_root_request: "{bystander}", superseded_by_request: "", content: "wake", lifecycle_state: "pending", backend_id: "", execution_origin: "scheduled", created_at: "2026-03-23T00:00:00Z", retry_count: 0, max_retries: 3, subagent_depth: 1, caused_by_parent_request_id: "{local_parent}", caused_by_parent_tool_call_id: "{local_tool}" }}) {{ _docID }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let foreign_identity =
        crate::KeyIdentity::load_or_create(admission.path.join("foreign-parent.key"), None)
            .unwrap();
    crate::test_support::install_test_behavior(&admission.node, foreign_identity.did(), "general")
        .await;
    let foreign_parent_id = format!("request-{fixture_name}-foreign");
    let mut foreign_parent = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request(
        &admission.node,
        &foreign_parent_id,
        &format!("session-{fixture_name}-foreign"),
        &foreign_identity,
        None,
    )
    .await;
    let foreign_child_id = format!("child-{fixture_name}-foreign");
    let mut foreign_bridge =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            admission.node.clone(),
            &mut foreign_parent,
            foreign_identity.did(),
            0,
            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
            "bridge-foreign",
            serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
            Some(SpawnAdmissionPlan {
                tool_call_id: "bridge-foreign".into(),
                child_request_id: foreign_child_id.clone(),
                spawn_target_did: admission.agent_did.clone(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Background,
            }),
            AwaitMode::Background,
            CancelPolicy::Cascade,
            true,
        )
        .await
        .expect("publish foreign principal's accepted bridge");
    // This bridge is still running; queued-descendant repair only needs its
    // accepted edge and the foreign parent's terminal observation.
    let foreign_parent_doc = foreign_bridge.request_doc_id().unwrap().to_owned();
    crate::tool_call_lifecycle::create_subagent_request_with_trusted_parent_request_id(
        &admission.node,
        foreign_child_id.clone(),
        foreign_parent_id,
        foreign_parent_doc.clone(),
        foreign_bridge.tool_call_id().to_owned(),
        foreign_bridge.doc_id().unwrap().to_owned(),
        0,
        admission.agent_did.clone(),
        "general".into(),
        "work".into(),
        None,
        foreign_identity.did().to_owned(),
    )
    .await
    .unwrap();
    update_request(
        &admission.node,
        &foreign_parent_doc,
        r#"lifecycle_state: "completed""#,
    )
    .await;

    let report =
        ToolCallLifecycle::reconcile_subagent_liveness(&admission.node, &admission.agent_did)
            .await
            .unwrap();
    assert_eq!(report.queued_descendants_interrupted, 2);
    for child_id in [&local_child_id, &foreign_child_id] {
        let child = crate::graphql::escape_graphql_string(child_id);
        let response = admission.node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}) {{ lifecycle_state }} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentRequest"][0]["lifecycle_state"],
            case.terminal_state.as_str()
        );
    }
    let response = admission.node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{bystander}" }} }}) {{ lifecycle_state }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        response.data.unwrap()["AgentRequest"][0]["lifecycle_state"],
        "pending"
    );
    let second =
        ToolCallLifecycle::reconcile_subagent_liveness(&admission.node, &admission.agent_did)
            .await
            .unwrap();
    assert!(second.is_noop());
    drop(foreign_bridge);
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).expect("remove exact queued-descendant fixture");
}

#[tokio::test]
async fn generated_orphan_background_recovery_cases_use_accepted_native_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "orphaned_background_tool_without_execution_to_cancelled",
        "orphaned_background_tool_expired_terminal_parent_to_timed_out",
        "orphaned_background_tool_unclaimed_to_failed",
        "orphaned_background_tool_terminal_parent_to_failed",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.execution_registered, Some(false), "{name}");
        let fixture_name = format!("recovery-closeout-{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: fixture_name.clone(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted orphan background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let parent_doc_id = admission.tool.request_doc_id().unwrap().to_owned();
        if case.parent_terminal == Some(true) {
            update_request(
                &admission.node,
                &parent_doc_id,
                r#"lifecycle_state: "completed""#,
            )
            .await;
        }
        if case.deadline_expired == Some(true) {
            update(
                &admission.node,
                &tool_doc_id,
                r#"deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        if case.unclaimed_expired == Some(true) {
            update(
                &admission.node,
                &tool_doc_id,
                r#"unclaimed_deadline_at: "2020-01-01T00:00:00Z""#,
            )
            .await;
        }
        let registry = crate::BackgroundExecutionRegistry::default();
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{ status lifecycle_state cancel_cause tool_failure_class }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let row = &response.data.unwrap()["AgentToolCall"][0];
        assert_eq!(row["status"], "completed", "{name}");
        assert_eq!(
            row["lifecycle_state"],
            case.terminal_state.as_str(),
            "{name}"
        );
        match case.recovery_cause.as_deref() {
            Some("deadlineExceeded" | "parentTerminal") => {
                assert_eq!(row["tool_failure_class"], "external", "{name}");
            }
            Some("unclaimedCrossPrincipalSpawn") => {
                assert_eq!(row["tool_failure_class"], "serviceUnavailable", "{name}");
            }
            Some("TerminalizeBackgroundedAsInterrupted" | "parentInterrupted") => {
                assert_eq!(row["cancel_cause"], "interrupted", "{name}");
                assert!(row["tool_failure_class"].is_null(), "{name}");
            }
            other => panic!("{name}: unsupported Lean recovery cause {other:?}"),
        }
        let session_id = format!("session-{fixture_name}");
        let (notifications, wakes) =
            completion_obligations(&admission.node, &session_id, &admission.agent_did).await;
        if let Some(reason) = case.notification_reason.as_deref() {
            assert_eq!(notifications.len(), 1, "{name}");
            assert!(
                notifications[0].contains(&format!("<reason>{reason}</reason>")),
                "{name}"
            );
            assert_eq!(wakes.len(), 1, "{name}");
        } else {
            assert!(notifications.is_empty(), "{name}");
            assert!(wakes.is_empty(), "{name}");
        }
        let second = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(second.tool_calls_terminalized, 0, "{name}");
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}

#[tokio::test]
async fn generated_missing_parent_deferred_cases_keep_accepted_row_running() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    for name in [
        "orphaned_background_tool_expired_missing_parent_deferred",
        "orphaned_background_tool_unclaimed_missing_parent_deferred",
    ] {
        let case = cases.iter().find(|case| case.name == name).unwrap();
        assert_eq!(case.terminal_state, "running", "{name}");
        assert_eq!((case.measure_before, case.measure_after), (0, 0), "{name}");
        let admission = published_admission(PublishedAdmissionOptions {
            name: format!("recovery-closeout-{name}"),
            real_identity: true,
            await_mode: AwaitMode::Background,
            ..Default::default()
        })
        .await
        .expect("publish accepted native background call");
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        remove_parent(&admission.node, admission.tool.request_doc_id().unwrap()).await;
        let field = if case.deadline_expired == Some(true) {
            r#"deadline_at: "2020-01-01T00:00:00Z""#
        } else {
            assert_eq!(case.unclaimed_expired, Some(true), "{name}");
            r#"unclaimed_deadline_at: "2020-01-01T00:00:00Z""#
        };
        update(&admission.node, &tool_doc_id, field).await;
        let error = match ToolCallLifecycle::load_by_doc_id(
            admission.node.clone(),
            &tool_doc_id,
            &admission.agent_did,
            admission.tool.session_id(),
            admission.tool.requester_did(),
        )
        .await
        {
            Ok(_) => panic!("{name}: missing accepted parent must not authorize rehydration"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("tool owner request is missing or ambiguous"),
            "{name}: unexpected missing-owner guard: {error:#}"
        );
        let registry = crate::BackgroundExecutionRegistry::default();
        let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
            &admission.node,
            &admission.agent_did,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(report.tool_calls_terminalized, 0, "{name}");
        let escaped = crate::graphql::escape_graphql_string(&tool_doc_id);
        let response = admission.node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped}" }} }}) {{ lifecycle_state }} }}"#
        )).await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        assert_eq!(
            response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
            "running",
            "{name}"
        );
        admission.node.shutdown().await;
        std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
    }
}
#[tokio::test]
async fn generated_background_completion_recovery_uses_accepted_native_call() {
    let cases = crate::lean_vocab_test::lean_recovery_sweep_cases();
    let case = cases
        .iter()
        .find(|case| {
            case.name == "terminal_background_tool_missing_completion_side_effects_to_converged"
        })
        .unwrap();
    let fixture_name = "recovery-closeout-background-completion";
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: fixture_name.into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted background call");
    admission
        .tool
        .bridge_failure(crate::tool_call_lifecycle::ChildTerminal::Failed {
            reason: "seed terminal background failure".into(),
            failure_class: crate::tool_call_lifecycle::FailureClass::External,
        })
        .await
        .unwrap();
    let report = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.agent_did,
    )
    .await
    .unwrap();
    assert_eq!(report.side_effects_converged, 1, "{}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.agent_did,
    )
    .await
    .unwrap();
    assert!(second.is_noop(), "{}", case.name);
    let session_id = format!("session-{fixture_name}");
    let (notifications, wakes) =
        completion_obligations(&admission.node, &session_id, &admission.agent_did).await;
    assert_eq!(notifications.len(), 1, "{}", case.name);
    assert_eq!(wakes.len(), 1, "{}", case.name);
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).expect("remove exact recovery fixture");
}

#[tokio::test]
async fn generated_unclaimed_remote_spawn_uses_accepted_bridge() {
    let case = crate::lean_vocab_test::lean_recovery_sweep_cases()
        .into_iter()
        .find(|case| case.name == "tool_running_unclaimed_cross_principal_spawn_to_failed")
        .unwrap();
    let (node, path, tool, agent_did) =
        accepted_remote_bridge("unclaimed-cross-principal", AwaitMode::Background).await;
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    update(
        &node,
        &tool_doc_id,
        r#"unclaimed_deadline_at: "2020-01-01T00:00:00Z""#,
    )
    .await;
    assert!(ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &tool_doc_id,
        &agent_did,
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .expect("rehydrate accepted remote spawn")
    .is_some());
    let report = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1, "{report:?}");
    let tool_doc_id = crate::graphql::escape_graphql_string(&tool_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{tool_doc_id}" }} }}) {{ lifecycle_state status tool_failure_class cancel_cascade_intent_at }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = &response.data.unwrap()["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], case.terminal_state.as_str());
    assert_eq!(row["status"], "completed");
    assert_eq!(row["tool_failure_class"], "serviceUnavailable");
    assert!(
        row["cancel_cascade_intent_at"].is_null(),
        "unclaimed expiry must suppress cascade"
    );
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact remote recovery fixture");
}

#[tokio::test]
async fn recovery_cascade_writes_remote_intent_from_accepted_bridge() {
    let name = "remote-cascade";
    let (node, path, tool, agent_did) = accepted_remote_bridge(name, AwaitMode::Foreground).await;
    let parent_doc_id = tool.request_doc_id().unwrap().to_owned();
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    let child_request_id = format!("child-{name}");
    let child_session_id = format!("session-child-{name}");
    let child_session = crate::graphql::escape_graphql_string(&child_session_id);
    let remote_did = crate::graphql::escape_graphql_string(REMOTE_DID);
    let session = node
        .execute(&format!(
            r#"mutation {{ create_AgentSession(input: {{ session_id: "{child_session}", agent_did: "{remote_did}", behavior_id: "general", created_at: "2026-03-23T00:00:00Z" }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(!session.has_errors(), "{:?}", session.errors);
    let child = crate::graphql::escape_graphql_string(&child_request_id);
    let parent_request_id = crate::graphql::escape_graphql_string(&format!("request-{name}"));
    let parent_doc = crate::graphql::escape_graphql_string(&parent_doc_id);
    let tool_call_id = crate::graphql::escape_graphql_string(tool.tool_call_id());
    let tool_doc = crate::graphql::escape_graphql_string(&tool_doc_id);
    let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{child}", agent_did: "{remote_did}", behavior_id: "general", session_id: "{child_session}", retry_parent_request: "", retry_root_request: "{child}", superseded_by_request: "", content: "work", lifecycle_state: "processing", backend_id: "", execution_origin: "interactive", created_at: "2026-03-23T00:00:00Z", retry_count: 0, max_retries: 3, subagent_depth: 1, caused_by_parent_request_id: "{parent_request_id}", caused_by_parent_request_doc_id: "{parent_doc}", caused_by_parent_tool_call_id: "{tool_call_id}", caused_by_parent_tool_call_doc_id: "{tool_doc}" }}) {{ _docID }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    update_request(&node, &parent_doc_id, r#"lifecycle_state: "interrupted""#).await;
    assert!(ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &tool_doc_id,
        &agent_did,
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .expect("rehydrate accepted remote cascade bridge")
    .is_some());
    let report = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .unwrap();
    assert_eq!(report.tool_calls_recovered, 1, "{report:?}");
    let escaped_tool = crate::graphql::escape_graphql_string(&tool_doc_id);
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped_tool}" }} }}) {{ lifecycle_state cancel_cause tool_failure_class cancel_cascade_intent_at cancel_pending_remote_ack }} AgentRequest(filter: {{ request_id: {{ _eq: "{child}" }} }}) {{ lifecycle_state interrupt_requested_at }} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    let bridge = &data["AgentToolCall"][0];
    assert_eq!(bridge["lifecycle_state"], "cancelled");
    assert_eq!(bridge["cancel_cause"], "interrupted");
    assert!(bridge["tool_failure_class"].is_null());
    assert!(bridge["cancel_cascade_intent_at"].is_string());
    assert_eq!(bridge["cancel_pending_remote_ack"], true);
    let child = &data["AgentRequest"][0];
    assert_eq!(child["lifecycle_state"], "processing");
    assert!(child["interrupt_requested_at"].is_null());
    let second = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .unwrap();
    assert_eq!(second.tool_calls_recovered, 0);
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact remote recovery fixture");
}
