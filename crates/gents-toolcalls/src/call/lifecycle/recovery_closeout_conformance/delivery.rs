use crate::call::lifecycle::admission_fixture::{
    complete_child_managed_verified, published_session_message, PublishedAdmissionOptions,
};
use crate::call::lifecycle::ToolCallLifecycle;
use gents_protocol::tool_call::AwaitMode;
use gents_store::identity::NodeIdentity;
use std::sync::Arc;

use crate::call::lifecycle::admission_fixture::{completion_obligations, update};

#[tokio::test]
async fn caused_result_is_delivered_after_the_row_outlives_its_stored_deadline() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-outlives-deadline".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted session message and materialize its request");
    let admission = &message.admission;
    let signer = admission
        .tool
        .access()
        .unwrap()
        .managed_agent_signer()
        .unwrap();
    let identities: Vec<Arc<dyn NodeIdentity>> = vec![Arc::new(signer.clone())];
    let node = &admission.node;
    let access = admission.tool.access().unwrap();
    let did = admission.node_did.clone();
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let session_id = admission.tool.session_id().to_owned();
    update(
        &access,
        &tool_doc_id,
        r#"deadline_at: "2020-01-01T00:00:00Z""#,
    )
    .await;

    let restart = ToolCallLifecycle::recover_all(node, &did, &identities)
        .await
        .unwrap();
    assert_eq!(restart.tool_calls_recovered, 0);
    assert_eq!(
        crate::background::completion::settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            &did
        )
        .await
        .unwrap(),
        0
    );
    let row = |access: gents_store::config_client::ConfigAccess| {
        let tool_doc_id = tool_doc_id.clone();
        async move {
            let response = access
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state cancel_cause }} }}"#,
                    gents_store::graphql::escape_graphql_string(&tool_doc_id)
                ))
                .await
                .unwrap();
            response["data"]["AgentToolCall"][0].clone()
        }
    };
    assert_eq!(row(access.clone()).await["lifecycle_state"], "running");
    let (notifications, _) =
        completion_obligations(&access, &session_id, &did, admission.tool.requester_did()).await;
    assert!(notifications.is_empty(), "{notifications:?}");

    complete_child_managed_verified(
        node,
        &message.caused_request_id,
        "late caused result",
        &signer,
        message.admission.runtime.clone(),
    )
    .await;
    assert_eq!(
        crate::background::completion::settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            &did
        )
        .await
        .unwrap(),
        1
    );
    let settled = row(access.clone()).await;
    assert_eq!(settled["lifecycle_state"], "completed");
    assert!(settled["cancel_cause"].is_null());
    let (notifications, wakes) =
        completion_obligations(&access, &session_id, &did, admission.tool.requester_did()).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(notifications[0].contains("late caused result"));
    assert_eq!(wakes.len(), 1);

    assert_eq!(
        ToolCallLifecycle::recover_all(node, &did, &identities)
            .await
            .unwrap()
            .tool_calls_recovered,
        0
    );
    assert_eq!(
        crate::background::completion::settle_running_session_message_rows_with_access(
            node,
            &admission.tool.access().unwrap(),
            &did
        )
        .await
        .unwrap(),
        0
    );
    let (notifications, _) =
        completion_obligations(&access, &session_id, &did, admission.tool.requester_did()).await;
    assert_eq!(notifications.len(), 1, "delivered exactly once");
    message
        .admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
}
