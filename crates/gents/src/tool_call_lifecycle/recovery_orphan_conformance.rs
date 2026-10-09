//! A session-message row outlives the request that made its call: recovery
//! never settles it, or reaches the request it caused, from the caller's fate.

use crate::lifecycle::{RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::tool_call_lifecycle::admission_fixture::{
    published_session_message_with_owner, PublishedAdmission, PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};
use gents_protocol::output::TerminalOutput;

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
async fn recovery_leaves_session_message_row_and_its_request_running_after_caller_interrupt() {
    let (message, mut owner) = published_session_message_with_owner(PublishedAdmissionOptions {
        name: "session-message-caller-interrupted".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted agent_new and its request");
    let admitted = message.admission;
    terminalize_accepted_parent(&admitted, &mut owner, RequestTerminalOutcome::Interrupted).await;
    drop(owner);
    let report = ToolCallLifecycle::recover_all(&admitted.node, &admitted.node_did)
        .await
        .expect("recover after the caller's interrupt");
    assert_eq!(report.tool_calls_recovered, 0);
    let tool_doc_id = admitted.tool.doc_id().unwrap().to_owned();
    let response = admitted
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state await_mode }} AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ caused_by_parent_tool_call_doc_id interrupt_requested_at lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(&tool_doc_id),
            crate::graphql::escape_graphql_string(&message.caused_request_doc_id),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    assert_eq!(data["AgentToolCall"][0]["lifecycle_state"], "running");
    assert_eq!(data["AgentToolCall"][0]["await_mode"], "background");
    assert_eq!(
        data["AgentRequest"][0]["caused_by_parent_tool_call_doc_id"],
        tool_doc_id
    );
    assert!(
        data["AgentRequest"][0]["interrupt_requested_at"].is_null(),
        "interrupting the caller must not reach the request its call caused"
    );
    admitted.node.shutdown().await;
    std::fs::remove_dir_all(admitted.path).expect("remove exact recovery fixture database");
}
