use super::super::admission_fixture::{
    fixture_job_pin, published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use super::super::{AwaitMode, ToolRowKind};
use super::super::{ToolCallLifecycle, ToolCallState};
use super::IllegalToolCallTransition;

async fn published_job_row(name: &str) -> PublishedAdmission {
    published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        await_mode: AwaitMode::Background,
        job_pin: true,
        ..Default::default()
    })
    .await
    .unwrap()
}

async fn teardown(admission: PublishedAdmission) {
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn job_row_has_kind_plugin_job() {
    let admission = published_job_row("job-kind").await;
    assert_eq!(admission.tool.kind(), ToolRowKind::PluginJob);
    let loaded = ToolCallLifecycle::load_by_doc_id(
        admission.node.clone(),
        admission.tool.doc_id().unwrap(),
        &admission.node_did,
        "session-job-kind",
        None,
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
        &crate::BackgroundExecutionRegistry::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.tool_calls_terminalized, 0);
    let loaded = ToolCallLifecycle::load_by_doc_id(
        admission.node.clone(),
        admission.tool.doc_id().unwrap(),
        &admission.node_did,
        "session-job-sweep",
        None,
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
            err.illegal(),
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
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ job_pin }} }}"#,
            admission.tool.doc_id().unwrap()
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let stored = &response.data.unwrap()["AgentToolCall"][0]["job_pin"];
    assert_eq!(
        serde_json::from_value::<gents_protocol::job::JobPin>(stored.clone()).unwrap(),
        fixture_job_pin()
    );
    teardown(admission).await;

    let default = published_admission(PublishedAdmissionOptions {
        name: "job-pin-default".to_owned(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(default.tool.kind(), ToolRowKind::Native);
    teardown(default).await;
}
