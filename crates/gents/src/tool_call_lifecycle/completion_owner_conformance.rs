//! R6 completion-continuation owner: accepted native output, canonical
//! notification reconstruction, and the independent failed-wake redrive.

use super::{
    admission_fixture::{published_admission, PublishedAdmission, PublishedAdmissionOptions},
    AwaitMode, ToolCallLifecycle,
};
use crate::goal::{load_canonical_goal, set_goal, GoalStatus};
use crate::graphql::escape_graphql_string;
use crate::lean_vocab_test::{
    lean_r6_backgrounding_case, lean_r6_backgrounding_cases, LeanR6BackgroundingCase,
};
use defra_node::EmbeddedNode;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use serde_json::{json, Value};

async fn rows(node: &EmbeddedNode, query: &str, field: &str) -> Vec<Value> {
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "{field}: {:?}", response.errors);
    response.data.expect("query data")[field]
        .as_array()
        .expect("rows")
        .clone()
}

async fn update(node: &EmbeddedNode, collection: &str, doc_id: &str, fields: &str) {
    let response = node.execute(&format!(
        r#"mutation {{ update_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#,
        escape_graphql_string(doc_id),
    )).await;
    assert!(
        !response.has_errors(),
        "{collection}: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.unwrap()[&format!("update_{collection}")]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn goal_status(case: &LeanR6BackgroundingCase) -> Option<GoalStatus> {
    case.goal_status.as_deref().map(|status| match status {
        "active" => GoalStatus::Active,
        "paused" => GoalStatus::Paused,
        "blocked" => GoalStatus::Blocked,
        "usage_limited" => GoalStatus::UsageLimited,
        "budget_limited" => GoalStatus::BudgetLimited,
        "complete" => GoalStatus::Complete,
        other => panic!("unknown generated Goal status {other}"),
    })
}

async fn admitted_parent(case: &LeanR6BackgroundingCase, branch: &str) -> PublishedAdmission {
    let redrive = lean_r6_backgrounding_case("failed_background_wake_with_budget_redrives");
    let name = format!(
        "completion-owner-parent-{}-{branch}",
        redrive.pre_parent_request_id.unwrap()
    );
    let admission = published_admission(PublishedAdmissionOptions {
        name,
        real_identity: true,
        await_mode: AwaitMode::Background,
        start_running: true,
        // The failed historical wake must be the actual latest request fact;
        // the redrive owner deliberately ignores a hand-edited session cache.
        request_created_at: (branch == "redrive").then(|| "2026-07-14T00:00:00Z".to_owned()),
        ..Default::default()
    })
    .await
    .expect("publish accepted background invocation on claimed request");
    let parent_doc = admission.tool.request_doc_id().unwrap();
    // The generated completion-owner row begins with a terminal parent.
    // Import that observation only AFTER the real claimed request has
    // published its immutable accepted header; this test exercises the
    // notification/redrive owners, not the request terminal transition.
    update(
        &admission.node,
        "AgentRequest",
        parent_doc,
        r#"lifecycle_state: "completed""#,
    )
    .await;
    let session = admission.tool.session_id.as_str();
    if let Some(status) = goal_status(case) {
        set_goal(
            &admission.node,
            &admission.agent_did,
            session,
            Some("One continuation owner"),
            Some(status),
            Some(Some(10)),
        )
        .await
        .expect("persist modeled Goal state");
    }
    admission
}

async fn notification_texts(admission: &PublishedAdmission) -> Vec<(Value, String)> {
    let query = format!(
        r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID request_doc_id role publication }} }}"#,
        escape_graphql_string(&admission.tool.session_id)
    );
    let headers = rows(&admission.node, &query, "AgentMessage").await;
    let mut notifications = Vec::new();
    for header in headers {
        let doc_id = header["_docID"].as_str().unwrap();
        let (_, native) = crate::session::load_canonical_message_from_node(
            &admission.node,
            doc_id,
            &admission.agent_did,
            Some(&admission.agent_did),
        )
        .await
        .expect("reconstruct exact canonical header");
        if let gents_protocol::message::Message::User { content } = native {
            let text = content
                .into_iter()
                .filter_map(|part| match part {
                    gents_protocol::message::UserContent::Text(text) => Some(text.text),
                    _ => None,
                })
                .collect::<String>();
            if text.contains("durable native output") {
                notifications.push((header, text));
            }
        }
    }
    notifications
}

async fn drive_notification(case: &LeanR6BackgroundingCase) {
    let mut admission = admitted_parent(case, "notification").await;
    let node = &admission.node;
    let did = admission.agent_did.as_str();
    let session = admission.tool.session_id.clone();
    let parent_doc = admission.tool.request_doc_id().unwrap().to_owned();
    let parent_row = rows(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id }} }}"#,
            escape_graphql_string(&parent_doc)
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(parent_row.len(), 1);
    let parent = parent_row[0]["request_id"].as_str().unwrap().to_owned();
    let tool_doc = admission.tool.doc_id().unwrap().to_owned();
    let before = load_canonical_goal(node, did, &session).await.unwrap();
    assert!(admission
        .tool
        .bridge_complete("durable native output".into())
        .await
        .unwrap());
    let pending = rows(node, &format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID status lifecycle_state request_doc_id }} }}"#,
        escape_graphql_string(&tool_doc)), "AgentToolCall").await;
    assert_eq!(pending.len(), 1, "{}", case.name);
    assert_eq!(pending[0]["status"], "completionPending", "{}", case.name);
    assert_eq!(pending[0]["lifecycle_state"], "completed", "{}", case.name);
    assert_eq!(pending[0]["request_doc_id"], parent_doc, "{}", case.name);

    let first = ToolCallLifecycle::reconcile_background_completion_side_effects(node, did)
        .await
        .expect("redrive durable native completion side effects");
    assert_eq!(first.side_effects_converged, 1, "{}: {first:?}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(node, did)
        .await
        .expect("replay completion side effects");
    assert!(second.is_noop(), "{}: {second:?}", case.name);

    let notifications = notification_texts(&admission).await;
    assert_eq!(
        !notifications.is_empty(),
        case.notification_persisted.unwrap(),
        "{}",
        case.name
    );
    assert_eq!(
        notifications.len(),
        1,
        "{}: replay duplicated notification",
        case.name
    );
    assert!(
        notifications[0].1.contains("durable native output"),
        "{}",
        case.name
    );
    let requests = rows(node, &format!(r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID request_id lifecycle_state execution_origin input }} }}"#,
        escape_graphql_string(&session)), "AgentRequest").await;
    assert_eq!(
        requests.len(),
        1 + usize::from(case.wake_created.unwrap()),
        "{}",
        case.name
    );
    let consumer = if case.wake_created.unwrap() {
        requests
            .iter()
            .find(|row| row["request_id"] != parent)
            .unwrap()
    } else {
        requests
            .iter()
            .find(|row| row["_docID"] == parent_doc)
            .unwrap()
    };
    assert_eq!(
        notifications[0].0["request_doc_id"], consumer["_docID"],
        "{}",
        case.name
    );
    if case.wake_created.unwrap() {
        assert_eq!(consumer["execution_origin"], "scheduled", "{}", case.name);
        assert_eq!(
            consumer["input"]["queue"]["source"], "background_completion",
            "{}",
            case.name
        );
        assert_eq!(
            consumer["input"]["queue"]["key"],
            format!("background_completion:{session}"),
            "{}",
            case.name
        );
    }
    let tool = rows(node, &format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID status lifecycle_state completion_notification_delivered_at }} }}"#,
        escape_graphql_string(&tool_doc)), "AgentToolCall").await;
    assert_eq!(tool.len(), 1);
    assert_eq!(tool[0]["status"], "completed", "{}", case.name);
    assert_eq!(tool[0]["lifecycle_state"], "completed", "{}", case.name);
    assert!(
        tool[0]["completion_notification_delivered_at"].is_string(),
        "{}",
        case.name
    );
    let after = load_canonical_goal(node, did, &session).await.unwrap();
    assert_eq!(
        json!(before),
        json!(after),
        "{}: completion changed Goal",
        case.name
    );
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

async fn seed_session_head(node: &EmbeddedNode, session: &str, request: &Value) {
    let observation = gents_protocol::session::SessionObservation {
        last_activity_at: request["created_at"].as_str().unwrap().to_owned(),
        preview: Some("background input".to_owned()),
        latest_request: Some(gents_protocol::session::SessionRequestObservation {
            request_doc_id: request["_docID"].as_str().unwrap().to_owned(),
            request_id: request["request_id"].as_str().unwrap().to_owned(),
            lifecycle_state: serde_json::from_value(request["lifecycle_state"].clone()).unwrap(),
        }),
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&json!({"observation": observation}))
            .unwrap();
    let response = node.execute(&format!(r#"mutation {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, input: {input}) {{ _docID }} }}"#,
        escape_graphql_string(session))).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

async fn drive_redrive(case: &LeanR6BackgroundingCase) {
    let admission = admitted_parent(case, "redrive").await;
    let node = &admission.node;
    let did = admission.agent_did.as_str();
    let session = admission.tool.session_id.as_str();
    let parent_doc = admission.tool.request_doc_id().unwrap();
    let parent_row = rows(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID request_id }} }}"#,
            escape_graphql_string(parent_doc)
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(parent_row.len(), 1);
    let parent = parent_row[0]["request_id"].as_str().unwrap();
    let model = lean_r6_backgrounding_case("failed_background_wake_with_budget_redrives");
    assert_eq!(
        model.post_parent_request_id,
        model.redrive_source_request_id
    );
    let failed_wake = format!("failed-wake-{}", model.redrive_source_request_id.unwrap());
    let source_depth = model.pre_depth.unwrap();
    let source_retry_count = model.retry_count.unwrap();
    let max_retries = model.max_retries.unwrap();
    let source_deadline =
        chrono::DateTime::from_timestamp(model.pre_execution_deadline.unwrap() as i64, 0)
            .unwrap()
            .to_rfc3339();
    let before = load_canonical_goal(node, did, session).await.unwrap();
    let input = RequestInput {
        queue: Some(RequestQueue {
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session}")),
            queued_after_request_id: Some(parent.to_owned()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(input).unwrap())
            .unwrap();
    let response = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{}", agent_did: "{}", requester_did: "{}", behavior_id: "general",
        session_id: "{}", content: "background input", input: {input},
        execution_origin: "scheduled", lifecycle_state: "failed",
        failure_reason: "backend admission failed", terminalized_at: "2026-07-15T00:00:00Z",
        created_at: "2026-07-15T00:00:00Z", retry_count: {source_retry_count}, max_retries: {max_retries},
        retry_root_request: "{}", terminal_redrive_attempts: 0,
        subagent_depth: {source_depth}, deadline: "{}",
        caused_by_parent_request_id: "{}", caused_by_parent_request_doc_id: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&failed_wake), escape_graphql_string(did), escape_graphql_string(did),
        escape_graphql_string(session), escape_graphql_string(&failed_wake),
        escape_graphql_string(&source_deadline), escape_graphql_string(parent),
        escape_graphql_string(parent_doc))).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let source = rows(node, &format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID request_id lifecycle_state created_at }} }}"#,
        escape_graphql_string(&failed_wake)), "AgentRequest").await;
    assert_eq!(source.len(), 1);
    seed_session_head(node, session, &source[0]).await;

    let first = crate::RequestLifecycle::redrive_failed_background_wakeups(node, did)
        .await
        .unwrap();
    assert_eq!(
        first.redriven > 0,
        case.redrive_allowed.unwrap(),
        "{}: {first:?}",
        case.name
    );
    assert_eq!(first.failed, 0, "{}: {first:?}", case.name);
    let second = crate::RequestLifecycle::redrive_failed_background_wakeups(node, did)
        .await
        .unwrap();
    assert_eq!(second.redriven, 0, "{}: replay added successor", case.name);
    let requests = rows(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
        _docID request_id retry_count retry_parent_request retry_parent_request_doc_id max_retries
        backend_id caused_by_parent_request_id caused_by_parent_request_doc_id subagent_depth
        deadline lifecycle_state
    }} }}"#,
            escape_graphql_string(session)
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(
        requests.len(),
        2 + usize::from(case.redrive_allowed.unwrap()),
        "{}",
        case.name
    );
    if case.redrive_allowed.unwrap() {
        let source = requests
            .iter()
            .find(|row| row["request_id"] == failed_wake)
            .unwrap();
        let successor = requests
            .iter()
            .find(|row| row["request_id"] != parent && row["request_id"] != failed_wake)
            .unwrap();
        assert_eq!(source["lifecycle_state"], "failed", "{}", case.name);
        assert_eq!(source["subagent_depth"].as_u64(), Some(source_depth));
        assert_eq!(source["deadline"].as_str(), Some(source_deadline.as_str()));
        assert_eq!(source["caused_by_parent_request_id"], parent);
        assert_eq!(source["caused_by_parent_request_doc_id"], parent_doc);
        assert_eq!(
            source["retry_count"].as_u64(),
            model.retry_count.map(|v| v as u64)
        );
        assert_eq!(successor["caused_by_parent_request_id"], failed_wake);
        assert_eq!(
            successor["caused_by_parent_request_doc_id"],
            source["_docID"]
        );
        assert_eq!(successor["retry_parent_request"], failed_wake);
        assert_eq!(successor["retry_parent_request_doc_id"], source["_docID"]);
        assert_eq!(successor["subagent_depth"].as_u64(), model.post_depth);
        assert_eq!(
            successor["retry_count"].as_u64(),
            model.post_retry_count.map(|v| v as u64)
        );
        assert_eq!(successor["max_retries"], max_retries);
        assert!(successor["backend_id"].is_null(), "{}", case.name);
        assert!(successor["deadline"].is_null(), "{}", case.name);
    }
    let after = load_canonical_goal(node, did, session).await.unwrap();
    assert_eq!(
        json!(before),
        json!(after),
        "{}: redrive changed Goal",
        case.name
    );
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn generated_r6_completion_owner_cases_use_accepted_native_output() {
    let cases = lean_r6_backgrounding_cases();
    let cases: Vec<_> = cases
        .iter()
        .filter(|case| case.group == "completion_continuation_owner")
        .collect();
    assert_eq!(cases.len(), 7, "Lean completion-owner family drifted");
    for case in cases {
        assert_eq!(
            case.action, "notify_and_select_continuation_owner",
            "{}",
            case.name
        );
        assert!(
            case.legal && case.notification_persisted == Some(true),
            "{}",
            case.name
        );
        drive_notification(case).await;
        drive_redrive(case).await;
    }
}

/// The watcher-selected request is deliberately still pending. The daemon
/// integration test hands it to `process_request`, which owns its claim.
pub(crate) struct SelectedBackgroundWake {
    pub admission: PublishedAdmission,
    pub wake: crate::watcher::AgentRequest,
    pub notification_text: String,
    wait_header: String,
    wait_native: gents_protocol::message::Message,
    notifications: Vec<(Value, String)>,
}

/// Compose the existing signed subagent-completion owners through watcher
/// selection, leaving the actual wake claim to the caller.
pub(crate) async fn selected_background_wake() -> SelectedBackgroundWake {
    use super::admission_fixture::{
        complete_child, publish_accepted_on_claimed_request, published_admission_with_owner,
    };
    use crate::background_completion::{
        project_background_subagent_completion, BackgroundCompletionOutcome,
    };
    use crate::lifecycle::{RequestTerminalOutcome, TerminalizeResult};
    use crate::watcher::{DefraWatcher, Watcher};
    use gents_protocol::message::{AssistantContent, Message};
    use gents_protocol::output::TerminalOutput;
    use std::time::Duration;

    let case =
        lean_r6_backgrounding_case("terminal_completion_message_precedes_claimed_continuation");
    assert!(case.legal);
    assert_eq!(case.action, "terminalize_append_notification_enqueue_claim");
    assert_eq!(
        case.result.as_deref(),
        Some("assistant_wait_precedes_notification")
    );
    assert_eq!(case.reason.as_deref(), Some("continuation_claimed"));
    assert_eq!(case.terminal_state, "completed");
    assert_eq!(case.queue_source.as_deref(), Some("background_completion"));
    assert_eq!(case.queue_key.as_deref(), Some("background_completion:900"));

    let child = "completion-order-child";
    let (mut admission, mut owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "completion-order".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: "completion-order-spawn".into(),
            child_request_id: child.into(),
            spawn_target_did: "fixture-overrides-with-owner".into(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        ..Default::default()
    })
    .await
    .unwrap();
    let node = &admission.node;
    let did = &admission.agent_did;
    let session = admission.tool.session_id.clone();
    crate::test_support::install_test_behavior(node, did, "general").await;
    admission
        .tool
        .publish_background_receipt("child started")
        .await
        .unwrap();
    super::create_subagent_request_with_request_id(
        node,
        child.into(),
        owner.request().request_id.clone(),
        owner.request().doc_id.clone(),
        admission.tool.tool_call_id().into(),
        admission.tool.doc_id().unwrap().into(),
        0,
        did.clone(),
        "general".into(),
        "child work".into(),
        Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
    )
    .await
    .unwrap();

    let mut wait = publish_accepted_on_claimed_request(
        node.clone(),
        &mut owner,
        did,
        1,
        "wait_subagent",
        "completion-order-wait",
        json!({"child_request_id": child}),
        None,
        AwaitMode::Foreground,
        super::CancelPolicy::Cascade,
        true,
    )
    .await
    .unwrap();
    let wait_header = wait.accepted_header_doc_id().unwrap().to_owned();
    complete_child(node, child, did, "durable native output").await;
    let projected = project_background_subagent_completion(node.clone(), child, did)
        .await
        .unwrap();
    assert!(
        matches!(projected, BackgroundCompletionOutcome::Projected { .. }),
        "{projected:?}"
    );

    let notifications = notification_texts(&admission).await;
    assert_eq!(notifications.len(), 1);
    let notification_id = notifications[0].0["_docID"].as_str().unwrap();
    let (wait_record, wait_native) =
        crate::session::load_canonical_message_from_node(node, &wait_header, did, Some(did))
            .await
            .unwrap();
    let (notification_record, _) =
        crate::session::load_canonical_message_from_node(node, notification_id, did, Some(did))
            .await
            .unwrap();
    assert!(matches!(wait_native, Message::Assistant { ref content, .. }
        if content.iter().any(|part| matches!(part, AssistantContent::ToolCall(call) if call.function.name == "wait_subagent"))));
    assert!(wait_record.sequence < notification_record.sequence);
    assert!(notifications[0].1.contains("<subagent-notification"));
    assert!(notifications[0].1.contains("durable native output"));

    let requests = rows(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ {} lifecycle_state }} }}"#,
            escape_graphql_string(&session),
            crate::watcher::AGENT_REQUEST_FIELDS,
        ),
        "AgentRequest",
    )
    .await;
    let wakes: Vec<gents_protocol::row::AgentRequestRow> = requests
        .into_iter()
        .map(|row| serde_json::from_value(row).unwrap())
        .filter(|row: &gents_protocol::row::AgentRequestRow| {
            row.input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .is_some_and(|queue| queue.source == QueueSource::BackgroundCompletion)
        })
        .collect();
    assert_eq!(wakes.len(), 1);
    let wake = &wakes[0];
    assert_eq!(wake.execution_origin.as_deref(), Some("scheduled"));
    assert_eq!(
        wake.lifecycle_state,
        Some(gents_protocol::request_lifecycle::RequestLifecycleState::Pending)
    );
    let queue = wake.input.as_ref().unwrap().queue.as_ref().unwrap();
    assert_eq!(queue.policy, QueuePolicy::Coalesce);
    assert_eq!(
        queue.key.as_deref(),
        Some(format!("background_completion:{session}").as_str())
    );
    assert_eq!(queue.background_completion_wake_version, Some(1));

    wait.complete("child completed").await.unwrap();
    assert_eq!(
        owner
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                TerminalOutput::Message {
                    message_doc_id: wait_header.clone()
                },
                None,
            )
            .await
            .unwrap(),
        TerminalizeResult::Won
    );
    let mut watcher = DefraWatcher::new(node.clone(), did);
    let selected = tokio::time::timeout(Duration::from_secs(2), watcher.next_request())
        .await
        .expect("queued completion becomes available")
        .unwrap()
        .unwrap();
    assert_eq!(selected.request_id, wake.request_id);
    assert_eq!(selected.session_id, session);
    drop(watcher);
    drop(owner);
    SelectedBackgroundWake {
        admission,
        wake: selected,
        notification_text: notifications[0].1.clone(),
        wait_header,
        wait_native,
        notifications,
    }
}

/// The composed continuation witness goes through publication and the real
/// child-completion projector before the watcher can expose the queued wake.
#[tokio::test]
async fn generated_r6_notification_precedes_continuation_claim() {
    let SelectedBackgroundWake {
        admission,
        wake: selected,
        notification_text: _,
        wait_header,
        wait_native,
        notifications,
    } = selected_background_wake().await;
    let node = &admission.node;
    let did = &admission.agent_did;
    let behavior = selected.behavior_id.clone();
    let mut continuation = crate::lifecycle::RequestLifecycle::new_with_agent_did(
        node.clone(),
        &behavior,
        did,
        selected,
        60,
    );
    assert_eq!(
        continuation.claim_with_identity().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed,
        "the selected wake must claim through the real lifecycle owner"
    );
    let after = notification_texts(&admission).await;
    assert_eq!(
        after, notifications,
        "claim preserves immutable notification history"
    );
    let (_, wait_after) =
        crate::session::load_canonical_message_from_node(node, &wait_header, did, Some(did))
            .await
            .unwrap();
    assert_eq!(
        wait_after, wait_native,
        "claim preserves the preceding assistant wait"
    );
    drop(continuation);
    node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}
