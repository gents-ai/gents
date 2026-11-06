use super::super::admission_fixture::{
    fixture_job_pin, published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use super::super::{AwaitMode, ToolRowKind};
use super::super::{ToolCallLifecycle, ToolCallState};
use super::IllegalToolCallTransition;

async fn published_job_row(name: &str) -> PublishedAdmission {
    use crate::store::identity::NodeIdentity;
    let mut admission = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        job_pin: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let root = crate::KeyIdentity::load_existing(admission.path.join("node.key"), None).unwrap();
    assert_eq!(root.did(), admission.node_did);
    let agent = crate::load_agent(&admission.node, admission.tool.actor().as_str())
        .await
        .unwrap()
        .unwrap();
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: agent.agent_did,
        creation_nonce: agent.creation_nonce,
    };
    let identity = crate::store::identity::resolve_agent_identity(&root, &binding).unwrap();
    admission
        .tool
        .bind_agent_signer(identity.managed_agent_signer())
        .unwrap();
    admission
}

async fn teardown(admission: PublishedAdmission) {
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn job_row_has_kind_plugin_job() {
    let admission = published_job_row("job-kind").await;
    assert_eq!(admission.tool.kind(), ToolRowKind::PluginJob);
    let loaded = ToolCallLifecycle::load_by_doc_id_as(
        admission.node.clone(),
        admission.tool.actor().clone(),
        admission.tool.doc_id().unwrap(),
        &admission.node_did,
        "session-job-kind",
        admission.tool.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded.kind(), ToolRowKind::PluginJob);
    teardown(admission).await;
}

#[tokio::test]
async fn orphan_sweep_excludes_job_rows() {
    let admission = published_job_row("job-sweep").await;
    let report = ToolCallLifecycle::reconcile_orphaned_background_tools(
        &admission.node,
        &admission.node_did,
        &super::super::recovery_closeout_conformance::recovery_identities(&admission),
        &crate::BackgroundExecutionRegistry::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.tool_calls_terminalized, 0);
    let loaded = ToolCallLifecycle::load_by_doc_id_as(
        admission.node.clone(),
        admission.tool.actor().clone(),
        admission.tool.doc_id().unwrap(),
        &admission.node_did,
        "session-job-sweep",
        admission.tool.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded.state, ToolCallState::Running);
    teardown(admission).await;
}

#[tokio::test]
async fn foreground_refuses_a_job_row() {
    let mut admission = published_job_row("job-foreground").await;
    let err = admission.tool.foreground().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::PluginJobIsBackgroundOnly)
        ),
        "expected PluginJobIsBackgroundOnly, got: {err:?}"
    );
    assert_eq!(admission.tool.await_mode, AwaitMode::Background);
    teardown(admission).await;
}

#[test]
fn in_orphan_sweep_scope_is_background_native_only() {
    for kind in [
        ToolRowKind::Native,
        ToolRowKind::SessionMessage,
        ToolRowKind::PluginJob,
    ] {
        for await_mode in [AwaitMode::Foreground, AwaitMode::Background] {
            assert_eq!(
                kind.in_orphan_sweep_scope(await_mode),
                kind == ToolRowKind::Native && await_mode == AwaitMode::Background,
                "{kind:?} {await_mode:?}"
            );
        }
    }
}

#[test]
fn may_foreground_is_false_for_session_message_and_plugin_job() {
    assert!(ToolRowKind::Native.may_foreground());
    assert!(!ToolRowKind::SessionMessage.may_foreground());
    assert!(!ToolRowKind::PluginJob.may_foreground());
}

#[tokio::test]
async fn published_admission_with_job_pin_loads_as_plugin_job() {
    let admission = published_job_row("job-pin").await;
    assert_eq!(admission.tool.kind(), ToolRowKind::PluginJob);
    let response = admission
        .tool
        .access()
        .unwrap()
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ job_pin }} }}"#,
            admission.tool.doc_id().unwrap()
        ))
        .await
        .expect("read pinned job row as its owning actor");
    let stored = &response["AgentToolCall"][0]["job_pin"];
    assert_eq!(
        serde_json::from_value::<gents_protocol::job::JobPin>(stored.clone()).unwrap(),
        fixture_job_pin()
    );
    teardown(admission).await;

    let default = published_admission(PublishedAdmissionOptions {
        name: "job-pin-default".to_owned(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(default.tool.kind(), ToolRowKind::Native);
    teardown(default).await;
}
