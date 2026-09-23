use super::{CancelCause, ToolCallLifecycle};
use crate::config_client::ConfigAccess;
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use defra_node::EmbeddedNode;

async fn lifecycle_row(node: &EmbeddedNode, doc_id: &str) -> serde_json::Value {
    let response = node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID lifecycle_state cancel_cause tool_failure_class request_doc_id deadline_at }} }}"#,
            crate::graphql::escape_graphql_string(doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentToolCall"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1, "physical lifecycle identity must be unique");
    rows.into_iter().next().unwrap()
}

async fn request_row(node: &EmbeddedNode, doc_id: &str) -> serde_json::Value {
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID lifecycle_state deadline execution_generation }} }}"#,
            crate::graphql::escape_graphql_string(doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 1, "physical request identity must be unique");
    rows.into_iter().next().unwrap()
}

async fn prepare_expired_composed_boundary(
    admission: &PublishedAdmission,
    tool_doc_id: &str,
    request_doc_id: &str,
) {
    let past = (chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
    let tool = admission.node.execute(&format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline_at: "{}" }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(tool_doc_id),
        crate::graphql::escape_graphql_string(&past),
    )).await;
    assert!(!tool.has_errors(), "{:?}", tool.errors);
    let request = admission.node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline: "{}" }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(request_doc_id),
        crate::graphql::escape_graphql_string(&past),
    )).await;
    assert!(!request.has_errors(), "{:?}", request.errors);
}

async fn interrupt_request(admission: &PublishedAdmission, request_doc_id: &str) {
    let response = admission.node.execute(&format!(
        r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "interrupted" }}) {{ _docID }} }}"#,
        crate::graphql::escape_graphql_string(request_doc_id),
    )).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

async fn teardown(admission: PublishedAdmission) {
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn generated_composed_invariants_use_canonical_admission_owner() {
    let witnesses = crate::lean_vocab_test::lean_composed_invariant_witnesses();
    assert_eq!(witnesses.len(), 4);
    for witness in witnesses {
        let running = witness.tool_pre_state == "running";
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: format!("composed-{}", witness.tool_call_id),
            start_running: running,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let request_doc_id = admission.tool.request_doc_id().unwrap().to_owned();

        assert!(
            witness.pre_tool_persisted,
            "canonical tool_spawn acceptance persists the pending physical row"
        );
        assert_eq!(
            witness.deadline_exceeded,
            witness.tool_current_time > witness.tool_deadline
        );
        if witness.deadline_exceeded {
            if running {
                prepare_expired_composed_boundary(&admission, &tool_doc_id, &request_doc_id).await;
                let report = ToolCallLifecycle::recover_all(&admission.node, &admission.agent_did)
                    .await
                    .unwrap();
                assert_eq!(report.tool_calls_recovered, 1);
            } else {
                admission
                    .tool
                    .cancel_before_dispatch(CancelCause::Deadline)
                    .await
                    .unwrap();
            }
        } else if running {
            interrupt_request(&admission, &request_doc_id).await;
            admission
                .tool
                .cancel_during_run(CancelCause::Interrupted)
                .await
                .unwrap();
        } else {
            interrupt_request(&admission, &request_doc_id).await;
            admission
                .tool
                .cancel_before_dispatch(CancelCause::Interrupted)
                .await
                .unwrap();
        }

        let row = lifecycle_row(&admission.node, &tool_doc_id).await;
        let request = request_row(&admission.node, &request_doc_id).await;
        assert_eq!(row["request_doc_id"], request_doc_id);
        let session_id = format!("session-composed-{}", witness.tool_call_id);
        let arguments = super::load_tool_call_arguments(
            &ConfigAccess::Local(admission.node.clone()),
            &tool_doc_id,
            &admission.agent_did,
            &session_id,
            None,
        )
        .await
        .expect("persisted tool must resolve through its canonical accepted header");
        assert!(!arguments.is_empty(), "{}", witness.scenario);
        assert_eq!(
            request["lifecycle_state"], witness.pre_request_state,
            "{}: request transition premise",
            witness.scenario
        );
        match witness.pre_request_admission.as_str() {
            "executing" => assert_eq!(witness.pre_request_state, "processing"),
            // The accepted physical header/tool binding survives the request
            // interrupt transition, while the request admission is correctly
            // released before cancellation is projected to its tool.
            "released" => assert_eq!(witness.pre_request_state, "interrupted"),
            other => panic!("unsupported composed request admission {other}"),
        }
        assert!(
            request["execution_generation"]
                .as_str()
                .is_some_and(|generation| !generation.is_empty()),
            "{}: accepted publication must remain bound to owned execution",
            witness.scenario
        );
        let tool_deadline = chrono::DateTime::parse_from_rfc3339(
            row["deadline_at"].as_str().expect("tool deadline"),
        )
        .unwrap();
        let request_deadline = chrono::DateTime::parse_from_rfc3339(
            request["deadline"].as_str().expect("request deadline"),
        )
        .unwrap();
        assert_eq!(
            tool_deadline, request_deadline,
            "{}: composed tool/request deadlines must remain coherent",
            witness.scenario
        );
        assert_eq!(row["lifecycle_state"], witness.tool_post_state);
        assert_eq!(
            row["cancel_cause"].as_str(),
            witness.cancel_cause.as_deref()
        );
        assert_eq!(
            row["tool_failure_class"].as_str(),
            (witness.deadline_exceeded && running).then_some("external")
        );
        teardown(admission).await;
    }
}
