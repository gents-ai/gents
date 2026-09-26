//! Orphaned subagent recovery against a genuinely accepted native spawn.
//! The publication and bridge transition remain owned by `admission_fixture`;
//! these tests only exercise the startup recovery consumer of that durable fact.

use crate::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::lifecycle::{RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission_with_owner, PublishedAdmission, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};
use crate::{Collection, ConfigAccess};
use gents_protocol::output::TerminalOutput;
use serde_json::json;

struct AuthorizationFixture {
    target_name: &'static str,
    target_did: Option<&'static str>,
    spawn_enabled: bool,
    background_enabled: bool,
    allow_cross_principal: bool,
}

impl Default for AuthorizationFixture {
    fn default() -> Self {
        Self {
            target_name: "child",
            target_did: None,
            spawn_enabled: true,
            background_enabled: true,
            allow_cross_principal: false,
        }
    }
}

async fn configured_accepted_orphan(
    name: &str,
    child_request_id: &str,
    authorization: AuthorizationFixture,
) -> PublishedAdmission {
    let (admitted, _owner) =
        configured_accepted_bridge(name, child_request_id, authorization).await;
    admitted
}

async fn configured_accepted_bridge(
    name: &str,
    child_request_id: &str,
    authorization: AuthorizationFixture,
) -> (PublishedAdmission, RequestLifecycle) {
    let (mut admitted, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: name.into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        cancel_policy: CancelPolicy::Cascade,
        spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: "bridge-native-tool".into(),
            child_request_id: child_request_id.into(),
            spawn_target_did: "overridden-by-fixture".into(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        ..Default::default()
    })
    .await
    .expect("publish canonical orphan bridge");
    admitted
        .tool
        .publish_background_receipt("child started")
        .await
        .expect("publish immutable background receipt");

    crate::test_support::install_test_behavior(&admitted.node, &admitted.agent_did, "general")
        .await;
    let target_did = authorization
        .target_did
        .unwrap_or(admitted.agent_did.as_str());
    let documents = [
        (
            Collection::Tools,
            json!({
                "agent_did": admitted.agent_did,
                "tools_id": "general:tools",
                "subagents": {
                    "target_ids": ["accepted-recovery-target"],
                    "spawn_enabled": authorization.spawn_enabled,
                    "background_enabled": authorization.background_enabled,
                    "allow_cross_principal": authorization.allow_cross_principal
                }
            }),
        ),
        (
            Collection::SubagentTarget,
            json!({
                "agent_did": admitted.agent_did,
                "target_id": "accepted-recovery-target",
                "name": authorization.target_name,
                "target_agent_did": target_did,
                "behavior_id": "general"
            }),
        ),
    ];
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .expect("valid canonical subagent authorization");
    ConfigAccess::transact_local(
        &admitted.node,
        None,
        "test.accepted_orphan_recovery",
        |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        },
    )
    .await
    .expect("configure parent subagent authorization");
    (admitted, owner)
}

async fn terminalize_accepted_parent(
    admitted: &PublishedAdmission,
    owner: &mut RequestLifecycle,
    outcome: RequestTerminalOutcome,
) {
    let request_doc_id = admitted
        .tool
        .request_doc_id()
        .expect("accepted bridge parent document");
    let accepted_header_doc_id = admitted
        .tool
        .accepted_header_doc_id()
        .expect("accepted parent assistant header");
    assert_eq!(
        owner
            .terminalize_owned(
                outcome,
                TerminalOutput::Message {
                    message_doc_id: accepted_header_doc_id.to_owned()
                },
                Some("parent interrupted"),
            )
            .await
            .expect("terminalize through retained request owner"),
        TerminalizeResult::Won,
    );
    let response = admitted
        .node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ terminal_output }} }}"#,
            crate::graphql::escape_graphql_string(request_doc_id),
        ))
        .await;
    assert!(
        !response.has_errors(),
        "terminal selection: {:?}",
        response.errors
    );
    assert_eq!(
        response.data.unwrap()["AgentRequest"][0]["terminal_output"],
        serde_json::to_value(TerminalOutput::Message {
            message_doc_id: accepted_header_doc_id.to_owned(),
        })
        .unwrap(),
    );
}

#[tokio::test]
async fn accepted_orphan_bridge_materializes_child_once_on_recovery() {
    let admitted = configured_accepted_orphan(
        "accepted-orphan-child-recovery",
        "child-accepted-orphan-recovery",
        AuthorizationFixture::default(),
    )
    .await;
    let node = admitted.node;
    let path = admitted.path;
    let agent_did = admitted.agent_did;
    let tool = admitted.tool;
    let first = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .expect("recover accepted orphan");
    assert_eq!(first.tool_calls_recovered, 0);
    let query = r#"{ AgentRequest(filter: { request_id: { _eq: "child-accepted-orphan-recovery" } }) { _docID request_id behavior_id content subagent_depth caused_by_parent_request_id caused_by_parent_tool_call_id } }"#;
    let response = node.execute(query).await;
    assert!(!response.has_errors(), "child query: {:?}", response.errors);
    let children = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .expect("child rows");
    assert_eq!(children.len(), 1, "accepted orphan creates one exact child");
    let child = &children[0];
    assert_eq!(child["request_id"], "child-accepted-orphan-recovery");
    assert_eq!(child["behavior_id"], "general");
    assert_eq!(child["content"], "work");
    assert_eq!(child["subagent_depth"], 1);
    assert_eq!(
        child["caused_by_parent_request_id"],
        "request-accepted-orphan-child-recovery"
    );
    assert_eq!(child["caused_by_parent_tool_call_id"], tool.tool_call_id());
    let child_doc_id = child["_docID"].as_str().unwrap().to_owned();

    let second = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .expect("replay accepted orphan recovery");
    assert_eq!(second.tool_calls_recovered, 0);
    let response = node.execute(query).await;
    assert!(
        !response.has_errors(),
        "replay child query: {:?}",
        response.errors
    );
    let children = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .expect("replayed child rows");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["_docID"], child_doc_id);

    drop(tool);
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact recovery fixture database");
}

async fn assert_accepted_orphan_denied_on_recovery(
    name: &str,
    authorization: AuthorizationFixture,
    expected_path: &str,
    expected_requested: &str,
) {
    let child_request_id = format!("child-{name}");
    let admitted = configured_accepted_orphan(name, &child_request_id, authorization).await;
    let node = admitted.node;
    let path = admitted.path;
    let agent_did = admitted.agent_did;
    let tool = admitted.tool;
    let tool_doc_id = tool.doc_id().expect("accepted bridge document").to_owned();
    let parent_request_doc_id = tool
        .request_doc_id()
        .expect("accepted bridge parent document")
        .to_owned();

    let report = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .expect("recovery must settle the disallowed accepted orphan");
    assert_eq!(report.tool_calls_recovered, 0);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state tool_failure_class }} AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(&tool_doc_id),
        crate::graphql::escape_graphql_string(&child_request_id),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "denied orphan query: {:?}",
        response.errors
    );
    let data = response.data.as_ref().expect("denied orphan rows");
    let tool_rows = data["AgentToolCall"].as_array().expect("tool rows");
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(tool_rows[0]["lifecycle_state"], "failed");
    assert_eq!(tool_rows[0]["tool_failure_class"], "serviceUnavailable");
    assert!(data["AgentRequest"]
        .as_array()
        .expect("child rows")
        .is_empty());
    let presentation = crate::tool_call_lifecycle::load_tool_call_presentation(
        &ConfigAccess::Local(node.clone()),
        &tool_doc_id,
        &agent_did,
        &format!("session-{name}"),
        Some(&agent_did),
    )
    .await
    .expect("read exact canonical denial result");
    assert_eq!(
        presentation.result.as_deref(),
        Some("child started"),
        "background receipt remains the one native invocation reply"
    );
    let output = crate::background_tools::observe_canonical_tool_output_with_access(
        &ConfigAccess::Local(node.clone()),
        &tool_doc_id,
        &parent_request_doc_id,
        &format!("session-{name}"),
        &agent_did,
        Some(&agent_did),
    )
    .await
    .expect("read exact terminal tool source");
    let crate::background_tools::CanonicalToolOutputObservation::Closed(result) = output else {
        panic!("denied background bridge must have a closed canonical tool source");
    };
    let result: serde_json::Value =
        serde_json::from_str(&result).expect("denial terminal source JSON");
    assert_eq!(result["failure_class"], "tool_not_allowed");
    assert_eq!(result["service_id"], "subagent");
    assert_eq!(result["path"], expected_path);
    assert_eq!(result["requested_tool_name"], expected_requested);

    let replay = ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .expect("denial replay is inert");
    assert_eq!(replay.tool_calls_recovered, 0);
    let replayed = node.execute(&query).await;
    assert!(
        !replayed.has_errors(),
        "replayed denial query: {:?}",
        replayed.errors
    );
    let data = replayed.data.as_ref().expect("replayed denial rows");
    assert_eq!(data["AgentToolCall"][0]["lifecycle_state"], "failed");
    assert!(data["AgentRequest"]
        .as_array()
        .expect("child rows")
        .is_empty());

    drop(tool);
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact recovery fixture database");
}

#[tokio::test]
async fn accepted_orphan_recovery_rejects_unauthorized_target() {
    assert_accepted_orphan_denied_on_recovery(
        "orphan-unauthorized-target",
        AuthorizationFixture {
            target_name: "other",
            ..Default::default()
        },
        "/name",
        "child",
    )
    .await;
}

#[tokio::test]
async fn accepted_orphan_recovery_rejects_spawn_disabled() {
    assert_accepted_orphan_denied_on_recovery(
        "orphan-spawn-disabled",
        AuthorizationFixture {
            spawn_enabled: false,
            ..Default::default()
        },
        "/",
        "spawn_subagent",
    )
    .await;
}

#[tokio::test]
async fn accepted_orphan_recovery_rejects_background_disabled() {
    assert_accepted_orphan_denied_on_recovery(
        "orphan-background-disabled",
        AuthorizationFixture {
            background_enabled: false,
            ..Default::default()
        },
        "/await_mode",
        "background",
    )
    .await;
}

#[tokio::test]
async fn accepted_orphan_recovery_rejects_cross_principal_without_opt_in() {
    assert_accepted_orphan_denied_on_recovery(
        "orphan-cross-principal-disabled",
        AuthorizationFixture {
            target_did: Some("did:key:zRemoteTargetForRecovery"),
            ..Default::default()
        },
        "/name",
        "child",
    )
    .await;
}

#[tokio::test]
async fn accepted_orphan_recovery_leaves_cascade_child_of_interrupted_parent_running() {
    let child_request_id = "child-orphan-parent-interrupted";
    let (admitted, mut owner) = configured_accepted_bridge(
        "orphan-parent-interrupted",
        child_request_id,
        AuthorizationFixture::default(),
    )
    .await;
    terminalize_accepted_parent(&admitted, &mut owner, RequestTerminalOutcome::Interrupted).await;
    drop(owner);
    let node = admitted.node;
    let path = admitted.path;
    let agent_did = admitted.agent_did;
    let tool = admitted.tool;
    ToolCallLifecycle::recover_all(&node, &agent_did)
        .await
        .expect("recover orphan after parent interrupt");
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_request_id}" }} }}, limit: 1) {{ _docID request_id caused_by_parent_tool_call_doc_id interrupt_requested_at }} }}"#,
        ))
        .await;
    assert!(!response.has_errors(), "child query: {:?}", response.errors);
    let children = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .expect("child rows");
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0]["caused_by_parent_tool_call_doc_id"],
        tool.doc_id().unwrap()
    );
    assert!(
        children[0]["interrupt_requested_at"].is_null(),
        "interrupting the parent must not reach its child"
    );
    drop(tool);
    node.shutdown().await;
    std::fs::remove_dir_all(path).expect("remove exact recovery fixture database");
}
