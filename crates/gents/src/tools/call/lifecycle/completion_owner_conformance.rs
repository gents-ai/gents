use super::{
    admission_fixture::{published_admission, PublishedAdmission, PublishedAdmissionOptions},
    AwaitMode, ToolCallLifecycle,
};
use crate::goal::{load_canonical_goal_in_txn, set_goal_from_access, GoalStatus};
use crate::lean_vocab_test::{
    lean_r6_backgrounding_case, lean_r6_backgrounding_cases, LeanR6BackgroundingCase,
};
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
use anyhow::Context;
use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use serde_json::{json, Value};

async fn rows(access: &ConfigAccess, query: &str, field: &str) -> Vec<Value> {
    let response = access.execute(query).await.expect("read fixture rows");
    response["data"][field].as_array().expect("rows").clone()
}

async fn update(access: &ConfigAccess, collection: &str, doc_id: &str, fields: &str) {
    let response = access.write("test.completion.update", &format!(
        r#"mutation {{ update_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#,
        escape_graphql_string(doc_id),
    )).await.expect("update fixture row");
    assert_eq!(
        response["data"][&format!("update_{collection}")]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn canonical_goal(
    access: &ConfigAccess,
    node_did: &str,
    session: &str,
) -> Option<crate::goal::GoalDocument> {
    access
        .transact_readonly("test.completion.goal", |txn| {
            Box::pin(async move { load_canonical_goal_in_txn(txn, node_did, session).await })
        })
        .await
        .expect("load modeled Goal under held actor")
}

async fn create_failed_wake(admission: &PublishedAdmission, mutation: &str) {
    let access = admission.tool.access().unwrap();
    let node_did = &admission.node_did;
    let session_id = &admission.tool.session_id;
    let agent_did = admission.tool.actor().as_str();
    let requester_did = admission.requester_identity.did();
    access
        .transact_write("test.completion.failed_wake", |txn| {
            Box::pin(async move {
                let session =
                    crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        node_did,
                        session_id,
                        Some(requester_did),
                    )
                    .await?
                    .context("fixture session unavailable")?;
                anyhow::ensure!(
                    session.session.agent_did == agent_did,
                    "fixture Agent mismatch"
                );
                let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    node_did,
                    agent_did,
                )
                .await?
                .context("fixture Agent unavailable")?;
                let response = txn.execute(mutation).await?;
                let request_doc = crate::store::graphql::created_doc_id(&response, "AgentRequest")?;
                crate::request::lifecycle::stage_request_relationships_in_txn(
                    txn,
                    &request_doc,
                    &agent_doc,
                    &session.doc_id,
                    agent_did,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .expect("publish failed wake with Agent relationships");
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
        request_created_at: branch
            .ends_with("redrive")
            .then(|| "2026-07-14T00:00:00Z".to_owned()),
        ..Default::default()
    })
    .await
    .expect("publish accepted background invocation on claimed request");
    let parent_doc = admission.tool.request_doc_id().unwrap();
    update(
        &admission.tool.access().unwrap(),
        "AgentRequest",
        parent_doc,
        r#"lifecycle_state: "completed""#,
    )
    .await;
    let session = admission.tool.session_id.as_str();
    if let Some(status) = goal_status(case) {
        set_goal_from_access(
            &admission.tool.access().unwrap(),
            &admission.node_did,
            session,
            Some("One continuation owner"),
            Some(status),
            Some(Some(10)),
            None,
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
    let response = admission.client.execute(&query).await.unwrap();
    let headers = response["data"]["AgentMessage"].as_array().unwrap().clone();
    let mut notifications = Vec::new();
    for header in headers {
        let doc_id = header["_docID"].as_str().unwrap();
        let (_, native) = crate::session::load_canonical_message(
            &admission.client,
            doc_id,
            &admission.node_did,
            Some(admission.requester_identity.did()),
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

fn recovery_identities(
    admission: &PublishedAdmission,
) -> Vec<std::sync::Arc<dyn crate::store::identity::NodeIdentity>> {
    vec![std::sync::Arc::new(
        admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
    )]
}

async fn drive_notification(case: &LeanR6BackgroundingCase) {
    let mut admission = admitted_parent(case, "notification").await;
    let node = &admission.node;
    let access = admission.tool.access().unwrap();
    let did = admission.node_did.as_str();
    let session = admission.tool.session_id.clone();
    let parent_doc = admission.tool.request_doc_id().unwrap().to_owned();
    let parent_row = rows(
        &access,
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
    let before = canonical_goal(&access, did, &session).await;
    assert!(admission
        .tool
        .complete_owned("durable native output", None)
        .await
        .unwrap());
    let pending = rows(&access, &format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID status lifecycle_state request_doc_id }} }}"#,
        escape_graphql_string(&tool_doc)), "AgentToolCall").await;
    assert_eq!(pending.len(), 1, "{}", case.name);
    assert_eq!(pending[0]["status"], "completionPending", "{}", case.name);
    assert_eq!(pending[0]["lifecycle_state"], "completed", "{}", case.name);
    assert_eq!(pending[0]["request_doc_id"], parent_doc, "{}", case.name);

    let first = ToolCallLifecycle::reconcile_background_completion_side_effects(
        node,
        did,
        &recovery_identities(&admission),
    )
    .await
    .expect("redrive durable native completion side effects");
    assert_eq!(first.side_effects_converged, 1, "{}: {first:?}", case.name);
    let second = ToolCallLifecycle::reconcile_background_completion_side_effects(
        node,
        did,
        &recovery_identities(&admission),
    )
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
    let requests = rows(&access, &format!(r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID request_id lifecycle_state execution_origin input }} }}"#,
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
    let tool = rows(&access, &format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID status lifecycle_state completion_notification_delivered_at }} }}"#,
        escape_graphql_string(&tool_doc)), "AgentToolCall").await;
    assert_eq!(tool.len(), 1);
    assert_eq!(tool[0]["status"], "completed", "{}", case.name);
    assert_eq!(tool[0]["lifecycle_state"], "completed", "{}", case.name);
    assert!(
        tool[0]["completion_notification_delivered_at"].is_string(),
        "{}",
        case.name
    );
    let after = canonical_goal(&access, did, &session).await;
    assert_eq!(
        json!(before),
        json!(after),
        "{}: completion changed Goal",
        case.name
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}

async fn seed_session_head(access: &ConfigAccess, session: &str, request: &Value) {
    let observation = gents_protocol::session::SessionObservation {
        reply_preview: None,
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
    let response = access.write("test.completion.session_head", &format!(r#"mutation {{ update_AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, input: {input}) {{ _docID }} }}"#,
        escape_graphql_string(session))).await.expect("update session head");
    assert_eq!(
        response["data"]["update_AgentSession"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn drive_redrive(case: &LeanR6BackgroundingCase) {
    let admission = admitted_parent(case, "redrive").await;
    let node = &admission.node;
    let access = admission.tool.access().unwrap();
    let did = admission.node_did.as_str();
    let agent_did = escape_graphql_string(admission.tool.actor().as_str());
    let session = admission.tool.session_id.as_str();
    let parent_doc = admission.tool.request_doc_id().unwrap();
    let parent_row = rows(
        &access,
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
    let before = canonical_goal(&access, did, session).await;
    let input = RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
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
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{}", purpose: "normal", node_did: "{}", requester_did: "{}", agent_did: "{agent_did}",
        session_id: "{}", content: "background input", input: {input},
        execution_origin: "scheduled", lifecycle_state: "failed",
        failure_reason: "backend admission failed", terminalized_at: "2026-07-15T00:00:00Z",
        created_at: "2026-07-15T00:00:00Z", retry_count: {source_retry_count}, max_retries: {max_retries},
        retry_root_request: "{}", terminal_redrive_attempts: 0,
        request_hop: {source_depth}, deadline: "{}",
        caused_by_parent_request_id: "{}", caused_by_parent_request_doc_id: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&failed_wake),
        escape_graphql_string(did),
        escape_graphql_string(admission.requester_identity.did()),
        escape_graphql_string(session),
        escape_graphql_string(&failed_wake),
        escape_graphql_string(&source_deadline),
        escape_graphql_string(parent),
        escape_graphql_string(parent_doc)
    );
    create_failed_wake(&admission, &mutation).await;
    let source = rows(&access, &format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID request_id lifecycle_state created_at }} }}"#,
        escape_graphql_string(&failed_wake)), "AgentRequest").await;
    assert_eq!(source.len(), 1);
    seed_session_head(&access, session, &source[0]).await;

    let first = crate::RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        node,
        did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert_eq!(
        first.redriven > 0,
        case.redrive_allowed.unwrap(),
        "{}: {first:?}",
        case.name
    );
    assert_eq!(first.failed, 0, "{}: {first:?}", case.name);
    let second = crate::RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        node,
        did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert_eq!(second.redriven, 0, "{}: replay added successor", case.name);
    let requests = rows(
        &access,
        &format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
        _docID request_id retry_count retry_parent_request retry_parent_request_doc_id max_retries
        backend_id caused_by_parent_request_id caused_by_parent_request_doc_id request_hop
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
        assert_eq!(source["request_hop"].as_u64(), Some(source_depth));
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
        assert_eq!(successor["request_hop"].as_u64(), model.post_depth);
        assert_eq!(
            successor["retry_count"].as_u64(),
            model.post_retry_count.map(|v| v as u64)
        );
        assert_eq!(successor["max_retries"], max_retries);
        assert!(successor["backend_id"].is_null(), "{}", case.name);
        assert!(successor["deadline"].is_null(), "{}", case.name);
    }
    let after = canonical_goal(&access, did, session).await;
    assert_eq!(
        json!(before),
        json!(after),
        "{}: redrive changed Goal",
        case.name
    );
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
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

#[tokio::test]
async fn background_wake_is_visible_to_the_owning_user_and_selected_agent() {
    use crate::store::config_client::{ConfigAccess, NodePermission};
    use crate::store::identity::NodeIdentity;

    let cases = lean_r6_backgrounding_cases();
    let case = cases
        .iter()
        .find(|case| {
            case.group == "completion_continuation_owner" && case.wake_created == Some(true)
        })
        .expect("modeled completion with a fresh wake");
    let mut admission = admitted_parent(case, "user-visible-wake").await;
    assert!(admission
        .tool
        .complete_owned("durable native output", None)
        .await
        .unwrap());
    let recovered = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert_eq!(recovered.side_effects_converged, 1);
    let replay = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert!(replay.is_noop());

    let notifications = notification_texts(&admission).await;
    assert_eq!(notifications.len(), 1);
    let wake_doc = notifications[0].0["request_doc_id"].as_str().unwrap();
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID agent_did session_id }} }}"#,
        escape_graphql_string(wake_doc)
    );
    let worker_access = admission.tool.access().unwrap();
    for reader in [&admission.client, &worker_access] {
        let rows = reader.execute(&query).await.unwrap();
        let rows = rows["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the owning User and working Agent see the wake"
        );
        assert_eq!(rows[0]["_docID"], wake_doc);
        assert_eq!(rows[0]["agent_did"], admission.tool.actor.as_str());
        assert_eq!(rows[0]["session_id"], admission.tool.session_id);
    }

    let stranger = tokio::task::spawn_blocking(|| {
        crate::support::fixtures::test_identity("unrelated-wake-user")
    })
    .await
    .unwrap();
    let user_did = identity::Did::new(admission.requester_identity.did()).unwrap();
    let binding = crate::store::identity::provision_agent_identity_authorized(
        &admission.node,
        admission.runtime.as_ref(),
        &user_did,
    )
    .await
    .unwrap();
    crate::test_support::install_test_agent_binding_with_access(
        admission.runtime.did(),
        "Unrelated agent",
        &binding,
        &admission.client,
    )
    .await;
    let sibling =
        crate::store::identity::resolve_agent_identity(admission.runtime.as_ref(), &binding)
            .unwrap();
    let admin = ConfigAccess::local_as(
        admission.node.clone(),
        identity::Did::new(&admission.node_did).unwrap(),
    );
    for did in [stranger.did(), sibling.did()] {
        let did = identity::Did::new(did).unwrap();
        for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
            admin.grant_node_permission(&did, permission).await.unwrap();
        }
    }
    let stranger_access = ConfigAccess::local_as(
        admission.node.clone(),
        identity::Did::new(stranger.did()).unwrap(),
    );
    let sibling_access = ConfigAccess::local_managed_agent(
        admission.node.clone(),
        identity::Did::new(sibling.did()).unwrap(),
        sibling.as_ref(),
    )
    .unwrap();
    for reader in [&stranger_access, &sibling_access] {
        let rows = reader.execute(&query).await.unwrap();
        assert!(rows["data"]["AgentRequest"].as_array().unwrap().is_empty());
    }
    let agent_did = admission.tool.actor.as_str();
    let session_id = admission.tool.session_id.as_str();
    let targets = admission
        .client
        .execute(&format!(
            r#"{{
        Agent(filter: {{agent_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}}
        AgentSession(filter: {{session_id: {{_eq: "{}"}}}}, limit: 2) {{_docID}}
    }}"#,
            escape_graphql_string(agent_did),
            escape_graphql_string(session_id)
        ))
        .await
        .unwrap();
    let agent_doc = targets["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let session_doc = targets["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .unwrap();
    admin
        .grant_node_permission(
            &identity::Did::new(agent_did).unwrap(),
            NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    worker_access
        .delete_resource_relationship("AgentRequest", wake_doc, "parent", "agents", agent_doc)
        .await
        .unwrap();
    worker_access
        .delete_resource_relationship("AgentRequest", wake_doc, "session", "sessions", session_doc)
        .await
        .unwrap();
    let hidden = admission.client.execute(&query).await.unwrap();
    assert!(hidden["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    let replay = ToolCallLifecycle::reconcile_background_completion_side_effects(
        &admission.node,
        &admission.node_did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert!(replay.is_noop());
    let hidden_after_replay = admission.client.execute(&query).await.unwrap();
    assert!(hidden_after_replay["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .is_empty());
    admission.node.shutdown().await.unwrap();
    std::fs::remove_dir_all(admission.path).unwrap();
}

pub(crate) struct SelectedBackgroundWake {
    pub admission: PublishedAdmission,
    pub wake: gents_protocol::row::AgentRequest,
    pub notification_text: String,
    wait_header: String,
    wait_native: gents_protocol::message::Message,
    notifications: Vec<(Value, String)>,
}

pub(crate) async fn selected_background_wake() -> SelectedBackgroundWake {
    selected_background_wake_with_successor(false).await.0
}

pub(crate) async fn selected_background_wake_with_successor(
    prepare_successor: bool,
) -> (SelectedBackgroundWake, Option<String>) {
    use super::admission_fixture::{
        complete_child_managed_verified, publish_accepted_on_claimed_request,
        published_session_message_with_owner,
    };
    use crate::request::lifecycle::{RequestTerminalOutcome, TerminalizeResult};
    use crate::request::watcher::{DefraWatcher, Watcher};
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

    let (message, mut owner) = published_session_message_with_owner(PublishedAdmissionOptions {
        name: "completion-order".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let admission = message.admission;
    let node = &admission.node;
    let did = &admission.node_did;
    let session = admission.tool.session_id.clone();

    let mut wait = publish_accepted_on_claimed_request(
        node.clone(),
        &mut owner,
        did,
        1,
        crate::tools::native::LIST_PROCESSES_TOOL_NAME,
        "completion-order-list",
        json!({}),
        AwaitMode::Foreground,
        true,
    )
    .await
    .unwrap();
    let successor = if prepare_successor {
        let mut call = publish_accepted_on_claimed_request(
            node.clone(),
            &mut owner,
            did,
            2,
            crate::tools::native::AGENT_NEW_TOOL_NAME,
            "completion-order-successor",
            json!({"agent": "general", "prompt": "second work"}),
            AwaitMode::Background,
            false,
        )
        .await
        .unwrap();
        let receipt = super::admission_fixture::materialize_session_message(
            node,
            &owner,
            &mut call,
            did,
            "second work",
        )
        .await
        .unwrap();
        Some(receipt.request_id)
    } else {
        None
    };
    let wait_header = wait.accepted_header_doc_id().unwrap().to_owned();
    complete_child_managed_verified(
        node,
        &message.caused_request_id,
        did,
        "durable native output",
        &admission
            .tool
            .access()
            .unwrap()
            .managed_agent_signer()
            .unwrap(),
        admission.runtime.clone(),
    )
    .await;
    let settled =
        crate::tools::background::completion::settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            did,
        )
        .await
        .unwrap();
    assert_eq!(settled, 1, "the caused request's terminal settles its row");

    let notifications = notification_texts(&admission).await;
    assert_eq!(notifications.len(), 1);
    let notification_id = notifications[0].0["_docID"].as_str().unwrap();
    let (wait_record, wait_native) = crate::session::load_canonical_message(
        &admission.client,
        &wait_header,
        did,
        Some(admission.requester_identity.did()),
    )
    .await
    .unwrap();
    let (notification_record, _) = crate::session::load_canonical_message(
        &admission.client,
        notification_id,
        did,
        Some(admission.requester_identity.did()),
    )
    .await
    .unwrap();
    assert!(matches!(wait_native, Message::Assistant { ref content, .. }
        if content.iter().any(|part| matches!(part, AssistantContent::ToolCall(call) if call.function.name == crate::tools::native::LIST_PROCESSES_TOOL_NAME))));
    assert!(wait_record.sequence < notification_record.sequence);
    assert!(notifications[0].1.contains("<tool-completion"));
    assert!(notifications[0].1.contains("durable native output"));

    let response = admission.client.execute(&format!(
        r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ {} lifecycle_state }} }}"#,
        escape_graphql_string(&session), gents_protocol::row::AGENT_REQUEST_FIELDS,
    )).await.unwrap();
    let requests = response["data"]["AgentRequest"].as_array().unwrap().clone();
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
    assert_eq!(wake.request_hop, Some(0));
    assert_eq!(
        queue.key.as_deref(),
        Some(format!("background_completion:{session}").as_str())
    );
    assert_eq!(queue.background_completion_wake_version, Some(1));

    wait.complete("listed").await.unwrap();
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
    let signer = admission
        .tool
        .access()
        .unwrap()
        .managed_agent_signer()
        .unwrap();
    let mut watcher =
        DefraWatcher::for_agents(node.clone(), did, vec![std::sync::Arc::new(signer)])
            .expect("request watcher");
    let selected = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let selected = watcher
                .next_request()
                .await
                .context("queued completion disappeared")??;
            if selected.request_id == wake.request_id {
                return Ok::<_, anyhow::Error>(selected);
            }
        }
    })
    .await
    .expect("queued completion becomes available")
    .unwrap();
    assert_eq!(selected.request_id, wake.request_id);
    assert_eq!(selected.session_id, session);
    drop(watcher);
    drop(owner);
    (
        SelectedBackgroundWake {
            admission,
            wake: selected,
            notification_text: notifications[0].1.clone(),
            wait_header,
            wait_native,
            notifications,
        },
        successor,
    )
}

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
    let did = &admission.node_did;
    let behavior = selected.agent_did.clone();
    let mut continuation = crate::request::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        &behavior,
        did,
        selected,
        60,
    );
    let signer = admission
        .tool
        .access()
        .unwrap()
        .managed_agent_signer()
        .unwrap();
    continuation.bind_managed_agent(&signer).unwrap();
    assert_eq!(
        continuation.claim_with_identity().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed,
        "the selected wake must claim through the real lifecycle owner"
    );
    let after = notification_texts(&admission).await;
    assert_eq!(
        after, notifications,
        "claim preserves immutable notification history"
    );
    let (_, wait_after) = crate::session::load_canonical_message(
        &admission.client,
        &wait_header,
        did,
        Some(admission.requester_identity.did()),
    )
    .await
    .unwrap();
    assert_eq!(
        wait_after, wait_native,
        "claim preserves the preceding assistant wait"
    );
    drop(continuation);
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn a_retried_over_bound_wake_is_refused_again() {
    let case = lean_r6_backgrounding_case("failed_background_wake_with_budget_redrives");
    let admission = admitted_parent(case, "over-bound-redrive").await;
    let node = &admission.node;
    let access = admission.tool.access().unwrap();
    let did = admission.node_did.as_str();
    let agent_did = escape_graphql_string(admission.tool.actor().as_str());
    admission
        .client
        .transact_write("test.completion.configure_hop_bound", |txn| {
            Box::pin(async move {
                let (_, mut node_input) =
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Node,
                        did,
                        did,
                    )
                    .await?
                    .context("owned Node is unavailable")?;
                node_input["max_request_hop"] = json!(8);
                let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
                    crate::store::config_client::DesiredStateApplyDocument {
                        collection: crate::Collection::Node,
                        add: node_input.clone(),
                        update: node_input,
                    },
                ])?;
                crate::documents::apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
        .expect("configure Node hop bound as the signed owning User");
    let session = admission.tool.session_id.as_str();
    let parent_doc = admission.tool.request_doc_id().unwrap();
    let parent = rows(
        &access,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_id }} }}"#,
            escape_graphql_string(parent_doc)
        ),
        "AgentRequest",
    )
    .await[0]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bound = 8;
    let over_bound = gents_protocol::request_lifecycle::next_request_hop(
        gents_protocol::request_lifecycle::RequestHopCause::CrossSession { cause_hop: bound },
        0,
    );
    assert!(!gents_protocol::request_lifecycle::request_hop_within_bound(Some(bound), over_bound));
    let refused_wake = "refused-over-bound-wake";
    let input = RequestInput {
        queue: Some(RequestQueue {
            delivery: Default::default(),
            position: None,
            source: QueueSource::BackgroundCompletion,
            policy: QueuePolicy::Coalesce,
            key: Some(format!("background_completion:{session}")),
            queued_after_request_id: Some(parent.clone()),
            interrupted_request_id: None,
            background_completion_wake_version: Some(1),
        }),
        ..Default::default()
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(input).unwrap())
            .unwrap();
    let deadline = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{refused_wake}", purpose: "normal", node_did: "{}", requester_did: "{}", agent_did: "{agent_did}",
        session_id: "{}", content: "background input", input: {input},
        execution_origin: "scheduled", lifecycle_state: "failed",
        failure_reason: "AgentRequest causal hop exceeds the target node's max_request_hop",
        terminalized_at: "2026-07-15T00:00:00Z", created_at: "2026-07-15T00:00:00Z", retry_count: 0, max_retries: 3,
        retry_root_request: "{refused_wake}", terminal_redrive_attempts: 0,
        request_hop: {}, deadline: "{}",
        caused_by_parent_request_id: "{}", caused_by_parent_request_doc_id: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(did),
        escape_graphql_string(admission.requester_identity.did()),
        escape_graphql_string(session),
        over_bound,
        escape_graphql_string(&deadline),
        escape_graphql_string(&parent),
        escape_graphql_string(parent_doc)
    );
    create_failed_wake(&admission, &mutation).await;
    let source = rows(&access, &format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{refused_wake}" }} }}) {{ _docID request_id lifecycle_state created_at }} }}"#), "AgentRequest").await;
    seed_session_head(&access, session, &source[0]).await;

    let report = crate::RequestLifecycle::redrive_failed_background_wakeups_with_identities(
        node,
        did,
        &recovery_identities(&admission),
    )
    .await
    .unwrap();
    assert_eq!(report.redriven, 1, "{report:?}");
    let successor = rows(
        &access,
        &format!(
            r#"{{ AgentRequest(filter: {{ retry_parent_request: {{ _eq: "{refused_wake}" }} }}) {{ request_hop }} }}"#
        ),
        "AgentRequest",
    )
    .await;
    assert_eq!(successor.len(), 1);
    let hop = successor[0]["request_hop"].as_u64().unwrap() as u32;
    assert_eq!(hop, over_bound);
    assert!(!gents_protocol::request_lifecycle::request_hop_within_bound(Some(bound), hop));
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}
