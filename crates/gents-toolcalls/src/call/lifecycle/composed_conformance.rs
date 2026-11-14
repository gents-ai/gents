use super::ToolCallLifecycle;
use crate::call::lifecycle::admission_fixture::{
    published_admission, PublishedAdmission, PublishedAdmissionOptions,
};
use gents_session::session::tool_call_state::CancelCause;
use gents_store::config_client::ConfigAccess;

/// The admission's Agent owns its request and tool rows; ACP hides them
/// from any other reader.
fn owner(admission: &PublishedAdmission) -> ConfigAccess {
    admission
        .tool
        .access()
        .expect("admission carries its Agent access")
}

async fn single_row(
    admission: &PublishedAdmission,
    query: String,
    collection: &str,
) -> serde_json::Value {
    let response = owner(admission).execute(&query).await.unwrap();
    let rows = response["data"][collection].as_array().unwrap().clone();
    assert_eq!(
        rows.len(),
        1,
        "physical {collection} identity must be unique"
    );
    rows.into_iter().next().unwrap()
}

async fn lifecycle_row(admission: &PublishedAdmission, doc_id: &str) -> serde_json::Value {
    single_row(
        admission,
        format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID lifecycle_state cancel_cause tool_failure_class request_doc_id deadline_at }} }}"#,
            gents_store::graphql::escape_graphql_string(doc_id)
        ),
        "AgentToolCall",
    )
    .await
}

async fn request_row(admission: &PublishedAdmission, doc_id: &str) -> serde_json::Value {
    single_row(
        admission,
        format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID lifecycle_state deadline execution_generation }} }}"#,
            gents_store::graphql::escape_graphql_string(doc_id)
        ),
        "AgentRequest",
    )
    .await
}

async fn owner_update(
    admission: &PublishedAdmission,
    collection: &str,
    doc_id: &str,
    fields: &str,
) {
    let response = owner(admission)
        .write(
            "test.composed_conformance.update",
            &format!(
                r#"mutation {{ update_{collection}(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ {fields} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(doc_id),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        response["data"][format!("update_{collection}")]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{collection} {doc_id} update must reach its row"
    );
}

async fn prepare_expired_composed_boundary(
    admission: &PublishedAdmission,
    tool_doc_id: &str,
    request_doc_id: &str,
) {
    let past = gents_store::graphql::escape_graphql_string(
        &(chrono::Utc::now() - chrono::Duration::seconds(5)).to_rfc3339(),
    );
    owner_update(
        admission,
        "AgentToolCall",
        tool_doc_id,
        &format!(r#"deadline_at: "{past}""#),
    )
    .await;
    owner_update(
        admission,
        "AgentRequest",
        request_doc_id,
        &format!(r#"deadline: "{past}""#),
    )
    .await;
}

async fn interrupt_request(admission: &PublishedAdmission, request_doc_id: &str) {
    owner_update(
        admission,
        "AgentRequest",
        request_doc_id,
        r#"lifecycle_state: "interrupted""#,
    )
    .await;
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
async fn composed_terminations_use_canonical_admission_owner() {
    for case in gents_lean_vocab::families::composed_termination_linked::cases() {
        let witness = &case.input;
        let running = witness.tool_pre_state == "running";
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: format!("composed-{}", witness.tool_call_id),
            real_identity: true,
            start_running: running,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
        let request_doc_id = admission.tool.request_doc_id().unwrap().to_owned();

        if witness.deadline_exceeded {
            if running {
                prepare_expired_composed_boundary(&admission, &tool_doc_id, &request_doc_id).await;
                let report = ToolCallLifecycle::recover_all(
                    &admission.node,
                    &admission.node_did,
                    &super::recovery_closeout_conformance::recovery_identities(&admission),
                )
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

        let row = lifecycle_row(&admission, &tool_doc_id).await;
        let request = request_row(&admission, &request_doc_id).await;
        assert_eq!(row["request_doc_id"], request_doc_id);
        let session_id = format!("session-composed-{}", witness.tool_call_id);
        let arguments = gents_session::tool_call::query::result::load_tool_call_arguments(
            &owner(&admission),
            &tool_doc_id,
            &admission.node_did,
            &session_id,
            admission.tool.requester_did(),
        )
        .await
        .expect("persisted tool must resolve through its canonical accepted header");
        assert!(!arguments.is_empty(), "{}", witness.scenario);
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
        case.check(
            gents_lean_vocab::families::composed_termination_linked::Output {
                pre_request_state: request["lifecycle_state"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                tool_post_state: row["lifecycle_state"].as_str().map(str::to_owned),
                cancel_cause: row["cancel_cause"].as_str().map(str::to_owned),
                tool_failure_class: row["tool_failure_class"].as_str().map(str::to_owned),
            },
        );
        teardown(admission).await;
    }
}
